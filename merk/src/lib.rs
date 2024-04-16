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

//! High-performance Merkle key/value store

// #![deny(missing_docs)]

#[cfg(feature = "full")]
extern crate core;

/// The top-level store API.
#[cfg(feature = "full")]
mod merk;

#[cfg(feature = "full")]
pub use crate::merk::{chunks::ChunkProducer, options::MerkOptions, restore::Restorer};

/// Provides a container type that allows temporarily taking ownership of a
/// value.
// TODO: move this into its own crate
#[cfg(feature = "full")]
pub mod owner;
/// Algorithms for generating and verifying Merkle proofs.
#[cfg(any(feature = "full", feature = "verify"))]
pub mod proofs;

/// Various helpers useful for tests or benchmarks.
#[cfg(feature = "full")]
pub mod test_utils;

/// The core tree data structure.
#[cfg(any(feature = "full", feature = "verify"))]
pub mod tree;

/// Errors
#[cfg(any(feature = "full", feature = "verify"))]
pub mod error;

/// Estimated costs
#[cfg(any(feature = "full", feature = "verify"))]
pub mod estimated_costs;

#[cfg(feature = "full")]
mod visualize;

#[cfg(feature = "full")]
pub use ed;
#[cfg(feature = "full")]
pub use error::Error;
#[cfg(any(feature = "full", feature = "verify"))]
pub use proofs::query::execute_proof;
#[cfg(any(feature = "full", feature = "verify"))]
pub use proofs::query::verify_query;
#[cfg(feature = "full")]
pub use tree::{
    BatchEntry, Link, MerkBatch, Op, PanicSource, HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32,
    HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};
#[cfg(any(feature = "full", feature = "verify"))]
pub use tree::{CryptoHash, TreeFeatureType};

#[cfg(feature = "full")]
pub use crate::merk::{
    defaults::ROOT_KEY_KEY,
    prove::{ProofConstructionResult, ProofWithoutEncodingResult},
    IsSumTree, KVIterator, Merk, MerkType, RootHashKeyAndSum,
};
#[cfg(feature = "full")]
pub use crate::visualize::VisualizeableMerk;
