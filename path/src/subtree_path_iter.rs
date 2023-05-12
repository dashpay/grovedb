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

//! Reverse iterator for a subtree path definition and implementation.
//!
//! Due to implementation details (one way link between derivations) it cannot
//! effectively iterate from the most shallow path segment to the deepest, so it
//! have to go in reverse direction.

use std::slice;

use crate::{subtree_path::SubtreePathBase, util::TwoDimensionalBytesIter};

/// (Reverse) iterator for a subtree path.
pub struct SubtreePathIter<'b, 's, B> {
    current_iter: CurrentSubtreePathIter<'b, 's, B>,
    next_subtree_path: Option<&'s SubtreePathBase<'b, B>>,
}

impl<'b, 's, B> SubtreePathIter<'b, 's, B> {
    pub(crate) fn new<I>(iter: I) -> Self
    where
        I: Into<CurrentSubtreePathIter<'b, 's, B>>,
    {
        SubtreePathIter {
            current_iter: iter.into(),
            next_subtree_path: None,
        }
    }

    pub(crate) fn new_with_next<I>(iter: I, next: &'s SubtreePathBase<'b, B>) -> Self
    where
        I: Into<CurrentSubtreePathIter<'b, 's, B>>,
    {
        SubtreePathIter {
            current_iter: iter.into(),
            next_subtree_path: Some(next),
        }
    }
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
            CurrentSubtreePathIter::OwnedBytes(bytes_iter) => {
                if let Some(item) = bytes_iter.next_back() {
                    Some(item)
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

pub(crate) enum CurrentSubtreePathIter<'b, 's, B> {
    Single(&'s [u8]),
    Slice(slice::Iter<'b, B>),
    OwnedBytes(TwoDimensionalBytesIter<'s>),
}

impl<'b, 's, B> From<TwoDimensionalBytesIter<'s>> for CurrentSubtreePathIter<'b, 's, B> {
    fn from(value: TwoDimensionalBytesIter<'s>) -> Self {
        CurrentSubtreePathIter::<B>::OwnedBytes(value)
    }
}

impl<'b, 's, B> From<slice::Iter<'b, B>> for CurrentSubtreePathIter<'b, 's, B> {
    fn from(value: slice::Iter<'b, B>) -> Self {
        CurrentSubtreePathIter::Slice(value)
    }
}

impl<'b, 's, B> From<&'s [u8]> for CurrentSubtreePathIter<'b, 's, B> {
    fn from(value: &'s [u8]) -> Self {
        CurrentSubtreePathIter::Single(value)
    }
}
