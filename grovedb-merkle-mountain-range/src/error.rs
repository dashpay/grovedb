/// Alias for `core::result::Result<T, Error>`.
pub type Result<T> = core::result::Result<T, Error>;

/// Unified error type for MMR operations.
///
/// Covers store failures, proof corruption, invalid inputs, and merge errors.
#[derive(Debug, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub enum Error {
    /// Tried to compute the root hash of an empty MMR.
    GetRootOnEmpty,
    /// The backing store returned data inconsistent with the expected MMR
    /// structure.
    InconsistentStore,
    /// An error propagated from the underlying storage layer.
    StoreError(String),
    /// Tried to verify proof of a non-leaf.
    NodeProofsNotSupported,
    /// The leaves list is empty or beyond the MMR range.
    GenProofForInvalidLeaves,
    /// A wrapped MMR operation failure.
    OperationFailed(String),
    /// Invalid MMR data (deserialization, corruption).
    InvalidData(String),
    /// Invalid input parameters.
    InvalidInput(String),
    /// Invalid proof during verification.
    InvalidProof(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        use Error::*;
        match self {
            GetRootOnEmpty => write!(f, "Get root on an empty MMR"),
            InconsistentStore => write!(f, "Inconsistent store"),
            StoreError(msg) => write!(f, "Store error: {}", msg),
            NodeProofsNotSupported => write!(f, "Tried to verify membership of a non-leaf"),
            GenProofForInvalidLeaves => write!(f, "Generate proof for invalid leaves"),
            OperationFailed(msg) => write!(f, "MMR operation failed: {}", msg),
            InvalidData(msg) => write!(f, "Invalid MMR data: {}", msg),
            InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
            InvalidProof(msg) => write!(f, "Invalid proof: {}", msg),
        }
    }
}

impl std::error::Error for Error {}
