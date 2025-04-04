use std::{
    cmp::PartialEq,
    mem,
    ops::{Add, Not},
};

/// [`Tag`] is a four-state `Enum` that can be embedded in a pointer as the two least
/// significant bits of the pointer value.
#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Tag {
    /// None tagged.
    None,
    /// The first bit is tagged.
    First,
    /// The second bit is tagged.
    Second,
    /// Both bits are tagged.
    Both,
}

impl Tag {
    /// Returns the tag embedded in the pointer.
    #[inline]
    pub(super) fn into_tag<P>(ptr: *const P) -> Self {
        #[allow(clippy::missing_transmute_annotations)]
        unsafe {
            mem::transmute(ptr as usize & Tag::Both as usize)
        }
    }

    /// Sets a tag, overwriting any existing tag in the pointer.
    #[inline]
    pub(super) fn update_tag<P>(ptr: *const P, tag: Tag) -> *const P {
        (ptr as usize & !(Tag::Both as usize) | tag as usize) as *const P
    }

    /// Returns the pointer with the tag bits erased.
    #[inline]
    pub(super) fn unset_tag<P>(ptr: *const P) -> *const P {
        (ptr as usize & !(Tag::Both as usize)) as *const P
    }
}

impl Add for Tag {
    type Output = Tag;

    #[inline]
    fn add(self, rhs: Self) -> Tag {
        #[allow(clippy::missing_transmute_annotations)]
        unsafe {
            mem::transmute(self as usize + rhs as usize)
        }
    }
}

impl Not for Tag {
    type Output = usize;

    #[inline]
    fn not(self) -> usize {
        !(self as usize)
    }
}
