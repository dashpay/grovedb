//! Compact two dimenstional bytes array structure.

use std::mem;

/// Bytes vector wrapper to have multiple byte arrays allocated continuosuly.
#[derive(Debug, Default, PartialEq, Eq, Hash)]
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
        self.data.extend_from_slice(&segment.len().to_ne_bytes());
        // Next the bytes segment itself
        self.data.extend_from_slice(segment);
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
        }
    }
}

pub(crate) struct TwoDimensionalBytesIter<'a> {
    bytes: &'a TwoDimensionalBytes,
    offset: usize,
}

impl<'a> Iterator for TwoDimensionalBytesIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.bytes.data.len() {
            // Scrolled to the end, nothing to iterate more
            return None;
        }

        // Like [Self::add_segment], but reverse: first we get N bytes, where N
        // is the size of usize and build segment length
        let length_size = mem::size_of::<usize>();
        let segment_length = usize::from_ne_bytes(
            self.bytes.data[self.offset..self.offset + mem::size_of::<usize>()]
                .try_into()
                .expect("internal structure bug"),
        );
        // Read segment length, moving offset forward
        self.offset += length_size;

        // Reading segment data starting from new offset
        let segment = &self.bytes.data[self.offset..self.offset + segment_length];
        // Move offset to the next segment data (must point to it's size)
        self.offset += segment_length;

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
    fn non_empty_two_dimensional_bytes() {
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
}
