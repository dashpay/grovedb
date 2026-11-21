//! A cursor through a write transaction: the committed records of the
//! moment it was opened, merged with the transaction's own writes.

use std::{cmp::Ordering, fmt, ops::Bound};

use lignin_format::{tree_page::Side, CellAddress, CommitVersion};
use lignin_store::load::LoadBuffers;
use lignin_tree::{
    cursor as tree,
    flat::{self, Committed},
    node::{Lease, NodeReader},
};

use super::{
    error::TxnError,
    frames::stored_key,
    key::{Subtree, Unit},
    opened::Opened,
    pages::{child_keys, find},
    path::{FlatReader, Idle, WritePath, IDLE},
    txn::{show_committed, WriteTxn},
    unlinked,
    view::{ChildKeys, NodeView},
    write_set::{Own, WriteSet},
};
use crate::{
    scope::table::Who,
    snapshot::{OperationGuard, VersionPin},
};

/// The committed side of a cursor of a write transaction
/// (`transactions.md` RD6): an ordered cursor over the records of one unit
/// as one committed version has them.
///
/// [`FlatSide`] is the one for flat records, and [`MerkSide`] the one for
/// the nodes of a Merk. What a side shows under a key is its own to say: a
/// value, or a node.
///
/// Every call that reads a page is given the guard of the operation it is
/// made in (PN5). The version is the cursor's, which keeps it pinned from
/// one call to the next.
pub trait CommittedSide<'s> {
    /// Goes to the first key at or after `key`.
    fn seek(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError>;

    /// Goes to the last key at or before `key`.
    fn seek_for_prev(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError>;

    /// Goes to the first key.
    fn seek_to_first(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError>;

    /// Goes to the last key.
    fn seek_to_last(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError>;

    /// Goes to the next key. Past the last one the side has no key.
    fn next(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError>;

    /// Goes to the key before. Before the first one the side has no key.
    fn prev(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError>;

    /// The key the side stands on, if it stands on one.
    fn key(&self) -> Option<&[u8]>;

    /// The subtree whose committed records the side shows, by the prefix
    /// its version has them under.
    fn under(&self) -> Subtree;

    /// Points the side at the committed records under the prefix `under`,
    /// at the same version. It then stands on no key.
    ///
    /// A cursor shows a subtree that its transaction moved from the
    /// records the committed version has under the prefix the subtree was
    /// taken from.
    fn point(&mut self, guard: &OperationGuard<'s>, under: Subtree) -> Result<(), TxnError>;
}

/// The committed flat records of one unit at one version.
pub struct FlatSide<'s> {
    path: WritePath<'s>,
    at: CommitVersion,
    /// The pages between two calls, bound to no operation, once a call has
    /// read through them.
    pages: Option<Committed<'s, 's, Idle>>,
    tree: flat::FlatTree,
    cursor: flat::Cursor,
    unit: Unit,
    key: Vec<u8>,
}

impl fmt::Debug for FlatSide<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlatSide")
            .field("unit", &self.unit)
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

impl<'s> FlatSide<'s> {
    /// Runs one call with the committed pages under `guard`, the guard of
    /// the operation the call is made in.
    fn read<'g, R>(
        &mut self,
        guard: &'g OperationGuard<'s>,
        run: impl FnOnce(&mut Self, &FlatReader<'s, 'g>) -> R,
    ) -> R {
        let pages = match self.pages.take() {
            Some(pages) => pages.under(guard),
            None => self.path.committed(guard, self.at),
        };
        let result = run(self, &pages);
        self.pages = Some(pages.under(&IDLE));
        result
    }
}

impl<'s> CommittedSide<'s> for FlatSide<'s> {
    fn seek(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError> {
        Ok(self.read(guard, |side, pages| side.cursor.seek(pages, key))?)
    }

    fn seek_for_prev(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError> {
        Ok(self.read(guard, |side, pages| side.cursor.seek_for_prev(pages, key))?)
    }

    fn seek_to_first(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        Ok(self.read(guard, |side, pages| side.cursor.seek_to_first(pages))?)
    }

    fn seek_to_last(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        Ok(self.read(guard, |side, pages| side.cursor.seek_to_last(pages))?)
    }

    fn next(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        Ok(self.read(guard, |side, pages| side.cursor.next(pages))?)
    }

    fn prev(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        Ok(self.read(guard, |side, pages| side.cursor.prev(pages))?)
    }

    fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }

    fn under(&self) -> Subtree {
        self.unit.subtree
    }

    fn point(&mut self, _guard: &OperationGuard<'s>, under: Subtree) -> Result<(), TxnError> {
        self.unit = Unit::new(self.unit.column, under);
        self.cursor = self.tree.cursor_within(under.as_bytes());
        Ok(())
    }
}

impl<'s> FlatSide<'s> {
    /// Appends the value of the record under `key` to `out`, and says
    /// whether the committed version has one. `key` is mostly the key the
    /// side stands on.
    fn value(
        &mut self,
        guard: &OperationGuard<'s>,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        Ok(self.read(guard, |side, pages| {
            if side.cursor.key() == Some(key) {
                return side.cursor.value(pages, out);
            }
            stored_key(&mut side.key, &side.unit.subtree, key);
            side.tree.get(pages, &side.key, out)
        })?)
    }
}

/// The committed nodes of one Merk at one version, in key order: the
/// ordered cursor of the tree layer ([`lignin_tree::cursor`]), which keeps
/// the way it came and no page pinned between two calls.
///
/// It reads under a lease of its own, taken when it is opened (PN1): its
/// handles are of the one version the cursor keeps pinned, whatever the
/// transaction's frames do meanwhile.
///
/// Where the subtree holds unlinked records at that version
/// (`lignindb::txn`, "Unlinked records"), they are shown among the nodes,
/// in key order: the side is then two cursors, one over the tree and one
/// over the records, and stands on the nearer of their keys. A subtree
/// that holds none has the cursor over the tree alone. Whether it holds
/// any is asked when the cursor is opened, and again of each Merk that
/// the cursor is pointed at ([`MerkCursor::retarget`]).
pub struct MerkSide<'s> {
    path: WritePath<'s>,
    at: CommitVersion,
    lease: Lease,
    cursor: tree::Cursor,
    /// The prefix the version has the Merk under.
    under: Subtree,
    buffers: LoadBuffers,
    unlinked: Option<UnlinkedSide<'s>>,
}

