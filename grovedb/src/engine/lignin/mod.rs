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
//! reads what the element hosts, enters those of its Merks that are not
//! entered and that no batch of the group enters itself, states anew
//! those that are entered under the element otherwise than it names
//! them, drops a secondary that the element hosts no more, and says to
//! the store which Merks the element's node names ([`enter`]): none,
//! where a batch empties the Merks of an indexed tree after the batch
//! that wrote its element as an empty tree.
//!
//! # A subtree leaves with its element
//!
//! GroveDB names no subtree where it removes an element. Where a group of
//! batches is applied that removes a node, the engine drops what the
//! directory has entered under that node, with every subtree below it
//! ([`gone`]), but for a subtree that a batch of the group was told to
//! move, which goes to another prefix ([`moved`]). The removals that
//! GroveDB staged for the records of a dropped
//! subtree are settled and charged, as on RocksDB, and the drop takes
//! the place of applying them. What a drop leaves in the pages is
//! reclaimed by the engine's background work.
//!
//! # What it does not do yet
//!
//! A first write into a subtree under a key that holds no element is
//! refused with `Error::NotSupported` and changes nothing.

mod behind;
mod checkpoint;
mod enter;
mod error;
mod gone;
mod moved;
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
    txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite, Unit, WriteTxn},
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

