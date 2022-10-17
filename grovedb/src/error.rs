#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("cyclic reference path")]
    CyclicReference,
    #[error("reference hops limit exceeded")]
    ReferenceLimit,
    #[error("missing reference {0}")]
    MissingReference(String),
    #[error("internal error: {0}")]
    InternalError(&'static str),
    #[error("invalid proof: {0}")]
    InvalidProof(&'static str),
    #[error("invalid input: {0}")]
    InvalidInput(&'static str),

    // Path errors

    // The path key not found could represent a valid query, just where the path key isn't there
    #[error("path key not found: {0}")]
    PathKeyNotFound(String),
    // The path not found could represent a valid query, just where the path isn't there
    #[error("path not found: {0}")]
    PathNotFound(&'static str),

    // The path's item by key referenced was not found
    #[error("corrupted referenced path key not found: {0}")]
    CorruptedReferencePathKeyNotFound(String),
    // The path referenced was not found
    #[error("corrupted referenced path not found: {0}")]
    CorruptedReferencePathNotFound(&'static str),

    // The invalid path represents a logical error from the client library
    #[error("invalid path: {0}")]
    InvalidPath(String),
    // The corrupted path represents a consistency error in internal groveDB logic
    #[error("corrupted path: {0}")]
    CorruptedPath(&'static str),

    // Query errors
    #[error("invalid query: {0}")]
    InvalidQuery(&'static str),
    #[error("missing parameter: {0}")]
    MissingParameter(&'static str),
    // Irrecoverable errors
    #[error("storage_cost error: {0}")]
    StorageError(#[from] storage::error::Error),
    #[error("data corruption error: {0}")]
    CorruptedData(String),

    #[error("corrupted code execution error: {0}")]
    CorruptedCodeExecution(&'static str),

    #[error("invalid batch operation error: {0}")]
    InvalidBatchOperation(&'static str),

    #[error("delete up tree stop height more than initial path size error: {0}")]
    DeleteUpTreeStopHeightMoreThanInitialPathSize(String),

    // Client allowed errors
    #[error("just in time element flags client error: {0}")]
    JustInTimeElementFlagsClientError(&'static str),

    #[error("split removal bytes client error: {0}")]
    SplitRemovalBytesClientError(&'static str),

    #[error("client returned non client error: {0}")]
    ClientReturnedNonClientError(&'static str),

    // Support errors
    #[error("not supported: {0}")]
    NotSupported(&'static str),
}