/// The unlinked records of one subtree at one version, in key order: the
/// flat records of the data column under the subtree.
struct UnlinkedSide<'s> {
    records: FlatSide<'s>,
    /// Whether the side was last moved backwards: its two cursors then
    /// stand each at its last key at or before the place, and otherwise
    /// each at its first key at or after it.
    back: bool,
}

impl<'s> UnlinkedSide<'s> {
    /// The unlinked records that the version `at` has under `subtree`,
    /// which stands nowhere yet.
    fn over(path: WritePath<'s>, at: CommitVersion, subtree: Subtree) -> Self {
        let unit = Unit::data(subtree);
        let tree = path.keyspaces().of(unit.column);
        Self {
            records: FlatSide {
                path,
                at,
                pages: None,
                cursor: tree.cursor_within(subtree.as_bytes()),
                tree,
                unit,
                key: Vec::new(),
            },
            back: false,
        }
    }
}

impl fmt::Debug for MerkSide<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MerkSide")
            .field("at", &self.at)
            .field("root", &self.cursor.root())
            .finish_non_exhaustive()
    }
}

impl<'s> MerkSide<'s> {
    /// The reader of the side's version for the operation that holds
    /// `guard`.
    fn reader<'g>(&self, guard: &'g OperationGuard<'s>) -> NodeReader<'g, OperationGuard<'s>> {
        NodeReader::new(self.path.loader, guard, self.lease, self.at)
    }

    /// Points the side at the Merk that its version has under `subtree`,
    /// whose root is at `root`: the tree, and the unlinked records under
    /// that prefix where `records` says that the Merk holds any. The
    /// side then stands nowhere.
    fn open_at(
        &mut self,
        guard: &OperationGuard<'s>,
        subtree: Subtree,
        root: Option<CellAddress>,
        records: bool,
    ) {
        self.unlinked = records.then(|| UnlinkedSide::over(self.path, self.at, subtree));
        self.cursor = tree::Cursor::open(&self.reader(guard), root);
        self.under = subtree;
    }

    /// Shows `read` the committed node under `key`, or `None` if the
    /// version has none. `key` is mostly the key the side stands on.
    ///
    /// `children`, where it is given, takes the key of each child of that
    /// node, read from the child's cell at the side's version.
    fn node<R>(
        &mut self,
        guard: &OperationGuard<'s>,
        key: &[u8],
        children: Option<&mut ChildKeys>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        // A key has a node of the tree or an unlinked record.
        if let Some(unlinked) = &mut self.unlinked
            && unlinked.records.key() == Some(key)
        {
            let mut record = Vec::new();
            let held = unlinked.records.value(guard, key, &mut record)?;
            let node = held.then(|| unlinked::decode(key, &record)).transpose()?;
            return Ok(read(node.map(NodeView::unlinked)));
        }
        if self.cursor.key() == Some(key) {
            let reader = self.reader(guard);
            let node = self.cursor.node(&reader, &mut self.buffers)?;
            if let (Some(node), Some(children)) = (&node, children) {
                for side in [Side::Left, Side::Right] {
                    let slot = children.slot(side);
                    if let Some(link) = node.node().link(side) {
                        let child = reader.node(link.child, &mut self.buffers)?;
                        slot.extend_from_slice(child.node().key());
                    }
                }
            }
            let store = self.path.values;
            return Ok(read(
                node.as_ref().map(|node| NodeView::committed(node, store)),
            ));
        }
        // The cursor stands on a key that the transaction wrote and has
        // taken back since: the version's node under it, if it has one, is
        // found by the key.
        let mut pages = (self.path).committed_pages(guard, self.at, self.lease, &mut self.buffers);
        let found = find(&mut pages, self.cursor.root(), key)?;
        if let (Some(found), Some(children)) = (found, children) {
            child_keys(&mut pages, found, children)?;
        }
        if let (None, Some(unlinked)) = (found, &mut self.unlinked) {
            let mut record = Vec::new();
            if unlinked.records.value(guard, key, &mut record)? {
                return Ok(read(Some(NodeView::unlinked(unlinked::decode(
                    key, &record,
                )?))));
            }
        }
        show_committed(&mut pages, found, read)
    }

    /// Has the cursor over the tree and the one over the unlinked records
    /// stand as a move in the direction `back` leaves them, at `key`: for
    /// a step that goes the other way than the side was last moved.
    fn turn(&mut self, guard: &OperationGuard<'s>, back: bool) -> Result<(), TxnError> {
        let turned = (self.unlinked.as_ref()).is_some_and(|unlinked| unlinked.back != back);
        if turned && let Some(key) = self.key().map(<[u8]>::to_vec) {
            match back {
                false => self.seek(guard, &key)?,
                true => self.seek_for_prev(guard, &key)?,
            }
        }
        Ok(())
    }

    /// Moves the cursor over the unlinked records, if the side has one,
    /// and notes the direction.
    fn records(
        &mut self,
        back: bool,
        go: impl FnOnce(&mut FlatSide<'s>) -> Result<(), TxnError>,
    ) -> Result<(), TxnError> {
        match &mut self.unlinked {
            None => Ok(()),
            Some(unlinked) => {
                unlinked.back = back;
                go(&mut unlinked.records)
            }
        }
    }

    /// Which of the two cursors stand on the side's key: the one over the
    /// tree, and the one over the unlinked records.
    fn on_key(&self) -> (bool, bool) {
        match &self.unlinked {
            None => (self.cursor.key().is_some(), false),
            Some(unlinked) => {
                unlinked::nearer(self.cursor.key(), unlinked.records.key(), unlinked.back)
            }
        }
    }
}

