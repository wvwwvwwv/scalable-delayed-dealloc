use crate::collectible::{Collectible, Link};
use crate::exit_guard::ExitGuard;
use crate::maybe_std::fence as maybe_std_fence;
use crate::{Epoch, Tag};
use std::mem;
use std::ptr::{self, addr_of_mut, NonNull};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{compiler_fence, AtomicPtr, AtomicU8};

/// [`Collector`] is a garbage collector that reclaims thread-locally unreachable instances
/// when they are globally unreachable.
#[derive(Debug, Default)]
#[repr(align(128))]
pub(super) struct Collector {
    state: AtomicU8,
    announcement: Epoch,
    next_epoch_update: u8,
    has_garbage: bool,
    num_readers: u32,
    previous_instance_link: Option<NonNull<dyn Collectible>>,
    current_instance_link: Option<NonNull<dyn Collectible>>,
    next_instance_link: Option<NonNull<dyn Collectible>>,
    next_link: AtomicPtr<Collector>,
    link: Link,
}

/// Data stored in a [`CollectorRoot`] is shared among [`Collector`] instances.
#[derive(Debug, Default)]
pub(super) struct CollectorRoot {
    epoch: AtomicU8,
    chain_head: AtomicPtr<Collector>,
}

/// [`CollectorAnchor`] helps allocate and cleanup the thread-local [`Collector`].
struct CollectorAnchor;

impl Collector {
    /// The cadence of an epoch update.
    const CADENCE: u8 = u8::MAX;

    /// A bit field representing a thread state where the thread does not have a
    /// [`Guard`](crate::Guard).
    const INACTIVE: u8 = 1_u8 << 2;

    /// A bit field representing a thread state where the thread has been terminated.
    const INVALID: u8 = 1_u8 << 3;

    /// Returns the [`Collector`] attached to the current thread.
    #[inline]
    pub(super) fn current() -> *mut Collector {
        LOCAL_COLLECTOR.with(|local_collector| {
            let mut collector_ptr = local_collector.load(Relaxed);

            if collector_ptr.is_null() {
                collector_ptr = COLLECTOR_ANCHOR.with(CollectorAnchor::alloc);
                local_collector.store(collector_ptr, Relaxed);
            }

            collector_ptr
        })
    }

    /// Acknowledges a new [`Guard`](crate::Guard) being instantiated.
    ///
    /// # Panics
    ///
    /// The method may panic if the number of readers has reached `u32::MAX`.
    #[inline]
    pub(super) unsafe fn new_guard(collector_ptr: *mut Collector, collect_garbage: bool) {
        if (*collector_ptr).num_readers > 0 {
            debug_assert_eq!((*collector_ptr).state.load(Relaxed) & Self::INACTIVE, 0);
            assert_ne!(
                (*collector_ptr).num_readers,
                u32::MAX,
                "Too many EBR guards"
            );

            (*collector_ptr).num_readers += 1;
            return;
        }

        debug_assert_eq!(
            (*collector_ptr).state.load(Relaxed) & Self::INACTIVE,
            Self::INACTIVE
        );
        (*collector_ptr).num_readers = 1;
        let new_epoch = Epoch::from(GLOBAL_ROOT.epoch.load(Relaxed));

        if cfg!(feature = "loom") || cfg!(not(any(target_arch = "x86", target_arch = "x86_64"))) {
            // What will happen after the fence strictly happens after the fence.
            (*collector_ptr).state.store(new_epoch.into(), Relaxed);
            maybe_std_fence(SeqCst);
        } else {
            // This special optimization is excerpted from
            // [`crossbeam_epoch`](https://docs.rs/crossbeam-epoch/).
            //
            // The rationale behind the code is, it compiles to `lock xchg` that
            // practically acts as a full memory barrier on `X86`, and is much faster than
            // `mfence`.
            (*collector_ptr).state.swap(new_epoch.into(), SeqCst);
        }

        if (*collector_ptr).announcement == new_epoch {
            return;
        }
        (*collector_ptr).announcement = new_epoch;

        if !collect_garbage {
            return;
        }

        let mut exit_guard = ExitGuard::new((collector_ptr, false), |(collector_ptr, result)| {
            if !result {
                Self::end_guard(collector_ptr);
            }
        });

        Collector::epoch_updated(exit_guard.0);
        exit_guard.1 = true;
    }

