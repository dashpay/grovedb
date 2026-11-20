//! The LigninDB engine: `lignindb` behind the [`Engine`] seam.
//!
//! [`LigninEngine`] is one open LigninDB database ([`lignindb::Db`]). A
//! subtree handle is the node store of Merk on it
//! (`grovedb_merk::store::lignin`), and the batch across subtrees is that
//! store's batch. `docs/lignindb/engine.md`, "The LigninDB engine", says
//! how each operation of the seam is met and which choices were made
//! here.
//!
//! # Transactions
//!
//! | Started by | It is | Reads | Writes |
//! | --- | --- | --- | --- |
//! | `start_transaction` | a write transaction of the database | its own writes, and otherwise the latest committed state at each read (`transactions.md` RD3) | applied through a scope that is opened for the one write and merged when it is made |
//! | `start_snapshot_read_transaction`, `start_read_transaction` | a snapshot | the version that was the latest when it started (RD1, RD2) | refused with `SnapshotReadOnlyTransaction` |
//! | `scope_transaction` | the view of an open write scope | as the scope reads | through the scope |
//!
//! A write with no transaction is made by `GroveDb` as a write
//! transaction of its own, which it commits at once (RD7). Any number of
//! write transactions are open and one commits at a time. A commit fails
//! with `CommitError::Conflict` exactly when a commit outside the
//! transaction wrote a key after the transaction first wrote it (CF2),
//! which is the rule of RocksDB's optimistic transactions.
//!
//! # Subtrees
//!
//! LigninDB reads and writes no record of a subtree that its subtree
//! directory has no entry for. GroveDB does not say which subtrees an
//! operation makes, so the engine says it where a batch is applied
//! ([`enter`]): a subtree that the batch writes into, and that the
//! transaction does not hold as entered, is entered with the batch, under
//! the element at the last segment of the path its handle was opened by.
//! A tree that nothing was ever written into is therefore in no directory,
//! and reads as the empty tree it is.
//!
//! # What it does not do yet
//!
//! Each is refused with `Error::NotSupported` and changes nothing: a
//! checkpoint of an open database (GROVE-08), the move of a subtree
//! (GROVE-03), a write into an indexed tree, whose element hosts more
//! than one Merk (GROVE-04), and the removal of the records that a flat
//! drop left (GROVE-05). The delete of a tree that holds elements is
//! refused by its handle (`FlatStore::clear` of the node store), and a
//! subtree is not dropped when its element goes (GROVE-05).

mod enter;
mod error;
#[cfg(test)]
mod tests;

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_merk::{
    store::lignin::{
        tree_type_bytes, LigninApplied, LigninBatch, LigninCursor, LigninHost, LigninKeyspace,
        LigninNodeStore, LigninSettled, LigninSnapshot, LigninSubtree, LigninView, LigninWriter,
    },
    TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    error::WriteScopeError,
    flat::{
        pending_prefix_drops_namespace, PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix,
    },
    prefix::build_prefix,
    Error, RawIterator,
};
use lignindb::{
    commit::NoStaleHashes,
    snapshot::Snapshot,
    txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite, WriteTxn},
    Db, Options,
};

use self::error::{failed, merk_error, not_supported, txn_error, write_error};
use super::{Engine, EngineBatch, SubtreeCursor, SubtreeHandle, SubtreeMove, WriteGroup};

/// The subtree at the root of a grove: the one with the empty path.
const ROOT: SubtreePrefix = [0; 32];

/// The LigninDB engine.
#[derive(Debug)]
pub struct LigninEngine {
    db: Db,
}

impl LigninEngine {
    /// The engine on an open LigninDB database. The root Merk of the grove
    /// is entered in the subtree directory if it is not.
    pub fn new(db: Db) -> Result<Self, Error> {
        let engine = Self { db };
        engine.enter_root()?;
        Ok(engine)
    }

    /// Opens the database at `path` with `options`.
    pub fn open_with<P: AsRef<Path>>(path: P, options: &Options) -> Result<Self, Error> {
        Self::new(Db::open(path, options).map_err(failed)?)
    }

