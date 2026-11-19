//! A cursor through a write transaction: the committed records of the
//! moment it was opened, merged with the transaction's own writes.

use std::{cmp::Ordering, fmt, ops::Bound};

use lignin_format::CommitVersion;
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
    pages::find,
    path::{FlatReader, Idle, WritePath, IDLE},
    txn::{show_committed, WriteTxn},
    view::NodeView,
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
pub struct MerkSide<'s> {
    path: WritePath<'s>,
    at: CommitVersion,
    lease: Lease,
    cursor: tree::Cursor,
    buffers: LoadBuffers,
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

    /// Shows `read` the committed node under `key`, or `None` if the
    /// version has none. `key` is mostly the key the side stands on.
    fn node<R>(
        &mut self,
        guard: &OperationGuard<'s>,
        key: &[u8],
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        if self.cursor.key() == Some(key) {
            let reader = self.reader(guard);
            let node = self.cursor.node(&reader, &mut self.buffers)?;
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
        show_committed(&mut pages, found, read)
    }
}

impl<'s> CommittedSide<'s> for MerkSide<'s> {
    fn seek(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek(&reader, &mut self.buffers, key)?;
        Ok(())
    }

    fn seek_for_prev(&mut self, guard: &OperationGuard<'s>, key: &[u8]) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek_for_prev(&reader, &mut self.buffers, key)?;
        Ok(())
    }

    fn seek_to_first(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek_to_first(&reader, &mut self.buffers)?;
        Ok(())
    }

    fn seek_to_last(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.seek_to_last(&reader, &mut self.buffers)?;
        Ok(())
    }

    fn next(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.next(&reader, &mut self.buffers)?;
        Ok(())
    }

    fn prev(&mut self, guard: &OperationGuard<'s>) -> Result<(), TxnError> {
        let reader = self.reader(guard);
        self.cursor.prev(&reader, &mut self.buffers)?;
        Ok(())
    }

    fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }
}

/// A cursor through a write transaction over the nodes of one Merk.
pub type MerkCursor<'t, 's> = Cursor<'t, 's, MerkSide<'s>>;

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
        // (PN5). The lease is the cursor's own (PN1).
        let guard = path.versions().guard();
        let pin = path.versions().pin();
        let at = pin.version();
        let mut opened = None;
        let directory = Opened::at(&mut opened, &path, &guard, at)?;
        let root = directory.root(&path, &guard, &subtree)?.flatten();
        let lease = Lease::take();
        let reader = NodeReader::new(path.loader, &guard, lease, at);
        let side = MerkSide {
            path,
            at,
            lease,
            cursor: tree::Cursor::open(&reader, root),
            buffers: LoadBuffers::new(),
        };
        let mut cursor = Self::over(txn, who, Unit::data(subtree), pin, side);
        cursor.opened = opened;
        Ok(cursor)
    }

    /// Shows `read` the node under the key the cursor stands on: the
    /// transaction's own node if it has written the key, and otherwise the
    /// node of the cursor's version. `None` if the cursor stands on no
    /// key, or if the transaction has deleted the node since the cursor
    /// came to it.
    ///
    /// `read` runs inside the operation: it must not call the transaction.
    pub fn node<R>(&mut self, read: impl FnOnce(Option<NodeView<'_>>) -> R) -> Result<R, TxnError> {
        let Some(key) = &self.key else {
            return Ok(read(None));
        };
        let key = key.clone();
        let subtree = self.unit.subtree;
        let read = match (self.txn).own_node_in(self.who, subtree, &key, read)? {
            Ok(shown) => return Ok(shown),
            Err(read) => read,
        };
        let guard = self.txn.path().versions().guard();
        match self.reads(&guard)? {
            true => self.side.node(&guard, &key, read),
            false => Ok(read(None)),
        }
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
    fn reads(&mut self, guard: &OperationGuard<'s>) -> Result<bool, TxnError> {
        let (path, at, subtree) = (*self.txn.path(), self.pin.version(), self.unit.subtree);
        let (opened, seen) = (&mut self.opened, self.seen);
        let (left, reads) = self.txn.with_set(self.who, self.unit, |set| {
            if let Some((_, reads)) = seen.filter(|(left, _)| *left == set.left()) {
                return Ok((set.left(), reads));
            }
            let opened = Opened::at(opened, &path, guard, at)?;
            let reads = opened.seen(&path, guard, set, &subtree)?.is_some();
            Ok::<_, TxnError>((set.left(), reads))
        })??;
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
