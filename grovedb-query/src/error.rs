/// Errors that can occur during query operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested operation is not supported for this query configuration.
    #[error("not supported error {0}")]
    NotSupported(String),

    /// The query would exceed the maximum allowed number of results.
    #[error("request amount exceeded error {0}")]
    RequestAmountExceeded(String),

    /// Internal error indicating corrupted or unexpected state during
    /// execution.
    #[error("corrupted code execution error {0}")]
    CorruptedCodeExecution(&'static str),

    /// The operation is invalid for the given query item type.
    #[error("invalid operation error {0}")]
    InvalidOperation(&'static str),

    /// Invalid proof error
    #[error("invalid proof error {0}")]
    InvalidProofError(String),

    /// Key ordering error
    #[error("key ordering error {0}")]
    KeyOrderingError(&'static str),

    /// Ed encoding/decoding error
    #[error("ed error: {0}")]
    EdError(ed::Error),
}
