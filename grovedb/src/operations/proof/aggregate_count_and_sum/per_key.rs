//! Per-key carrier walker: dispatches leaf vs. carrier shape based on
//! the [`AggregateCountAndSumClassification`], walks the path-prefix
//! layers with single-key descents, then either emits a single
//! `(empty_key, count, sum)` triple (leaf shape) or executes the
//! carrier's multi-key merk proof and recurses through `subquery_path`
//! per matched outer key (carrier shape).
//!
//! Combined-side mirror of
//! [`crate::operations::proof::aggregate_count::per_key`] /
//! [`crate::operations::proof::aggregate_sum::per_key`].
//!
//! V0 (`MerkOnlyLayerProof`) envelopes are rejected at the entry-point
//! gate in [`super::mod`] before they reach this walker.
//!
//! **Dual-axis invariant:** unlike the sum-side helper which accepts
//! the broader sum-bearing set (`ProvableSumTree` or PCPS), the
//! combined-aggregate terminal check (in
//! [`super::helpers::enforce_lower_chain`]) rejects every leaf-target
//! element that isn't a `ProvableCountProvableSumTree`. Only PCPS hosts
//! bind BOTH a count and a sum into the node hash, so only PCPS can
//! ground a combined-aggregate proof.

use grovedb_merk::CryptoHash;
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{
        aggregate_count_and_sum::{
            classification::AggregateCountAndSumClassification,
            helpers::{
                enforce_lower_chain, execute_carrier_layer_proof, expect_merk_bytes,
                verify_count_and_sum_leaf, verify_single_key_layer_proof_v0, OuterMatch,
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
    classification: &AggregateCountAndSumClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64, i64)>), Error> {
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
/// it performs a single-key descent; once it reaches the carrier merk
/// it dispatches on the classification shape.
fn verify_v1_per_key(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    classification: &AggregateCountAndSumClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64, i64)>), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;

    if depth < path_keys.len() {
        let next_key = path_keys[depth].to_vec();
        let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
            verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;
        let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "combined-aggregate proof missing lower layer for path key {}",
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
        // Terminal here only in the LEAF shape — where the path's
        // final element is itself the
        // ProvableCountProvableSumTree. In carrier shape the final
        // path element is the carrier (which could be any tree
        // containing PCPS children); the terminal type check is
        // enforced deeper, on each outer-key match in
        // `verify_v1_carrier_layer` / `verify_v1_subquery_path`.
        let is_terminal =
            depth + 1 == path_keys.len() && classification.carrier_outer_items.is_none();
        enforce_lower_chain(
            path_query,
            &next_key,
            &proven_value_bytes,
            &lower_hash,
            &parent_proof_hash,
            is_terminal,
            grove_version,
        )?;
        return Ok((parent_root_hash, results));
    }

    match &classification.carrier_outer_items {
        None => {
            let (root, count, sum) = verify_count_and_sum_leaf(
                merk_bytes,
                &classification.leaf_inner_range,
                path_query,
            )?;
            Ok((root, vec![(Vec::new(), count, sum)]))
        }
        Some(outer_items) => verify_v1_carrier_layer(
            layer,
            merk_bytes,
            path_query,
            outer_items,
            path_query.query.limit,
            classification,
            grove_version,
        ),
    }
}

/// Execute the carrier's multi-key outer merk proof, then for each
/// matched outer key descend the `subquery_path` (if any) and the
/// leaf combined-aggregate proof, enforcing the chain at each step.
/// Returns one `(outer_key, count, sum)` triple per match in
/// query-direction order.
fn verify_v1_carrier_layer(
    layer: &LayerProof,
    merk_bytes: &[u8],
    path_query: &PathQuery,
    outer_items: &[QueryItem],
    outer_limit: Option<u16>,
    classification: &AggregateCountAndSumClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64, i64)>), Error> {
    let (carrier_root, matched) = execute_carrier_layer_proof(
        merk_bytes,
        outer_items,
        classification.carrier_left_to_right,
        outer_limit,
        path_query,
    )?;

    let subquery_path = classification
        .carrier_subquery_path
        .as_ref()
        .ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                "carrier combined-aggregate classification missing subquery_path".to_string(),
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
                    "carrier combined-aggregate proof missing lower layer for outer key {}",
                    hex::encode(&outer_key)
                ),
            )
        })?;

        let (lower_root, count, sum) = verify_v1_subquery_path(
            lower_layer,
            path_query,
            subquery_path,
            0,
            &classification.leaf_inner_range,
            grove_version,
        )?;

        // Terminal here when the subquery_path is empty — the outer
        // match goes directly to the leaf merk. The terminal-type
        // gate in `enforce_lower_chain` here rejects every
        // non-PCPS leaf, enforcing the dual-axis invariant.
        let is_terminal = subquery_path.is_empty();
        enforce_lower_chain(
            path_query,
            &outer_key,
            &value_bytes,
            &lower_root,
            &commitment_hash,
            is_terminal,
            grove_version,
        )?;
        results.push((outer_key, count, sum));
    }

    Ok((carrier_root, results))
}

/// Walk the carrier's `subquery_path` (zero or more intermediate
/// single-key layers between an outer match and the leaf merk),
/// terminating in the merk-level combined-aggregate verifier. The
/// terminal-type gate in `enforce_lower_chain` rejects every
/// leaf-target element that isn't a `ProvableCountProvableSumTree`.
fn verify_v1_subquery_path(
    layer: &LayerProof,
    path_query: &PathQuery,
    subquery_path: &[Vec<u8>],
    depth: usize,
    inner_range: &QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64, i64), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;
    if depth == subquery_path.len() {
        return verify_count_and_sum_leaf(merk_bytes, inner_range, path_query);
    }
    let next_key = subquery_path[depth].clone();
    let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
        verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;
    let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "carrier combined-aggregate proof missing subquery_path layer for key {}",
                hex::encode(&next_key)
            ),
        )
    })?;
    let (lower_hash, count, sum) = verify_v1_subquery_path(
        lower_layer,
        path_query,
        subquery_path,
        depth + 1,
        inner_range,
        grove_version,
    )?;
    let is_terminal = depth + 1 == subquery_path.len();
    enforce_lower_chain(
        path_query,
        &next_key,
        &proven_value_bytes,
        &lower_hash,
        &parent_proof_hash,
        is_terminal,
        grove_version,
    )?;
    Ok((parent_root_hash, count, sum))
}
