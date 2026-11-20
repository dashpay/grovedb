//! Frames: the memory of the pool, and the header that says what each piece
//! of it holds.
//!
//! This file holds all the unsafe code of the pool. The rules that make it
//! sound are stated here once, as the **access rules**, and every unsafe
//! block names the rule it relies on. The rest of the pool is safe code
//! over the types of this file, each of which stands for one way of being
//! allowed to reach a frame: a [`ChainRead`] or a [`ChainWrite`] (the
//! bucket's lock is held), a [`Pinned`] (a pin is held) and an
//! [`OwnedFrame`] (the frame is nobody else's). A [`Frozen`] is a pin on
//! content that can no longer change, and a [`Shared`] or an [`Exclusive`]
//! is a pin with the frame's latch.
//!
//! # Access rules
//!
//! A frame's *content* is its bytes and its attached values. Its header is
//! made of atomics and of fields that are set when the frame is made and
//! never again.
//!
//! - **A1. A frame is reached through its bucket's lock, through a pin, or
//!   by its owner.** A pointer to a frame is followed only by a thread that
//!   holds the lock of the bucket the frame is linked in, that holds a pin
//!   on the frame, or that owns the frame outright (A3). One thing more is
//!   allowed, of the header alone: **the header of an arena frame may be
//!   read by anyone.** It lives as long as the arena and never moves, so a
//!   thread with no lock and no pin can follow a bucket's arena frames from
//!   one header to the next. What such a thread reads decides nothing until
//!   it holds a pin (A6). A bucket keeps its arena frames and its frames
//!   beside the arena in two lists, so that this walk never comes to a
//!   frame that can be freed.
//! - **A2. A frame that is linked or has a pin is not freed and not given
//!   to anyone else.** An arena frame's memory lives as long as the arena.
//!   An overflow frame is deallocated only by its owner (A3).
//! - **A3. A frame is owned outright by exactly one thread** when it is
//!   unlinked and has no pin: it was taken off the free list or newly
//!   allocated; it was unlinked under its bucket's lock while it had no
//!   pin; it is an overflow frame and its last pin went after it was
//!   retired; or it is an arena frame that was retired, and was found
//!   without a pin under the lock of the arena's list of dead frames. An
//!   [`OwnedFrame`] is that ownership.
//! - **A4. Content is written only with exclusive access**: by the owner
//!   (A3), or under the exclusive latch of a private frame.
//! - **A5. Content is read only while it cannot be written**: under a pin on
//!   a frame that is published or dead, whose content does not change again
//!   until the frame is owned outright; or under a latch of a private
//!   frame; or by the owner.
//! - **A6. A pin is a count on the frame or an announcement of it.** The
//!   count is in the frame's header. An announcement is the frame's number
//!   in a cell of the announcer's own lane (`lanes.rs`), and only an arena
//!   frame is announced.
//!   - The count leaves zero only under the bucket's lock while the frame
//!     is linked, or by the holder of an announcement of the frame.
//!   - An announcement is a pin once the announcer has found, after it
//!     announced, that the bucket was not changed since before it found the
//!     frame linked there ([`Chain::pin_unlocked`]). Until then it is
//!     nothing, and the announcer takes it back.
//!   - Whoever needs a frame to have no pin looks for announcements first
//!     and at the count after them. A second pin is a count taken under the
//!     first, never an announcement, so no pin moves to where the look has
//!     already been.
//!
//! **Why an announcement holds.** Every change of a bucket is made under
//! its lock, between two steps of the bucket's count of changes
//! ([`ChainWrite`]), and a frame is taken out of a bucket only in such a
//! change. The announcer reads the count, finds the frame, announces, and
//! reads the count again; the one who takes a frame out steps the count
//! and then looks for announcements. All four are `SeqCst`. If the
//! announcer read the same count twice, its second read was before the
//! step, so its announcement was too, and the look finds it. If the look
//! misses an announcement, the step was before the announcer's second
//! read, which then differs from the first.
//!
//! **Why what the announcer found is so.** What a lookup reads of a linked
//! frame (its page, its versions, what it is, the next frame) is written
//! by the frame's owner before it is linked, or in a change of the bucket.
//! The changes of a bucket follow one another under its lock, and each
//! ends with a step of the count. So the announcer's first read of the
//! count comes after everything that was done to the bucket's frames in
//! the changes up to that step, and what it reads afterwards is at least
//! that new. With the same count read again, there was nothing newer: the
//! announcer walked the bucket's arena frames as they were, and the frame
//! it picked was linked, a page version, and the one it asked for.
//!
//! Three things outside the unsafe blocks carry these rules, and they are
//! kept in this file and in `lanes.rs`:
//!
//! - **What a frame is** (`kind`) says whether it can be written under a
//!   latch. It changes for a frame that is owned ([`OwnedFrame::set_kind`]),
//!   from private to published under the exclusive latch
//!   ([`Exclusive::publish`]), and to dead when a chain retires a frame. A
//!   published frame never becomes private again while it has a pin.
//! - **Which arena a frame belongs to** (`home`). A chain links only frames
//!   of its own arena ([`ChainWrite::link`]), and an arena frees only its
//!   own ([`Arena::free_overflow`]), so a frame never outlives the arena
//!   whose borrow its pins carry.
//! - **The count of changes of a bucket**, which only [`ChainWrite`] steps,
//!   and the lanes, which find every announcement made before they are
//!   looked through.

// The atomics that are plain in every build: a model does not follow them.
use std::sync::atomic as plain;
use std::{
    alloc::{self, Layout},
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ptr::{self, NonNull},
    sync::Arc,
};

use lignin_format::value_log::PendingValue;

use super::{
    error::CacheError,
    lanes::{Announcement, Lanes},
    sync::{
        self, settled_u64, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Mutex, Ordering, Padded,
        Reading, RwLock, RwLockReadGuard, RwLockWriteGuard, Tracked, Writing,
    },
};

/// The raw value of "no version" in a header field.
pub(super) const NO_VERSION: u64 = u64::MAX;

/// What a frame is to the pool.
pub(super) mod kind {
    /// On the free list.
    pub(in super::super) const FREE: u8 = 0;
    /// Owned outright by a call that is filling it.
    pub(in super::super) const BLANK: u8 = 1;
    /// A page version. Its content does not change again.
    pub(in super::super) const PUBLISHED: u8 = 2;
    /// A private copy of an open write transaction.
    pub(in super::super) const PRIVATE: u8 = 3;
    /// Unlinked, and waiting for its last pin to go.
    pub(in super::super) const DEAD: u8 = 4;
}

/// Why a published frame cannot be evicted, beside its pins.
pub(super) mod flag {
    /// No segment holds this content, and write-back still owes it.
    pub(in super::super) const UNWRITTEN: u8 = 1;
    /// No segment holds this content and none ever will: it is kept for the
    /// readers that can still ask for it.
    pub(in super::super) const HELD: u8 = 2;
    /// The frame has attached values.
    pub(in super::super) const ATTACHED: u8 = 4;
}

const RETIRED: u64 = 1 << 32;
const PINS: u64 = RETIRED - 1;

/// What names a value attached to a frame: the eight bytes of the pending
/// value whose bytes they are, which its cell or entry in the page holds
/// (`log-format.md` LG21). The pool compares them and gives them no meaning.
pub type ValueId = [u8; PendingValue::ID_LEN];

/// A value attached to a frame: bytes that belong to the page and do not fit
/// in it.
#[derive(Clone)]
pub(super) struct Attached {
    pub(super) id: ValueId,
    pub(super) bytes: Arc<[u8]>,
}

/// The number of an arena frame is below this, so that it fits a cell of a
/// lane with one added.
const NO_INDEX: u32 = u32::MAX;

/// One frame: a header, and a pointer to `len` bytes.
///
/// The header is two cache lines. The first holds what a lookup reads. A
/// reader of the page writes it once after each pass of the clock, to mark
/// the frame as used, and not otherwise. The second holds what changes
/// while the frame is in use: its count of pins, its latch, its attached
/// values.
#[repr(C, align(64))]
pub(super) struct Frame {
    page: AtomicU32,
    kind: AtomicU8,
    flags: AtomicU8,
    referenced: plain::AtomicBool,
    born: AtomicU64,
    superseded: AtomicU64,
    next: AtomicPtr<Frame>,
    bytes: NonNull<u8>,
    len: usize,
    /// The arena the frame belongs to.
    home: u64,
    /// The frame's number in its arena, or [`NO_INDEX`] for a frame beside
    /// it.
    index: u32,
    /// The pin count in the low 32 bits, and the retired mark above them.
    state: AtomicU64,
    base: AtomicU64,
    owner: AtomicU64,
    latch: RwLock<()>,
    access: Tracked,
    attached: UnsafeCell<Vec<Attached>>,
}

