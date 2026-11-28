//! Aligned ranges and aligned memory for direct reads. All of it is safe
//! code: the memory itself is `ring.rs`'s [`Memory`].

use std::{
    io::{self, ErrorKind},
    sync::{Mutex, MutexGuard, PoisonError},
};

use super::ring::Memory;

/// No file is longer, so no read goes past it.
pub(super) const MAX_FILE_LEN: u64 = i64::MAX as u64;

/// What a direct read of a file has to be a multiple of: the offset it
/// starts at, its length, and the address of the memory it reads into. A
/// power of two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Align(u64);

impl Align {
    /// What is taken where the system does not say: a multiple of the
    /// logical block sizes storage has, 512 and 4,096 bytes.
    pub(super) const DEFAULT: Self = Self(4096);

    /// The largest alignment this layer reads with.
    pub(super) const LARGEST: Self = Self(LARGEST_BUFFER as u64);

    /// `bytes` as an alignment, if it is a power of two that this layer
    /// reads with.
    pub(super) fn new(bytes: u64) -> Option<Self> {
        (bytes.is_power_of_two() && bytes <= Self::LARGEST.0).then_some(Self(bytes))
    }

    /// The alignment for a file of which the system says what memory and
    /// what offsets its direct I/O needs. `None` if it needs none this
    /// layer can give, or if either is zero, which is how the system says
    /// that the file takes no direct I/O.
    pub(super) fn reported(memory: u32, offset: u32) -> Option<Self> {
        let memory = Self::new(u64::from(memory))?;
        let offset = Self::new(u64::from(offset))?;
        Some(memory.max(offset))
    }

    pub(super) fn get(self) -> u64 {
        self.0
    }

    fn down(self, offset: u64) -> u64 {
        offset & !(self.0 - 1)
    }

    /// `offset` rounded up. It does not overflow for an offset inside a
    /// file ([`MAX_FILE_LEN`]).
    fn up(self, offset: u64) -> u64 {
        self.down(offset + (self.0 - 1))
    }

    fn divides(self, value: u64) -> bool {
        value & (self.0 - 1) == 0
    }

    /// Whether a read into `memory` at `offset` is aligned in all three:
    /// where it starts, how long it is and where its memory is.
    pub(super) fn fits(self, offset: u64, memory: &[u8]) -> bool {
        self.fits_but_for_its_length(offset, memory) && self.divides(memory.len() as u64)
    }

    /// Whether a read into `memory` at `offset` is aligned in where it
    /// starts and where its memory is.
    pub(super) fn fits_but_for_its_length(self, offset: u64, memory: &[u8]) -> bool {
        self.divides(offset) && self.divides(memory.as_ptr().addr() as u64)
    }
}

/// Where a read of `len` bytes at `offset` ends, cut at the largest offset
/// a file has.
fn end_of(offset: u64, len: usize) -> u64 {
    offset.saturating_add(len as u64).min(MAX_FILE_LEN)
}

/// The aligned range that holds the `len` bytes at `offset`: where it
/// starts and where it ends. `None` if no byte of a file can be in it.
///
/// It ends at a multiple of the alignment, but where that would be beyond
/// the largest offset a file has: there it ends at that offset, and is not
/// a multiple of the alignment long.
pub(super) fn span(offset: u64, len: usize, align: Align) -> Option<(u64, u64)> {
    let end = end_of(offset, len);
    let to = align.up(end).min(MAX_FILE_LEN);
    (offset < end).then_some((align.down(offset), to))
}