impl<'s> CommittedSide<'s> for MerkSide<'s> {
    fn seek(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek(&reader, &mut self.buffers, key)?;
        self.records(false, |records| records.seek(guard, key))
    }

    fn seek_for_prev(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek_for_prev(&reader, &mut self.buffers, key)?;
        self.records(true, |records| records.seek_for_prev(guard, key))
    }

    fn seek_to_first(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek_to_first(&reader, &mut self.buffers)?;
        self.records(false, |records| records.seek_to_first(guard))
    }

    fn seek_to_last(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek_to_last(&reader, &mut self.buffers)?;
        self.records(true, |records| records.seek_to_last(guard))
    }

    fn next(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        self.turn(guard, false)?;
        let (node, record) = self.on_key();
        if node {
            let reader = self.reader(guard);
            self.cursor.next(&reader, &mut self.buffers)?;
        }
        match record {
            true => self.records(false, |records| records.next(guard)),
            false => Ok(()),
        }
    }

    fn prev(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        self.turn(guard, true)?;
        let (node, record) = self.on_key();
        if node {
            let reader = self.reader(guard);
            self.cursor.prev(&reader, &mut self.buffers)?;
        }
        match record {
            true => self.records(true, |records| records.prev(guard)),
            false => Ok(()),
        }
    }

    fn key(&self) -> Option<&[u8]> {
        let Some(unlinked) = &self.unlinked else {
            return self.cursor.key();
        };
        match self.on_key() {
            (true, _) => self.cursor.key(),
            (false, true) => unlinked.records.key(),
            (false, false) => None,
        }
    }

    fn under(&self) -> Subtree {
        self.under
    }

    fn point(&mut self, guard: &OperationGuard<'s>, under: Subtree) -> Result<(), TxnError> {
        let mut opened = None;
        let directory = Opened::at(&mut opened, &self.path, guard, self.at)?;
        let root = directory.root(&self.path, guard, &under)?.flatten();
        self.cursor = tree::Cursor::open(&self.reader(guard), root);
        self.under = under;
        Ok(())
    }
}

/// A cursor through a write transaction over the nodes of one Merk.
pub type MerkCursor<'t, 's> = Cursor<'t, 's, MerkSide<'s>>;

