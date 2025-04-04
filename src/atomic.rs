use crate::{maybe_std::AtomicPtr, ref_counted::RefCounted, Guard, Owned, Ptr, Shared, Tag};
use std::{marker::PhantomData, mem, panic::UnwindSafe, ptr, sync::atomic::Ordering};

pub(super) mod ownership {
    use crate::ref_counted::RefCounted;

    pub(super) trait Type {
        const IS_OWNED: bool;

        fn generate_refcounted<T>(instance: T) -> *const RefCounted<T>;
    }

    pub struct Owned;

    impl Type for Owned {
        const IS_OWNED: bool = true;

        fn generate_refcounted<T>(instance: T) -> *const RefCounted<T> {
            RefCounted::new_unique(instance)
        }
    }

    pub struct Shared;

    impl Type for Shared {
        const IS_OWNED: bool = false;

        fn generate_refcounted<T>(instance: T) -> *const RefCounted<T> {
            RefCounted::new_shared(instance)
        }
    }
}

/// [`AtomicOwned`] owns the underlying instance, and allows users to perform atomic operations
/// on the pointer to it.
pub type AtomicOwned<T> = Atomic<T, ownership::Owned>;

/// [`AtomicShared`] owns the underlying instance, and allows users to perform atomic operations
/// on the pointer to it.
pub type AtomicShared<T> = Atomic<T, ownership::Shared>;

#[allow(private_bounds)]
/// [`Atomic`] owns the underlying instance, and allows users to perform atomic operations
/// on the pointer to it.
pub struct Atomic<T, O: ownership::Type>(AtomicPtr<RefCounted<T>>, PhantomData<O>);

#[allow(private_bounds)]
impl<T: 'static, O: ownership::Type> Atomic<T, O> {
    /// Creates a new [`Atomic`] from an instance of `T`.
    ///
    /// The type of the instance must be determined at compile-time, must not contain non-static
    /// references, and must not be a non-static reference since the instance can, theoretically,
    /// live as long as the process. For instance, `struct Disallowed<'l, T>(&'l T)` is not
    /// allowed, because an instance of the type cannot outlive `'l` whereas the garbage collector
    /// does not guarantee that the instance is dropped within `'l`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Atomic, AtomicOwned, AtomicShared};
    ///
    /// let atomic_owned: AtomicOwned<usize> = Atomic::new(10);   // OR: AtomicOwned::new(10)
    /// let atomic_shared: AtomicOwned<usize> = Atomic::new(10); // OR: AtomicOwned::new(10)
    /// ```
    #[inline]
    pub fn new(instance: T) -> Self {
        Atomic(
            AtomicPtr::new(O::generate_refcounted(instance).cast_mut()),
            PhantomData,
        )
    }
}

#[allow(private_bounds)]
impl<T, O: ownership::Type> Atomic<T, O> {
    /// Creates a null [`Atomic`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, AtomicShared};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// ```
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        Self(AtomicPtr::new(ptr::null_mut()), PhantomData)
    }

    /// Returns `true` if the [`Atomic`] is null.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, AtomicShared, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// atomic_owned.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed);
    /// assert!(atomic_owned.is_null(Relaxed));
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// atomic_shared.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed);
    /// assert!(atomic_shared.is_null(Relaxed));
    /// ```
    #[inline]
    #[must_use]
    pub fn is_null(&self, order: Ordering) -> bool {
        Tag::unset_tag(self.0.load(order)).is_null()
    }

    /// Loads a pointer value from the [`Atomic`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, AtomicShared, Guard};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(11);
    /// let guard = Guard::new();
    /// let ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 11);
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(11);
    /// let guard = Guard::new();
    /// let ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 11);
    /// ```
    #[inline]
    #[must_use]
    pub fn load<'g>(&self, order: Ordering, _: &'g Guard) -> Ptr<'g, T> {
        Ptr::from(self.0.load(order))
    }

    /// Returns its [`Tag`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, AtomicShared, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// assert_eq!(atomic_owned.tag(Relaxed), Tag::None);
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// assert_eq!(atomic_shared.tag(Relaxed), Tag::None);
    /// ```
    #[inline]
    #[must_use]
    pub fn tag(&self, order: Ordering) -> Tag {
        Tag::into_tag(self.0.load(order))
    }

    /// Sets a new [`Tag`] if the given condition is met.
    ///
    /// Returns `true` if the new [`Tag`] has been successfully set.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, AtomicShared, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// assert!(atomic_owned.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed));
    /// assert_eq!(atomic_owned.tag(Relaxed), Tag::Both);
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// assert!(atomic_shared.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed));
    /// assert_eq!(atomic_shared.tag(Relaxed), Tag::Both);
    /// ```
    #[inline]
    pub fn update_tag_if(
        &self,
        tag: Tag,
        mut condition: impl FnMut(Ptr<T>) -> bool,
        set_order: Ordering,
        fetch_order: Ordering,
    ) -> bool {
        self.0
            .fetch_update(set_order, fetch_order, |ptr| {
                condition(Ptr::from(ptr)).then(|| Tag::update_tag(ptr, tag).cast_mut())
            })
            .is_ok()
    }
}

