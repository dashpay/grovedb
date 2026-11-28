//! The [`Vfs`] backend for Linux that opens the files it reads with
//! `O_DIRECT`.
//!
//! It is compiled for Linux with the feature `direct_io`, which is off
//! unless it is asked for. [`DirectVfs`] is [`StdVfs`] but for one thing: a
//! file that is opened with [`OpenMode::Read`] is opened with `O_DIRECT`.
//! A read through such a handle asks the system for the bytes past its
//! page cache, into memory of this layer, where a read through [`StdVfs`]
//! is answered from that cache and fills it. Whether the system then reads
//! past its page cache is the file system's to say:
//! [Alignment](self#alignment) names file systems that do not.
//!
//! | Operation | What it is made with |
//! | --- | --- |
//! | [`Vfs::open`] with [`OpenMode::Read`] | `open` with `O_RDONLY \| O_DIRECT`, and `statx` for the alignment |
//! | [`VfsFile::read_at`] on such a handle | `pread64` of the aligned range around the bytes, into aligned memory of this layer, and a copy: see [Single reads](self#single-reads) |
//! | [`DirectVfs::read_batch`] | `io_uring_enter` for the reads a ring takes, and the single read for every other: see [Reads together](self#reads-together) |
//! | [`VfsFile::sync_data`], [`VfsFile::sync_all`] on such a handle | `fdatasync`, `fsync` |
//! | [`Vfs::open`] with [`OpenMode::Write`] or [`OpenMode::Append`], and every operation on such a handle | [`StdVfs`]'s, unchanged |
//! | Every operation on names, directories and locks, and [`Vfs::sync_together`] | [`StdVfs`]'s, unchanged |
//!
//! # Alignment
//!
//! A direct read has to start at a multiple of a block size, be a multiple
//! of it long, and read into memory at a multiple of it. A caller of
//! [`VfsFile::read_at`] keeps none of the three: this layer reads the
//! aligned range around what was asked into memory of its own and copies
//! the bytes that were asked for. The alignment of a file is what `statx`
//! reports for it (`STATX_DIOALIGN`, the larger of the two it gives), and
//! 4,096 bytes where the system or the file system does not report one,
//! as no system before Linux 6.1 does. If a file needs a larger alignment
//! than 4,096 bytes and the system does not say so, its reads are made
//! with 4,096 bytes all the same, and one that does not fit what the file
//! needs gets what the system answers it: `EINVAL` on the file systems
//! that check.
//!
//! [`Vfs::open`] with [`OpenMode::Read`] fails with [`Unsupported`] where
//! the system refuses the open with `EINVAL` and the path is no directory,
//! which is how a file system without direct I/O answers; and where
//! `statx` reports an alignment of zero, which is how the system says that
//! the file takes no direct I/O, or one that is no power of two or is more
//! than 1 MiB. That is an error [`StdVfs`] does not have.
//!
//! A file system can take the flag and read through its cache all the
//! same, and this layer cannot tell. As Linux 6.12 has them, tmpfs does
//! so for every file (`mm/shmem.c`), and btrfs for an extent that is
//! compressed or kept inline (`fs/btrfs/direct-io.c`), among other cases.
//! A read is then right, and is not past the cache.
//!
//! # Single reads
//!
//! The aligned range of a read is asked for with one `pread64` if it is at
//! most 1 MiB long, and in pieces of 1 MiB otherwise. A call that a signal
//! interrupts is made again. Where a call brings fewer bytes than it was
//! asked for and the read is not full yet, the file's length is asked
//! for, which says whether the file ends there. If it does, so does the
//! read. If it does not, the rest is asked for again, and the read fails
//! once two calls in a row have brought nothing new. A read into an empty
//! buffer, or from the largest offset a file can have or beyond it, makes
//! no call.
//!
//! No read goes beyond the largest offset a file can have, which is a
//! multiple of no alignment above one byte. A range that would reach
//! beyond it ends there, so with such an alignment the call for the last
//! block below that offset is not a multiple of the alignment long. Where
//! a file is that long, a file system that insists on the alignment
//! refuses that call, and the read fails with its error.
//!
//! The memory of a read is a buffer of 128 KiB if the aligned range fits
//! one and the file's alignment is at most 4,096 bytes. Up to 32 such
//! buffers are kept from one read to the next. For any other read the
//! memory is mapped for the call. A read that needs a new mapping fails
//! where the system gives none. The memory is the layer's own mappings,
//! never the heap:
//! see [A fork](self#a-fork).
//!
//! # What is as on the std backend, and what is not
//!
//! Writes are not direct. A handle that is opened to write or to append is
//! [`StdVfs`]'s own handle, so an append makes the system calls it makes
//! there, and a file's length is the bytes that were written, to the byte.
//! Neither can be had from `O_DIRECT` without more: a direct write is a
//! multiple of the block size long, at a multiple of it, so a file that is
//! appended to in pieces of any length would have to be written a block at
//! a time and cut back to its length afterwards.
//!
//! So every rule of [`crate::vfs`] about writes, lengths, syncs, names and
//! locks is kept here exactly as [`StdVfs`] keeps it, the last rule under
//! [Durability](crate::vfs#durability) among them: writes wait in the page
//! cache, a write-back can fail unreported, and a sync vouches for a
//! change only through a handle that was open when the change was made. A
//! sync through a direct handle is the system's sync of a descriptor that
//! was opened to read, as on [`StdVfs`].
//!
//! Reads differ from [`StdVfs`]'s in this:
//!
//! - **An open to read can be refused**, with [`Unsupported`]
//!   ([Alignment](self#alignment)).
//! - **A read can make the system write.** Where the page cache holds
//!   bytes of the range that were written and are not on storage yet, a
//!   read that is made past the cache has the system write them out and
//!   wait, before it reads. That is how a direct read comes to see a
//!   write that returned before it began and that only the cache holds.
//!   The exception of [`crate::vfs`] stands here as there: a write that
//!   is lost to a failed write-back can cease to be visible to reads.
//! - **A read can fail for a write-back that failed.** If that writing
//!   out fails, the read can fail with the system's error, where a read
//!   through [`StdVfs`] is answered from the cache while the cache holds
//!   the bytes. And the
//!   system keeps one mark of a failed write-back for the whole file,
//!   which a read can find and take away: a read can then fail for the
//!   write-back of a range it does not read, and no later read fails for
//!   it. Not every read looks at the mark. `docs/lignindb/sync-handles.md`,
//!   section 7, has what was read of this in the source of Linux.
//! - **A read that is made past the cache leaves nothing in it** for the
//!   next read, and the system reads nothing ahead for it.
//!
//! # Reads together
//!
//! [`DirectVfs::read_batch`] takes a list of reads and fills the buffer of
//! each with the bytes [`VfsFile::read_exact_at`] would fill it with. For
//! a list of two or more an io_uring instance is used where the system
//! gives one. Of each 32 reads of the list in turn, the reads that a ring
//! takes, if there are any, are handed to the system together, with an
//! `io_uring_enter` that waits for all of them; the call is made again
//! where a signal interrupts it, where the system takes fewer reads than
//! it was given, where it returns before every answer is there, and where
//! it answers `EAGAIN` or `EBUSY`. A ring takes a read of a file that this
//! layer opened to read and whose alignment is at most 4,096 bytes, if its
//! buffer is not empty, it starts below the largest offset a file can
//! have, and its aligned range is at most 128 KiB long.
//!
//! The bytes of each read that the ring's answer holds whole are then
//! copied to its buffer. After that the reads of those 32 that are left
//! are settled in the order of the list, up to the first that fails:
//!
//! - A read that the ring answered with an error fails the batch with that
//!   error. It is not made a second time: the system can report the
//!   failure of a write-back to one read and to no later one. `EAGAIN`,
//!   `EINTR` and `ECANCELED` are not taken for such an answer: a read that
//!   is answered with one of them is made by the single read.
//! - A read that was handed to a ring that had to be given up fails the
//!   batch, unless the ring knows that the system answered it or that the
//!   system did not take it: the system may have made the read, and what
//!   it brought is not known.
//! - Every other read that the ring did not bring whole is made by its
//!   file's [`VfsFile::read_exact_at`]: one that the ring was not given,
//!   one that the system is known not to have taken from it, one whose
//!   bytes the ring's answer does not hold all of, as where the file
//!   ends, and one that it answered with one of those three errors.
//!
//! A ring needs Linux 5.6 and a process that is allowed the io_uring
//! calls, which a container's seccomp profile or the
//! `kernel.io_uring_disabled` setting can refuse. A batch of two reads or
//! more that finds no ring kept asks the system for one, and up to eight
//! are kept for later batches. Once three times a ring could not be made
//! or had to be given up, this file layer asks for no further one:
//! batches go on with the rings that are kept, and where none is, with
//! single reads. A ring is
//! given up where its system call fails, where the system neither takes a
//! read nor answers one call after call, where it answers a read it was
//! not given or answers one twice, where it gives no memory for the
//! reads, and in the child of a fork. A ring that is given up in the
//! middle of a list is not replaced for the rest of that list, whose
//! reads are then single reads.
//!
//! A ring reads into memory of its own, 128 KiB for each read of the
//! largest batch it has made, which stays with the ring. It is never the
//! caller's memory, because the system writes into it until it has
//! answered: `ring.rs`, which holds all the `unsafe` code of this module,
//! says how that is kept, and what becomes of that memory when a ring is
//! given up.
//!
//! # A fork
//!
//! Linux's manual page for `open` warns that I/O with `O_DIRECT` into
//! private memory must not run beside a fork. The memory this layer reads
//! into is mappings that are marked `MADV_WIPEONFORK`, which a fork does
//! not share with the child (`ring.rs` has the argument), so a fork in
//! another thread, or while a ring that was given up still has reads
//! under way, leaves the parent's reads as they are. The mark needs Linux
//! 4.14: on an older system a read that needs memory fails.
//!
//! What this layer keeps to in the child of a fork is for a call that
//! begins there. The kept memory is zeroed and the child's own, and a
//! batch that would hand a read to a ring that the parent made gives the
//! ring up instead and makes its reads one by one. A call of this layer
//! that the forking thread was inside when it forked, which a fork from a
//! signal handler makes possible, must not be gone on with in the child:
//! the memory the call reads into is wiped there, and what it returns
//! need not be the file's bytes. What a fork does to the locks inside
//! this layer is what it does to any lock of a process with several
//! threads, so a child that goes on to read should make a file layer of
//! its own.
//!
//! A task that shares the address space of the one that made a file layer
//! and not its table of descriptors, as `clone` with `CLONE_VM` and
//! without `CLONE_FILES` makes one, must not use that layer or its files:
//! the two tables can come to differ, and the number of a descriptor can
//! then name another file there.
//!
//! [`Unsupported`]: io::ErrorKind::Unsupported