/// Whether the version `at` holds unlinked records under `subtree`, which
/// it has entered, for a cursor that keeps `directory` of that version.
/// An answer is kept there: once the cursor has one for a subtree, it is
/// not looked up again for that cursor. Where the cursor has none yet,
/// the transaction is asked, which looks it up unless it keeps the answer
/// for that version itself (`WriteTxn::committed_has_unlinked`). A lookup
/// that fails leaves no answer.
fn holds_unlinked<'s>(
    txn: &WriteTxn<'s>,
    directory: &mut Opened,
    guard: &OperationGuard<'s>,
    at: CommitVersion,
    subtree: &Subtree,
) -> Result<bool, TxnError> {
    if let Some(known) = directory.unlinked_known(subtree) {
        return Ok(known);
    }
    let has = txn.committed_has_unlinked(guard, at, subtree)?;
    directory.know_unlinked(subtree, has);
    Ok(has)
}

/// Which way a cursor last moved, which says where its committed side
/// stands: at the first committed key at or after the cursor's key, or at
/// the last one at or before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    Forward,
    Back,
    /// The committed side stands nowhere that follows from the cursor's
    /// key: it was not moved while it was not read.
    Lost,
}

/// A cursor through a write transaction over the records of one unit, in
/// key order (`transactions.md` RD6).
///
/// - **The committed side is fixed** at the latest committed version of the
///   moment the cursor was opened, which the cursor keeps pinned (PN1). A
///   commit that is published afterwards is not seen by this cursor, and is
///   seen by the next one.
/// - **The transaction's own writes are live.** A write the transaction
///   makes after the cursor was opened is seen when the cursor reaches its
///   key, and a delete hides the committed record. A rollback takes the
///   writes out again in the same way.
/// - **The cursor stands on a key, not on a record.** After a write to the
///   key it stands on, [`Self::key`] is still that key, [`Self::value`]
///   reads what is under it now, and a step goes on from the key. So the
///   delete of the record the cursor stands on, followed by a step, is
///   defined (`flat-store.md`, rule 7), and so is every other write under
///   an open cursor.
///
/// Each call is one operation of the transaction. It holds an operation
/// guard for its length, beside the cursor's pin (PN5): background work
/// can give a pinned page version another place without a commit. The
/// transaction's lock is held only while its write set is looked at.
pub struct Cursor<'t, 's, C = FlatSide<'s>> {
    txn: &'t WriteTxn<'s>,
    who: Who,
    unit: Unit,
    pin: VersionPin<'s>,
    side: C,
    /// The subtree directory of the cursor's version, through which the
    /// cursor opens its subtree.
    opened: Option<Opened>,
    /// Whether the committed side is read, and the count of the
    /// transaction's drops and clears it was found at
    /// ([`WriteSet::left`]): the cursor asks again when that moved.
    seen: Option<(u64, bool)>,
    key: Option<Vec<u8>>,
    way: Way,
}

impl<'t, 's> Cursor<'t, 's> {
    pub(crate) fn flat(txn: &'t WriteTxn<'s>, who: Who, unit: Unit) -> Self {
        let path = *txn.path();
        // The version is pinned here, and its pages are read from the
        // cursor's first step on, each under the guard of its operation
        // (PN5).
        let pin = path.versions().pin();
        let tree = path.keyspaces().of(unit.column);
        let side = FlatSide {
            path,
            at: pin.version(),
            pages: None,
            cursor: tree.cursor_within(unit.subtree.as_bytes()),
            tree,
            unit,
            key: Vec::new(),
        };
        Self::over(txn, who, unit, pin, side)
    }

    /// Appends the value under the key the cursor stands on to `out`, and
    /// says whether there is one: none if the cursor stands on no key, or
    /// if the transaction has deleted the record since the cursor came to
    /// it.
    pub fn value(&mut self, out: &mut Vec<u8>) -> Result<bool, TxnError> {
        let Some(key) = &self.key else {
            return Ok(false);
        };
        let key = key.clone();
        let key = &key;
        let guard = self.txn.path().versions().guard();
        let reads = self.reads(&guard)?;
        let unit = self.unit;
        let own = self
            .txn
            .with_set(self.who, self.unit, |set| match set.own(unit, key) {
                Own::Put(value) => {
                    out.extend_from_slice(value);
                    Ok(Some(true))
                }
                Own::Deleted => Ok(Some(false)),
                Own::Unwritten => Ok(None),
                Own::Node(_) => Err(TxnError::Damaged("a Merk node under a cursor of records")),
            })??;
        match own {
            Some(found) => Ok(found),
            None if reads => self.side.value(&guard, key, out),
            None => Ok(false),
        }
    }
}

