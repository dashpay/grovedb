//! The arena the bytes of a write set live in.

/// The bytes a new chunk holds, unless one piece is longer.
const CHUNK: usize = 64 << 10;

/// A piece of an [`Arena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    chunk: u32,
    start: u32,
    len: u32,
}

impl Span {
    /// A piece of no bytes, of any arena.
    pub(crate) const EMPTY: Self = Self {
        chunk: 0,
        start: 0,
        len: 0,
    };

    /// The length of the piece in bytes.
    pub(crate) const fn len(self) -> usize {
        self.len as usize
    }
}

/// A place in an [`Arena`]: everything that was put in after it can be
/// taken out again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArenaMark {
    chunks: usize,
    used: usize,
}

/// Bytes that are put in one after another and taken out only from the end:
/// the keys, values and node records of a write set.
///
/// A piece never moves. A chunk is allocated once, at the size of the
/// pieces it will hold, so putting a piece in allocates only when a chunk
/// is full.
#[derive(Debug, Clone, Default)]
pub(crate) struct Arena {
    chunks: Vec<Vec<u8>>,
    bytes: usize,
}

impl Arena {
    /// Puts `bytes` in, and returns where they are.
    ///
    /// # Panics
    ///
    /// When one piece, or the number of chunks, is above 2^32 − 1. A value
    /// is at most that long (`value-log-format.md` V9).
    pub(crate) fn put(&mut self, bytes: &[u8]) -> Span {
        if bytes.is_empty() {
            return Span::EMPTY;
        }
        let fits = self
            .chunks
            .last()
            .is_some_and(|chunk| chunk.capacity() - chunk.len() >= bytes.len());
        if !fits {
            self.chunks.push(Vec::with_capacity(bytes.len().max(CHUNK)));
        }
        let index = self.chunks.len() - 1;
        let chunk = &mut self.chunks[index];
        let start = chunk.len();
        chunk.extend_from_slice(bytes);
        self.bytes += bytes.len();
        Span {
            chunk: u32::try_from(index).expect("fewer than 2^32 chunks"),
            start: u32::try_from(start).expect("a chunk of less than 4 GiB"),
            len: u32::try_from(bytes.len()).expect("a piece of less than 4 GiB"),
        }
    }

    /// The bytes at `span`.
    pub(crate) fn get(&self, span: Span) -> &[u8] {
        if span.len == 0 {
            return &[];
        }
        let start = span.start as usize;
        &self.chunks[span.chunk as usize][start..start + span.len()]
    }

    /// The place after the last piece.
    pub(crate) fn mark(&self) -> ArenaMark {
        ArenaMark {
            chunks: self.chunks.len(),
            used: self.chunks.last().map_or(0, Vec::len),
        }
    }

    /// Takes out every piece that was put in after `mark`.
    pub(crate) fn rewind(&mut self, mark: ArenaMark) {
        while self.chunks.len() > mark.chunks {
            let chunk = self.chunks.pop().expect("more chunks than the mark has");
            self.bytes -= chunk.len();
        }
        if let Some(chunk) = self.chunks.last_mut() {
            self.bytes -= chunk.len() - mark.used;
            chunk.truncate(mark.used);
        }
    }

    /// Takes out every piece.
    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
    }

    /// The bytes of all pieces.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_stay_where_they_are_put() {
        let mut arena = Arena::default();
        let empty = arena.put(b"");
        assert_eq!((empty, arena.get(empty)), (Span::EMPTY, &b""[..]));
        let first = arena.put(b"first");
        let long = arena.put(&vec![7; CHUNK * 2]);
        let last = arena.put(b"last");
        assert_eq!(arena.get(first), b"first");
        assert_eq!(arena.get(long), &vec![7; CHUNK * 2][..]);
        assert_eq!((arena.get(last), last.len()), (&b"last"[..], 4));
        assert_eq!(arena.bytes(), 5 + CHUNK * 2 + 4);
    }

    #[test]
    fn what_was_put_in_after_a_mark_is_taken_out() {
        let mut arena = Arena::default();
        let start = arena.mark();
        let kept = arena.put(b"kept");
        let mark = arena.mark();
        arena.put(b"gone");
        arena.put(&vec![1; CHUNK]);
        arena.put(&vec![2; CHUNK]);
        arena.rewind(mark);
        assert_eq!(arena.bytes(), 4);
        assert_eq!(arena.get(kept), b"kept");
        // What is put in next takes the place of what went.
        let next = arena.put(b"next");
        assert_eq!(arena.get(next), b"next");
        assert_eq!(arena.get(kept), b"kept");
        arena.rewind(start);
        assert_eq!((arena.bytes(), arena.mark()), (0, start));
        arena.put(b"again");
        arena.clear();
        assert_eq!(arena.bytes(), 0);
    }
}
