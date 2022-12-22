#[cfg(feature = "full")]
extern crate core;

/// The top-level store API.
#[cfg(feature = "full")]
mod merk;

#[cfg(feature = "full")]
pub use crate::merk::{chunks::ChunkProducer, options::MerkOptions, restore::Restorer};

/// Provides a container type that allows temporarily taking ownership of a
/// value.
// TODO: move this into its own crate
#[cfg(feature = "full")]
pub mod owner;
/// Algorithms for generating and verifying Merkle proofs.
#[cfg(any(feature = "full", feature = "verify"))]
pub mod proofs;

/// Various helpers useful for tests or benchmarks.
#[cfg(feature = "full")]
pub mod test_utils;

/// The core tree data structure.
#[cfg(any(feature = "full", feature = "verify"))]
pub mod tree;

/// Errors
#[cfg(any(feature = "full", feature = "verify"))]
pub mod error;

/// Estimated costs
#[cfg(feature = "full")]
pub mod estimated_costs;

#[cfg(feature = "full")]
mod visualize;

#[cfg(feature = "full")]
pub use ed;
#[cfg(feature = "full")]
pub use error::Error;
#[cfg(any(feature = "full", feature = "verify"))]
pub use proofs::query::{execute_proof, verify_query};
#[cfg(feature = "full")]
pub use tree::{
    BatchEntry, Link, MerkBatch, Op, PanicSource, HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32,
    HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};
#[cfg(any(feature = "full", feature = "verify"))]
pub use tree::{CryptoHash, TreeFeatureType};

#[cfg(feature = "full")]
pub use crate::merk::{
    defaults::ROOT_KEY_KEY, IsSumTree, KVIterator, Merk, MerkType, ProofConstructionResult,
    ProofWithoutEncodingResult, RootHashKeyAndSum,
};
#[cfg(feature = "full")]
pub use crate::visualize::VisualizeableMerk;
