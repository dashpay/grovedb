// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Errors

/// Errors
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
    InvalidProof(&'static str),
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
    StorageError(#[from] storage::error::Error),

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

    // Client allowed errors
    #[error("just in time element flags client error: {0}")]
    /// Just in time element flags client error
    JustInTimeElementFlagsClientError(&'static str),

    #[error("split removal bytes client error: {0}")]
    /// Split removal bytes client error
    SplitRemovalBytesClientError(&'static str),

    #[error("client returned non client error: {0}")]
    /// Client returned non client error
    ClientReturnedNonClientError(&'static str),

    #[error("override not allowed error: {0}")]
    /// Override not allowed
    OverrideNotAllowed(&'static str),

    #[error("path not found in cache for estimated costs: {0}")]
    /// Path not found in cache for estimated costs
    PathNotFoundInCacheForEstimatedCosts(String),

    // Support errors
    #[error("not supported: {0}")]
    /// Not supported
    NotSupported(&'static str),

    // Merk errors
    #[error("merk error: {0}")]
    /// Merk error
    MerkError(merk::error::Error),
}
