//! The `unsafe` code of the direct I/O path: the memory that direct reads
//! read into, and one io_uring instance, through which a batch of reads is
//! handed to the system together.
//!
//! # Memory
//!
//! Linux's manual page for `open` warns that I/O with `O_DIRECT` into
//! private memory must not run beside a fork: the copy on write that a
//! fork sets up can send what the read brings to a page the process no
//! longer has. So a direct read never reads into the heap. Its memory is a
//! mapping of its own ([`Memory`]) that is marked `MADV_WIPEONFORK`: a fork
//! does not share such a mapping's pages with the child, which gets it
//! zeroed (`dup_mmap` in `kernel/fork.c` of Linux 6.12 leaves out
//! `copy_page_range` for it), so the pages a read is under way into stay
//! the parent's as they are.
//!
//! - **M1.** A [`Memory`] is a private mapping of no file that the value
//!   owns. It is as long as the value says, which is no longer than a
//!   slice may be, and the system gave it zeroed. Nothing reads or writes
//!   it but through the value's two borrows, and the system for a read it
//!   was handed (R1). Nothing unmaps it but the value's drop, which asks
//!   the system once; a value that is never dropped (R2) keeps its
//!   mapping.
//!
//! The same advice makes the mark by which a ring knows the address space
//! it was made in ([`Owner`]): a byte that is set when the ring is made and
//! that the child of a fork finds zero, whatever its process id is. The
//! mark does not tell apart tasks that share one address space, as
//! `clone` with `CLONE_VM` makes them: they share the ring's memory and
//! every value of this module, as threads do. One of them that does not
//! share the table of descriptors as well is outside what this module is
//! for: an entry names its file by the number of a descriptor.
//!
//! # What the system is given, and for how long
//!
//! A read is handed to the system as a submission entry that holds the
//! address of memory to read into. The system writes into that memory at
//! any time from the call that takes the entry until it has answered it
//! with a completion entry. So the rules of a batch are:
//!
//! - **R1.** The memory a read is given is the ring's own ([`Batch::bounce`]):
//!   never the caller's. It is as long as the read at least, and it is not
//!   freed, moved or looked at from when the read is handed over until the
//!   read is answered.
//! - **R2.** [`Batch::in_flight`] is at least the number of reads the
//!   system has taken and not answered, from before the system can take the
//!   first one. A batch that is dropped while it is not zero leaks its
//!   memory instead of freeing it. The bytes of a read are given out
//!   ([`Batch::outcome`]) only once that read is answered.
//!
//! [`Batch::read`] returns `Ok` when every read is answered. Otherwise the
//! batch is spoiled: no system call is made for it again ([`Batch::read`]
//! refuses), so by Q1 the system takes no further entry of it, and the
//! entries it did not take are never read. The count is then:
//!
//! - zero, where a batch that was not spoiled before is refused before an
//!   entry is written: for want of memory, or in an address space that
//!   did not make the ring;
//! - the entries the system took less the answers this process took from
//!   the queue, where the batch was given up because its system call
//!   failed or did nothing;
//! - the whole batch on every other way out once the count is set, which
//!   is before the entries are written: an answer that fits no read of
//!   the batch or comes a second time, more answers than entries that
//!   were taken, and a panic of the system call. A panic of one of the
//!   checks a batch begins with comes before that and leaves the count
//!   as it was: nothing was handed over.
//!
//! # The queues
//!
//! The two queues are memory that the system and this process share
//! ([`Queues`]). Each has a head and a tail that count up and wrap, and an
//! array of a power of two of entries that they index by their low bits.
//!
//! - **Q1.** The submission queue is written by this process alone: its
//!   entries and its tail. The system reads them inside the system call
//!   that submits, on the calling thread, in their order, and has moved
//!   the head over the entries it took when that call returns. A ring is
//!   made with no flag, so no thread of the system polls the queue. A copy
//!   of the address space, which the child of a fork has, shares the
//!   queues' memory with the original, so a ring is used in the address
//!   space it was made in and in no copy of it: [`Ring::read`] asks the
//!   ring's [`Owner`] and refuses in a copy, before it touches a queue. A
//!   copy that is made after a call has asked, by a fork from a signal
//!   handler while the call is inside this module, is not found by that
//!   call: the file layer rules out going on with such a call in the
//!   child.
//! - **Q2.** The completion queue is written by the system, at any time and
//!   on any thread: an entry at the tail, and then the tail. This process
//!   reads only entries below a tail it has loaded, and then moves the
//!   head. Heads and tails are read and written as atomics.
//! - **Q3.** Every offset the system reports is checked against the length
//!   of the mapping before anything is read or written there
//!   ([`Region::at`]), and every index is masked to the number of entries.
//!
//! A batch is used through `&mut`, so by one thread at a time. It starts
//! with both queues empty, which [`Batch::read`] checks, and leaves them
//! empty unless it is spoiled.

use std::{
    ffi::c_void,
    fmt, io,
    marker::PhantomData,
    mem::{size_of, ManuallyDrop},
    os::fd::{AsFd, OwnedFd, RawFd},
    ptr::{self, NonNull},
    slice,
    sync::atomic::{AtomicU32, Ordering},
};

use rustix::{
    io::Errno,
    io_uring::{
        addr_or_splice_off_in_union, io_uring_cqe, io_uring_enter, io_uring_params, io_uring_ptr,
        io_uring_setup, io_uring_sqe, io_uring_user_data, len_union, off_or_addr2_union,
        IoringEnterFlags, IoringFeatureFlags, IoringOp, IORING_OFF_SQES, IORING_OFF_SQ_RING,
    },
    mm::{mmap, mmap_anonymous, munmap, MapFlags, ProtFlags},
};

use super::aligned::{AlignedBuf, KEPT_ALIGN, KEPT_BUFFER};

/// How many reads one system call takes.
pub(super) const DEPTH: usize = 32;

/// The longest read a ring makes: the length of its memory for one read.
pub(super) const LONGEST: usize = KEPT_BUFFER;

/// How many system calls in a row, not counting those a signal
/// interrupted, may neither take a read nor bring an answer before a batch
/// is given up.
const PATIENCE: u32 = 16;

/// An answer that has not come.
const UNANSWERED: i32 = i32::MIN;