use std::{
    any::Any,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, OpenOptionsExt},
    },
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

use rustix::fs::{statx, AtFlags, OFlags, StatxFlags};

use self::{
    aligned::{read_through, span, Align, Buffers, KEPT_ALIGN, MAX_FILE_LEN},
    ring::{Outcome, Ring},
};
use crate::{
    std_fs::StdVfs,
    vfs::{DirEntry, OpenMode, SyncItem, Vfs, VfsFile, VfsLock},
};

mod aligned;
#[allow(unsafe_code)]
mod ring;
#[cfg(all(test, not(miri)))]
mod tests;

/// How many rings are kept from one batch to the next. A batch that finds
/// none kept makes one.
const KEPT_RINGS: usize = 8;

/// After so many rings that could not be made or had to be given up, no
/// further ring is made.
const RING_FAILURES: u32 = 3;

/// The [`Vfs`] over the operating system's files that opens the files it
/// reads with `O_DIRECT`: see the [module documentation](self).
///
/// Clones share the memory and the rings that reads are made with, and
/// their counts.
#[derive(Clone, Debug, Default)]
pub struct DirectVfs {
    shared: Arc<Shared>,
}

/// What the reads of a [`DirectVfs`] and of its files are made with.
#[derive(Debug, Default)]
struct Shared {
    buffers: Buffers,
    rings: Mutex<Rings>,
    preads: AtomicU64,
    ring_calls: AtomicU64,
    ring_reads: AtomicU64,
    rings_lost: AtomicU64,
    bytes: AtomicU64,
}