    /// The LigninDB database underneath.
    pub fn database(&self) -> &Db {
        &self.db
    }

    /// Enters the subtree of the root Merk, which has no element to be
    /// entered under, where the latest version does not have it: in a
    /// database that was just made, and after a wipe.
    fn enter_root(&self) -> Result<(), Error> {
        let root = Subtree::new(ROOT);
        let entered = {
            let snapshot = self.db.snapshot();
            let operation = snapshot.operation();
            let opened = operation.open(root).map_err(failed)?;
            opened.is_some()
        };
        if !entered {
            let tree_type = tree_type_bytes(TreeType::NormalTree);
            let enter = [SubtreeWrite::Enter {
                subtree: root,
                tree_type: &tree_type,
                host: None,
            }];
            let batch = Batch {
                subtrees: &enter,
                ..Batch::default()
            };
            (self.db.write(&batch, &mut NoStaleHashes)).map_err(write_error)?;
        }
        Ok(())
    }
}

/// One writer's scope of a write transaction, as the engine keeps it: with
/// the lifetime of its borrow alone.
trait ScopeView: Send {
    fn view(&self) -> LigninView<'_>;

    fn merge(self: Box<Self>);

    fn rollback(self: Box<Self>);
}

impl ScopeView for LigninWriter<'_, '_> {
    fn view(&self) -> LigninView<'_> {
        self.into()
    }

    fn merge(self: Box<Self>) {
        self.into_scope().merge();
    }

    fn rollback(self: Box<Self>) {
        self.into_scope().rollback();
    }
}

enum Kind<'db> {
    Write(WriteTxn<'db>),
    /// A snapshot, with how many savepoints were set on it: it has nothing
    /// to undo, and keeps the count that a transaction's stack has.
    Snapshot(LigninSnapshot<'db>, AtomicUsize),
    Scope(Box<dyn ScopeView + 'db>),
}

/// A transaction of the LigninDB engine: a write transaction, a snapshot,
/// or the view of a write scope.
pub struct LigninTransaction<'db> {
    kind: Kind<'db>,
}

impl std::fmt::Debug for LigninTransaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match &self.kind {
            Kind::Write(_) => "LigninTransaction(write)",
            Kind::Snapshot(..) => "LigninTransaction(snapshot)",
            Kind::Scope(_) => "LigninTransaction(write scope)",
        })
    }
}

impl<'db> LigninTransaction<'db> {
    fn snapshot(snapshot: Snapshot<'db>) -> Self {
        let kind = Kind::Snapshot(LigninSnapshot::new(snapshot), AtomicUsize::new(0));
        Self { kind }
    }

    fn view(&self) -> LigninView<'_> {
        match &self.kind {
            Kind::Write(transaction) => transaction.into(),
            Kind::Snapshot(snapshot, _) => snapshot.into(),
            Kind::Scope(scope) => scope.view(),
        }
    }

    /// Whether the transaction is a snapshot: it reads one version and
    /// refuses every write.
    pub fn is_snapshot_read(&self) -> bool {
        matches!(self.kind, Kind::Snapshot(..))
    }

    /// The write transaction of the database underneath, for a transaction
    /// that is one.
    pub fn write_transaction(&self) -> Option<&WriteTxn<'db>> {
        match &self.kind {
            Kind::Write(transaction) => Some(transaction),
            Kind::Snapshot(..) | Kind::Scope(_) => None,
        }
    }
}

impl EngineBatch for LigninBatch {
    fn next_operation(&self) {
        LigninBatch::next_operation(self)
    }

    fn take_pending(&self) -> Self {
        LigninBatch::take_pending(self)
    }

    fn merge_overwriting(&self, other: Self) {
        LigninBatch::merge_overwriting(self, other)
    }
}

/// Settled batches that wait to be applied to their transaction together,
/// in the order they were settled.
#[derive(Debug, Default)]
pub struct LigninWriteGroup {
    parts: Vec<LigninSettled>,
    pending_cost: OperationCost,
}

