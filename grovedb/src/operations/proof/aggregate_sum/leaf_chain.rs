//! Leaf-chain walker: descends `path_query.path` via single-key existence
//! proofs and delegates to the merk-level sum verifier at the leaf
//! merk. Drives the single-`i64` entry point
//! [`crate::GroveDb::verify_aggregate_sum_query`].
//!
//! Mirror of [`super::super::aggregate_count::leaf_chain`] for the
//! `ProvableSumTree` flavor.
//!
//! V0 (`MerkOnlyLayerProof`) envelopes are rejected at the entry-point
//! gate in [`super::mod`] before they reach this walker — V0 predates
//! the aggregate-sum feature and cannot legitimately carry one.

use grovedb_merk::CryptoHash;
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{
        aggregate_sum::helpers::{
            enforce_lower_chain, expect_merk_bytes, verify_single_key_layer_proof_v0,
            verify_sum_leaf,
        },
        LayerProof,
    },
    Error, PathQuery,
};

/// Walk `path_query.path` layer by layer through `layer.lower_layers`,
/// verifying a single-key existence proof at each non-leaf depth and
/// delegating to [`verify_sum_leaf`] at the leaf. At each non-leaf step,
/// the chain check `combine_hash(H(value), lower_root) ==
/// parent_value_hash` ties the layer's sum to the GroveDB root hash.
pub(super) fn verify_v1_leaf_chain(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    inner_range: &QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, i64), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;

    if depth == path_keys.len() {
        // Strict-shape gate: a leaf-shape aggregate-sum proof terminates
        // in the merk that holds the actual count proof; that merk is a
        // *leaf* of the GroveDB-proof envelope and must carry no further
        // `lower_layers`. Without this check, an attacker can attach
        // arbitrary unverified `LayerProof`s under the leaf and produce
        // byte-distinct envelopes that all verify to the same `(root,
        // sum)`, harming determinism (caching, deduplication) and
        // enlarging the attack surface for downstream consumers that
        // syntactically scan proof structure.
        if !layer.lower_layers.is_empty() {
            return Err(Error::InvalidProof(
                path_query.clone(),
                "aggregate-sum proof contains unexpected lower layers below the leaf merk"
                    .to_string(),
            ));
        }
        return verify_sum_leaf(merk_bytes, inner_range, path_query);
    }

    let next_key = path_keys[depth].to_vec();
    // Strict-shape gate: at each non-leaf depth the honest prover
    // emits exactly one `lower_layers` entry — the descent into the
    // next path key. Reject any other shape (extra siblings, missing
    // descent, or descent under a different key) so the verified
    // path-prefix is unambiguous and proofs are uniquely byte-shaped.
    if layer.lower_layers.len() != 1 || !layer.lower_layers.contains_key(&next_key) {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-sum proof has unexpected lower-layer shape at depth {} (expected \
                 exactly one entry for path key {})",
                depth,
                hex::encode(&next_key)
            ),
        ));
    }
    let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
        verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;

    let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-sum proof missing lower layer for path key {}",
                hex::encode(&next_key)
            ),
        )
    })?;
    let (lower_hash, sum) = verify_v1_leaf_chain(
        lower_layer,
        path_query,
        path_keys,
        depth + 1,
        inner_range,
        grove_version,
    )?;

    let is_terminal = depth + 1 == path_keys.len();
    enforce_lower_chain(
        path_query,
        &next_key,
        &proven_value_bytes,
        &lower_hash,
        &parent_proof_hash,
        is_terminal,
        grove_version,
    )?;

    Ok((parent_root_hash, sum))
}