#[derive(Debug, Default)]
struct Rings {
    kept: Vec<Ring>,
    /// How many times a ring could not be made or had to be given up.
    failures: u32,
}

/// How many reads a [`DirectVfs`] has made, since it was made
/// ([`DirectVfs::stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectStats {
    /// The `pread64` calls on files that are open for direct reads.
    pub preads: u64,
    /// The `io_uring_enter` calls of batches.
    pub ring_calls: u64,
    /// The reads that a ring answered, with their bytes, with fewer or
    /// with an error, and whose answer this layer could match to its read.
    pub ring_reads: u64,
    /// The rings that were given up ([Reads together](self#reads-together)).
    pub rings_lost: u64,
    /// The bytes that the `pread64` calls and the reads counted in
    /// `ring_reads` brought: of each its aligned range, or as much of that
    /// as the file has.
    pub bytes: u64,
}

/// One read of a batch ([`DirectVfs::read_batch`]).
#[derive(Debug)]
pub struct ReadAt<'a> {
    /// The file to read.
    pub file: &'a dyn VfsFile,
    /// Where in the file the read starts.
    pub offset: u64,
    /// What the read fills: the file has to have that many bytes from
    /// `offset`.
    pub buf: &'a mut [u8],
}

impl DirectVfs {
    /// A file layer that reads directly, with memory and rings of its own.
    pub fn new() -> Self {
        Self::default()
    }