// SAFETY: a frame is shared between threads by design. Its header is made of
// atomics and a lock. Its content, behind `bytes` and `attached`, is reached
// only through the unsafe methods below, whose callers keep rules A4 and A5,
// so no thread writes content that another can read or write.
//
// No `Send` is declared for a frame: none is moved by value to another
// thread. A frame stays at its address and threads share it. The arena
// that owns it says for itself why it may change threads.
unsafe impl Sync for Frame {}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("kind", &self.kind())
            .field("page", &self.page_raw())
            .field("born", &self.born())
            .field("superseded", &self.superseded())
            .field("pins", &self.pins())
            .field("overflow", &self.is_overflow())
            .finish_non_exhaustive()
    }
}

impl Frame {
    fn new(bytes: NonNull<u8>, len: usize, home: u64, index: u32) -> Self {
        Self {
            page: AtomicU32::new(0),
            kind: AtomicU8::new(kind::FREE),
            flags: AtomicU8::new(0),
            referenced: plain::AtomicBool::new(false),
            born: AtomicU64::new(NO_VERSION),
            superseded: AtomicU64::new(NO_VERSION),
            next: AtomicPtr::new(ptr::null_mut()),
            bytes,
            len,
            home,
            index,
            state: AtomicU64::new(0),
            base: AtomicU64::new(NO_VERSION),
            owner: AtomicU64::new(0),
            latch: RwLock::new(()),
            access: Tracked::new(),
            attached: UnsafeCell::new(Vec::new()),
        }
    }

    pub(super) fn kind(&self) -> u8 {
        self.kind.load(Ordering::Acquire)
    }

    /// What a frame is decides who may read and write its content (A4,
    /// A5), so only this file changes it: for a frame that is owned, under
    /// the exclusive latch of a private copy, or when a chain retires a
    /// frame.
    ///
    /// The store is `SeqCst` for the one who retires a frame, against the
    /// one who takes an announcement back ([`Self::is_dead_after`]).
    fn set_kind(&self, kind: u8) {
        self.kind.store(kind, Ordering::SeqCst);
    }

    /// Takes back an announcement of this frame, and says whether the
    /// frame is dead, in that order: whoever retires a frame marks it dead
    /// and then looks for announcements, so one of the two sees the other,
    /// and a dead frame whose last pin this was is not left unnoticed.
    fn is_dead_after(&self, announcement: Announcement<'_>) -> bool {
        announcement.withdraw();
        self.kind.load(Ordering::SeqCst) == kind::DEAD
    }

    pub(super) fn flags(&self) -> u8 {
        self.flags.load(Ordering::Acquire)
    }

    pub(super) fn set_flags(&self, flags: u8) {
        self.flags.store(flags, Ordering::Release);
    }

    pub(super) fn has(&self, flag: u8) -> bool {
        self.flags() & flag != 0
    }

    /// Whether the frame was allocated beside the arena.
    pub(super) fn is_overflow(&self) -> bool {
        self.index == NO_INDEX
    }

    /// The page id the frame holds, or 0 for none.
    pub(super) fn page_raw(&self) -> u32 {
        self.page.load(Ordering::Acquire)
    }

    pub(super) fn set_page_raw(&self, page: u32) {
        self.page.store(page, Ordering::Release);
    }

    pub(super) fn born(&self) -> u64 {
        self.born.load(Ordering::Acquire)
    }

    pub(super) fn set_born(&self, born: u64) {
        self.born.store(born, Ordering::Release);
    }

    pub(super) fn superseded(&self) -> u64 {
        self.superseded.load(Ordering::Acquire)
    }

    pub(super) fn set_superseded(&self, superseded: u64) {
        self.superseded.store(superseded, Ordering::Release);
    }

    pub(super) fn base(&self) -> u64 {
        self.base.load(Ordering::Acquire)
    }

    pub(super) fn set_base(&self, base: u64) {
        self.base.store(base, Ordering::Release);
    }

    pub(super) fn owner(&self) -> u64 {
        self.owner.load(Ordering::Acquire)
    }

    pub(super) fn set_owner(&self, owner: u64) {
        self.owner.store(owner, Ordering::Release);
    }

    fn next(&self) -> Option<FramePtr> {
        pointer_of(self.next.load(Ordering::Acquire))
    }

    fn set_next(&self, next: Option<FramePtr>) {
        self.next.store(raw_of(next), Ordering::Release);
    }

    /// Marks the frame as used, for the clock.
    pub(super) fn touch(&self) {
        if !self.referenced.load(Ordering::Relaxed) {
            self.referenced.store(true, Ordering::Relaxed);
        }
    }

    /// Takes the clock's mark off, and says whether it was set. A mark
    /// that is not set is not written.
    pub(super) fn take_referenced(&self) -> bool {
        self.referenced.load(Ordering::Relaxed) && self.referenced.swap(false, Ordering::Relaxed)
    }

    fn pin(&self) {
        let before = self.state.fetch_add(1, Ordering::AcqRel);
        assert!(before & PINS < PINS, "a frame has too many pins");
    }

    /// Drops a pin. `true` says the frame was retired and this was its last
    /// pin.
    fn unpin(&self) -> bool {
        Self::unpin_count(&self.state)
    }

    /// Drops a pin from a frame's count. `true` says the frame was retired
    /// and this was its last pin.
    ///
    /// For a frame beside the arena this is given the count and not the
    /// frame, as [`Self::retire_count`] is. From the moment the count says
    /// that the frame has an owner, that owner can free it, on another
    /// thread, before this call has returned. A call may outlive what a
    /// reference to an atomic points to, as the last `drop` of an `Arc`
    /// does. It may not outlive what a reference to a frame points to.
    fn unpin_count(state: &AtomicU64) -> bool {
        let before = state.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(before & PINS > 0, "a frame was unpinned without a pin");
        before == RETIRED | 1
    }

    /// Marks the frame as retired. `true` says it has no pin.
    fn retire(&self) -> bool {
        Self::retire_count(&self.state)
    }

    /// Marks a frame as retired in its count. `true` says it has no pin.
    /// If it has one, whoever gives back the last owns the frame from then
    /// on ([`Self::unpin_count`]).
    fn retire_count(state: &AtomicU64) -> bool {
        let before = state.fetch_or(RETIRED, Ordering::AcqRel);
        debug_assert!(before & RETIRED == 0, "a frame was retired twice");
        before & PINS == 0
    }

    /// The frame's count of pins. An announcement is not in it (A6).
    pub(super) fn pins(&self) -> u32 {
        (self.state.load(Ordering::Acquire) & PINS) as u32
    }

    /// Whether the frame has no counted pin and is not retired.
    pub(super) fn is_unpinned(&self) -> bool {
        self.state.load(Ordering::Acquire) == 0
    }

    fn latch(&self) -> &RwLock<()> {
        &self.latch
    }

    /// A read access for a model to check (A5).
    fn reading(&self) -> Reading {
        self.access.reading()
    }

    /// The write access for a model to check (A4).
    pub(super) fn writing(&self) -> Writing {
        self.access.writing()
    }

    /// The frame's bytes.
    ///
    /// # Safety
    ///
    /// The caller may read the content (A5), and no exclusive access to it
    /// begins while the slice is in use.
    unsafe fn bytes(&self) -> &[u8] {
        // SAFETY: `bytes` points to `len` initialised bytes that live as
        // long as the frame (the arena's block, or the overflow frame's own
        // allocation). The caller keeps A5, so nothing writes them.
        unsafe { std::slice::from_raw_parts(self.bytes.as_ptr(), self.len) }
    }

    /// The frame's bytes, to change.
    ///
    /// # Safety
    ///
    /// The caller has exclusive access to the content (A4) for as long as
    /// the slice is in use, and makes no second slice of it.
    #[allow(clippy::mut_from_ref)]
    unsafe fn bytes_mut(&self) -> &mut [u8] {
        // SAFETY: as in `bytes`, and the caller keeps A4, so this is the
        // only reference to the bytes.
        unsafe { std::slice::from_raw_parts_mut(self.bytes.as_ptr(), self.len) }
    }

    /// The values attached to the frame.
    ///
    /// # Safety
    ///
    /// As [`Self::bytes`].
    unsafe fn attached(&self) -> &[Attached] {
        // SAFETY: the caller keeps A5, so nothing writes the list.
        unsafe { &*self.attached.get() }
    }

    /// The values attached to the frame, to change.
    ///
    /// # Safety
    ///
    /// As [`Self::bytes_mut`].
    #[allow(clippy::mut_from_ref)]
    unsafe fn attached_mut(&self) -> &mut Vec<Attached> {
        // SAFETY: the caller keeps A4, so this is the only reference.
        unsafe { &mut *self.attached.get() }
    }
}

/// A pointer to a frame. It is only an address: following it takes one of
/// the three ways of A1.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct FramePtr(NonNull<Frame>);

// SAFETY: the pointer is only an address. Every use of what it points to
// goes through `FramePtr::frame`, whose caller keeps A1.
unsafe impl Send for FramePtr {}
// SAFETY: as above.
unsafe impl Sync for FramePtr {}