/// Zeroed memory for direct reads, which the child of a fork does not
/// share: see [Memory](self#memory).
pub(super) struct Memory {
    start: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is the process's, whichever thread made it (M1), and
// the value owns it as a boxed slice owns its bytes: it goes to another
// thread with the value.
unsafe impl Send for Memory {}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl Memory {
    /// `len` bytes of zeroed memory, at a multiple of the system's page
    /// size. It fails for more bytes than a slice may have, where the
    /// system gives no mapping, and where it does not know the advice,
    /// which came with Linux 4.14.
    pub(super) fn new(len: usize) -> io::Result<Self> {
        if isize::try_from(len).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory for direct reads that is longer than a slice may be",
            ));
        }
        // SAFETY: a new mapping of no file at an address the system
        // chooses, so it takes the place of no memory that anything has.
        let start = unsafe {
            mmap_anonymous(
                ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            )
        }?;
        let start = NonNull::new(start.cast::<u8>())
            .ok_or_else(|| io::Error::other("a mapping at address 0"))?;
        let memory = Self { start, len };
        memory.wipe_on_fork().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("memory for direct reads cannot be kept out of a fork: {error}"),
            )
        })?;
        Ok(memory)
    }

    #[cfg(not(miri))]
    fn wipe_on_fork(&self) -> io::Result<()> {
        use rustix::mm::{madvise, Advice};
        // SAFETY: the advice is for the whole of this value's own mapping
        // (M1). It changes nothing of the mapping in this process: it says
        // what the child of a fork finds there.
        unsafe {
            madvise(
                self.start.as_ptr().cast(),
                self.len,
                Advice::LinuxWipeOnFork,
            )
        }?;
        Ok(())
    }

    /// Miri has neither the advice nor a fork.
    #[cfg(miri)]
    fn wipe_on_fork(&self) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: `len` bytes, which `new` checked to be no more than a
        // slice may have, that the value owns and that were zeroed when
        // they were mapped (M1), borrowed with the value. Whoever
        // hands the address to the system does not call this while the
        // system may write there (R1).
        unsafe { slice::from_raw_parts(self.start.as_ptr(), self.len) }
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, and the value is borrowed for itself alone.
        unsafe { slice::from_raw_parts_mut(self.start.as_ptr(), self.len) }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        #[cfg(test)]
        UNMAPPED.with(|unmapped| unmapped.set(unmapped.get() + 1));
        // SAFETY: the whole of the mapping `new` made, once: the value
        // owns it (M1) and is going away, so nothing borrows its bytes.
        // If the system refuses, the mapping stays, which loses memory and
        // breaks no rule.
        let _ = unsafe { munmap(self.start.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
thread_local! {
    /// How many [`Memory`] values this thread has dropped.
    static UNMAPPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The mark of the address space it was made in: a copy of the address
/// space, which the child of a fork has, finds it wiped, as it finds every
/// [`Memory`].
#[derive(Debug)]
struct Owner(Memory);

impl Owner {
    fn new() -> io::Result<Self> {
        let mut mark = Memory::new(1)?;
        mark.bytes_mut()[0] = 1;
        Ok(Self(mark))
    }

    /// Whether this is the address space the mark was made in, and no
    /// copy of it.
    fn is_here(&self) -> bool {
        self.0.bytes()[0] == 1
    }
}

/// One read of a batch: `len` bytes of the file `fd` from `offset`, aligned
/// as the file needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Read {
    pub(super) fd: RawFd,
    pub(super) offset: u64,
    pub(super) len: u32,
}

/// Memory that holds queues: a mapping of the ring, or in a test a block of
/// the test's own.
#[derive(Clone, Copy)]
struct Region {
    start: *mut u8,
    len: usize,
}

impl Region {
    /// The address of `count` values of `T` at `offset`, if all of them are
    /// inside the region and the address is aligned for `T` (Q3).
    fn at<T>(self, offset: u32, count: u32) -> Option<*mut T> {
        let offset = usize::try_from(offset).ok()?;
        let bytes = size_of::<T>().checked_mul(usize::try_from(count).ok()?)?;
        if offset.checked_add(bytes)? > self.len {
            return None;
        }
        let at = self.start.wrapping_add(offset).cast::<T>();
        at.is_aligned().then_some(at)
    }
}

/// The two queues of a ring, as pointers into the memory the system
/// shares, which is valid for `'m`.
struct Queues<'m> {
    entries: u32,
    sq_head: *mut u32,
    sq_tail: *mut u32,
    sqes: *mut io_uring_sqe,
    cq_mask: u32,
    cq_head: *mut u32,
    cq_tail: *mut u32,
    cqes: *mut io_uring_cqe,
    /// The memory the pointers are into.
    memory: PhantomData<&'m ()>,
}

// SAFETY: the pointers are into memory that belongs to the process and not
// to a thread: a mapping, or in a test a block the test keeps. Nothing in
// `Queues` is bound to the thread that made it, and it is used through
// `&mut` (Q1, Q2), so by one thread at a time.
unsafe impl Send for Queues<'_> {}

impl Queues<'_> {
    /// The queues that `params` describes, in `rings` (both queues' heads,
    /// tails and arrays) and `entries` (the submission entries). `None` if
    /// what `params` says does not fit the two regions.
    ///
    /// # Safety
    ///
    /// `rings` and `entries` are valid for reads and writes for the
    /// lifetime the caller gives the queues, and nothing but the system
    /// and these queues reads or writes them meanwhile. `rings` holds, at
    /// the offsets `params` gives for the two masks, values that were
    /// written before.
    unsafe fn over(rings: Region, entries: Region, params: &io_uring_params) -> Option<Self> {
        let (sq, cq) = (&params.sq_off, &params.cq_off);
        let sq_entries = params.sq_entries;
        let cq_entries = params.cq_entries;
        if !sq_entries.is_power_of_two() || !cq_entries.is_power_of_two() {
            return None;
        }
        let sq_mask = rings.at::<u32>(sq.ring_mask, 1)?;
        let cq_mask = rings.at::<u32>(cq.ring_mask, 1)?;
        let array = rings.at::<u32>(sq.array, sq_entries)?;
        let queues = Self {
            entries: sq_entries,
            sq_head: rings.at(sq.head, 1)?,
            sq_tail: rings.at(sq.tail, 1)?,
            sqes: entries.at(0, sq_entries)?,
            cq_mask: cq_entries - 1,
            cq_head: rings.at(cq.head, 1)?,
            cq_tail: rings.at(cq.tail, 1)?,
            cqes: rings.at(cq.cqes, cq_entries)?,
            memory: PhantomData,
        };
        // SAFETY: both are inside `rings` and aligned (Q3), and the caller
        // says that they hold values. The system does not change a mask.
        let masks = unsafe { (sq_mask.read(), cq_mask.read()) };
        if masks != (sq_entries - 1, cq_entries - 1) {
            return None;
        }
        for index in 0..sq_entries {
            // SAFETY: `array` has `sq_entries` values inside `rings` (Q3),
            // and only this process writes the submission queue (Q1). From
            // here on slot `index` of the queue is entry `index`.
            unsafe { array.add(index as usize).write(index) };
        }
        Some(queues)
    }

    /// A head or a tail, as the atomic it is shared as.
    fn counter(&self, at: *mut u32) -> &AtomicU32 {
        // SAFETY: `at` is one of the four pointers `over` made: inside the
        // region and aligned for a `u32` (Q3), and valid for the lifetime
        // of the queues, as the caller of `over` said. The system and this
        // process both use it as an atomic (Q2).
        unsafe { AtomicU32::from_ptr(at) }
    }

    /// Writes the entries of `reads` behind the tail of the submission
    /// queue and moves the tail over them. `memory` gives the address the
    /// read of each index reads into.
    fn submit(&mut self, reads: &[Read], mut memory: impl FnMut(usize) -> *mut u8) {
        assert!(reads.len() <= self.entries as usize);
        let tail = self.counter(self.sq_tail).load(Ordering::Relaxed);
        for (index, read) in reads.iter().enumerate() {
            let entry = io_uring_sqe {
                opcode: IoringOp::Read,
                fd: read.fd,
                off_or_addr2: off_or_addr2_union { off: read.offset },
                addr_or_splice_off_in: addr_or_splice_off_in_union {
                    addr: io_uring_ptr::new(memory(index).cast::<c_void>()),
                },
                len: len_union { len: read.len },
                user_data: io_uring_user_data::from_u64(index as u64),
                ..Default::default()
            };
            let slot = tail.wrapping_add(index as u32) & (self.entries - 1);
            // SAFETY: `slot` is below `entries`, the number of entries
            // `sqes` has (Q3). The system reads an entry only below the
            // tail, which is moved after this (Q1).
            unsafe { self.sqes.add(slot as usize).write(entry) };
        }
        self.counter(self.sq_tail)
            .store(tail.wrapping_add(reads.len() as u32), Ordering::Release);
    }

    /// Where the head of the submission queue is: the system moves it over
    /// every entry it takes.
    fn taken(&self) -> u32 {
        self.counter(self.sq_head).load(Ordering::Acquire)
    }

    /// Whether the system has taken every entry and this process every
    /// answer.
    fn are_empty(&self) -> bool {
        let at = |counter: *mut u32| self.counter(counter).load(Ordering::Acquire);
        at(self.sq_head) == at(self.sq_tail) && at(self.cq_head) == at(self.cq_tail)
    }

    /// Takes every completion entry there is, gives `answer` the index and
    /// the result of each, and moves the head over them.
    fn reap(&mut self, mut answer: impl FnMut(u64, i32)) {
        let mut head = self.counter(self.cq_head).load(Ordering::Relaxed);
        let tail = self.counter(self.cq_tail).load(Ordering::Acquire);
        while head != tail {
            let slot = (head & self.cq_mask) as usize;
            // SAFETY: `slot` is below the number of entries `cqes` has
            // (Q3). The entry is below the tail that was loaded, so the
            // system has written it and writes it no more until the head
            // has passed it (Q2).
            let (index, result) = unsafe {
                let entry = self.cqes.add(slot);
                ((*entry).user_data.u64_(), (*entry).res)
            };
            answer(index, result);
            head = head.wrapping_add(1);
        }
        self.counter(self.cq_head).store(head, Ordering::Release);
    }
}

/// Why a batch did not answer every read. It is spoiled then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Spoiled {
    /// The system call failed, and not with an error that asks for
    /// another try.
    Call,
    /// The system neither took a read nor answered one, call after call.
    Idle,
    /// The system answered a read that it was not given, or one twice.
    Stray,
    /// The ring was made in another address space: this one is a copy of
    /// it, as the child of a fork has.
    Foreign,
    /// The system gave no memory for the reads to read into.
    NoMemory,
}

