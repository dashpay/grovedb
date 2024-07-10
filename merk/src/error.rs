//! Errors
#[cfg(feature = "full")]
use crate::proofs::chunk::error::ChunkError;

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, thiserror::Error)]
/// Errors
pub enum Error {
    // Input data errors
    /// Overflow
    #[error("overflow error {0}")]
    Overflow(&'static str),

    /// Division by zero error
    #[error("divide by zero error {0}")]
    DivideByZero(&'static str),

    /// Wrong estimated costs element type for level error
    #[error("wrong estimated costs element type for level error {0}")]
    WrongEstimatedCostsElementTypeForLevel(&'static str),

    /// Corrupted sum node error
    #[error("corrupted sum node error {0}")]
    CorruptedSumNode(&'static str),

    /// Invalid input error
    #[error("invalid input error {0}")]
    InvalidInputError(&'static str),

    /// Corrupted code execution
    #[error("corrupted code execution error {0}")]
    CorruptedCodeExecution(&'static str),

    /// Corrupted state
    #[error("corrupted state: {0}")]
    CorruptedState(&'static str),

    /// Chunking error
    #[cfg(feature = "full")]
    #[error("chunking error {0}")]
    ChunkingError(ChunkError),

    // TODO: remove
    /// Old chunking error
    #[error("chunking error {0}")]
    OldChunkingError(&'static str),

    /// Chunk restoring error
    #[cfg(feature = "full")]
    #[error("chunk restoring error {0}")]
    ChunkRestoringError(ChunkError),

    // TODO: remove
    /// Chunk restoring error
    #[error("chunk restoring error {0}")]
    OldChunkRestoringError(String),

    /// Key not found error
    #[error("key not found error {0}")]
    KeyNotFoundError(&'static str),

    /// Key ordering error
    #[error("key ordering error {0}")]
    KeyOrderingError(&'static str),

    /// Invalid proof error
    #[error("invalid proof error {0}")]
    InvalidProofError(String),

    /// Proof creation error
    #[error("proof creation error {0}")]
    ProofCreationError(String),

    /// Cyclic error
    #[error("cyclic error {0}")]
    CyclicError(&'static str),

    /// Not supported error
    #[error("not supported error {0}")]
    NotSupported(String),

    /// Request amount exceeded error
    #[error("request amount exceeded error {0}")]
    RequestAmountExceeded(String),

    /// Invalid operation error
    #[error("invalid operation error {0}")]
    InvalidOperation(&'static str),

    /// Internal error
    #[error("internal error {0}")]
    InternalError(&'static str),

    /// Specialized costs error
    #[error("specialized costs error {0}")]
    SpecializedCostsError(&'static str),

    /// Client corruption error
    #[error("client corruption error {0}")]
    ClientCorruptionError(String),

    #[cfg(feature = "full")]
    /// Storage error
    #[error("storage error {0}")]
    StorageError(grovedb_storage::Error),

    // Merk errors
    /// Ed error
    #[error("ed error: {0}")]
    EdError(ed::Error),

    // Costs errors
    /// Costs errors
    #[error("costs error: {0}")]
    CostsError(grovedb_costs::error::Error),
    // Version errors
    #[error(transparent)]
    /// Version error
    VersionError(grovedb_version::error::GroveVersionError),
}

impl From<grovedb_version::error::GroveVersionError> for Error {
    fn from(value: grovedb_version::error::GroveVersionError) -> Self {
        Error::VersionError(value)
    }
}
