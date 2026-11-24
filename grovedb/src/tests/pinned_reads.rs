//! [`PinnedReads`]: the RocksDB engine with one difference. The transaction
//! of a read that was given none is a snapshot read transaction, as on an
//! engine that pins such a read to one state (RD2 of
//! `docs/lignindb/transactions.md`). A read that started two transactions
//! would read two states on such an engine, and a read that wrote would be
//! refused.
//!
//! It counts the transactions it starts, for
//! `one_transaction_per_operation_tests`, and the write scopes it is asked
//! for. It is also a registered engine of
//! the suite (`pinned-reads` in `test-engines.txt`): its handles and
//! transactions are RocksDB's, and the engine itself is a type of its own
//! that gives no way to the storage underneath.

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use grovedb_costs::{CostContext, CostResult};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix},
    Error as StorageError,
};

use crate::{engine::SubtreeCursor, Engine, RocksDbEngine};

pub struct PinnedReads {
    engine: RocksDbEngine,
    pub(crate) reads_started: AtomicUsize,
    pub(crate) writes_started: AtomicUsize,
    pub(crate) snapshots_started: AtomicUsize,
    /// How many times a write scope was asked for.
    pub(crate) scopes_opened: AtomicUsize,
}

impl PinnedReads {
    fn over(engine: RocksDbEngine) -> Self {
        PinnedReads {
            engine,
            reads_started: AtomicUsize::new(0),
            writes_started: AtomicUsize::new(0),
            snapshots_started: AtomicUsize::new(0),
            scopes_opened: AtomicUsize::new(0),
        }
    }
}

impl PendingPrefixDrops for PinnedReads {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, StorageError> {
        self.engine.pending_prefix_drop_records()
    }

    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), StorageError> {
        self.engine.delete_prefix_ranges(prefixes)
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), StorageError> {
        self.engine
            .remove_pending_prefix_drop_record(primary_prefix)
    }
}

impl Engine for PinnedReads {
    type Applied = <RocksDbEngine as Engine>::Applied;
    type Batch = <RocksDbEngine as Engine>::Batch;
    type ImmediateSubtree<'db> = <RocksDbEngine as Engine>::ImmediateSubtree<'db>;
    type Subtree<'db> = <RocksDbEngine as Engine>::Subtree<'db>;
    type SubtreeMove<'db> = <RocksDbEngine as Engine>::SubtreeMove<'db>;
    type Transaction<'db> = <RocksDbEngine as Engine>::Transaction<'db>;
    type WriteGroup = <RocksDbEngine as Engine>::WriteGroup;
    type WriteScope<'db> = <RocksDbEngine as Engine>::WriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        RocksDbEngine::open(path).map(Self::over)
    }

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        RocksDbEngine::open_checkpoint(path).map(Self::over)
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), StorageError> {
        self.engine.create_checkpoint(path)
    }

    fn flush(&self) -> Result<(), StorageError> {
        self.engine.flush()
    }

    fn wipe(&self) -> Result<(), StorageError> {
        self.engine.wipe()
    }

    fn start_transaction(&self) -> Self::Transaction<'_> {
        self.writes_started.fetch_add(1, Ordering::Relaxed);
        self.engine.start_transaction()
    }

    fn start_snapshot_read_transaction(&self) -> Self::Transaction<'_> {
        self.snapshots_started.fetch_add(1, Ordering::Relaxed);
        self.engine.start_snapshot_read_transaction()
    }

    fn start_read_transaction(&self) -> Self::Transaction<'_> {
        self.reads_started.fetch_add(1, Ordering::Relaxed);
        self.engine.start_snapshot_read_transaction()
    }

    fn commit_transaction(
        &self,
        transaction: Self::Transaction<'_>,
    ) -> CostResult<(), StorageError> {
        self.engine.commit_transaction(transaction)
    }

    fn rollback_transaction(
        &self,
        transaction: &Self::Transaction<'_>,
    ) -> Result<(), StorageError> {
        self.engine.rollback_transaction(transaction)
    }

    fn set_savepoint(&self, transaction: &Self::Transaction<'_>) -> Result<(), StorageError> {
        self.engine.set_savepoint(transaction)
    }

    fn rollback_to_savepoint(
        &self,
        transaction: &Self::Transaction<'_>,
    ) -> Result<(), StorageError> {
        self.engine.rollback_to_savepoint(transaction)
    }

    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a Self::Transaction<'_>,
    ) -> Result<Self::WriteScope<'a>, StorageError> {
        self.scopes_opened.fetch_add(1, Ordering::Relaxed);
        self.engine.open_write_scope(transaction)
    }

    fn scope_transaction<'s, 'a: 's>(scope: &'s Self::WriteScope<'a>) -> &'s Self::Transaction<'a> {
        RocksDbEngine::scope_transaction(scope)
    }

    fn merge_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), StorageError> {
        self.engine.merge_write_scope(scope)
    }

    fn rollback_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), StorageError> {
        self.engine.rollback_write_scope(scope)
    }

    fn rolls_back_write_scopes(&self) -> bool {
        self.engine.rolls_back_write_scopes()
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.engine.subtree(path, batch, transaction)
    }

    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>> {
        self.engine.subtree_by_prefix(prefix, batch, transaction)
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.engine.immediate_subtree(path, transaction)
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>> {
        self.engine.immediate_subtree_by_prefix(prefix, transaction)
    }

    fn settle_batch(
        &self,
        group: &mut Self::WriteGroup,
        batch: Self::Batch,
    ) -> CostResult<(), StorageError> {
        self.engine.settle_batch(group, batch)
    }

    fn apply_write_group(
        &self,
        group: Self::WriteGroup,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<Self::Applied, StorageError> {
        self.engine.apply_write_group(group, transaction)
    }

    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self> {
        RocksDbEngine::retarget_cursor(cursor, prefix)
    }

    fn move_subtree<'a>(
        &'a self,
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a Self::Batch,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::SubtreeMove<'a>> {
        self.engine.move_subtree(from, to, batch, transaction)
    }
}

