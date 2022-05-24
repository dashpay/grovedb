/// The top-level store API.
#[cfg(feature = "full")]
mod merk;

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

#[allow(deprecated)]
pub use proofs::query::verify_query;
pub use proofs::query::{execute_proof, verify};
pub use tree::{BatchEntry, Hash, MerkBatch, Op, PanicSource, HASH_LENGTH};

// #[cfg(feature = "full")]
// // pub use crate::merk::{chunks, restore, Merk};
pub use crate::merk::{KVIterator, Merk, ProofConstructionResult, ProofWithoutEncodingResult};
