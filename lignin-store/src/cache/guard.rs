//! What the pool hands out: a pinned page version, a frame to fill, a private
//! copy and its two latches.

use std::{
    fmt,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use lignin_format::{CommitVersion, PageId};

use super::{
    error::CacheError,
    frame::{
        flag, Attached, Exclusive, Frame, FramePtr, Frozen, OwnedFrame, Pinned, Shared, ValueId,
    },
    pool::BufferPool,
    sync::Writing,
    OwnerId,
};

fn version(raw: u64) -> Option<CommitVersion> {
    CommitVersion::new(raw)
}

fn find(values: &[Attached], id: ValueId) -> Option<&[u8]> {
    values
        .iter()
        .find(|value| value.id == id)
        .map(|value| &*value.bytes)
}

fn list(values: &[Attached]) -> impl Iterator<Item = (ValueId, &[u8])> {
    values.iter().map(|value| (value.id, &*value.bytes))
}

fn put(
    pool: &BufferPool,
    values: &mut Vec<Attached>,
    id: ValueId,
    bytes: Arc<[u8]>,
) -> Option<Arc<[u8]>> {
    pool.count_attached(bytes.len());
    let before = match values.iter_mut().find(|value| value.id == id) {
        Some(value) => Some(std::mem::replace(&mut value.bytes, bytes)),
        None => {
            values.push(Attached { id, bytes });
            None
        }
    };
    if let Some(before) = &before {
        pool.count_detached(before.len());
    }
    before
}

fn take(pool: &BufferPool, values: &mut Vec<Attached>, id: ValueId) -> Option<Arc<[u8]>> {
    let at = values.iter().position(|value| value.id == id)?;
    let value = values.swap_remove(at);
    pool.count_detached(value.bytes.len());
    Some(value.bytes)
}

fn mark(frame: &Frame, attached: bool) {
    let flags = frame.flags() & !flag::ATTACHED;
    frame.set_flags(if attached {
        flags | flag::ATTACHED
    } else {
        flags
    });
}

/// One version of a page, pinned in the pool.
///
/// It is the page's bytes, [`BufferPool::frame_len`] of them, and they do
/// not change while this lives. The frame is not evicted, reused or freed
/// until every `PageRef` to it is dropped. Cloning takes another pin on the
/// same frame.
///
/// Reading through it takes no latch and no lock.
pub struct PageRef<'p> {
    pool: &'p BufferPool,
    frozen: Option<Frozen<'p>>,
}

impl<'p> PageRef<'p> {
    pub(super) fn new(pool: &'p BufferPool, frozen: Frozen<'p>) -> Self {
        Self {
            pool,
            frozen: Some(frozen),
        }
    }

    fn frozen(&self) -> &Frozen<'p> {
        self.frozen.as_ref().expect("held until the drop")
    }

    pub(super) fn frame(&self) -> &Frame {
        self.frozen().frame()
    }

    pub(super) fn pointer(&self) -> FramePtr {
        self.frozen().pointer()
    }

    /// Whether the handle is of `pool`.
    pub(super) fn is_of(&self, pool: &BufferPool) -> bool {
        std::ptr::eq(self.pool, pool)
    }

    pub(super) fn attached_values(&self) -> &[Attached] {
        self.frozen().attached()
    }

    /// The page.
    pub fn page(&self) -> PageId {
        PageId::new(self.frame().page_raw()).expect("a pinned frame holds a page")
    }

    /// The commit that bore this page version (`transactions.md` PV1). It
    /// identifies the page version for as long as the page id is not used
    /// for another page: a commit that changes the page bears another
    /// version, and a page step that changes how this one is stored does
    /// not.
    pub fn born(&self) -> CommitVersion {
        version(self.frame().born()).expect("a page version has a born version")
    }

    /// The value attached to the page under `id`, if there is one.
    pub fn attached(&self, id: ValueId) -> Option<&[u8]> {
        find(self.attached_values(), id)
    }

    /// Every value attached to the page, with its id, in no order.
    pub fn attachments(&self) -> impl Iterator<Item = (ValueId, &[u8])> {
        list(self.attached_values())
    }
}

impl Deref for PageRef<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.frozen().bytes()
    }
}

impl AsRef<[u8]> for PageRef<'_> {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl Clone for PageRef<'_> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool,
            frozen: Some(self.frozen().again()),
        }
    }
}

impl Drop for PageRef<'_> {
    fn drop(&mut self) {
        if let Some(frozen) = self.frozen.take() {
            self.pool.unpin(frozen.into_pin());
        }
    }
}

