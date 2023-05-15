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

//! Difinitions of versatile type representing a path to a subtree that can own
//! certain path segments.

use std::hash::{Hash, Hasher};

use crate::{
    subtree_path_ref::SubtreePathRefInner,
    util::{CowLike, TwoDimensionalBytes},
    SubtreePathIter, SubtreePathRef,
};

/// Path to a GroveDB's subtree.
#[derive(Debug)]
pub struct SubtreePath<'b, B> {
    /// Derivation starting point.
    pub(crate) base: SubtreePathRef<'b, B>,
    /// Path information relative to [base](Self::base).
    pub(crate) relative: SubtreePathRelative<'b>,
}

/// Hash order is the same as iteration order: from most deep path segment up to
/// root.
impl<'b, B: AsRef<[u8]>> Hash for SubtreePath<'b, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.relative.hash(state);
        self.base.hash(state);
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

impl<'bl, 'br, BL, BR> PartialEq<SubtreePath<'br, BR>> for SubtreePathRef<'bl, BL>
where
    BL: AsRef<[u8]>,
    BR: AsRef<[u8]>,
{
    fn eq(&self, other: &SubtreePath<'br, BR>) -> bool {
        self.clone().into_reverse_iter().eq(other.reverse_iter())
    }
}

impl<'bl, 'br, BL, BR> PartialEq<SubtreePathRef<'br, BR>> for SubtreePath<'bl, BL>
where
    BL: AsRef<[u8]>,
    BR: AsRef<[u8]>,
{
    fn eq(&self, other: &SubtreePathRef<'br, BR>) -> bool {
        self.reverse_iter().eq(other.clone().into_reverse_iter())
    }
}

impl<'b, B: AsRef<[u8]>> Eq for SubtreePath<'b, B> {}

impl<'s, 'b, B> From<&'s SubtreePathRef<'b, B>> for SubtreePath<'b, B> {
    fn from(value: &'s SubtreePathRef<'b, B>) -> Self {
        SubtreePath {
            base: value.clone(),
            relative: SubtreePathRelative::Empty,
        }
    }
}

/// Derived subtree path on top of base path.
#[derive(Debug)]
pub(crate) enum SubtreePathRelative<'r> {
    /// Equivalent to the base path.
    Empty,
    /// Added one child segment.
    Single(CowLike<'r>),
    /// Derivation with multiple owned path segments at once
    Multi(TwoDimensionalBytes),
}

impl Hash for SubtreePathRelative<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(segment) => segment.hash(state),
            SubtreePathRelative::Multi(bytes) => {
                bytes.reverse_iter().for_each(|segment| segment.hash(state))
            }
        }
    }
}

impl SubtreePath<'static, [u8; 0]> {
    /// Creates empty subtree path
    pub fn new() -> Self {
        SubtreePath {
            base: [].as_ref().into(),
            relative: SubtreePathRelative::Empty,
        }
    }
}

impl<'b, B: AsRef<[u8]>> SubtreePath<'b, B> {
    /// Get a derived path that will use another subtree path (or reuse the base
    /// slice) as it's base, then could be edited in place.
    pub fn derive_owned(&'b self) -> SubtreePath<'b, B> {
        match self.relative {
            // If this derived path makes no difference, derive from base
            SubtreePathRelative::Empty => self.base.derive_owned(),
            // Otherwise a new derived subtree path must point to this one as it's base
            _ => SubtreePath {
                base: SubtreePathRefInner::SubtreePath(self).into(),
                relative: SubtreePathRelative::Empty,
            },
        }
    }

    /// Get a derived path for a parent and a chopped segment. Returned
    /// [SubtreePathRef] will be linked to this [SubtreePath] because it might
    /// contain owned data and it has to outlive [SubtreePathRef].
    pub fn derive_parent<'s>(&'s self) -> Option<(SubtreePathRef<'s, B>, &'s [u8])> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.derive_parent(),
            SubtreePathRelative::Single(relative) => Some((self.base.clone(), relative.as_ref())),
            SubtreePathRelative::Multi(_) => {
                let mut iter = self.reverse_iter();
                iter.next()
                    .map(|segment| (SubtreePathRefInner::SubtreePathIter(iter).into(), segment))
            }
        }
    }

    /// Get a derived path with a child path segment added.
    pub fn derive_owned_with_child<'s, S>(&'b self, segment: S) -> SubtreePath<'b, B>
    where
        S: Into<CowLike<'s>>,
        's: 'b,
    {
        SubtreePath {
            base: SubtreePathRefInner::SubtreePath(self).into(),
            relative: SubtreePathRelative::Single(segment.into()),
        }
    }

    /// Adds path segment in place.
    pub fn push_segment(&mut self, segment: &[u8]) {
        match &mut self.relative {
            SubtreePathRelative::Empty => {
                let mut bytes = TwoDimensionalBytes::new();
                bytes.add_segment(segment);
                self.relative = SubtreePathRelative::Multi(bytes);
            }
            SubtreePathRelative::Single(old_segment) => {
                let mut bytes = TwoDimensionalBytes::new();
                bytes.add_segment(old_segment);
                bytes.add_segment(segment);
                self.relative = SubtreePathRelative::Multi(bytes);
            }
            SubtreePathRelative::Multi(bytes) => bytes.add_segment(segment),
        }
    }

    /// Returns an iterator for the subtree path by path segments.
    pub fn reverse_iter(&'b self) -> SubtreePathIter<'b, B> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.clone().into_reverse_iter(),
            SubtreePathRelative::Single(item) => {
                SubtreePathIter::new_with_next(item.as_ref(), &self.base)
            }
            SubtreePathRelative::Multi(bytes) => {
                SubtreePathIter::new_with_next(bytes.reverse_iter(), &self.base)
            }
        }
    }

    /// Collect path as a vector of vectors, but this actually negates all the
    /// benefits of this library.
    pub fn to_vec(&self) -> Vec<Vec<u8>> {
        let mut result = Vec::new();

        // Because of the nature of this library, the vector will be built
        // from it's end
        match &self.relative {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(s) => result.push(s.to_vec()),
            SubtreePathRelative::Multi(bytes) => {
                bytes.reverse_iter().for_each(|s| result.push(s.to_vec()))
            }
        }

        match &self.base.0 {
            SubtreePathRefInner::Slice(slice) => slice
                .iter()
                .rev()
                .for_each(|x| result.push(x.as_ref().to_vec())),
            SubtreePathRefInner::SubtreePath(path) => {
                path.reverse_iter().for_each(|x| result.push(x.to_vec()))
            }
            SubtreePathRefInner::SubtreePathIter(iter) => {
                iter.clone().for_each(|x| result.push(x.as_ref().to_vec()))
            }
        };

        result.reverse();
        result
    }

    /// Retuns `true` if the subtree path is empty, so it points to the root
    /// tree.
    pub fn is_root(&self) -> bool {
        match self {
            Self {
                base,
                relative: SubtreePathRelative::Empty,
            } => base.is_root(),
            _ => false,
        }
    }
}
