//! Which of the hashes a cell stores are stale.

use std::fmt;

use lignin_format::tree_page::{HashField, Side};

/// A set of the hashes one cell stores: a flag for each [`HashField`].
///
/// As the marks of a node it says which of its stored hashes are stale. As
/// the answer of [`check`](super::check) it says which differ from their
/// recomputation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Stale(u8);

impl Stale {
    /// No hash.
    pub const NONE: Self = Self(0);

    /// Every hash a cell can store.
    pub const ALL: Self = Self(0b1111);

    const fn bit(field: HashField) -> u8 {
        match field {
            HashField::Value => 1 << 0,
            HashField::KeyValue => 1 << 1,
            HashField::Child(Side::Left) => 1 << 2,
            HashField::Child(Side::Right) => 1 << 3,
        }
    }

    /// The set of `field` alone.
    pub const fn of(field: HashField) -> Self {
        Self(Self::bit(field))
    }

    /// Whether `field` is in the set.
    pub const fn has(self, field: HashField) -> bool {
        self.0 & Self::bit(field) != 0
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// How many hashes are in the set.
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// The set with `field`.
    #[must_use]
    pub const fn with(self, field: HashField) -> Self {
        Self(self.0 | Self::bit(field))
    }

    /// The set without `field`.
    #[must_use]
    pub const fn without(self, field: HashField) -> Self {
        Self(self.0 & !Self::bit(field))
    }

    /// The hashes of either set.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every hash of `other` is in the set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The hashes of the set, in the order of [`HashField::ALL`].
    pub fn fields(self) -> impl Iterator<Item = HashField> {
        HashField::ALL
            .into_iter()
            .filter(move |field| self.has(*field))
    }
}

/// The name of a stored hash, for a message.
pub(super) const fn name(field: HashField) -> &'static str {
    match field {
        HashField::Value => "value hash",
        HashField::KeyValue => "key-value hash",
        HashField::Child(Side::Left) => "hash of the left child",
        HashField::Child(Side::Right) => "hash of the right child",
    }
}

impl fmt::Debug for Stale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.fields().map(name)).finish()
    }
}

impl From<HashField> for Stale {
    fn from(field: HashField) -> Self {
        Self::of(field)
    }
}

impl FromIterator<HashField> for Stale {
    fn from_iter<I: IntoIterator<Item = HashField>>(fields: I) -> Self {
        fields.into_iter().fold(Self::NONE, Self::with)
    }
}
