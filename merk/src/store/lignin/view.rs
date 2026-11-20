//! What a handle of the store is opened on: a write transaction, one
//! writer's scope of it, or a snapshot.
//!
//! | View | Reads | Writes |
//! | --- | --- | --- |
//! | a write transaction (`WriteTxn`) | its own writes, and otherwise the latest committed state at each read (`transactions.md` RD3), from outside every scope | through a scope that is opened for the one write and merged when it is made |
//! | a writer ([`LigninWriter`]) | as its scope reads: its own writes, then the transaction's, then the committed state | through its scope |
//! | a snapshot ([`LigninSnapshot`]) | the one version it pinned (RD1) | refused, with `SnapshotReadOnlyTransaction` |

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use grovedb_storage::Error as StorageError;
use lignindb::{
    snapshot::{Kept, NodeWalk, Opened, Operation, ReadError, RecordWalk, Snapshot},
    txn::{ChildKeys, Subtree, WriteTxn},
};

use super::{
    access::{
        column, stored_node, with_child_keys, Access, BatchPart, Child, Location, Prefix, Seek,
        StoredNode, SubtreeChange, Walk, Write, Written,
    },
    flat::{FlatAccess, FlatWalk},
    refused, LigninEntry, LigninHosted, LigninWriter,
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

/// A snapshot of a LigninDB database as the store reads it: one committed
/// version, pinned for as long as this lives (`transactions.md` RD1).
///
/// Every read through it is one operation of the snapshot. A subtree is
/// looked up in the subtree directory once, and its entry is kept for the
/// reads that follow: a snapshot's version does not change. A write
/// through it is refused with `SnapshotReadOnlyTransaction`.
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
    /// from its kept entry, or by one lookup in the directory.
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
                let found = operation.open(Subtree::new(*subtree)).map_err(unread)?;
                let kept = found.map(|found| found.keep());
                let opened = match &kept {
                    Some(kept) => operation.resume(kept).map_err(unread)?,
                    None => None,
                };
                let mut all = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
                all.insert(*subtree, kept);
                Ok(opened)
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
    with_value: bool,
) -> Result<Option<StoredNode>, Error> {
    let mut children = ChildKeys::new();
    let node = opened
        .node(key, Some(&mut children), |view| {
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
        self.walk = (self.snapshot).reading(subtree, |opened| Ok(Some(opened.walk())))?;
        Ok(())
    }
}

/// The cursor of a snapshot over the flat records of one column of a
/// subtree.
struct SnapshotRecords<'a, 's> {
    snapshot: &'a Snapshot<'s>,
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
            walk,
        }))
    }
}

impl Access for LigninSnapshot<'_> {
    fn cursor<'a>(&'a self, subtree: &Prefix) -> Result<Box<dyn Walk + 'a>, Error> {
        let walk = self.reading(subtree, |opened| Ok(Some(opened.walk())))?;
        Ok(Box::new(SnapshotNodes {
            snapshot: self,
            walk,
        }))
    }

    fn node(
        &self,
        subtree: &Prefix,
        key: &[u8],
        _hint: Option<Location>,
        with_value: bool,
    ) -> Result<Option<StoredNode>, Error> {
        self.reading(subtree, |opened| node_of(opened, key, with_value))
    }

    fn committed_node(&self, subtree: &Prefix, key: &[u8]) -> Result<Option<StoredNode>, Error> {
        self.reading(subtree, |opened| node_of(opened, key, false))
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
    ) -> Result<Vec<Written>, Error> {
        Err(read_only("the application of a batch"))
    }
}
