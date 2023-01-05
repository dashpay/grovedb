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
