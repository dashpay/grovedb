//! Reading the value of a node of a committed page.

use std::cell::RefCell;

use lignin_format::{
    tree_page::CellValue,
    value_log::{PendingValue, ValuePointer},
    CommitVersion, PageId,
};
use lignin_store::{
    cache::PageRef,
    load::{LoadBuffers, PageLoader},
    values::ValueStore,
};

use super::error::ValueError;
use crate::node::{NodeRef, PinnedNode};

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

    /// The value at `pointer`, which a cell of the page `owner` holds.
    pub(super) fn placed<'a>(
        &self,
        pointer: ValuePointer,
        owner: PageId,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        read_placed(self.store, pointer, owner, buf)
    }

    /// The value of `node`, a node of the committed page in `frame`, which
    /// the caller read the node from and holds ([`read_framed`]).
    pub(super) fn framed<'a>(
        &self,
        frame: &'a PageRef<'_>,
        node: NodeRef<'a>,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        read_framed(self.store, frame, node, buf)
    }

    /// The bytes of the pending value `pending`, which are with the frame
    /// of the committed page `page`.
    ///
    /// The page is loaded for this, so a caller asks it only of a page
    /// that the scope holds a private copy of: no page step is made
    /// beside a private copy (`transactions.md` PV7), and the frame the
    /// store has for the page is then the one the scope copied, with the
    /// bytes. The frame of a page that nobody has copied can be another
    /// by the time it is loaded again: write-back places the value in a
    /// page step and takes its bytes off the frame it puts there. A node
    /// of such a page is read from the frame it was read in
    /// ([`read_framed`]).
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
///   read (`transactions.md` FL7). It is found with the page that the node
///   holds pinned, not with the page as it is stored now: write-back can
///   have placed the value since, and taken its bytes off the frame that
///   replaced this one.
///
/// `store` is the value log of the store the page is of, and `buf` is what
/// a read from it reads into.
pub fn read_value<'a>(
    store: &ValueStore,
    node: &'a PinnedNode<'_>,
    buf: &'a mut ValueBuf,
) -> Result<&'a [u8], ValueError> {
    read_framed(store, node.page().frame(), node.node(), buf)
}

/// The value of `node`, a node of the committed page in `frame`: the read
/// of [`read_value`], for a caller that holds the frame it read the node
/// from and has no pin of the node's own. The frame is the one `node`
/// was read from, and not one that was loaded for the page afterwards.
pub(super) fn read_framed<'a>(
    store: &ValueStore,
    frame: &'a PageRef<'_>,
    node: NodeRef<'a>,
    buf: &'a mut ValueBuf,
) -> Result<&'a [u8], ValueError> {
    let page = frame.page();
    match node.value() {
        CellValue::Inline(bytes) => Ok(bytes),
        CellValue::OutOfLine(pointer) => read_placed(store, pointer, page, buf),
        CellValue::Pending(pending) => pending_bytes(frame.attached(pending.id()), pending, page),
    }
}
