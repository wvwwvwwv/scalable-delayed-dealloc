/// [`Epoch`] represents the period of time the global epoch value stays the same.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord)]
pub struct Epoch {
    value: u8,
}

impl Epoch {
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
    #[inline]
    #[must_use]
    pub fn next(self) -> Epoch {
        match self.value {
            0 => Epoch { value: 1 },
            1 => Epoch { value: 2 },
            _ => Epoch { value: 0 },
        }
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
    #[inline]
    #[must_use]
    pub fn prev(self) -> Epoch {
        match self.value {
            0 => Epoch { value: 2 },
            1 => Epoch { value: 0 },
            _ => Epoch { value: 1 },
        }
    }

    /// Construct an [`Epoch`] from a [`u8`] value.
    #[inline]
    pub(super) fn from_u8(value: u8) -> Epoch {
        Epoch { value }
    }
}

impl From<Epoch> for u8 {
    #[inline]
    fn from(epoch: Epoch) -> Self {
        epoch.value
    }
}