unsafe impl<T: Send, O: ownership::Type> Send for Atomic<T, O> {}

unsafe impl<T: Sync, O: ownership::Type> Sync for Atomic<T, O> {}

impl<T: UnwindSafe, O: ownership::Type> UnwindSafe for Atomic<T, O> {}

impl<T> AtomicOwned<T> {
    /// Creates a new [`AtomicOwned`] from an [`Owned`] of `T`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, Owned};
    ///
    /// let owned: Owned<usize> = Owned::new(10);
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::from(owned);
    /// ```
    #[inline]
    #[must_use]
    pub const fn from(r#type: Owned<T>) -> Self {
        let ptr = r#type.underlying_ptr();
        mem::forget(r#type);

        Self(AtomicPtr::new(ptr.cast_mut()), PhantomData)
    }

    /// Stores the given value into the [`AtomicOwned`] and returns the original value.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, Guard, Owned, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(14);
    /// let guard = Guard::new();
    /// let (old, tag) = atomic_owned.swap((Some(Owned::new(15)), Tag::Second), Relaxed);
    /// assert_eq!(tag, Tag::None);
    /// assert_eq!(*old.unwrap(), 14);
    /// let (old, tag) = atomic_owned.swap((None, Tag::First), Relaxed);
    /// assert_eq!(tag, Tag::Second);
    /// assert_eq!(*old.unwrap(), 15);
    /// let (old, tag) = atomic_owned.swap((None, Tag::None), Relaxed);
    /// assert_eq!(tag, Tag::First);
    /// assert!(old.is_none());
    /// ```
    #[inline]
    pub fn swap(
        &self,
        (ptr, tag): (Option<Owned<T>>, Tag),
        order: Ordering,
    ) -> (Option<Owned<T>>, Tag) {
        let desired = Tag::update_tag(
            ptr.as_ref().map_or_else(ptr::null, Owned::underlying_ptr),
            tag,
        )
        .cast_mut();

        let previous = self.0.swap(desired, order);
        let tag = Tag::into_tag(previous);
        let previous_ptr = Tag::unset_tag(previous).cast_mut();
        mem::forget(ptr);

        (ptr::NonNull::new(previous_ptr).map(Owned::from), tag)
    }

    /// Stores `new` into the [`AtomicOwned`] if the current value is the same as `current`.
    ///
    /// Returns the previously held value and the updated [`Ptr`].
    ///
    /// # Errors
    ///
    /// Returns `Err` with the supplied [`Owned`] and the current [`Ptr`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, Guard, Owned, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// atomic_owned.update_tag_if(Tag::Both, |_| true, Relaxed, Relaxed);
    /// assert!(atomic_owned.compare_exchange(
    ///     ptr, (Some(Owned::new(18)), Tag::First), Relaxed, Relaxed, &guard).is_err());
    ///
    /// ptr.set_tag(Tag::Both);
    /// let old: Owned<usize> = atomic_owned.compare_exchange(
    ///     ptr,
    ///     (Some(Owned::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard).unwrap().0.unwrap();
    /// assert_eq!(*old, 17);
    /// drop(old);
    ///
    /// assert!(atomic_owned.compare_exchange(
    ///     ptr, (Some(Owned::new(19)), Tag::None), Relaxed, Relaxed, &guard).is_err());
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    /// ```
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn compare_exchange<'g>(
        &self,
        current: Ptr<'g, T>,
        (ptr, tag): (Option<Owned<T>>, Tag),
        success: Ordering,
        failure: Ordering,
        _: &'g Guard,
    ) -> Result<(Option<Owned<T>>, Ptr<'g, T>), (Option<Owned<T>>, Ptr<'g, T>)> {
        let desired = Tag::update_tag(
            ptr.as_ref().map_or_else(ptr::null, Owned::underlying_ptr),
            tag,
        )
        .cast_mut();

        match self.0.compare_exchange(
            current.as_underlying_ptr().cast_mut(),
            desired,
            success,
            failure,
        ) {
            Ok(previous) => {
                let previous = ptr::NonNull::new(Tag::unset_tag(previous).cast_mut());
                mem::forget(ptr);
                Ok((previous.map(Owned::from), Ptr::from(desired)))
            }
            Err(actual) => Err((ptr, Ptr::from(actual))),
        }
    }

