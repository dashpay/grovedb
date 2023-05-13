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
    base: SubtreePathRef<'b, B>,
    /// Path information relative to [base](Self::base).
    relative: SubtreePathRelative<'b>,
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

impl<'bl, 'br, BL, BR> PartialEq<SubtreePathRef<'br, BR>> for SubtreePathRef<'bl, BL>
where
    BL: AsRef<[u8]>,
    BR: AsRef<[u8]>,
{
    fn eq(&self, other: &SubtreePathRef<'br, BR>) -> bool {
        self.reverse_iter().eq(other.reverse_iter())
    }
}

impl<'b, B: AsRef<[u8]>> Eq for SubtreePath<'b, B> {}

/// Path to a GroveDB's subtree with no owned data.
#[derive(Debug)]
pub struct SubtreePathRef<'b, B>(SubtreePathRefInner<'b, B>);

/// Wrapped inner representation of subtree path ref.
#[derive(Debug)]
enum SubtreePathRefInner<'b, B> {
    /// The referred path is a slice, might a provided by user or a subslice
    /// when deriving a parent.
    Slice(&'b [B]),
    /// Links to an existing subtree path that became a derivation point.
    SubtreePath(&'b SubtreePath<'b, B>),
    /// Links to an existing subtree path with owned segments using it's
    /// iterator to support parent derivations.
    SubtreePathIter(SubtreePathIter<'b, 'b, B>),
}

impl<'b, B> From<SubtreePathRefInner<'b, B>> for SubtreePathRef<'b, B> {
    fn from(value: SubtreePathRefInner<'b, B>) -> Self {
        Self(value)
    }
}

/// Hash order is the same as iteration order: from most deep path segment up to
/// root.
impl<'b, B: AsRef<[u8]>> Hash for SubtreePathRef<'b, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            SubtreePathRefInner::Slice(slice) => slice
                .iter()
                .map(AsRef::as_ref)
                .rev()
                .for_each(|s| s.hash(state)),
            SubtreePathRefInner::SubtreePath(path) => path.hash(state),
            SubtreePathRefInner::SubtreePathIter(path_iter) => {
                path_iter.clone().for_each(|s| s.hash(state))
            }
        }
    }
}

/// For the same reason as for `Hash` implementation, derived impl requires
/// generics to carry /// trait bounds that actually don't needed.
impl<B> Clone for SubtreePathRef<'_, B> {
    fn clone(&self) -> Self {
        match &self.0 {
            SubtreePathRefInner::Slice(x) => SubtreePathRefInner::Slice(x),
            SubtreePathRefInner::SubtreePath(x) => SubtreePathRefInner::SubtreePath(x),
            SubtreePathRefInner::SubtreePathIter(x) => {
                SubtreePathRefInner::SubtreePathIter(x.clone())
            }
        }
        .into()
    }
}

impl<'b, B: AsRef<[u8]>> SubtreePathRef<'b, B> {
    /// Get a derived path that will reuse this [Self] as it's base path and
    /// capable of owning data.
    pub fn derive_editable(&self) -> SubtreePath<'b, B> {
        SubtreePath {
            base: self.clone(),
            relative: SubtreePathRelative::Empty,
        }
    }

    /// Get a derived path with a child path segment added.
    pub fn derive_child<'s, S>(&'b self, segment: S) -> SubtreePath<'b, B>
    where
        S: Into<CowLike<'s>>,
        's: 'b,
    {
        SubtreePath {
            base: self.clone(),
            relative: SubtreePathRelative::Single(segment.into()),
        }
    }

    /// Get a derived subtree path for a parent with care for base path slice
    /// case. The main difference from [SubtreePath::derive_parent] is that
    /// lifetime of returned [Self] if not limited to the scope where this
    /// function was called so it's possible to follow to ancestor paths
    /// without keeping previous result as it still will link to `'b`
    /// (latest [SubtreePath] or initial slice of data).
    pub fn derive_parent(&self) -> Option<(SubtreePathRef<'b, B>, &'b [u8])> {
        match &self.0 {
            SubtreePathRefInner::Slice(path) => path
                .split_last()
                .map(|(tail, rest)| (SubtreePathRefInner::Slice(rest).into(), tail.as_ref())),
            SubtreePathRefInner::SubtreePath(path) => path.derive_parent(),
            SubtreePathRefInner::SubtreePathIter(iter) => {
                let mut derived_iter = iter.clone();
                derived_iter.next().map(|segment| {
                    (
                        SubtreePathRefInner::SubtreePathIter(derived_iter).into(),
                        segment,
                    )
                })
            }
        }
    }

    /// Get a reverse path segments iterator.
    pub fn reverse_iter<'s>(&'s self) -> SubtreePathIter<'b, 's, B> {
        match &self.0 {
            SubtreePathRefInner::Slice(slice) => SubtreePathIter::new(slice.iter()),
            SubtreePathRefInner::SubtreePath(path) => path.reverse_iter(),
            SubtreePathRefInner::SubtreePathIter(iter) => iter.clone(),
        }
    }

    /// Retuns `true` if the subtree path is empty, so it points to the root
    /// tree.
    pub fn is_root(&self) -> bool {
        match &self.0 {
            SubtreePathRefInner::Slice(s) => s.is_empty(),
            SubtreePathRefInner::SubtreePath(path) => path.is_root(),
            SubtreePathRefInner::SubtreePathIter(iter) => iter.is_empty(),
        }
    }

    /// Collect path as a vector of vectors, but this actually negates all the
    /// benefits of this library.
    pub fn to_vec(&self) -> Vec<Vec<u8>> {
        match &self.0 {
            SubtreePathRefInner::Slice(slice) => {
                slice.iter().map(|x| x.as_ref().to_vec()).collect()
            }
            SubtreePathRefInner::SubtreePath(path) => path.to_vec(),
            SubtreePathRefInner::SubtreePathIter(iter) => {
                iter.clone().map(|x| x.as_ref().to_vec()).collect()
            }
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
    /// Derivation with multiple owned path segments at once
    Multi(TwoDimensionalBytes),
}

impl Hash for SubtreePathRelative<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(segment) => segment.hash(state),
            SubtreePathRelative::Multi(bytes) => bytes
                .into_iter()
                .rev()
                .for_each(|segment| segment.hash(state)),
        }
    }
}

