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

//! GroveDB subtree path manipulation library.

#![deny(missing_docs)]

mod util;

use core::slice;
use std::hash::{Hash, Hasher};

use util::CowLike;

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

impl<'b, B: AsRef<[u8]>> PartialEq for SubtreePath<'b, B> {
    fn eq(&self, other: &Self) -> bool {
        self.reverse_iter().eq(other.reverse_iter())
    }
}

impl<'b, B: AsRef<[u8]>> Eq for SubtreePath<'b, B> {}

/// A variant of a subtree path from which the new path is derived.
/// The new path is reusing the existing one instead of owning a copy of the same data.
#[derive(Debug)]
enum SubtreePathBase<'b, B> {
    /// The base path is a slice, might a provided by user or a subslice when
    /// deriving a parent.
    Slice(&'b [B]),
    /// If the subtree path base cannot be represented as a subset of initially
    /// provided slice, which is handled by [Slice](Self::Slice), this variant is
    /// used to refer to other derived path.
    DerivedPath(&'b SubtreePath<'b, B>),
}

impl<'b, B: AsRef<[u8]>> Hash for SubtreePathBase<'b, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Slice(slice) => slice.iter().map(AsRef::as_ref).for_each(|s| s.hash(state)),
            Self::DerivedPath(path) => path.hash(state),
        }
    }
}

/// For the same reason as for `Hash` implementation, derived impl requires generics to carry /// trait bounds that actually don't needed.
impl<B> Clone for SubtreePathBase<'_, B> {
    fn clone(&self) -> Self {
        match self {
            Self::Slice(x) => Self::Slice(x),
            Self::DerivedPath(x) => Self::DerivedPath(x),
        }
    }
}

/// Base path doesn't have any owned data and basically a pointer, so it's cheap to be [Copy].
impl<B> Copy for SubtreePathBase<'_, B> {}

impl<'b, B: AsRef<[u8]>> SubtreePathBase<'b, B> {
    /// Get a derived subtree path for a parent with care for base path slice case.
    fn derive_parent(&self) -> Option<(SubtreePath<'b, B>, &'b [u8])> {
        match self {
            SubtreePathBase::Slice(path) => path
                .split_last()
                .map(|(tail, rest)| (SubtreePath::from(rest), tail.as_ref())),
            SubtreePathBase::DerivedPath(path) => path.derive_parent(),
        }
    }

    /// Get a reverse path segments iterator.
    fn reverse_iter<'s>(&'s self) -> SubtreePathIter<'b, 's, B> {
        match self {
            SubtreePathBase::Slice(slice) => SubtreePathIter {
                current_iter: CurrentSubtreePathIter::Slice(slice.iter()),
                next_subtree_path: None,
            },
            SubtreePathBase::DerivedPath(path) => path.reverse_iter(),
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

/// Creates a [SubtreePath] from a [SubtreePath] reference. This way functions could be
/// generic over different ways of representing subtree path.
impl<'b, 'a: 'b, B> From<&'a SubtreePath<'b, B>> for SubtreePath<'b, B> {
    fn from(value: &'a SubtreePath<'b, B>) -> Self {
        SubtreePath {
            base: SubtreePathBase::DerivedPath(value),
            relative: SubtreePathRelative::Empty,
        }
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
    /// Get a derived path that will use another subtree path as it's base.
    pub fn derive(&'b self) -> SubtreePath<'b, B> {
        SubtreePath {
            base: SubtreePathBase::DerivedPath(self),
            relative: SubtreePathRelative::Empty,
        }
    }

    /// Get a derived path for a parent and a chopped segment.
    pub fn derive_parent<'s>(&'s self) -> Option<(SubtreePath<'b, B>, &'s [u8])> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.derive_parent(),
            SubtreePathRelative::Single(relative) => Some((
                SubtreePath {
                    base: self.base,
                    relative: SubtreePathRelative::Empty,
                },
                relative.as_ref(),
            )),
        }
    }

    /// Get a derived path with a child path segment added. The lifetime of the path
    /// will remain the same in case of owned data (segment is a vector) or will match
    /// the slice's lifetime.
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
            SubtreePathRelative::Single(item) => SubtreePathIter {
                current_iter: CurrentSubtreePathIter::Single(item),
                next_subtree_path: Some(&self.base),
            },
        }
    }

    /// Collect path as a vector of vectors, but this actually negates all the benefits of this library.
    pub fn to_vec(&self) -> Vec<Vec<u8>> {
        let mut result = match self.base {
            SubtreePathBase::Slice(s) => s.iter().map(|x| x.as_ref().to_vec()).collect(),
            SubtreePathBase::DerivedPath(p) => p.to_vec(),
        };

        match &self.relative {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(s) => {
                result.push(s.to_vec());
            }
        }

        result
    }

    /// Retuns `true` if the subtree path is empty, so it points to the root tree.
    pub fn is_root(&self) -> bool {
        match self {
            Self {
                base,
                relative: SubtreePathRelative::Empty,
            } => match base {
                SubtreePathBase::Slice(s) => s.is_empty(),
                SubtreePathBase::DerivedPath(path) => path.is_root(),
            },
            _ => false,
        }
    }
}

/// (Reverse) iterator for a subtree path.
/// Due to implementation details it cannot effectively iterate from the most shallow
/// path segment to the deepest, so it have to go in reverse direction.
pub struct SubtreePathIter<'b, 's, B> {
    current_iter: CurrentSubtreePathIter<'b, 's, B>,
    next_subtree_path: Option<&'s SubtreePathBase<'b, B>>,
}

