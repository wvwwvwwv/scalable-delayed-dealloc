use super::collectible::{Collectible, DeferredClosure};
use super::collector::Collector;
use std::panic::UnwindSafe;

/// [`Guard`] allows the user to read [`AtomicShared`](super::AtomicShared) and keeps the
/// underlying instance pinned to the thread.
///
/// [`Guard`] internally prevents the global epoch value from passing through the value
/// announced by the current thread, thus keeping reachable instances in the thread from being
/// garbage collected.
pub struct Guard {
    collector_ptr: *mut Collector,
}

impl Guard {
    /// Creates a new [`Guard`].
    ///
    /// # Panics
    ///
    /// The maximum number of [`Guard`] instances in a thread is limited to `u32::MAX`; a
    /// thread panics when the number of [`Guard`] instances in the thread exceeds the limit.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Guard;
    ///
    /// let guard = Guard::new();
    /// ```
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        let collector_ptr = Collector::current();
        unsafe { (*collector_ptr).new_guard(true) };
        Self { collector_ptr }
    }

    /// Defers dropping and memory reclamation of the supplied [`Box`] of a type implementing
    /// [`Collectible`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Collectible};
    /// use std::ptr::NonNull;
    ///
    /// struct C(usize, Option<NonNull<dyn Collectible>>);
    ///
    /// impl Collectible for C {
    ///     fn next_ptr_mut(&mut self) -> &mut Option<NonNull<dyn Collectible>> {
    ///         &mut self.1
    ///     }
    /// }
    ///
    /// let boxed: Box<C> = Box::new(C(7, None));
    ///
    /// let static_ref: &'static C = unsafe { std::mem::transmute(&*boxed) };
    ///
    /// let guard = Guard::new();
    /// guard.defer(boxed);
    ///
    /// assert_eq!(static_ref.0, 7);
    /// ```
    #[inline]
    pub fn defer(&self, collectible: Box<dyn Collectible>) {
        self.collect(Box::into_raw(collectible));
    }

    /// Executes the supplied closure at a later point of time.
    ///
    /// It is guaranteed that the closure will be executed after every [`Guard`] at the moment when
    /// the method was invoked is dropped, however it is totally non-deterministic when exactly the
    /// closure will be executed.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Guard;
    ///
    /// let guard = Guard::new();
    /// guard.defer_execute(|| println!("deferred"));
    /// ```
    #[inline]
    pub fn defer_execute<F: 'static + FnOnce()>(&self, f: F) {
        self.defer(Box::new(DeferredClosure::new(f)));
    }

    /// Creates a new [`Guard`] for dropping an instance in the supplied [`Collector`].
    #[inline]
    pub(super) fn new_for_drop(collector_ptr: *mut Collector) -> Self {
        unsafe {
            (*collector_ptr).new_guard(false);
        }
        Self { collector_ptr }
    }

    /// Reclaims the supplied instance.
    #[inline]
    pub(super) fn collect(&self, collectible: *mut dyn Collectible) {
        unsafe {
            (*self.collector_ptr).reclaim(collectible);
        }
    }
}

impl Default for Guard {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            (*self.collector_ptr).end_guard();
        }
    }
}

impl UnwindSafe for Guard {}
