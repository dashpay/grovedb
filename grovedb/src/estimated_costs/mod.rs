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

//! Estimated costs

mod average_case_costs;
#[cfg(all(test, feature = "full", grovedb_test_engine))]
mod stop_point_tests;
#[cfg(all(test, feature = "rocksdb_storage"))]
mod storage_parameter_tests;
mod worst_case_costs;

use std::collections::{HashMap, HashSet};

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions, tree_type::TreeType, HASH_BLOCK_SIZE_U32,
    HASH_LENGTH_U32,
};
use grovedb_private_document_store::EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS;
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;
use itertools::Itertools;

use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    Element, Error,
};

/// The blocks a hash of `bytes` bytes takes: one call for each.
pub(crate) fn hash_blocks(bytes: u32) -> u32 {
    1 + bytes.saturating_sub(1) / HASH_BLOCK_SIZE_U32
}

/// The bytes that the length of a byte string of `len` bytes takes where an
/// element is serialized: one below 251, three up to 65,535, five beyond.
/// A cost counts a length by `required_space`, which is two bytes from 128
/// to 16,383, three to 2,097,151, four to 268,435,455 and five beyond, so
/// the two differ from 128 bytes to 16,383 and from 65,536 to 268,435,455.
pub(crate) fn serialized_length_len(len: u32) -> u32 {
    match len {
        0..=250 => 1,
        251..=65_535 => 3,
        _ => 5,
    }
}

/// The hash calls that the roots of the secondaries of an indexed tree add
/// to the hash of its element, over those of a tree of one Merk. Such a
/// tree combines the hash of its value with its root, 64 bytes in one
/// block. An indexed tree combines the hash of its value with the root of
/// its primary and a third hash, 96 bytes in two blocks: one call more.
/// The third is the root of its secondary, or for a tree that names its
/// axes the digest of their roots, which is hashed from one byte and 33 for
/// each axis.
pub(crate) fn indexed_tree_hash_calls(tree_type: TreeType, axes: usize) -> Result<u32, Error> {
    Ok(match tree_type {
        TreeType::ProvableCountProvableSumIndexedTree
        | TreeType::ProvableCountProvableBigSumIndexedTree => {
            // An indexed tree names one to three axes. An element that
            // names more is no tree GroveDB stores.
            if axes > MOST_AXES_OF_AN_INDEXED_TREE {
                return Err(Error::InvalidInput(
                    "an indexed tree is estimated with more axes than one can name",
                ));
            }
            1 + hash_blocks(1 + 33 * axes as u32)
        }
        indexed if indexed.is_indexed_primary() => 1,
        _ => 0,
    })
}

/// The most axes an indexed tree can name.
pub(crate) const MOST_AXES_OF_AN_INDEXED_TREE: usize = 3;

/// Adds `part` to `total` in full, or refuses and leaves `total` as it
/// was. `+=` of a cost stops at the most a field holds, and a sum that
/// stopped there is no count. The costs are those of a finalize and of the
/// open of a Merk, which remove no bytes (FZ4): a part that removes some is
/// refused.
pub(crate) fn add_in_full(total: &mut OperationCost, part: OperationCost) -> Result<(), Error> {
    let overflow = || {
        Error::MerkError(grovedb_merk::Error::Overflow(
            "overflow for the cost of an estimate",
        ))
    };
    if part.storage_cost.removed_bytes.has_removal() {
        return Err(Error::CorruptedCodeExecution(
            "the estimate of a finalize or of an open has removed bytes",
        ));
    }
    let sinsemilla = total
        .sinsemilla_hash_calls
        .checked_add(part.sinsemilla_hash_calls)
        .ok_or_else(overflow)?;
    let seeks = total
        .seek_count
        .checked_add(part.seek_count)
        .ok_or_else(overflow)?;
    let loaded = total
        .storage_loaded_bytes
        .checked_add(part.storage_loaded_bytes)
        .ok_or_else(overflow)?;
    let hashes = total
        .hash_node_calls
        .checked_add(part.hash_node_calls)
        .ok_or_else(overflow)?;
    let added = total
        .storage_cost
        .added_bytes
        .checked_add(part.storage_cost.added_bytes)
        .ok_or_else(overflow)?;
    let replaced = total
        .storage_cost
        .replaced_bytes
        .checked_add(part.storage_cost.replaced_bytes)
        .ok_or_else(overflow)?;
    total.seek_count = seeks;
    total.storage_loaded_bytes = loaded;
    total.hash_node_calls = hashes;
    total.storage_cost.added_bytes = added;
    total.storage_cost.replaced_bytes = replaced;
    total.sinsemilla_hash_calls = sinsemilla;
    Ok(())
}

