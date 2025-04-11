/// [`Epoch`] represents the period of time the global epoch value stays the same.
///
/// [`Epoch`] rotates [`u8`] values in a range of `[0..3]` while the crate itself functions
/// correctly if the range is limited to `[0..2]`. The one additional state is useful for users to
/// determine whether a certain memory chunk can be deallocated or not by using values returned
/// from [`Guard::epoch`](crate::Guard::epoch), e.g., if an [`Owned`](crate::Owned) was retired in
/// epoch `1`, then the [`Owned`](crate::Owned) will become completely unreachable in epoch `0`.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Epoch(u8);

impl Epoch {
    /// This crate uses `4` epoch values.
    const EPOCHS: u8 = 3; // For use with AND instead of modulo.

    /// Returns a future [`Epoch`] when the current readers will not be present.
    ///
    /// The current [`Epoch`] may lag behind the global epoch value by `1`, therefore this method
    /// returns an [`Epoch`] three epochs next to `self`.
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let next_generation = initial.next_generation();
    /// assert_eq!(next_generation, initial.next().next().next());
    /// ```
    #[inline]
    #[must_use]
    pub const fn next_generation(self) -> Epoch {
        self.prev()
    }

    /// Returns the next [`Epoch`] value.
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let next = initial.next();
    /// assert!(initial < next);
    ///
    /// let next_next = next.next();
    /// assert!(next < next_next);
    /// ```
    #[allow(clippy::precedence)]
    #[inline]
    #[must_use]
    pub const fn next(self) -> Epoch {
        Epoch(self.0 + 1 & Self::EPOCHS)
    }

    /// Returns the previous [`Epoch`] value.
    ///
    /// ```
    /// use sdd::Epoch;
    ///
    /// let initial = Epoch::default();
    ///
    /// let prev = initial.prev();
    /// assert!(initial < prev);
    ///
    /// let prev_prev = prev.prev();
    /// assert!(prev_prev < prev);
    /// ```
    #[allow(clippy::precedence)]
    #[inline]
    #[must_use]
    pub const fn prev(self) -> Epoch {
        Epoch(self.0 + Self::EPOCHS & Self::EPOCHS)
    }
}

impl From<Epoch> for u8 {
    #[inline]
    fn from(epoch: Epoch) -> Self {
        epoch.0
    }
}

impl From<u8> for Epoch {
    #[inline]
    fn from(value: u8) -> Self {
        Epoch(value)
    }
}
