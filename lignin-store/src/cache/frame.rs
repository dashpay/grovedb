//! Frames: the memory of the pool, and the header that says what each piece
//! of it holds.
//!
//! This file holds all the unsafe code of the pool. The rules that make it
//! sound are stated here once, as the **access rules**, and every unsafe
//! block names the rule it relies on. The rest of the pool is safe code
//! over three types of this file, each of which stands for one way of being
//! allowed to reach a frame: a [`Chain`] (the bucket's lock is held), a
//! [`Pinned`] (a pin is held) and an [`OwnedFrame`] (the frame is nobody
//! else's). A [`Frozen`] is a pin on content that can no longer change, and
//! a [`Shared`] or an [`Exclusive`] is a pin with the frame's latch.
//!
//! # Access rules
//!
//! A frame's *content* is its bytes and its attached values. Its header is
//! made of atomics and can be read by anyone who may reach the frame.
//!
//! - **A1. A frame is reached through its bucket's lock, through a pin, or
//!   by its owner.** A pointer to a frame is followed only by a thread that
//!   holds the lock of the bucket the frame is linked in, that holds a pin
//!   on the frame, or that owns the frame outright (A3). The pin count goes
//!   from zero to one only under that bucket's lock, while the frame is
//!   linked.
//! - **A2. A frame that is linked or has a pin is not freed and not given
//!   to anyone else.** An arena frame's memory lives as long as the arena.
//!   An overflow frame is deallocated only by its owner (A3).
//! - **A3. A frame is owned outright by exactly one thread** when it is
//!   unlinked and has no pin: it was taken off the free list or newly
//!   allocated, it was unlinked with no pin under its bucket's lock, or its
//!   last pin went after it was retired. An [`OwnedFrame`] is that
//!   ownership.
//! - **A4. Content is written only with exclusive access**: by the owner
//!   (A3), or under the exclusive latch of a private frame.
//! - **A5. Content is read only while it cannot be written**: under a pin on
//!   a frame that is published or dead, whose content does not change again
//!   until the frame is owned outright; or under a latch of a private
//!   frame; or by the owner.

use std::{
    alloc::{self, Layout},
    cell::UnsafeCell,
    fmt,
    marker::PhantomData,
    ptr::{self, NonNull},
    sync::Arc,
};

use super::{
    error::CacheError,
    sync::{
        self, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering, Reading, RwLock,
        RwLockReadGuard, RwLockWriteGuard, Tracked, Writing,
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

/// A value attached to a frame: bytes that belong to the page and do not fit
/// in it.
#[derive(Clone)]
pub(super) struct Attached {
    pub(super) tag: u32,
    pub(super) bytes: Arc<[u8]>,
}

/// One frame: a header, and a pointer to `len` bytes.
pub(super) struct Frame {
    /// The pin count in the low 32 bits, and the retired mark above them.
    state: AtomicU64,
    kind: AtomicU8,
    flags: AtomicU8,
    referenced: std::sync::atomic::AtomicBool,
    overflow: bool,
    page: AtomicU32,
    born: AtomicU64,
    superseded: AtomicU64,
    base: AtomicU64,
    owner: AtomicU64,
    next: AtomicPtr<Frame>,
    latch: RwLock<()>,
    access: Tracked,
    bytes: NonNull<u8>,
    len: usize,
    attached: UnsafeCell<Vec<Attached>>,
}

// SAFETY: a frame is shared between threads by design. Its header is made of
// atomics and a lock. Its content, behind `bytes` and `attached`, is reached
// only through the unsafe methods below, whose callers keep rules A4 and A5,
// so no thread writes content that another can read or write.
unsafe impl Send for Frame {}
// SAFETY: as above.
unsafe impl Sync for Frame {}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("kind", &self.kind())
            .field("page", &self.page_raw())
            .field("born", &self.born())
            .field("superseded", &self.superseded())
            .field("pins", &self.pins())
            .field("overflow", &self.overflow)
            .finish_non_exhaustive()
    }
}

