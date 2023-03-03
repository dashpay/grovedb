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

    /// Chunking error
    #[error("chunking error {0}")]
    ChunkingError(&'static str),

    /// Chunk restoring error
    #[error("chunk restoring error {0}")]
    ChunkRestoringError(String),

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
    NotSupported(&'static str),

    /// Request amount exceeded error
    #[error("request amount exceeded error {0}")]
    RequestAmountExceeded(String),

    /// Invalid operation error
    #[error("invalid operation error {0}")]
    InvalidOperation(&'static str),

    /// Specialized costs error
    #[error("specialized costs error {0}")]
    SpecializedCostsError(&'static str),

    /// Client corruption error
    #[error("client corruption error {0}")]
    ClientCorruptionError(String),

    #[cfg(feature = "full")]
    /// Storage error
    #[error("storage error {0}")]
    StorageError(storage::Error),

    // Merk errors
    /// Ed error
    #[error("ed error: {0}")]
    EdError(ed::Error),

    // Costs errors
    /// Costs errors
    #[error("costs error: {0}")]
    CostsError(costs::error::Error),
}
