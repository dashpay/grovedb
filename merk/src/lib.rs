//! High-performance Merkle key/value store

// #![deny(missing_docs)]

/// The top-level store API.
#[cfg(feature = "minimal")]
pub mod merk;

#[cfg(feature = "grovedbg")]
pub mod debugger;

#[cfg(feature = "minimal")]
pub use crate::merk::{chunks::ChunkProducer, options::MerkOptions, restore::Restorer};

/// Provides a container type that allows temporarily taking ownership of a
/// value.
// TODO: move this into its own crate
#[cfg(feature = "minimal")]
pub mod owner;
/// Algorithms for generating and verifying Merkle proofs.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod proofs;

/// Various helpers useful for tests or benchmarks.
#[cfg(feature = "test_utils")]
pub mod test_utils;

/// The core tree data structure.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod tree;

/// Errors
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod error;

/// Estimated costs
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod estimated_costs;

#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod tree_type;
#[cfg(feature = "minimal")]
mod visualize;

#[cfg(feature = "minimal")]
pub use ed;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use error::Error;
#[cfg(feature = "minimal")]
pub use tree::{
    BatchEntry, Link, MerkBatch, Op, PanicSource, HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32,
    HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree::{CryptoHash, TreeFeatureType};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree_type::MaybeTree;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree_type::TreeType;

#[cfg(feature = "minimal")]
pub use crate::merk::{
    defaults::ROOT_KEY_KEY,
    prove::{ProofConstructionResult, ProofWithoutEncodingResult},
    KVIterator, Merk, MerkType, RootHashKeyAndAggregateData,
};
#[cfg(feature = "minimal")]
pub use crate::visualize::VisualizeableMerk;
