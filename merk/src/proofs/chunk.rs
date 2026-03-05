//! Chunk proofs

mod binary_range;
#[cfg(feature = "minimal")]
/// Chunk generation and verification for tree synchronization.
pub mod chunk;
/// Chunk operation types for encoding and decoding.
pub mod chunk_op;
/// Chunk-related error types.
pub mod error;
#[cfg(feature = "minimal")]
pub mod util;