impl fmt::Debug for FramePtr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FramePtr({:p})", self.0)
    }
}

impl FramePtr {
    /// The frame.
    ///
    /// # Safety
    ///
    /// The caller may reach the frame (A1), and the reference is not used
    /// after that stops being so.
    unsafe fn frame<'a>(self) -> &'a Frame {
        // SAFETY: the pointer came from the arena's headers or from
        // `Arena::allocate_overflow`, and by A1 and A2 the frame is still
        // there.
        unsafe { self.0.as_ref() }
    }
}

/// A frame that one thread owns outright (A3): it is linked nowhere and has
/// no pin, so nobody else can reach it.
///
/// It is not dropped: it is linked into a chain, or handed back to the pool,
/// which puts it on the free list or deallocates it.
#[must_use]
#[derive(Debug)]
pub(super) struct OwnedFrame(FramePtr);

impl OwnedFrame {
    pub(super) fn header(&self) -> &Frame {
        // SAFETY: the owner may reach the frame (A1).
        unsafe { self.0.frame() }
    }

    /// Says what the frame is to be once it is linked. Nobody else can
    /// reach it yet, so nobody reads or writes its content by the old kind.
    pub(super) fn set_kind(&self, kind: u8) {
        self.header().set_kind(kind);
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: the owner may read the content (A5), and the slice
        // borrows the ownership, so nothing writes while it is in use.
        unsafe { self.header().bytes() }
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the owner has exclusive access (A4), and the slice
        // borrows the ownership mutably, so it is the only one.
        unsafe { self.header().bytes_mut() }
    }

    pub(super) fn attached(&self) -> &[Attached] {
        // SAFETY: as in `bytes`.
        unsafe { self.header().attached() }
    }

    pub(super) fn attached_mut(&mut self) -> &mut Vec<Attached> {
        // SAFETY: as in `bytes_mut`.
        unsafe { self.header().attached_mut() }
    }

    /// Makes the frame free again: no page, no mark, no attached value. The
    /// bytes are left as they are. Returns the bytes the attached values
    /// took.
    pub(super) fn reset(&mut self) -> usize {
        let frame = self.header();
        let _writing = frame.writing();
        // SAFETY: the owner has exclusive access (A4), and `self` is
        // borrowed mutably.
        let attached = unsafe { frame.attached_mut() };
        let bytes = attached.iter().map(|value| value.bytes.len()).sum();
        attached.clear();
        frame.state.store(0, Ordering::Release);
        frame.set_kind(kind::FREE);
        frame.set_flags(0);
        frame.referenced.store(false, Ordering::Relaxed);
        frame.set_page_raw(0);
        frame.set_born(NO_VERSION);
        frame.set_superseded(NO_VERSION);
        frame.set_base(NO_VERSION);
        frame.set_owner(0);
        frame.set_next(None);
        bytes
    }
}

/// One pin on a frame, taken from an arena that lives for `'a`: a count on
/// the frame, or an announcement of it (A6). While it lives, the frame is
/// neither freed nor given to anyone else (A2).
///
/// It is not dropped: [`Pinned::unpin`] or [`ChainWrite::unpin_linked`]
/// gives the pin back.
#[must_use]
#[derive(Debug)]
pub(super) struct Pinned<'a> {
    frame: FramePtr,
    /// The cell that announces the frame, for a pin that is not counted.
    announced: Option<Announcement<'a>>,
    arena: PhantomData<&'a Arena>,
}

impl<'a> Pinned<'a> {
    pub(super) fn frame(&self) -> &Frame {
        // SAFETY: the holder of a pin may reach the frame (A1), and the
        // frame is there for as long as the pin and the arena are (A2).
        unsafe { self.frame.frame() }
    }

    pub(super) fn pointer(&self) -> FramePtr {
        self.frame
    }

    /// Whether the pin is an announcement, which only an arena frame has.
    pub(super) fn is_announced(&self) -> bool {
        self.announced.is_some()
    }

    /// A second pin on the same frame. It is a count, whatever the first
    /// is (A6).
    pub(super) fn again(&self) -> Pinned<'a> {
        self.frame().pin();
        Pinned {
            frame: self.frame,
            announced: None,
            arena: PhantomData,
        }
    }

    /// Gives back a pin on an arena frame. `true` says that the frame is
    /// dead and may have lost its last pin: the caller then takes what the
    /// arena's dead frames give ([`Arena::take_dead`]).
    ///
    /// A pin on a frame beside the arena is given back under the frame's
    /// bucket's lock instead ([`ChainWrite::unpin_linked`]), so that the
    /// frame can be taken out in the same step.
    pub(super) fn unpin(self) -> bool {
        // SAFETY: the pin is held (A1) until it is given back below, and
        // from then on only the header of an arena frame is read, which
        // anyone may read (A1): an announced frame is an arena frame (A6),
        // and a counted pin on any other frame is refused first.
        let frame = unsafe { self.frame.frame() };
        match self.announced {
            Some(announcement) => frame.is_dead_after(announcement),
            None => {
                assert!(
                    !frame.is_overflow(),
                    "a frame beside the arena is unpinned under its bucket's lock"
                );
                frame.unpin()
            }
        }
    }

    /// The pin as one on frozen content, if the frame is a page version
    /// now. Otherwise the pin, unchanged.
    pub(super) fn into_frozen(self) -> Result<Frozen<'a>, Pinned<'a>> {
        if self.frame().kind() != kind::PUBLISHED {
            return Err(self);
        }
        let reading = self.frame().reading();
        Ok(Frozen {
            _reading: reading,
            pin: self,
        })
    }

    /// Takes the frame's latch for reading, and waits for it.
    pub(super) fn read(&self) -> Shared<'_> {
        Shared::new(self.frame(), sync::read(self.frame().latch()))
    }

    /// Takes the frame's latch for reading if no writer has it.
    pub(super) fn try_read(&self) -> Option<Shared<'_>> {
        sync::try_read(self.frame().latch()).map(|latch| Shared::new(self.frame(), latch))
    }

    /// Takes the frame's latch for writing, and waits for it. `None` if the
    /// frame is not a private copy any more.
    pub(super) fn write(&self) -> Option<Exclusive<'_>> {
        Exclusive::new(self.frame(), sync::write(self.frame().latch()))
    }

    /// Takes the frame's latch for writing if nobody has it: `None` if
    /// somebody does, `Some(None)` if the frame is not a private copy any
    /// more.
    pub(super) fn try_write(&self) -> Option<Option<Exclusive<'_>>> {
        sync::try_write(self.frame().latch()).map(|latch| Exclusive::new(self.frame(), latch))
    }
}

/// A pin on a frame whose content is frozen: the frame was a page version
/// when the pin was turned into this, and the content of a page version does
/// not change again while the frame has a pin (A5).
#[must_use]
#[derive(Debug)]
pub(super) struct Frozen<'a> {
    _reading: Reading,
    pin: Pinned<'a>,
}

impl<'a> Frozen<'a> {
    pub(super) fn frame(&self) -> &Frame {
        self.pin.frame()
    }

    pub(super) fn pointer(&self) -> FramePtr {
        self.pin.pointer()
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: A5. The frame was published when this was made, the pin
        // has been held since, and a published frame's content is written
        // again only by an owner, which a pinned frame does not have.
        unsafe { self.frame().bytes() }
    }

    pub(super) fn attached(&self) -> &[Attached] {
        // SAFETY: as in `bytes`.
        unsafe { self.frame().attached() }
    }

    /// A second pin on the same frozen content.
    pub(super) fn again(&self) -> Frozen<'a> {
        Frozen {
            _reading: self.frame().reading(),
            pin: self.pin.again(),
        }
    }

    /// Ends the read access, and leaves the pin to be given back.
    pub(super) fn into_pin(self) -> Pinned<'a> {
        self.pin
    }
}

/// The shared latch of a pinned frame: its content can be read and cannot be
/// written while this lives.
pub(super) struct Shared<'l> {
    reading: Reading,
    frame: &'l Frame,
    latch: RwLockReadGuard<'l, ()>,
}

impl<'l> Shared<'l> {
    fn new(frame: &'l Frame, latch: RwLockReadGuard<'l, ()>) -> Self {
        Self {
            reading: frame.reading(),
            frame,
            latch,
        }
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: A5. The frame is pinned, so it has no owner. If it is a
        // private copy, writing it takes the exclusive latch, which this
        // shared hold keeps out. If it is not, nothing writes it at all.
        unsafe { self.frame.bytes() }
    }

    pub(super) fn attached(&self) -> &[Attached] {
        // SAFETY: as in `bytes`.
        unsafe { self.frame.attached() }
    }
}

impl fmt::Debug for Shared<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("frame", self.frame)
            .field("reading", &self.reading)
            .field("latch", &self.latch)
            .finish()
    }
}