    /// Stores `new` into the [`AtomicOwned`] if the current value is the same as `current`.
    ///
    /// This method is allowed to spuriously fail even when the comparison succeeds.
    ///
    /// Returns the previously held value and the updated [`Ptr`].
    ///
    /// # Errors
    ///
    /// Returns `Err` with the supplied [`Owned`] and the current [`Ptr`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, Guard, Owned, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// while let Err((_, actual)) = atomic_owned.compare_exchange_weak(
    ///     ptr,
    ///     (Some(Owned::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard) {
    ///     ptr = actual;
    /// }
    ///
    /// let mut ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 18);
    /// ```
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn compare_exchange_weak<'g>(
        &self,
        current: Ptr<'g, T>,
        (ptr, tag): (Option<Owned<T>>, Tag),
        success: Ordering,
        failure: Ordering,
        _: &'g Guard,
    ) -> Result<(Option<Owned<T>>, Ptr<'g, T>), (Option<Owned<T>>, Ptr<'g, T>)> {
        let desired = Tag::update_tag(
            ptr.as_ref().map_or_else(ptr::null, Owned::underlying_ptr),
            tag,
        )
        .cast_mut();

        match self.0.compare_exchange_weak(
            current.as_underlying_ptr().cast_mut(),
            desired,
            success,
            failure,
        ) {
            Ok(previous) => {
                let previous = ptr::NonNull::new(Tag::unset_tag(previous).cast_mut());
                mem::forget(ptr);
                Ok((previous.map(Owned::from), Ptr::from(desired)))
            }
            Err(actual) => Err((ptr, Ptr::from(actual))),
        }
    }

    /// Converts `self` into an [`Owned`].
    ///
    /// Returns `None` if `self` did not own an instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, Owned};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(55);
    /// let owned: Owned<usize> = atomic_owned.into_owned(Relaxed).unwrap();
    /// assert_eq!(*owned, 55);
    /// ```
    #[inline]
    #[must_use]
    pub fn into_owned(self, order: Ordering) -> Option<Owned<T>> {
        let ptr = self.0.swap(ptr::null_mut(), order);
        ptr::NonNull::new(Tag::unset_tag(ptr).cast_mut()).map(Owned::from)
    }
}