impl<'t, 's> MerkCursor<'t, 's> {
    pub(crate) fn merk(
        txn: &'t WriteTxn<'s>,
        who: Who,
        subtree: Subtree,
    ) -> Result<Self, TxnError> {
        let path = *txn.path();
        // The root is the one the pinned version has, read under a guard
        // (PN5). The lease is the cursor's own (PN1). The transaction pins
        // the version, and dates the lease with it: a write may keep the
        // value of a node the cursor shows, by the node's handle (CF2a).
        let lease = Lease::take();
        let (guard, pin) = txn.pin_shown(lease);
        let at = pin.version();
        let mut opened = None;
        let directory = Opened::at(&mut opened, &path, &guard, at)?;
        let entered = directory.root(&path, &guard, &subtree)?;
        let root = entered.flatten();
        // Whether the version holds unlinked records under the subtree
        // is asked once, and of the transaction, which may know. No record
        // is read under a subtree that is not entered.
        let unit = Unit::data(subtree);
        let records = entered.is_some() && holds_unlinked(txn, directory, &guard, at, &subtree)?;
        let unlinked = records.then(|| UnlinkedSide::over(path, at, subtree));
        let reader = NodeReader::new(path.loader, &guard, lease, at);
        let side = MerkSide {
            path,
            at,
            lease,
            cursor: tree::Cursor::open(&reader, root),
            under: subtree,
            buffers: LoadBuffers::new(),
            unlinked,
        };
        let mut cursor = Self::over(txn, who, unit, pin, side);
        cursor.opened = opened;
        Ok(cursor)
    }

    /// Points the cursor at the Merk `subtree`. Its committed side stays
    /// the version the cursor was opened on, which it goes on keeping
    /// pinned, under the lease it took then: it now shows the nodes that
    /// version has of `subtree`, and among them the unlinked records that
    /// version has of it and none of the Merk the cursor was pointed at
    /// before, found through that version's subtree
    /// directory, merged with what the transaction has written to
    /// `subtree` by each call (`transactions.md` RD6). So one cursor reads
    /// several Merks as one committed state has them, whatever is
    /// committed between two of its reads.
    ///
    /// The cursor then stands on no key, whether or not the call fails:
    /// it is sought before it is read or stepped. A call that fails
    /// leaves it pointed at the Merk it was pointed at before.
    ///
    /// A later seek, and a read or a step from a key the cursor stands
    /// on, is a read of `subtree` like any other, and is refused where
    /// another open scope holds that Merk for writing (SC3). A read or a
    /// step of a cursor that stands on no key reads nothing, and is not
    /// refused. This call makes at most one lookup in the directory, and
    /// none for a subtree the cursor has opened before. Where that
    /// version has `subtree` entered, it asks whether the Merk holds
    /// unlinked records, as the opening of a cursor does. The cursor
    /// keeps each answer: once it has one for a subtree, it has it
    /// looked up no more, whichever version the transaction has read
    /// since, and it has none looked up where the transaction keeps the
    /// answer for the cursor's version. A lookup that fails leaves no
    /// answer. It reads no node.
    pub fn retarget(&mut self, subtree: Subtree) -> Result<(), TxnError> {
        self.key = None;
        self.way = Way::Lost;
        let path = *self.txn.path();
        let guard = path.versions().guard();
        let at = self.side.at;
        let directory = Opened::at(&mut self.opened, &path, &guard, at)?;
        let entered = directory.root(&path, &guard, &subtree)?;
        let records =
            entered.is_some() && holds_unlinked(self.txn, directory, &guard, at, &subtree)?;
        let root = entered.flatten();
        self.side.open_at(&guard, subtree, root, records);
        self.unit = Unit::data(subtree);
        // Whether the committed side is read is asked again, of the new
        // subtree, by the next move.
        self.seen = None;
        Ok(())
    }

