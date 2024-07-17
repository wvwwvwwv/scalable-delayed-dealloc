use super::exit_guard::ExitGuard;
use super::optional_private::fence as augmented_fence;
use super::optional_private::thread_local as augmented_thread_local;
use super::optional_private::AtomicPtr as AugmentedAtomicPtr;
use super::optional_private::AtomicU8 as AugmentedAtomicU8;
use super::{Collectible, Epoch, Tag};
use std::ptr::{self, NonNull};
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};
use std::sync::Arc;

/// [`Collector`] is a garbage collector that reclaims thread-locally unreachable instances
/// when they are globally unreachable.
#[derive(Debug)]
#[repr(align(128))]
pub(super) struct Collector {
    state: AugmentedAtomicU8,
    announcement: Epoch,
    next_epoch_update: u8,
    has_garbage: bool,
    num_readers: u32,
    root: Arc<CollectorRoot>,
    previous_instance_link: Option<NonNull<dyn Collectible>>,
    current_instance_link: Option<NonNull<dyn Collectible>>,
    next_instance_link: Option<NonNull<dyn Collectible>>,
    next_link: AtomicPtr<Collector>,
    link: Option<NonNull<dyn Collectible>>,
}

/// Data stored in a [`CollectorRoot`] is shared among [`Collector`] instances.
#[derive(Debug, Default)]
pub(super) struct CollectorRoot {
    epoch: AugmentedAtomicU8,
    chain_head: AugmentedAtomicPtr<Collector>,
}