/// The exclusive latch of a pinned private frame: its content can be read
/// and written by the holder alone while this lives.
pub(super) struct Exclusive<'l> {
    writing: Writing,
    frame: &'l Frame,
    latch: RwLockWriteGuard<'l, ()>,
}

impl<'l> Exclusive<'l> {
    /// `None` if the frame is not a private copy. Whoever makes a private
    /// copy something else holds this latch while it does, so a frame that
    /// is private here stays private while the latch is held.
    fn new(frame: &'l Frame, latch: RwLockWriteGuard<'l, ()>) -> Option<Self> {
        (frame.kind() == kind::PRIVATE).then(|| Self {
            writing: frame.writing(),
            frame,
            latch,
        })
    }

    pub(super) fn frame(&self) -> &'l Frame {
        self.frame
    }

    /// Makes the private copy a page version and ends the write access.
    /// From here on no exclusive latch is given out for the frame, so its
    /// content does not change again while it has a pin. The latch itself
    /// stays taken until the returned guard is dropped, so that whoever
    /// publishes can finish its step first.
    pub(super) fn publish(self) -> Sealed<'l> {
        self.frame.set_kind(kind::PUBLISHED);
        Sealed { _latch: self.latch }
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: A5. The exclusive latch keeps every other access out.
        unsafe { self.frame.bytes() }
    }

    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: A4. The frame is a private copy under its exclusive
        // latch, and the slice borrows that latch mutably.
        unsafe { self.frame.bytes_mut() }
    }

    pub(super) fn attached(&self) -> &[Attached] {
        // SAFETY: as in `bytes`.
        unsafe { self.frame.attached() }
    }

    pub(super) fn attached_mut(&mut self) -> &mut Vec<Attached> {
        // SAFETY: as in `bytes_mut`.
        unsafe { self.frame.attached_mut() }
    }
}

/// The latch of a frame that was just published, with no access to its
/// content: it only keeps other holders of the latch out.
pub(super) struct Sealed<'l> {
    _latch: RwLockWriteGuard<'l, ()>,
}

impl fmt::Debug for Exclusive<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exclusive")
            .field("frame", self.frame)
            .field("writing", &self.writing)
            .field("latch", &self.latch)
            .finish()
    }
}

/// The frames of one bucket, and the lock they are reached under.
///
/// Whoever holds the lock may follow the pointers the bucket holds (A1): a
/// [`ChainRead`] to look, a [`ChainWrite`] to change. A reader with no lock
/// pins a page version of the bucket's arena frames with
/// [`Chain::pin_unlocked`].
pub(super) struct Chain {
    lock: RwLock<()>,
    links: Links,
}

impl fmt::Debug for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chain").finish_non_exhaustive()
    }
}

/// What a bucket links. Outside this file it is reached only through a
/// [`ChainRead`] or a [`ChainWrite`], so under the bucket's lock.
pub(super) struct Links {
    /// Two steps for every change of the bucket: one when its lock is
    /// taken for writing, one before it is given back. It is odd while a
    /// change is under way.
    changes: AtomicU64,
    /// The first of the bucket's arena frames, each of which leads to the
    /// next. These pointers are to arena frames and to nothing else (A1).
    first: AtomicPtr<Frame>,
    /// The first of the bucket's frames beside the arena. It is read and
    /// written under the lock only.
    beside: plain::AtomicPtr<Frame>,
    /// How many times a commit has installed or freed a page of this
    /// bucket, or a page step has replaced a frame of it. It is read and
    /// written under the lock only.
    stamp: plain::AtomicU64,
}

/// A frame of a chain, for as long as the chain's lock is held.
#[derive(Clone, Copy)]
pub(super) struct Entry<'c> {
    pointer: FramePtr,
    frame: &'c Frame,
}

impl<'c> Entry<'c> {
    pub(super) fn frame(&self) -> &'c Frame {
        self.frame
    }

    pub(super) fn pointer(&self) -> FramePtr {
        self.pointer
    }

    /// Pins the frame, by its count. The chain's lock is held and the frame
    /// is linked, which is one of the two ways a count leaves zero (A6).
    pub(super) fn pin<'a>(&self, arena: &'a Arena) -> Pinned<'a> {
        assert_eq!(self.frame.home, arena.id, "a frame of another pool");
        self.frame.pin();
        Pinned {
            frame: self.pointer,
            announced: None,
            arena: PhantomData,
        }
    }

    /// Pins the frame as frozen content, if it is a page version.
    pub(super) fn freeze<'a>(&self, arena: &'a Arena) -> Option<Frozen<'a>> {
        if self.frame.kind() != kind::PUBLISHED {
            return None;
        }
        let pin = self.pin(arena);
        Some(Frozen {
            _reading: self.frame.reading(),
            pin,
        })
    }
}

pub(super) struct Entries<'c> {
    cursor: Option<FramePtr>,
    /// The frames beside the arena, which come after the arena frames.
    beside: Option<FramePtr>,
    chain: PhantomData<&'c Links>,
}

impl<'c> Iterator for Entries<'c> {
    type Item = Entry<'c>;

    fn next(&mut self) -> Option<Self::Item> {
        let pointer = match self.cursor {
            Some(pointer) => pointer,
            None => self.beside.take()?,
        };
        // SAFETY: the pointer is in a chain that is borrowed for `'c` under
        // its lock, or by the one who alone can reach it
        // (`Chain::take_all`), so the frame is linked (A1), and a linked
        // frame is not freed (A2).
        let frame: &'c Frame = unsafe { pointer.frame() };
        self.cursor = frame.next();
        Some(Entry { pointer, frame })
    }
}

fn pointer_of(raw: *mut Frame) -> Option<FramePtr> {
    NonNull::new(raw).map(FramePtr)
}

fn raw_of(pointer: Option<FramePtr>) -> *mut Frame {
    pointer.map_or(ptr::null_mut(), |pointer| pointer.0.as_ptr())
}

impl Links {
    fn new() -> Self {
        Self {
            changes: settled_u64(0),
            first: AtomicPtr::new(ptr::null_mut()),
            beside: plain::AtomicPtr::new(ptr::null_mut()),
            stamp: plain::AtomicU64::new(0),
        }
    }

    fn first(&self) -> Option<FramePtr> {
        pointer_of(self.first.load(Ordering::Acquire))
    }

    fn beside(&self) -> Option<FramePtr> {
        pointer_of(self.beside.load(plain::Ordering::Relaxed))
    }

    /// The frames of the bucket: its arena frames, and then those beside
    /// the arena.
    pub(super) fn iter(&self) -> Entries<'_> {
        Entries {
            cursor: self.first(),
            beside: self.beside(),
            chain: PhantomData,
        }
    }

    /// How many times a commit has installed or freed a page of this
    /// bucket, or a page step has replaced a frame of it.
    pub(super) fn stamp(&self) -> u64 {
        self.stamp.load(plain::Ordering::Relaxed)
    }

    /// Takes a frame out of the list it is linked in. The caller holds the
    /// bucket's lock for writing, or is the only one who can reach the
    /// bucket.
    fn unlink(&self, pointer: FramePtr) -> bool {
        let in_arena = self.unlink_from(self.first(), pointer, |first| {
            self.first.store(raw_of(first), Ordering::Release);
        });
        in_arena
            || self.unlink_from(self.beside(), pointer, |beside| {
                self.beside.store(raw_of(beside), plain::Ordering::Relaxed);
            })
    }

    fn unlink_from(
        &self,
        head: Option<FramePtr>,
        pointer: FramePtr,
        set_head: impl FnOnce(Option<FramePtr>),
    ) -> bool {
        let mut before: Option<FramePtr> = None;
        let mut cursor = head;
        while let Some(current) = cursor {
            // SAFETY: `current` is linked in this chain, and the caller
            // holds its lock for writing (A1, A2).
            let frame = unsafe { current.frame() };
            let after = frame.next();
            if current == pointer {
                match before {
                    // SAFETY: as above, `before` is linked in this chain.
                    Some(before) => unsafe { before.frame() }.set_next(after),
                    None => set_head(after),
                }
                frame.set_next(None);
                return true;
            }
            before = Some(current);
            cursor = after;
        }
        false
    }
}

/// The shared lock of a bucket: its frames can be looked at, and none comes
/// or goes.
pub(super) struct ChainRead<'c> {
    links: &'c Links,
    _lock: RwLockReadGuard<'c, ()>,
}

impl std::ops::Deref for ChainRead<'_> {
    type Target = Links;

    fn deref(&self) -> &Links {
        self.links
    }
}

/// The exclusive lock of a bucket, and one change of it: the bucket's count
/// of changes is odd for as long as this lives, and whoever pins without
/// the lock sees that the bucket was changed (A6).
pub(super) struct ChainWrite<'c> {
    links: &'c Links,
    _lock: RwLockWriteGuard<'c, ()>,
}

impl std::ops::Deref for ChainWrite<'_> {
    type Target = Links;

    fn deref(&self) -> &Links {
        self.links
    }
}

