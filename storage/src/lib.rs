#![deny(missing_docs)]

//! Storage abstraction for GroveDB.

pub mod error;
#[cfg(feature = "rocksdb_storage")]
pub mod rocksdb_storage;
mod storage;
pub mod worst_case_costs;

pub use crate::storage::{
    AbstractBatchOperation, Batch, RawIterator, Storage, StorageBatch, StorageContext, ChildrenSizes
};

pub use crate::error::Error;
