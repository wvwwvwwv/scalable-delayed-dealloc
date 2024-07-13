use std::ptr::NonNull;

/// [`Collectible`] defines key methods for `Self` to be reclaimed by the EBR garbage collector.
///
/// The module provides managed handles which implement [`Collectible`] in tandem with atomic
/// reference counting, however it is also possible to manually implement the [`Collectible`] trait
/// for a type to pass an instance of the type to the EBR garbage collector via
/// [`Guard::defer`](super::Guard::defer).
///
/// # Examples
///
/// ```
/// use sdd::{Guard, Collectible};
/// use std::ptr::NonNull;
///
/// struct LazyString(String, Option<NonNull<dyn Collectible>>);
///
/// impl Collectible for LazyString {
///     fn next_ptr_mut(&mut self) -> &mut Option<NonNull<dyn Collectible>> {
///         &mut self.1
///     }
/// }
///
/// let boxed: Box<LazyString> = Box::new(LazyString(String::from("Lazy"), None));
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
    /// Returns a mutable reference to the next [`Collectible`] pointer.
    fn next_ptr_mut(&mut self) -> &mut Option<NonNull<dyn Collectible>>;
}

/// [`DeferredClosure`] implements [`Collectible`] for a closure to execute it after all the
/// readers in the process at the moment are gone.
pub(super) struct DeferredClosure<F: 'static + FnOnce()> {
    f: Option<F>,
    link: Option<NonNull<dyn Collectible>>,
}

impl<F: 'static + FnOnce()> DeferredClosure<F> {
    /// Creates a new [`DeferredClosure`].
    #[inline]
    pub fn new(f: F) -> Self {
        DeferredClosure {
            f: Some(f),
            link: None,
        }
    }
}

impl<F: 'static + FnOnce()> Collectible for DeferredClosure<F> {
    #[inline]
    fn next_ptr_mut(&mut self) -> &mut Option<NonNull<dyn Collectible>> {
        &mut self.link
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
