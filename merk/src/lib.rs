#![feature(map_first_last)]

#[global_allocator]
#[cfg(feature = "full")]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

#[cfg(feature = "full")]
pub use rocksdb;

/// Error and Result types.
mod error;
/// The top-level store API.
#[cfg(feature = "full")]
mod merk;
pub use crate::merk::column_families;
/// Provides a container type that allows temporarily taking ownership of a
/// value.
// TODO: move this into its own crate
pub mod owner;
/// Algorithms for generating and verifying Merkle proofs.
pub mod proofs;

/// Various helpers useful for tests or benchmarks.
pub mod test_utils;
/// The core tree data structure.
pub mod tree;

pub use error::{Error, Result};
#[allow(deprecated)]
pub use proofs::query::verify_query;
pub use proofs::query::{execute_proof, verify};
pub use tree::{Batch, BatchEntry, Hash, Op, PanicSource, HASH_LENGTH};

#[cfg(feature = "full")]
// pub use crate::merk::{chunks, restore, Merk};
pub use crate::merk::{chunks, Merk};