impl Frame {
    fn new(bytes: NonNull<u8>, len: usize, overflow: bool) -> Self {
        Self {
            state: AtomicU64::new(0),
            kind: AtomicU8::new(kind::FREE),
            flags: AtomicU8::new(0),
            referenced: std::sync::atomic::AtomicBool::new(false),
            overflow,
            page: AtomicU32::new(0),
            born: AtomicU64::new(NO_VERSION),
            superseded: AtomicU64::new(NO_VERSION),
            base: AtomicU64::new(NO_VERSION),
            owner: AtomicU64::new(0),
            next: AtomicPtr::new(ptr::null_mut()),
            latch: RwLock::new(()),
            access: Tracked::new(),
            bytes,
            len,
            attached: UnsafeCell::new(Vec::new()),
        }
    }

    pub(super) fn kind(&self) -> u8 {
        self.kind.load(Ordering::Acquire)
    }

    pub(super) fn set_kind(&self, kind: u8) {
        self.kind.store(kind, Ordering::Release);
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
        self.overflow
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
        NonNull::new(self.next.load(Ordering::Acquire)).map(FramePtr)
    }

    fn set_next(&self, next: Option<FramePtr>) {
        let raw = next.map_or(ptr::null_mut(), |next| next.0.as_ptr());
        self.next.store(raw, Ordering::Release);
    }

    /// Marks the frame as used, for the clock.
    pub(super) fn touch(&self) {
        if !self.referenced.load(Ordering::Relaxed) {
            self.referenced.store(true, Ordering::Relaxed);
        }
    }

    /// Takes the clock's mark off, and says whether it was set.
    pub(super) fn take_referenced(&self) -> bool {
        self.referenced.swap(false, Ordering::Relaxed)
    }

    fn pin(&self) {
        let before = self.state.fetch_add(1, Ordering::AcqRel);
        assert!(before & PINS < PINS, "a frame has too many pins");
    }

    /// Drops a pin. `true` says the frame was retired and this was its last
    /// pin.
    fn unpin(&self) -> bool {
        let before = self.state.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(before & PINS > 0, "a frame was unpinned without a pin");
        before == RETIRED | 1
    }

    /// Marks the frame as retired. `true` says it has no pin.
    fn retire(&self) -> bool {
        let before = self.state.fetch_or(RETIRED, Ordering::AcqRel);
        debug_assert!(before & RETIRED == 0, "a frame was retired twice");
        before & PINS == 0
    }

    pub(super) fn pins(&self) -> u32 {
        (self.state.load(Ordering::Acquire) & PINS) as u32
    }

    /// Whether the frame has no pin and is not retired.
    pub(super) fn is_unpinned(&self) -> bool {
        self.state.load(Ordering::Acquire) == 0
    }

    pub(super) fn latch(&self) -> &RwLock<()> {
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

/// One pin on a frame, taken from an arena that lives for `'a`. While it
/// lives, the frame is neither freed nor given to anyone else (A2).
///
/// It is not dropped: [`Pinned::unpin`] or [`Chain::unpin_linked`] gives the
/// pin back.
#[must_use]
#[derive(Debug)]
pub(super) struct Pinned<'a> {
    frame: FramePtr,
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

    /// A second pin on the same frame.
    pub(super) fn again(&self) -> Pinned<'a> {
        self.frame().pin();
        Pinned {
            frame: self.frame,
            arena: PhantomData,
        }
    }

    /// Gives the pin back. If the frame was retired and this was its last
    /// pin, the caller now owns it.
    ///
    /// The caller does not use this for an overflow frame that is not
    /// retired: such a frame is unpinned under its bucket's lock
    /// ([`Chain::unpin_linked`]), so that it can be taken out in the same
    /// step.
    pub(super) fn unpin(self) -> Option<OwnedFrame> {
        // By A3 the frame is owned outright once it is retired, which
        // unlinked it, and its last pin is gone.
        self.frame().unpin().then_some(OwnedFrame(self.frame))
    }
}

impl<'a> Pinned<'a> {
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

impl fmt::Debug for Exclusive<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exclusive")
            .field("frame", self.frame)
            .field("writing", &self.writing)
            .field("latch", &self.latch)
            .finish()
    }
}

/// The frames of one bucket. It is only ever reached under the bucket's
/// lock, which is what lets it follow the pointers it holds (A1).
#[derive(Debug)]
pub(super) struct Chain {
    head: Option<FramePtr>,
    /// How many times a commit has installed or freed a page of this
    /// bucket, or a page step has replaced a frame of it.
    pub(super) stamp: u64,
}