/// Fills `buf` from `offset` with aligned reads, until it is full or the
/// file ends, and returns how many bytes it filled.
///
/// `pread` is the direct read: it is given memory and an offset that are
/// aligned to `align`, and returns how many bytes of the file it read from
/// there. The memory is a multiple of `align` long too, but where it ends
/// at the largest offset a file has. `bounce` is the aligned memory the
/// reads are made into, a multiple of `align` long and not empty.
/// `file_len` is the length of the file now.
///
/// A read that brings fewer bytes than it was asked for is the end of the
/// file only if `file_len` says so. Otherwise it is taken up where it
/// stopped, and the second of two reads in a row that bring nothing new
/// fails the call: no count is returned that would say that the file ends
/// where it does not.
pub(super) fn read_through(
    buf: &mut [u8],
    offset: u64,
    align: Align,
    bounce: &mut [u8],
    mut pread: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
    mut file_len: impl FnMut() -> io::Result<u64>,
) -> io::Result<usize> {
    let Some((_, to)) = span(offset, buf.len(), align) else {
        return Ok(0);
    };
    let end = end_of(offset, buf.len());
    let mut have = offset;
    let mut stalled = false;
    while have < end {
        let from = align.down(have);
        let Some(len) = usize::try_from(to.saturating_sub(from))
            .ok()
            .map(|len| len.min(bounce.len()))
            .filter(|len| *len > 0)
        else {
            break;
        };
        let read = match pread(&mut bounce[..len], from) {
            Ok(read) => read.min(len),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let reached = from + read as u64;
        let new = reached > have;
        if new {
            let upto = reached.min(end);
            buf[(have - offset) as usize..(upto - offset) as usize]
                .copy_from_slice(&bounce[(have - from) as usize..(upto - from) as usize]);
            have = upto;
        }
        let stalled_before = std::mem::replace(&mut stalled, !new);
        if read == len || have == end {
            continue;
        }
        if file_len()? <= have.max(reached) {
            break;
        }
        if !new && stalled_before {
            return Err(io::Error::other(format!(
                "direct reads at offset {from} twice ended at offset {reached}, before the end of \
                 the file"
            )));
        }
    }
    Ok((have - offset) as usize)
}

/// Zeroed memory whose first byte is at a multiple of an alignment.
///
/// It is a [`Memory`] that is longer than asked by the alignment, and the
/// part of it that starts at the first aligned address.
#[derive(Debug)]
pub(super) struct AlignedBuf {
    memory: Memory,
    start: usize,
    len: usize,
}

impl AlignedBuf {
    /// Fails where the system gives no memory for it.
    pub(super) fn new(len: usize, align: Align) -> io::Result<Self> {
        #[cfg(test)]
        if FAIL_NEXT.with(|fail| fail.replace(false)) {
            return Err(ErrorKind::OutOfMemory.into());
        }
        let align = align.get() as usize;
        let memory = Memory::new(len + align - 1)?;
        let start = memory.bytes().as_ptr().addr().wrapping_neg() & (align - 1);
        Ok(Self { memory, start, len })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.memory.bytes()[self.start..self.start + self.len]
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.memory.bytes_mut()[self.start..self.start + self.len]
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the next [`AlignedBuf::new`] of this thread fail, as it does where
/// the system gives no memory.
#[cfg(test)]
pub(super) fn fail_the_next_buffer() {
    FAIL_NEXT.with(|fail| fail.set(true));
}

/// The length of the buffers that are kept from one read to the next: what
/// the block of a page of the largest size takes, with the rest of the
/// aligned range around it.
pub(super) const KEPT_BUFFER: usize = 128 << 10;

/// The alignment of the buffers that are kept.
pub(super) const KEPT_ALIGN: Align = Align::DEFAULT;

/// The most memory one read takes at a time. A longer read is made in
/// pieces of this length.
const LARGEST_BUFFER: usize = 1 << 20;

/// How many buffers are kept.
const KEPT: usize = 32;

/// The buffers of single reads, kept from one read to the next.
#[derive(Debug, Default)]
pub(super) struct Buffers {
    free: Mutex<Vec<AlignedBuf>>,
}

impl Buffers {
    /// Runs `read` with aligned memory for a read of `len` bytes: at
    /// least `len` rounded up to `align`, or [`LARGEST_BUFFER`] if that is
    /// more, and a multiple of `align` long. Memory of the kept length is
    /// kept for the next read. It fails, and `read` is not run, where the
    /// system gives no memory.
    pub(super) fn with<T>(
        &self,
        len: usize,
        align: Align,
        read: impl FnOnce(&mut [u8]) -> T,
    ) -> io::Result<T> {
        if len > KEPT_BUFFER || align > KEPT_ALIGN {
            let whole = align.up(len.min(LARGEST_BUFFER) as u64) as usize;
            let mut buf = AlignedBuf::new(whole.min(LARGEST_BUFFER), align)?;
            return Ok(read(buf.bytes_mut()));
        }
        let kept = self.free().pop();
        let mut buf = match kept {
            Some(kept) => kept,
            None => AlignedBuf::new(KEPT_BUFFER, KEPT_ALIGN)?,
        };
        let done = read(buf.bytes_mut());
        let mut free = self.free();
        if free.len() < KEPT {
            free.push(buf);
        }
        Ok(done)
    }

    fn free(&self) -> MutexGuard<'_, Vec<AlignedBuf>> {
        // A buffer is whole whatever a holder of the lock did before it
        // panicked.
        self.free.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn kept(&self) -> usize {
        self.free().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn align(bytes: u64) -> Align {
        Align::new(bytes).expect("an alignment")
    }

    fn buffer(len: usize, align: Align) -> AlignedBuf {
        AlignedBuf::new(len, align).expect("memory")
    }

    #[test]
    fn an_alignment_is_a_power_of_two_this_layer_reads_with() {
        for bytes in [1, 512, 4096, 65536, 1 << 20] {
            assert_eq!(align(bytes).get(), bytes);
        }
        for bytes in [0, 3, 4095, 6144, 1 << 21, u64::MAX] {
            assert_eq!(Align::new(bytes), None, "{bytes}");
        }
        assert_eq!(Align::reported(512, 4096), Some(align(4096)));
        assert_eq!(Align::reported(4096, 512), Some(align(4096)));
        assert_eq!(Align::reported(512, 512), Some(align(512)));
        assert_eq!(Align::reported(0, 512), None);
        assert_eq!(Align::reported(512, 0), None);
        assert_eq!(Align::reported(0, 0), None);
        assert_eq!(Align::reported(1 << 21, 512), None);
        assert_eq!(Align::reported(512, 1000), None);
    }

    #[test]
    fn a_span_is_the_aligned_range_around_a_read() {
        let a = align(4096);
        assert_eq!(span(0, 1, a), Some((0, 4096)));
        assert_eq!(span(0, 4096, a), Some((0, 4096)));
        assert_eq!(span(1, 4096, a), Some((0, 8192)));
        assert_eq!(span(4095, 2, a), Some((0, 8192)));
        assert_eq!(span(4096, 4096, a), Some((4096, 8192)));
        assert_eq!(span(10_000, 30_000, a), Some((8192, 40_960)));
        assert_eq!(span(7, 0, a), None);
        assert_eq!(span(700, 100, align(512)), Some((512, 1024)));
        assert_eq!(span(700, 100, align(1)), Some((700, 800)));

        // Below the largest offset a file has, the last range ends at
        // that offset, and is not a multiple of the alignment long.
        let top = MAX_FILE_LEN - 4095;
        assert_eq!(
            span(top - 5000, 100_000, a),
            Some((top - 8192, MAX_FILE_LEN))
        );
        assert_eq!(span(top - 5000, 100, a), Some((top - 8192, top - 4096)));
        assert_eq!(
            span(top - 1, usize::MAX, a),
            Some((top - 4096, MAX_FILE_LEN))
        );
        assert_eq!(span(top, 10, a), Some((top, MAX_FILE_LEN)));
        assert_eq!(span(MAX_FILE_LEN - 1, 10, a), Some((top, MAX_FILE_LEN)));
        assert_eq!(
            span(MAX_FILE_LEN - 1, 1, align(1)),
            Some((MAX_FILE_LEN - 1, MAX_FILE_LEN))
        );
        assert_eq!(span(MAX_FILE_LEN, 10, a), None);
        assert_eq!(span(u64::MAX, 10, a), None);
    }

    /// A file of `len` bytes whose byte at `offset` is `offset as u8 ^ 0x5a`,
    /// read as a direct read reads: only when everything is aligned, and at
    /// most `most` bytes a call.
    fn file(
        len: u64,
        a: Align,
        most: usize,
        calls: &mut Vec<(u64, usize)>,
    ) -> impl FnMut(&mut [u8], u64) -> io::Result<usize> + '_ {
        move |into, at| {
            let at_the_top = at + into.len() as u64 == MAX_FILE_LEN;
            assert!(
                a.fits(at, into) || (at_the_top && a.fits_but_for_its_length(at, into)),
                "{at} {}",
                into.len()
            );
            calls.push((at, into.len()));
            let count = (len.saturating_sub(at) as usize).min(into.len()).min(most);
            for (i, byte) in into[..count].iter_mut().enumerate() {
                *byte = (at + i as u64) as u8 ^ 0x5a;
            }
            Ok(count)
        }
    }

    fn expected(offset: u64, len: usize) -> Vec<u8> {
        (0..len as u64).map(|i| (offset + i) as u8 ^ 0x5a).collect()
    }

    #[test]
    fn a_read_is_made_of_aligned_reads_and_copied_out() {
        for bytes in [1, 512, 4096] {
            let a = align(bytes);
            let mut bounce = buffer(16 * 4096, a);
            for (offset, len) in [(0, 1), (0, 4096), (1, 4096), (4095, 2), (5000, 20_000)] {
                let mut calls = Vec::new();
                let mut buf = vec![0; len];
                let read = read_through(
                    &mut buf,
                    offset,
                    a,
                    bounce.bytes_mut(),
                    file(1 << 20, a, usize::MAX, &mut calls),
                    || panic!("a whole read does not ask for the length"),
                )
                .unwrap();
                assert_eq!(read, len);
                assert_eq!(buf, expected(offset, len));
                let (from, to) = span(offset, len, a).unwrap();
                assert_eq!(calls, [(from, (to - from) as usize)], "one read");
            }
        }
    }

    #[test]
    fn a_read_stops_where_the_file_ends() {
        let a = align(4096);
        let mut bounce = buffer(8 * 4096, a);
        for (file_len, offset, len, read_len) in [
            (10_000, 0, 20_000, 10_000),
            (10_000, 9_999, 5, 1),
            (10_000, 10_000, 5, 0),
            (10_000, 50_000, 5, 0),
            (8192, 4096, 8192, 4096),
            (8192, 8192, 1, 0),
            (0, 0, 100, 0),
        ] {
            let mut calls = Vec::new();
            let mut buf = vec![0; len];
            let read = read_through(
                &mut buf,
                offset,
                a,
                bounce.bytes_mut(),
                file(file_len, a, usize::MAX, &mut calls),
                || Ok(file_len),
            )
            .unwrap();
            assert_eq!(read, read_len, "{file_len} {offset} {len}");
            assert_eq!(buf[..read], expected(offset, read));
            assert_eq!(calls.len(), 1, "the length says that the file ends");
        }
    }

    #[test]
    fn a_read_that_is_cut_short_is_taken_up_where_it_stopped() {
        let a = align(512);
        let mut bounce = buffer(64 * 512, a);
        for most in [512, 700, 4096] {
            let mut calls = Vec::new();
            let mut buf = vec![0; 9000];
            let read = read_through(
                &mut buf,
                300,
                a,
                bounce.bytes_mut(),
                file(1 << 20, a, most, &mut calls),
                || Ok(1 << 20),
            )
            .unwrap();
            assert_eq!(read, 9000);
            assert_eq!(buf, expected(300, 9000));
            assert!(calls.len() > 1);
        }
    }

    #[test]
    fn a_read_that_stops_before_the_end_of_the_file_twice_is_no_end_of_the_file() {
        let a = align(512);
        let mut bounce = buffer(64 * 512, a);
        // Reads that bring less than the bytes before the offset: nothing
        // new, twice.
        for most in [0, 1, 100] {
            let mut calls = Vec::new();
            let mut buf = vec![0; 9000];
            let error = read_through(
                &mut buf,
                300,
                a,
                bounce.bytes_mut(),
                file(1 << 20, a, most, &mut calls),
                || Ok(1 << 20),
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Other, "{most}");
            assert_eq!(calls.len(), 2);
        }

        // One such read, and then the rest: the file was not at its end.
        let mut calls = 0;
        let mut buf = vec![0; 9000];
        let mut whole = Vec::new();
        let mut reads = file(1 << 20, a, usize::MAX, &mut whole);
        let read = read_through(
            &mut buf,
            300,
            a,
            bounce.bytes_mut(),
            |into, at| {
                calls += 1;
                if calls == 1 {
                    Ok(100)
                } else {
                    reads(into, at)
                }
            },
            || Ok(1 << 20),
        )
        .unwrap();
        assert_eq!((read, calls), (9000, 2));
        assert_eq!(buf, expected(300, 9000));

        // A read that brings nothing, a read that fills its memory, and
        // then another that brings nothing: the two are not in a row.
        let mut small = buffer(2 * 512, a);
        let mut calls = 0;
        let mut buf = vec![0; 9000];
        let mut whole = Vec::new();
        let mut reads = file(1 << 20, a, usize::MAX, &mut whole);
        let read = read_through(
            &mut buf,
            300,
            a,
            small.bytes_mut(),
            |into, at| {
                calls += 1;
                if calls % 2 == 1 {
                    Ok(0)
                } else {
                    reads(into, at)
                }
            },
            || Ok(1 << 20),
        )
        .unwrap();
        assert_eq!((read, calls), (9000, 20));
        assert_eq!(buf, expected(300, 9000));

        // The file grew between a read and the question for its length:
        // the read is taken up, and ends where the file ends then.
        let mut calls = Vec::new();
        let mut lens = [20_000, 10_000].into_iter();
        let mut buf = vec![0; 30_000];
        let read = read_through(
            &mut buf,
            0,
            a,
            bounce.bytes_mut(),
            file(10_000, a, usize::MAX, &mut calls),
            || Ok(lens.next().expect("two short reads")),
        )
        .unwrap();
        assert_eq!(read, 10_000);
        assert_eq!(calls.len(), 2);

        // The file shrank between the read and the question: the read
        // ends with what it brought.
        let mut calls = Vec::new();
        let read = read_through(
            &mut buf,
            0,
            a,
            bounce.bytes_mut(),
            file(10_000, a, usize::MAX, &mut calls),
            || Ok(512),
        )
        .unwrap();
        assert_eq!((read, calls.len()), (10_000, 1));

        // A read that is short and still fills what was asked for does not
        // ask for the length.
        let mut calls = Vec::new();
        let read = read_through(
            &mut buf[..9_990],
            0,
            a,
            bounce.bytes_mut(),
            file(10_000, a, usize::MAX, &mut calls),
            || panic!("the bytes are there"),
        )
        .unwrap();
        assert_eq!((read, calls.len()), (9_990, 1));

        // The length cannot be read: that is the error.
        let mut calls = Vec::new();
        let error = read_through(
            &mut buf,
            0,
            a,
            bounce.bytes_mut(),
            file(10_000, a, usize::MAX, &mut calls),
            || Err(ErrorKind::PermissionDenied.into()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_read_longer_than_its_memory_is_made_in_pieces() {
        let a = align(4096);
        let mut bounce = buffer(2 * 4096, a);
        let mut calls = Vec::new();
        let mut buf = vec![0; 30_000];
        let read = read_through(
            &mut buf,
            100,
            a,
            bounce.bytes_mut(),
            file(1 << 20, a, usize::MAX, &mut calls),
            || panic!("a whole read does not ask for the length"),
        )
        .unwrap();
        assert_eq!(read, 30_000);
        assert_eq!(buf, expected(100, 30_000));
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|(_, len)| *len <= 2 * 4096));
    }

    #[test]
    fn an_interrupted_read_is_made_again_and_an_error_is_passed_on() {
        let a = align(4096);
        let mut bounce = buffer(4096, a);
        let mut calls = 0;
        let mut buf = [0; 10];
        let no_len = || panic!("a whole read does not ask for the length");
        let read = read_through(
            &mut buf,
            0,
            a,
            bounce.bytes_mut(),
            |into, _| {
                calls += 1;
                if calls < 3 {
                    return Err(ErrorKind::Interrupted.into());
                }
                into.fill(7);
                Ok(into.len())
            },
            no_len,
        )
        .unwrap();
        assert_eq!((read, calls, buf), (10, 3, [7; 10]));

        let error = read_through(
            &mut buf,
            0,
            a,
            bounce.bytes_mut(),
            |_, _| Err(ErrorKind::PermissionDenied.into()),
            no_len,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn the_bytes_below_the_largest_offset_are_read_and_nothing_beyond_it() {
        let a = align(4096);
        let mut bounce = buffer(8192, a);
        let no_len = || panic!("the read does not ask for the length");
        // A file as long as a file can be: its last block is not whole.
        for (offset, len, read_len) in [
            (MAX_FILE_LEN - 5, 1, 1),
            (MAX_FILE_LEN - 5, 5, 5),
            (MAX_FILE_LEN - 5, 10, 5),
            (MAX_FILE_LEN - 1, usize::MAX, 1),
            (MAX_FILE_LEN - 5000, 5000, 5000),
            (MAX_FILE_LEN - 20_000, 30_000, 20_000),
        ] {
            let mut calls = Vec::new();
            let mut buf = vec![0; len.min(40_000)];
            let read = read_through(
                &mut buf,
                offset,
                a,
                bounce.bytes_mut(),
                file(MAX_FILE_LEN, a, usize::MAX, &mut calls),
                no_len,
            )
            .unwrap();
            assert_eq!(read, read_len, "{offset} {len}");
            assert_eq!(buf[..read], expected(offset, read));
            let last = calls.last().expect("a read");
            assert_eq!(last.0 + last.1 as u64, MAX_FILE_LEN);
        }
        // A shorter file ends where it ends, and the length says so.
        let mut calls = Vec::new();
        let mut buf = [0; 10];
        let read = read_through(
            &mut buf,
            MAX_FILE_LEN - 5,
            a,
            bounce.bytes_mut(),
            file(10_000, a, usize::MAX, &mut calls),
            || Ok(10_000),
        )
        .unwrap();
        assert_eq!((read, calls.len()), (0, 1));

        // At the largest offset and beyond it there is nothing to read.
        for offset in [MAX_FILE_LEN, u64::MAX] {
            let read = read_through(
                &mut buf,
                offset,
                a,
                bounce.bytes_mut(),
                |_, _| panic!("nothing is there to read"),
                || panic!("and nothing to ask"),
            )
            .unwrap();
            assert_eq!(read, 0);
        }
        // A file that claims more than it was asked for is not believed.
        let read = read_through(
            &mut buf,
            0,
            a,
            bounce.bytes_mut(),
            |into, _| Ok(into.len() + 100),
            || panic!("a whole read does not ask for the length"),
        )
        .unwrap();
        assert_eq!(read, 10);
    }

    #[test]
    fn a_buffer_is_zeroed_and_starts_at_a_multiple_of_its_alignment() {
        for bytes in [1, 512, 4096, 65536] {
            let a = align(bytes);
            for len in [bytes as usize, 3 * bytes as usize] {
                let mut buf = buffer(len, a);
                assert_eq!(buf.bytes().len(), len);
                assert!(buf.bytes().iter().all(|byte| *byte == 0));
                assert!(a.fits(0, buf.bytes()));
                buf.bytes_mut().fill(1);
                assert!(a.fits(0, buf.bytes_mut()));
            }
        }
        assert!(!align(4096).fits(1, buffer(4096, align(4096)).bytes()));
        assert!(!align(4096).fits(0, &buffer(4096, align(4096)).bytes()[..100]));
        assert!(!align(4096).fits(0, &buffer(8192, align(4096)).bytes()[1..4097]));
    }

    #[test]
    fn buffers_of_the_kept_length_are_kept_and_others_are_not() {
        let buffers = Buffers::default();
        buffers
            .with(4096, KEPT_ALIGN, |first| {
                buffers
                    .with(KEPT_BUFFER, align(512), |second| {
                        assert_eq!((first.len(), second.len()), (KEPT_BUFFER, KEPT_BUFFER));
                        assert!(KEPT_ALIGN.fits(0, second));
                        assert_eq!(buffers.kept(), 0);
                    })
                    .unwrap();
                assert_eq!(buffers.kept(), 1);
            })
            .unwrap();
        assert_eq!(buffers.kept(), 2);
        let kept = buffers.with(4096, KEPT_ALIGN, |_| buffers.kept()).unwrap();
        assert_eq!((kept, buffers.kept()), (1, 2));

        let wide = align(65536);
        let lens = [
            (KEPT_BUFFER + 4096, KEPT_ALIGN, KEPT_BUFFER + 4096),
            (KEPT_BUFFER + 5, KEPT_ALIGN, KEPT_BUFFER + 4096),
            (65536, wide, 65536),
            (100, wide, 65536),
            (64 << 20, KEPT_ALIGN, LARGEST_BUFFER),
            (usize::MAX, wide, LARGEST_BUFFER),
        ];
        for (len, align, given) in lens {
            let found = buffers
                .with(len, align, |memory| (memory.len(), align.fits(0, memory)))
                .unwrap();
            assert_eq!(found, (given, true), "{len}");
        }
        assert_eq!(buffers.kept(), 2, "none of those is kept");

        fn nested(buffers: &Buffers, depth: usize) {
            if depth > 0 {
                buffers
                    .with(1, KEPT_ALIGN, |_| nested(buffers, depth - 1))
                    .unwrap();
            }
        }
        nested(&buffers, KEPT + 5);
        assert_eq!(buffers.kept(), KEPT);
    }

    #[test]
    fn a_read_gets_no_memory_where_the_system_gives_none() {
        let buffers = Buffers::default();
        for (len, align) in [(4096, KEPT_ALIGN), (KEPT_BUFFER + 1, KEPT_ALIGN)] {
            fail_the_next_buffer();
            let error = buffers
                .with(len, align, |_| panic!("no read is made"))
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::OutOfMemory);
        }
        assert_eq!(buffers.kept(), 0);
        // The next read finds the system as it is.
        assert_eq!(
            buffers.with(1, KEPT_ALIGN, |memory| memory.len()).unwrap(),
            KEPT_BUFFER
        );
    }
}