    /// A file layer that makes every batch with single reads, as on a
    /// system that gives no ring.
    #[cfg(all(test, not(miri)))]
    fn without_rings() -> Self {
        let vfs = Self::default();
        vfs.shared.rings().failures = RING_FAILURES;
        vfs
    }

    /// The reads made so far through this file layer, its clones and
    /// their files.
    pub fn stats(&self) -> DirectStats {
        let count = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        DirectStats {
            preads: count(&self.shared.preads),
            ring_calls: count(&self.shared.ring_calls),
            ring_reads: count(&self.shared.ring_reads),
            rings_lost: count(&self.shared.rings_lost),
            bytes: count(&self.shared.bytes),
        }
    }

    /// Fills the buffer of every read of `reads` from its file with the
    /// bytes [`VfsFile::read_exact_at`] would fill it with, with fewer
    /// system calls where the system allows: see
    /// [Reads together](self#reads-together).
    ///
    /// A file of the list may be any [`VfsFile`]. A ring is given only
    /// reads of files that a [`DirectVfs`] opened to read; every other
    /// file is read through its own `read_exact_at`.
    ///
    /// If it fails, it fails with the error of one read of the list: the
    /// error of its file's `read_exact_at`, the error the system answered
    /// it with through a ring, or an error of the kind
    /// [`Other`](ErrorKind::Other) where its ring was given up and does
    /// not say what became of it. What the buffers of the list hold is
    /// then unspecified, and every other read of the list may or may not
    /// have been made.
    pub fn read_batch(&self, reads: &mut [ReadAt<'_>]) -> io::Result<()> {
        let mut ring = if reads.len() > 1 {
            self.shared.take_ring()
        } else {
            None
        };
        let done = reads
            .chunks_mut(ring::DEPTH)
            .try_for_each(|chunk| self.shared.read_chunk(&mut ring, chunk));
        if let Some(ring) = ring {
            self.shared.give_ring(ring);
        }
        done
    }
}

impl Shared {
    fn rings(&self) -> MutexGuard<'_, Rings> {
        // A ring that a panic interrupted is not among the kept ones: a
        // ring is taken out of the list for a batch.
        self.rings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A ring for one batch: a kept one, or a new one if the system gives
    /// it and rings have not failed too often.
    fn take_ring(&self) -> Option<Ring> {
        {
            let mut rings = self.rings();
            if let Some(ring) = rings.kept.pop() {
                return Some(ring);
            }
            if rings.failures >= RING_FAILURES {
                return None;
            }
        }
        let made = Ring::new();
        if made.is_err() {
            self.rings().failures += 1;
        }
        made.ok()
    }

    fn give_ring(&self, ring: Ring) {
        let mut rings = self.rings();
        if ring.is_usable() && rings.kept.len() < KEPT_RINGS {
            rings.kept.push(ring);
        }
    }

    fn count(&self, counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }

