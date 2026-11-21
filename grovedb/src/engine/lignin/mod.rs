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
//! An indexed tree hosts more than one Merk: its primary, at the path of
//! its element, and a secondary for each of its axes. A deferred handle
//! that is opened with a batch beside the subtree of a path that has a
//! last segment names the element under that segment
//! ([`Engine::subtree_beside`]). Where a group of batches is applied that
//! holds a write for one of these Merks through a handle that names the
//! element, and no batch of the group enters that Merk itself, the engine
//! reads what the element hosts, enters those of its Merks that no batch
//! of the group enters itself if none of them is entered, and says to the
//! store which Merks the element's node names ([`enter`]).
//!
//! # What it does not do yet
//!
//! Each is refused with `Error::NotSupported` and changes nothing: a
//! checkpoint of an open database and the open of one (GROVE-08), the
//! application of a batch that moves a subtree which is entered, which
//! is walked and charged and then refused until the transaction can give
//! a subtree another prefix (`TXN-03-F4`), a first write into a subtree
//! under a key that holds no element, and the removal of the records
//! that a flat drop left (GROVE-05). The delete of a tree whose
//! Merk holds elements is refused by its handle (`FlatStore::clear` of
//! the node store), and a subtree is not dropped when its element goes
//! (GROVE-05).

mod enter;
mod error;
mod over;
mod restore;
#[cfg(test)]
mod tests;

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_merk::{
    store::{
        lignin::{
            hash::MerkHashRules, tree_type_bytes, LigninApplied, LigninBatch, LigninCursor,
            LigninFlatCursor, LigninHost, LigninKeyspace, LigninNodeStore, LigninSettled,
            LigninSnapshot, LigninSubtree, LigninView, LigninWriter,
        },
        NodeStore,
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
    Error, FlatStore, RawIterator,
};
use grovedb_version::version::GroveVersion;
use lignindb::{
    commit::NoStaleHashes,
    snapshot::Snapshot,
    txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite, WriteTxn},
    Db, Options,
};

use self::error::{failed, merk_error, not_supported, txn_error, write_error};
use super::{
    move_record, Engine, EngineBatch, SubtreeCursor, SubtreeHandle, SubtreeMove, WriteGroup,
};
use crate::replication::INCOMPLETE_RESTORE_AUX_KEY;

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
    ///
    /// The check that a close of the database makes in a debug build is
    /// given Merk's rules for a check ([`MerkHashRules::for_check`]), so
    /// that it holds the stored hashes of the nodes of every Merk in use
    /// to them: each is made again and compared, but for the value
    /// hashes that those rules take as they are stored. Elements are
    /// read as the latest GroveVersion reads them, as the engine reads a
    /// host.
    pub fn new(db: Db) -> Result<Self, Error> {
        if cfg!(debug_assertions) {
            let rules = MerkHashRules::for_check(GroveVersion::latest());
            db.check_hashes_by(Box::new(rules));
        }
        let engine = Self { db };
        engine.enter_hostless()?;
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

    /// The mark of a restore that did not finish, as the latest version
    /// holds it (`GroveDb::has_incomplete_restore`): the aux record that
    /// state sync keeps at the root from the first commit of a session
    /// that commits in parts until the commit at its end. It is read
    /// through a snapshot.
    fn incomplete_restore_mark(&self) -> Result<Option<Vec<u8>>, Error> {
        #[cfg(test)]
        if MARK_READ_FAILS.with(|fails| fails.replace(false)) {
            return Err(not_supported("a read that a test has fail"));
        }
        let snapshot = LigninSnapshot::new(self.db.snapshot());
        let handle = LigninNodeStore::read_only(&snapshot, LigninSubtree::alone(ROOT));
        let aux = handle.aux();
        grovedb_storage::FlatStore::get(&aux, INCOMPLETE_RESTORE_AUX_KEY).value
    }

    /// Whether the latest version has `subtree` in its subtree directory.
    fn entered(&self, subtree: Subtree) -> Result<bool, Error> {
        let snapshot = self.db.snapshot();
        let operation = snapshot.operation();
        let opened = operation.open(subtree).map_err(failed)?;
        Ok(opened.is_some())
    }

    /// Enters the subtrees that no element hosts and that every grove has
    /// ([`hostless`]), where the latest version does not have them: in a
    /// database that was just made, in one commit. A database that has
    /// both is not written.
    ///
    /// Each is entered if it is absent, so a write of another thread that
    /// entered one since the look is no error: the commit finds that
    /// subtree and enters the other.
    fn enter_hostless(&self) -> Result<(), Error> {
        let normal = tree_type_bytes(TreeType::NormalTree);
        let mut enters = Vec::with_capacity(2);
        for (subtree, tree_type) in hostless(&normal) {
            if !self.entered(subtree)? {
                enters.push(SubtreeWrite::EnterIfAbsent {
                    subtree,
                    tree_type,
                    host: None,
                });
            }
        }
        if enters.is_empty() {
            return Ok(());
        }
        let batch = Batch {
            subtrees: &enters,
            ..Batch::default()
        };
        (self.db.write(&batch, &mut NoStaleHashes)).map_err(write_error)?;
        Ok(())
    }
}

