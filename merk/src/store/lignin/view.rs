//! What a handle of the store is opened on: a write transaction, one
//! writer's scope of it, or a snapshot.
//!
//! | View | Reads | Writes |
//! | --- | --- | --- |
//! | a write transaction (`WriteTxn`) | its own writes, and otherwise the latest committed state at each read (`transactions.md` RD3), from outside every scope | through a scope that is opened for the one write and merged when it is made |
//! | a writer ([`LigninWriter`]) | as its scope reads: its own writes, then the transaction's, then the committed state | through its scope |
//! | a snapshot ([`LigninSnapshot`]) | the one version it pinned (RD1) | refused, with `SnapshotReadOnlyTransaction` |

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use grovedb_storage::Error as StorageError;
use lignindb::{
    snapshot::{Kept, NodeWalk, Opened, Operation, ReadError, RecordWalk, Snapshot},
    txn::{ChildKeys, Column, StandIns, Subtree, Unit, WriteTxn},
};

use super::{
    access::{
        column, stored_node, with_child_keys, Access, BatchPart, Child, LigninEntryRefused,
        Location, Prefix, Seek, StoredNode, SubtreeChange, Walk, Write, Written,
    },
    directory::keeps_flat_records,
    flat::{FlatAccess, FlatWalk},
    refused, LigninEntry, LigninHost, LigninHosted, LigninWriter,
};
use crate::{cost_model::Keyspace, Error};

/// What a handle reads and writes through: a write transaction, a writer
/// of one, or a snapshot. It is made of a reference to one of them.
#[derive(Clone, Copy)]
pub struct LigninView<'a>(pub(super) &'a (dyn Access + 'a));

impl std::fmt::Debug for LigninView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LigninView").finish_non_exhaustive()
    }
}

impl<'a> From<&'a WriteTxn<'_>> for LigninView<'a> {
    fn from(transaction: &'a WriteTxn<'_>) -> Self {
        Self(transaction)
    }
}

impl<'a> From<&'a LigninWriter<'_, '_>> for LigninView<'a> {
    fn from(writer: &'a LigninWriter<'_, '_>) -> Self {
        Self(writer)
    }
}

impl<'a> From<&'a LigninSnapshot<'_>> for LigninView<'a> {
    fn from(snapshot: &'a LigninSnapshot<'_>) -> Self {
        Self(snapshot)
    }
}

impl LigninView<'_> {
    /// What the subtree directory holds of the subtree `prefix` as the
    /// view shows it, or `None` if the subtree is not entered.
    ///
    /// It is the read of
    /// [`LigninNodeStore::entry`](super::LigninNodeStore::entry) of a
    /// handle of that subtree on this view, in any of the three modes,
    /// with the same answer and the same refusal, and no handle is made
    /// for it: a caller that asks only whether a subtree is entered, or
    /// what hosts it, can ask the view.
    pub fn entry(&self, prefix: &[u8; 32]) -> Result<Option<LigninEntry>, Error> {
        self.0.subtree(prefix)
    }
}

/// How many subtrees a thread remembers ([`LAST_OPENED`]).
const REMEMBERED: usize = 32;

thread_local! {
    /// What this thread last kept of the subtrees it opened on a snapshot,
    /// each in the place that the first byte of its prefix names: the
    /// entry of the subtree at one version of one database.
    ///
    /// A read with no transaction is made on a snapshot of its own, and a
    /// thread that reads one subtree again and again, as the resolution of
    /// references into one tree does, would look its entry up for every
    /// read. A snapshot's first open of a subtree that the thread
    /// remembers resumes from here instead (`Operation::resume`, which
    /// says what a resume reads and when it looks the subtree up), and
    /// one of a subtree it does not remember looks the subtree up. What
    /// is remembered holds no version, no page and no database open: for
    /// each of a thread's 32 places, the bytes of one entry.
    static LAST_OPENED: RefCell<[Option<Kept>; REMEMBERED]> =
        const { RefCell::new([const { None }; REMEMBERED]) };
}

/// The place of `subtree` among the subtrees a thread remembers.
fn place(subtree: &Prefix) -> usize {
    usize::from(subtree[0]) % REMEMBERED
}

/// What the thread last kept of `subtree`, if it remembers the subtree.
pub(super) fn last_opened(subtree: &Prefix) -> Option<Kept> {
    let remembered = LAST_OPENED.try_with(|last| {
        let last = last.try_borrow().ok()?;
        let kept = last[place(subtree)].as_ref();
        kept.filter(|kept| kept.subtree().as_bytes() == subtree)
            .cloned()
    });
    remembered.ok().flatten()
}

/// Remembers `kept` as what the thread last kept of `subtree`.
fn remember(subtree: &Prefix, kept: &Kept) {
    let _ = LAST_OPENED.try_with(|last| {
        if let Ok(mut last) = last.try_borrow_mut() {
            last[place(subtree)] = Some(kept.clone());
        }
    });
}

