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
//! Opposed to [SubtreePathBuilder] which is some kind of a builder,
//! [SubtreePath] is a way to refer to path data which makes it a great
//! candidate to use as a function argument where a subtree path is expected,
//! combined with it's various `From` implementations it can cover slices, owned
//! subtree paths and other path references if use as generic [Into].

use std::hash::{Hash, Hasher};

use crate::{
    subtree_path_builder::{SubtreePathBuilder, SubtreePathRelative},
    util::CowLike,
    SubtreePathIter,
};

/// Path to a GroveDB's subtree with no owned data and cheap to clone.
#[derive(Debug)]
pub struct SubtreePath<'b, B> {
    pub(crate) ref_variant: SubtreePathInner<'b, B>,
}

/// Wrapped inner representation of subtree path ref.
#[derive(Debug)]
pub(crate) enum SubtreePathInner<'b, B> {
    /// The referred path is a slice, might a provided by user or a subslice
    /// when deriving a parent.
    Slice(&'b [B]),
    /// Links to an existing subtree path that became a derivation point.
    SubtreePath(&'b SubtreePathBuilder<'b, B>),
    /// Links to an existing subtree path with owned segments using it's
    /// iterator to support parent derivations.
    /// This may sound tricky, but `SubtreePathIter` fits there nicely because
    /// like the other variants of [SubtreePathInner] it points to some segments
    /// data, but because of parent derivations on packed path segments we need
    /// to keep track where are we, that's exactly what iterator does + holds a
    /// link to the next part of our subtree path chain.
    SubtreePathIter(SubtreePathIter<'b, B>),
}

impl<'bl, 'br, BL, BR> PartialEq<SubtreePath<'br, BR>> for SubtreePath<'bl, BL>
where
    BL: AsRef<[u8]>,
    BR: AsRef<[u8]>,
{
    fn eq(&self, other: &SubtreePath<'br, BR>) -> bool {
        self.clone()
            .into_reverse_iter()
            .eq(other.clone().into_reverse_iter())
    }
}

impl<'b, B: AsRef<[u8]>> Eq for SubtreePath<'b, B> {}

impl<'b, B> From<SubtreePathInner<'b, B>> for SubtreePath<'b, B> {
    fn from(ref_variant: SubtreePathInner<'b, B>) -> Self {
        Self { ref_variant }
    }
}

impl<'b, B> From<&'b [B]> for SubtreePath<'b, B> {
    fn from(value: &'b [B]) -> Self {
        SubtreePathInner::Slice(value).into()
    }
}

impl<'b, B, const N: usize> From<&'b [B; N]> for SubtreePath<'b, B> {
    fn from(value: &'b [B; N]) -> Self {
        SubtreePathInner::Slice(value).into()
    }
}

/// Create a link to existing [SubtreePath] that cannot outlive it, because it
/// possibly owns some of the path segments.
impl<'s, 'b, B> From<&'s SubtreePathBuilder<'b, B>> for SubtreePath<'s, B> {
    fn from(value: &'s SubtreePathBuilder<'b, B>) -> Self {
        SubtreePathInner::SubtreePath(value).into()
    }
}

/// Hash order is the same as iteration order: from most deep path segment up to
/// root.
impl<'b, B: AsRef<[u8]>> Hash for SubtreePath<'b, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.ref_variant {
            SubtreePathInner::Slice(slice) => slice
                .iter()
                .map(AsRef::as_ref)
                .rev()
                .for_each(|s| s.hash(state)),
            SubtreePathInner::SubtreePath(path) => path.hash(state),
            SubtreePathInner::SubtreePathIter(path_iter) => {
                path_iter.clone().for_each(|s| s.hash(state))
            }
        }
    }
}

/// For the same reason as for `Hash` implementation, derived impl requires
/// generics to carry trait bounds that actually don't needed.
impl<B> Clone for SubtreePath<'_, B> {
    fn clone(&self) -> Self {
        match &self.ref_variant {
            SubtreePathInner::Slice(x) => SubtreePathInner::Slice(x),
            SubtreePathInner::SubtreePath(x) => SubtreePathInner::SubtreePath(x),
            SubtreePathInner::SubtreePathIter(x) => SubtreePathInner::SubtreePathIter(x.clone()),
        }
        .into()
    }
}

impl SubtreePath<'static, [u8; 0]> {
    /// Get empty subtree path (meaning it'll point to the root tree).
    pub const fn empty() -> Self {
        SubtreePath {
            ref_variant: SubtreePathInner::Slice(&[]),
        }
    }
}

impl<'b, B: AsRef<[u8]>> SubtreePath<'b, B> {
    /// Get a derived path that will reuse this [Self] as it's base path and
    /// capable of owning data.
    pub fn derive_owned(&self) -> SubtreePathBuilder<'b, B> {
        self.into()
    }

    /// Get a derived path with a child path segment added.
    pub fn derive_owned_with_child<'s, S>(&'b self, segment: S) -> SubtreePathBuilder<'b, B>
    where
        S: Into<CowLike<'s>>,
        's: 'b,
    {
        SubtreePathBuilder {
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
    pub fn derive_parent(&self) -> Option<(SubtreePath<'b, B>, &'b [u8])> {
        match &self.ref_variant {
            SubtreePathInner::Slice(path) => path
                .split_last()
                .map(|(tail, rest)| (SubtreePathInner::Slice(rest).into(), tail.as_ref())),
            SubtreePathInner::SubtreePath(path) => path.derive_parent(),
            SubtreePathInner::SubtreePathIter(iter) => {
                let mut derived_iter = iter.clone();
                derived_iter.next().map(|segment| {
                    (
                        SubtreePathInner::SubtreePathIter(derived_iter).into(),
                        segment,
                    )
                })
            }
        }
    }

    /// Get a reverse path segments iterator.
    pub fn into_reverse_iter(self) -> SubtreePathIter<'b, B> {
        match self.ref_variant {
            SubtreePathInner::Slice(slice) => SubtreePathIter::new(slice.iter()),
            SubtreePathInner::SubtreePath(path) => path.reverse_iter(),
            SubtreePathInner::SubtreePathIter(iter) => iter,
        }
    }

    /// Retuns `true` if the subtree path is empty, so it points to the root
    /// tree.
    pub fn is_root(&self) -> bool {
        match &self.ref_variant {
            SubtreePathInner::Slice(s) => s.is_empty(),
            SubtreePathInner::SubtreePath(path) => path.is_root(),
            SubtreePathInner::SubtreePathIter(iter) => iter.is_empty(),
        }
    }

    /// Collect path as a vector of vectors, but this actually negates all the
    /// benefits of this library.
    pub fn to_vec(&self) -> Vec<Vec<u8>> {
        match &self.ref_variant {
            SubtreePathInner::Slice(slice) => slice.iter().map(|x| x.as_ref().to_vec()).collect(),
            SubtreePathInner::SubtreePath(path) => path.to_vec(),
            SubtreePathInner::SubtreePathIter(iter) => {
                let mut path = iter
                    .clone()
                    .map(|x| x.as_ref().to_vec())
                    .collect::<Vec<Vec<u8>>>();
                path.reverse();
                path
            }
        }
    }
}