impl Drop for ChainWrite<'_> {
    fn drop(&mut self) {
        // The lock is given back after this, when the fields are dropped.
        self.links.changes.fetch_add(1, Ordering::SeqCst);
    }
}

/// A moment in a read that takes no lock ([`Chain::pin_unlocked`]), at
/// which a test changes the pool under the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Moment {
    /// The reader has picked a frame and has not announced it.
    Picked,
    /// The reader has announced the frame and has not looked at the count
    /// of changes again.
    Announced,
}

/// What became of a pin that was asked for with no lock
/// ([`Chain::pin_unlocked`]).
#[derive(Debug)]
pub(super) enum Unlocked<'a> {
    /// The page version, pinned by an announcement.
    Found(Frozen<'a>),
    /// Nothing was pinned: the caller asks under the bucket's lock.
    Not,
    /// As [`Self::Not`], and an announcement was taken back from a frame
    /// that is dead: the caller takes what the arena's dead frames give
    /// ([`Arena::take_dead`]).
    Dead,
}

impl Chain {
    pub(super) fn new() -> Self {
        Self {
            lock: RwLock::new(()),
            links: Links::new(),
        }
    }

    /// Takes the bucket's lock to look.
    pub(super) fn read(&self) -> ChainRead<'_> {
        ChainRead {
            _lock: sync::read(&self.lock),
            links: &self.links,
        }
    }

    /// Takes the bucket's lock to change the bucket, and counts the change.
    pub(super) fn write(&self) -> ChainWrite<'_> {
        let lock = sync::write(&self.lock);
        self.links.changes.fetch_add(1, Ordering::SeqCst);
        ChainWrite {
            links: &self.links,
            _lock: lock,
        }
    }

    /// Pins, with no lock and by an announcement, the page version among
    /// the bucket's arena frames that `wanted` picks.
    ///
    /// It gives up, and the caller then asks under the lock, when there is
    /// no such frame, when the bucket is being changed or was changed
    /// while this looked, and when the calling thread's lane has no cell
    /// left. `arena` is the arena of the pool the bucket belongs to.
    ///
    /// `at` is told when the frame is picked and when it is announced.
    /// Nothing is done there but by a test, which changes the pool under
    /// the reader at one of those moments.
    pub(super) fn pin_unlocked<'a>(
        &self,
        arena: &'a Arena,
        wanted: impl Fn(&Frame) -> bool,
        mut at: impl FnMut(Moment),
    ) -> Unlocked<'a> {
        let links = &self.links;
        let before = links.changes.load(Ordering::SeqCst);
        if before % 2 == 1 {
            return Unlocked::Not;
        }
        // A chain has no more frames than the arena. More steps than that
        // are a walk that other threads keep leading on.
        let mut steps = arena.len();
        let mut cursor = links.first.load(Ordering::Acquire);
        let (pointer, frame) = loop {
            let Some(pointer) = NonNull::new(cursor) else {
                return Unlocked::Not;
            };
            assert!(
                arena.holds(pointer),
                "a chain of arena frames left the arena"
            );
            // SAFETY: the pointer came from `first` or from the `next` of a
            // frame, which hold pointers to the starts of headers, and it
            // lies among the headers of `arena`, which was just checked.
            // The header of an arena frame may be read by anyone for as
            // long as the arena lives (A1). That the walk comes to no
            // other frame is what the two lists of a bucket are for:
            // `first`, and `next` of an arena frame, point to arena frames
            // only.
            let frame: &'a Frame = unsafe { pointer.as_ref() };
            if frame.kind() == kind::PUBLISHED && wanted(frame) {
                break (FramePtr(pointer), frame);
            }
            if steps == 0 {
                return Unlocked::Not;
            }
            steps -= 1;
            cursor = frame.next.load(Ordering::Acquire);
        };
        at(Moment::Picked);
        let Some(announcement) = arena.lanes.announce(frame.index) else {
            return Unlocked::Not;
        };
        at(Moment::Announced);
        if links.changes.load(Ordering::SeqCst) != before {
            return if frame.is_dead_after(announcement) {
                Unlocked::Dead
            } else {
                Unlocked::Not
            };
        }
        // The bucket was not changed between the two reads of its count.
        // So all the while the frame was linked in it, a page version, and
        // as `wanted` saw it, and it was so after the announcement was
        // made: the announcement is a pin (A6), on content that does not
        // change again while the frame has a pin (A5).
        Unlocked::Found(Frozen {
            _reading: frame.reading(),
            pin: Pinned {
                frame: pointer,
                announced: Some(announcement),
                arena: PhantomData,
            },
        })
    }

    /// Empties the chain of a pool that is going away. The arena is borrowed
    /// mutably, so no pin on any of its frames is left, and the caller owns
    /// every frame that was linked. A chain that holds a frame of another
    /// arena is refused: a pin on that frame borrows the other arena.
    pub(super) fn take_all(&mut self, arena: &mut Arena) -> Vec<OwnedFrame> {
        let links = &self.links;
        let mut frames = Vec::new();
        for entry in links.iter() {
            assert_eq!(entry.frame.home, arena.id, "a frame of another pool");
        }
        while let Some(pointer) = links.first().or_else(|| links.beside()) {
            links.unlink(pointer);
            // By A3: unlinked, and a frame of `arena`, every pin on which
            // borrows the arena that is borrowed mutably here.
            frames.push(OwnedFrame(pointer));
        }
        frames
    }
}

impl ChainWrite<'_> {
    /// Stamps the bucket for a commit that installed or freed a page of
    /// it, or a page step that replaced a frame of it: a load that began
    /// before is turned back.
    pub(super) fn advance_stamp(&self) {
        self.links.stamp.fetch_add(1, plain::Ordering::Relaxed);
    }

    /// Links a frame its caller owned. From here on other threads can reach
    /// it.
    pub(super) fn link(&mut self, arena: &Arena, frame: OwnedFrame) -> Entry<'_> {
        assert!(arena.owns(&frame), "a frame of another pool");
        let pointer = frame.0;
        let header = frame.header();
        if header.is_overflow() {
            header.set_next(self.links.beside());
            self.links
                .beside
                .store(raw_of(Some(pointer)), plain::Ordering::Relaxed);
        } else {
            header.set_next(self.links.first());
            self.links
                .first
                .store(raw_of(Some(pointer)), Ordering::Release);
        }
        Entry {
            pointer,
            // SAFETY: the frame is linked in this chain now, and the chain
            // stays borrowed while the entry lives (A1).
            frame: unsafe { pointer.frame() },
        }
    }

    /// Takes a frame out of the chain if it is linked here, has no pin and
    /// passes `check`. The caller then owns it.
    pub(super) fn take_unpinned(
        &mut self,
        arena: &Arena,
        pointer: FramePtr,
        check: impl FnOnce(&Frame) -> bool,
    ) -> Option<OwnedFrame> {
        let entry = self.iter().find(|entry| entry.pointer == pointer)?;
        if !entry.frame.is_unpinned() || !check(entry.frame) {
            return None;
        }
        // Announcements first, and the count once more after them (A6).
        if arena.announces(entry.frame) || !entry.frame.is_unpinned() {
            return None;
        }
        self.links.unlink(pointer);
        // By A3: the frame is unlinked. It had no announcement that is a
        // pin, because this change of the bucket was counted before the
        // lanes were looked through, and no count after that, under the
        // lock without which a count leaves zero only by an announcement.
        Some(OwnedFrame(pointer))
    }

    /// Takes a frame out of the chain whatever pins it has, and marks it
    /// dead. If it has no pin the caller owns it. Otherwise an overflow
    /// frame is owned by whoever gives back its last pin, and an arena
    /// frame waits among the arena's dead frames ([`Arena::take_dead`]).
    pub(super) fn retire(&mut self, arena: &Arena, pointer: FramePtr) -> Option<OwnedFrame> {
        if !self.links.unlink(pointer) {
            return None;
        }
        // SAFETY: the frame was linked in this chain until the line above,
        // under the chain's lock, so it was not freed (A2). It is not
        // retired yet, so no other thread can come to own it before it is
        // marked retired below. That step is taken on the count alone, and
        // the reference to an overflow frame is not used after it.
        let frame = unsafe { pointer.frame() };
        frame.set_kind(kind::DEAD);
        if frame.is_overflow() {
            // By A3: the frame is unlinked, it is never announced, and the
            // count had no pin.
            return Frame::retire_count(&frame.state).then_some(OwnedFrame(pointer));
        }
        arena.bury(pointer)
    }

    /// Takes out of the chain a frame that the caller holds a pin on, and
    /// marks it dead.
    pub(super) fn retire_pinned(&mut self, arena: &Arena, pointer: FramePtr) {
        let owned = self.retire(arena, pointer);
        assert!(owned.is_none(), "the caller holds a pin on the frame");
    }

    /// Gives back a counted pin on an overflow frame, under the lock of the
    /// bucket the frame's page is in. If that leaves the frame with no pin
    /// and `idle` holds for it, the frame is taken out of the chain in the
    /// same step, and the caller owns it. A retired frame whose last pin
    /// this was is owned as well.
    ///
    /// Under the lock of another bucket the frame is not found here and
    /// stays where it is linked, with no pin. That breaks the pool's bound
    /// on memory and nothing else: the pin is given back on the count
    /// alone, and nothing of the frame is read after it.
    pub(super) fn unpin_linked(
        &mut self,
        arena: &Arena,
        pin: Pinned<'_>,
        idle: impl FnOnce(&Frame) -> bool,
    ) -> Option<OwnedFrame> {
        assert!(
            pin.frame().is_overflow() && !pin.is_announced(),
            "an arena frame is unpinned with no lock"
        );
        assert_eq!(pin.frame().home, arena.id, "a frame of another pool");
        let pointer = pin.frame;
        if Frame::unpin_count(&pin.frame().state) {
            // By A3: retired, which unlinked it, and this was the last pin
            // of a frame that is never announced.
            return Some(OwnedFrame(pointer));
        }
        self.take_unpinned(arena, pointer, idle)
    }
}