    /// Makes the reads of `chunk`, at most as many as a ring takes: through
    /// `ring` those that it can make, and then in the order of the list
    /// every read that it did not bring whole.
    ///
    /// What the ring brought is counted, the reads it brought whole are
    /// copied out, and a ring that was given up is counted and dropped,
    /// before the first single read
    /// is made: a file of the list is any [`VfsFile`], and what its read
    /// does, a panic or a fork among it, finds nothing of this chunk left
    /// in the ring.
    fn read_chunk(&self, ring: &mut Option<Ring>, chunk: &mut [ReadAt<'_>]) -> io::Result<()> {
        let mut left: Vec<Left> = chunk.iter().map(|_| Left::Single).collect();
        // The reads the ring is given, and where in the chunk each is.
        let (entries, places): (Vec<ring::Read>, Vec<usize>) = chunk
            .iter()
            .enumerate()
            .filter(|_| ring.is_some())
            .filter_map(|(place, read)| Some((ring_read(read)?, place)))
            .unzip();
        if let Some(batch) = ring.as_mut().filter(|_| !entries.is_empty()) {
            let lost = batch.read(&entries).is_err();
            self.count(&self.ring_calls, batch.calls());
            for (slot, (entry, place)) in entries.iter().zip(places).enumerate() {
                let outcome = batch.outcome(slot);
                if let Outcome::Answered(answer) = &outcome {
                    self.count(&self.ring_reads, 1);
                    self.count(
                        &self.bytes,
                        answer.as_ref().map_or(0, |bytes| bytes.len() as u64),
                    );
                }
                let read = &mut chunk[place];
                left[place] = match settle(outcome, read.offset - entry.offset, read.buf.len()) {
                    Settled::Whole(bytes) => {
                        read.buf.copy_from_slice(bytes);
                        Left::Nothing
                    }
                    Settled::Again => Left::Single,
                    Settled::Failed(error) => Left::Failed(error),
                };
            }
            if lost {
                // It is not used again.
                self.count(&self.rings_lost, 1);
                self.rings().failures += 1;
                *ring = None;
            }
        }
        chunk
            .iter_mut()
            .zip(left)
            .try_for_each(|(read, left)| match left {
                Left::Nothing => Ok(()),
                Left::Single => read.file.read_exact_at(read.buf, read.offset),
                Left::Failed(error) => Err(error),
            })
    }
}

/// What is left to do for a read of a chunk once the ring's answers were
/// taken.
#[derive(Debug)]
enum Left {
    /// Nothing: the ring brought its bytes, and they are in its buffer.
    Nothing,
    /// The single read.
    Single,
    /// To fail with this error.
    Failed(io::Error),
}

/// What a ring's answer makes of a read of a batch.
#[derive(Debug)]
enum Settled<'a> {
    /// The ring brought the bytes of the read.
    Whole(&'a [u8]),
    /// The read is made by the single read: the ring did not make it, or
    /// brought fewer bytes than the read needs.
    Again,
    /// The read failed, with what the system reported of it to the ring,
    /// or it is not known what became of it.
    Failed(io::Error),
}

/// Settles a read of `len` bytes that starts `skip` bytes into the aligned
/// range a ring was asked for, by what became of that range in the ring.
///
/// An error is the read's own, and is not asked for a second time: the
/// system can report the failure of a write-back to one read and to no
/// later one. For the same reason a read of which the ring does not say
/// what became of it is not made again: it fails. Three errors are not
/// taken for the read's own: a read that is answered with one of them is
/// made again.
fn settle(outcome: Outcome<'_>, skip: u64, len: usize) -> Settled<'_> {
    use rustix::io::Errno;
    match outcome {
        Outcome::Answered(Ok(bytes)) => usize::try_from(skip)
            .ok()
            .and_then(|start| bytes.get(start..start.checked_add(len)?))
            .map_or(Settled::Again, Settled::Whole),
        Outcome::Answered(Err(error)) => {
            let not_made = [Errno::AGAIN, Errno::INTR, Errno::CANCELED]
                .iter()
                .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()));
            if not_made {
                Settled::Again
            } else {
                Settled::Failed(error)
            }
        }
        Outcome::NotMade => Settled::Again,
        Outcome::Unknown => Settled::Failed(io::Error::other(
            "the io_uring instance of a batch of reads was given up, and it is not known \
             whether the system made this read or what it brought",
        )),
    }
}

/// The read of a batch as a ring makes it: the aligned range around its
/// bytes. `None` for a read that a ring is not given: of a file that is
/// not open for direct reads, of nothing, of more than a ring's memory
/// holds, or with a wider alignment than that memory has.
fn ring_read(read: &ReadAt<'_>) -> Option<ring::Read> {
    let file: &dyn Any = read.file;
    let file = file.downcast_ref::<DirectFile>()?;
    let (from, to) = span(read.offset, read.buf.len(), file.align)?;
    let len = u32::try_from(to - from).ok()?;
    (len as usize <= ring::LONGEST && file.align <= KEPT_ALIGN).then_some(ring::Read {
        fd: file.file.as_raw_fd(),
        offset: from,
        len,
    })
}

impl Vfs for DirectVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        match mode {
            OpenMode::Read => Ok(Box::new(DirectFile::open(path, self.shared.clone())?)),
            OpenMode::Write(_) | OpenMode::Append(_) => StdVfs.open(path, mode),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdVfs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        StdVfs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.sync_dir(path)
    }

    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        // A handle of this layer is synced by its own call there, as a
        // file of any other file layer is.
        StdVfs.sync_together(items)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock_existing(path)
    }
}

