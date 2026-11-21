//! The names of the RocksDB storage layer, on the cost model.
//!
//! Every type GroveDB, Merk and the append-only trees name in
//! `grovedb_storage::rocksdb_storage` is here under the same name. None of
//! them holds an engine: a [`RocksDbStorage`] is a
//! [`Database`](crate::cost_model::state::Database) of logical records, and
//! each call charges through [`rules`](crate::cost_model::rules).

mod storage;
pub mod storage_context;
#[path = "../../../storage/src/rocksdb_storage/test_utils.rs"]
pub mod test_utils;
#[cfg(test)]
#[path = "../../../storage/src/rocksdb_storage/tests.rs"]
mod tests;

pub use storage_context::{
    AuxKeyspace, MetaKeyspace, PrefixedRocksDbBatch, PrefixedRocksDbImmediateStorageContext,
    PrefixedRocksDbRawIterator, PrefixedRocksDbTransactionContext,
};

pub use self::storage::{EngineError as Error, RocksDbStorage, Tx, WriteBatchWithTransaction};
pub use crate::flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord};
