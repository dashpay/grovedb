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

//! Merk tree hash

#[cfg(any(feature = "full", feature = "verify"))]
use costs::{CostContext, CostsExt, OperationCost};
#[cfg(any(feature = "full", feature = "verify"))]
use integer_encoding::*;

/// The length of a `Hash` (in bytes).
#[cfg(any(feature = "full", feature = "verify"))]
pub const HASH_LENGTH: usize = 32;
#[cfg(feature = "full")]
pub const HASH_LENGTH_X2: usize = 64;
#[cfg(feature = "full")]
pub const HASH_LENGTH_U32: u32 = 32;
#[cfg(feature = "full")]
pub const HASH_LENGTH_U32_X2: u32 = 64;
#[cfg(feature = "full")]
pub const HASH_BLOCK_SIZE: usize = 64;
#[cfg(feature = "full")]
pub const HASH_BLOCK_SIZE_U32: u32 = 64;

/// A zero-filled `Hash`.
#[cfg(any(feature = "full", feature = "verify"))]
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];

/// A cryptographic hash digest.
#[cfg(any(feature = "full", feature = "verify"))]
pub type CryptoHash = [u8; HASH_LENGTH];

#[cfg(any(feature = "full", feature = "verify"))]
/// Hashes a value
pub fn value_hash(value: &[u8]) -> CostContext<CryptoHash> {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();

    let val_length = value.len().encode_var_vec();
    hasher.update(val_length.as_slice());
    hasher.update(value);

    let hashes = 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes as u16,
        ..Default::default()
    })
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Hashes a key/value pair.
///
/// The result is Hash(key_len, key, Hash(value_len, value))
pub fn kv_hash(key: &[u8], value: &[u8]) -> CostContext<CryptoHash> {
    let mut cost = OperationCost::default();

    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();

    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);

    let value_hash = value_hash(value);
    hasher.update(value_hash.unwrap_add_cost(&mut cost).as_slice());

    let hashes = 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());

    cost.hash_node_calls += hashes as u16;
    hash.wrap_with_cost(cost)
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Computes the kv hash given a kv digest
pub fn kv_digest_to_kv_hash(key: &[u8], value_hash: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();

    let key_length = key.len().encode_var_vec();
    hasher.update(key_length.as_slice());
    hasher.update(key);

    hasher.update(value_hash.as_slice());

    let hashes = 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes as u16,
        ..Default::default()
    })
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Hashes a node based on the hash of its key/value pair, the hash of its left
/// child (if any), and the hash of its right child (if any).
pub fn node_hash(
    kv: &CryptoHash,
    left: &CryptoHash,
    right: &CryptoHash,
) -> CostContext<CryptoHash> {
    // TODO: make generic to allow other hashers
    let mut hasher = blake3::Hasher::new();
    hasher.update(kv);
    hasher.update(left);
    hasher.update(right);

    // hashes will always be 2
    let hashes = 2; // 1 + (hasher.count() - 1) / 64;

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: hashes,
        ..Default::default()
    })
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Combines two hash values into one
pub fn combine_hash(hash_one: &CryptoHash, hash_two: &CryptoHash) -> CostContext<CryptoHash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(hash_one);
    hasher.update(hash_two);

    let res = hasher.finalize();
    let mut hash: CryptoHash = Default::default();
    hash.copy_from_slice(res.as_bytes());
    hash.wrap_with_cost(OperationCost {
        hash_node_calls: 1, // as this will fit on exactly 1 block
        ..Default::default()
    })
}