    /// Shows `read` the node under the key the cursor stands on: the
    /// transaction's own node if it has written the key, and otherwise the
    /// node of the cursor's version. `None` if the cursor stands on no
    /// key, or if the transaction has deleted the node since the cursor
    /// came to it.
    ///
    /// `read` runs inside the operation: it must not call the transaction.
    pub fn node<R>(&mut self, read: impl FnOnce(Option<NodeView<'_>>) -> R) -> Result<R, TxnError> {
        self.show(None, read)
    }

    /// As [`Self::node`], and reads the key of each child of the node into
    /// `children`, in the same operation, where the node is shown from a
    /// cell: from the child's cell in the transaction's frames for a node
    /// it wrote, and otherwise from the child's cell at the cursor's
    /// version. A node that is shown as a record of the write set has the
    /// keys in its links ([`LinkView::key`](super::LinkView::key)), and
    /// `children` holds none, as with no node shown.
    ///
    /// It is what a record of the node as Merk encodes it takes
    /// (`tree-page.md`, section 7): a cell holds the length of a child's
    /// key and not the key (TP10).
    pub fn node_with_children<R>(
        &mut self,
        children: &mut ChildKeys,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        self.show(Some(children), read)
    }

    fn show<R>(
        &mut self,
        mut children: Option<&mut ChildKeys>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        if let Some(children) = children.as_deref_mut() {
            children.clear();
        }
        let Some(key) = &self.key else {
            return Ok(read(None));
        };
        let key = key.clone();
        let subtree = self.unit.subtree;
        let own = children.as_deref_mut();
        let read = match (self.txn).own_node_in(self.who, subtree, &key, own, read)? {
            Ok(shown) => return Ok(shown),
            Err(read) => read,
        };
        let guard = self.txn.path().versions().guard();
        let mut found = false;
        let shown = match self.reads(&guard) {
            Ok(true) => self.side.node(&guard, &key, children, |view| {
                found = view.is_some();
                read(view)
            }),
            Ok(false) => Ok(read(None)),
            Err(error) => Err(error),
        };
        // The transaction's lock is not held over the cursor's own read,
        // and another thread may have ended the transaction, or had it
        // forget the lease, meanwhile. So the cursor says what it read
        // whenever the read was made, and the transaction decides by what
        // it holds then.
        if shown.is_ok() {
            (self.txn).cursor_read(self.side.lease, self.side.at, found);
        }
        shown
    }
}

/// A cursor of a scope between two calls, kept apart from the scope
/// ([`Scope::parked_merk_cursor`](super::Scope::parked_merk_cursor)).
///
/// A cursor borrows the scope it was opened through, so its holder keeps
/// the scope borrowed for as long as the cursor lives. A parked cursor
/// borrows the transaction instead and offers no call of its own: it is
/// moved and read through [`Scope::resume`](super::Scope::resume) of the
/// scope that opened it, which gives a [`Resumed`] for the length of one
/// borrow of the scope. So it is never used without its scope, nor after
/// the scope has ended, and it can be kept beside a scope that is behind a
/// lock.
///
/// Parked, it holds what a cursor holds between two calls: the pin of its
/// version, its lease, and the key it stands on (RD6). It holds no page:
/// the ordered cursor of the tree layer keeps none pinned between two
/// calls ([`lignin_tree::cursor`]). The pin of the version is held until
/// the parked cursor is dropped, also after its scope has ended.
pub struct ParkedCursor<'t, 's, C = FlatSide<'s>> {
    cursor: Cursor<'t, 's, C>,
}

const _: fn() = || {
    fn moved<T: Send>() {}
    moved::<MerkCursor<'static, 'static>>();
    moved::<ParkedCursor<'static, 'static, MerkSide<'static>>>();
};

impl<'t, 's, C> ParkedCursor<'t, 's, C> {
    pub(crate) fn new(cursor: Cursor<'t, 's, C>) -> Self {
        Self { cursor }
    }

    /// The cursor, for the scope `who` of the transaction `txn`, if that
    /// is the scope that opened it.
    pub(crate) fn resume(
        &mut self,
        txn: &WriteTxn<'s>,
        who: Who,
    ) -> Option<Resumed<'_, 't, 's, C>> {
        let opened = std::ptr::eq(self.cursor.txn, txn) && self.cursor.who == who;
        opened.then_some(Resumed {
            cursor: &mut self.cursor,
        })
    }
}

/// A parked cursor for the length of one borrow of the scope that opened
/// it ([`Scope::resume`](super::Scope::resume)): the calls of a [`Cursor`],
/// each as the cursor makes it.
///
/// It does not hand the cursor out. A cursor that could be taken from its
/// place could be used once its scope has ended.
pub struct Resumed<'a, 't, 's, C = FlatSide<'s>> {
    cursor: &'a mut Cursor<'t, 's, C>,
}

impl<'s, C: CommittedSide<'s>> Resumed<'_, '_, 's, C> {
    /// [`Cursor::version`].
    pub fn version(&self) -> CommitVersion {
        self.cursor.version()
    }

    /// [`Cursor::valid`].
    pub fn valid(&self) -> bool {
        self.cursor.valid()
    }

    /// [`Cursor::key`].
    pub fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }

    /// [`Cursor::seek`].
    pub fn seek(&mut self, key: &[u8]) -> Result<(), TxnError> {
        self.cursor.seek(key)
    }

    /// [`Cursor::seek_for_prev`].
    pub fn seek_for_prev(&mut self, key: &[u8]) -> Result<(), TxnError> {
        self.cursor.seek_for_prev(key)
    }

    /// [`Cursor::seek_to_first`].
    pub fn seek_to_first(&mut self) -> Result<(), TxnError> {
        self.cursor.seek_to_first()
    }

    /// [`Cursor::seek_to_last`].
    pub fn seek_to_last(&mut self) -> Result<(), TxnError> {
        self.cursor.seek_to_last()
    }

    /// [`Cursor::next`].
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<(), TxnError> {
        self.cursor.next()
    }

    /// [`Cursor::prev`].
    pub fn prev(&mut self) -> Result<(), TxnError> {
        self.cursor.prev()
    }
}

#[cfg(test)]
impl<'t, 's, C> Resumed<'_, 't, 's, C> {
    /// The cursor itself, for the tests of this crate.
    pub(crate) fn cursor(&mut self) -> &mut Cursor<'t, 's, C> {
        self.cursor
    }
}

