#![deny(missing_docs)]

//! Storage abstraction for GroveDB.

#[cfg(feature = "rocksdb_storage")]
pub mod rocksdb_storage;
mod storage;

pub use crate::storage::{
    Batch, BatchOperation, RawIterator, Storage, StorageBatch, StorageContext,
};