impl WriteGroup for LigninWriteGroup {
    fn pending_cost(&self) -> &OperationCost {
        &self.pending_cost
    }
}

impl<'db> SubtreeHandle<'db> for LigninNodeStore<'db> {
    type Aux<'a>
        = LigninKeyspace<'a, 'db>
    where
        Self: 'a;
    type Meta<'a>
        = LigninKeyspace<'a, 'db>
    where
        Self: 'a;

    fn aux(&self) -> Self::Aux<'_> {
        LigninNodeStore::aux(self)
    }

    fn meta(&self) -> Self::Meta<'_> {
        LigninNodeStore::meta(self)
    }
}

/// One writer's part of a write transaction of the LigninDB engine. A
/// scope that is dropped is rolled back.
pub struct LigninWriteScope<'db> {
    view: LigninTransaction<'db>,
}

impl std::fmt::Debug for LigninWriteScope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LigninWriteScope").finish_non_exhaustive()
    }
}

/// The move of a subtree to another prefix, which the LigninDB engine
/// does not make yet: the first step says so (GROVE-03).
#[derive(Debug)]
pub struct LigninSubtreeMove;

impl SubtreeMove for LigninSubtreeMove {
    fn move_next<T>(
        &mut self,
        _inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, crate::Error>,
        _bill: impl FnOnce(&[u8], &[u8]) -> KeyValueStorageCost,
    ) -> CostResult<Option<T>, crate::Error> {
        Err(not_supported("the move of a subtree to another prefix").into())
            .wrap_with_cost(OperationCost::default())
    }
}

impl PendingPrefixDrops for LigninEngine {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error> {
        let snapshot = LigninSnapshot::new(self.db.snapshot());
        let namespace = LigninSubtree::alone(*pending_prefix_drops_namespace());
        let handle = LigninNodeStore::read_only(&snapshot, namespace);
        let meta = handle.meta();
        let mut cursor = grovedb_storage::FlatStore::cursor(&meta);
        let mut records = Vec::new();
        cursor.seek_to_first().unwrap();
        while cursor.valid().unwrap() {
            if let (Some(key), Some(value)) = (cursor.key().unwrap(), cursor.value().unwrap()) {
                records.push(PendingPrefixDropRecord::decode(key, value)?);
            }
            cursor.next().unwrap();
        }
        cursor.status()?;
        Ok(records)
    }

    fn delete_prefix_ranges(&self, _prefixes: &[SubtreePrefix]) -> Result<(), Error> {
        Err(not_supported(
            "the removal of the records that a flat drop left",
        ))
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), Error> {
        let namespace = Subtree::new(*pending_prefix_drops_namespace());
        let delete = [RecordWrite::delete(Column::Meta, namespace, primary_prefix)];
        let batch = Batch {
            records: &delete,
            ..Batch::default()
        };
        (self.db.write(&batch, &mut NoStaleHashes)).map_err(write_error)?;
        Ok(())
    }
}

/// The subtree at `path` as a handle names it, at the cost of deriving
/// its prefix from the path (`cost-model.md` P1). With `hosted` it names
/// the element that hosts it: the one under the last segment of the path,
/// in the subtree of the segments before.
fn named<'b, B: AsRef<[u8]> + 'b>(
    path: SubtreePath<'b, B>,
    hosted: bool,
) -> CostContext<LigninSubtree> {
    let host = match path.derive_parent() {
        Some((parent, key)) if hosted => Some((build_prefix(parent).value, key.to_vec())),
        _ => None,
    };
    build_prefix(path).map(|prefix| LigninSubtree {
        prefix,
        host: host.map(|(subtree, key)| LigninHost {
            subtree,
            key,
            hosted: vec![prefix],
        }),
    })
}