/// A snapshot of a LigninDB database as the store reads it: one committed
/// version, pinned for as long as this lives (`transactions.md` RD1).
///
/// Every read through it is one operation of the snapshot. The entry of
/// a subtree that was looked up in the subtree directory is kept, and the
/// reads that follow open the subtree from the kept entry: a snapshot's
/// version does not change. The first open of a subtree takes the entry
/// that the thread last kept of it, through this snapshot or another,
/// where that was kept at the snapshot's version of the same database,
/// and looks the subtree up otherwise. A node that is a cell of the
/// Merk's tree is read with where the cell is, and with where the cell
/// of each child is whose key the read took from that cell, under the
/// snapshot's lease. A read that is given such a location gives it to
/// the engine, which follows it or searches by key
/// (`Opened::follow_by_kind` of `lignindb` says when). A build that holds
/// its reads (`lignindb/held_reads`) makes the lookup and the search
/// beside every entry and location it takes this way. A write through it
/// is refused with `SnapshotReadOnlyTransaction`.
pub struct LigninSnapshot<'s> {
    snapshot: Snapshot<'s>,
    /// The subtrees that were opened, each with its entry, or with none
    /// where the version has no subtree under the prefix.
    opened: Mutex<HashMap<Prefix, Option<Kept>>>,
}

impl std::fmt::Debug for LigninSnapshot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LigninSnapshot")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

fn unread(error: ReadError) -> Error {
    refused(&error.to_string())
}

fn read_only(what: &'static str) -> Error {
    Error::StorageError(StorageError::SnapshotReadOnlyTransaction(what))
}

impl<'s> LigninSnapshot<'s> {
    /// The store's view of `snapshot`.
    pub fn new(snapshot: Snapshot<'s>) -> Self {
        Self {
            snapshot,
            opened: Mutex::default(),
        }
    }

    /// The snapshot.
    pub fn snapshot(&self) -> &Snapshot<'s> {
        &self.snapshot
    }

    /// Opens `subtree` in `operation`, an operation of this snapshot:
    /// from its kept entry, from the entry the thread last kept of the
    /// subtree at this version, or by one lookup in the directory.
    fn open<'a, 'o>(
        &self,
        operation: &'a Operation<'o>,
        subtree: &Prefix,
    ) -> Result<Option<Opened<'a, 'o>>, Error> {
        let kept = {
            let opened = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
            opened.get(subtree).cloned()
        };
        match kept {
            Some(None) => Ok(None),
            Some(Some(kept)) => operation.resume(&kept).map_err(unread),
            None => {
                let found = match last_opened(subtree) {
                    Some(last) => operation.resume(&last),
                    None => operation.open(Subtree::new(*subtree)),
                };
                let kept = found.map_err(unread)?.map(|found| found.keep());
                if let Some(kept) = &kept {
                    remember(subtree, kept);
                }
                // Of two first opens of one subtree, the entry that was
                // kept first is the one both read through.
                let kept = {
                    let mut all = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
                    all.entry(*subtree).or_insert(kept).clone()
                };
                match &kept {
                    Some(kept) => operation.resume(kept).map_err(unread),
                    None => Ok(None),
                }
            }
        }
    }

    /// Reads in `subtree`, in an operation of its own. A subtree that is
    /// not entered at the snapshot's version has nothing to read.
    fn reading<T>(
        &self,
        subtree: &Prefix,
        read: impl FnOnce(&Opened<'_, '_>) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        let operation = self.snapshot.operation();
        match self.open(&operation, subtree)? {
            Some(opened) => read(&opened),
            None => Ok(None),
        }
    }
}

fn node_of(
    opened: &Opened<'_, '_>,
    key: &[u8],
    hint: Option<Location>,
    with_value: bool,
) -> Result<Option<StoredNode>, Error> {
    let mut children = ChildKeys::new();
    let hint = hint.map(Location::handle);
    let node = opened
        .follow_by_kind(key, hint, Some(&mut children), keeps_flat_records, |view| {
            view.map(|view| stored_node(&view, with_value)).transpose()
        })
        .map_err(unread)??;
    node.map(|node| with_child_keys(node, &mut children))
        .transpose()
}

macro_rules! walk_to {
    ($walk:expr, $operation:expr, $to:expr) => {
        match $to {
            Seek::First => $walk.seek_to_first($operation),
            Seek::Last => $walk.seek_to_last($operation),
            Seek::From(key) => $walk.seek($operation, key),
            Seek::Back(key) => $walk.seek_for_prev($operation, key),
            Seek::Next => $walk.next($operation),
            Seek::Prev => $walk.prev($operation),
        }
        .map_err(unread)
    };
}

/// The cursor of a snapshot over the nodes of one Merk. With no walk the
/// subtree is not entered at the snapshot's version, and has no node.
struct SnapshotNodes<'a, 's> {
    snapshot: &'a LigninSnapshot<'s>,
    subtree: Subtree,
    walk: Option<NodeWalk>,
}