/// An estimate in full, or its refusal with nothing added: an estimate
/// that grove version 6 selects leaves no part of itself in what it
/// returns where it refuses.
pub(crate) fn in_full_or_refused(estimate: CostResult<(), Error>) -> CostResult<(), Error> {
    match estimate.value {
        Ok(()) => estimate,
        Err(error) => Err(error).wrap_with_cost(OperationCost::default()),
    }
}

/// A cost that is hash calls alone.
pub(crate) fn hash_calls(calls: u32) -> OperationCost {
    OperationCost {
        hash_node_calls: calls,
        ..Default::default()
    }
}

/// The number of marked elements of one host.
pub(crate) fn marked_count(keys: usize) -> Result<u32, Error> {
    u32::try_from(keys).map_err(|_| {
        Error::MerkError(grovedb_merk::Error::Overflow(
            "more marked elements in one Merk than an estimate can count",
        ))
    })
}

/// The most bytes the cost of an element counts over what the element
/// serializes to. A cost counts each aggregate of a tree at a fixed width, a
/// count and a sum at 8 bytes and a sum of 128 bits at 16, where the element
/// has a varint that can be one byte, and the length of the flags as a
/// varint, which is a byte longer than the element has it from 128 bytes to
/// 250. The longest is an indexed tree by its count and its sum of 128 bits
/// that names its axes: 28 bytes. A worst case that is told how long an
/// element can be takes its node to cost this much more.
pub(crate) const MOST_BYTES_OF_A_COST_OVER_ITS_ELEMENT: u32 = 64;

/// The most bytes an element may serialize to for an estimate to size its
/// node: 65,536 under what 32 bits hold, as `EstimatedLayerSizes::check_sizes`
/// has it for the sizes of a layer. The estimates add the key, the hashes,
/// the feature, a link and the lengths of a node to those bytes, well under
/// that many, in 32 bits.
pub(crate) const MOST_BYTES_OF_A_SIZED_ELEMENT: u32 = u32::MAX - 65_536;

/// `len`, the bytes an element serializes to, as an estimate of grove
/// version 6 takes them: refused where the arithmetic that sizes a node
/// from them would not hold them. The estimates of the grove versions
/// before take any length, and wrap or panic near the end of 32 bits.
pub(crate) fn sized_for_a_node(len: usize) -> Result<u32, Error> {
    u32::try_from(len)
        .ok()
        .filter(|len| *len <= MOST_BYTES_OF_A_SIZED_ELEMENT)
        .ok_or(Error::Overflow(
            "an element longer than an estimate can size a node by",
        ))
}

/// The bytes `value` serializes to, or a refusal where an estimate cannot
/// size a node by them (`sized_for_a_node`). An estimate that grove version
/// 6 selects asks this before it calls a helper that adds in 32 bits
/// unchecked.
pub(crate) fn element_len_for_an_estimate(
    value: &Element,
    grove_version: &GroveVersion,
) -> Result<u32, Error> {
    sized_for_a_node(value.serialized_size(grove_version)?)
}

