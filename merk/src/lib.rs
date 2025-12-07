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

/// The top-level store API.
#[cfg(feature = "minimal")]
pub mod merk;

#[cfg(feature = "grovedbg")]
pub mod debugger;

#[cfg(feature = "minimal")]
pub use crate::merk::{chunks::ChunkProducer, options::MerkOptions, restore::Restorer};

/// Provides a container type that allows temporarily taking ownership of a
/// value.
// TODO: move this into its own crate
#[cfg(feature = "minimal")]
pub mod owner;
/// Algorithms for generating and verifying Merkle proofs.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod proofs;

/// Various helpers useful for tests or benchmarks.
#[cfg(feature = "test_utils")]
pub mod test_utils;

/// The core tree data structure.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod tree;

/// Errors
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod error;

/// Estimated costs
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod estimated_costs;

mod element;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod tree_type;
#[cfg(feature = "minimal")]
mod visualize;

#[cfg(feature = "minimal")]
pub use ed;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use error::Error;
#[cfg(feature = "minimal")]
pub use tree::{
    BatchEntry, Link, MerkBatch, Op, PanicSource, HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32,
    HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree::{CryptoHash, TreeFeatureType};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree_type::MaybeTree;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use tree_type::TreeType;

#[cfg(feature = "minimal")]
pub use crate::merk::{
    defaults::ROOT_KEY_KEY,
    prove::{ProofConstructionResult, ProofWithoutEncodingResult},
    KVIterator, Merk, MerkType, RootHashKeyAndAggregateData,
};
#[cfg(feature = "minimal")]
pub use crate::visualize::VisualizeableMerk;