impl<T> AtomicShared<T> {
    /// Creates a new [`AtomicShared`] from a [`Shared`] of `T`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Shared};
    ///
    /// let shared: Shared<usize> = Shared::new(10);
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::from(shared);
    /// ```
    #[inline]
    #[must_use]
    pub const fn from(r#type: Shared<T>) -> Self {
        let ptr = r#type.underlying_ptr();
        mem::forget(r#type);

        Self(AtomicPtr::new(ptr.cast_mut()), PhantomData)
    }

    /// Stores the given value into the [`AtomicShared`] and returns the original value.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Guard, Shared, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(14);
    /// let guard = Guard::new();
    /// let (old, tag) = atomic_shared.swap((Some(Shared::new(15)), Tag::Second), Relaxed);
    /// assert_eq!(tag, Tag::None);
    /// assert_eq!(*old.unwrap(), 14);
    /// let (old, tag) = atomic_shared.swap((None, Tag::First), Relaxed);
    /// assert_eq!(tag, Tag::Second);
    /// assert_eq!(*old.unwrap(), 15);
    /// let (old, tag) = atomic_shared.swap((None, Tag::None), Relaxed);
    /// assert_eq!(tag, Tag::First);
    /// assert!(old.is_none());
    /// ```
    #[inline]
    pub fn swap(
        &self,
        (ptr, tag): (Option<Shared<T>>, Tag),
        order: Ordering,
    ) -> (Option<Shared<T>>, Tag) {
        let desired = Tag::update_tag(
            ptr.as_ref().map_or_else(ptr::null, Shared::underlying_ptr),
            tag,
        )
        .cast_mut();

        let previous = self.0.swap(desired, order);
        let tag = Tag::into_tag(previous);
        let previous_ptr = Tag::unset_tag(previous).cast_mut();
        mem::forget(ptr);

        (ptr::NonNull::new(previous_ptr).map(Shared::from), tag)
    }

    /// Stores `new` into the [`AtomicShared`] if the current value is the same as `current`.
    ///
    /// Returns the previously held value and the updated [`Ptr`].
    ///
    /// # Errors
    ///
    /// Returns `Err` with the supplied [`Shared`] and the current [`Ptr`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Guard, Shared, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// atomic_shared.update_tag_if(Tag::Both, |_| true, Relaxed, Relaxed);
    /// assert!(atomic_shared.compare_exchange(
    ///     ptr, (Some(Shared::new(18)), Tag::First), Relaxed, Relaxed, &guard).is_err());
    ///
    /// ptr.set_tag(Tag::Both);
    /// let old: Shared<usize> = atomic_shared.compare_exchange(
    ///     ptr,
    ///     (Some(Shared::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard).unwrap().0.unwrap();
    /// assert_eq!(*old, 17);
    /// drop(old);
    ///
    /// assert!(atomic_shared.compare_exchange(
    ///     ptr, (Some(Shared::new(19)), Tag::None), Relaxed, Relaxed, &guard).is_err());
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    /// ```
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn compare_exchange<'g>(
        &self,
        current: Ptr<'g, T>,
        (ptr, tag): (Option<Shared<T>>, Tag),
        success: Ordering,
        failure: Ordering,
        _: &'g Guard,
    ) -> Result<(Option<Shared<T>>, Ptr<'g, T>), (Option<Shared<T>>, Ptr<'g, T>)> {
        let desired = Tag::update_tag(
            ptr.as_ref().map_or_else(ptr::null, Shared::underlying_ptr),
            tag,
        )
        .cast_mut();

        match self.0.compare_exchange(
            current.as_underlying_ptr().cast_mut(),
            desired,
            success,
            failure,
        ) {
            Ok(previous) => {
                let previous = ptr::NonNull::new(Tag::unset_tag(previous).cast_mut());
                mem::forget(ptr);
                Ok((previous.map(Shared::from), Ptr::from(desired)))
            }
            Err(actual) => Err((ptr, Ptr::from(actual))),
        }
    }