impl fmt::Debug for PageRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageRef")
            .field("page", &self.page())
            .field("born", &self.born())
            .finish_non_exhaustive()
    }
}

/// A frame that belongs to its holder alone: nothing else can reach it until
/// it is handed to the pool.
///
/// It is [`BufferPool::frame_len`] bytes, to be filled. What they hold at
/// first is not specified: a frame is not cleared when it is reused. Dropping
/// it gives the frame back.
pub struct Blank<'p> {
    pool: &'p BufferPool,
    frame: Option<OwnedFrame>,
    writing: Option<Writing>,
}

impl<'p> Blank<'p> {
    pub(super) fn new(pool: &'p BufferPool, frame: OwnedFrame) -> Self {
        let writing = frame.header().writing();
        Self {
            pool,
            frame: Some(frame),
            writing: Some(writing),
        }
    }

    fn owned(&self) -> &OwnedFrame {
        self.frame.as_ref().expect("held until the drop")
    }

    fn owned_mut(&mut self) -> &mut OwnedFrame {
        self.frame.as_mut().expect("held until the drop")
    }

    pub(super) fn into_frame(mut self) -> OwnedFrame {
        self.writing = None;
        self.frame.take().expect("held until the drop")
    }

    pub(super) fn has_attached(&self) -> bool {
        !self.owned().attached().is_empty()
    }

    /// Whether the frame is of `pool`.
    pub(super) fn is_of(&self, pool: &BufferPool) -> bool {
        std::ptr::eq(self.pool, pool)
    }

    /// Makes this frame a copy of `page`: its bytes and its attached values.
    /// The values are shared with `page`, not copied.
    pub fn copy_from(&mut self, page: &PageRef<'_>) {
        self.clear_attached();
        self.owned_mut().bytes_mut().copy_from_slice(page);
        for value in page.attached_values() {
            self.attach(value.id, Arc::clone(&value.bytes));
        }
    }

    /// The value attached under `id`, if there is one.
    pub fn attached(&self, id: ValueId) -> Option<&[u8]> {
        find(self.owned().attached(), id)
    }

    /// Attaches `bytes` under `id`, and returns the value that was there.
    pub fn attach(&mut self, id: ValueId, bytes: Arc<[u8]>) -> Option<Arc<[u8]>> {
        let before = put(self.pool, self.owned_mut().attached_mut(), id, bytes);
        self.mark_attached();
        before
    }

    /// Takes the value attached under `id` off the frame.
    pub fn detach(&mut self, id: ValueId) -> Option<Arc<[u8]>> {
        let value = take(self.pool, self.owned_mut().attached_mut(), id);
        self.mark_attached();
        value
    }

    fn clear_attached(&mut self) {
        let pool = self.pool;
        let values = self.owned_mut().attached_mut();
        let bytes: usize = values.iter().map(|value| value.bytes.len()).sum();
        values.clear();
        pool.count_detached(bytes);
        self.mark_attached();
    }

    fn mark_attached(&mut self) {
        mark(self.owned().header(), self.has_attached());
    }
}

impl Deref for Blank<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.owned().bytes()
    }
}

impl DerefMut for Blank<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.owned_mut().bytes_mut()
    }
}

impl Drop for Blank<'_> {
    fn drop(&mut self) {
        self.writing = None;
        if let Some(frame) = self.frame.take() {
            self.pool.release(frame);
        }
    }
}

impl fmt::Debug for Blank<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Blank").finish_non_exhaustive()
    }
}

/// A private copy of a page: what an open write transaction changes, and
/// nobody else can see (`transactions.md` WT4).
///
/// The handle pins the frame. The copy itself belongs to its owner and stays
/// in the pool, whatever handles exist, until a commit installs it
/// ([`BufferPool::install`]) or it is discarded ([`BufferPool::discard`]).
/// A handle that is left after either still reads what the frame holds, and
/// can no longer change it. The bytes are reached through the copy's latch:
/// [`Self::read`] and [`Self::write`].
pub struct PrivateRef<'p> {
    pool: &'p BufferPool,
    pin: Option<Pinned<'p>>,
}

impl<'p> PrivateRef<'p> {
    pub(super) fn new(pool: &'p BufferPool, pin: Pinned<'p>) -> Self {
        Self {
            pool,
            pin: Some(pin),
        }
    }

