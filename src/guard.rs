use super::collectible::DeferredClosure;
use super::collector::Collector;
use super::{Collectible, Epoch};
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

    /// Returns the epoch in which the current thread lives.
    ///
    /// This method can be used to check whether a retired memory region is potentially reachable or
    /// not. A chunk of memory retired in a witnessed [`Epoch`] can be deallocated after the thread
    /// has observed three new epochs. For instance, if the witnessed epoch value is `1` in the
    /// current thread where the global epoch value is `2`, and an instance of a [`Collectible`]
    /// type is retired in the same thread, the instance can be dropped when the thread witnesses
    /// `0` which is three epochs away from `1`.
    ///
    /// In other words, there can be potential readers of the memory chunk until the current thread
    /// witnesses the previous epoch. In the above example, the global epoch can be in `2`
    /// while the current thread has only witnessed `1`, and therefore there can a reader of the
    /// memory chunk in another thread in epoch `2`. The reader can survive until the global epoch
    /// reaches `0` again, because the thread being in `2` prevents the global epoch from reaching
    /// `0`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Owned};
    /// use std::sync::atomic::AtomicBool;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// static DROPPED: AtomicBool = AtomicBool::new(false);
    ///
    /// struct D(&'static AtomicBool);
    ///
    /// impl Drop for D {
    ///     fn drop(&mut self) {
    ///         self.0.store(true, Relaxed);
    ///     }
    /// }
    ///
    /// let owned = Owned::new(D(&DROPPED));
    ///
    /// let epoch_before = Guard::new().epoch();
    ///
    /// drop(owned);
    /// assert!(!DROPPED.load(Relaxed));
    ///
    /// while Guard::new().epoch() == epoch_before {
    ///     assert!(!DROPPED.load(Relaxed));
    /// }
    ///
    /// while Guard::new().epoch() == epoch_before.next() {
    ///     assert!(!DROPPED.load(Relaxed));
    /// }
    ///
    /// while Guard::new().epoch() == epoch_before.next().next() {
    ///     assert!(!DROPPED.load(Relaxed));
    /// }
    ///
    /// assert!(DROPPED.load(Relaxed));
    /// assert_eq!(Guard::new().epoch(), epoch_before.prev());
    ///
    /// ```
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        Collector::current_epoch()
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
