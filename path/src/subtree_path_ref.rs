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

//! Difinitions of type representing a path to a subtree made of borrowed data.
//!
//! Opposed to [SubtreePath] which is some kind of a builder, [SubtreePathRef]
//! is a way to refer to path data which makes it a great candidate to use as
//! a function argument where a subtree path is expected, combined with it's
//! various `From` implementations it can cover slices, owned subtree paths and
//! other path references.

use std::hash::{Hash, Hasher};

use crate::{subtree_path::SubtreePathRelative, util::CowLike, SubtreePath, SubtreePathIter};

/// Path to a GroveDB's subtree with no owned data.
#[derive(Debug)]
pub struct SubtreePathRef<'b, B>(pub(crate) SubtreePathRefInner<'b, B>);

/// Wrapped inner representation of subtree path ref.
#[derive(Debug)]
pub(crate) enum SubtreePathRefInner<'b, B> {
    /// The referred path is a slice, might a provided by user or a subslice
    /// when deriving a parent.
    Slice(&'b [B]),
    /// Links to an existing subtree path that became a derivation point.
    SubtreePath(&'b SubtreePath<'b, B>),
    /// Links to an existing subtree path with owned segments using it's
    /// iterator to support parent derivations.
    SubtreePathIter(SubtreePathIter<'b, B>),
}

impl<'bl, 'br, BL, BR> PartialEq<SubtreePathRef<'br, BR>> for SubtreePathRef<'bl, BL>
where
    BL: AsRef<[u8]>,
    BR: AsRef<[u8]>,
{
    fn eq(&self, other: &SubtreePathRef<'br, BR>) -> bool {
        self.clone().into_reverse_iter().eq(other.clone().into_reverse_iter())
    }
}

impl<'b, B> From<SubtreePathRefInner<'b, B>> for SubtreePathRef<'b, B> {
    fn from(value: SubtreePathRefInner<'b, B>) -> Self {
        Self(value)
    }
}

// Following [From] implementations allow to use many types in places where
// a subtree path is expected:

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

/// Create a link to existing [SubtreePath] that cannot outlive it, because it
/// possibly owns some of the path segments.
impl<'s, 'b, B> From<&'s SubtreePath<'b, B>> for SubtreePathRef<'s, B> {
    fn from(value: &'s SubtreePath<'b, B>) -> Self {
        SubtreePathRefInner::SubtreePath(value).into()
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

impl SubtreePathRef<'static, [u8; 0]> {
    /// Get empty subtree path (meaning it'll point to the root tree).
    pub const fn empty() -> Self {
        SubtreePathRef(SubtreePathRefInner::Slice(&[]))
    }
}

impl<'b, B: AsRef<[u8]>> SubtreePathRef<'b, B> {
    /// Get a derived path that will reuse this [Self] as it's base path and
    /// capable of owning data.
    pub fn derive_owned(&self) -> SubtreePath<'b, B> {
        self.into()
    }

    /// Get a derived path with a child path segment added.
    pub fn derive_owned_with_child<'s, S>(&'b self, segment: S) -> SubtreePath<'b, B>
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
    pub fn into_reverse_iter(self) -> SubtreePathIter<'b, B> {
        match self.0 {
            SubtreePathRefInner::Slice(slice) => SubtreePathIter::new(slice.iter()),
            SubtreePathRefInner::SubtreePath(path) => path.reverse_iter(),
            SubtreePathRefInner::SubtreePathIter(iter) => iter,
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