/// The count of write scopes, and what `GroveDb` asks this engine for.
mod scopes {
    use std::sync::atomic::Ordering;

    use grovedb_version::version::GROVE_VERSIONS;

    use super::PinnedReads;
    use crate::{Element, Engine, GroveDb};

    /// A dedicated insert into an indexed tree over a child that is a tree
    /// undoes its sweep by a write scope where the engine rolls one back.
    /// RocksDB rolls none back, and this engine answers as RocksDB does: the
    /// insert asks it for no scope, where its put is refused and where it
    /// is made. So the one scope of the RocksDB engine is free for every
    /// transaction all through the call.
    #[test]
    fn a_dedicated_insert_over_a_tree_asks_this_engine_for_no_write_scope() {
        const TOP: &[u8] = b"indexed";
        const INDEX: &[&[u8]] = &[TOP, b"by"];
        for v in GROVE_VERSIONS.iter() {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<PinnedReads>::open_with_engine(dir.path()).expect("a database");
            assert!(!db.engine().rolls_back_write_scopes());
            let tx = db.start_transaction();
            let t = Some(&tx);
            let root: &[&[u8]] = &[];
            let (tree, index) = (
                Element::empty_tree(),
                Element::empty_provable_sum_indexed_tree(),
            );
            (db.insert(root, TOP, tree, None, t, v).unwrap()).expect("a tree");
            (db.insert(&[TOP], b"by", index, None, t, v).unwrap()).expect("an indexed tree");
            let over = |key: &[u8]| {
                let branch = Element::empty_sum_tree();
                db.insert_into_provable_sum_indexed_tree(INDEX, key, branch, t, v)
                    .unwrap()
            };
            // Three sum trees whose sums are in range together, and are
            // not without the negative one.
            for (key, sum) in [(b"g", -10), (b"a", i64::MAX - 5), (b"z", 8)] {
                over(key.as_slice()).expect("a sum tree");
                let item = Element::new_sum_item(sum);
                let put = db.insert(&[TOP, b"by", key.as_slice()], b"n", item, None, t, v);
                put.unwrap().expect("a sum item in it");
            }
            over(b"g".as_slice())
                .expect_err("the sum overflows without the tree that is written over");
            over(b"z".as_slice()).expect("the insert over a tree is made");
            assert_eq!(db.engine().scopes_opened.load(Ordering::Relaxed), 0);
            // The count is of every scope that is asked for.
            drop(db.engine().open_write_scope(&tx).expect("a scope"));
            assert_eq!(db.engine().scopes_opened.load(Ordering::Relaxed), 1);
        }
    }
}
