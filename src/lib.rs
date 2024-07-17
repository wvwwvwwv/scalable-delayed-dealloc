#![deny(missing_docs, warnings, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

mod atomic_owned;
pub use atomic_owned::AtomicOwned;

mod atomic_shared;
pub use atomic_shared::AtomicShared;

mod guard;
pub use guard::Guard;

mod collectible;
pub use collectible::Collectible;

mod epoch;
pub use epoch::Epoch;

mod owned;
pub use owned::Owned;

mod ptr;
pub use ptr::Ptr;

mod shared;
pub use shared::Shared;

mod tag;
pub use tag::Tag;

mod collector;
mod exit_guard;
mod ref_counted;

/// Prepares a garbage collector for the current thread.
///
/// This method is useful in an environment where heap memory allocation is strictly controlled.
/// [`Guard::new`] will never fail afterwards in the current thread until [`suspend`] is called as
/// long as [`drop`] of every [`Collectible`] type is infallible.
///
/// # Panics
///
/// Panics if memory allocation failed.
///
/// # Examples
///
/// ```
/// use sdd::{prepare, Guard};
///
/// prepare();
///
/// let guard = Guard::new();
/// ```
#[inline]
pub fn prepare() {
    let guard = Guard::new();
    drop(guard);
}

/// Suspends the garbage collector of the current thread.
///
/// If returns `false` if there is an active [`Guard`] in the thread. Otherwise, it passes all its
/// retired instances to a free flowing garbage container that can be cleaned up by other threads.
///
/// # Examples
///
/// ```
/// use sdd::{suspend, Guard, Shared};
///
/// assert!(suspend());
///
/// {
///     let shared: Shared<usize> = Shared::new(47);
///     let guard = Guard::new();
///     shared.release(&guard);
///     assert!(!suspend());
/// }
///
/// assert!(suspend());
///
/// let new_shared: Shared<usize> = Shared::new(17);
/// let guard = Guard::new();
/// new_shared.release(&guard);
/// ```
#[inline]
#[must_use]
pub fn suspend() -> bool {
    collector::Collector::pass_garbage()
}

#[cfg(not(feature = "sdd_loom"))]
mod optional_public {
    pub(crate) use std::sync::atomic::AtomicPtr;
}

#[cfg(feature = "sdd_loom")]
mod optional_public {
    pub(crate) use loom::sync::atomic::AtomicPtr;
}

#[cfg(not(loom_test))]
mod optional_private {
    pub(crate) use std::sync::atomic::{fence, AtomicPtr, AtomicU8};
    pub(crate) use std::thread_local;
}

#[cfg(loom_test)]
mod optional_private {
    pub(crate) use loom::sync::atomic::{fence, AtomicPtr, AtomicU8};
    pub(crate) use loom::thread_local;
}

#[cfg(test)]
mod tests;
