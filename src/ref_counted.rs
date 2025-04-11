use crate::collectible::{Collectible, Link};
use crate::collector::Collector;
use std::mem::offset_of;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, Relaxed};

/// [`RefCounted`] stores an instance of type `T`, and a union of a link to the next
/// [`Collectible`] or the reference counter.
pub(super) struct RefCounted<T> {
    instance: T,
    next_or_refcnt: Link,
}

impl<T> RefCounted<T> {
    /// Creates a new [`RefCounted`] that allows ownership sharing.
    #[inline]
    pub(super) fn new_shared(instance: T) -> *const RefCounted<T> {
        let boxed = Box::new(Self {
            instance,
            next_or_refcnt: Link::new_shared(),
        });

        Box::into_raw(boxed)
    }

    /// Creates a new [`RefCounted`] that disallows reference counting.
    ///
    /// The reference counter field is never used until the instance is retired.
    #[inline]
    pub(super) fn new_unique(instance: T) -> *const RefCounted<T> {
        let boxed = Box::new(Self {
            instance,
            next_or_refcnt: Link::new_unique(),
        });

        Box::into_raw(boxed)
    }

    /// Tries to add a strong reference to the underlying instance.
    ///
    /// `order` must be as strong as `Acquire` for the caller to correctly validate the newest
    /// state of the pointer.
    #[inline]
    pub(super) fn try_add_ref(&self, order: Ordering) -> bool {
        self.ref_cnt()
            .fetch_update(order, order, |r| (r & 1 == 1).then_some(r + 2))
            .is_ok()
    }

    /// Returns a mutable reference to the instance if the number of owners is `1`.
    #[inline]
    pub(super) fn get_mut_shared(&mut self) -> Option<&mut T> {
        let value = self.ref_cnt().load(Relaxed);
        (value == 1).then_some(&mut self.instance)
    }

    /// Returns a mutable reference to the instance if it is uniquely owned.
    #[inline]
    pub(super) fn get_mut_unique(&mut self) -> &mut T {
        debug_assert_eq!(self.ref_cnt().load(Relaxed), 0);
        &mut self.instance
    }

    /// Adds a strong reference to the underlying instance.
    #[inline]
    pub(super) fn add_ref(&self) {
        self.ref_cnt().fetch_add(2, Relaxed);
    }

    /// Drops a strong reference to the underlying instance.
    ///
    /// Returns `true` if it the last reference was dropped.
    #[inline]
    pub(super) fn drop_ref(&self) -> bool {
        // It does not have to be a load-acquire as everything's synchronized via the global
        // epoch.
        let mut current = self.ref_cnt().load(Relaxed);
        while let Err(updated) = self.ref_cnt().compare_exchange_weak(
            current,
            current.saturating_sub(2),
            Relaxed,
            Relaxed,
        ) {
            current = updated;
        }

        current == 1
    }

    /// Returns a pointer to the instance.
    #[inline]
    pub(super) fn inst_ptr(self_ptr: *const Self) -> *const T {
        let offset = offset_of!(Self, instance);
        #[allow(clippy::cast_lossless)]
        let is_valid = !self_ptr.is_null() as usize;
        unsafe { self_ptr.cast::<u8>().add(offset * is_valid).cast() }
    }

    /// Returns a reference to its reference count.
    #[inline]
    pub(super) fn ref_cnt(&self) -> &AtomicUsize {
        self.next_or_refcnt.ref_cnt()
    }

    /// Passes a pointer to [`RefCounted`] to the garbage collector.
    #[inline]
    pub(super) fn pass_to_collector(ptr: *mut Self) {
        unsafe {
            Collector::collect(Collector::current(), ptr as *mut dyn Collectible);
        }
    }
}

impl<T> Deref for RefCounted<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.instance
    }
}

impl<T> Collectible for RefCounted<T> {
    #[inline]
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>> {
        self.next_or_refcnt.next_ptr()
    }

    #[inline]
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>) {
        self.next_or_refcnt.set_next_ptr(next_ptr);
    }
}