/// The hash calls of the put of `value` under a key of `key_len` bytes,
/// for the node of the element and nothing above it
/// (`docs/lignindb/cost-model.md`, ES2), counted from the bytes of the
/// element:
///
/// - the hash of the value, which is the element's bytes with their length
///   before them. An element that keeps a list of its referrers is hashed
///   without the list;
/// - for a tree, the hash that combines it with the root of what the tree
///   holds, and for an indexed tree `indexed_tree_hash_calls`; for a
///   private document store the hashes of its empty root;
/// - for a reference, the hash that combines it with the hash of its
///   target. Reading the target is no part of this;
/// - the hash of the key with the value's hash, and the two of the node.
///
/// `serialized_len` is the bytes `value` serializes to, as the caller
/// measured them (`element_len_for_an_estimate`).
pub(crate) fn element_put_hash_calls(
    key_len: u32,
    value: &Element,
    serialized_len: u32,
    grove_version: &GroveVersion,
) -> Result<u32, Error> {
    let overflow = || Error::Overflow("overflow for the hash calls of an element");
    // `serialized_len` is what is hashed for an element that keeps no list
    // of its referrers, which is not encoded again here.
    let len = if value.supports_backward_references() {
        let len = value
            .stripped_of_backward_references()
            .serialized_size(grove_version)?;
        u32::try_from(len).map_err(|_| overflow())?
    } else {
        serialized_len
    };
    let hashed = len
        .checked_add(len.required_space() as u32)
        .ok_or_else(overflow)?;
    let mut calls = hash_blocks(hashed);
    let element = value.underlying();
    if let Some((_, tree_type)) = value.tree_flags_and_type() {
        let axes = match element {
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _)
            | Element::ProvableCountProvableBigSumIndexedTree(_, _, _, axes, _) => axes.len(),
            _ => 1,
        };
        calls += 1 + indexed_tree_hash_calls(tree_type, axes)?;
        if matches!(element, Element::PrivateDocumentStore(..)) {
            calls += EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS;
        }
    } else if element.is_reference() {
        calls += 1;
    }
    let key_and_value_hash = key_len
        .checked_add(key_len.required_space() as u32 + HASH_LENGTH_U32)
        .ok_or_else(overflow)?;
    Ok(calls + hash_blocks(key_and_value_hash) + 2)
}

/// Sets the hash calls of `cost` to those of the put of `value` under
/// `key`. `cost` is an estimate of that put alone, with no node above it,
/// which counted the blocks of a length that is the element's cost: longer
/// than its bytes for an element with a sum, and without the length the
/// bytes are hashed with.
///
/// `serialized_len` is the bytes of `value` as the caller measured them
/// (`element_len_for_an_estimate`).
pub(crate) fn set_element_put_hash_calls(
    cost: &mut OperationCost,
    key: &KeyInfo,
    value: &Element,
    serialized_len: u32,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    cost.hash_node_calls = element_put_hash_calls(
        u32::from(key.max_length()),
        value,
        serialized_len,
        grove_version,
    )?;
    Ok(())
}

/// An order of keys in which two keys are equal exactly where `==` says
/// they are. `KeyInfo`'s own order takes a key that is known and a key that
/// is described by its longest length as equal where their bytes and that
/// length are, which `==` and a hash of the key do not.
fn key_order(key: &KeyInfo) -> (&[u8], bool, u8) {
    match key {
        KeyInfo::KnownKey(key) => (key, false, 0),
        KeyInfo::MaxKeySize {
            unique_id,
            max_size,
        } => (unique_id, true, *max_size),
    }
}

/// The marks a finalize finds after writes that stopped at the Merks at
/// `stop_points`: for each Merk that hosts a marked element, the keys of its
/// marked elements, the deepest Merk first.
///
/// A write marks the host of its stop point. A finalize marks the host of
/// every Merk it rewrites an element of, so every Merk above a stop point
/// hosts a mark. The root Merk has no host.
///
/// A path and a key are told apart as the caller's map of layers tells them
/// apart, by `==`, and the order of the result is of the paths and keys
/// alone: the order of `stop_points` does not show in it.
fn marked_hosts(stop_points: &[KeyInfoPath]) -> Vec<(KeyInfoPath, Vec<KeyInfo>)> {
    let mut marked: HashMap<KeyInfoPath, HashSet<KeyInfo>> = HashMap::new();
    for stop_point in stop_points {
        let mut path = stop_point.clone();
        while let Some((key, parent)) = path.split_last() {
            let (key, parent) = (key.clone(), KeyInfoPath(parent.to_vec()));
            if !marked.entry(parent.clone()).or_default().insert(key) {
                break;
            }
            path = parent;
        }
    }
    fn path_order(path: &KeyInfoPath) -> Vec<(&[u8], bool, u8)> {
        path.0.iter().map(key_order).collect()
    }
    marked
        .into_iter()
        .map(|(host, keys)| {
            let keys = keys
                .into_iter()
                .sorted_by(|a, b| key_order(a).cmp(&key_order(b)))
                .collect();
            (host, keys)
        })
        .sorted_by(|(a, _), (b, _)| {
            b.len()
                .cmp(&a.len())
                .then_with(|| path_order(a).cmp(&path_order(b)))
        })
        .collect()
}

fn missing_layer(estimate: &str, path: &KeyInfoPath) -> Error {
    Error::PathNotFoundInCacheForEstimatedCosts(format!(
        "{estimate}: no layer information for path: {}",
        path.0.iter().map(|k| hex::encode(k.as_slice())).join("/")
    ))
}