/// [`GlobalRoot`] provides the globally accessible [`CollectorRoot`].
#[derive(Debug, Default)]
struct GlobalRoot(AtomicPtr<CollectorRoot>);

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

    /// Acknowledges a new [`Guard`](super::Guard) being instantiated.
    ///
    /// Returns `true` if a new epoch was announced.
    ///
    /// # Panics
    ///
    /// The method may panic if the number of readers has reached `u32::MAX`.
    #[inline]
    pub(super) fn new_guard(&mut self, collect_garbage: bool) {
        if self.num_readers == 0 {
            debug_assert_eq!(self.state.load(Relaxed) & Self::INACTIVE, Self::INACTIVE);
            self.num_readers = 1;
            let new_epoch = Epoch::from_u8(self.root.epoch.load(Relaxed));
            if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
                // This special optimization is excerpted from
                // [`crossbeam_epoch`](https://docs.rs/crossbeam-epoch/).
                //
                // The rationale behind the code is, it compiles to `lock xchg` that
                // practically acts as a full memory barrier on `X86`, and is much faster than
                // `mfence`.
                self.state.swap(new_epoch.into(), SeqCst);
            } else {
                // What will happen after the fence strictly happens after the fence.
                self.state.store(new_epoch.into(), Relaxed);
                augmented_fence(SeqCst);
            }
            if self.announcement != new_epoch {
                self.announcement = new_epoch;
                if collect_garbage {
                    let mut exit_guard = ExitGuard::new((self, false), |(guard, result)| {
                        if !result {
                            guard.end_guard();
                        }
                    });
                    exit_guard.0.epoch_updated();
                    exit_guard.1 = true;
                }
            }
        } else {
            debug_assert_eq!(self.state.load(Relaxed) & Self::INACTIVE, 0);
            assert_ne!(self.num_readers, u32::MAX, "Too many EBR guards");
            self.num_readers += 1;
        }
    }

    #[inline]
    /// Accelerates garbage collection.
    pub(super) fn accelerate(&mut self) {
        mark_scan_enforced(&self.root.chain_head);
        self.next_epoch_update = 0;
    }

    /// Acknowledges an existing [`Guard`](super::Guard) being dropped.
    #[inline]
    pub(super) fn end_guard(&mut self) {
        debug_assert_eq!(self.state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(self.state.load(Relaxed), u8::from(self.announcement));

        if self.num_readers == 1 {
            if self.next_epoch_update == 0 {
                if self.has_garbage
                    || Tag::into_tag(self.root.chain_head.load(Relaxed)) != Tag::First
                {
                    self.try_scan();
                }
                self.next_epoch_update = if self.has_garbage {
                    Self::CADENCE / 4
                } else {
                    Self::CADENCE
                };
            } else {
                self.next_epoch_update = self.next_epoch_update.saturating_sub(1);
            }

            // What has happened cannot be observed after the thread setting itself inactive has
            // been witnessed.
            self.state
                .store(u8::from(self.announcement) | Self::INACTIVE, Release);
            self.num_readers = 0;
        } else {
            self.num_readers -= 1;
        }
    }

    /// Returns the current epoch.
    #[inline]
    pub(super) fn current_epoch(&self) -> Epoch {
        // It is called by an active `Guard` therefore it is after a `SeqCst` memory barrier. Each
        // epoch update is preceded by another `SeqCst` memory barrier, therefore those two events
        // are globally ordered. If the `SeqCst` event during the `Guard` creation happened before
        // the other `SeqCst` event, this will either load the last previous epoch value, or the
        // current value. If not, it is guaranteed that it reads the latest global epoch value.
        Epoch::from_u8(self.root.epoch.load(Relaxed))
    }

    /// Reclaims garbage instances.
    #[inline]
    pub(super) fn reclaim(&mut self, instance_ptr: *mut dyn Collectible) {
        if let Some(mut ptr) = NonNull::new(instance_ptr) {
            unsafe {
                *ptr.as_mut().next_ptr_mut() = self.current_instance_link.take();
                self.current_instance_link.replace(ptr);
                self.next_epoch_update = self
                    .next_epoch_update
                    .saturating_sub(1)
                    .min(Self::CADENCE / 4);
                self.has_garbage = true;
            }
        }
    }

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

    /// Sets a [`Collector`] associated with the supplied [`CollectorRoot`] to the thread.
    ///
    /// # Safety
    ///
    /// It is unsafe. Never use this method outside test code.
    ///
    /// # Panics
    ///
    /// Panics if a [`Collector`] is previously set.
    #[cfg(loom_test)]
    pub(super) unsafe fn set_root(root: &Arc<CollectorRoot>) {
        LOCAL_COLLECTOR.with(|local_collector| {
            assert!(local_collector.load(Relaxed).is_null());
            let collector_ptr = Collector::alloc(root);
            local_collector.store(collector_ptr, Relaxed);
        });
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
                    mark_scan_enforced(&collector.root.chain_head);
                }
            }
            true
        })
    }

    /// Acknowledges a new global epoch.
    pub(super) fn epoch_updated(&mut self) {
        debug_assert_eq!(self.state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(self.state.load(Relaxed), u8::from(self.announcement));

        let mut garbage_link = self.next_instance_link.take();
        self.next_instance_link = self.previous_instance_link.take();
        self.previous_instance_link = self.current_instance_link.take();
        self.has_garbage =
            self.next_instance_link.is_some() || self.previous_instance_link.is_some();
        while let Some(mut instance_ptr) = garbage_link.take() {
            garbage_link = unsafe { *instance_ptr.as_mut().next_ptr_mut() };
            let mut guard = ExitGuard::new(garbage_link, |mut garbage_link| {
                while let Some(mut instance_ptr) = garbage_link.take() {
                    // Something went wrong during dropping and deallocating an instance.
                    garbage_link = unsafe { *instance_ptr.as_mut().next_ptr_mut() };

                    // Previous `drop_and_dealloc` may have accessed `self.current_instance_link`.
                    std::sync::atomic::compiler_fence(Acquire);
                    self.reclaim(instance_ptr.as_ptr());
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

    /// Tries to scan the [`Collector`] instances to update the global epoch.
    pub(super) fn try_scan(&mut self) -> bool {
        debug_assert_eq!(self.state.load(Relaxed) & Self::INACTIVE, 0);
        debug_assert_eq!(self.state.load(Relaxed), u8::from(self.announcement));

        // Only one thread that acquires the chain lock is allowed to scan the thread-local
        // collectors.
        let lock_result = self
            .root
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
        if let Ok(mut collector_ptr) = lock_result {
            let mut self_guard = ExitGuard::new(self, |a| {
                // Unlock the chain.
                loop {
                    let result = a.root.chain_head.fetch_update(Release, Relaxed, |p| {
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

            let known_epoch = self_guard.state.load(Relaxed);
            let mut update_global_epoch = true;
            let mut prev_collector_ptr: *mut Collector = ptr::null_mut();
            while !collector_ptr.is_null() {
                if ptr::eq(*self_guard, collector_ptr) {
                    prev_collector_ptr = collector_ptr;
                    collector_ptr = self_guard.next_link.load(Relaxed);
                    continue;
                }

                let collector_state = unsafe { (*collector_ptr).state.load(Relaxed) };
                let next_collector_ptr = unsafe { (*collector_ptr).next_link.load(Relaxed) };
                if (collector_state & Self::INVALID) != 0 {
                    // The collector is obsolete.
                    let result = if prev_collector_ptr.is_null() {
                        self_guard
                            .root
                            .chain_head
                            .fetch_update(Release, Relaxed, |p| {
                                let tag = Tag::into_tag(p);
                                debug_assert!(tag == Tag::First || tag == Tag::Both);
                                if ptr::eq(Tag::unset_tag(p), collector_ptr) {
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
                        self_guard.reclaim(collector_ptr);
                        collector_ptr = next_collector_ptr;
                        continue;
                    }
                } else if (collector_state & Self::INACTIVE) == 0 && collector_state != known_epoch
                {
                    // Not ready for an epoch update.
                    update_global_epoch = false;
                    break;
                }
                prev_collector_ptr = collector_ptr;
                collector_ptr = next_collector_ptr;
            }

            if update_global_epoch {
                // It is a new era; a fence is required.
                augmented_fence(SeqCst);
                self_guard
                    .root
                    .epoch
                    .store(Epoch::from_u8(known_epoch).next().into(), Relaxed);
                return true;
            }
        }

        false
    }

    /// Allocates a new [`Collector`].
    fn alloc(root: &Arc<CollectorRoot>) -> *mut Collector {
        let boxed = Box::new(Collector {
            state: AugmentedAtomicU8::new(Self::INACTIVE),
            announcement: Epoch::default(),
            next_epoch_update: Self::CADENCE,
            has_garbage: false,
            num_readers: 0,
            root: root.clone(),
            previous_instance_link: None,
            current_instance_link: None,
            next_instance_link: None,
            next_link: AtomicPtr::default(),
            link: None,
        });
        let ptr = Box::into_raw(boxed);
        let mut current = root.chain_head.load(Relaxed);
        loop {
            unsafe {
                (*ptr)
                    .next_link
                    .store(Tag::unset_tag(current).cast_mut(), Relaxed);
            }

            // It keeps the tag intact.
            let tag = Tag::into_tag(current);
            let new = Tag::update_tag(ptr, tag).cast_mut();
            if let Err(actual) = root
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
}

impl Drop for Collector {
    #[inline]
    fn drop(&mut self) {
        self.state.store(0, Relaxed);
        self.announcement = Epoch::default();
        while self.has_garbage {
            self.epoch_updated();
        }
    }
}

impl Collectible for Collector {
    #[inline]
    fn next_ptr_mut(&mut self) -> &mut Option<NonNull<dyn Collectible>> {
        &mut self.link
    }
}

impl GlobalRoot {
    fn alloc(&self) -> Arc<CollectorRoot> {
        let mut global_root_ptr = self.0.load(Relaxed);
        if global_root_ptr.is_null() {
            let root = Arc::<CollectorRoot>::default();
            let new_global_root_ptr = Arc::into_raw(root);
            if let Err(actual) = self.0.compare_exchange(
                global_root_ptr,
                new_global_root_ptr.cast_mut(),
                AcqRel,
                Acquire,
            ) {
                drop(unsafe { Arc::from_raw(new_global_root_ptr) });
                global_root_ptr = actual;
            } else {
                global_root_ptr = new_global_root_ptr.cast_mut();
            }
        }
        let global_root = unsafe { Arc::from_raw(global_root_ptr) };
        let global_root_clone_ptr = Arc::into_raw(global_root.clone());
        debug_assert_eq!(global_root_clone_ptr, global_root_ptr);
        global_root
    }
}

impl Drop for GlobalRoot {
    fn drop(&mut self) {
        let global_root = self.0.load(Relaxed);
        if !global_root.is_null() {
            drop(unsafe { Arc::from_raw(global_root) });
        }
    }
}

impl CollectorAnchor {
    fn alloc(&self) -> *mut Collector {
        let _: &CollectorAnchor = self;
        let root = GLOBAL_ROOT.alloc();
        Collector::alloc(&root)
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
fn mark_scan_enforced(chain_head: &AugmentedAtomicPtr<Collector>) {
    // `Tag::Second` indicates that there is a garbage `Collector`.
    let _result = chain_head.fetch_update(Release, Relaxed, |p| {
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
    let mut chain_head_ptr = (*collector_ptr).root.chain_head.load(Relaxed);
    if Tag::into_tag(chain_head_ptr) == Tag::Second {
        // Another thread was joined before, and has yet to be cleaned up.
        let guard = super::Guard::new_for_drop(collector_ptr);
        (*collector_ptr).try_scan();
        drop(guard);
        chain_head_ptr = (*collector_ptr).root.chain_head.load(Relaxed);
    }
    if (*collector_ptr).next_link.load(Relaxed).is_null()
        && ptr::eq(collector_ptr, chain_head_ptr)
        && (*collector_ptr)
            .root
            .chain_head
            .compare_exchange(chain_head_ptr, ptr::null_mut(), Relaxed, Relaxed)
            .is_ok()
    {
        // If it is the head, and the only `Collector` in the chain, drop it here.
        while (*collector_ptr).has_garbage {
            let guard = super::Guard::new_for_drop(collector_ptr);
            (*collector_ptr).epoch_updated();
            drop(guard);
        }
        drop(Box::from_raw(collector_ptr));
        return;
    }

    // Need to keep a strong reference to the root right after the collector is marked invalid.
    let root = (*collector_ptr).root.clone();
    (*collector_ptr).state.fetch_or(Collector::INVALID, Release);
    mark_scan_enforced(&root.chain_head);
}

augmented_thread_local! {
    #[allow(clippy::thread_local_initializer_can_be_made_const)]
    static COLLECTOR_ANCHOR: CollectorAnchor = CollectorAnchor;
    static LOCAL_COLLECTOR: AtomicPtr<Collector> = AtomicPtr::default();
}

/// The global and default [`CollectorRoot`].
static GLOBAL_ROOT: GlobalRoot = GlobalRoot(AtomicPtr::new(ptr::null_mut()));
