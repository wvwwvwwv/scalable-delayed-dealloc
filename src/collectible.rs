use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicPtr, AtomicUsize};

/// [`Collectible`] defines the memory layout for the type in order to be passed to the garbage
/// collector.
///
/// # Examples
///
/// ```
/// use sdd::{Collectible, Guard, Link};
/// use std::ptr::NonNull;
///
/// struct LazyString(String, Link);
///
/// impl Collectible for LazyString {
///     fn next_ptr(&self) -> Option<NonNull<dyn Collectible>> {
///         self.1.next_ptr()
///     }
///     fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>) {
///         self.1.set_next_ptr(next_ptr);
///     }
/// }
///
/// let boxed: Box<LazyString> = Box::new(LazyString(String::from("Lazy"), Link::default()));
///
/// let static_ref: &'static LazyString = unsafe { std::mem::transmute(&*boxed) };
/// let guard_for_ref = Guard::new();
///
/// let guard_to_drop = Guard::new();
/// guard_to_drop.defer(boxed);
/// drop(guard_to_drop);
///
/// // The reference is valid as long as a `Guard` that had been created before `boxed` was
/// // passed to a `Guard` survives.
/// assert_eq!(static_ref.0, "Lazy");
/// ```
pub trait Collectible {
    /// Returns the next [`Collectible`] pointer.
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>>;

    /// Sets the next [`Collectible`] pointer.
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>);
}

/// [`Link`] implements [`Collectible`].
#[derive(Debug, Default)]
pub struct Link {
    data: (AtomicUsize, AtomicPtr<usize>),
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
            data: (AtomicUsize::new(1), AtomicPtr::new(ptr::null_mut())),
        }
    }

    #[inline]
    pub(super) const fn new_unique() -> Self {
        Link {
            data: (AtomicUsize::new(0), AtomicPtr::new(ptr::null_mut())),
        }
    }

    #[inline]
    pub(super) const fn ref_cnt(&self) -> &AtomicUsize {
        &self.data.0
    }
}

impl Collectible for Link {
    #[inline]
    fn next_ptr(&self) -> Option<NonNull<dyn Collectible>> {
        let fat_ptr: (*mut usize, *mut usize) = (
            self.data.0.load(Relaxed) as *mut usize,
            self.data.1.load(Relaxed),
        );
        unsafe { std::mem::transmute(fat_ptr) }
    }

    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    #[inline]
    fn set_next_ptr(&self, next_ptr: Option<NonNull<dyn Collectible>>) {
        let data: (*mut usize, *mut usize) = next_ptr.map_or_else(
            || (ptr::null_mut(), ptr::null_mut()),
            |p| unsafe { std::mem::transmute(p) },
        );
        self.data.0.store(data.0 as usize, Relaxed);
        self.data.1.store(data.1, Relaxed);
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
        if let Some(f) = self.f.take() {
            f();
        }
    }
}