    pub(super) fn pin(&self) -> &Pinned<'p> {
        self.pin.as_ref().expect("held until the drop")
    }

    pub(super) fn pointer(&self) -> FramePtr {
        self.pin().pointer()
    }

    /// Whether the handle is of `pool`.
    pub(super) fn is_of(&self, pool: &BufferPool) -> bool {
        std::ptr::eq(self.pool, pool)
    }

    /// The page.
    pub fn page(&self) -> PageId {
        PageId::new(self.pin().frame().page_raw()).expect("a pinned frame holds a page")
    }

    /// Who the copy belongs to.
    pub fn owner(&self) -> OwnerId {
        OwnerId::new(self.pin().frame().owner())
    }

    /// The commit that bore the page version this copy was made from, or
    /// `None` for a page that its transaction created.
    pub fn base(&self) -> Option<CommitVersion> {
        version(self.pin().frame().base())
    }

    /// Takes the latch for reading, and waits for a writer to finish.
    pub fn read(&self) -> SharedLatch<'_> {
        SharedLatch {
            latch: self.pin().read(),
        }
    }

    /// Takes the latch for reading, unless a writer has it.
    pub fn try_read(&self) -> Option<SharedLatch<'_>> {
        self.pin().try_read().map(|latch| SharedLatch { latch })
    }

    /// Takes the latch for writing, and waits for every other holder to
    /// finish.
    ///
    /// It fails with [`CacheError::NotPrivate`] if the copy was installed or
    /// discarded through another handle.
    pub fn write(&self) -> Result<ExclusiveLatch<'_>, CacheError> {
        self.exclusive(self.pin().write())
    }

    /// Takes the latch for writing, unless somebody has it: `Ok(None)` then.
    pub fn try_write(&self) -> Result<Option<ExclusiveLatch<'_>>, CacheError> {
        self.pin()
            .try_write()
            .map(|latch| self.exclusive(latch))
            .transpose()
    }

    fn exclusive<'l>(
        &'l self,
        latch: Option<Exclusive<'l>>,
    ) -> Result<ExclusiveLatch<'l>, CacheError> {
        match latch {
            Some(latch) => Ok(ExclusiveLatch {
                pool: self.pool,
                latch,
            }),
            None => Err(CacheError::NotPrivate { page: self.page() }),
        }
    }
}

impl Clone for PrivateRef<'_> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool,
            pin: Some(self.pin().again()),
        }
    }
}

impl Drop for PrivateRef<'_> {
    fn drop(&mut self) {
        if let Some(pin) = self.pin.take() {
            self.pool.unpin(pin);
        }
    }
}

impl fmt::Debug for PrivateRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateRef")
            .field("page", &self.page())
            .field("owner", &self.owner())
            .field("base", &self.base())
            .finish_non_exhaustive()
    }
}

/// The shared latch of a private copy: its bytes, which nobody changes while
/// this lives.
#[derive(Debug)]
pub struct SharedLatch<'l> {
    latch: Shared<'l>,
}

impl SharedLatch<'_> {
    /// The value attached under `id`, if there is one.
    pub fn attached(&self, id: ValueId) -> Option<&[u8]> {
        find(self.latch.attached(), id)
    }

    /// Every attached value, with its id, in no order.
    pub fn attachments(&self) -> impl Iterator<Item = (ValueId, &[u8])> {
        list(self.latch.attached())
    }
}

impl Deref for SharedLatch<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.latch.bytes()
    }
}

/// The exclusive latch of a private copy: its bytes, which only the holder
/// reads or changes while this lives.
#[derive(Debug)]
pub struct ExclusiveLatch<'l> {
    pool: &'l BufferPool,
    latch: Exclusive<'l>,
}

impl ExclusiveLatch<'_> {
    /// The value attached under `id`, if there is one.
    pub fn attached(&self, id: ValueId) -> Option<&[u8]> {
        find(self.latch.attached(), id)
    }

    /// Every attached value, with its id, in no order.
    pub fn attachments(&self) -> impl Iterator<Item = (ValueId, &[u8])> {
        list(self.latch.attached())
    }

    /// Attaches `bytes` under `id`, and returns the value that was there.
    pub fn attach(&mut self, id: ValueId, bytes: Arc<[u8]>) -> Option<Arc<[u8]>> {
        let before = put(self.pool, self.latch.attached_mut(), id, bytes);
        mark(self.latch.frame(), true);
        before
    }

    /// Takes the value attached under `id` off the copy.
    pub fn detach(&mut self, id: ValueId) -> Option<Arc<[u8]>> {
        let value = take(self.pool, self.latch.attached_mut(), id);
        mark(self.latch.frame(), !self.latch.attached().is_empty());
        value
    }
}

impl Deref for ExclusiveLatch<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.latch.bytes()
    }
}

impl DerefMut for ExclusiveLatch<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.latch.bytes_mut()
    }
}