/// The layout of `count` frames of `frame_len` bytes side by side.
///
/// A block of frames is asked for with an alignment of 4,096 bytes,
/// whatever the size of a store's pages, each of which is a multiple of
/// that. So every frame of a store's pages starts at a multiple of 4,096
/// bytes, and that is all that is promised of where it starts. A frame
/// length that is no multiple of 4,096, which only tests have, is aligned
/// to 64 bytes.
fn layout(frame_len: usize, count: usize) -> Result<Layout, CacheError> {
    let align = if frame_len.is_multiple_of(4096) {
        4096
    } else {
        64
    };
    frame_len
        .checked_mul(count)
        .and_then(|size| Layout::from_size_align(size, align).ok())
        .ok_or(CacheError::CapacityTooLarge)
}

fn allocate(layout: Layout) -> NonNull<u8> {
    assert!(layout.size() > 0, "a frame has at least one byte");
    // SAFETY: the layout's size is above zero.
    let memory = unsafe { alloc::alloc_zeroed(layout) };
    NonNull::new(memory).unwrap_or_else(|| alloc::handle_alloc_error(layout))
}

/// A block from the allocator, asked for zeroed, which this value gives back
/// when it is dropped.
struct Allocated {
    start: NonNull<u8>,
    layout: Layout,
}

impl Allocated {
    fn new(layout: Layout) -> Self {
        Self {
            start: allocate(layout),
            layout,
        }
    }
}

impl Drop for Allocated {
    fn drop(&mut self) {
        // SAFETY: `start` was allocated with `layout` in `new`, and only
        // this value frees it, once. Whoever holds this value keeps no
        // pointer into the block in use beyond its drop (the arena drops
        // its headers first).
        unsafe { alloc::dealloc(self.start.as_ptr(), self.layout) };
    }
}

use mapping::Mapped;

/// Memory that is a mapping of its own, on the systems whose call for one
/// this file declares: Linux on x86-64 and on AArch64, and macOS, each with
/// 64-bit pointers.
///
/// The declarations of `mmap` and `munmap` and the numbers of their flags
/// are those of these systems' C libraries, written out here because the
/// crate depends on no binding to them. No other system is guessed at: on
/// any other the type below has no value.
#[cfg(any(
    all(
        target_pointer_width = "64",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_pointer_width = "64", target_os = "macos"),
))]
mod mapping {
    use std::{
        alloc::Layout,
        ffi::{c_int, c_void},
        ptr::{self, NonNull},
    };

    /// A mapping of memory that is no file's, which this value made and
    /// asks the system to unmap when it is dropped. The system gives such
    /// memory zeroed, so nothing is written to make it.
    pub(super) struct Mapped {
        start: NonNull<u8>,
        len: usize,
    }

    impl Mapped {
        /// A mapping of `layout.size()` bytes for reading and writing, or
        /// `None` if the system refuses one.
        ///
        /// A mapping starts at a multiple of the system's page size, which
        /// is at least 4,096 bytes on these systems and so at least the
        /// alignment of any block of frames (`layout`).
        pub(super) fn new(layout: Layout) -> Option<Self> {
            const PROT_READ: c_int = 1;
            const PROT_WRITE: c_int = 2;
            const MAP_PRIVATE: c_int = 2;
            const MAP_ANONYMOUS: c_int = if cfg!(target_os = "macos") {
                0x1000
            } else {
                0x20
            };
            // SAFETY: `mmap` as POSIX has it and as the C library of each
            // of these systems declares it. The module is compiled for
            // 64-bit pointers only, where `size_t` is `usize` and `off_t`
            // is `i64` on each of them.
            unsafe extern "C" {
                fn mmap(
                    addr: *mut c_void,
                    len: usize,
                    prot: c_int,
                    flags: c_int,
                    fd: c_int,
                    offset: i64,
                ) -> *mut c_void;
            }
            assert!(layout.size() > 0, "a frame has at least one byte");
            // SAFETY: a new mapping of no file, private to the process, at
            // an address the system chooses. It takes the place of no
            // memory that anything has, and the call reads and writes none.
            let start = unsafe {
                mmap(
                    ptr::null_mut(),
                    layout.size(),
                    PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            // The call's answer for a refusal is the address of all ones
            // (`MAP_FAILED`).
            if start.addr() == usize::MAX {
                return None;
            }
            let start = NonNull::new(start.cast::<u8>())
                .expect("the system chooses the address, and never the address 0");
            let mapped = Self {
                start,
                len: layout.size(),
            };
            assert!(
                start.addr().get().is_multiple_of(layout.align()),
                "a mapping starts at a multiple of the system's page size"
            );
            Some(mapped)
        }

        /// The first byte of the mapping. A pointer made from it is good
        /// for `len` bytes until this value is dropped.
        pub(super) fn start(&self) -> NonNull<u8> {
            self.start
        }

        pub(super) fn len(&self) -> usize {
            self.len
        }
    }

    impl Drop for Mapped {
        fn drop(&mut self) {
            // SAFETY: `munmap` as POSIX has it and as the C library of each
            // of these systems declares it.
            unsafe extern "C" {
                fn munmap(addr: *mut c_void, len: usize) -> c_int;
            }
            // SAFETY: `start` and `len` are the whole of the mapping that
            // `new` made, which only this value unmaps, and it does so
            // once. Whoever holds this value keeps no pointer into the
            // mapping in use beyond its drop (the arena drops its headers
            // first).
            let refused = unsafe { munmap(self.start.as_ptr().cast(), self.len) };
            // The system can refuse: Linux does when the range is the
            // middle of one of its own mappings, which it has joined with
            // a neighbour, and the process is at its limit of mappings.
            // The mapping then stays the process's until the process ends,
            // which loses memory and breaks no rule. A build with debug
            // assertions stops on a refusal that the system reports.
            debug_assert_eq!(refused, 0, "the system unmaps a mapping");
        }
    }
}

/// On a system whose call for a mapping this file does not declare, no
/// memory is mapped: the type has no value.
#[cfg(not(any(
    all(
        target_pointer_width = "64",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_pointer_width = "64", target_os = "macos"),
)))]
mod mapping {
    use std::{alloc::Layout, ptr::NonNull};

    pub(super) enum Mapped {}

    impl Mapped {
        pub(super) fn new(_: Layout) -> Option<Self> {
            None
        }

        pub(super) fn start(&self) -> NonNull<u8> {
            match *self {}
        }

        pub(super) fn len(&self) -> usize {
            match *self {}
        }
    }
}

/// The block of an arena's frames, owned. When this value is dropped the
/// block goes back the way it was come by: to the allocator, or, a mapping,
/// to the system, which is asked to unmap it.
enum Block {
    /// From the allocator, which was asked for it zeroed.
    Allocated(Allocated),
    /// A mapping of the arena's own, which the system gives zeroed.
    Mapped(Mapped),
}

impl Block {
    /// The first byte of the block. A pointer made from it is good for the
    /// block's length until this value is dropped.
    fn start(&self) -> NonNull<u8> {
        match self {
            Block::Allocated(allocated) => allocated.start,
            Block::Mapped(mapped) => mapped.start(),
        }
    }
}

