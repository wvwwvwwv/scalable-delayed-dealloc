use super::exit_guard::ExitGuard;
use super::maybe_std::fence as maybe_std_fence;
use super::{Collectible, Epoch, Link, Tag};
use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{AtomicPtr, AtomicU8};

/// [`Collector`] is a garbage collector that reclaims thread-locally unreachable instances
/// when they are globally unreachable.
#[derive(Debug)]
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
    /// [`Guard`](super::Guard).
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

    /// Acknowledges a new [`Guard`](super::Guard) being instantiated.
    ///
    /// # Panics
    ///
    /// The method may panic if the number of readers has reached `u32::MAX`.
    #[inline]
    pub(super) fn new_guard(collector_ptr: *mut Collector, collect_garbage: bool) {
        let collector = unsafe { &mut *collector_ptr };
        if collector.num_readers == 0 {
            debug_assert_eq!(
                collector.state.load(Relaxed) & Self::INACTIVE,
                Self::INACTIVE
            );
            collector.num_readers = 1;
            let new_epoch = Epoch::from_u8(GLOBAL_ROOT.epoch.load(Relaxed));
            if cfg!(feature = "loom") || cfg!(not(any(target_arch = "x86", target_arch = "x86_64")))
            {
                // What will happen after the fence strictly happens after the fence.
                collector.state.store(new_epoch.into(), Relaxed);
                maybe_std_fence(SeqCst);
            } else {
                // This special optimization is excerpted from
                // [`crossbeam_epoch`](https://docs.rs/crossbeam-epoch/).
                //
                // The rationale behind the code is, it compiles to `lock xchg` that
                // practically acts as a full memory barrier on `X86`, and is much faster than
                // `mfence`.
                collector.state.swap(new_epoch.into(), SeqCst);
            }
            if collector.announcement != new_epoch {
                collector.announcement = new_epoch;
                if collect_garbage {
                    let mut exit_guard =
                        ExitGuard::new((collector_ptr, false), |(collector_ptr, result)| {
                            if !result {
                                Self::end_guard(collector_ptr);
                            }
                        });
                    Collector::epoch_updated(exit_guard.0);
                    exit_guard.1 = true;
                }
            }
        } else {
            debug_assert_eq!(collector.state.load(Relaxed) & Self::INACTIVE, 0);
            assert_ne!(collector.num_readers, u32::MAX, "Too many EBR guards");
            collector.num_readers += 1;
        }
    }

    /// Acknowledges an existing [`Guard`](super::Guard) being dropped.
    #[inline]
    pub(super) fn end_guard(collector_ptr: *mut Collector) {
        let mut collector = unsafe { &mut *collector_ptr };
        debug_assert_eq!(collector.state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(
            collector.state.load(Relaxed),
            u8::from(collector.announcement)
        );

        if collector.num_readers == 1 {
            if collector.next_epoch_update == 0 {
                if collector.has_garbage
                    || Tag::into_tag(GLOBAL_ROOT.chain_head.load(Relaxed)) != Tag::First
                {
                    Collector::scan(collector_ptr);
                    collector = unsafe { &mut *collector_ptr };
                }
                collector.next_epoch_update = if collector.has_garbage {
                    Self::CADENCE / 4
                } else {
                    Self::CADENCE
                };
            } else {
                collector.next_epoch_update = collector.next_epoch_update.saturating_sub(1);
            }

            // What has happened cannot be observed after the thread setting itself inactive has
            // been witnessed.
            collector
                .state
                .store(u8::from(collector.announcement) | Self::INACTIVE, Release);
            collector.num_readers = 0;
        } else {
            collector.num_readers -= 1;
        }
    }

    /// Returns the current epoch.
    #[inline]
    pub(super) fn current_epoch() -> Epoch {
        // It is called by an active `Guard` therefore it is after a `SeqCst` memory barrier. Each
        // epoch update is preceded by another `SeqCst` memory barrier, therefore those two events
        // are globally ordered. If the `SeqCst` event during the `Guard` creation happened before
        // the other `SeqCst` event, this will either load the last previous epoch value, or the
        // current value. If not, it is guaranteed that it reads the latest global epoch value.
        Epoch::from_u8(GLOBAL_ROOT.epoch.load(Relaxed))
    }

    #[inline]
    /// Accelerates garbage collection.
    pub(super) fn accelerate(&mut self) {
        mark_scan_enforced();
        self.next_epoch_update = 0;
    }

    /// Collects garbage instances.
    #[inline]
    pub(super) fn collect(collector_ptr: *mut Collector, instance_ptr: *mut dyn Collectible) {
        if let Some(ptr) = NonNull::new(instance_ptr) {
            unsafe {
                let collector = &mut *collector_ptr;
                ptr.as_ref()
                    .set_next_ptr(collector.current_instance_link.take());
                collector.current_instance_link.replace(ptr);
                collector.next_epoch_update = collector
                    .next_epoch_update
                    .saturating_sub(1)
                    .min(Self::CADENCE / 4);
                collector.has_garbage = true;
            }
        }
    }

    /// Passes its garbage instances to other threads.
    #[inline]
    pub(super) fn pass_garbage() -> bool {
        LOCAL_COLLECTOR.with(|local_collector| {
            let collector_ptr = local_collector.load(Relaxed);
            if let Some(collector) = unsafe { collector_ptr.as_mut() } {
                if collector.num_readers != 0 {
                    return false;
                }
                if collector.has_garbage {
                    collector.state.fetch_or(Collector::INVALID, Release);
                    local_collector.store(ptr::null_mut(), Relaxed);
                    mark_scan_enforced();
                }
            }
            true
        })
    }

    /// Allocates a new [`Collector`].
    fn alloc() -> *mut Collector {
        let boxed = Box::new(Collector {
            state: AtomicU8::new(Self::INACTIVE),
            announcement: Epoch::default(),
            next_epoch_update: Self::CADENCE,
            has_garbage: false,
            num_readers: 0,
            previous_instance_link: Link::default().next_ptr(),
            current_instance_link: Link::default().next_ptr(),
            next_instance_link: Link::default().next_ptr(),
            next_link: AtomicPtr::default(),
            link: Link::default(),
        });
        let ptr = Box::into_raw(boxed);
        let mut current = GLOBAL_ROOT.chain_head.load(Relaxed);
        loop {
            unsafe {
                (*ptr)
                    .next_link
                    .store(Tag::unset_tag(current).cast_mut(), Relaxed);
            }

            // It keeps the tag intact.
            let tag = Tag::into_tag(current);
            let new = Tag::update_tag(ptr, tag).cast_mut();
            if let Err(actual) = GLOBAL_ROOT
                .chain_head
                .compare_exchange_weak(current, new, Release, Relaxed)
            {
                current = actual;
            } else {
                break;
            }
        }
        ptr
    }

    /// Acknowledges a new global epoch.
    fn epoch_updated(collector_ptr: *mut Collector) {
        let collector = unsafe { &mut *collector_ptr };
        debug_assert_eq!(collector.state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(
            collector.state.load(Relaxed),
            u8::from(collector.announcement)
        );

        let mut garbage_link = collector.next_instance_link.take();
        collector.next_instance_link = collector.previous_instance_link.take();
        collector.previous_instance_link = collector.current_instance_link.take();
        collector.has_garbage =
            collector.next_instance_link.is_some() || collector.previous_instance_link.is_some();
        while let Some(instance_ptr) = garbage_link.take() {
            garbage_link = unsafe { instance_ptr.as_ref().next_ptr() };
            let mut guard = ExitGuard::new(garbage_link, |mut garbage_link| {
                while let Some(instance_ptr) = garbage_link.take() {
                    // Something went wrong during dropping and deallocating an instance.
                    garbage_link = unsafe { instance_ptr.as_ref().next_ptr() };

                    // Previous `drop_and_dealloc` may have accessed `self.current_instance_link`.
                    std::sync::atomic::compiler_fence(Acquire);
                    Collector::collect(collector_ptr, instance_ptr.as_ptr());
                }
            });

            // The `drop` below may access `self.current_instance_link`.
            std::sync::atomic::compiler_fence(Acquire);
            unsafe {
                drop(Box::from_raw(instance_ptr.as_ptr()));
            }
            garbage_link = guard.take();
        }
    }

    /// Clears all the garbage instances for dropping the [`Collector`].
    fn clear_for_drop(collector_ptr: *mut Collector) {
        let collector = unsafe { &mut *collector_ptr };
        for mut link in [
            collector.previous_instance_link.take(),
            collector.current_instance_link.take(),
            collector.next_instance_link.take(),
        ] {
            while let Some(instance_ptr) = link.take() {
                link = unsafe { instance_ptr.as_ref().next_ptr() };
                unsafe {
                    drop(Box::from_raw(instance_ptr.as_ptr()));
                }
            }
        }
        while let Some(link) = unsafe { (*collector_ptr).current_instance_link.take() } {
            let mut current = Some(link);
            while let Some(instance_ptr) = current.take() {
                current = unsafe { instance_ptr.as_ref().next_ptr() };
                unsafe {
                    drop(Box::from_raw(instance_ptr.as_ptr()));
                }
            }
        }
    }

    /// Scans the [`Collector`] instances to update the global epoch.
    fn scan(collector_ptr: *mut Collector) -> bool {
        let collector = unsafe { &*collector_ptr };
        debug_assert_eq!(collector.state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(
            collector.state.load(Relaxed),
            u8::from(collector.announcement)
        );

        // Only one thread that acquires the chain lock is allowed to scan the thread-local
        // collectors.
        let lock_result = GLOBAL_ROOT
            .chain_head
            .fetch_update(Acquire, Acquire, |p| {
                let tag = Tag::into_tag(p);
                if tag == Tag::First || tag == Tag::Both {
                    None
                } else {
                    Some(Tag::update_tag(p, Tag::First).cast_mut())
                }
            })
            .map(|p| Tag::unset_tag(p).cast_mut());
        if let Ok(mut current_collector_ptr) = lock_result {
            let _guard = ExitGuard::new((), |()| {
                // Unlock the chain.
                loop {
                    let result = GLOBAL_ROOT.chain_head.fetch_update(Release, Relaxed, |p| {
                        let tag = Tag::into_tag(p);
                        debug_assert!(tag == Tag::First || tag == Tag::Both);
                        let new_tag = if tag == Tag::First {
                            Tag::None
                        } else {
                            Tag::Second
                        };
                        Some(Tag::update_tag(p, new_tag).cast_mut())
                    });
                    if result.is_ok() {
                        break;
                    }
                }
            });

            let known_epoch = collector.state.load(Relaxed);
            let mut update_global_epoch = true;
            let mut prev_collector_ptr: *mut Collector = ptr::null_mut();
            while !current_collector_ptr.is_null() {
                if ptr::eq(collector_ptr, current_collector_ptr) {
                    prev_collector_ptr = current_collector_ptr;
                    current_collector_ptr = collector.next_link.load(Relaxed);
                    continue;
                }

                let collector_state = unsafe { (*current_collector_ptr).state.load(Relaxed) };
                let next_collector_ptr =
                    unsafe { (*current_collector_ptr).next_link.load(Relaxed) };
                if (collector_state & Self::INVALID) != 0 {
                    // The collector is obsolete.
                    let result = if prev_collector_ptr.is_null() {
                        GLOBAL_ROOT
                            .chain_head
                            .fetch_update(Release, Relaxed, |p| {
                                let tag = Tag::into_tag(p);
                                debug_assert!(tag == Tag::First || tag == Tag::Both);
                                if ptr::eq(Tag::unset_tag(p), current_collector_ptr) {
                                    Some(Tag::update_tag(next_collector_ptr, tag).cast_mut())
                                } else {
                                    None
                                }
                            })
                            .is_ok()
                    } else {
                        unsafe {
                            (*prev_collector_ptr)
                                .next_link
                                .store(next_collector_ptr, Relaxed);
                        }
                        true
                    };
                    if result {
                        Self::collect(collector_ptr, current_collector_ptr);
                        current_collector_ptr = next_collector_ptr;
                        continue;
                    }
                } else if (collector_state & Self::INACTIVE) == 0 && collector_state != known_epoch
                {
                    // Not ready for an epoch update.
                    update_global_epoch = false;
                    break;
                }
                prev_collector_ptr = current_collector_ptr;
                current_collector_ptr = next_collector_ptr;
            }

            if update_global_epoch {
                // It is a new era; a fence is required.
                maybe_std_fence(SeqCst);
                GLOBAL_ROOT
                    .epoch
                    .store(Epoch::from_u8(known_epoch).next().into(), Relaxed);
                return true;
            }
        }

        false
    }
}

impl Drop for Collector {
    #[inline]
    fn drop(&mut self) {
        let collector_ptr = ptr::addr_of_mut!(*self);
        Self::clear_for_drop(collector_ptr);
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
        unsafe {
            try_drop_local_collector();
        }
    }
}

/// Marks the head of a chain that there is a potentially unreachable `Collector` in the chain.
fn mark_scan_enforced() {
    // `Tag::Second` indicates that there is a garbage `Collector`.
    let _result = GLOBAL_ROOT.chain_head.fetch_update(Release, Relaxed, |p| {
        let new_tag = match Tag::into_tag(p) {
            Tag::None => Tag::Second,
            Tag::First => Tag::Both,
            Tag::Second | Tag::Both => return None,
        };
        Some(Tag::update_tag(p, new_tag).cast_mut())
    });
}

/// Tries to drop the local [`Collector`] if it is the sole survivor.
///
/// # Safety
///
/// The function is safe to call only when the thread is being joined.
unsafe fn try_drop_local_collector() {
    let collector_ptr = LOCAL_COLLECTOR.with(|local_collector| local_collector.load(Relaxed));
    if collector_ptr.is_null() {
        return;
    }
    let mut chain_head_ptr = GLOBAL_ROOT.chain_head.load(Relaxed);
    if Tag::into_tag(chain_head_ptr) == Tag::Second {
        // Another thread was joined before, and has yet to be cleaned up.
        let guard = super::Guard::new_for_drop(collector_ptr);
        Collector::scan(collector_ptr);
        drop(guard);
        chain_head_ptr = GLOBAL_ROOT.chain_head.load(Relaxed);
    }
    if (*collector_ptr).next_link.load(Relaxed).is_null()
        && ptr::eq(collector_ptr, chain_head_ptr)
        && GLOBAL_ROOT
            .chain_head
            .compare_exchange(chain_head_ptr, ptr::null_mut(), Relaxed, Relaxed)
            .is_ok()
    {
        // If it is the head, and the only `Collector` in the chain, drop it here.
        drop(Box::from_raw(collector_ptr));
        return;
    }

    (*collector_ptr).state.fetch_or(Collector::INVALID, Release);
    mark_scan_enforced();
}

thread_local! {
    static COLLECTOR_ANCHOR: CollectorAnchor = const { CollectorAnchor };
    static LOCAL_COLLECTOR: AtomicPtr<Collector> = AtomicPtr::default();
}

/// The global and default [`CollectorRoot`].
static GLOBAL_ROOT: CollectorRoot = CollectorRoot {
    epoch: AtomicU8::new(0),
    chain_head: AtomicPtr::new(ptr::null_mut()),
};
