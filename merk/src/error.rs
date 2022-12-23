#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("overflow error {0}")]
    Overflow(&'static str),

    #[error("divide by zero error {0}")]
    DivideByZero(&'static str),

    #[error("wrong estimated costs element type for level error {0}")]
    WrongEstimatedCostsElementTypeForLevel(&'static str),

    #[error("corrupted sum node error {0}")]
    CorruptedSumNode(&'static str),

    #[error("invalid input error {0}")]
    InvalidInputError(&'static str),

    #[error("corruption error {0}")]
    CorruptionError(&'static str),

    #[error("chunking error {0}")]
    ChunkingError(&'static str),

    #[error("chunk restoring error {0}")]
    ChunkRestoringError(String),

    #[error("key not found error {0}")]
    KeyNotFoundError(&'static str),

    #[error("key ordering error {0}")]
    KeyOrderingError(&'static str),

    #[error("invalid proof error {0}")]
    InvalidProofError(String),

    #[error("proof creation error {0}")]
    ProofCreationError(String),

    #[error("cyclic error {0}")]
    CyclicError(&'static str),

    #[error("not supported error {0}")]
    NotSupported(&'static str),

    #[error("request amount exceeded error {0}")]
    RequestAmountExceeded(String),

    #[error("invalid operation error {0}")]
    InvalidOperation(&'static str),

    #[error("specialized costs error {0}")]
    SpecializedCostsError(&'static str),

    #[error("client corruption error {0}")]
    ClientCorruptionError(String),

    #[error("storage error {0}")]
    StorageError(storage::Error),

    // Merk errors
    #[error("ed error: {0}")]
    EdError(ed::Error),

    // Costs errors
    #[error("costs error: {0}")]
    CostsError(costs::error::Error),
}