/// The fixed part of the pool: `count` frames of `frame_len` bytes in one
/// block of memory, with their headers.
///
/// The headers are a boxed slice that the arena holds as a raw pointer, so
/// that every pointer to a header comes from that one pointer and stays good
/// for as long as the arena lives.
///
/// The block is zeroed either way it is come by, because a frame's bytes
/// are handed out as a slice before anything is written to them. A block
/// of [`Arena::MAPPED_FROM`] bytes or more is a mapping of its own where
/// the system has one for this file ([`Mapped`]): the system gives it
/// zeroed, so the arena does not write its frames to make them. A smaller
/// block, and every block on another system or where the system refuses a
/// mapping, is the allocator's, which is asked for it zeroed.
pub(super) struct Arena {
    /// A number no other arena of the process has: its frames carry it.
    id: u64,
    frames: NonNull<Frame>,
    count: usize,
    /// The block of the frames. The arena's drop frees the headers, which
    /// point into it, and this value is dropped after that, with the
    /// arena's fields.
    block: Block,
    frame_len: usize,
    /// Where the pins that are not counted are announced (A6).
    lanes: Lanes,
    /// The arena frames that were retired while they had a pin, linked
    /// through their headers. A frame leaves this list for its owner when
    /// it is found here without a pin (A3).
    dead: Padded<Mutex<Option<FramePtr>>>,
}

// SAFETY: the arena owns its headers and its block, and nothing else frees
// them. An arena that goes to another thread takes them along at the
// addresses they have, and that thread may use and drop them: a header
// holds atomics, a lock, attached values (shared buffers that any thread
// may drop) and a pointer into the block, and none of these is bound to
// the thread that made it. Neither is the block: the allocator takes a
// block back on any thread, and a mapping is the process's and is unmapped
// on any. The lanes and the list of dead frames are atomics and a lock
// around a pointer that is only an address.
unsafe impl Send for Arena {}
// SAFETY: what the arena holds itself is never changed, is atomic or is
// behind a lock, and its headers are shared as frames are (`Sync for
// Frame`): their content under rules A4 and A5.
unsafe impl Sync for Arena {}

impl fmt::Debug for Arena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arena")
            .field("frames", &self.count)
            .field("frame_len", &self.frame_len)
            .finish()
    }
}

impl Arena {
    /// The bytes one frame costs beside its page: its header.
    pub(super) const HEADER_LEN: usize = size_of::<Frame>();

    /// The bytes of frames from which an arena's block is a mapping of its
    /// own, where the system has one for this file. A smaller block is the
    /// allocator's, which is asked for it zeroed.
    pub(super) const MAPPED_FROM: usize = 1 << 20;

    /// The arena, and each of its frames, owned.
    pub(super) fn new(
        count: usize,
        frame_len: usize,
    ) -> Result<(Self, Vec<OwnedFrame>), CacheError> {
        Self::make(count, frame_len, true)
    }

    /// As [`Self::new`]. Without `may_map` the system is not asked for a
    /// mapping, and the arena does what it does when the system refuses
    /// one: a test's way to that.
    fn make(
        count: usize,
        frame_len: usize,
        may_map: bool,
    ) -> Result<(Self, Vec<OwnedFrame>), CacheError> {
        assert!(count > 0 && frame_len > 0);
        static ARENAS: plain::AtomicU64 = plain::AtomicU64::new(1);
        let id = ARENAS.fetch_add(1, plain::Ordering::Relaxed);
        let layout = layout(frame_len, count)?;
        let last = u32::try_from(count - 1).map_err(|_| CacheError::CapacityTooLarge)?;
        if last >= NO_INDEX - 1 {
            return Err(CacheError::CapacityTooLarge);
        }
        // Each of these owns what it holds, and the headers stay a box
        // until all of them are made: a panic on the way frees what was
        // made before it.
        let lanes = Lanes::new(Lanes::count_for(count));
        let dead = Padded(Mutex::new(None));
        let mapped = (may_map && layout.size() >= Self::MAPPED_FROM)
            .then(|| Mapped::new(layout))
            .flatten();
        let block = match mapped {
            Some(mapped) => Block::Mapped(mapped),
            None => Block::Allocated(Allocated::new(layout)),
        };
        let start = block.start();
        let headers: Box<[Frame]> = (0..=last)
            .map(|index| {
                // SAFETY: `start` is the start of a block of `layout.size()`
                // bytes, from the allocator or a mapping, and
                // `index * frame_len` is below that size, so the result is
                // inside the block.
                let bytes = unsafe { start.add(index as usize * frame_len) };
                Frame::new(bytes, frame_len, id, index)
            })
            .collect();
        let frames =
            NonNull::new(Box::into_raw(headers).cast::<Frame>()).expect("a box is not null");
        let arena = Self {
            id,
            frames,
            count,
            block,
            frame_len,
            lanes,
            dead,
        };
        // By A3: nothing has seen these frames yet.
        let owned = (0..count)
            .rev()
            .map(|index| OwnedFrame(arena.pointer(index)))
            .collect();
        Ok((arena, owned))
    }

    pub(super) fn len(&self) -> usize {
        self.count
    }

    /// Whether the frame is one of this arena's: a fixed one, or one that
    /// was allocated beside them. A frame of another arena is never linked
    /// into this pool's table, put on its free list or freed by it.
    pub(super) fn owns(&self, frame: &OwnedFrame) -> bool {
        frame.header().home == self.id
    }

    pub(super) fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// The bytes of the arena's block that are a mapping of its own: all of
    /// its frames, or none.
    pub(super) fn mapped(&self) -> usize {
        match &self.block {
            Block::Allocated(_) => 0,
            Block::Mapped(mapped) => mapped.len(),
        }
    }

    /// The lanes of the pool: its announcements, and its threads' counts.
    pub(super) fn lanes(&self) -> &Lanes {
        &self.lanes
    }

    /// Whether `pointer` lies among the arena's own headers. A pointer that
    /// a chain or a frame keeps is to the start of a header, so one that
    /// lies among these is to one of the arena's frames.
    fn holds(&self, pointer: NonNull<Frame>) -> bool {
        let first = self.frames.as_ptr().addr();
        pointer.as_ptr().addr().wrapping_sub(first) < self.count * size_of::<Frame>()
    }

    /// Whether a cell of a lane announces `frame` at the moment this looks
    /// at it. A frame beside the arena is never announced (A6). A frame of
    /// another arena is refused: it is announced in that arena's lanes,
    /// where this would not look.
    fn announces(&self, frame: &Frame) -> bool {
        assert_eq!(frame.home, self.id, "a frame of another pool");
        !frame.is_overflow() && self.lanes.announces(frame.index)
    }

    /// Takes an arena frame that was just retired: the caller owns it if it
    /// has no pin, and otherwise it waits among the dead frames.
    fn bury(&self, pointer: FramePtr) -> Option<OwnedFrame> {
        // SAFETY: the header of an arena frame may be read by anyone (A1).
        let frame = unsafe { pointer.frame() };
        let mut dead = sync::lock(&self.dead);
        frame.retire();
        if self.has_no_pin(frame) {
            // By A3: the frame was unlinked in a change of its bucket,
            // which was counted before this look, and it is on no list.
            return Some(OwnedFrame(pointer));
        }
        frame.set_next(*dead);
        *dead = Some(pointer);
        None
    }

    /// Whether a frame has no pin: no announcement, and after that no count
    /// (A6). For a frame that can still be found in its bucket, the answer
    /// is of one moment and decides nothing.
    pub(super) fn has_no_pin(&self, frame: &Frame) -> bool {
        !self.announces(frame) && frame.pins() == 0
    }

    /// One of the dead frames that has no pin any more, taken off their
    /// list: the caller owns it. `None` if every dead frame has a pin.
    ///
    /// It is asked whenever a pin on a dead frame was given back
    /// ([`Pinned::unpin`], [`Unlocked::Dead`]), until it gives nothing.
    pub(super) fn take_dead(&self) -> Option<OwnedFrame> {
        let mut dead = sync::lock(&self.dead);
        let mut before: Option<FramePtr> = None;
        let mut cursor = *dead;
        while let Some(current) = cursor {
            // SAFETY: the list holds arena frames, whose headers may be
            // read by anyone (A1).
            let frame = unsafe { current.frame() };
            let after = frame.next();
            if self.has_no_pin(frame) {
                match before {
                    // SAFETY: as above.
                    Some(before) => unsafe { before.frame() }.set_next(after),
                    None => *dead = after,
                }
                frame.set_next(None);
                // By A3: the frame was retired, which unlinked it, it is
                // off the list of dead frames under that list's lock, and
                // it has no pin. No pin is taken on it from here on: it is
                // in no bucket, and a second pin needs a first.
                return Some(OwnedFrame(current));
            }
            before = Some(current);
            cursor = after;
        }
        None
    }

    /// The header of an arena frame, which lives as long as the arena.
    pub(super) fn frame(&self, index: usize) -> &Frame {
        // SAFETY: the header is in the arena's slice, which lives as long
        // as the arena, and a header is only ever shared (A2).
        unsafe { self.pointer(index).0.as_ref() }
    }

    pub(super) fn pointer(&self, index: usize) -> FramePtr {
        assert!(index < self.count, "no such frame");
        // SAFETY: `index` is below the length of the slice `frames` points
        // to, so the result is inside it.
        FramePtr(unsafe { self.frames.add(index) })
    }

