//! The cost model of `docs/lignindb/cost-model.md`, as code.
//!
//! The document specifies what today's code charges for each storage
//! event as a function of logical sizes. This module is those functions
//! ([`rules`]), a Merk node as the fields an engine keeps of it with the
//! length of the record today's engine would store ([`node`]), and a
//! logical model of the state they are applied to ([`state`]): ordered
//! records per subtree and keyspace, a transaction's writes over them,
//! cursors and batches. No storage engine is involved and no node record
//! is kept.
//!
//! # Checking it against the corpus
//!
//! GroveDB cannot be run on another store from inside this workspace: it
//! names the RocksDB storage types until the `Engine` seam (SEAM-04) is
//! cut. So the check builds the workspace a second time with the storage
//! crate replaced. `lignin-testkit/cost-model-storage` is a crate with the
//! name and the interface of `grovedb-storage`, whose contexts, batches and
//! cursors are this module. `lignin-testkit/cost-model-storage/check.sh`
//! lays out a copy of the workspace in which `storage` is that crate, and
//! replays the golden corpus there. GroveDB's and Merk's own code runs
//! unchanged above it and decides which records each operation reads and
//! writes; every charge of the storage layer comes from [`rules`], and
//! RocksDB is not compiled in. A replay that is green there says that the
//! rules predict every recorded `OperationCost`, and the root hashes,
//! results and proofs say that the model kept the same state.
//!
//! The tests of this module check the same code inside this workspace,
//! against Merk's encoding and against the RocksDB contexts themselves.

pub mod node;
pub mod rules;
pub mod state;

#[cfg(test)]
mod against_merk;
#[cfg(test)]
mod against_rocksdb;
