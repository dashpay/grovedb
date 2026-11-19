//! Reading the value of a node of a committed page.

use std::cell::RefCell;

use lignin_format::{
    tree_page::CellValue,
    value_log::{PendingValue, ValuePointer},
    CommitVersion, PageId,
};
use lignin_store::{
    load::{LoadBuffers, PageLoader},
    values::ValueStore,
};

use super::error::ValueError;
use crate::node::{NodeRef, PinnedNode, PinnedPage};

/// The memory a read of a value out of line uses, kept by the reader from
/// one read to the next.
///
/// It grows to the longest value that was read. A value of one record is
/// returned from the bytes that were read, and one of several records
/// (`value-log-format.md` V6) is put together once.
#[derive(Debug, Default)]
pub struct ValueBuf {
    records: Vec<u8>,
    joined: Vec<u8>,
}

impl ValueBuf {
    /// A buffer that holds nothing yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The value at `pointer`, which a cell of the page `owner` holds: one
/// positional read, checked against the pointer's length and the owner.
pub(super) fn read_placed<'a>(
    store: &ValueStore,
    pointer: ValuePointer,
    owner: PageId,
    buf: &'a mut ValueBuf,
) -> Result<&'a [u8], ValueError> {
    let value = store.read(pointer, owner, &mut buf.records)?;
    if let Some(bytes) = value.contiguous() {
        return Ok(bytes);
    }
    buf.joined.clear();
    for part in value.parts() {
        buf.joined.extend_from_slice(part);
    }
    Ok(&buf.joined)
}

/// The bytes that are kept for the pending value `pending` of a cell of
/// `page`, if they are as long as the cell says.
pub(super) fn pending_bytes(
    bytes: Option<&[u8]>,
    pending: PendingValue,
    page: PageId,
) -> Result<&[u8], ValueError> {
    bytes
        .filter(|bytes| bytes.len() == pending.value_len() as usize)
        .ok_or(ValueError::NoValue { page })
}

/// What a write scope reads the values from that are not in its memory:
/// the value log, and the committed pages at the version the scope reads.
///
/// The caller holds an operation guard while it uses this, and `at` is a
/// version it has pinned, or the latest version, read after the guard was
/// taken (`transactions.md` PN5).
#[derive(Debug)]
pub struct ValueSource<'s> {
    store: &'s ValueStore,
    loader: PageLoader<'s>,
    at: CommitVersion,
    buffers: RefCell<LoadBuffers>,
}

impl<'s> ValueSource<'s> {
    /// The value log `store`, and the pages of `loader`'s store at the
    /// version `at`.
    pub fn new(store: &'s ValueStore, loader: PageLoader<'s>, at: CommitVersion) -> Self {
        Self {
            store,
            loader,
            at,
            buffers: RefCell::default(),
        }
    }

    /// The value log.
    pub const fn store(&self) -> &'s ValueStore {
        self.store
    }

    /// The version the committed pages are read at.
    pub const fn at(&self) -> CommitVersion {
        self.at
    }

    /// The value at `pointer`, which a cell of the page `owner` holds.
    pub(super) fn placed<'a>(
        &self,
        pointer: ValuePointer,
        owner: PageId,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        read_placed(self.store, pointer, owner, buf)
    }

    /// The bytes of the pending value `pending`, which are with the frame
    /// of the committed page `page`.
    pub(super) fn pending<'a>(
        &self,
        page: PageId,
        pending: PendingValue,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        let frame = self
            .loader
            .get(page, self.at, &mut self.buffers.borrow_mut())?;
        let bytes = pending_bytes(frame.attached(pending.id()), pending, page)?;
        buf.joined.clear();
        buf.joined.extend_from_slice(bytes);
        Ok(&buf.joined)
    }
}

/// The value of `node`, a node of a committed page.
///
/// - A value in the cell is returned from the frame, with no copy.
/// - A value in the value log is read with one positional read, and is
///   checked to be of the length the cell states and owned by the node's
///   page (`value-log-format.md` P4): a pointer that a cell carried to
///   another page without its value fails here.
/// - A pending value is returned from the bytes that are with the frame of
///   the page, under the value's name. Neither the log nor the value log is
///   read (`transactions.md` FL7).
///
/// `store` is the value log of the store the page is of, and `buf` is what
/// a read from it reads into.
pub fn read_value<'a>(
    store: &ValueStore,
    node: &'a PinnedNode<'_>,
    buf: &'a mut ValueBuf,
) -> Result<&'a [u8], ValueError> {
    read_value_of(store, node.page(), node.node(), buf)
}

/// The value of `node`, which was read from `page`: [`read_value`] for a
/// reader that holds the page and a view of the node.
///
/// A pending value is found with the page that is held, not with the page
/// as it is stored now: write-back can have placed the value since, and
/// taken its bytes off the frame that replaced this one.
pub fn read_value_of<'a>(
    store: &ValueStore,
    page: &'a PinnedPage<'_>,
    node: NodeRef<'a>,
    buf: &'a mut ValueBuf,
) -> Result<&'a [u8], ValueError> {
    let id = node.address().page();
    match node.value() {
        CellValue::Inline(bytes) => Ok(bytes),
        CellValue::OutOfLine(pointer) => read_placed(store, pointer, id, buf),
        CellValue::Pending(pending) => {
            let attached = page.frame().attached(pending.id());
            pending_bytes(attached.filter(|_| page.id() == id), pending, id)
        }
    }
}
