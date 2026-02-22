//! Merk proofs

#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod branch;
#[cfg(feature = "minimal")]
pub mod chunk;
pub mod query;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod tree;

// Re-export Op and Node from grovedb-query
// Re-export hex_to_ascii for use in verify.rs
pub use grovedb_query::hex_to_ascii;
// Re-export encode_into from grovedb-query
#[cfg(feature = "minimal")]
pub use grovedb_query::proofs::encode_into;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_query::proofs::{Node, Op};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use query::Query;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree::{execute, Tree};

/// Decoder iterates over proof bytes, yielding Op values with merk Error type.
///
/// This wraps grovedb-query's Decoder, converting its error type to merk's
/// Error.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub struct Decoder<'a> {
    inner: grovedb_query::proofs::Decoder<'a>,
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl<'a> Decoder<'a> {
    /// Create a new Decoder from proof bytes
    pub const fn new(proof_bytes: &'a [u8]) -> Self {
        Decoder {
            inner: grovedb_query::proofs::Decoder::new(proof_bytes),
        }
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl Iterator for Decoder<'_> {
    type Item = Result<Op, crate::error::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|result| result.map_err(Into::into))
    }
}

/// Re-export encoding module for backward compatibility
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod encoding {
    //! Re-exports of proof encoding types from grovedb-query
    pub use grovedb_query::proofs::encode_into;

    pub use super::Decoder;
}
