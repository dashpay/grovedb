//! The engines of the suite that are on RocksDB.

use std::path::Path;

use grovedb_costs::{CostContext, CostResult};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{
        PrefixedRocksDbImmediateStorageContext, PrefixedRocksDbTransactionContext, RocksDbStorage,
    },
    Error, Storage, StorageBatch,
};

use super::{NodeStoreOnly, TestEngine};

/// RocksDB: a handle is the storage context of its subtree, which is a
/// `NodeStore` through the adapter of `store::kv`.
pub struct RocksDbTestEngine {
    storage: RocksDbStorage,
}

impl RocksDbTestEngine {
    /// The storage underneath, for a test that is about RocksDB.
    pub fn storage(&self) -> &RocksDbStorage {
        &self.storage
    }
}

impl TestEngine for RocksDbTestEngine {
    type Batch = StorageBatch;
    type ImmediateSubtree<'a> = PrefixedRocksDbImmediateStorageContext<'a>;
    type Subtree<'a> = PrefixedRocksDbTransactionContext<'a>;
    type Transaction<'db> = <RocksDbStorage as Storage<'db>>::Transaction;

    fn open(path: &Path) -> Self {
        RocksDbTestEngine {
            storage: RocksDbStorage::default_rocksdb_with_path(path)
                .expect("cannot open rocksdb storage"),
        }
    }

    fn start_transaction(&self) -> Self::Transaction<'_> {
        self.storage.start_transaction()
    }

    fn commit_transaction(&self, transaction: Self::Transaction<'_>) -> CostResult<(), Error> {
        self.storage.commit_transaction(transaction)
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a StorageBatch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.storage
            .get_transactional_storage_context(path, batch, transaction)
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.storage
            .get_immediate_storage_context(path, transaction)
    }

    fn apply_batch(
        &self,
        batch: StorageBatch,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<(), Error> {
        self.storage
            .commit_multi_context_batch(batch, Some(transaction))
    }
}

/// RocksDB behind handles that are a [`NodeStore`] and nothing else.
///
/// A handle of [`RocksDbTestEngine`] is a storage context as well, so a test
/// that reads or writes records through it compiles there. Here it does not:
/// the suite on this engine is the part of the suite that a store which is not
/// a storage context can run.
pub struct NodeStoreOnlyEngine {
    engine: RocksDbTestEngine,
}

impl TestEngine for NodeStoreOnlyEngine {
    type Batch = <RocksDbTestEngine as TestEngine>::Batch;
    type ImmediateSubtree<'a> =
        NodeStoreOnly<<RocksDbTestEngine as TestEngine>::ImmediateSubtree<'a>>;
    type Subtree<'a> = NodeStoreOnly<<RocksDbTestEngine as TestEngine>::Subtree<'a>>;
    type Transaction<'db> = <RocksDbTestEngine as TestEngine>::Transaction<'db>;

    fn open(path: &Path) -> Self {
        NodeStoreOnlyEngine {
            engine: RocksDbTestEngine::open(path),
        }
    }

    fn start_transaction(&self) -> Self::Transaction<'_> {
        self.engine.start_transaction()
    }

    fn commit_transaction(&self, transaction: Self::Transaction<'_>) -> CostResult<(), Error> {
        self.engine.commit_transaction(transaction)
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
        self.engine
            .subtree(path, batch, transaction)
            .map(|store| NodeStoreOnly { store })
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.engine
            .immediate_subtree(path, transaction)
            .map(|store| NodeStoreOnly { store })
    }

    fn apply_batch(
        &self,
        batch: Self::Batch,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<(), Error> {
        self.engine.apply_batch(batch, transaction)
    }
}
