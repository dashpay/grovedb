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

use core::slice;
use std::borrow::Cow;

/// Path to a GroveDB's subtree.
#[derive(Debug)]
pub struct SubtreePath<'b, B> {
    /// Derivation starting point.
    base: SubtreePathBase<'b, B>,
    /// Path information relative to [base](Self::base).
    relative: SubtreePathRelative<'b>,
}

/// A variant of a subtree path from which the new path is derived.
/// The new path is reusing the existing one instead of owning a copy of the same data.
#[derive(Debug)]
enum SubtreePathBase<'b, B> {
    /// The base path is a slice, usually provided at a subroutine entrypoint,
    /// like a user wants to insert into specific subtree and provides a path.
    Slice(&'b [B]),
    /// If the subtree path base cannot be represented as a subset of initially provided slice,
    /// which is handled by [Slice](Self::Slice), this variant is used to refer to other derived
    /// path.
    DerivedPath(&'b SubtreePath<'b, B>),
}

impl<B> Clone for SubtreePathBase<'_, B> {
    fn clone(&self) -> Self {
        match self {
            Self::Slice(x) => Self::Slice(x),
            Self::DerivedPath(x) => Self::DerivedPath(x),
        }
    }
}

impl<B> Copy for SubtreePathBase<'_, B> {}

impl<'b, B> SubtreePathBase<'b, B>
where
    B: Clone + AsRef<[u8]>,
{
    /// Get a derivated subtree path for a parent with care for base path slice case.
    fn parent(&self) -> Option<(SubtreePath<'b, B>, Cow<'b, [u8]>)> {
        match self {
            SubtreePathBase::Slice(path) => path
                .split_last()
                .map(|(tail, rest)| (SubtreePath::from_slice(rest), Cow::Borrowed(tail.as_ref()))),
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
    Single(Cow<'r, [u8]>),
    /// Added nested path.
    Multi(Cow<'r, [Cow<'r, [u8]>]>),
    // /// Went up from some multiple times derivated subtree path and basically
    // /// we have a slice of a shorter version of its relative path.
    // MultiBorrowed(&'r [Cow<'r, [u8]>]),
}

impl<'b, B> SubtreePath<'b, B>
where
    B: Clone + AsRef<[u8]>,
{
    /// Init a subtree path from a slice of path segments.
    pub fn from_slice(slice: &'b [B]) -> Self {
        SubtreePath {
            base: SubtreePathBase::Slice(slice),
            relative: SubtreePathRelative::Empty,
        }
    }

    /// Get a derivated path for a parent and a chopped segment.
    pub fn derive_parent(&'b self) -> Option<(SubtreePath<'b, B>, Cow<'b, [u8]>)> {
        match &self.relative {
            SubtreePathRelative::Empty => self.base.parent(),
            SubtreePathRelative::Single(relative) => Some((
                SubtreePath {
                    base: self.base,
                    relative: SubtreePathRelative::Empty,
                },
                Cow::Borrowed(relative),
            )),
            SubtreePathRelative::Multi(relative) => {
                let (tail, rest) = relative
                    .split_last()
                    .expect("Empty variant is handled separately");

                if relative.len() == 2 {
                    Some((
                        SubtreePath {
                            base: self.base,
                            relative: SubtreePathRelative::Single(Cow::Borrowed(rest[0].as_ref())),
                        },
                        Cow::Borrowed(tail),
                    ))
                } else {
                    Some((
                        SubtreePath {
                            base: self.base,
                            relative: SubtreePathRelative::Multi(Cow::Borrowed(rest)),
                        },
                        Cow::Borrowed(tail),
                    ))
                }
            }
        }
    }

    /// Get a derivated path with a child path segment added. The lifetime of the path
    /// will remain the same in case of owned data (segment is a vector) or will match
    /// the slice's lifetime.
    pub fn derive_child<S>(&'b self, segment: S) -> SubtreePath<'b, B>
    where
        S: Into<Cow<'b, [u8]>>,
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
                current_iter: CurrentSubtreePathIter::Single(item.as_ref()),
                next_subtree_path: Some(&self.base),
            },
            SubtreePathRelative::Multi(_) => {
                todo!()
            }
        }
    }

    #[cfg(test)]
    pub fn to_vec(&self) -> Vec<&[u8]> {
        let mut result = match self.base {
            SubtreePathBase::Slice(s) => s.iter().map(AsRef::as_ref).collect(),
            SubtreePathBase::DerivedPath(p) => p.to_vec(),
        };

        match &self.relative {
            SubtreePathRelative::Empty => {}
            SubtreePathRelative::Single(s) => {
                result.push(s);
            }
            SubtreePathRelative::Multi(s) => {
                result.extend(s.iter().map(AsRef::as_ref));
            }
        }

        result
    }
}