impl Walk for SnapshotNodes<'_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        key.clear();
        let Some(walk) = &mut self.walk else {
            return Ok(false);
        };
        let operation = self.snapshot.snapshot.operation();
        walk_to!(walk, &operation, to)?;
        key.extend_from_slice(walk.key().unwrap_or_default());
        Ok(walk.key().is_some())
    }

    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error> {
        let operation = self.snapshot.snapshot.operation();
        let beside = operation.beside_with(Column::Data, self.subtree, back, limit, stand_ins);
        beside.map_err(unread)
    }

    fn node(&mut self) -> Result<Option<StoredNode>, Error> {
        let Some(walk) = &mut self.walk else {
            return Ok(None);
        };
        let operation = self.snapshot.snapshot.operation();
        let mut children = ChildKeys::new();
        let node = walk
            .node_with_children(&operation, &mut children, |view| {
                view.map(|view| stored_node(&view, true)).transpose()
            })
            .map_err(unread)??;
        node.map(|node| with_child_keys(node, &mut children))
            .transpose()
    }

    fn retarget(&mut self, subtree: &Prefix) -> Result<(), Error> {
        self.walk = (self.snapshot).reading(subtree, |opened| {
            let walk = opened.walk_by_kind(keeps_flat_records);
            walk.map(Some).map_err(unread)
        })?;
        self.subtree = Subtree::new(*subtree);
        Ok(())
    }
}

/// The cursor of a snapshot over the flat records of one column of a
/// subtree.
struct SnapshotRecords<'a, 's> {
    snapshot: &'a Snapshot<'s>,
    unit: Unit,
    walk: Option<RecordWalk>,
}

impl FlatWalk for SnapshotRecords<'_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        key.clear();
        let Some(walk) = &mut self.walk else {
            return Ok(false);
        };
        let operation = self.snapshot.operation();
        walk_to!(walk, &operation, to)?;
        key.extend_from_slice(walk.key().unwrap_or_default());
        Ok(walk.key().is_some())
    }

    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error> {
        let (operation, unit) = (self.snapshot.operation(), self.unit);
        let beside = operation.beside_with(unit.column, unit.subtree, back, limit, stand_ins);
        beside.map_err(unread)
    }

    fn value(&mut self, out: &mut Vec<u8>) -> Result<bool, Error> {
        let Some(walk) = &mut self.walk else {
            return Ok(false);
        };
        let operation = self.snapshot.operation();
        walk.value(&operation, out).map_err(unread)
    }
}

impl FlatAccess for LigninSnapshot<'_> {
    fn records<'a>(
        &'a self,
        keyspace: Keyspace,
        subtree: &Prefix,
    ) -> Result<Box<dyn FlatWalk + 'a>, Error> {
        let walk = self.reading(subtree, |opened| {
            Ok(Some(opened.record_walk(column(keyspace))))
        })?;
        Ok(Box::new(SnapshotRecords {
            snapshot: &self.snapshot,
            unit: Unit::new(column(keyspace), Subtree::new(*subtree)),
            walk,
        }))
    }
}

impl Access for LigninSnapshot<'_> {
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error> {
        let walk = self.reading(subtree, |opened| {
            let walk = opened.walk_by_kind(keeps_flat_records);
            walk.map(Some).map_err(unread)
        })?;
        Ok(Box::new(SnapshotNodes {
            snapshot: self,
            subtree: Subtree::new(*subtree),
            walk,
        }))
    }

    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error> {
        self.reading(subtree, |opened| node_of(opened, key, hint, with_value))
    }

    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        self.reading(subtree, |opened| node_of(opened, key, None, false))
    }

    fn child(&self, _subtree: &Prefix, _parent: Location, _left: bool) -> Result<Child, Error> {
        // A node of a snapshot is read with the key of each child.
        Err(refused(
            "a child is not read apart from its parent in a snapshot",
        ))
    }

    fn subtree(&self, subtree: &Prefix) -> Result<Option<LigninEntry>, Error> {
        self.reading(subtree, |opened| {
            let entry = opened.entry();
            Ok(Some(LigninEntry {
                root_key: entry.root().map(|root| root.key.to_vec()),
                tree_type: entry.tree_type().to_vec(),
                host: entry.host().map(|host| LigninHosted {
                    subtree: *host.parent,
                    key: host.key.to_vec(),
                    position: host.position,
                }),
            }))
        })
    }

    fn record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.reading(subtree, |opened| {
            let mut value = Vec::new();
            let found = opened.get(column(keyspace), key, &mut value);
            Ok(found.map_err(unread)?.then_some(value))
        })
    }

    fn committed_record(
        &self,
        keyspace: Keyspace,
        subtree: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.record(keyspace, subtree, key)
    }

    fn apply(&self, _write: &Write) -> Result<Written, Error> {
        Err(read_only("a write"))
    }

    fn apply_parts(
        &self,
        _parts: &[&dyn BatchPart],
        _enters: &[(usize, SubtreeChange)],
        _entry: &mut Option<LigninEntryRefused>,
    ) -> Result<Vec<Written>, Error> {
        Err(read_only("the application of a batch"))
    }

    fn name_hosted(&self, _hosts: &[LigninHost]) -> Result<(), Error> {
        Err(read_only("the naming of a Merk in its host"))
    }
}