/// The subtrees that no element hosts and that every grove has, each with
/// the tree type it is entered with: the root Merk, as a normal tree whose
/// type bytes are `normal`, and the namespace of the pending-drop records,
/// which keeps no tree.
fn hostless(normal: &[u8]) -> [(Subtree, &[u8]); 2] {
    let namespace = Subtree::new(*pending_prefix_drops_namespace());
    [(Subtree::new(ROOT), normal), (namespace, &[])]
}

/// What a test has happen while a group is applied, with the transaction
/// the group is applied to.
#[cfg(test)]
type Between = Box<dyn FnOnce(&LigninTransaction<'_>)>;

#[cfg(test)]
type Waiting = std::cell::RefCell<std::collections::VecDeque<Between>>;

#[cfg(test)]
thread_local! {
    /// What a test has happen on its own thread between a look that says
    /// which subtrees a group enters and the application after it, one
    /// for each look and in their order.
    static AFTER_A_LOOK: Waiting =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    /// What a test has happen on its own thread after an application
    /// that the transaction refused, before the engine asks whether
    /// anything was published, one for each refusal and in their order.
    static AFTER_A_REFUSAL: Waiting =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    /// What each application of a group that failed answered, on this
    /// thread and in their order, for a test to read.
    static REFUSED: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// What a test has happen on its own thread inside a look, once: the
    /// number of reads of an entry that it comes after, and what it is.
    static AFTER_ENTRY_READS: std::cell::RefCell<Option<(usize, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
    /// A test's word that the next read of the mark of a restore that did
    /// not finish fails, on this thread.
    static MARK_READ_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Has `happens` happen beside a group. A group that it applies itself
/// meets nothing that waits after a look or after a refusal.
#[cfg(test)]
fn beside_the_group(happens: impl FnOnce()) {
    let held = (AFTER_A_LOOK.take(), AFTER_A_REFUSAL.take());
    happens();
    AFTER_A_LOOK.set(held.0);
    AFTER_A_REFUSAL.set(held.1);
}

/// Has the next of `waiting` happen.
#[cfg(test)]
fn happen(waiting: &'static std::thread::LocalKey<Waiting>, transaction: &LigninTransaction<'_>) {
    let next = waiting.with_borrow_mut(|waiting| waiting.pop_front());
    if let Some(next) = next {
        beside_the_group(|| next(transaction));
    }
}

/// Counts a read of an entry by a look, and has what waits for that
/// read happen ([`AFTER_ENTRY_READS`]).
#[cfg(test)]
fn an_entry_was_read() {
    let due = AFTER_ENTRY_READS.with_borrow_mut(|waiting| match waiting {
        Some((reads, _)) if *reads > 1 => {
            *reads -= 1;
            None
        }
        waiting => waiting.take().map(|(_, due)| due),
    });
    if let Some(due) = due {
        beside_the_group(due);
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
    Write(Box<WriteTxn<'db>>),
    /// A snapshot, with how many savepoints were set on it: it has nothing
    /// to undo, and keeps the count that a transaction's stack has.
    Snapshot(Box<LigninSnapshot<'db>>, AtomicUsize),
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
        let snapshot = Box::new(LigninSnapshot::new(snapshot));
        let kind = Kind::Snapshot(snapshot, AtomicUsize::new(0));
        Self { kind }
    }

    fn view(&self) -> LigninView<'_> {
        match &self.kind {
            Kind::Write(transaction) => (&**transaction).into(),
            Kind::Snapshot(snapshot, _) => (&**snapshot).into(),
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

/// The records of the data keyspace of a subtree, as a move walks them:
/// the nodes of its Merk, each as the record Merk encodes of it, or the
/// flat records of a tree that keeps its data flat.
#[derive(Debug)]
enum Records<'db> {
    Nodes(LigninCursor<'db>),
    Flat(LigninFlatCursor<'db>),
}

/// A subtree on its way to another prefix on the LigninDB engine.
///
/// The subtree is not copied: it goes to its new prefix with the batch,
/// its nodes where they are (`tree-layout.md` D5), and the batch says so
/// ([`LigninBatch::move_subtree`]): where the subtree is entered when the
/// walk begins, and otherwise where it is entered when the walk ends,
/// which a commit beside the transaction made so. The cursor of the walk
/// reads the committed state as it was when the cursor was made, with
/// the transaction's own writes: it shows no record of a subtree that
/// was entered after that.
/// What a move is charged on existing
/// GroveVersions is what the copy of RocksDB is charged, record by
/// record, so each step reads the next record through a cursor of the
/// transaction, at a cursor's cost, shows it to the caller, and stages
/// for it the two charges of the copy: the put where the record lands,
/// with the bill the caller gives, and the delete where it leaves, which
/// frees nothing. No record is written.
#[derive(Debug)]
pub struct LigninSubtreeMove<'db> {
    records: Records<'db>,
    /// The handle the subtree is read by where it is.
    source: LigninNodeStore<'db>,
    from: SubtreePrefix,
    to: SubtreePrefix,
    batch: &'db LigninBatch,
    /// Whether the batch was told of the move.
    told: bool,
}

impl SubtreeMove for LigninSubtreeMove<'_> {
    fn move_next<T>(
        &mut self,
        inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, crate::Error>,
        bill: impl FnOnce(&[u8], &[u8]) -> KeyValueStorageCost,
    ) -> CostResult<Option<T>, crate::Error> {
        let Self {
            records,
            source,
            from,
            to,
            batch,
            told,
        } = self;
        let staged = || Ok(()).wrap_with_cost(OperationCost::default());
        let land = |key: &[u8], record: &[u8]| {
            batch.charge_landed(to, key, record.len(), bill(key, record));
            staged()
        };
        // Billed where it lands. In a swap the charge of the record that
        // the other move lands under the same key stands against this one.
        let clear = |key: &[u8]| {
            batch.charge_cleared(from, key, KeyValueStorageCost::default());
            staged()
        };
        let moved = match records {
            Records::Nodes(cursor) => move_record(cursor, inspect, land, clear),
            Records::Flat(cursor) => move_record(cursor, inspect, land, clear),
        };
        let mut cost = OperationCost::default();
        let found = cost_return_on_error!(&mut cost, moved);
        if found.is_none() && !*told {
            // The walk has ended on a subtree that was not entered when
            // it began. Where it is entered now, a commit beside the
            // transaction entered it since, whatever the walk saw of it:
            // the batch holds the move.
            let entry = source.entry().map_err(merk_error);
            let entry = cost_return_on_error_no_add!(cost, entry.map_err(crate::Error::from));
            if entry.is_some() {
                batch.move_subtree(from, to);
                *told = true;
            }
        }
        Ok(found).wrap_with_cost(cost)
    }
}

impl Drop for LigninEngine {
    /// A database that holds the commits of a restore that did not finish
    /// is left out of the check that a close makes in a debug build
    /// ([`Self::new`], `Db::unchecked_at_close`): its elements can commit
    /// to Merks that were not restored, which that check reports, and its
    /// owner closes it to discard it. For a database with no such mark
    /// the engine changes nothing of the close, and so for one whose
    /// restore committed at its end, which takes the mark away.
    ///
    /// The close is taken out of the check only on a mark that was read:
    /// where the read of the mark fails, the engine changes nothing of
    /// the close. While the thread is panicking it reads nothing.
    fn drop(&mut self) {
        if !cfg!(debug_assertions) || std::thread::panicking() {
            return;
        }
        if matches!(self.incomplete_restore_mark(), Ok(Some(_))) {
            self.db.unchecked_at_close();
        }
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

/// The subtree `prefix` as a handle names it that was opened beside the
/// subtree of the element at `path`. With `hosted` it names that element,
/// and of what the element hosts the subtree of its own path: the place
/// of `prefix` among them is the element's to say, and the engine reads
/// it there when the handle's batch is applied ([`enter`]). The prefixes
/// of the path and of the Merk that holds the element are derived for it
/// and not charged.
fn beside<'b, B: AsRef<[u8]> + 'b>(
    path: SubtreePath<'b, B>,
    prefix: SubtreePrefix,
    hosted: bool,
) -> LigninSubtree {
    let host = match path.derive_parent() {
        Some((parent, key)) if hosted => Some(LigninHost {
            subtree: build_prefix(parent).value,
            key: key.to_vec(),
            hosted: vec![build_prefix(path).value],
        }),
        _ => None,
    };
    LigninSubtree { prefix, host }
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
        // A subtree at a later position is one of several that its
        // element hosts, which a handle by prefix cannot name: it is
        // written through a handle that was opened beside the first
        // ([`beside`]).
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
    type SubtreeMove<'db> = LigninSubtreeMove<'db>;
    type Transaction<'db> = LigninTransaction<'db>;
    type WriteGroup = LigninWriteGroup;
    type WriteScope<'db> = LigninWriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Self::open_with(path, &Options::default())
    }

    fn open_checkpoint<P: AsRef<Path>>(_path: P) -> Result<Self, Error> {
        // The engine makes no checkpoint yet, and has no way to tell one
        // from the directory of a database: `GroveDb::delete_checkpoint`
        // removes whatever this opens (GROVE-08).
        Err(not_supported("the open of a checkpoint"))
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, _path: P) -> Result<(), Error> {
        Err(not_supported("a checkpoint of an open database"))
    }

    fn flush(&self) -> Result<(), Error> {
        self.db.flush().map(|_| ()).map_err(failed)
    }

    fn wipe(&self) -> Result<(), Error> {
        // One commit: the wipe, and the two subtrees every grove has.
        let normal = tree_type_bytes(TreeType::NormalTree);
        (self.db.wipe_entering(&hostless(&normal))).map_err(write_error)?;
        Ok(())
    }

    fn start_transaction(&self) -> LigninTransaction<'_> {
        LigninTransaction {
            kind: Kind::Write(Box::new(self.db.begin())),
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
        // A handle that writes is told its path: what a batch drops is
        // found by it ([`over`]).
        let segments = batch.map(|_| path.to_vec());
        named(path, batch.is_some()).map(|subtree| {
            let handle = LigninNodeStore::deferred(transaction.view(), subtree, batch);
            match segments {
                Some(segments) => handle.at_path(segments),
                None => handle,
            }
        })
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

    fn subtree_beside<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        prefix: SubtreePrefix,
        batch: Option<&'a LigninBatch>,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        let subtree = beside(path, prefix, batch.is_some());
        LigninNodeStore::deferred(transaction.view(), subtree, batch)
            .wrap_with_cost(OperationCost::default())
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

    fn adopt_restored_subtrees<'b, B>(
        &self,
        path: SubtreePath<'b, B>,
        transaction: &LigninTransaction<'_>,
    ) -> Result<(), Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        restore::adopt(transaction.view(), path)
    }

    fn settle_batch(
        &self,
        group: &mut LigninWriteGroup,
        batch: LigninBatch,
    ) -> CostResult<(), Error> {
        // A delete that came with no cost is charged from the record the
        // committed state holds, whatever a transaction wrote there
        // (`transactions.md` RD10): read where the delete is come to,
        // through a write transaction that nothing is written into.
        let committed = self.db.begin();
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
        let mut parts = group.parts;
        // A snapshot refuses the writes itself, and is asked for nothing
        // before it does.
        if transaction.is_snapshot_read() {
            return LigninSettled::apply(parts, view, &[]).map_err(merk_error);
        }
        // A group in which a batch enters, drops or clears a subtree
        // itself can be refused for an entry by its own doing, with
        // nothing published beside it: it is not looked for again.
        let own = parts.iter().any(LigninSettled::writes_directory);
        loop {
            // A commit that is published from here on is one the look may
            // not have read.
            let before = self.db.version();
            let looked =
                enter::owed(view, &parts).and_then(|owed| Ok((owed, over::left(view, &parts)?)));
            let (owed, left) = match looked {
                Ok(looked) => looked,
                Err(error) => return Err(error).wrap_with_cost(OperationCost::default()),
            };
            #[cfg(test)]
            happen(&AFTER_A_LOOK, transaction);
            // What an indexed tree hosts is said of its element in
            // every batch of the group, as this look read the element.
            // So is the Merk of a tree that one batch of several writes
            // while another writes its element, and that an element
            // hosts nothing whose subtree is dropped.
            for part in &mut parts {
                part.forget_hosts();
                part.forget_drops();
                for host in (owed.hosts.iter().chain(&owed.apart)).chain(&left.hosts) {
                    part.host(host);
                }
            }
            // What is dropped leaves with the first batch, which is the
            // one that enters again what the group still writes.
            if let Some(first) = parts.first_mut() {
                for dropped in &left.drops {
                    first.drop_subtree(dropped);
                }
            }
            // A subtree that is dropped and entered again is entered
            // once, as the look for what is dropped has it, where the
            // look for what is entered had found it absent before.
            let again =
                |prefix: &[u8; 32]| left.enters.iter().any(|(known, _)| known.prefix == *prefix);
            let owed_enters = (owed.enters.iter()).filter(|(subtree, _)| !again(&subtree.prefix));
            let enters: Vec<_> = (left.enters.iter().chain(owed_enters)).cloned().collect();
            let applied = LigninSettled::apply_kept(&mut parts, view, &enters);
            let Err(refused) = &applied.value else {
                return applied.map_err(|refused| merk_error(refused.error));
            };
            #[cfg(test)]
            {
                REFUSED.with_borrow_mut(|all| all.push(refused.error.to_string()));
                happen(&AFTER_A_REFUSAL, transaction);
            }
            // The group is looked for and applied again where the
            // transaction found the entry of a subtree otherwise than
            // the look had, and not by a drop that the look stated; no
            // batch of the group enters, drops or clears a subtree
            // itself; and a commit was published since before the look.
            // Every other refusal is the caller's answer.
            let stale = (refused.entry).is_some_and(|entry| {
                let found_otherwise = owed.found_otherwise(entry) || left.found_otherwise(entry);
                found_otherwise && !left.own_doing(entry)
            });
            if !stale || own || self.db.version() == before {
                return applied.map_err(|refused| merk_error(refused.error));
            }
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
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a LigninBatch,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninSubtreeMove<'a>> {
        let source = LigninNodeStore::read_only(transaction.view(), LigninSubtree::alone(from));
        // A subtree that is not entered has no record and no entry to
        // move: its walk finds nothing, and the batch is told of no move.
        // A read of the entry that the transaction refuses is met again by
        // the cursor, whose status ends the move. The cursor is made after
        // the entry is read. A subtree that is entered in between or
        // during the walk, and still is where the walk ends, is found by
        // the read there.
        let entry = source.entry().ok().flatten();
        #[cfg(test)]
        an_entry_was_read();
        let told = entry.is_some();
        if told {
            batch.move_subtree(&from, &to);
        }
        let mut cost = OperationCost::default();
        let records = match entry.is_some_and(|entry| entry.root_key.is_none()) {
            false => {
                let mut cursor = NodeStore::cursor(&source);
                cursor.seek_to_first().unwrap_add_cost(&mut cost);
                Records::Nodes(cursor)
            }
            true => {
                let mut cursor = FlatStore::cursor(&source);
                cursor.seek_to_first().unwrap_add_cost(&mut cost);
                Records::Flat(cursor)
            }
        };
        LigninSubtreeMove {
            records,
            source,
            from,
            to,
            batch,
            told,
        }
        .wrap_with_cost(cost)
    }
}
