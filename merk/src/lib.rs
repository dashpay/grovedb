extern crate core;

/// The top-level store API.
#[cfg(feature = "full")]
mod merk;

pub use crate::merk::{chunks::ChunkProducer, options::MerkOptions, restore::Restorer};

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

/// Errors
pub mod error;
/// Estimated costs
pub mod estimated_costs;
mod visualize;

pub use ed;
pub use error::Error;
#[allow(deprecated)]
pub use proofs::query::verify_query;
pub use proofs::query::{execute_proof, verify};
pub use tree::{
    BatchEntry, CryptoHash, Link, MerkBatch, Op, PanicSource, TreeFeatureType, HASH_BLOCK_SIZE,
    HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};

// #[cfg(feature = "full")]
// // pub use crate::merk::{chunks, restore, Merk};
pub use crate::merk::{
    defaults::ROOT_KEY_KEY, IsSumTree, KVIterator, Merk, MerkType, ProofConstructionResult,
    ProofWithoutEncodingResult,
};
pub use crate::visualize::VisualizeableMerk;