    /// A frame beside the arena.
    pub(super) fn allocate_overflow(&self) -> OwnedFrame {
        let layout = layout(self.frame_len, 1).expect("the arena has this layout many times");
        let bytes = allocate(layout);
        let frame = Box::new(Frame::new(bytes, self.frame_len, self.id, NO_INDEX));
        let frame = NonNull::new(Box::into_raw(frame)).expect("a box is not null");
        // By A3: nothing has seen this frame yet.
        OwnedFrame(FramePtr(frame))
    }

    /// Gives the memory of an overflow frame back.
    pub(super) fn free_overflow(&self, frame: OwnedFrame) {
        assert!(self.owns(&frame), "a frame of another pool");
        assert!(frame.header().is_overflow(), "an arena frame is not freed");
        let layout = layout(self.frame_len, 1).expect("the arena has this layout many times");
        // SAFETY: an overflow frame came out of a box in
        // `allocate_overflow`. Its owner is the only one who can reach it
        // (A3), and gives it up here.
        let frame = unsafe { Box::from_raw(frame.0 .0.as_ptr()) };
        // SAFETY: `bytes` was allocated with this layout by
        // `allocate_overflow`, and the frame that pointed to it is dropped
        // at the end of this call.
        unsafe { alloc::dealloc(frame.bytes.as_ptr(), layout) };
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        let headers = ptr::slice_from_raw_parts_mut(self.frames.as_ptr(), self.count);
        // SAFETY: `frames` and `count` are the boxed slice that `new` turned
        // into a raw pointer, and the arena is its only owner. Nothing can
        // follow a pointer to a header any more: a pin borrows the arena.
        drop(unsafe { Box::from_raw(headers) });
        // The block's value is dropped with the arena's fields, which is
        // after this and so after the headers.
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    /// A pool that goes away takes every frame out of its table, the ones
    /// beside the arena too, and owns them.
    #[test]
    fn a_chain_gives_its_frames_up_to_the_arena_they_are_of() {
        let (mut arena, mut free) = Arena::new(2, 64).expect("a small arena");
        let mut chain = Chain::new();
        for frame in [free.pop().expect("a frame"), arena.allocate_overflow()] {
            frame.set_kind(kind::PUBLISHED);
            chain.write().link(&arena, frame);
        }
        assert_eq!(chain.read().iter().count(), 2);
        let taken = chain.take_all(&mut arena);
        assert_eq!(taken.len(), 2);
        assert_eq!(chain.read().iter().count(), 0);
        for frame in taken {
            assert!(arena.owns(&frame));
            if frame.header().is_overflow() {
                arena.free_overflow(frame);
            }
        }
    }

    /// What a refused call panicked with.
    fn refusal<T>(call: impl FnOnce() -> T) -> String {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
        let payload = caught.err().expect("the call is refused");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
            .expect("a panic with a message")
    }

    /// An arena that gets no mapping for a block that could be one, as
    /// when the system refuses, asks the allocator for the block, and its
    /// frames are zeroed and aligned as those of any arena.
    #[test]
    fn an_arena_that_gets_no_mapping_takes_its_block_from_the_allocator() {
        let (count, frame_len) = (Arena::MAPPED_FROM / 8_192, 8_192);
        let (arena, frames) = Arena::make(count, frame_len, false).expect("an arena");
        assert_eq!(arena.mapped(), 0);
        assert_eq!(frames.len(), count);
        for frame in [&frames[0], &frames[count - 1]] {
            assert_eq!(frame.bytes().len(), frame_len);
            assert!(frame.bytes().iter().all(|&byte| byte == 0));
            assert!(frame.bytes().as_ptr().addr().is_multiple_of(4096));
        }
        drop(frames);
    }

    /// A mapping the system cannot give is `None`: no address space holds a
    /// block of this size. Miri has no address space of its own to refuse
    /// from, and stops when it is asked for so much.
    #[cfg_attr(miri, ignore = "Miri stops on a mapping that is too large")]
    #[test]
    fn a_mapping_the_system_refuses_is_none() {
        let most = (isize::MAX as usize - 4095) & !4095;
        let layout = Layout::from_size_align(most, 4096).expect("a layout");
        assert!(Mapped::new(layout).is_none());
    }

    /// The frames of a chain are owned by whoever empties it because their
    /// pins borrow the arena that is borrowed mutably. That says nothing of
    /// a frame of another arena, so such a chain is left as it is.
    #[test]
    fn a_chain_is_not_emptied_for_another_arena() {
        let (mut arena, mut free) = Arena::new(1, 64).expect("a small arena");
        let (mut other, _free) = Arena::new(1, 64).expect("a small arena");
        let mut chain = Chain::new();
        chain.write().link(&arena, free.pop().expect("a frame"));

        let refused = refusal(|| chain.take_all(&mut other).len());
        assert!(refused.contains("a frame of another pool"), "{refused}");
        assert_eq!(chain.read().iter().count(), 1, "nothing was unlinked");
        assert_eq!(chain.take_all(&mut arena).len(), 1);
    }

    /// A pin is looked for in the lanes of the frame's own arena. Asked of
    /// another arena, which would look in the wrong lanes and find the
    /// frame free to take, the answer is a refusal, and the frame stays.
    #[test]
    fn a_frame_is_not_taken_out_for_another_arena() {
        let (mut arena, mut free) = Arena::new(1, 64).expect("a small arena");
        let (other, _free) = Arena::new(1, 64).expect("a small arena");
        let mut chain = Chain::new();
        let frame = free.pop().expect("a frame");
        frame.set_kind(kind::PUBLISHED);
        let pointer = chain.write().link(&arena, frame).pointer();

        let refused = refusal(|| {
            let mut chain = chain.write();
            chain.take_unpinned(&other, pointer, |_| true).is_some()
        });
        assert!(refused.contains("a frame of another pool"), "{refused}");
        assert_eq!(chain.read().iter().count(), 1, "nothing was unlinked");
        let taken = chain.write().take_unpinned(&arena, pointer, |_| true);
        assert!(taken.is_some());
        assert!(chain.take_all(&mut arena).is_empty());
    }

    /// The last pin of a frame beside the arena goes while another thread
    /// takes the frame out, and whichever of the two comes to own the frame
    /// frees it at once, while the other may still be in the step that
    /// made it so. The pin is given back under a lock that is not the
    /// frame's bucket's, so no lock keeps the two apart: the frame is
    /// owned once and freed once all the same, and Miri finds nothing that
    /// still refers to it.
    #[test]
    fn a_frame_beside_the_arena_is_freed_once_whichever_lock_its_last_pin_goes_under() {
        let rounds = if cfg!(miri) { 4 } else { 2_000 };
        for _ in 0..rounds {
            let (arena, _free) = Arena::new(1, 64).expect("a small arena");
            let (home, elsewhere) = (Chain::new(), Chain::new());
            let frame = arena.allocate_overflow();
            frame.set_kind(kind::PUBLISHED);
            let (pointer, pin) = {
                let mut chain = home.write();
                let entry = chain.link(&arena, frame);
                (entry.pointer(), entry.pin(&arena))
            };

            let free =
                |owned: Option<OwnedFrame>| owned.map(|frame| arena.free_overflow(frame)).is_some();
            let freed = std::thread::scope(|scope| {
                let retiring = scope.spawn(|| free(home.write().retire(&arena, pointer)));
                let unpinning =
                    scope.spawn(|| free(elsewhere.write().unpin_linked(&arena, pin, |_| true)));
                [retiring, unpinning].map(|thread| thread.join().expect("no panic"))
            });
            assert_eq!(freed.iter().filter(|&&freed| freed).count(), 1, "{freed:?}");
            assert_eq!(home.read().iter().count(), 0);
        }
    }
}

#[cfg(all(test, not(loom), target_os = "linux", target_pointer_width = "64"))]
mod layout {
    use std::mem::offset_of;

    use super::Frame;

    /// What a lookup reads of a frame is on the first cache line of its
    /// header, and what a pin, a latch or an attached value writes is on the
    /// second: a reader of a page is not slowed by the pins others count on
    /// it.
    #[test]
    fn a_lookup_reads_the_line_of_a_header_that_pins_do_not_write() {
        assert_eq!((size_of::<Frame>(), align_of::<Frame>()), (128, 64));
        let read = [
            offset_of!(Frame, page),
            offset_of!(Frame, kind),
            offset_of!(Frame, flags),
            offset_of!(Frame, referenced),
            offset_of!(Frame, born),
            offset_of!(Frame, superseded),
            offset_of!(Frame, next),
            offset_of!(Frame, bytes),
            offset_of!(Frame, len),
            offset_of!(Frame, home),
            offset_of!(Frame, index),
        ];
        let written = [
            offset_of!(Frame, state),
            offset_of!(Frame, base),
            offset_of!(Frame, owner),
            offset_of!(Frame, latch),
            offset_of!(Frame, attached),
        ];
        assert!(read.iter().all(|&offset| offset < 64), "{read:?}");
        assert!(written.iter().all(|&offset| offset >= 64), "{written:?}");
    }
}
