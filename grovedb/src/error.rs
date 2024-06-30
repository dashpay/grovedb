//! GroveDB Errors

/// GroveDB Errors
#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Input data errors
    #[error("cyclic reference path")]
    /// Cyclic reference
    CyclicReference,
    #[error("reference hops limit exceeded")]
    /// Reference limit
    ReferenceLimit,
    #[error("missing reference {0}")]
    /// Missing reference
    MissingReference(String),
    #[error("internal error: {0}")]
    /// Internal error
    InternalError(&'static str),
    #[error("invalid proof: {0}")]
    /// Invalid proof
    InvalidProof(String),
    #[error("invalid input: {0}")]
    /// Invalid input
    InvalidInput(&'static str),

    #[error("wrong element type: {0}")]
    /// Invalid element type
    WrongElementType(&'static str),

    // Path errors
    /// The path key not found could represent a valid query, just where the
    /// path key isn't there
    #[error("path key not found: {0}")]
    PathKeyNotFound(String),
    /// The path not found could represent a valid query, just where the path
    /// isn't there
    #[error("path not found: {0}")]
    PathNotFound(String),
    /// The path not found could represent a valid query, just where the parent
    /// path merk isn't there
    #[error("path parent layer not found: {0}")]
    PathParentLayerNotFound(String),

    /// The path's item by key referenced was not found
    #[error("corrupted referenced path key not found: {0}")]
    CorruptedReferencePathKeyNotFound(String),
    /// The path referenced was not found
    #[error("corrupted referenced path not found: {0}")]
    CorruptedReferencePathNotFound(String),
    /// The path's parent merk wasn't found
    #[error("corrupted referenced path key not found: {0}")]
    CorruptedReferencePathParentLayerNotFound(String),

    /// The invalid parent layer path represents a logical error from the client
    /// library
    #[error("invalid parent layer path: {0}")]
    InvalidParentLayerPath(String),
    /// The invalid path represents a logical error from the client library
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// The corrupted path represents a consistency error in internal groveDB
    /// logic
    #[error("corrupted path: {0}")]
    CorruptedPath(&'static str),

    // Query errors
    #[error("invalid query: {0}")]
    /// Invalid query
    InvalidQuery(&'static str),
    #[error("missing parameter: {0}")]
    /// Missing parameter
    MissingParameter(&'static str),
    #[error("invalid parameter: {0}")]
    /// Invalid parameter
    InvalidParameter(&'static str),

    #[cfg(feature = "full")]
    // Irrecoverable errors
    #[error("storage_cost error: {0}")]
    /// Storage error
    StorageError(#[from] grovedb_storage::error::Error),

    #[error("data corruption error: {0}")]
    /// Corrupted data
    CorruptedData(String),

    #[error("invalid code execution error: {0}")]
    /// Invalid code execution
    InvalidCodeExecution(&'static str),
    #[error("corrupted code execution error: {0}")]
    /// Corrupted code execution
    CorruptedCodeExecution(&'static str),

    #[error("invalid batch operation error: {0}")]
    /// Invalid batch operation
    InvalidBatchOperation(&'static str),

    #[error("delete up tree stop height more than initial path size error: {0}")]
    /// Delete up tree stop height more than initial path size
    DeleteUpTreeStopHeightMoreThanInitialPathSize(String),

    #[error("deleting non empty tree error: {0}")]
    /// Deleting non empty tree
    DeletingNonEmptyTree(&'static str),

    #[error("clearing tree with subtrees not allowed error: {0}")]
    /// Clearing tree with subtrees not allowed
    ClearingTreeWithSubtreesNotAllowed(&'static str),

    // Client allowed errors
    #[error("just in time element flags client error: {0}")]
    /// Just in time element flags client error
    JustInTimeElementFlagsClientError(String),

    #[error("split removal bytes client error: {0}")]
    /// Split removal bytes client error
    SplitRemovalBytesClientError(String),

    #[error("client returned non client error: {0}")]
    /// Client returned non client error
    ClientReturnedNonClientError(String),

    #[error("override not allowed error: {0}")]
    /// Override not allowed
    OverrideNotAllowed(&'static str),

    #[error("path not found in cache for estimated costs: {0}")]
    /// Path not found in cache for estimated costs
    PathNotFoundInCacheForEstimatedCosts(String),

    // Support errors
    #[error("not supported: {0}")]
    /// Not supported
    NotSupported(String),

    // Merk errors
    #[error("merk error: {0}")]
    /// Merk error
    MerkError(grovedb_merk::error::Error),
}