impl<'s> Resumed<'_, '_, 's, MerkSide<'s>> {
    /// [`MerkCursor::retarget`].
    pub fn retarget(&mut self, subtree: Subtree) -> Result<(), TxnError> {
        self.cursor.retarget(subtree)
    }

    /// [`MerkCursor::node`].
    pub fn node<R>(&mut self, read: impl FnOnce(Option<NodeView<'_>>) -> R) -> Result<R, TxnError> {
        self.cursor.node(read)
    }

    /// [`MerkCursor::node_with_children`].
    pub fn node_with_children<R>(
        &mut self,
        children: &mut ChildKeys,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        self.cursor.node_with_children(children, read)
    }
}

impl<C: fmt::Debug> fmt::Debug for ParkedCursor<'_, '_, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ParkedCursor").field(&self.cursor).finish()
    }
}

impl<'t, 's, C: CommittedSide<'s>> Cursor<'t, 's, C> {
    /// A cursor whose committed side is `side`, which reads the version
    /// `pin` holds.
    pub(crate) fn over(
        txn: &'t WriteTxn<'s>,
        who: Who,
        unit: Unit,
        pin: VersionPin<'s>,
        side: C,
    ) -> Self {
        Self {
            txn,
            who,
            unit,
            pin,
            side,
            opened: None,
            seen: None,
            key: None,
            way: Way::Forward,
        }
    }

    /// Whether the committed side is read: the cursor's version has the
    /// subtree entered, and the transaction has not dropped or cleared a
    /// Merk it is below (`transactions.md` CV9). No record is read under a
    /// subtree without its entry.
    ///
    /// The subtree is opened once, and asked for again whenever the
    /// transaction has dropped or cleared a subtree since, or has taken
    /// such a write back.
    ///
    /// Where the transaction moved the subtree, the committed side is the
    /// records that the cursor's version has under the prefix the subtree
    /// was taken from, and where a move took a subtree from the cursor's
    /// prefix and brought none, or takes the subtree out with one above
    /// it ([`Opened::leaves_with_a_move`]), it is not read. A transaction that serves
    /// its records by key refuses the read where a move decides what is
    /// under the subtree ([`BatchError::MovedByKey`](super::BatchError)).
    fn reads(&mut self, guard: &OperationGuard<'s>) -> Result<bool, TxnError> {
        let (path, at, subtree) = (*self.txn.path(), self.pin.version(), self.unit.subtree);
        (self.txn).guard_cursor(self.who, subtree, &mut self.opened, guard, at)?;
        let (opened, seen) = (&mut self.opened, self.seen);
        let (left, reads, under) = self.txn.with_set(self.who, self.unit, |set| {
            if let Some((_, reads)) = seen.filter(|(left, _)| *left == set.left()) {
                return Ok((set.left(), reads, None));
            }
            let from = set.committed_from(subtree);
            let opened = Opened::at(opened, &path, guard, at)?;
            let reads = match from {
                Some((under, since)) => {
                    !opened.leaves_with_a_move(&path, guard, set, &subtree)?
                        && (opened.seen_before(&path, guard, set, &under, since)?).is_some()
                }
                None => false,
            };
            Ok::<_, TxnError>((set.left(), reads, from.map(|(under, _)| under)))
        })??;
        if let Some(under) = under.filter(|under| *under != self.side.under()) {
            self.side.point(guard, under)?;
        }
        if seen != Some((left, reads)) {
            // The transaction dropped or cleared a subtree, or took such a
            // write back: the committed side may have come into the
            // cursor's view or left it, and is placed again by the next
            // move.
            self.way = Way::Lost;
        }
        self.seen = Some((left, reads));
        Ok(reads)
    }

    /// The committed version the cursor reads beside the transaction's
    /// writes.
    pub fn version(&self) -> CommitVersion {
        self.pin.version()
    }

    /// Whether the cursor stands on a key.
    pub fn valid(&self) -> bool {
        self.key.is_some()
    }

    /// The key the cursor stands on.
    pub fn key(&self) -> Option<&[u8]> {
        self.key.as_deref()
    }

    /// Goes to the first key at or after `key`.
    pub fn seek(&mut self, key: &[u8]) -> Result<(), TxnError> {
        let guard = self.txn.path().versions().guard();
        if self.reads(&guard)? {
            self.side.seek(&guard, key)?;
        }
        self.settle(&guard, Bound::Included(key.to_vec()), Way::Forward)
    }

    /// Goes to the last key at or before `key`.
    pub fn seek_for_prev(&mut self, key: &[u8]) -> Result<(), TxnError> {
        let guard = self.txn.path().versions().guard();
        if self.reads(&guard)? {
            self.side.seek_for_prev(&guard, key)?;
        }
        self.settle(&guard, Bound::Included(key.to_vec()), Way::Back)
    }

    /// Goes to the first key.
    pub fn seek_to_first(&mut self) -> Result<(), TxnError> {
        let guard = self.txn.path().versions().guard();
        if self.reads(&guard)? {
            self.side.seek_to_first(&guard)?;
        }
        self.settle(&guard, Bound::Unbounded, Way::Forward)
    }

    /// Goes to the last key.
    pub fn seek_to_last(&mut self) -> Result<(), TxnError> {
        let guard = self.txn.path().versions().guard();
        if self.reads(&guard)? {
            self.side.seek_to_last(&guard)?;
        }
        self.settle(&guard, Bound::Unbounded, Way::Back)
    }

    /// Goes to the next key. Past the last key, and from no key, the
    /// cursor stands on none.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<(), TxnError> {
        let Some(key) = self.key.take() else {
            return Ok(());
        };
        let guard = self.txn.path().versions().guard();
        // The committed side goes to the first committed key after `key`.
        if self.reads(&guard)? {
            if self.way != Way::Forward {
                self.side.seek(&guard, &key)?;
            }
            if self.side.key() == Some(&key[..]) {
                self.side.next(&guard)?;
            }
        }
        self.settle(&guard, Bound::Excluded(key), Way::Forward)
    }

    /// Goes to the key before. Before the first key, and from no key, the
    /// cursor stands on none.
    pub fn prev(&mut self) -> Result<(), TxnError> {
        let Some(key) = self.key.take() else {
            return Ok(());
        };
        let guard = self.txn.path().versions().guard();
        if self.reads(&guard)? {
            if self.way != Way::Back {
                self.side.seek_for_prev(&guard, &key)?;
            }
            if self.side.key() == Some(&key[..]) {
                self.side.prev(&guard)?;
            }
        }
        self.settle(&guard, Bound::Excluded(key), Way::Back)
    }

    /// Finds the first key within `bound`, going `way`, that has a record
    /// in the transaction's view, and stands on it. The committed side
    /// stands at its first key within the bound.
    fn settle(
        &mut self,
        guard: &OperationGuard<'s>,
        mut bound: Bound<Vec<u8>>,
        way: Way,
    ) -> Result<(), TxnError> {
        // Where the committed side is not read, it stands nowhere.
        let reads = self.seen.is_some_and(|(_, reads)| reads);
        self.way = match reads {
            true => way,
            false => Way::Lost,
        };
        self.key = None;
        let back = way == Way::Back;
        let unit = self.unit;
        loop {
            let committed = self.side.key().filter(|_| reads);
            // The next key the transaction wrote, and whether it is a
            // record now.
            let own = self.txn.with_set(self.who, self.unit, |set| {
                let within = match (&bound, back) {
                    (Bound::Unbounded, _) => (Bound::Unbounded, Bound::Unbounded),
                    (Bound::Included(key), false) => (Bound::Included(&key[..]), Bound::Unbounded),
                    (Bound::Excluded(key), false) => (Bound::Excluded(&key[..]), Bound::Unbounded),
                    (Bound::Included(key), true) => (Bound::Unbounded, Bound::Included(&key[..])),
                    (Bound::Excluded(key), true) => (Bound::Unbounded, Bound::Excluded(&key[..])),
                };
                let written = set.written_key(unit, within, back);
                let first = first_of(written, committed, back);
                first.map(|(key, written)| (key.to_vec(), visible(set, unit, key, written)))
            })?;
            let Some((key, shown)) = own else {
                return Ok(());
            };
            if shown {
                self.key = Some(key);
                return Ok(());
            }
            // The key has no record in the transaction's view: it goes on
            // behind it.
            if committed == Some(&key[..]) {
                match back {
                    false => self.side.next(guard)?,
                    true => self.side.prev(guard)?,
                }
            }
            bound = Bound::Excluded(key);
        }
    }
}