/// `handle`, as the engine hands a handle out: with its root-key record
/// kept by its prefix in the roots keyspace of the namespace that no
/// element hosts ([`hostless`]), unless it is the root Merk's, which
/// stays under its own prefix. A handle of the namespace's own prefix
/// keeps its record in the namespace as the record of its own subtree
/// ([`LigninNodeStore::root_key_kept_in`]).
///
/// RocksDB keeps a root-key record in a column of its own under the
/// prefix of its subtree. GroveDB's delete of a tree and its move of a
/// tree remove and copy records of the data keyspace alone, so the
/// record of a secondary of an indexed tree stays under its prefix when
/// the tree is deleted or moved away, does not come with a tree that is
/// moved, and is read by the next open of a secondary under that prefix
/// whose element names no root (`Merk::open_base`), which is charged the
/// bytes of the key in it and the fetch of the node it names. Kept in the
/// subtree, the record would leave with a drop and go with a move. Kept
/// here, it stays under the prefix of a subtree that is dropped or moved
/// away, and a moved subtree brings none. The store finds a node from
/// the root that the Merk's entry in the subtree directory names, and by
/// no such record. A record stays under its prefix, as on RocksDB, until
/// a write of it puts another or removes it, a drain names the prefix
/// ([`PendingPrefixDrops::delete_prefix_ranges`]) or the grove is wiped.
fn keeping_its_root_key(handle: LigninNodeStore<'_>) -> LigninNodeStore<'_> {
    match handle.subtree().prefix == ROOT {
        true => handle,
        false => handle.root_key_kept_in(*pending_prefix_drops_namespace()),
    }
}

/// `handle`, as the engine hands a handle out: with cursors that count
/// what flat drops left behind, by the marks that the engine keeps in the
/// aux keyspace of the namespace of the pending-drop records
/// ([`behind`], [`LigninNodeStore::left_behind_in`]).
fn counting_what_is_left(handle: LigninNodeStore<'_>) -> LigninNodeStore<'_> {
    handle.left_behind_in(*pending_prefix_drops_namespace())
}

/// The subtrees that no element hosts and that every grove has, each with
/// the tree type it is entered with: the root Merk, as a normal tree whose
/// type bytes are `normal`, and the namespace of the pending-drop records,
/// which keeps no tree. The namespace holds the pending-drop records in
/// its meta keyspace and the root-key records of the grove's subtrees in
/// its roots keyspace ([`keeping_its_root_key`]).
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
    /// What a test has happen on its own thread where the next walk of a
    /// move begins, before the entry of its subtree is read, once.
    static BEFORE_A_WALK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
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
    /// The database of the engine that began it.
    db: &'db Db,
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
    fn snapshot(db: &'db Db) -> Self {
        let snapshot = Box::new(LigninSnapshot::new(db.snapshot()));
        let kind = Kind::Snapshot(snapshot, AtomicUsize::new(0));
        Self { kind, db }
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
/// its nodes where they are (`tree-layout.md` D5), where the batch is
/// applied ([`moved`]), and the batch says so
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
            // the batch holds the move. Where it is not, the batch is told
            // that the walk found none.
            let entry = source.entry().map_err(merk_error);
            let entry = cost_return_on_error_no_add!(cost, entry.map_err(crate::Error::from));
            match entry {
                Some(_) => batch.move_subtree(from, to),
                None => batch.move_of_none(from, to),
            }
            *told = true;
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

    /// Removes what is under each of `prefixes`, outside any transaction.
    /// A subtree that the latest version has entered under the prefix is
    /// dropped, with every subtree below it, but for the namespace of the
    /// root-key records (below), and the root-key record that
    /// is kept for the prefix outside its subtree
    /// ([`keeping_its_root_key`]) is removed, as the range delete of
    /// RocksDB covers the roots column. The marks of what a flat drop
    /// left behind under the prefix ([`behind`]) are removed with them.
    /// A prefix is one commit where it has any of the three, and no
    /// write where it has none.
    ///
    /// A flat drop on this engine drops the subtree with its element, in
    /// the drop's own commit (`transactions.md` 14.5 D3), so the drain of
    /// its pending-drop record finds nothing entered under the record's
    /// prefixes, unless a write that no element leads to entered a
    /// subtree under one of them since. What it does find is the root-key
    /// record of each of those prefixes that has one, and the marks of
    /// those that held a record. GroveDB drains a pending-drop record
    /// once the path of the dropped tree leads to no element. The call
    /// itself looks at none of this: a prefix that is entered is dropped,
    /// whatever leads to it.
    ///
    /// The namespace that keeps those records is not dropped where its
    /// own prefix is given, which the record of no flat drop of GroveDB
    /// names. The records of its data, aux and meta keyspaces are
    /// removed, the marks of every prefix among them, and of its roots
    /// keyspace its own root-key record, in one commit. The root-key
    /// records that it keeps for other prefixes stay, as they do on
    /// RocksDB, where they are under those prefixes. The transaction
    /// refuses the removal by range of the data keyspace of a subtree
    /// whose Merk has a root, so the call fails while a Merk under the
    /// namespace's prefix has a node.
    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error> {
        let namespace = *pending_prefix_drops_namespace();
        for prefix in prefixes {
            if *prefix == [0xFF; 32] {
                // The words of the RocksDB engine, which refuses the same.
                return Err(Error::StorageError(
                    "delete_prefix_ranges: refusing to range-delete the all-0xFF prefix"
                        .to_string(),
                ));
            }
            let subtree = Subtree::new(*prefix);
            // Where the root-key record of the prefix is kept, and whether
            // there is one. The record of the root Merk is in its own
            // subtree and leaves with it: it is not looked for.
            let (home, key, keeps_a_root_key) = {
                let snapshot = LigninSnapshot::new(self.db.snapshot());
                let handle = LigninNodeStore::read_only(&snapshot, LigninSubtree::alone(*prefix));
                let handle = keeping_its_root_key(handle);
                let (home, key) = handle.root_key_place();
                let kept = *prefix != ROOT
                    && (NodeStore::root_key(&handle).value)
                        .map_err(merk_error)?
                        .is_some();
                (home, key, kept)
            };
            // The marks of what a flat drop left behind under the prefix.
            let marks = {
                let snapshot = LigninSnapshot::new(self.db.snapshot());
                behind::marks_of((&snapshot).into(), prefix)?
            };
            let entered = self.entered(subtree)?;
            if !entered && !keeps_a_root_key && marks.is_empty() {
                continue;
            }
            let kept_record = RecordWrite::delete(Column::Roots, Subtree::new(home), &key);
            let kept_record = keeps_a_root_key.then_some(kept_record);
            let marks = (marks.iter())
                .map(|key| RecordWrite::delete(Column::Aux, Subtree::new(namespace), key));
            let every_record_of = |column| RecordWrite::DeleteRange {
                unit: Unit::new(column, subtree),
                start: &[],
                end: None,
            };
            let (subtrees, records): (Vec<_>, Vec<_>) = match *prefix == namespace {
                // What is under the namespace's own prefix. Of what it
                // keeps for other prefixes, that is the marks, which are
                // records of its aux keyspace, and no root-key record.
                true => {
                    let own = [Column::Data, Column::Aux, Column::Meta].map(every_record_of);
                    (Vec::new(), own.into_iter().chain(kept_record).collect())
                }
                false => {
                    let drop = entered.then_some(SubtreeWrite::Drop { subtree });
                    (
                        drop.into_iter().collect(),
                        kept_record.into_iter().chain(marks).collect(),
                    )
                }
            };
            let batch = Batch {
                subtrees: &subtrees,
                records: &records,
                ..Batch::default()
            };
            (self.db.write(&batch, &mut NoStaleHashes)).map_err(write_error)?;
        }
        Ok(())
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

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Self::open_checkpoint_at(path.as_ref())
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.checkpoint_at(path.as_ref())
    }

    fn delete_checkpoint<P: AsRef<Path>>(path: P) -> Result<(), crate::Error> {
        Self::delete_checkpoint_at(path.as_ref()).map_err(Into::into)
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
            db: &self.db,
        }
    }

    fn start_snapshot_read_transaction(&self) -> LigninTransaction<'_> {
        LigninTransaction::snapshot(&self.db)
    }

    fn start_read_transaction(&self) -> LigninTransaction<'_> {
        LigninTransaction::snapshot(&self.db)
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
        let db = transaction.db;
        match &transaction.kind {
            Kind::Write(transaction) => {
                let scope = transaction.scope().map_err(txn_error)?;
                let writer: Box<dyn ScopeView + 'a> = Box::new(LigninWriter::new(scope));
                Ok(LigninWriteScope {
                    view: LigninTransaction {
                        kind: Kind::Scope(writer),
                        db,
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
        // found by it ([`over`], [`gone`]).
        let segments = batch.map(|_| path.to_vec());
        named(path, batch.is_some()).map(|subtree| {
            let handle = LigninNodeStore::deferred(transaction.view(), subtree, batch);
            let handle = keeping_its_root_key(handle);
            let handle = counting_what_is_left(handle);
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
        let handle = keeping_its_root_key(LigninNodeStore::deferred(view, subtree, batch));
        counting_what_is_left(handle).wrap_with_cost(OperationCost::default())
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
        let handle = LigninNodeStore::deferred(transaction.view(), subtree, batch);
        counting_what_is_left(keeping_its_root_key(handle)).wrap_with_cost(OperationCost::default())
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        named(path, true).map(|subtree| {
            let handle = LigninNodeStore::immediate(transaction.view(), subtree);
            counting_what_is_left(keeping_its_root_key(handle))
        })
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a LigninTransaction<'_>,
    ) -> CostContext<LigninNodeStore<'a>> {
        let view = transaction.view();
        let subtree = named_by_prefix(prefix, Some(view));
        let handle = keeping_its_root_key(LigninNodeStore::immediate(view, subtree));
        counting_what_is_left(handle).wrap_with_cost(OperationCost::default())
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
        // The look below is bounded by the version of this engine's
        // database: a transaction that another engine began is refused,
        // with nothing read through it and nothing applied to it.
        if !std::ptr::eq(transaction.db, &self.db) {
            let refused = failed("the transaction was begun by another engine");
            return Err(refused).wrap_with_cost(OperationCost::default());
        }
        let view = transaction.view();
        let mut parts = group.parts;
        // A snapshot refuses the writes itself, and is asked for nothing
        // before it does.
        if transaction.is_snapshot_read() {
            return LigninSettled::apply(parts, view, &[]).map_err(merk_error);
        }
        // A batch that lands an element which names data where the walk
        // of its move found no subtree is refused: the directory does not
        // hold what that element says.
        if let Err(error) = moved::found_none(&parts) {
            return Err(error).wrap_with_cost(OperationCost::default());
        }
        // A group in which a batch enters, drops or clears a subtree
        // itself, or states one anew, can be refused for an entry by its
        // own doing, with nothing published beside it: it is not looked
        // for again.
        let own = parts.iter().any(LigninSettled::writes_directory);
        loop {
            // A commit that is published from here on is one the look may
            // not have read.
            let before = self.db.version();
            // What leaves by a removed node is looked for first: nothing
            // is entered on account of a write that is left out. Where
            // an element is written over a tree and its look states a
            // drop, the first look is made again with that drop.
            let looked = gone::gone(view, &parts, own, &over::Left::default()).and_then(|gone| {
                // What the enters were worked out with.
                let before = gone.unentered();
                let owed = enter::owed(view, &parts, &before)?;
                let left = over::left(view, &parts, &gone.left_out)?;
                let gone = match left.drops.is_empty() {
                    true => gone,
                    false => gone::gone(view, &parts, own, &left)?,
                };
                Ok((owed, left, gone, before, moved::moved(view, &parts)?))
            });
            let (owed, left, gone, left_before, moved) = match looked {
                Ok(looked) => looked,
                Err(error) => return Err(error).wrap_with_cost(OperationCost::default()),
            };
            #[cfg(test)]
            happen(&AFTER_A_LOOK, transaction);
            // What an indexed tree hosts is said of its element in
            // every batch of the group, as this look read the element.
            // So is the Merk of a tree that one batch of several writes
            // while another writes its element, that an element hosts
            // nothing whose subtree is dropped, and what the elements
            // host that a moved subtree goes to or leaves. No batch hands
            // over a write that this look leaves out.
            for part in &mut parts {
                part.forget_hosts();
                part.forget_drops();
                part.forget_moves();
                part.forget_stated();
                let stated = (owed.hosts.iter().chain(&owed.apart)).chain(&left.hosts);
                for host in stated.chain(&moved.hosts) {
                    part.host(host);
                }
                for left_out in &gone.left_out {
                    part.leave_out(left_out);
                }
                for removing in &gone.removing {
                    part.remove_if_entered(removing);
                }
            }
            // What is entered under a removed node when the group is laid
            // out leaves with the batch that removes the node.
            // Where the group hands a write over for the prefix as the
            // look found it, which is not under the node, no drop is
            // stated: the first batch says that the prefix is not
            // entered under the node, before anything of the group is
            // laid out.
            for under in &gone.drops {
                let (prefix, above, key) = (&under.prefix, &under.above, &under.key);
                let part = match gone.beside.contains(prefix) {
                    false => {
                        parts[under.part].drop_if_entered(prefix, above, key);
                        continue;
                    }
                    true => parts.first_mut(),
                };
                if let Some(first) = part {
                    first.not_under(prefix, above, key);
                }
            }
            // A secondary that hangs under an indexed tree whose element
            // hosts it no more leaves with the batch that writes the
            // element, and what the group removes from it is not handed
            // over.
            for (part, stray) in &owed.strays {
                parts[*part].drop_subtree(stray);
                for part in &mut parts {
                    part.leave_out(stray);
                }
            }
            // A Merk of an indexed tree that its element names at another
            // place, or as a tree of another type, than it is entered
            // with is stated anew with the batch that puts the element's
            // node.
            for (part, subtree, tree_type) in owed.restated.iter().chain(&left.restated) {
                if let Err(error) = parts[*part].state_anew(subtree, tree_type) {
                    return Err(merk_error(error)).wrap_with_cost(OperationCost::default());
                }
            }
            // What an element was written over leaves with the first
            // batch, which is the one that enters again what the group
            // still writes; what is below a drop above goes with that.
            // The drop names the node of the element.
            if let Some(first) = parts.first_mut() {
                let stated = (left.drops.iter()).filter(|dropped| !gone.covered.contains(*dropped));
                for dropped in stated {
                    match left.under.get(dropped) {
                        Some((above, key)) => first.drop_under(dropped, above, key, false),
                        None => first.drop_subtree(dropped),
                    }
                }
            }
            // A subtree is moved with the batch that was told of it, and
            // what makes room for it is dropped with the same batch,
            // under the node it was found under and dated at the read of
            // that node.
            for (part, dropped, above, key) in &moved.drops {
                parts[*part].drop_under(dropped, above, key, true);
            }
            for (part, stated) in &moved.moves {
                parts[*part].move_as(stated);
            }
            // What a flat drop of the group leaves behind on RocksDB.
            let dropped = (gone.found.iter().copied())
                .chain(moved.drops.iter().map(|(_, dropped, ..)| *dropped))
                .chain(left.drops.iter().copied())
                .collect();
            match behind::left_behind(view, &parts, &dropped) {
                Ok(marks) => {
                    for (part, keyspace, prefix) in marks {
                        let namespace = pending_prefix_drops_namespace();
                        parts[part].leave_behind(namespace, keyspace, &prefix);
                    }
                }
                Err(error) => return Err(error).wrap_with_cost(OperationCost::default()),
            }
            // A subtree that is dropped and entered again is entered
            // once, as the look for what is dropped has it, where the
            // look for what is entered had found it absent before.
            let again =
                |prefix: &[u8; 32]| left.enters.iter().any(|(known, _)| known.prefix == *prefix);
            // Two enters are not handed over: one of the look for what
            // is written over, for a subtree that a drop of a removed
            // node takes, and one of the look for what is entered, where
            // the second look for what leaves found, after the enters
            // were worked out, that the group holds nothing to apply to
            // the subtree: its writes are left out, or are removals
            // that are made if the subtree is entered. For a subtree
            // whose element hosts one Merk that is asked of the subtree
            // itself. The Merks of an indexed tree are entered together
            // or not at all, also those that the group holds no write
            // for: not at all where that holds of each of them that the
            // group holds a write for, and no batch of the group states a
            // value for their element or removes it. A node of the
            // element that a batch puts with the value it had names
            // them all the same, and its cell points at no Merk where
            // they are not entered.
            let unentered = gone.unentered();
            let held: std::collections::BTreeSet<[u8; 32]> = (parts.iter())
                .flat_map(LigninSettled::written)
                .map(|subtree| subtree.prefix)
                .collect();
            let since = |subtree: &LigninSubtree| {
                let prefix = &subtree.prefix;
                match &subtree.host {
                    Some(host) if host.hosted.len() > 1 => {
                        let written = host.hosted.iter().filter(|merk| held.contains(*merk));
                        let mut written = written.peekable();
                        let stored = matches!(enter::host_of(&parts, host), enter::Host::Stored);
                        stored
                            && written.peek().is_some()
                            && written.all(|merk| unentered.contains(merk))
                    }
                    _ => unentered.contains(prefix) && !left_before.contains(prefix),
                }
            };
            let entered_again = (left.enters.iter())
                .filter(|(subtree, _)| !gone.left_out.contains(&subtree.prefix));
            let owed_enters = (owed.enters.iter())
                .filter(|(subtree, _)| !again(&subtree.prefix) && !since(subtree));
            let enters: Vec<_> = entered_again.chain(owed_enters).cloned().collect();
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
            // the look had, and not by a drop that the look stated for
            // a subtree which the group hands a write for; no batch of
            // the group enters, drops or clears a subtree itself, or
            // states one anew; and a commit was published since before
            // the look. Every other refusal is the caller's answer. What
            // is entered under a removed node is judged by the
            // transaction where the group is laid out: it leaves with
            // the node, and nothing is refused, where the group hands no
            // write over for it; where the group wrote it as its look
            // found it, the group is refused as a conflict, which is
            // the caller's answer.
            let stale = (refused.entry).is_some_and(|entry| {
                let found_otherwise = owed.found_otherwise(entry) || left.found_otherwise(entry);
                let own_doing = left.own_doing(entry) && !gone.leaves_out(entry);
                found_otherwise && !own_doing
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
        #[cfg(test)]
        if let Some(due) = BEFORE_A_WALK.take() {
            beside_the_group(due);
        }
        let source = LigninNodeStore::read_only(transaction.view(), LigninSubtree::alone(from));
        // A subtree that is not entered has no record and no entry to
        // move: its walk finds nothing, and the batch is told of no move.
        // Where it is not entered at the end of the walk either, the batch
        // is told that the walk found none (`moved::found_none`).
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