/// A frame of a chain, for as long as the chain is borrowed.
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

    /// Pins the frame. This is the one place a pin count leaves zero: the
    /// chain is borrowed, so the bucket's lock is held and the frame is
    /// linked (A1).
    pub(super) fn pin<'a>(&self, _arena: &'a Arena) -> Pinned<'a> {
        self.frame.pin();
        Pinned {
            frame: self.pointer,
            arena: PhantomData,
        }
    }
}

impl<'c> Entry<'c> {
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
    chain: PhantomData<&'c Chain>,
}

impl<'c> Iterator for Entries<'c> {
    type Item = Entry<'c>;

    fn next(&mut self) -> Option<Self::Item> {
        let pointer = self.cursor?;
        // SAFETY: the pointer is in a chain that is borrowed for `'c`, so
        // the bucket's lock is held for `'c` and the frame is linked (A1),
        // and a linked frame is not freed (A2).
        let frame: &'c Frame = unsafe { pointer.frame() };
        self.cursor = frame.next();
        Some(Entry { pointer, frame })
    }
}

impl Chain {
    pub(super) fn new() -> Self {
        Self {
            head: None,
            stamp: 0,
        }
    }

    pub(super) fn iter(&self) -> Entries<'_> {
        Entries {
            cursor: self.head,
            chain: PhantomData,
        }
    }

    /// Links a frame its caller owned. From here on other threads can reach
    /// it.
    pub(super) fn link(&mut self, frame: OwnedFrame) -> Entry<'_> {
        let pointer = frame.0;
        frame.header().set_next(self.head);
        self.head = Some(pointer);
        Entry {
            pointer,
            // SAFETY: the frame is linked in this chain now, and the chain
            // stays borrowed while the entry lives (A1).
            frame: unsafe { pointer.frame() },
        }
    }

    fn unlink(&mut self, pointer: FramePtr) -> bool {
        let mut before: Option<FramePtr> = None;
        let mut cursor = self.head;
        while let Some(current) = cursor {
            // SAFETY: `current` is linked in this chain, which is borrowed
            // mutably, so the bucket's lock is held (A1, A2).
            let frame = unsafe { current.frame() };
            let after = frame.next();
            if current == pointer {
                match before {
                    // SAFETY: as above, `before` is linked in this chain.
                    Some(before) => unsafe { before.frame() }.set_next(after),
                    None => self.head = after,
                }
                frame.set_next(None);
                return true;
            }
            before = Some(current);
            cursor = after;
        }
        false
    }

    /// Empties the chain of a pool that is going away. The arena is borrowed
    /// mutably, so no pin on any of its frames is left, and the caller owns
    /// every frame that was linked.
    pub(super) fn take_all(&mut self, _arena: &mut Arena) -> Vec<OwnedFrame> {
        let mut frames = Vec::new();
        while let Some(pointer) = self.head {
            self.unlink(pointer);
            // By A3: unlinked, and a `Pinned` borrows the arena.
            frames.push(OwnedFrame(pointer));
        }
        frames
    }

    /// Takes a frame out of the chain if it is linked here, has no pin and
    /// passes `check`. The caller then owns it.
    pub(super) fn take_unpinned(
        &mut self,
        pointer: FramePtr,
        check: impl FnOnce(&Frame) -> bool,
    ) -> Option<OwnedFrame> {
        let entry = self.iter().find(|entry| entry.pointer == pointer)?;
        if !entry.frame.is_unpinned() || !check(entry.frame) {
            return None;
        }
        self.unlink(pointer);
        // By A3: the frame is unlinked, and it had no pin while this
        // chain's lock was held, under which alone a first pin is taken.
        Some(OwnedFrame(pointer))
    }

    /// Takes a frame out of the chain whatever pins it has, and marks it
    /// dead. If it has no pin the caller owns it. Otherwise whoever gives
    /// back its last pin does.
    pub(super) fn retire(&mut self, pointer: FramePtr) -> Option<OwnedFrame> {
        if !self.unlink(pointer) {
            return None;
        }
        // SAFETY: the frame was linked in this chain until the line above,
        // under the chain's lock, so it was not freed (A2). It is not
        // retired yet, so no other thread can come to own it before
        // `Frame::retire` below, which is the last use of the reference.
        let frame = unsafe { pointer.frame() };
        frame.set_kind(kind::DEAD);
        // By A3: the frame is unlinked, and `retire` saw no pin.
        frame.retire().then_some(OwnedFrame(pointer))
    }

    /// Gives a pin back under the lock of the bucket the frame's page is
    /// in. If that leaves the frame with no pin and `idle` holds for it, the
    /// frame is taken out of the chain in the same step, and the caller owns
    /// it. A retired frame whose last pin this was is owned as well.
    pub(super) fn unpin_linked(
        &mut self,
        pin: Pinned<'_>,
        idle: impl FnOnce(&Frame) -> bool,
    ) -> Option<OwnedFrame> {
        let pointer = pin.frame;
        if pin.frame().unpin() {
            // By A3: retired, which unlinked it, and this was the last pin.
            return Some(OwnedFrame(pointer));
        }
        self.take_unpinned(pointer, idle)
    }
}

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