/// The key that comes first, going back or forward, of the next key the
/// transaction wrote and the next committed key, with whether the
/// transaction wrote it.
fn first_of<'a>(
    written: Option<&'a [u8]>,
    committed: Option<&'a [u8]>,
    back: bool,
) -> Option<(&'a [u8], bool)> {
    match (written, committed) {
        (None, None) => None,
        (Some(key), None) => Some((key, true)),
        (None, Some(key)) => Some((key, false)),
        (Some(written), Some(committed)) => {
            let order = match back {
                false => written.cmp(committed),
                true => committed.cmp(written),
            };
            match order {
                Ordering::Less | Ordering::Equal => Some((written, true)),
                Ordering::Greater => Some((committed, false)),
            }
        }
    }
}

/// Whether `key` has a record in the transaction's view: its own put, or a
/// committed record that none of its deletes covers (RD3).
fn visible(set: &WriteSet, unit: Unit, key: &[u8], written: bool) -> bool {
    match written {
        true => matches!(set.own(unit, key), Own::Put(_) | Own::Node(_)),
        false => !set.hides(unit, key),
    }
}

impl<C: fmt::Debug> fmt::Debug for Cursor<'_, '_, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cursor")
            .field("unit", &self.unit)
            .field("version", &self.pin.version())
            .field("key", &self.key)
            .field("side", &self.side)
            .finish_non_exhaustive()
    }
}