    /// Stores `new` into the [`AtomicShared`] if the current value is the same as `current`.
    ///
    /// This method is allowed to spuriously fail even when the comparison succeeds.
    ///
    /// Returns the previously held value and the updated [`Ptr`].
    ///
    /// # Errors
    ///
    /// Returns `Err` with the supplied [`Shared`] and the current [`Ptr`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Guard, Shared, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// while let Err((_, actual)) = atomic_shared.compare_exchange_weak(
    ///     ptr,
    ///     (Some(Shared::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard) {
    ///     ptr = actual;
    /// }
    ///
    /// let mut ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 18);
    /// ```
    #[inline]
    #[allow(clippy::type_complexity)]
    pub fn compare_exchange_weak<'g>(
        &self,
        current: Ptr<'g, T>,
        (ptr, tag): (Option<Shared<T>>, Tag),
        success: Ordering,
        failure: Ordering,
        _: &'g Guard,
    ) -> Result<(Option<Shared<T>>, Ptr<'g, T>), (Option<Shared<T>>, Ptr<'g, T>)> {
        let desired = Tag::update_tag(
            ptr.as_ref().map_or_else(ptr::null, Shared::underlying_ptr),
            tag,
        )
        .cast_mut();

        match self.0.compare_exchange_weak(
            current.as_underlying_ptr().cast_mut(),
            desired,
            success,
            failure,
        ) {
            Ok(previous) => {
                let previous = ptr::NonNull::new(Tag::unset_tag(previous).cast_mut());
                mem::forget(ptr);
                Ok((previous.map(Shared::from), Ptr::from(desired)))
            }
            Err(actual) => Err((ptr, Ptr::from(actual))),
        }
    }

    /// Clones `self` including tags.
    ///
    /// If `self` is not supposed to be an `AtomicShared::null`, this will never return an
    /// `AtomicShared::null`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Guard};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(59);
    /// let guard = Guard::new();
    /// let atomic_shared_clone = atomic_shared.clone(Relaxed, &guard);
    /// let ptr = atomic_shared_clone.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 59);
    /// ```
    #[inline]
    #[must_use]
    pub fn clone(&self, order: Ordering, guard: &Guard) -> AtomicShared<T> {
        self.get_shared(order, guard)
            .map_or_else(Self::null, Self::from)
    }

    /// Tries to create a [`Shared`] out of `self`.
    ///
    /// If `self` is not supposed to be an `AtomicShared::null`, this will never return `None`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Guard, Shared};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(47);
    /// let guard = Guard::new();
    /// let shared: Shared<usize> = atomic_shared.get_shared(Relaxed, &guard).unwrap();
    /// assert_eq!(*shared, 47);
    /// ```
    #[inline]
    #[must_use]
    pub fn get_shared(&self, order: Ordering, _: &Guard) -> Option<Shared<T>> {
        let mut ptr = Tag::unset_tag(self.0.load(order));

        while !ptr.is_null() {
            if unsafe { (*ptr).try_add_ref(Ordering::Acquire) } {
                return ptr::NonNull::new(ptr.cast_mut()).map(Shared::from);
            }

            ptr = Tag::unset_tag(self.0.load(order));
        }

        None
    }

    /// Converts `self` into a [`Shared`].
    ///
    /// Returns `None` if `self` did not own an instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Shared};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(55);
    /// let owned: Shared<usize> = atomic_shared.into_shared(Relaxed).unwrap();
    /// assert_eq!(*owned, 55);
    /// ```
    #[inline]
    #[must_use]
    pub fn into_shared(self, order: Ordering) -> Option<Shared<T>> {
        let ptr = self.0.swap(ptr::null_mut(), order);
        ptr::NonNull::new(Tag::unset_tag(ptr).cast_mut()).map(Shared::from)
    }
}

impl<T> Clone for AtomicShared<T> {
    #[inline]
    fn clone(&self) -> AtomicShared<T> {
        self.clone(Ordering::Acquire, &Guard::new())
    }
}

impl<T, O: ownership::Type> Drop for Atomic<T, O> {
    #[inline]
    fn drop(&mut self) {
        let ptr = Tag::unset_tag(self.0.load(Ordering::Relaxed));
        if ptr.is_null() {
            return;
        }

        if O::IS_OWNED || unsafe { (*ptr).drop_ref() } {
            RefCounted::pass_to_collector(ptr.cast_mut());
        }
    }
}