/// The fixed part of the pool: `count` frames of `frame_len` bytes in one
/// block of memory, with their headers.
pub(super) struct Arena {
    frames: Box<[Frame]>,
    memory: NonNull<u8>,
    layout: Layout,
    frame_len: usize,
}

// SAFETY: the arena owns its block, which is reached only through the
// frames, and those are `Send` and `Sync`.
unsafe impl Send for Arena {}
// SAFETY: as above.
unsafe impl Sync for Arena {}

impl fmt::Debug for Arena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arena")
            .field("frames", &self.frames.len())
            .field("frame_len", &self.frame_len)
            .finish()
    }
}

impl Arena {
    /// The bytes one frame costs beside its page: its header.
    pub(super) const HEADER_LEN: usize = size_of::<Frame>();

    /// The arena, and each of its frames, owned.
    pub(super) fn new(
        count: usize,
        frame_len: usize,
    ) -> Result<(Self, Vec<OwnedFrame>), CacheError> {
        assert!(count > 0 && frame_len > 0);
        let layout = layout(frame_len, count)?;
        let memory = allocate(layout);
        let frames: Box<[Frame]> = (0..count)
            .map(|index| {
                // SAFETY: `index * frame_len` is below the size of the block
                // that `memory` points to, so the result is inside it.
                let bytes = unsafe { memory.add(index * frame_len) };
                Frame::new(bytes, frame_len, false)
            })
            .collect();
        // By A3: nothing has seen these frames yet.
        let owned = frames
            .iter()
            .rev()
            .map(|frame| OwnedFrame(FramePtr(NonNull::from(frame))))
            .collect();
        let arena = Self {
            frames,
            memory,
            layout,
            frame_len,
        };
        Ok((arena, owned))
    }

    pub(super) fn len(&self) -> usize {
        self.frames.len()
    }

    pub(super) fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// The header of an arena frame, which lives as long as the arena.
    pub(super) fn frame(&self, index: usize) -> &Frame {
        &self.frames[index]
    }

    pub(super) fn pointer(&self, index: usize) -> FramePtr {
        FramePtr(NonNull::from(&self.frames[index]))
    }

    /// A frame beside the arena.
    pub(super) fn allocate_overflow(&self) -> OwnedFrame {
        let layout = layout(self.frame_len, 1).expect("the arena has this layout many times");
        let bytes = allocate(layout);
        let frame = Box::new(Frame::new(bytes, self.frame_len, true));
        // By A3: nothing has seen this frame yet.
        OwnedFrame(FramePtr(NonNull::from(Box::leak(frame))))
    }

    /// Gives the memory of an overflow frame back.
    pub(super) fn free_overflow(&self, frame: OwnedFrame) {
        assert!(frame.header().is_overflow(), "an arena frame is not freed");
        let layout = layout(self.frame_len, 1).expect("the arena has this layout many times");
        // SAFETY: an overflow frame was leaked from a box by
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
        // SAFETY: `memory` was allocated with `layout` in `new`, and the
        // frames that point into it are dropped with the arena.
        unsafe { alloc::dealloc(self.memory.as_ptr(), self.layout) };
    }
}