/// (Reverse) iterator for a subtree path.
/// Due to implementation details it cannot effectively iterate from the most shallow
/// path segment to the deepest, so it have to go in reverse direction.
pub struct SubtreePathIter<'b, 's, B> {
    current_iter: CurrentSubtreePathIter<'b, 's, B>,
    next_subtree_path: Option<&'s SubtreePathBase<'b, B>>,
}

impl<'b, 's, B> Iterator for SubtreePathIter<'b, 's, B>
where
    B: AsRef<[u8]> + Clone,
    'b: 's,
{
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

    use super::*;

    fn print_slice_str(slice: &[&[u8]]) {
        let mut formatted = String::from("[");
        for s in slice {
            write!(
                &mut formatted,
                "{}, ",
                std::str::from_utf8(s).expect("should be a valid utf8 for tests")
            )
            .expect("writing into String shouldn't fail");
        }
        write!(&mut formatted, "]").expect("writing into String shouldn't fail");

        println!("{formatted}");
    }

    fn derive_child_static<'s, B>(path: &'s SubtreePath<'s, B>) -> SubtreePath<'s, B>
    where
        B: Clone + AsRef<[u8]>,
    {
        path.derive_child(b"static".as_ref())
    }

    fn derive_child_owned<'s, B>(path: &'s SubtreePath<'s, B>) -> SubtreePath<'s, B>
    where
        B: Clone + AsRef<[u8]>,
    {
        path.derive_child(b"owned".to_vec())
    }

    #[test]
    fn compilation_playground() {
        let base: [&'static [u8]; 3] = [b"one", b"two", b"three"];
        let path = SubtreePath::from_slice(&base);
        print_slice_str(&path.to_vec());

        let base = [b"one".to_vec(), b"two".to_vec(), b"three".to_vec()];
        let path = SubtreePath::from_slice(&base);
        let (path2, segment) = path.derive_parent().unwrap();
        print_slice_str(&path2.to_vec());
        dbg!(std::str::from_utf8(&segment).unwrap());

        let base = [b"lol".to_vec(), b"kek".to_vec()];
        let path = SubtreePath::from_slice(&base);
        let path3 = path.derive_child(b"hmm".to_vec());
        print_slice_str(&path3.to_vec());
        let path4 = derive_child_static(&path3);
        print_slice_str(&path4.to_vec());

        let base = [b"lol".to_vec(), b"kek".to_vec()];
        let path = SubtreePath::from_slice(&base);
        let (path3, _) = path.derive_parent().unwrap();
        print_slice_str(&path3.to_vec());
        let path4 = derive_child_static(&path3);
        print_slice_str(&path4.to_vec());

        let base: [&'static [u8]; 3] = [b"one", b"two", b"three"];
        let path = SubtreePath::from_slice(&base);
        let path2 = derive_child_owned(&path);
        print_slice_str(&path2.to_vec());

        path2
            .reverse_iter()
            .for_each(|seg| println!("{}", std::str::from_utf8(seg).unwrap()));
    }
}
