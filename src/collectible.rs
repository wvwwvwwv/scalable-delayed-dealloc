use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicPtr, AtomicUsize};

/// [`Collectible`] defines the memory layout for the type in order to be passed to the garbage
/// collector.
pub(super) trait Collectible {
    /// Returns the next [`Collectible`] pointer.
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>>;

    /// Sets the next [`Collectible`] pointer.
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>);
}

/// [`Link`] implements [`Collectible`].
#[derive(Debug, Default)]
pub struct Link {
    ref_cnt: AtomicUsize,
    ptr: AtomicPtr<usize>,
}

/// [`DeferredClosure`] implements [`Collectible`] for a closure to execute it after all the
/// current readers in the process are gone.
pub(super) struct DeferredClosure<F: 'static + FnOnce()> {
    f: Option<F>,
    link: Link,
}

impl Link {
    #[inline]
    pub(super) const fn new_shared() -> Self {
        Link {
            ref_cnt: AtomicUsize::new(1),
            ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }

    #[inline]
    pub(super) const fn new_unique() -> Self {
        Link {
            ref_cnt: AtomicUsize::new(0),
            ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }

    #[inline]
    pub(super) const fn ref_cnt(&self) -> &AtomicUsize {
        &self.ref_cnt
    }
}

impl Collectible for Link {
    #[inline]
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>> {
        let fat_ptr: (*mut usize, *mut usize) = (
            self.ref_cnt.load(Relaxed) as *mut usize,
            self.ptr.load(Relaxed),
        );

        #[allow(clippy::missing_transmute_annotations)]
        unsafe {
            std::mem::transmute(fat_ptr)
        }
    }

    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    #[inline]
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>) {
        let (ref_cnt, ptr): (*mut usize, *mut usize) = next_ptr.map_or_else(
            || (ptr::null_mut(), ptr::null_mut()),
            #[allow(clippy::missing_transmute_annotations)]
            |p| unsafe { std::mem::transmute(p) },
        );

        self.ref_cnt.store(ref_cnt as usize, Relaxed);
        self.ptr.store(ptr, Relaxed);
    }
}

impl<F: 'static + FnOnce()> DeferredClosure<F> {
    /// Creates a new [`DeferredClosure`].
    #[inline]
    pub fn new(f: F) -> Self {
        DeferredClosure {
            f: Some(f),
            link: Link::default(),
        }
    }
}

impl<F: 'static + FnOnce()> Collectible for DeferredClosure<F> {
    #[inline]
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>> {
        self.link.next_ptr()
    }

    #[inline]
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>) {
        self.link.set_next_ptr(next_ptr);
    }
}

impl<F: 'static + FnOnce()> Drop for DeferredClosure<F> {
    #[inline]
    fn drop(&mut self) {
        let Some(f) = self.f.take() else {
            return;
        };

        f();
    }
}