/// A file that is open for direct reads.
#[derive(Debug)]
struct DirectFile {
    file: File,
    align: Align,
    shared: Arc<Shared>,
    sync_failure: Mutex<Option<String>>,
}

impl DirectFile {
    fn open(path: &Path, shared: Arc<Shared>) -> io::Result<Self> {
        let direct = OFlags::DIRECT.bits() as i32;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(direct)
            .open(path)
        {
            Ok(file) => file,
            // What a file system answers that takes no `O_DIRECT`, and
            // some of them for a directory.
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::INVAL.raw_os_error()) => {
                return Err(match fs::metadata(path) {
                    Ok(named) if named.is_dir() => is_a_directory(path),
                    _ => no_direct_io(path, &error.to_string()),
                });
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.is_dir() {
            return Err(is_a_directory(path));
        }
        let reported = statx(&file, "", AtFlags::EMPTY_PATH, StatxFlags::DIOALIGN)
            .ok()
            .filter(|found| {
                StatxFlags::from_bits_retain(found.stx_mask).contains(StatxFlags::DIOALIGN)
            })
            .map(|found| (found.stx_dio_mem_align, found.stx_dio_offset_align));
        let align = alignment(reported).ok_or_else(|| {
            no_direct_io(
                path,
                &format!("the system reports the alignment {reported:?}"),
            )
        })?;
        Ok(Self {
            file,
            align,
            shared,
            sync_failure: Mutex::new(None),
        })
    }

    /// The direct read: `into` and `offset` are aligned as the file needs,
    /// but for the length of a read that ends at the largest offset a file
    /// has.
    fn pread(&self, into: &mut [u8], offset: u64) -> io::Result<usize> {
        let at_the_top = offset.checked_add(into.len() as u64) == Some(MAX_FILE_LEN);
        assert!(
            self.align.fits(offset, into)
                || (at_the_top && self.align.fits_but_for_its_length(offset, into)),
            "a direct read of {} bytes at {offset} into {:p} is not aligned to {} bytes",
            into.len(),
            into.as_ptr(),
            self.align.get()
        );
        self.shared.count(&self.shared.preads, 1);
        let read = self.file.read_at(into, offset)?;
        self.shared.count(&self.shared.bytes, read as u64);
        Ok(read)
    }

    fn sync_using(&self, sync: impl FnOnce(&File) -> io::Result<()>) -> io::Result<()> {
        let mut failure = self
            .sync_failure
            .lock()
            .map_err(|_| io::Error::other("an earlier operation on this file panicked"))?;
        if let Some(failure) = &*failure {
            return Err(io::Error::other(failure.clone()));
        }
        sync(&self.file).inspect_err(|error| {
            *failure = Some(format!(
                "an earlier sync of this file failed ({error}), so this handle can no longer \
                 make it durable"
            ));
        })
    }
}

/// The alignment of a file's direct reads, from what the system reports of
/// it: the alignment of memory and of offsets, or nothing where the system
/// does not say. `None` for a file that cannot be read directly.
fn alignment(reported: Option<(u32, u32)>) -> Option<Align> {
    match reported {
        Some((memory, offset)) => Align::reported(memory, offset),
        None => Some(Align::DEFAULT),
    }
}

fn is_a_directory(path: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::IsADirectory,
        format!("{} is a directory", path.display()),
    )
}

fn no_direct_io(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        ErrorKind::Unsupported,
        format!(
            "{} cannot be opened for direct reads: {why}",
            path.display()
        ),
    )
}

fn read_only(operation: &str) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidInput,
        format!("{operation} needs a file that was opened to write"),
    )
}

impl VfsFile for DirectFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let Some((from, to)) = span(offset, buf.len(), self.align) else {
            return Ok(0);
        };
        let len = usize::try_from(to - from).unwrap_or(usize::MAX);
        self.shared.buffers.with(len, self.align, |bounce| {
            read_through(
                buf,
                offset,
                self.align,
                bounce,
                |into, at| self.pread(into, at),
                || self.len(),
            )
        })?
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> io::Result<()> {
        Err(read_only("write_at"))
    }

    fn append(&self, _buf: &[u8]) -> io::Result<u64> {
        Err(read_only("append"))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync_using(File::sync_data)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync_using(File::sync_all)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn set_len(&self, _len: u64) -> io::Result<()> {
        Err(read_only("set_len"))
    }
}
