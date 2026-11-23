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
    key::{Column, Subtree, Unit},
    opened::Opened,
    pages::{child_keys, find},
    path::{FlatReader, Idle, WritePath, IDLE},
    txn::{show_committed, Pinned, Sided, WriteTxn},
    unlinked,
    view::{ChildKeys, FlatKinds, NodeView},
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

    /// [`Self::point`], as a cursor calls it: `asked` hands the side what
    /// the cursor keeps of its version. A side that asks nothing of it is
    /// pointed as by [`Self::point`].
    #[doc(hidden)]
    fn point_asking(
        &mut self,
        asked: Asked<'_, 's>,
        guard: &OperationGuard<'s>,
        under: Subtree,
    ) -> Result<(), TxnError> {
        let _ = asked;
        self.point(guard, under)
    }
}

/// What a cursor hands its committed side where it has the side pointed
/// at another prefix: the cursor's transaction, and what the cursor keeps
/// of the directory of its version.
pub struct Asked<'a, 's> {
    txn: &'a WriteTxn<'s>,
    kept: &'a mut Opened,
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
    /// The flat records of `unit` at the version `at`, which the caller
    /// keeps pinned.
    fn of(path: WritePath<'s>, at: CommitVersion, unit: Unit) -> Self {
        let tree = path.keyspaces().of(unit.column);
        Self {
            path,
            at,
            pages: None,
            cursor: tree.cursor_within(unit.subtree.as_bytes()),
            tree,
            unit,
            key: Vec::new(),
        }
    }

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
/// the cursor is pointed at ([`MerkCursor::retarget`]), and where the
/// side is pointed at the prefix that its version has a subtree under
/// which the transaction moved: the unlinked records of that Merk are
/// read there with its nodes.
///
/// A side that was told which tree types keep their data as flat records
/// ([`FlatKinds`]) shows the records of such a subtree as they are
/// stored, and not as unlinked records: by the tree type that its version
/// has the subtree entered with under the prefix it reads, which it looks
/// at where it is opened and pointed.
pub struct MerkSide<'s> {
    path: WritePath<'s>,
    at: CommitVersion,
    lease: Lease,
    cursor: tree::Cursor,
    /// The prefix the version has the Merk under.
    under: Subtree,
    buffers: LoadBuffers,
    unlinked: Option<UnlinkedSide<'s>>,
    flat: Option<FlatKinds>,
    /// Whether the version has the subtree entered as a tree that keeps
    /// its data as flat records.
    raw: bool,
}

