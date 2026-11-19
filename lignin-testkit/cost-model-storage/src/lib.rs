//! The cost model of `docs/lignindb/cost-model.md` with the interface of
//! `grovedb-storage`.
//!
//! This crate takes the place of `grovedb-storage` in the workspace that
//! `check.sh` lays out, so that GroveDB and Merk run on the model with no
//! change to their code. The traits, the multi-context batch and the flat
//! keyspace seam are the real crate's source files, taken by path. What is
//! replaced is the RocksDB implementation: [`rocksdb_storage`] keeps the
//! names GroveDB uses, and behind them is `lignin_testkit::cost_model`, a
//! logical state whose every charge is a function of sizes.
//!
//! The tests of this crate are the real crate's: the multi-context batch,
//! the flat keyspace seam, and the whole suite of the RocksDB storage
//! layer, run on the model.

#![forbid(unsafe_code)]

pub mod cost_model;
pub mod error;
#[path = "../../../storage/src/flat.rs"]
pub mod flat;
pub mod prefix;
#[cfg(feature = "rocksdb_storage")]
pub mod rocksdb_storage;
#[path = "../../../storage/src/storage.rs"]
mod storage;
#[path = "../../../storage/src/worst_case_costs.rs"]
pub mod worst_case_costs;

pub use crate::{
    error::Error,
    flat::FlatStore,
    storage::{Batch, ChildrenSizes, RawIterator, Storage, StorageBatch, StorageContext},
};
