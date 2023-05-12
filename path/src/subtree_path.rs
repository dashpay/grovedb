// MIT LICENSE
//
// Copyright (c) 2023 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Difinitions of versatile type representing a path to a subtree.

use std::hash::{Hash, Hasher};

use crate::{
    util::{CowLike, TwoDimensionalBytes},
    SubtreePathIter,
};

/// Path to a GroveDB's subtree.
#[derive(Debug)]
pub struct SubtreePath<'b, B> {
    /// Derivation starting point.
    base: SubtreePathBase<'b, B>,
    /// Path information relative to [base](Self::base).
    relative: SubtreePathRelative<'b>,
}

/// Does what derived implementation would do, but moving trait bounds away from
/// structure definition.
impl<'b, B: AsRef<[u8]>> Hash for SubtreePath<'b, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.base.hash(state);
        self.relative.hash(state);
    }
}

impl<'bl, 'br, BL, BR> PartialEq<SubtreePath<'br, BR>> for SubtreePath<'bl, BL>
where
    BL: AsRef<[u8]>,
    BR: AsRef<[u8]>,
{
    fn eq(&self, other: &SubtreePath<'br, BR>) -> bool {
        self.reverse_iter().eq(other.reverse_iter())
    }
}

impl<'b, B: AsRef<[u8]>> Eq for SubtreePath<'b, B> {}

/// A variant of a subtree path from which the new path is derived.
/// The new path is reusing the existing one instead of owning a copy of the
/// same data.
#[derive(Debug)]
pub(crate) enum SubtreePathBase<'b, B> {
    /// Owned subtree path
    Owned(TwoDimensionalBytes),
    /// The base path is a slice, might a provided by user or a subslice when
    /// deriving a parent.
    Slice(&'b [B]),
    /// If the subtree path base cannot be represented as a subset of initially
    /// provided slice, which is handled by [Slice](Self::Slice), this variant
    /// is used to refer to other derived path.
    DerivedPath(&'b SubtreePath<'b, B>),
}

impl<'b, B: AsRef<[u8]>> Hash for SubtreePathBase<'b, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Slice(slice) => slice.iter().map(AsRef::as_ref).for_each(|s| s.hash(state)),
            Self::DerivedPath(path) => path.hash(state),
            Self::Owned(bytes) => bytes.into_iter().for_each(|s| s.hash(state)),
        }
    }
}

// /// For the same reason as for `Hash` implementation, derived impl requires
// /// generics to carry /// trait bounds that actually don't needed.
// impl<B> Clone for SubtreePathBase<'_, B> {
//     fn clone(&self) -> Self {
//         match self {
//             Self::Slice(x) => Self::Slice(x),
//             Self::DerivedPath(x) => Self::DerivedPath(x),
//         }
//     }
// }

// /// Base path doesn't have any owned data and basically a pointer, so it's
// cheap /// to be [Copy].
// impl<B> Copy for SubtreePathBase<'_, B> {}

impl<'b, B: AsRef<[u8]>> SubtreePathBase<'b, B> {
    /// Get a derived path that will reuse this [Self] as it's base path.
    fn derive(&self) -> SubtreePath<'b, B> {
        match self {
            SubtreePathBase::Slice(s) => SubtreePath {
                base: SubtreePathBase::Slice(s),
                relative: SubtreePathRelative::Empty,
            },
            SubtreePathBase::DerivedPath(path) => SubtreePath {
                base: SubtreePathBase::DerivedPath(path),
                relative: SubtreePathRelative::Empty,
            },
            _ => todo!(),
        }
    }

    /// Get a derived subtree path for a parent with care for base path slice
    /// case.
    fn derive_parent(&self) -> Option<(SubtreePath<'b, B>, &'b [u8])> {
        match self {
            SubtreePathBase::Slice(path) => path
                .split_last()
                .map(|(tail, rest)| (SubtreePath::from(rest), tail.as_ref())),
            SubtreePathBase::DerivedPath(path) => path.derive_parent(),
            _ => todo!(),
        }
    }

    /// Get a reverse path segments iterator.
    pub(crate) fn reverse_iter<'s>(&'s self) -> SubtreePathIter<'b, 's, B> {
        match self {
            SubtreePathBase::Slice(slice) => SubtreePathIter::new(slice.iter()),
            SubtreePathBase::DerivedPath(path) => path.reverse_iter(),
            SubtreePathBase::Owned(bytes) => SubtreePathIter::new(bytes.into_iter()),
        }
    }
}

