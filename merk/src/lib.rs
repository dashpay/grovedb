/// The top-level store API.
#[cfg(feature = "full")]
mod merk;

pub use crate::merk::{chunks::ChunkProducer, restore::Restorer};

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

mod visualize;

pub use ed;
#[allow(deprecated)]
pub use proofs::query::verify_query;
pub use proofs::query::{execute_proof, verify};
pub use tree::{
    BatchEntry, CryptoHash, MerkBatch, Op, PanicSource, HASH_BLOCK_SIZE, HASH_LENGTH,
    HASH_LENGTH_U32,
};

// #[cfg(feature = "full")]
// // pub use crate::merk::{chunks, restore, Merk};
pub use crate::merk::{
    defaults::ROOT_KEY_KEY, KVIterator, Merk, ProofConstructionResult, ProofWithoutEncodingResult,
};
pub use crate::visualize::VisualizeableMerk;