    /// Acknowledges an existing [`Guard`](crate::Guard) being dropped.
    #[inline]
    pub(super) unsafe fn end_guard(collector_ptr: *mut Collector) {
        debug_assert_eq!((*collector_ptr).state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(
            (*collector_ptr).state.load(Relaxed),
            (*collector_ptr).announcement.into()
        );

        if (*collector_ptr).num_readers != 1 {
            (*collector_ptr).num_readers -= 1;
            return;
        }

        (*collector_ptr).num_readers = 0;
        (*collector_ptr).next_epoch_update = if (*collector_ptr).next_epoch_update == 0 {
            if (*collector_ptr).has_garbage
                || Tag::into_tag(GLOBAL_ROOT.chain_head.load(Relaxed)) == Tag::Second
            {
                Collector::scan(collector_ptr);
            }

            #[allow(clippy::cast_lossless)]
            let has_garbage = (*collector_ptr).has_garbage as usize;
            Self::CADENCE >> (has_garbage << 1)
        } else {
            (*collector_ptr).next_epoch_update.saturating_sub(1)
        };

        // What has happened cannot be observed after the thread setting itself inactive has
        // been witnessed.
        (*collector_ptr).state.store(
            u8::from((*collector_ptr).announcement) | Self::INACTIVE,
            Release,
        );
    }

    /// Returns the current epoch.
    #[inline]
    pub(super) fn current_epoch() -> Epoch {
        // It is called by an active `Guard` therefore it is after a `SeqCst` memory barrier. Each
        // epoch update is preceded by another `SeqCst` memory barrier, therefore those two events
        // are globally ordered. If the `SeqCst` event during the `Guard` creation happened before
        // the other `SeqCst` event, this will either load the last previous epoch value, or the
        // current value. If not, it is guaranteed that it reads the latest global epoch value.
        Epoch::from(GLOBAL_ROOT.epoch.load(Relaxed))
    }

    #[inline]
    /// Accelerates garbage collection.
    pub(super) fn accelerate(&mut self) {
        mark_scan_enforced();
        self.next_epoch_update = 0;
    }

    /// Collects garbage instances.
    #[inline]
    pub(super) unsafe fn collect(
        collector_ptr: *mut Collector,
        instance_ptr: *mut dyn Collectible,
    ) {
        if instance_ptr.is_null() {
            return;
        }
        (*instance_ptr).set_next_ptr((*collector_ptr).current_instance_link.take());
        (*collector_ptr).current_instance_link = NonNull::new(instance_ptr);
        (*collector_ptr).next_epoch_update = (*collector_ptr)
            .next_epoch_update
            .saturating_sub(1)
            .min(Self::CADENCE / 4);
        (*collector_ptr).has_garbage = true;
    }

    /// Passes its garbage instances to other threads.
    #[inline]
    pub(super) fn pass_garbage() -> bool {
        fn pass_garbage(collector: &AtomicPtr<Collector>) -> bool {
            let collector_ptr = collector.load(Relaxed);

            if collector_ptr.is_null() {
                return true;
            }

            let (old_collector, collector) = (collector, unsafe { &*collector_ptr });

            if collector.num_readers != 0 {
                return false;
            }

            if collector.has_garbage {
                collector.state.fetch_or(Collector::INVALID, Release);
                old_collector.store(ptr::null_mut(), Relaxed);
                mark_scan_enforced();
            }

            true
        }

        LOCAL_COLLECTOR.with(pass_garbage)
    }

    /// Allocates a new [`Collector`].
    fn alloc() -> *mut Collector {
        let boxed: Box<Collector> = Box::default();
        boxed.state.store(Self::INACTIVE, Relaxed);

        let ptr = Box::into_raw(boxed);
        let mut current = GLOBAL_ROOT.chain_head.load(Relaxed);

        unsafe {
            (*ptr)
                .next_link
                .store(Tag::unset_tag(current).cast_mut(), Relaxed);
        }

        while let Err(actual) = GLOBAL_ROOT.chain_head.compare_exchange_weak(
            current,
            Tag::update_tag(ptr, Tag::into_tag(current)).cast_mut(),
            Release,
            Relaxed,
        ) {
            current = actual;
            unsafe {
                (*ptr)
                    .next_link
                    .store(Tag::unset_tag(current).cast_mut(), Relaxed);
            }
        }

        ptr
    }

    /// Acknowledges a new global epoch.
    unsafe fn epoch_updated(collector_ptr: *mut Collector) {
        debug_assert_eq!((*collector_ptr).state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(
            (*collector_ptr).state.load(Relaxed),
            u8::from((*collector_ptr).announcement)
        );

        let mut garbage_link = (*collector_ptr).next_instance_link.take();
        (*collector_ptr).next_instance_link = (*collector_ptr).previous_instance_link.take();
        (*collector_ptr).previous_instance_link = (*collector_ptr).current_instance_link.take();
        (*collector_ptr).has_garbage = (*collector_ptr).next_instance_link.is_some()
            || (*collector_ptr).previous_instance_link.is_some();

        while let Some(instance_ptr) = garbage_link.take() {
            garbage_link = (*instance_ptr.as_ptr()).next_ptr();

            let mut guard = ExitGuard::new(garbage_link, |mut garbage_link| {
                while let Some(instance_ptr) = garbage_link.take() {
                    // Something went wrong during dropping and deallocating an instance.
                    garbage_link = (*instance_ptr.as_ptr()).next_ptr();

                    // Previous `drop_and_dealloc` may have accessed `self.current_instance_link`.
                    compiler_fence(Acquire);
                    Collector::collect(collector_ptr, instance_ptr.as_ptr());
                }
            });

            // The `drop` below may access `self.current_instance_link`.
            compiler_fence(Acquire);
            drop(Box::from_raw(instance_ptr.as_ptr()));
            garbage_link = guard.take();
        }
    }

    /// Clears all the garbage instances for dropping the [`Collector`].
    unsafe fn clear_for_drop(collector_ptr: *mut Collector) {
        let mut valid = true;

        while mem::take(&mut valid) {
            for mut link in [
                (*collector_ptr).previous_instance_link.take(),
                (*collector_ptr).current_instance_link.take(),
                (*collector_ptr).next_instance_link.take(),
            ] {
                while let Some(instance_ptr) = link {
                    valid = true;
                    link = (*instance_ptr.as_ptr()).next_ptr();
                    drop(Box::from_raw(instance_ptr.as_ptr()));
                }
            }
        }
    }

    /// Scans the [`Collector`] instances to update the global epoch.
    unsafe fn scan(collector_ptr: *mut Collector) -> bool {
        debug_assert_eq!((*collector_ptr).state.load(Relaxed) & Self::INVALID, 0);

        // Only one thread that acquires the chain lock is allowed to scan the thread-local
        // collectors.
        let Ok(mut current_ptr) = Self::lock_chain() else {
            return false;
        };

        let _guard = ExitGuard::new((), |()| Self::unlock_chain());
        let known_epoch = (*collector_ptr).state.load(Relaxed);
        let mut previous_ptr: *mut Collector = ptr::null_mut();

        while !current_ptr.is_null() {
            #[allow(clippy::ptr_eq)]
            if collector_ptr == current_ptr {
                previous_ptr = current_ptr;
                current_ptr = (*collector_ptr).next_link.load(Relaxed);
                continue;
            }

            let state = (*current_ptr).state.load(Acquire);
            let next_ptr = (*current_ptr).next_link.load(Relaxed);

            if (state & Self::INVALID) == 0 && state != known_epoch {
                // Not ready for an epoch update.
                return false;
            } else if state == known_epoch {
                previous_ptr = current_ptr;
                current_ptr = next_ptr;
                continue;
            }

            // The collector is obsolete.
            let result = if previous_ptr.is_null() {
                GLOBAL_ROOT
                    .chain_head
                    .fetch_update(Release, Relaxed, |p| {
                        let tag = Tag::into_tag(p);
                        debug_assert!(tag == Tag::First || tag == Tag::Both);
                        #[allow(clippy::ptr_eq)]
                        (Tag::unset_tag(p) == current_ptr)
                            .then(|| Tag::update_tag(next_ptr, tag).cast_mut())
                    })
                    .is_ok()
            } else {
                (*previous_ptr).next_link.store(next_ptr, Relaxed);
                true
            };

            if !result {
                previous_ptr = current_ptr;
                current_ptr = next_ptr;
                continue;
            }

            Self::collect(collector_ptr, current_ptr);
            current_ptr = next_ptr;
        }

        // It is a new era; a fence is required.
        maybe_std_fence(SeqCst);
        GLOBAL_ROOT
            .epoch
            .store(Epoch::from(known_epoch).next().into(), Relaxed);

        true
    }

    /// Clears the [`Collector`] chain to if all are invalid.
    unsafe fn clear_chain() -> bool {
        let lock_result = Self::lock_chain();
        let Ok(collector_head) = lock_result else {
            return false;
        };

        let _guard = ExitGuard::new((), |()| Self::unlock_chain());

        let mut current_collector_ptr = collector_head;
        while !current_collector_ptr.is_null() {
            if ((*current_collector_ptr).state.load(Acquire) & Self::INVALID) == 0 {
                return false;
            }

            current_collector_ptr = (*current_collector_ptr).next_link.load(Relaxed);
        }

        // Reaching here means that there is no `Ptr` that possibly sees any garbage instances
        // in those `Collector` instances in the chain.
        let result = GLOBAL_ROOT.chain_head.fetch_update(Release, Relaxed, |p| {
            #[allow(clippy::ptr_eq)]
            (Tag::unset_tag(p) == collector_head).then(|| {
                let tag = Tag::into_tag(p);
                debug_assert!(tag == Tag::First || tag == Tag::Both);
                Tag::update_tag(ptr::null::<Collector>(), tag).cast_mut()
            })
        });

        if result.is_err() {
            return false;
        }

        let mut current_collector_ptr = collector_head;
        while !current_collector_ptr.is_null() {
            let next_collector_ptr = (*current_collector_ptr).next_link.load(Relaxed);
            drop(Box::from_raw(current_collector_ptr));
            current_collector_ptr = next_collector_ptr;
        }

        true
    }

    /// Locks the chain.
    fn lock_chain() -> Result<*mut Collector, *mut Collector> {
        GLOBAL_ROOT
            .chain_head
            .fetch_update(Acquire, Acquire, |p| {
                let tag = Tag::into_tag(p);

                (tag == Tag::None || tag == Tag::Second)
                    .then(|| Tag::update_tag(p, Tag::First).cast_mut())
            })
            .map(|p| Tag::unset_tag(p).cast_mut())
    }

    /// Unlocks the chain.
    fn unlock_chain() {
        let mut result = Err(ptr::null_mut());

        while result.is_err() {
            result = GLOBAL_ROOT.chain_head.fetch_update(Release, Relaxed, |p| {
                let tag = Tag::into_tag(p);
                debug_assert!(tag == Tag::First || tag == Tag::Both);

                Some(
                    #[allow(clippy::missing_transmute_annotations)]
                    Tag::update_tag(p, unsafe { mem::transmute(tag as usize & !Tag::First) })
                        .cast_mut(),
                )
            });
        }
    }
}

impl Drop for Collector {
    #[inline]
    fn drop(&mut self) {
        let collector_ptr = addr_of_mut!(*self);

        unsafe {
            Self::clear_for_drop(collector_ptr);
        }
    }
}

impl Collectible for Collector {
    #[inline]
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>> {
        self.link.next_ptr()
    }

    #[inline]
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>) {
        self.link.set_next_ptr(next_ptr);
    }
}

impl CollectorAnchor {
    fn alloc(&self) -> *mut Collector {
        let _: &CollectorAnchor = self;
        Collector::alloc()
    }
}

impl Drop for CollectorAnchor {
    #[inline]
    fn drop(&mut self) {
        fn drop(collector: &AtomicPtr<Collector>) {
            unsafe {
                let collector_ptr = collector.load(Relaxed);
                if !collector_ptr.is_null() {
                    (*collector_ptr).state.fetch_or(Collector::INVALID, Release);
                }

                let mut temp_collector = Collector::default();
                temp_collector.state.store(Collector::INACTIVE, Relaxed);
                collector.store(addr_of_mut!(temp_collector), Release);
                if !Collector::clear_chain() {
                    mark_scan_enforced();
                }

                Collector::clear_for_drop(addr_of_mut!(temp_collector));
                collector.store(ptr::null_mut(), Release);
            }
        }

        // `LOCAL_COLLECTOR` is the last thread-local variable to be dropped.
        LOCAL_COLLECTOR.with(drop);
    }
}

/// Marks the head of a chain that there is a potentially unreachable `Collector` in the chain.
fn mark_scan_enforced() {
    // `Tag::Second` indicates that there is a garbage `Collector`.
    let _ = GLOBAL_ROOT.chain_head.fetch_update(Release, Relaxed, |p| {
        let tag = Tag::into_tag(p);

        if tag == Tag::Second || tag == Tag::Both {
            return None;
        }

        Some(Tag::update_tag(p, tag + Tag::Second).cast_mut())
    });
}

thread_local! {
    static LOCAL_COLLECTOR: AtomicPtr<Collector> = const { AtomicPtr::new(ptr::null_mut()) };
    static COLLECTOR_ANCHOR: CollectorAnchor = const { CollectorAnchor };
}

/// The global and default [`CollectorRoot`].
static GLOBAL_ROOT: CollectorRoot = CollectorRoot {
    epoch: AtomicU8::new(0),
    chain_head: AtomicPtr::new(ptr::null_mut()),
};