/// What became of one read of a batch.
#[derive(Debug)]
pub(super) enum Outcome<'a> {
    /// The system answered the read: with the bytes it brought, which can
    /// be fewer than were asked for, or with its error.
    Answered(io::Result<&'a [u8]>),
    /// The system did not take the read, and never will: it was not made.
    NotMade,
    /// The system took the read and this process has no answer to it, or
    /// the answers of the batch cannot be told apart: whether the read was
    /// made, and what it brought, is not known.
    Unknown,
}

/// The queues with the memory their reads read into.
struct Batch<'m> {
    queues: Queues<'m>,
    /// The memory of the read of each index (R1).
    bounce: Vec<AlignedBuf>,
    /// How many bytes the read of each index asked for, and what the
    /// system answered it.
    answers: Vec<(u32, i32)>,
    /// At least the reads the system has taken and not answered (R2).
    in_flight: u32,
    /// Whether the batch can be used again.
    spoiled: bool,
    /// The reads of the last batch from this index on are known not to
    /// have been taken by the system. While nothing is known, it is the
    /// number of the batch's reads.
    untaken_from: u32,
    /// Whether an answer of the last batch can be matched to its read.
    matched: bool,
    /// The system calls the last batch made.
    calls: u64,
}

impl<'m> Batch<'m> {
    fn new(queues: Queues<'m>) -> Self {
        Self {
            queues,
            bounce: Vec::new(),
            answers: Vec::new(),
            in_flight: 0,
            spoiled: false,
            untaken_from: 0,
            matched: true,
            calls: 0,
        }
    }

    /// Spoils the batch for `count` reads of which none is handed to the
    /// system.
    fn refuse(&mut self, count: usize, why: Spoiled) -> Spoiled {
        self.answers.clear();
        self.answers.resize(count, (0, UNANSWERED));
        self.spoiled = true;
        self.untaken_from = 0;
        self.matched = true;
        self.calls = 0;
        why
    }

    /// Makes `reads`, at most [`DEPTH`] of at most [`LONGEST`] bytes each.
    /// `enter` is the system call: it is given how many entries to take
    /// and how many answers to wait for, and returns how many it took.
    ///
    /// On `Ok` every read is answered. On `Err` the batch is spoiled and
    /// is never read with again. Either way [`Self::outcome`] says what
    /// became of each read.
    fn read(
        &mut self,
        reads: &[Read],
        mut enter: impl FnMut(u32, u32) -> io::Result<u32>,
    ) -> Result<(), Spoiled> {
        assert!(!self.spoiled && self.in_flight == 0);
        assert!(reads.len() <= DEPTH.min(self.queues.entries as usize));
        assert!(reads.iter().all(|read| read.len as usize <= LONGEST));
        assert!(self.queues.are_empty());
        let count = reads.len() as u32;
        while self.bounce.len() < reads.len() {
            match AlignedBuf::new(LONGEST, KEPT_ALIGN) {
                Ok(memory) => self.bounce.push(memory),
                Err(_) => return Err(self.refuse(reads.len(), Spoiled::NoMemory)),
            }
        }
        self.answers.clear();
        self.answers
            .extend(reads.iter().map(|read| (read.len, UNANSWERED)));
        self.untaken_from = count;
        self.matched = true;
        self.calls = 0;

        let start = self.queues.taken();
        // From here until every read is answered the memory is the
        // system's (R2): the count stands before the first entry is there
        // to take.
        self.in_flight = count;
        self.spoiled = true;
        let bounce = &mut self.bounce;
        self.queues
            .submit(reads, |index| bounce[index].bytes_mut().as_mut_ptr());

        let mut answered = 0;
        // The looks in a row that found nothing new after a call that was
        // not interrupted.
        let mut idle = 0;
        let mut seen = 0;
        loop {
            let taken = self.queues.taken().wrapping_sub(start).min(count);
            let answers = &mut self.answers;
            let mut new = 0;
            let mut stray = false;
            self.queues.reap(|index, result| {
                match usize::try_from(index)
                    .ok()
                    .and_then(|index| answers.get_mut(index))
                {
                    Some((_, answer)) if *answer == UNANSWERED && result != UNANSWERED => {
                        *answer = result;
                        new += 1;
                    }
                    _ => stray = true,
                }
            });
            if stray {
                self.matched = false;
                return Err(Spoiled::Stray);
            }
            answered += new;
            if answered == count {
                break;
            }
            if new > 0 || taken > seen {
                idle = 0;
            }
            if idle >= PATIENCE {
                self.given_up(start, count, answered);
                return Err(Spoiled::Idle);
            }
            seen = taken;
            // The system waits for answers only if it took every entry it
            // was given, so it never waits for more than are in flight.
            match enter(count - taken, count - answered) {
                Ok(_) => idle += 1,
                // A signal: the call is made again, however often.
                Err(error) if is(&error, Errno::INTR) => {}
                Err(error) if is(&error, Errno::AGAIN) || is(&error, Errno::BUSY) => idle += 1,
                Err(_) => {
                    self.calls += 1;
                    self.given_up(start, count, answered);
                    return Err(Spoiled::Call);
                }
            }
            self.calls += 1;
        }
        self.in_flight = 0;
        self.spoiled = false;
        Ok(())
    }

    /// Settles a batch of `count` that is given up after a system call
    /// returned and before another is made. The system takes no entry
    /// while this runs, nor later (Q1), and it takes entries in their
    /// order, so the entries it took are those before where the head of
    /// the submission queue stands now, counted from `start`. The reads in
    /// flight are those less the `answered` ones. If the two numbers
    /// cannot both be right, every read counts and nothing is known.
    fn given_up(&mut self, start: u32, count: u32, answered: u32) {
        let taken = self.queues.taken().wrapping_sub(start).min(count);
        match taken.checked_sub(answered) {
            Some(left) => {
                self.in_flight = left;
                self.untaken_from = taken;
            }
            None => {
                self.in_flight = count;
                self.matched = false;
            }
        }
    }

    /// What became of the read of `index` of the last batch. The bytes of
    /// a read are given out only once the system has answered that read
    /// (R2), so they are the batch's alone again.
    fn outcome(&self, index: usize) -> Outcome<'_> {
        let Some(&(asked, answer)) = self.answers.get(index).filter(|_| self.matched) else {
            return Outcome::Unknown;
        };
        if answer == UNANSWERED {
            return if index >= self.untaken_from as usize {
                Outcome::NotMade
            } else {
                Outcome::Unknown
            };
        }
        Outcome::Answered(match usize::try_from(answer) {
            Ok(len) => Ok(&self.bounce[index].bytes()[..len.min(asked as usize)]),
            Err(_) => Err(io::Error::from_raw_os_error(answer.wrapping_neg())),
        })
    }

    /// The memory of a batch whose reads may be in flight, taken out of it
    /// in wrappers that do not unmap it: the system may still write into
    /// it (R2).
    fn abandoned(&mut self) -> Option<Vec<ManuallyDrop<AlignedBuf>>> {
        (self.in_flight != 0).then(|| {
            std::mem::take(&mut self.bounce)
                .into_iter()
                .map(ManuallyDrop::new)
                .collect()
        })
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        // The memory of reads that may be in flight is given up, not
        // unmapped. Everything else is freed with the batch.
        let _ = self.abandoned();
    }
}

/// Whether a failed system call failed with `errno`.
fn is(error: &io::Error, errno: Errno) -> bool {
    error.raw_os_error() == Some(errno.raw_os_error())
}

/// A mapping of a part of a ring, unmapped when it is dropped.
struct Mapping {
    start: NonNull<c_void>,
    len: usize,
}

// SAFETY: a mapping is the process's, whichever thread made it, and any
// thread may unmap it.
unsafe impl Send for Mapping {}

impl Mapping {
    fn of(ring: &OwnedFd, len: usize, offset: u64) -> io::Result<Self> {
        // SAFETY: a new mapping at an address the system chooses, so it
        // takes the place of no memory that anything has. It is shared
        // with the system, which is what a ring is for, and is read and
        // written through `Queues` only.
        let start = unsafe {
            mmap(
                ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED | MapFlags::POPULATE,
                ring,
                offset,
            )
        }?;
        let start =
            NonNull::new(start).ok_or_else(|| io::Error::other("a mapping at address 0"))?;
        Ok(Self { start, len })
    }

