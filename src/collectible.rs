use std::ptr::NonNull;

/// [`Collectible`] defines the memory layout for the type in order to be passed to the garbage
/// collector.
///
/// # Examples
///
/// ```
/// use sdd::{Collectible, Guard};
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
/// current readers in the process are gone.
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