/// Creates a [SubtreePath] from slice.
impl<'b, B> From<&'b [B]> for SubtreePathRef<'b, B> {
    fn from(value: &'b [B]) -> Self {
        SubtreePathRefInner::Slice(value).into()
    }
}

impl<'b, B, const N: usize> From<&'b [B; N]> for SubtreePathRef<'b, B> {
    fn from(value: &'b [B; N]) -> Self {
        SubtreePathRefInner::Slice(value).into()
    }
}

// /// Creates a [SubtreePath] from a [SubtreePath] reference. This way
// functions /// could be generic over different ways of representing subtree
// path. impl<'b, 'a: 'b, B: AsRef<[u8]>> From<&'a SubtreePath<'b, B>> for
// SubtreePath<'b, B> {     fn from(value: &'a SubtreePath<'b, B>) -> Self {
//         value.derive_editable()
//     }
// }

impl SubtreePath<'static, [u8; 0]> {
    /// Creates empty subtree path
    pub fn new() -> Self {
        SubtreePath {
            base: SubtreePathRefInner::Slice(&[]).into(),
            relative: SubtreePathRelative::Empty,
        }
    }
}

impl<'b, B: AsRef<[u8]>> SubtreePath<'b, B> {
    /// Get a derived path that will use another subtree path (or reuse the base
    /// slice) as it's base, then could be edited in place.
    pub fn derive_editable(&'b self) -> SubtreePath<'b, B> {
        match self.relative {
            // If this derived path makes no difference, derive from base
            SubtreePathRelative::Empty => self.base.derive_editable(),
            // Otherwise a new derived subtree path must point to this one as it's base
            _ => SubtreePath {
                base: SubtreePathRefInner::SubtreePath(self).into(),
                relative: SubtreePathRelative::Empty,
            },
        }
    }

    /// Immutable branch from a subtree path with no added information.
    pub fn derive(&'b self) -> SubtreePathRef<'b, B> {
        SubtreePathRefInner::SubtreePath(&self).into()
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
    pub fn derive_child<'s, S>(&'b self, segment: S) -> SubtreePath<'b, B>
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
    pub fn reverse_iter<'s>(&'s self) -> SubtreePathIter<'b, 's, B> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.reverse_iter(),
            SubtreePathRelative::Single(item) => {
                SubtreePathIter::new_with_next(item.as_ref(), &self.base)
            }
            SubtreePathRelative::Multi(bytes) => {
                SubtreePathIter::new_with_next(bytes.into_iter(), &self.base)
            }
        }
    }

    /// Collect path as a vector of vectors, but this actually negates all the
    /// benefits of this library.
    pub fn to_vec(&self) -> Vec<Vec<u8>> {
        let mut result = match &self.base.0 {
            SubtreePathRefInner::Slice(slice) => {
                slice.iter().map(|x| x.as_ref().to_vec()).collect()
            }
            SubtreePathRefInner::SubtreePath(path) => path.to_vec(),
            SubtreePathRefInner::SubtreePathIter(iter) => {
                let mut base_vec = iter
                    .clone()
                    .map(|x| x.as_ref().to_vec())
                    .collect::<Vec<_>>();
                base_vec.reverse();
                base_vec
            }
        };

        match &self.relative {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(s) => result.push(s.to_vec()),
            SubtreePathRelative::Multi(bytes) => {
                bytes.into_iter().for_each(|s| result.push(s.to_vec()))
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
            } => base.is_root(),
            _ => false,
        }
    }
}