/// Derived subtree path on top of base path.
#[derive(Debug)]
enum SubtreePathRelative<'r> {
    /// Equivalent to the base path.
    Empty,
    /// Added one child segment.
    Single(CowLike<'r>),
}

impl Hash for SubtreePathRelative<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Empty => {}
            Self::Single(s) => {
                s.hash(state);
            }
        }
    }
}

/// Creates a [SubtreePath] from slice.
impl<'b, B> From<&'b [B]> for SubtreePath<'b, B> {
    fn from(value: &'b [B]) -> Self {
        SubtreePath {
            base: SubtreePathBase::Slice(value),
            relative: SubtreePathRelative::Empty,
        }
    }
}

/// Creates a [SubtreePath] from a [SubtreePath] reference. This way functions
/// could be generic over different ways of representing subtree path.
impl<'b, 'a: 'b, B: AsRef<[u8]>> From<&'a SubtreePath<'b, B>> for SubtreePath<'b, B> {
    fn from(value: &'a SubtreePath<'b, B>) -> Self {
        value.derive()
    }
}

impl SubtreePath<'static, [u8; 0]> {
    /// Creates empty subtree path
    pub const fn new() -> Self {
        SubtreePath {
            base: SubtreePathBase::Slice(&[]),
            relative: SubtreePathRelative::Empty,
        }
    }
}

impl<'b, B: AsRef<[u8]>> SubtreePath<'b, B> {
    /// Get a derived path that will use another subtree path (or reuse the base
    /// slice) as it's base.
    pub fn derive(&'b self) -> SubtreePath<'b, B> {
        match self.relative {
            // If this derived path makes no difference, derive from base
            SubtreePathRelative::Empty => self.base.derive(),
            // Otherwise a new derived subtree path must point to this one as it's base
            _ => SubtreePath {
                base: SubtreePathBase::DerivedPath(self),
                relative: SubtreePathRelative::Empty,
            },
        }
    }

    /// Get a derived path for a parent and a chopped segment.
    pub fn derive_parent<'s>(&'s self) -> Option<(SubtreePath<'b, B>, &'s [u8])> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.derive_parent(),
            SubtreePathRelative::Single(relative) => Some((self.base.derive(), relative.as_ref())),
        }
    }

    /// Get a derived path with a child path segment added. The lifetime of the
    /// path will remain the same in case of owned data (segment is a
    /// vector) or will match the slice's lifetime.
    pub fn derive_child<'s, S>(&'b self, segment: S) -> SubtreePath<'b, B>
    where
        S: Into<CowLike<'s>>,
        's: 'b,
    {
        SubtreePath {
            base: SubtreePathBase::DerivedPath(self),
            relative: SubtreePathRelative::Single(segment.into()),
        }
    }

    /// Returns an iterator for the subtree path by path segments.
    pub fn reverse_iter<'s>(&'s self) -> SubtreePathIter<'b, 's, B> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.reverse_iter(),
            SubtreePathRelative::Single(item) => {
                SubtreePathIter::new_with_next(item.as_ref(), &self.base)
            }
        }
    }

    /// Collect path as a vector of vectors, but this actually negates all the
    /// benefits of this library.
    pub fn to_vec(&self) -> Vec<Vec<u8>> {
        let mut result = match self.base {
            SubtreePathBase::Slice(s) => s.iter().map(|x| x.as_ref().to_vec()).collect(),
            SubtreePathBase::DerivedPath(p) => p.to_vec(),
            _ => todo!(),
        };

        match &self.relative {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(s) => {
                result.push(s.to_vec());
            }
        }

        result
    }

    /// Retuns `true` if the subtree path is empty, so it points to the root
    /// tree.
    pub fn is_root(&self) -> bool {
        match self {
            Self {
                base,
                relative: SubtreePathRelative::Empty,
            } => match base {
                SubtreePathBase::Slice(s) => s.is_empty(),
                SubtreePathBase::DerivedPath(path) => path.is_root(),
                SubtreePathBase::Owned(bytes) => bytes.len() == 0,
            },
            _ => false,
        }
    }
}