    fn region(&self) -> Region {
        Region {
            start: self.start.as_ptr().cast(),
            len: self.len,
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the whole of the mapping `of` made, once: the value owns
        // it. The `Queues` over it are not used after this, because a
        // `Ring` drops its batch before its mappings. If the system
        // refuses, the mapping stays, which loses memory and breaks no
        // rule.
        let _ = unsafe { munmap(self.start.as_ptr(), self.len) };
    }
}

/// Whether the system the tests run on gives rings. It has to, unless
/// whoever runs the tests says `LIGNIN_IO_URING=absent`, and then it must
/// not: a run that is told of no ring does not pass on a system with one.
#[cfg(test)]
pub(super) fn system_gives_rings() -> bool {
    let ring = Ring::new();
    match std::env::var("LIGNIN_IO_URING").as_deref() {
        Err(_) => {
            ring.expect(
                "this test needs io_uring: Linux 5.6 or later, and a process that is allowed the \
                 calls. LIGNIN_IO_URING=absent says that the system has none",
            );
            true
        }
        Ok("absent") => {
            assert!(ring.is_err(), "LIGNIN_IO_URING=absent, and there is a ring");
            false
        }
        Ok(other) => panic!("LIGNIN_IO_URING is `absent` or is not set: {other}"),
    }
}

/// A ring of the system with its queues mapped.
///
/// The fields are dropped in their order: the batch, whose queues point
/// into the mappings, then the mappings, then the ring itself.
pub(super) struct Ring {
    /// Its queues are in the two mappings, which have no lifetime of
    /// their own to give it: `'static` here says "for as long as this
    /// ring", and the batch does not leave the ring.
    batch: Batch<'static>,
    _rings: Mapping,
    _entries: Mapping,
    ring: OwnedFd,
    /// The address space the ring was made in (Q1).
    owner: Owner,
}

impl fmt::Debug for Ring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ring")
            .field("ring", &self.ring)
            .field("spoiled", &self.batch.spoiled)
            .finish_non_exhaustive()
    }
}

impl Ring {
    /// A new ring, if the system gives one that reads can be made with:
    /// Linux 5.6 or later, where the process is allowed the calls.
    pub(super) fn new() -> io::Result<Self> {
        let unsupported = |what: &str| io::Error::new(io::ErrorKind::Unsupported, what.to_string());
        let owner = Owner::new()?;
        let mut params = io_uring_params::default();
        // SAFETY: no flag is set, so the call reads nothing but `params`,
        // which it fills in.
        let ring = unsafe { io_uring_setup(DEPTH as u32, &mut params) }?;
        // Both queues in one mapping came with Linux 5.4. The read at an
        // offset into memory came with 5.6, as did the feature that is
        // asked for with it; and since 5.5 a call that takes fewer entries
        // than it was given does not wait, which `Batch::read` relies on.
        let needed = IoringFeatureFlags::SINGLE_MMAP | IoringFeatureFlags::RW_CUR_POS;
        if !params.features.contains(needed) {
            return Err(unsupported(
                "this system's io_uring is older than Linux 5.6",
            ));
        }
        let size = |offset: u32, entries: u32, entry: usize| {
            (entries as usize)
                .checked_mul(entry)
                .and_then(|bytes| bytes.checked_add(offset as usize))
        };
        let lens = (
            size(params.sq_off.array, params.sq_entries, size_of::<u32>()),
            size(
                params.cq_off.cqes,
                params.cq_entries,
                size_of::<io_uring_cqe>(),
            ),
            size(0, params.sq_entries, size_of::<io_uring_sqe>()),
        );
        let (Some(sq), Some(cq), Some(entries)) = lens else {
            return Err(unsupported("the ring's queues are too long to map"));
        };
        let rings = Mapping::of(&ring, sq.max(cq), IORING_OFF_SQ_RING)?;
        let entries = Mapping::of(&ring, entries, IORING_OFF_SQES)?;
        // SAFETY: the two mappings are of this ring, are readable and
        // writable, and live as long as the `Ring` that is made here,
        // which keeps the queues to itself and drops them before the
        // mappings: that is the lifetime the queues are given. Nothing
        // else has the mappings. The system wrote the masks when it made
        // the ring.
        let queues = unsafe { Queues::over(rings.region(), entries.region(), &params) }
            .ok_or_else(|| unsupported("the ring's queues are not where the system said"))?;
        Ok(Self {
            batch: Batch::new(queues),
            _rings: rings,
            _entries: entries,
            ring,
            owner,
        })
    }

    /// A ring that was made in another address space, as the child of a
    /// fork finds one: with its mark wiped.
    #[cfg(test)]
    pub(super) fn foreign() -> io::Result<Self> {
        let mut ring = Self::new()?;
        ring.owner.0.bytes_mut()[0] = 0;
        Ok(ring)
    }

    /// A ring whose system call fails for good, for the tests of what
    /// becomes of a batch then.
    #[cfg(all(test, not(miri)))]
    pub(super) fn broken() -> io::Result<Self> {
        let mut ring = Self::new()?;
        ring.ring = std::fs::File::open("/dev/null")?.into();
        Ok(ring)
    }

    /// Makes `reads` with the system call of the ring: see [`Batch::read`].
    /// In a copy of the address space the ring was made in it makes none,
    /// touches no queue, and the ring is spoiled (Q1).
    pub(super) fn read(&mut self, reads: &[Read]) -> Result<(), Spoiled> {
        if !self.owner.is_here() {
            return Err(self.batch.refuse(reads.len(), Spoiled::Foreign));
        }
        let ring = self.ring.as_fd();
        self.batch.read(reads, |submit, wait| {
            // SAFETY: the entries the call takes are the reads `Batch::read`
            // has just written, whose memory is the batch's own and stays
            // as it is until each is answered (R1, R2). No flag but the
            // one to wait is set, so the call reads no other argument.
            unsafe { io_uring_enter(ring, submit, wait, IoringEnterFlags::GETEVENTS) }
                .map_err(io::Error::from)
        })
    }

    /// What became of the read of `index` of the last batch.
    pub(super) fn outcome(&self, index: usize) -> Outcome<'_> {
        self.batch.outcome(index)
    }

    /// The system calls the last batch made.
    pub(super) fn calls(&self) -> u64 {
        self.batch.calls
    }

    /// Whether the ring can be used for another batch.
    pub(super) fn is_usable(&self) -> bool {
        !self.batch.spoiled
    }
}

#[cfg(test)]
mod tests {

    use rustix::io_uring::{io_cqring_offsets, io_sqring_offsets};

    use super::*;

    /// The layout the model gives its rings: the submission queue's
    /// counters, then its array, then the completion queue's counters and
    /// its entries.
    fn layout(sq_entries: u32, cq_entries: u32) -> (io_uring_params, usize) {
        let mut sq = io_sqring_offsets::default();
        (sq.head, sq.tail, sq.ring_mask, sq.ring_entries) = (0, 4, 8, 12);
        (sq.flags, sq.dropped, sq.array) = (16, 20, 64);
        let mut cq = io_cqring_offsets::default();
        let base = 64 + 4 * sq_entries;
        (cq.head, cq.tail, cq.ring_mask, cq.ring_entries) = (base, base + 4, base + 8, base + 12);
        (cq.overflow, cq.flags, cq.cqes) = (base + 16, base + 20, base + 64);
        let mut params = io_uring_params::default();
        (params.sq_entries, params.cq_entries) = (sq_entries, cq_entries);
        (params.sq_off, params.cq_off) = (sq, cq);
        let len = (base + 64) as usize + cq_entries as usize * size_of::<io_uring_cqe>();
        (params, len)
    }

    /// The byte of the model's file at `offset`.
    fn byte(fd: RawFd, offset: u64) -> u8 {
        (offset as u8).wrapping_mul(31) ^ ((offset >> 12) as u8).wrapping_mul(13) ^ fd as u8
    }

