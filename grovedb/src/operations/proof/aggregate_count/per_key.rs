//! Per-key carrier walker: dispatches leaf vs. carrier shape based on
//! the [`AggregateCountClassification`], walks the path-prefix layers
//! with single-key descents, then either emits a single
//! `(empty_key, u64)` entry (leaf shape) or executes the carrier's
//! multi-key merk proof and recurses through `subquery_path` per
//! matched outer key (carrier shape).
//!
//! V0 (`MerkOnlyLayerProof`) envelopes are rejected at the entry-point
//! gate in [`super::mod`] before they reach this walker — V0 predates
//! the aggregate-count feature and cannot legitimately carry one.

use grovedb_merk::CryptoHash;
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{
        aggregate_count::{
            classification::AggregateCountClassification,
            helpers::{
                enforce_lower_chain, execute_carrier_layer_proof, expect_merk_bytes,
                verify_count_leaf, verify_single_key_layer_proof_v0, OuterMatch,
            },
        },
        LayerProof,
    },
    Error, PathQuery,
};

/// Entry point for the per-key carrier walker. Wraps the recursive
/// [`verify_v1_per_key`] with `depth = 0`.
pub(super) fn verify_v1_with_classification(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    classification: &AggregateCountClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
    verify_v1_per_key(
        layer,
        path_query,
        path_keys,
        0,
        classification,
        grove_version,
    )
}

/// Recursive worker for the per-key walk. While `depth < path_keys.len()`
/// it performs a single-key descent (same as the leaf chain walker);
/// once it reaches the carrier merk it dispatches on the classification
/// shape: leaf collapses to a one-entry result vector, carrier executes
/// the multi-key proof and fans out via [`verify_v1_carrier_layer`].
fn verify_v1_per_key(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    classification: &AggregateCountClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;

    if depth < path_keys.len() {
        let next_key = path_keys[depth].to_vec();
        let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
            verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;
        let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "aggregate-count proof missing lower layer for path key {}",
                    hex::encode(&next_key)
                ),
            )
        })?;
        let (lower_hash, results) = verify_v1_per_key(
            lower_layer,
            path_query,
            path_keys,
            depth + 1,
            classification,
            grove_version,
        )?;
        enforce_lower_chain(
            path_query,
            &next_key,
            &proven_value_bytes,
            &lower_hash,
            &parent_proof_hash,
            grove_version,
        )?;
        return Ok((parent_root_hash, results));
    }

    match &classification.carrier_outer_items {
        None => {
            let (root, count) =
                verify_count_leaf(merk_bytes, &classification.leaf_inner_range, path_query)?;
            Ok((root, vec![(Vec::new(), count)]))
        }
        Some(outer_items) => verify_v1_carrier_layer(
            layer,
            merk_bytes,
            path_query,
            outer_items,
            classification,
            grove_version,
        ),
    }
}

/// Execute the carrier's multi-key outer merk proof, then for each
/// matched outer key descend the `subquery_path` (if any) and the
/// leaf count proof, enforcing the chain at each step. Returns one
/// `(outer_key, count)` entry per match in query-direction order.
fn verify_v1_carrier_layer(
    layer: &LayerProof,
    merk_bytes: &[u8],
    path_query: &PathQuery,
    outer_items: &[QueryItem],
    classification: &AggregateCountClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
    let (carrier_root, matched) = execute_carrier_layer_proof(
        merk_bytes,
        outer_items,
        classification.carrier_left_to_right,
        path_query,
    )?;

    // Invariant from `classify_aggregate_count_path_query`: whenever
    // `carrier_outer_items` is `Some`, `carrier_subquery_path` is also
    // `Some` (possibly empty). Surface a verification failure rather
    // than aborting if the invariant ever drifts.
    let subquery_path = classification
        .carrier_subquery_path
        .as_ref()
        .ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                "carrier aggregate-count classification missing subquery_path".to_string(),
            )
        })?;

    let mut results = Vec::with_capacity(matched.len());
    for OuterMatch {
        outer_key,
        value_bytes,
        commitment_hash,
    } in matched
    {
        let lower_layer = layer.lower_layers.get(&outer_key).ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier aggregate-count proof missing lower layer for outer key {}",
                    hex::encode(&outer_key)
                ),
            )
        })?;

        let (lower_root, count) = verify_v1_subquery_path(
            lower_layer,
            path_query,
            subquery_path,
            0,
            &classification.leaf_inner_range,
            grove_version,
        )?;

        enforce_lower_chain(
            path_query,
            &outer_key,
            &value_bytes,
            &lower_root,
            &commitment_hash,
            grove_version,
        )?;
        results.push((outer_key, count));
    }

    Ok((carrier_root, results))
}

/// Walk the carrier's `subquery_path` (zero or more intermediate
/// single-key layers between an outer match and the leaf merk),
/// terminating in the merk-level count verifier.
fn verify_v1_subquery_path(
    layer: &LayerProof,
    path_query: &PathQuery,
    subquery_path: &[Vec<u8>],
    depth: usize,
    inner_range: &QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;
    if depth == subquery_path.len() {
        return verify_count_leaf(merk_bytes, inner_range, path_query);
    }
    let next_key = subquery_path[depth].clone();
    let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
        verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;
    let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "carrier aggregate-count proof missing subquery_path layer for key {}",
                hex::encode(&next_key)
            ),
        )
    })?;
    let (lower_hash, count) = verify_v1_subquery_path(
        lower_layer,
        path_query,
        subquery_path,
        depth + 1,
        inner_range,
        grove_version,
    )?;
    enforce_lower_chain(
        path_query,
        &next_key,
        &proven_value_bytes,
        &lower_hash,
        &parent_proof_hash,
        grove_version,
    )?;
    Ok((parent_root_hash, count))
}