/// Whether `flat` says of the tree type `entered` that a tree of it keeps
/// its data as flat records.
fn keeps_flat(flat: Option<FlatKinds>, entered: Option<&[u8]>) -> bool {
    flat.is_some_and(|flat| entered.is_some_and(flat))
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
        Self {
            records: FlatSide::of(path, at, Unit::data(subtree)),
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
    /// The Merk that the version `at` has under `subtree`, read under
    /// `guard` through the directory in `opened`: its nodes from the root
    /// that version has, and its unlinked records where it holds any. The
    /// caller keeps `at` pinned.
    fn of(
        txn: &WriteTxn<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        lease: Lease,
        opened: &mut Option<Opened>,
        subtree: Subtree,
        flat: Option<FlatKinds>,
    ) -> Result<Self, TxnError> {
        let path = *txn.path();
        let directory = Opened::at(opened, &path, guard, at)?;
        let entered = directory.root(&path, guard, &subtree)?;
        let root = entered.flatten();
        let raw = keeps_flat(flat, directory.tree_type(&path, guard, &subtree)?);
        // Whether the version holds unlinked records under the subtree
        // is asked once, and of the transaction, which may know. No record
        // is read under a subtree that is not entered.
        let records = entered.is_some() && holds_unlinked(txn, directory, guard, at, &subtree)?;
        let unlinked = records.then(|| UnlinkedSide::over(path, at, subtree));
        let reader = NodeReader::new(path.loader, guard, lease, at);
        Ok(Self {
            path,
            at,
            lease,
            cursor: tree::Cursor::open(&reader, root),
            under: subtree,
            buffers: LoadBuffers::new(),
            unlinked,
            flat,
            raw,
        })
    }

    /// The reader of the side's version for the operation that holds
    /// `guard`.
    fn reader<'g>(&self, guard: &'g OperationGuard<'s>) -> NodeReader<'g, OperationGuard<'s>> {
        NodeReader::new(self.path.loader, guard, self.lease, self.at)
    }

    /// Points the side at the Merk that its version has under `subtree`,
    /// whose root is at `root`: the tree, and the unlinked records under
    /// that prefix where `records` says that the Merk holds any. `raw`
    /// says whether the version has the subtree entered as a tree that
    /// keeps its data as flat records. The side then stands nowhere.
    fn open_at(
        &mut self,
        guard: &OperationGuard<'s>,
        subtree: Subtree,
        root: Option<CellAddress>,
        records: bool,
        raw: bool,
    ) {
        self.raw = raw;
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
            if held && self.raw {
                return Ok(read(Some(NodeView::flat(key, &record))));
            }
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
                if self.raw {
                    return Ok(read(Some(NodeView::flat(key, &record))));
                }
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
        let entered = directory.root(&self.path, guard, &under)?;
        // The unlinked records are those under the prefix that the
        // version has the Merk under, and whether it holds any there is
        // looked up for it. No record is read under a subtree that is
        // not entered.
        let records = entered.is_some() && directory.has_unlinked(&self.path, guard, &under)?;
        let raw = keeps_flat(self.flat, directory.tree_type(&self.path, guard, &under)?);
        self.open_at(guard, under, entered.flatten(), records, raw);
        Ok(())
    }

    fn point_asking(
        &mut self,
        asked: Asked<'_, 's>,
        guard: &OperationGuard<'s>,
        under: Subtree,
    ) -> Result<(), TxnError> {
        let Asked { txn, kept } = asked;
        let entered = kept.root(&self.path, guard, &under)?;
        // As in `point`, but the directory is the one that the cursor
        // keeps, and whether the Merk holds unlinked records is asked as
        // where the cursor is opened (`holds_unlinked`).
        let records = entered.is_some() && holds_unlinked(txn, kept, guard, self.at, &under)?;
        let raw = keeps_flat(self.flat, kept.tree_type(&self.path, guard, &under)?);
        self.open_at(guard, under, entered.flatten(), records, raw);
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
    /// The version of the committed side.
    at: CommitVersion,
    /// The pin of that version. A cursor that another makes for the
    /// length of one of its own calls has none: the pin of the other
    /// holds the version for it ([`Self::beside`]).
    _pin: Option<VersionPin<'s>>,
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
    /// The next change of the prefix that the committed side is read
    /// under fails, as a read of the directory does: for a test.
    #[cfg(test)]
    refuse_place: bool,
    /// The next read of a node of the cursor's own version fails, as a
    /// read of a page does: for a test.
    #[cfg(test)]
    refuse_node: bool,
}

impl<'t, 's> Cursor<'t, 's> {
    pub(crate) fn flat(txn: &'t WriteTxn<'s>, who: Who, unit: Unit) -> Self {
        let path = *txn.path();
        // The version is pinned here, and its pages are read from the
        // cursor's first step on, each under the guard of its operation
        // (PN5).
        let pin = path.versions().pin();
        let side = FlatSide::of(path, pin.version(), unit);
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
        // One look at what the transaction holds says what is under the
        // key: its own record, or the record of the cursor's version, and
        // then under which prefix that version has it.
        let guard = self.txn.path().versions().guard();
        let pinned = Pinned {
            opened: &mut self.opened,
            guard: &guard,
            at: self.at,
            seen: self.seen,
            lease: None,
        };
        let side = match (self.txn).cursor_record(self.who, self.unit, key, pinned, out)? {
            Ok(found) => return Ok(found),
            Err(side) => side,
        };
        match self.placed(&guard, side)? {
            true => self.side.value(&guard, key, out),
            false => Ok(false),
        }
    }
}

impl<'t, 's> MerkCursor<'t, 's> {
    pub(crate) fn merk(
        txn: &'t WriteTxn<'s>,
        who: Who,
        subtree: Subtree,
        flat: Option<FlatKinds>,
    ) -> Result<Self, TxnError> {
        // The root is the one the pinned version has, read under a guard
        // (PN5). The lease is the cursor's own (PN1). The transaction pins
        // the version, and dates the lease with it: a write may keep the
        // value of a node the cursor shows, by the node's handle (CF2a).
        let lease = Lease::take();
        let (guard, pin) = txn.pin_shown(lease);
        let at = pin.version();
        let mut opened = None;
        let side = MerkSide::of(txn, &guard, at, lease, &mut opened, subtree, flat)?;
        let mut cursor = Self::over(txn, who, Unit::data(subtree), pin, side);
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
        let raw = keeps_flat(
            self.side.flat,
            directory.tree_type(&path, &guard, &subtree)?,
        );
        self.side.open_at(&guard, subtree, root, records, raw);
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
        // One operation of the transaction says what is under the key:
        // its own node, or the node of the cursor's version, and then
        // under which prefix that version has it.
        let guard = self.txn.path().versions().guard();
        let pinned = Pinned {
            opened: &mut self.opened,
            guard: &guard,
            at: self.at,
            seen: self.seen,
            lease: Some(self.side.lease),
        };
        let own = children.as_deref_mut();
        let flat = self.side.flat;
        let shown = (self.txn).cursor_node(self.who, (subtree, &key), own, pinned, flat, read)?;
        let (read, side) = match shown {
            Ok(shown) => return Ok(shown),
            Err(rest) => rest,
        };
        // The transaction's lock is not held over the cursor's own read,
        // and another thread may have ended the transaction, or had it
        // forget the lease, meanwhile. So the cursor tells the
        // transaction of a node before `read` is given it, whose handle
        // another thread can use from then on, and says what it read
        // when the read ends, however it ends: the transaction decides
        // each time by what it holds then.
        let mut told = Told {
            txn: self.txn,
            lease: self.side.lease,
            at: self.side.at,
            found: false,
        };
        let shown = self.placed(&guard, side).and_then(|reads| match reads {
            #[cfg(test)]
            true if std::mem::take(&mut self.refuse_node) => {
                Err(TxnError::Damaged("a read of a node that a test refuses"))
            }
            true => self.side.node(&guard, &key, children, |view| {
                // An unlinked record has no handle for a write to name:
                // it dates nothing, as a read that finds no node.
                if view.as_ref().is_some_and(|view| view.handle().is_some()) {
                    told.shown();
                }
                read(view)
            }),
            false => Ok(read(None)),
        });
        drop(told);
        shown
    }
}

/// The read of a node that a cursor makes of its own version, once the
/// transaction has handed it back (`WriteTxn::cursor_node`): the
/// transaction is told of a node with a handle that is found, before the
/// caller is given it (`WriteTxn::cursor_shown`), and that the read
/// ended, and whether such a node was found, when this goes, however the
/// read ends (`WriteTxn::cursor_read`). An unlinked record has no handle
/// ([`NodeView::handle`]): it is told as a read that found no node.
struct Told<'t, 's> {
    txn: &'t WriteTxn<'s>,
    lease: Lease,
    at: CommitVersion,
    found: bool,
}

impl Told<'_, '_> {
    /// A node with a handle is found, and the caller is about to be given
    /// it.
    fn shown(&mut self) {
        self.found = true;
        (self.txn).cursor_shown(self.lease, self.at);
    }
}

impl Drop for Told<'_, '_> {
    fn drop(&mut self) {
        (self.txn).cursor_read(self.lease, self.at, self.found);
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

    /// [`Cursor::beside`].
    pub fn beside(&mut self, back: bool, limit: usize) -> Result<usize, TxnError> {
        self.cursor.beside(back, limit)
    }

    /// [`Cursor::beside_with`].
    pub fn beside_with(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, TxnError> {
        self.cursor.beside_with(back, limit, stand_ins)
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

/// Keys of one unit that each stand for a record beside a cursor's
/// subtree ([`Cursor::beside_with`]). An engine that keeps, under one
/// subtree, what stands for records of other subtrees names them so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandIns<'a> {
    /// The unit that holds the keys.
    pub unit: Unit,
    /// The first key that is counted, if the unit holds it.
    pub low: &'a [u8],
    /// The key that every counted key is below.
    pub high: &'a [u8],
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
        let at = pin.version();
        Self::at(txn, who, unit, at, Some(pin), side)
    }

    fn at(
        txn: &'t WriteTxn<'s>,
        who: Who,
        unit: Unit,
        at: CommitVersion,
        pin: Option<VersionPin<'s>>,
        side: C,
    ) -> Self {
        Self {
            txn,
            who,
            unit,
            at,
            _pin: pin,
            side,
            opened: None,
            seen: None,
            key: None,
            way: Way::Forward,
            #[cfg(test)]
            refuse_place: false,
            #[cfg(test)]
            refuse_node: false,
        }
    }

    /// Has the next read of a node of the cursor's own version fail, as a
    /// read of a page does.
    #[cfg(test)]
    pub(crate) fn refuse_node(&mut self) {
        self.refuse_node = true;
    }

    /// Has the next change of the prefix that the committed side is read
    /// under fail, as a read of the directory does.
    #[cfg(test)]
    pub(crate) fn refuse_place(&mut self) {
        self.refuse_place = true;
    }

    /// How many records the cursor's column holds in the subtrees after
    /// the cursor's own in the order of their prefixes, or with `back` in
    /// those before it, counted from the nearest subtree on and up to
    /// `limit`. The nodes of a Merk and the flat records of the data
    /// column are both records of that column (`transactions.md` ST1).
    ///
    /// They are the records that a cursor of each of those subtrees shows,
    /// opened at this cursor's version: the committed records of that
    /// version, with the transaction's writes as they are now (RD6). The
    /// cursor stays where it is.
    ///
    /// It is what tells where a step from past the end of a subtree leads
    /// on an engine that keeps the records of every subtree of a column
    /// in one keyspace (`flat-store.md`, rule 6).
    ///
    /// For each subtree it comes to, the call seeks once in the column's
    /// tree, which finds the nearest subtree the version stores a record
    /// under. For the data column it also walks the entries of the
    /// subtree directory to the nearest Merk with a node, and no further
    /// than the subtree the seek found or one the transaction wrote
    /// ([`Directory::rooted_beside`](lignin_tree::directory::Directory::rooted_beside)).
    /// It then steps over the records of the nearest of these until it
    /// has counted `limit`. A subtree that holds records which nobody is
    /// shown (those a drop left, and those the transaction deleted) is
    /// passed, at the same cost.
    ///
    /// Each subtree is read as a cursor of this cursor's scope and
    /// version reads it, and is refused where such a cursor is: with
    /// [`TxnError::ScopeOverlap`] where another open scope holds that
    /// subtree's unit for writing (SC3).
    pub fn beside(&mut self, back: bool, limit: usize) -> Result<usize, TxnError> {
        self.beside_with(back, limit, None)
    }

    /// As [`Self::beside`], for an engine that keeps, as keys of one unit,
    /// what stands for records of other subtrees (`stand_ins`).
    ///
    /// The records of that unit are counted as records of no subtree:
    /// where the unit is of the cursor's column, its subtree is passed
    /// as one that holds no record. Each key of the unit from
    /// [`StandIns::low`] on and below [`StandIns::high`] is counted as
    /// one record beside the cursor's subtree, whichever way is asked:
    /// the caller names the keys that stand for records that way. They
    /// are counted after the records of the subtrees, up to `limit` with
    /// them, and are the keys that a cursor of the unit shows which is
    /// opened at this cursor's version, as the subtrees are read. The
    /// unit is not read where the subtrees fill the limit. Where they do
    /// not, it is one seek in the unit's column, and steps over its keys
    /// until the limit is reached or the range ends.
    pub fn beside_with(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, TxnError> {
        let column = self.unit.column;
        let apart = stand_ins
            .filter(|stand_ins| stand_ins.unit.column == column)
            .map(|stand_ins| stand_ins.unit.subtree);
        let (mut from, mut found) = (self.unit.subtree, 0);
        while found < limit {
            let Some(next) = self.neighbour(&from, back)? else {
                break;
            };
            if Some(next) != apart {
                found += self.count(Unit::new(column, next), None, limit - found)?;
            }
            from = next;
        }
        if let Some(stand_ins) = stand_ins.filter(|_| found < limit) {
            let range = Some((stand_ins.low, stand_ins.high));
            found += self.count(stand_ins.unit, range, limit - found)?;
        }
        Ok(found)
    }

    /// The nearest subtree after `from`, or with `back` before it, that
    /// may hold a record of the cursor's column in the cursor's view: one
    /// the cursor's version stores a record under, one the transaction
    /// has written or moved, and for the data column one whose Merk has a
    /// node at that version.
    fn neighbour(&mut self, from: &Subtree, back: bool) -> Result<Option<Subtree>, TxnError> {
        let (path, column) = (*self.txn.path(), self.unit.column);
        let nearest = |found: [Option<Subtree>; 2]| {
            let found = found.into_iter().flatten();
            match back {
                false => found.min(),
                true => found.max(),
            }
        };
        let guard = path.versions().guard();
        let pages = path.committed(&guard, self.at);
        let stored = (path.keyspaces()).stored_beside(&pages, column, from, back)?;
        let near = nearest([stored, self.txn.written_beside(from, back)]);
        if column != Column::Data {
            return Ok(near);
        }
        let directory = Opened::at(&mut self.opened, &path, &guard, self.at)?;
        let rooted = directory.rooted_beside(&path, &guard, from, near.as_ref(), back)?;
        Ok(nearest([near, rooted]))
    }

    /// How many records of `unit` a cursor of this cursor's version shows,
    /// up to `limit`: for the data column a cursor over the nodes of a
    /// Merk, which shows the flat records of that column among them. With
    /// `range`, those whose key is at or after its first key and below
    /// its second.
    fn count(
        &mut self,
        unit: Unit,
        range: Option<(&[u8], &[u8])>,
        limit: usize,
    ) -> Result<usize, TxnError> {
        let (txn, who, at) = (self.txn, self.who, self.at);
        let path = *txn.path();
        if unit.column != Column::Data {
            let side = FlatSide::of(path, at, unit);
            let mut records = Cursor::at(txn, who, unit, at, None, side);
            records.opened = self.opened.take();
            let counted = records.count_keys(range, limit);
            self.opened = records.opened.take();
            return counted;
        }
        let guard = path.versions().guard();
        let lease = Lease::take();
        let side = MerkSide::of(txn, &guard, at, lease, &mut self.opened, unit.subtree, None)?;
        drop(guard);
        let mut nodes = Cursor::at(txn, who, unit, at, None, side);
        nodes.opened = self.opened.take();
        let counted = nodes.count_keys(range, limit);
        self.opened = nodes.opened.take();
        counted
    }

    /// Walks the cursor from its first key and counts the keys, up to
    /// `limit`. With `range`, from the first key at or after its first
    /// key, and the keys below its second.
    fn count_keys(
        &mut self,
        range: Option<(&[u8], &[u8])>,
        limit: usize,
    ) -> Result<usize, TxnError> {
        let mut found = 0;
        match range {
            Some((low, _)) => self.seek(low)?,
            None => self.seek_to_first()?,
        }
        let within = |key: &Option<Vec<u8>>| match (key, range) {
            (Some(key), Some((_, high))) => key.as_slice() < high,
            (Some(_), None) => true,
            (None, _) => false,
        };
        while within(&self.key) && found < limit {
            found += 1;
            if found < limit {
                self.next()?;
            }
        }
        Ok(found)
    }

    /// Whether the committed side is read: the cursor's version has the
    /// subtree entered, and the transaction has not taken it with a
    /// subtree that it dropped, or with a Merk that it cleared
    /// (`transactions.md` CV9). No record is read under a subtree without
    /// its entry. `WriteTxn::committed_side` says by what that is
    /// decided.
    ///
    /// The subtree is opened once, and asked for again whenever the
    /// transaction has dropped or cleared a subtree since, or has taken
    /// such a write back.
    ///
    /// Where the transaction moved the subtree, the committed side is the
    /// records that the cursor's version has under the prefix the subtree
    /// was taken from, where the transaction's own writes left the Merk
    /// the one that version has there, and never where a move took a
    /// subtree from the cursor's prefix and brought none. A subtree that
    /// no move names and that leaves with one is not read
    /// ([`Opened::leaves_with_a_move`]).
    /// Whoever has no frames that show the moves over the cursor's
    /// version is refused where a move decides what is under the subtree,
    /// by the directory of that version (`WriteTxn::cursor_side`): one
    /// that is answered from the write set, and a cursor that was opened
    /// before a commit which the transaction has taken in since.
    fn reads(&mut self, guard: &OperationGuard<'s>) -> Result<bool, TxnError> {
        let pinned = Pinned {
            opened: &mut self.opened,
            guard,
            at: self.at,
            seen: self.seen,
            lease: None,
        };
        let side = (self.txn).cursor_side(self.who, self.unit, pinned)?;
        self.placed(guard, side)
    }

    /// Takes in what the transaction said of the committed side: the
    /// side is pointed at the prefix it is read under, and says whether
    /// it is read.
    fn placed(&mut self, guard: &OperationGuard<'s>, side: Sided) -> Result<bool, TxnError> {
        let Sided { left, reads, under } = side;
        if let Some(under) = under.filter(|under| *under != self.side.under()) {
            #[cfg(test)]
            if std::mem::take(&mut self.refuse_place) {
                return Err(TxnError::Damaged("a change of prefix that a test refuses"));
            }
            let path = *self.txn.path();
            let kept = Opened::at(&mut self.opened, &path, guard, self.at)?;
            let txn = self.txn;
            self.side.point_asking(Asked { txn, kept }, guard, under)?;
        }
        if self.seen != Some((left, reads)) {
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
        self.at
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
            .field("version", &self.at)
            .field("key", &self.key)
            .field("side", &self.side)
            .finish_non_exhaustive()
    }
}