    /// What the model of the system does in one system call.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Does {
        /// Takes the entries it is given and reads the whole of each.
        Reads,
        /// Takes the entries and reads so many bytes of each.
        Brings(i32),
        /// Takes the entries and fails each read with this error number.
        Fails(i32),
        /// Takes at most so many entries, and reads them.
        Takes(u32),
        /// Takes at most so many entries and answers none of them in this
        /// call: it holds them.
        Holds(u32),
        /// Takes nothing, and reads so many of the reads it holds, the one
        /// it took last first.
        Releases(u32),
        /// Takes nothing, and fails the read it has held longest with this
        /// error number.
        FailsHeld(i32),
        /// Takes the entries and answers each with an index that was not
        /// given.
        Strays,
        /// Takes the entries and answers each read twice.
        Doubles,
        /// Fails the call itself with this error number, having taken
        /// nothing.
        Refuses(i32),
        /// Takes the entries, holds them, and fails the call for good.
        TakesAndBreaks,
        /// Takes the entries, reads the whole of each, and fails the call
        /// for good.
        ReadsAndBreaks,
    }

    use model::Model;

    /// The model of the system's side of a ring. It is in a module of its
    /// own so that the tests use its blocks, and the entries it holds,
    /// through the functions here, which keep a model to its one batch.
    mod model {
        use std::{
            cell::{Cell, RefCell},
            collections::VecDeque,
        };

        use super::*;

        /// A model of the system's side of a ring, in memory of its own:
        /// it takes entries from the submission queue, reads a file of its
        /// own into the memory they name, and puts answers in the
        /// completion queue.
        pub(super) struct Model {
            /// The two blocks, which the model owns and reaches only
            /// through `rings` and `entries`.
            _rings: Vec<u64>,
            _entries: Vec<io_uring_sqe>,
            rings: Region,
            entries: Region,
            params: io_uring_params,
            script: RefCell<VecDeque<Does>>,
            calls: RefCell<Vec<(u32, u32)>>,
            seen: RefCell<Vec<Read>>,
            /// The entries it took and has not answered. Each names memory
            /// of the model's one batch.
            held: RefCell<VecDeque<io_uring_sqe>>,
            /// Whether the one batch of the model was made.
            made: Cell<bool>,
            /// Whether memory of the batch was freed under the model: it
            /// takes no call then.
            ended: Cell<bool>,
        }

        impl Model {
            pub(super) fn new(sq_entries: u32, cq_entries: u32) -> Self {
                let (params, len) = layout(sq_entries, cq_entries);
                let mut rings = vec![0u64; len.div_ceil(8)];
                let mut entries = vec![io_uring_sqe::default(); sq_entries as usize];
                let model = Self {
                    rings: Region {
                        start: rings.as_mut_ptr().cast(),
                        len,
                    },
                    entries: Region {
                        start: entries.as_mut_ptr().cast(),
                        len: entries.len() * size_of::<io_uring_sqe>(),
                    },
                    _rings: rings,
                    _entries: entries,
                    params,
                    script: RefCell::default(),
                    calls: RefCell::default(),
                    seen: RefCell::default(),
                    held: RefCell::default(),
                    made: Cell::new(false),
                    ended: Cell::new(false),
                };
                model
                    .counter(params.sq_off.ring_mask)
                    .store(sq_entries - 1, Ordering::Relaxed);
                model
                    .counter(params.cq_off.ring_mask)
                    .store(cq_entries - 1, Ordering::Relaxed);
                model
            }

            /// The batch on the model's queues. A model has one, which
            /// borrows it.
            pub(super) fn batch(&self) -> Batch<'_> {
                assert!(!self.made.replace(true), "a model has one batch");
                // SAFETY: the two blocks are the model's, which the batch
                // borrows, so they are valid for the lifetime the queues
                // get here. They are read and written by this one batch
                // and by the model, which is the system here: it reads and
                // writes them as the system does. The masks were put
                // there.
                let queues = unsafe { Queues::over(self.rings, self.entries, &self.params) };
                Batch::new(queues.expect("queues"))
            }

            /// Where the model's blocks are and what its queues are like,
            /// for the test of queues that do not fit.
            pub(super) fn blocks(&self) -> (Region, Region, io_uring_params) {
                (self.rings, self.entries, self.params)
            }

            fn counter(&self, offset: u32) -> &AtomicU32 {
                // SAFETY: inside the block and aligned, as `Region::at`
                // checked, and used as an atomic by the queues as well.
                unsafe { AtomicU32::from_ptr(self.rings.at(offset, 1).expect("in the block")) }
            }

            /// Moves the head and the tail of each queue, which has to be
            /// empty, to where they are about to wrap around.
            pub(super) fn wind(&self) {
                let (sq, cq) = (&self.params.sq_off, &self.params.cq_off);
                for (head, tail, to) in [
                    (sq.head, sq.tail, u32::MAX - 2),
                    (cq.head, cq.tail, u32::MAX - 7),
                ] {
                    let (head, tail) = (self.counter(head), self.counter(tail));
                    assert_eq!(
                        head.load(Ordering::Relaxed),
                        tail.load(Ordering::Relaxed),
                        "a queue that holds something"
                    );
                    head.store(to, Ordering::Relaxed);
                    tail.store(to, Ordering::Relaxed);
                }
            }

            /// Leaves an answer in the completion queue that no batch
            /// asked for: the entry at the tail as it is.
            pub(super) fn leave_an_answer(&self) {
                self.counter(self.params.cq_off.tail)
                    .fetch_add(1, Ordering::Relaxed);
            }

            /// Takes the entry at the head of the submission queue,
            /// through the queue's array of indices, as the system does.
            fn take(&self) -> Option<io_uring_sqe> {
                let sq = &self.params.sq_off;
                let head = self.counter(sq.head);
                let at = head.load(Ordering::Relaxed);
                if at == self.counter(sq.tail).load(Ordering::Acquire) {
                    return None;
                }
                let slot = (at & (self.params.sq_entries - 1)) as usize;
                let array = self
                    .rings
                    .at::<u32>(sq.array, self.params.sq_entries)
                    .expect("in the block");
                let entries = self
                    .entries
                    .at::<io_uring_sqe>(0, self.params.sq_entries)
                    .expect("in the block");
                // SAFETY: both are inside their blocks, as `Region::at`
                // checked: the array for the slot, which is masked, and
                // the entries for the index, which is checked here as the
                // system checks it. The entry is below the tail, so the
                // batch wrote it and does not write it again before the
                // head passes it.
                let entry = unsafe {
                    let index = array.add(slot).read();
                    assert!(index < self.params.sq_entries, "an index outside the queue");
                    entries.add(index as usize).read()
                };
                assert!(entry.opcode == IoringOp::Read);
                head.store(at.wrapping_add(1), Ordering::Release);
                Some(entry)
            }

            /// Makes the read of `entry`, which the model took from the
            /// queue, and answers it. `result` is the count or the error
            /// it answers with, or the whole read if there is none, and
            /// `tag` what is added to the index it answers under.
            fn answer(&self, entry: &io_uring_sqe, result: Option<i32>, tag: u64) {
                // SAFETY: the fields a read is submitted with.
                let (offset, memory, len, index) = unsafe {
                    (
                        entry.off_or_addr2.off,
                        entry.addr_or_splice_off_in.addr.ptr.cast::<u8>(),
                        entry.len.len,
                        entry.user_data.u64_(),
                    )
                };
                self.seen.borrow_mut().push(Read {
                    fd: entry.fd,
                    offset,
                    len,
                });
                let result = result.unwrap_or(len as i32);
                // What it writes is never more than it was asked for, also
                // where the count it answers with is.
                for at in 0..result.clamp(0, len as i32) as usize {
                    // SAFETY: the entry is one that `take` took from the
                    // queue of the model's one batch, in this call or in
                    // an earlier one (`held`), so the memory it names is
                    // that batch's own for this read, `len` bytes of it at
                    // least. The batch does not touch it, and does not
                    // unmap it, while the read is in flight: what these
                    // tests are for. A test that unmaps it does so through
                    // `free`, which forgets the entries that are held and
                    // ends the model.
                    unsafe { memory.add(at).write(byte(entry.fd, offset + at as u64)) };
                }
                let cq = &self.params.cq_off;
                let tail = self.counter(cq.tail);
                let at = tail.load(Ordering::Relaxed);
                let slot = (at & (self.params.cq_entries - 1)) as usize;
                let entries = self
                    .rings
                    .at::<io_uring_cqe>(cq.cqes, self.params.cq_entries)
                    .expect("in the block");
                // SAFETY: inside the block, as `Region::at` checked, and
                // at the tail, where the batch does not read yet.
                unsafe {
                    let entry = entries.add(slot);
                    (*entry).user_data = io_uring_user_data::from_u64(index + tag);
                    (*entry).res = result;
                }
                tail.store(at.wrapping_add(1), Ordering::Release);
            }

            /// The system call, as the model answers it.
            fn enter(&self, submit: u32, wait: u32) -> io::Result<u32> {
                assert!(!self.ended.get(), "the model has ended");
                self.calls.borrow_mut().push((submit, wait));
                let does = self.script.borrow_mut().pop_front().unwrap_or(Does::Reads);
                let mut held = self.held.borrow_mut();
                let most = match does {
                    Does::Refuses(errno) => return Err(io::Error::from_raw_os_error(errno)),
                    Does::Releases(count) => {
                        for _ in 0..count {
                            let entry = held.pop_back().expect("a read that is held");
                            self.answer(&entry, None, 0);
                        }
                        0
                    }
                    Does::FailsHeld(errno) => {
                        let entry = held.pop_front().expect("a read that is held");
                        self.answer(&entry, Some(-errno), 0);
                        0
                    }
                    Does::Takes(most) | Does::Holds(most) => most,
                    _ => u32::MAX,
                };
                let mut taken = 0;
                while taken < submit.min(most) {
                    let Some(entry) = self.take() else { break };
                    taken += 1;
                    match does {
                        Does::Holds(_) | Does::TakesAndBreaks => held.push_back(entry),
                        Does::Brings(count) => self.answer(&entry, Some(count), 0),
                        Does::Fails(errno) => self.answer(&entry, Some(-errno), 0),
                        Does::Strays => self.answer(&entry, None, 1000),
                        Does::Doubles => {
                            self.answer(&entry, None, 0);
                            self.answer(&entry, None, 0);
                        }
                        _ => self.answer(&entry, None, 0),
                    }
                }
                if matches!(does, Does::TakesAndBreaks | Does::ReadsAndBreaks) {
                    return Err(io::Error::from_raw_os_error(Errno::BADF.raw_os_error()));
                }
                Ok(taken)
            }

            /// A batch of `reads` with the model as the system.
            pub(super) fn read(
                &self,
                batch: &mut Batch<'_>,
                reads: &[Read],
            ) -> Result<(), Spoiled> {
                self.is_on(batch);
                batch.read(reads, |submit, wait| self.enter(submit, wait))
            }

            /// Holds the model to its own batch: the one whose queues are
            /// in the model's blocks.
            fn is_on(&self, batch: &Batch<'_>) {
                assert!(
                    ptr::eq(batch.queues.sqes.cast::<u8>(), self.entries.start),
                    "the batch of another model"
                );
            }

            pub(super) fn script(&self, does: impl IntoIterator<Item = Does>) {
                self.script.borrow_mut().extend(does);
            }

            /// The calls the model was given so far: how many entries to
            /// take and how many answers to wait for.
            pub(super) fn calls(&self) -> Vec<(u32, u32)> {
                self.calls.borrow().clone()
            }

            /// The reads the model made so far, in the order it made them.
            pub(super) fn seen(&self) -> Vec<Read> {
                self.seen.borrow().clone()
            }

            /// Forgets the calls and the reads so far.
            pub(super) fn forget(&self) {
                self.calls.borrow_mut().clear();
                self.seen.borrow_mut().clear();
            }

            /// Unmaps the memory that the model's batch gives up for the
            /// reads it has in flight, and returns how many buffers that
            /// was.
            /// The model is the system, and it writes no more: it forgets
            /// the reads it holds, and takes no further call.
            pub(super) fn free(&self, batch: &mut Batch<'_>) -> usize {
                self.is_on(batch);
                let memory = batch.abandoned().expect("reads in flight");
                self.held.borrow_mut().clear();
                self.ended.set(true);
                let count = memory.len();
                memory
                    .into_iter()
                    .map(ManuallyDrop::into_inner)
                    .for_each(drop);
                count
            }
        }
    }

    /// What the lengths of the model's reads are multiples of. Miri runs
    /// the model a byte at a time, so the reads are short there.
    const UNIT: u32 = if cfg!(miri) { 64 } else { 4096 };

    fn reads(count: usize) -> Vec<Read> {
        (0..count)
            .map(|index| Read {
                fd: 3 + index as RawFd % 4,
                offset: 4096 * (7 * index as u64 % 50),
                len: UNIT * (1 + index as u32 % 3),
            })
            .collect()
    }

    /// Whether the read of `index` was answered with all the bytes of
    /// `read`.
    fn is_read(batch: &Batch<'_>, index: usize, read: &Read) -> bool {
        let Outcome::Answered(Ok(bytes)) = batch.outcome(index) else {
            return false;
        };
        let expected = (0..u64::from(read.len)).map(|at| byte(read.fd, read.offset + at));
        bytes.iter().copied().eq(expected)
    }

    fn assert_read(batch: &Batch<'_>, reads: &[Read]) {
        for (index, read) in reads.iter().enumerate() {
            assert!(is_read(batch, index, read), "read {index}");
        }
        assert!(matches!(batch.outcome(reads.len()), Outcome::Unknown));
    }

    /// The error number a read was answered with.
    fn failed_with(batch: &Batch<'_>, index: usize) -> Option<i32> {
        match batch.outcome(index) {
            Outcome::Answered(Err(error)) => error.raw_os_error(),
            _ => None,
        }
    }

    fn unmapped() -> usize {
        UNMAPPED.with(|unmapped| unmapped.get())
    }

    #[test]
    fn memory_is_zeroed_whole_pages_that_are_unmapped_once() {
        let before = unmapped();
        for len in [1, 4096, 5000, 132 << 10] {
            let mut memory = Memory::new(len).unwrap();
            assert_eq!(memory.bytes().len(), len);
            assert!(memory.bytes().iter().all(|byte| *byte == 0));
            assert_eq!(memory.bytes().as_ptr().addr() % 4096, 0);
            memory.bytes_mut().fill(7);
            assert!(memory.bytes().iter().all(|byte| *byte == 7));
            assert!(format!("{memory:?}").starts_with("Memory"));
        }
        assert_eq!(unmapped(), before + 4);
        // The system maps nothing of no length, and the value is not made.
        // Nor is one of more bytes than a slice may have, which the system
        // is not asked for.
        assert!(Memory::new(0).is_err());
        let too_long = Memory::new(isize::MAX as usize + 1).unwrap_err();
        assert_eq!(too_long.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(unmapped(), before + 4);
    }

    #[test]
    fn memory_goes_to_another_thread() {
        let mut memory = Memory::new(8192).unwrap();
        memory.bytes_mut()[8191] = 9;
        let read = std::thread::spawn(move || memory.bytes()[8191]);
        assert_eq!(read.join().unwrap(), 9);
    }

    #[test]
    fn an_owner_is_the_process_whose_mark_is_not_wiped() {
        let mut owner = Owner::new().unwrap();
        assert!(owner.is_here());
        // What the child of a fork finds: the mapping zeroed.
        owner.0.bytes_mut()[0] = 0;
        assert!(!owner.is_here());
    }

    #[test]
    fn a_batch_is_one_call_that_takes_every_read_and_waits_for_every_answer() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        for count in [1, 2, 7, 32] {
            let reads = reads(count);
            model.forget();
            model.read(&mut batch, &reads).unwrap();
            assert_eq!(batch.calls, 1);
            assert_eq!(model.calls(), [(count as u32, count as u32)]);
            assert_eq!(model.seen(), reads);
            assert_read(&batch, &reads);
        }
        assert!(!batch.spoiled);
    }

    #[test]
    fn the_queues_wrap_and_every_batch_finds_them_empty() {
        // 8 entries and batches of 5: the tail passes the end of the array
        // in every second batch.
        let model = Model::new(8, 16);
        let mut batch = model.batch();
        for round in 0..if cfg!(miri) { 8 } else { 40 } {
            let reads: Vec<Read> = reads(5 + round).split_off(round);
            model.read(&mut batch, &reads).unwrap();
            assert_eq!(batch.calls, 1);
            assert_read(&batch, &reads);
        }
        // And with counters that are about to wrap around.
        model.wind();
        for _ in 0..4 {
            let reads = reads(7);
            model.read(&mut batch, &reads).unwrap();
            assert_read(&batch, &reads);
        }
    }

    #[test]
    fn a_short_read_and_a_failed_read_are_answered_as_they_are() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        let reads = reads(3);
        let len = |batch: &Batch<'_>, index| match batch.outcome(index) {
            Outcome::Answered(Ok(bytes)) => Some(bytes.len()),
            _ => None,
        };
        model.script([Does::Brings(40)]);
        model.read(&mut batch, &reads).unwrap();
        for index in 0..3 {
            assert_eq!(len(&batch, index), Some(40));
        }
        model.script([Does::Brings(0)]);
        model.read(&mut batch, &reads).unwrap();
        assert_eq!(len(&batch, 0), Some(0));
        model.script([Does::Fails(Errno::IO.raw_os_error())]);
        model.read(&mut batch, &reads).unwrap();
        for index in 0..3 {
            assert_eq!(failed_with(&batch, index), Some(Errno::IO.raw_os_error()));
        }
        // A count beyond what a read asked for is not believed.
        model.script([Does::Brings(i32::MAX)]);
        model.read(&mut batch, &reads[..1]).unwrap();
        assert_eq!(len(&batch, 0), Some(reads[0].len as usize));
        assert!(!batch.spoiled);
    }

    #[test]
    fn a_call_that_takes_fewer_reads_or_is_interrupted_is_made_again() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        let reads = reads(10);
        model.script([
            Does::Refuses(Errno::INTR.raw_os_error()),
            Does::Takes(4),
            Does::Refuses(Errno::AGAIN.raw_os_error()),
            Does::Takes(1),
            Does::Refuses(Errno::BUSY.raw_os_error()),
        ]);
        model.read(&mut batch, &reads).unwrap();
        assert_eq!(batch.calls, 6);
        assert_eq!(
            model.calls(),
            [(10, 10), (10, 10), (6, 6), (6, 6), (5, 5), (5, 5)]
        );
        assert_eq!(model.seen(), reads);
        assert_read(&batch, &reads);
        assert!(!batch.spoiled);
    }

    #[test]
    fn answers_that_come_late_and_in_another_order_are_matched_to_their_reads() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        let reads = reads(5);
        model.script([Does::Holds(5), Does::Releases(2), Does::Releases(3)]);
        model.read(&mut batch, &reads).unwrap();
        assert_eq!(model.calls(), [(5, 5), (0, 5), (0, 3)]);
        // The last read was answered first.
        assert_eq!(model.seen()[0], reads[4]);
        assert_read(&batch, &reads);
        assert!(!batch.spoiled);
    }

    #[test]
    fn a_call_that_fails_for_good_spoils_the_batch_and_its_memory_is_given_up() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        let reads = reads(6);
        model.script([Does::TakesAndBreaks]);
        assert_eq!(model.read(&mut batch, &reads), Err(Spoiled::Call));
        assert!(batch.spoiled);
        assert_eq!((batch.in_flight, batch.calls), (6, 1));
        // Nothing is given out of memory that the system may write, and
        // nothing is known of a read it took.
        for index in 0..6 {
            assert!(matches!(batch.outcome(index), Outcome::Unknown));
        }
        // The memory is taken out of a batch that goes, so that the system
        // can still write it. Here the model is the system and writes no
        // more, so the test frees it.
        assert_eq!(model.free(&mut batch), 6);
        assert!(batch.bounce.is_empty());
    }

    #[test]
    fn answers_that_come_with_a_call_that_fails_are_not_looked_at() {
        // The system took every read and answered each, and then its call
        // failed. The batch does not look at the queue again: every read
        // counts as in flight, nothing is known of any of them, and the
        // memory is given up though no read is under way.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([Does::ReadsAndBreaks]);
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::Call));
        assert_eq!((batch.in_flight, batch.calls), (3, 1));
        for index in 0..3 {
            assert!(matches!(batch.outcome(index), Outcome::Unknown));
        }
        assert!(!batch.queues.are_empty());
        model.free(&mut batch);
    }

    #[test]
    #[should_panic(expected = "the batch of another model")]
    fn a_model_takes_no_batch_but_its_own() {
        let (model, other) = (Model::new(32, 64), Model::new(32, 64));
        let mut batch = other.batch();
        let _ = model.read(&mut batch, &reads(1));
    }

    #[test]
    fn a_batch_that_is_given_up_says_what_became_of_each_read() {
        // Four of six are taken and none is answered, and then the call
        // fails for good.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([Does::Holds(4), Does::Refuses(Errno::BADF.raw_os_error())]);
        assert_eq!(model.read(&mut batch, &reads(6)), Err(Spoiled::Call));
        assert_eq!(batch.in_flight, 4);
        assert_eq!(model.calls(), [(6, 6), (2, 6)]);
        for index in 0..6 {
            match batch.outcome(index) {
                Outcome::Unknown => assert!(index < 4, "{index}"),
                Outcome::NotMade => assert!(index >= 4, "{index}"),
                Outcome::Answered(_) => panic!("{index} was not answered"),
            }
        }
        model.free(&mut batch);

        // Two of six are answered, three more are taken, and then nothing
        // comes.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        let six = reads(6);
        model.script([Does::Takes(2), Does::Holds(3)]);
        model.script((0..=PATIENCE).map(|_| Does::Holds(0)));
        assert_eq!(model.read(&mut batch, &six), Err(Spoiled::Idle));
        assert_eq!(batch.in_flight, 3);
        // The reads that were answered have their bytes: the system is
        // done with their memory.
        assert!(is_read(&batch, 0, &six[0]) && is_read(&batch, 1, &six[1]));
        for index in 2..5 {
            assert!(matches!(batch.outcome(index), Outcome::Unknown));
        }
        assert!(matches!(batch.outcome(5), Outcome::NotMade));
        model.free(&mut batch);

        // The call fails for good before the system took anything: nothing
        // is in flight, no read was made, and the batch frees its memory.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([Does::Refuses(Errno::BADF.raw_os_error())]);
        assert_eq!(model.read(&mut batch, &reads(6)), Err(Spoiled::Call));
        assert_eq!((batch.in_flight, batch.calls), (0, 1));
        assert!(batch.spoiled);
        for index in 0..6 {
            assert!(matches!(batch.outcome(index), Outcome::NotMade));
        }
        assert!(batch.abandoned().is_none());
        assert_eq!(batch.bounce.len(), 6);
    }

    #[test]
    fn an_error_that_was_answered_is_kept_when_the_batch_is_given_up_after_it() {
        // Two of three are taken. The first is answered with an error, and
        // then the call fails for good.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([
            Does::Holds(2),
            Does::FailsHeld(Errno::IO.raw_os_error()),
            Does::Refuses(Errno::BADF.raw_os_error()),
        ]);
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::Call));
        assert_eq!((batch.in_flight, batch.calls), (1, 3));
        assert_eq!(failed_with(&batch, 0), Some(Errno::IO.raw_os_error()));
        assert!(matches!(batch.outcome(1), Outcome::Unknown));
        assert!(matches!(batch.outcome(2), Outcome::NotMade));
        model.free(&mut batch);
    }

    #[test]
    #[should_panic(expected = "!self.spoiled")]
    fn a_spoiled_batch_makes_no_system_call_again() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([Does::Refuses(Errno::BADF.raw_os_error())]);
        assert_eq!(model.read(&mut batch, &reads(2)), Err(Spoiled::Call));
        let _ = batch.read(&reads(2), |_, _| unreachable!("no call is made"));
    }

    #[test]
    fn a_batch_that_is_dropped_frees_its_memory_unless_a_read_is_in_flight() {
        // Every read answered: the memory of each goes with the batch.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.read(&mut batch, &reads(3)).unwrap();
        let before = unmapped();
        drop(batch);
        assert_eq!(unmapped(), before + 3);

        // Given up with nothing taken: the same.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([Does::Refuses(Errno::BADF.raw_os_error())]);
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::Call));
        let before = unmapped();
        drop(batch);
        assert_eq!(unmapped(), before + 3);

        // Given up with reads in flight: none of it is unmapped, and it
        // stays the process's for as long as the process lives.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script([Does::TakesAndBreaks]);
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::Call));
        let before = unmapped();
        drop(batch);
        assert_eq!(unmapped(), before);
    }

    #[test]
    fn a_ring_that_does_nothing_is_given_up_after_some_calls() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script((0..PATIENCE).map(|_| Does::Refuses(Errno::AGAIN.raw_os_error())));
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::Idle));
        assert_eq!(model.calls().len(), PATIENCE as usize);
        assert_eq!(batch.calls, u64::from(PATIENCE));
        // Nothing was taken, so nothing is in flight and no read was made.
        assert!(batch.spoiled);
        assert_eq!(batch.in_flight, 0);
        assert!(matches!(batch.outcome(0), Outcome::NotMade));

        // Calls that a signal interrupts are made again without end.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.script((0..10 * PATIENCE).map(|_| Does::Refuses(Errno::INTR.raw_os_error())));
        let reads = reads(3);
        model.read(&mut batch, &reads).unwrap();
        assert_eq!(batch.calls, u64::from(10 * PATIENCE + 1));
        assert_read(&batch, &reads);
    }

    #[test]
    fn an_answer_that_fits_no_read_spoils_the_batch_and_nothing_is_known() {
        for does in [Does::Strays, Does::Doubles] {
            let model = Model::new(32, 64);
            let mut batch = model.batch();
            model.script([does]);
            assert_eq!(model.read(&mut batch, &reads(2)), Err(Spoiled::Stray));
            assert!(batch.spoiled);
            // Every read counts as in flight, and no answer is believed:
            // the answers cannot be told apart.
            assert_eq!(batch.in_flight, 2);
            for index in 0..2 {
                assert!(matches!(batch.outcome(index), Outcome::Unknown), "{does:?}");
            }
            model.free(&mut batch);
        }
    }

    #[test]
    fn a_batch_that_is_refused_hands_nothing_to_the_system() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        let reads = reads(4);
        model.read(&mut batch, &reads).unwrap();
        assert_eq!(batch.refuse(2, Spoiled::Foreign), Spoiled::Foreign);
        assert!(batch.spoiled);
        assert_eq!((batch.in_flight, batch.calls), (0, 0));
        // The answers of the batch before are gone.
        assert!(matches!(batch.outcome(0), Outcome::NotMade));
        assert!(matches!(batch.outcome(1), Outcome::NotMade));
        assert!(matches!(batch.outcome(2), Outcome::Unknown));
        assert_eq!(model.calls().len(), 1);
    }

    #[test]
    fn a_batch_with_no_memory_for_its_reads_makes_none() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        crate::linux::aligned::fail_the_next_buffer();
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::NoMemory));
        assert!(batch.spoiled);
        assert_eq!(batch.in_flight, 0);
        assert!(matches!(batch.outcome(2), Outcome::NotMade));
        assert!(model.calls().is_empty());
        assert!(batch.queues.are_empty());

        // The same where the batch has memory for some of its reads from
        // a batch before.
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.read(&mut batch, &reads(2)).unwrap();
        crate::linux::aligned::fail_the_next_buffer();
        assert_eq!(model.read(&mut batch, &reads(3)), Err(Spoiled::NoMemory));
        assert_eq!((batch.in_flight, batch.bounce.len()), (0, 2));
        for index in 0..3 {
            assert!(matches!(batch.outcome(index), Outcome::NotMade));
        }
        assert_eq!(model.calls().len(), 1);
        assert!(batch.queues.are_empty());
    }

    #[test]
    #[should_panic(expected = "are_empty")]
    fn a_batch_does_not_begin_on_queues_that_hold_something() {
        let model = Model::new(32, 64);
        let mut batch = model.batch();
        model.leave_an_answer();
        let _ = batch.read(&reads(2), |_, _| unreachable!("no call is made"));
    }

    #[test]
    fn queues_that_do_not_fit_their_memory_are_refused() {
        let model = Model::new(8, 16);
        let (rings, entries, fitting) = model.blocks();
        let made = |params: &io_uring_params, rings: Region, entries: Region| {
            // SAFETY: the blocks are the model's, which lives to the end
            // of the test. Nothing else uses them: no batch is made of
            // this model, and the queues that are made here are dropped
            // at once.
            unsafe { Queues::over(rings, entries, params) }.is_some()
        };
        assert!(made(&fitting, rings, entries));

        let short = |region: Region, by: usize| Region {
            start: region.start,
            len: region.len - by,
        };
        assert!(!made(&fitting, short(rings, 1), entries));
        assert!(!made(&fitting, rings, short(entries, 1)));

        let changes: [fn(&mut io_uring_params); 8] = [
            |params| params.sq_entries = 6,
            |params| params.cq_entries = 0,
            |params| params.sq_entries = 16,
            |params| params.cq_entries = 32,
            |params| params.sq_off.head = u32::MAX - 1,
            |params| params.sq_off.tail = 2,
            |params| params.cq_off.cqes += 4,
            |params| params.cq_off.ring_mask = params.cq_off.head,
        ];
        for change in changes {
            let mut params = fitting;
            change(&mut params);
            assert!(!made(&params, rings, entries), "{params:?}");
        }

        // A place is in a region only whole, and only where its type can
        // stand.
        assert!(rings.at::<u32>(0, 1).is_some());
        assert!(rings.at::<u32>(rings.len as u32 - 4, 1).is_some());
        assert!(rings.at::<u32>(rings.len as u32 - 3, 1).is_none());
        assert!(rings.at::<u32>(rings.len as u32, 1).is_none());
        assert!(rings.at::<u32>(u32::MAX, 1).is_none());
        assert!(rings.at::<u64>(0, u32::MAX).is_none());
        assert!(rings.at::<u32>(2, 1).is_none());
        assert!(rings.at::<u8>(rings.len as u32, 0).is_some());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no io_uring")]
    fn a_ring_is_made_on_one_thread_and_used_and_dropped_on_another() {
        use std::{fs::File, io::Write, os::fd::AsRawFd, thread};

        fn goes_to_another_thread<T: Send>(_: &T) {}
        if !system_gives_rings() {
            return;
        }
        let mut ring = Ring::new().expect("a ring");
        goes_to_another_thread(&ring);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        File::create(&path).unwrap().write_all(&[7; 8192]).unwrap();
        let file = File::open(&path).unwrap();
        let read = Read {
            fd: file.as_raw_fd(),
            offset: 4096,
            len: 4096,
        };
        thread::spawn(move || {
            ring.read(&[read, read]).unwrap();
            let Outcome::Answered(Ok(bytes)) = ring.outcome(1) else {
                panic!("the read was answered");
            };
            assert_eq!(bytes, [7; 4096]);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no io_uring")]
    fn a_ring_of_the_system_reads_files() {
        use std::{fs::File, io::Write, os::fd::AsRawFd};

        if !system_gives_rings() {
            return;
        }
        let mut ring = Ring::new().expect("a ring");
        assert!(format!("{ring:?}").starts_with("Ring"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let content: Vec<u8> = (0..200_000u32).map(|at| (at % 251) as u8).collect();
        File::create(&path).unwrap().write_all(&content).unwrap();
        let file = File::open(&path).unwrap();
        let reads: Vec<Read> = (0..DEPTH as u64)
            .map(|index| Read {
                fd: file.as_raw_fd(),
                offset: 4096 * index,
                len: 4096 * (1 + index as u32 % 4),
            })
            .collect();
        let read = |ring: &Ring, index: usize| match ring.outcome(index) {
            Outcome::Answered(Ok(bytes)) => bytes.to_vec(),
            other => panic!("read {index}: {other:?}"),
        };
        for _ in 0..3 {
            ring.read(&reads).unwrap();
            assert!(ring.calls() >= 1);
            for (index, asked) in reads.iter().enumerate() {
                let from = asked.offset as usize;
                assert_eq!(
                    read(&ring, index),
                    &content[from..from + asked.len as usize]
                );
            }
        }
        // Past the end of the file, and a descriptor that is none.
        let past = [
            Read {
                fd: file.as_raw_fd(),
                offset: 196_608,
                len: 8192,
            },
            Read {
                fd: -1,
                offset: 0,
                len: 4096,
            },
        ];
        ring.read(&past).unwrap();
        assert_eq!(read(&ring, 0), &content[196_608..]);
        let Outcome::Answered(Err(error)) = ring.outcome(1) else {
            panic!("the read failed");
        };
        assert_eq!(error.raw_os_error(), Some(Errno::BADF.raw_os_error()));
        assert!(ring.is_usable());
    }

    #[test]
    #[cfg_attr(miri, ignore = "Miri has no io_uring")]
    fn a_ring_whose_mark_is_wiped_is_given_up_and_handed_nothing() {
        if !system_gives_rings() {
            return;
        }
        let mut ring = Ring::foreign().expect("a ring");
        let read = Read {
            fd: -1,
            offset: 0,
            len: 4096,
        };
        assert_eq!(ring.read(&[read, read]), Err(Spoiled::Foreign));
        assert!(!ring.is_usable());
        // No entry was written, no memory was handed out, and no read was
        // made.
        assert!(matches!(ring.outcome(0), Outcome::NotMade));
        assert!(matches!(ring.outcome(1), Outcome::NotMade));
        assert!(ring.batch.queues.are_empty());
        assert_eq!((ring.batch.in_flight, ring.calls()), (0, 0));
        assert!(ring.batch.bounce.is_empty());
    }
}
