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

//! Compact two dimenstional bytes array structure.

use std::mem;

/// Bytes vector wrapper to have multiple byte arrays allocated continuosuly.
#[derive(Debug, Default)]
pub(crate) struct TwoDimensionalBytes {
    n_segments: usize,
    data: Vec<u8>,
}

impl TwoDimensionalBytes {
    /// Create empty structure.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes segment.
    pub fn add_segment(&mut self, segment: &[u8]) {
        // First it goes byte representation of the segment length
        let length_bytes = segment.len().to_ne_bytes();
        self.data.extend_from_slice(&length_bytes);
        // Next the bytes segment itself
        self.data.extend_from_slice(segment);
        // Again, segment length to be able to iterate in both sides
        self.data.extend_from_slice(&length_bytes);
        self.n_segments += 1;
    }

    pub fn len(&self) -> usize {
        self.n_segments
    }
}

impl<'a> IntoIterator for &'a TwoDimensionalBytes {
    type IntoIter = TwoDimensionalBytesIter<'a>;
    type Item = &'a [u8];

    fn into_iter(self) -> Self::IntoIter {
        TwoDimensionalBytesIter {
            bytes: self,
            offset: 0,
            offset_back: self.data.len(),
        }
    }
}

pub(crate) struct TwoDimensionalBytesIter<'a> {
    bytes: &'a TwoDimensionalBytes,
    offset: usize,
    offset_back: usize,
}

impl TwoDimensionalBytesIter<'_> {
    /// Check if the iterator is finished.
    /// Per [DoubleEndediterator] docs: "It is important to note that both back
    /// and forth work on the same range, and do not cross: iteration is
    /// over when they meet in the middle."
    fn is_ended(&self) -> bool {
        self.offset >= self.offset_back
    }
}

impl<'a> Iterator for TwoDimensionalBytesIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_ended() {
            return None;
        }

        // Like [Self::add_segment], but reverse: first we get N bytes, where N
        // is the size of usize and build segment length
        let length_size = mem::size_of::<usize>();
        let segment_length = usize::from_ne_bytes(
            self.bytes.data[self.offset..self.offset + length_size]
                .try_into()
                .expect("internal structure bug"),
        );
        // Read segment length, moving offset forward
        self.offset += length_size;

        // Reading segment data starting from new offset
        let segment = &self.bytes.data[self.offset..self.offset + segment_length];

        // Ignore segment length bytes that were intended for reverse iteration and
        // move offset to the next segment data (must point to it's size)
        self.offset += length_size + segment_length;

        Some(segment)
    }
}

impl<'a> DoubleEndedIterator for TwoDimensionalBytesIter<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.is_ended() {
            return None;
        }

        // Like [Self::add_segment], but reverse: first we get N bytes, where N
        // is the size of usize and build segment length
        let length_size = mem::size_of::<usize>();
        let segment_length = usize::from_ne_bytes(
            self.bytes.data[self.offset_back - length_size..self.offset_back]
                .try_into()
                .expect("internal structure bug"),
        );
        // Read segment length, moving offset backwards
        self.offset_back -= length_size;

        // Reading segment data starting from new offset
        let segment = &self.bytes.data[self.offset_back - segment_length..self.offset_back];

        // Ignore segment length bytes that were intended for reverse iteration and
        // move offset to the next (according to iteration direction) segment data (must
        // point to it's size)
        self.offset_back -= length_size + segment_length;

        Some(segment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_two_dimensional_bytes() {
        let bytes = TwoDimensionalBytes::new();
        assert_eq!(bytes.len(), 0);

        let mut iter = bytes.into_iter();
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn non_empty_two_dimensional_bytes_forward_iterator() {
        let mut bytes = TwoDimensionalBytes::default();
        bytes.add_segment(b"ayya");
        bytes.add_segment(b"ayyb");
        bytes.add_segment(b"didn'texpectthat!");
        bytes.add_segment(b"ayyd");

        assert_eq!(bytes.len(), 4);

        let mut iter = bytes.into_iter();
        assert_eq!(iter.next(), Some(b"ayya".as_ref()));
        assert_eq!(iter.next(), Some(b"ayyb".as_ref()));
        assert_eq!(iter.next(), Some(b"didn'texpectthat!".as_ref()));
        assert_eq!(iter.next(), Some(b"ayyd".as_ref()));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);

        // Can do it twice
        assert_eq!(bytes.len(), 4);

        let mut iter = bytes.into_iter();
        assert_eq!(iter.next(), Some(b"ayya".as_ref()));
        assert_eq!(iter.next(), Some(b"ayyb".as_ref()));
        assert_eq!(iter.next(), Some(b"didn'texpectthat!".as_ref()));
        assert_eq!(iter.next(), Some(b"ayyd".as_ref()));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn non_empty_two_dimensional_bytes_backward_iterator() {
        let mut bytes = TwoDimensionalBytes::default();
        bytes.add_segment(b"ayya");
        bytes.add_segment(b"ayyb");
        bytes.add_segment(b"didn'texpectthat!");
        bytes.add_segment(b"ayyd");

        assert_eq!(bytes.len(), 4);

        let mut iter = bytes.into_iter().rev();
        assert_eq!(iter.next(), Some(b"ayyd".as_ref()));
        assert_eq!(iter.next(), Some(b"didn'texpectthat!".as_ref()));
        assert_eq!(iter.next(), Some(b"ayyb".as_ref()));
        assert_eq!(iter.next(), Some(b"ayya".as_ref()));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);

        // Can do it twice
        assert_eq!(bytes.len(), 4);

        let mut iter = bytes.into_iter().rev();
        assert_eq!(iter.next(), Some(b"ayyd".as_ref()));
        assert_eq!(iter.next(), Some(b"didn'texpectthat!".as_ref()));
        assert_eq!(iter.next(), Some(b"ayyb".as_ref()));
        assert_eq!(iter.next(), Some(b"ayya".as_ref()));
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }
}