/// The subtree with `prefix` as a handle names it: with the element that
/// hosts it where a write through the handle has to say so, read from the
/// subtree directory as the transaction has it. A read that the
/// transaction refuses names no host, and the handle's own reads and
/// writes meet the refusal.
fn named_by_prefix(prefix: SubtreePrefix, view: Option<LigninView<'_>>) -> LigninSubtree {
    let alone = LigninSubtree::alone(prefix);
    let Some(view) = view else {
        return alone;
    };
    let entry = LigninNodeStore::read_only(view, alone.clone()).entry();
    match entry.ok().flatten().and_then(|entry| entry.host) {
        // An element that hosts more than one Merk is said with every
        // one of them, which only its element names (GROVE-04).
        Some(host) if host.position == 0 => LigninSubtree {
            prefix,
            host: Some(LigninHost {
                subtree: host.subtree,
                key: host.key,
                hosted: vec![prefix],
            }),
        },
        _ => alone,
    }
}

impl Engine for LigninEngine {
    type Applied = LigninApplied;
    type Batch = LigninBatch;
    type ImmediateSubtree<'db> = LigninNodeStore<'db>;
    type Subtree<'db> = LigninNodeStore<'db>;
    type SubtreeMove<'db> = LigninSubtreeMove;
    type Transaction<'db> = LigninTransaction<'db>;
    type WriteGroup = LigninWriteGroup;
    type WriteScope<'db> = LigninWriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Self::open_with(path, &Options::default())
    }

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        // A checkpoint is a database directory, and is opened as one
        // (`lignindb::checkpoint`). None is made where there is none.
        let options = Options {
            create_if_missing: false,
            ..Options::default()
        };
        Self::open_with(path, &options)
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, _path: P) -> Result<(), Error> {
        Err(not_supported("a checkpoint of an open database"))
    }

    fn flush(&self) -> Result<(), Error> {
        self.db.flush().map(|_| ()).map_err(failed)
    }

    fn wipe(&self) -> Result<(), Error> {
        self.db.wipe().map_err(write_error)?;
        self.enter_root()
    }

    fn start_transaction(&self) -> LigninTransaction<'_> {
        LigninTransaction {
            kind: Kind::Write(self.db.begin()),
        }
    }

    fn start_snapshot_read_transaction(&self) -> LigninTransaction<'_> {
        LigninTransaction::snapshot(self.db.snapshot())
    }

    fn start_read_transaction(&self) -> LigninTransaction<'_> {
        LigninTransaction::snapshot(self.db.snapshot())
    }

    fn commit_transaction(&self, transaction: LigninTransaction<'_>) -> CostResult<(), Error> {
        let committed = match &transaction.kind {
            Kind::Write(transaction) => (self.db.commit(transaction, &mut NoStaleHashes))
                .map(|_| ())
                .map_err(write_error),
            Kind::Snapshot(..) => Err(Error::SnapshotReadOnlyTransaction("commit")),
            Kind::Scope(_) => Err(Error::WriteScope(WriteScopeError::ScopesOpen)),
        };
        // A commit ends its transaction however it ends (CM6): one that
        // the engine left open is given up here.
        drop(transaction);
        committed.wrap_with_cost(OperationCost::default())
    }

    fn rollback_transaction(&self, transaction: &LigninTransaction<'_>) -> Result<(), Error> {
        match &transaction.kind {
            Kind::Write(transaction) => self.db.rollback(transaction).map_err(txn_error),
            Kind::Snapshot(..) => Ok(()),
            Kind::Scope(_) => Err(Error::WriteScope(WriteScopeError::ScopesOpen)),
        }
    }

    fn set_savepoint(&self, transaction: &LigninTransaction<'_>) -> Result<(), Error> {
        match &transaction.kind {
            Kind::Write(transaction) => transaction.set_savepoint().map_err(error::savepoint_error),
            Kind::Snapshot(_, savepoints) => {
                savepoints.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Kind::Scope(_) => Err(Error::WriteScope(WriteScopeError::ScopesOpen)),
        }
    }

    fn rollback_to_savepoint(&self, transaction: &LigninTransaction<'_>) -> Result<(), Error> {
        match &transaction.kind {
            Kind::Write(transaction) => transaction
                .rollback_to_savepoint()
                .map_err(error::savepoint_error),
            Kind::Snapshot(_, savepoints) => savepoints
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |set| {
                    set.checked_sub(1)
                })
                .map(|_| ())
                .map_err(|_| {
                    error::savepoint_error(lignindb::savepoint::SavepointError::NoSavepoint)
                }),
            Kind::Scope(_) => Err(Error::WriteScope(WriteScopeError::ScopesOpen)),
        }
    }

    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a LigninTransaction<'_>,
    ) -> Result<LigninWriteScope<'a>, Error> {
        match &transaction.kind {
            Kind::Write(transaction) => {
                let scope = transaction.scope().map_err(txn_error)?;
                let writer: Box<dyn ScopeView + 'a> = Box::new(LigninWriter::new(scope));
                Ok(LigninWriteScope {
                    view: LigninTransaction {
                        kind: Kind::Scope(writer),
                    },
                })
            }
            Kind::Snapshot(..) => Err(Error::SnapshotReadOnlyTransaction("write scope")),
            Kind::Scope(_) => Err(not_supported(
                "a write scope that is opened on the view of another",
            )),
        }
    }

    fn scope_transaction<'s, 'a: 's>(scope: &'s LigninWriteScope<'a>) -> &'s LigninTransaction<'a> {
        &scope.view
    }

    fn merge_write_scope(&self, scope: LigninWriteScope<'_>) -> Result<(), Error> {
        if let Kind::Scope(scope) = scope.view.kind {
            scope.merge();
        }
        Ok(())
    }

    fn rollback_write_scope(&self, scope: LigninWriteScope<'_>) -> Result<(), Error> {
        if let Kind::Scope(scope) = scope.view.kind {
            scope.rollback();
        }
        Ok(())
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a LigninBatch>,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        named(path, batch.is_some())
            .map(|subtree| LigninNodeStore::deferred(transaction.view(), subtree, batch))
    }

    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a LigninBatch>,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>> {
        let view = transaction.view();
        let subtree = named_by_prefix(prefix, batch.map(|_| view));
        LigninNodeStore::deferred(view, subtree, batch).wrap_with_cost(OperationCost::default())
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        named(path, true).map(|subtree| LigninNodeStore::immediate(transaction.view(), subtree))
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>> {
        let view = transaction.view();
        let subtree = named_by_prefix(prefix, Some(view));
        LigninNodeStore::immediate(view, subtree).wrap_with_cost(OperationCost::default())
    }

    fn settle_batch(
        &self,
        group: &mut LigninWriteGroup,
        batch: LigninBatch,
    ) -> CostResult<(), Error> {
        // A delete that came with no cost is charged from the record the
        // committed state holds, whatever a transaction wrote there
        // (`transactions.md` RD10).
        let committed = LigninSnapshot::new(self.db.snapshot());
        batch
            .settle(&committed)
            .map_ok(|settled| {
                group.pending_cost += settled.pending_cost().clone();
                group.parts.push(settled);
            })
            .map_err(merk_error)
    }

    fn apply_write_group(
        &self,
        group: LigninWriteGroup,
        transaction: &LigninTransaction<'_>,
    ) -> CostResult<LigninApplied, Error> {
        let view = transaction.view();
        // A snapshot refuses the writes itself, and is asked for nothing
        // before it does.
        let enters = match transaction.is_snapshot_read() {
            true => Ok(Vec::new()),
            false => enter::owed(view, &group.parts),
        };
        match enters {
            Ok(enters) => LigninSettled::apply(group.parts, view, &enters).map_err(merk_error),
            Err(error) => Err(error).wrap_with_cost(OperationCost::default()),
        }
    }

    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self> {
        LigninCursor::retarget(cursor, &prefix)
    }

    fn move_subtree<'a>(
        &'a self,
        _from: SubtreePrefix,
        _to: SubtreePrefix,
        _batch: &'a LigninBatch,
        _transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninSubtreeMove> {
        LigninSubtreeMove.wrap_with_cost(OperationCost::default())
    }
}
