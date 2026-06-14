//! Typed, opaque identifiers.
//!
//! [`Id<T>`] is a `u64` newtype tagged with a marker type so that, say, a
//! `Id<TabMarker>` cannot be accidentally used where a `Id<ProcessMarker>` is
//! expected. [`IdAllocator<T>`] hands out monotonically increasing ids.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// An opaque identifier tagged with marker type `T`.
pub struct Id<T> {
    raw: u64,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    /// Wrap a raw value. Prefer [`IdAllocator`] for fresh ids.
    pub const fn from_raw(raw: u64) -> Self {
        Id {
            raw,
            _marker: PhantomData,
        }
    }

    /// The underlying value.
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

// Manual impls so they don't require `T: Trait` (derives would add bad bounds).
impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Id<T> {}
impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl<T> Eq for Id<T> {}
impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.raw.cmp(&other.raw)
    }
}
impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}
impl<T> std::fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.raw)
    }
}

/// Hands out fresh [`Id<T>`] values, starting at 1 (0 is reserved as a niche).
pub struct IdAllocator<T> {
    next: AtomicU64,
    _marker: PhantomData<fn() -> T>,
}

impl<T> IdAllocator<T> {
    /// A new allocator whose first id will be 1.
    pub const fn new() -> Self {
        IdAllocator {
            next: AtomicU64::new(1),
            _marker: PhantomData,
        }
    }

    /// Allocate the next id.
    pub fn alloc(&self) -> Id<T> {
        Id::from_raw(self.next.fetch_add(1, AtomicOrdering::Relaxed))
    }
}

impl<T> Default for IdAllocator<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum TabMarker {}
    enum ProcMarker {}

    #[test]
    fn allocator_is_monotonic_from_one() {
        let alloc = IdAllocator::<TabMarker>::new();
        assert_eq!(alloc.alloc().raw(), 1);
        assert_eq!(alloc.alloc().raw(), 2);
        assert_eq!(alloc.alloc().raw(), 3);
    }

    #[test]
    fn ids_are_copy_and_comparable() {
        let a = Id::<ProcMarker>::from_raw(7);
        let b = a;
        assert_eq!(a, b);
        assert!(Id::<ProcMarker>::from_raw(1) < Id::<ProcMarker>::from_raw(2));
    }
}