impl<'s, 'b: 's, B: AsRef<[u8]>> Iterator for SubtreePathIter<'b, 's, B> {
    type Item = &'s [u8];

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.current_iter {
            CurrentSubtreePathIter::Single(item) => {
                let path_segment = *item;
                if let Some(next_path) = self.next_subtree_path {
                    *self = next_path.reverse_iter();
                }
                Some(path_segment)
            }
            CurrentSubtreePathIter::Slice(slice_iter) => {
                if let Some(item) = slice_iter.next_back() {
                    Some(item.as_ref())
                } else {
                    if let Some(next_path) = self.next_subtree_path {
                        *self = next_path.reverse_iter();
                        self.next()
                    } else {
                        None
                    }
                }
            }
        }
    }
}

enum CurrentSubtreePathIter<'b, 's, B> {
    Single(&'s [u8]),
    Slice(slice::Iter<'b, B>),
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use crate::util::calculate_hash;

    use super::*;

    fn print_path<B: AsRef<[u8]>>(path: &SubtreePath<B>) {
        let path_vec = path.to_vec();
        let mut formatted = String::from("[");
        for s in path_vec {
            write!(
                &mut formatted,
                "{}, ",
                std::str::from_utf8(&s).expect("should be a valid utf8 for tests")
            )
            .expect("writing into String shouldn't fail");
        }
        write!(&mut formatted, "]").expect("writing into String shouldn't fail");

        println!("{formatted}");
    }

    fn derive_child_static<'s, B: AsRef<[u8]>>(path: &'s SubtreePath<'s, B>) -> SubtreePath<'s, B> {
        path.derive_child(b"static".as_ref())
    }

    fn derive_child_owned<'s, B: AsRef<[u8]>>(path: &'s SubtreePath<'s, B>) -> SubtreePath<'s, B> {
        path.derive_child(b"owned".to_vec())
    }

    #[test]
    fn compilation_playground() {
        let base: [&'static [u8]; 3] = [b"one", b"two", b"three"];
        let path = SubtreePath::from(base.as_ref());
        print_path(&path);

        let base = [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];
        let path = SubtreePath::from(base.as_ref());
        let (path2, segment) = path.derive_parent().unwrap();
        print_path(&path2);
        dbg!(std::str::from_utf8(&segment).unwrap());

        let base = [b"lol".to_owned(), b"kek".to_owned()];
        let path = SubtreePath::from(base.as_ref());
        let path3 = path.derive_child(b"hmm".to_vec());
        print_path(&path3);
        let path4 = derive_child_static(&path3);
        print_path(&path4);

        let base = [b"lol".to_owned(), b"kek".to_owned()];
        let path = SubtreePath::from(base.as_ref());
        let (path3, _) = path.derive_parent().unwrap();
        print_path(&path3);
        let path4 = derive_child_static(&path3);
        print_path(&path4);

        let base: [&'static [u8]; 3] = [b"one", b"two", b"three"];
        let path = SubtreePath::from(base.as_ref());
        let path2 = derive_child_owned(&path);
        print_path(&path2);

        path2
            .reverse_iter()
            .for_each(|seg| println!("{}", std::str::from_utf8(seg).unwrap()));
    }

    #[test]
    fn test_hashes_are_equal() {
        let path_array = [
            b"one".to_vec(),
            b"two".to_vec(),
            b"three".to_vec(),
            b"four".to_vec(),
            b"five".to_vec(),
        ];
        let path_base_slice_vecs = SubtreePath::from(path_array.as_ref());
        let path_array = [
            b"one".as_ref(),
            b"two".as_ref(),
            b"three".as_ref(),
            b"four".as_ref(),
            b"five".as_ref(),
        ];
        let path_base_slice_slices = SubtreePath::from(path_array.as_ref());

        let path_array = [
            b"one".as_ref(),
            b"two".as_ref(),
            b"three".as_ref(),
            b"four".as_ref(),
            b"five".as_ref(),
            b"six".as_ref(),
        ];
        let path_base_slice_too_much = SubtreePath::from(path_array.as_ref());
        let path_base_unfinished = SubtreePath::from([b"one", b"two"].as_ref());
        let path_empty = SubtreePath::new();

        let path_derived_11 = path_empty.derive_child(b"one".as_ref());
        let path_derived_12 = path_derived_11.derive_child(b"two".as_ref());
        let path_derived_13 = path_derived_12.derive_child(b"three".as_ref());
        let path_derived_14 = path_derived_13.derive_child(b"four".to_vec());
        let path_derived_1 = path_derived_14.derive_child(b"five".as_ref());

        let (path_derived_2, _) = path_base_slice_too_much.derive_parent().unwrap();

        let path_derived_31 = path_base_unfinished.derive_child(b"three".to_vec());
        let path_derived_32 = path_derived_31.derive_child(b"four".as_ref());
        let path_derived_3 = path_derived_32.derive_child(b"five".as_ref());

        let hash = calculate_hash(&path_base_slice_vecs);
        assert_eq!(calculate_hash(&path_base_slice_slices), hash);
        assert_eq!(calculate_hash(&path_derived_1), hash);
        assert_eq!(calculate_hash(&path_derived_2), hash);
        assert_eq!(calculate_hash(&path_derived_3), hash);
    }

    #[test]
    fn test_is_root() {
        let path_empty = SubtreePath::<[u8; 0]>::from([].as_ref());
        assert!(path_empty.is_root());

        let path_derived = path_empty.derive_child(b"two".as_ref());
        assert!(path_derived.derive_parent().unwrap().0.is_root());

        let path_not_empty = SubtreePath::from([b"one"].as_ref());
        assert!(path_not_empty.derive_parent().unwrap().0.is_root());
    }
}
