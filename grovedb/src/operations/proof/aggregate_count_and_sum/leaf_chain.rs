//! Leaf-chain walker: descends `path_query.path` via single-key
//! existence proofs and delegates to the merk-level combined-aggregate
//! verifier at the leaf merk. Drives the single-`(u64, i64)` entry
//! point [`crate::GroveDb::verify_aggregate_count_and_sum_query`].
//!
//! Mirror of [`super::super::aggregate_sum::leaf_chain`] for the
//! dual-axis PCPS host. V0 (`MerkOnlyLayerProof`) envelopes are
//! rejected at the entry-point gate in [`super::mod`] before they
//! reach this walker — V0 predates the combined-aggregate feature
//! and cannot legitimately carry one.

use grovedb_merk::CryptoHash;
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{
        aggregate_count_and_sum::helpers::{
            enforce_lower_chain, expect_merk_bytes, verify_count_and_sum_leaf,
            verify_single_key_layer_proof_v0,
        },
        LayerProof,
    },
    Error, PathQuery,
};

/// Walk `path_query.path` layer by layer through `layer.lower_layers`,
/// verifying a single-key existence proof at each non-leaf depth and
/// delegating to [`verify_count_and_sum_leaf`] at the leaf. At each
/// non-leaf step, the chain check
/// `combine_hash(H(value), lower_root) == parent_value_hash` ties
/// the layer's `(count, sum)` to the GroveDB root hash.
pub(super) fn verify_v1_leaf_chain(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    inner_range: &QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64, i64), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;

    if depth == path_keys.len() {
        // Strict-shape gate: a combined-aggregate proof terminates in
        // the merk that holds the actual aggregate proof; that merk
        // is a *leaf* of the GroveDB-proof envelope and must carry no
        // further `lower_layers`. Without this check, an attacker can
        // attach arbitrary unverified `LayerProof`s under the leaf and
        // produce byte-distinct envelopes that all verify to the same
        // `(root, count, sum)`, harming determinism and enlarging
        // the attack surface for downstream consumers that
        // syntactically scan proof structure.
        if !layer.lower_layers.is_empty() {
            return Err(Error::InvalidProof(
                path_query.clone(),
                "combined-aggregate proof contains unexpected lower layers below the leaf merk"
                    .to_string(),
            ));
        }
        return verify_count_and_sum_leaf(merk_bytes, inner_range, path_query);
    }

    let next_key = path_keys[depth].to_vec();
    // Strict-shape gate (size): at each non-leaf depth the honest
    // prover emits exactly one `lower_layers` entry — the descent
    // into the next path key.
    if layer.lower_layers.len() != 1 {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "combined-aggregate proof has {} lower-layer entries at depth {} (expected \
                 exactly one entry for path key {})",
                layer.lower_layers.len(),
                depth,
                hex::encode(&next_key)
            ),
        ));
    }
    let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
        verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;

    // Strict-shape gate (key): the sole entry must be under the
    // expected descent key.
    let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "combined-aggregate proof's sole lower-layer entry at depth {} is not keyed \
                 by the expected path key {}",
                depth,
                hex::encode(&next_key)
            ),
        )
    })?;
    let (lower_hash, count, sum) = verify_v1_leaf_chain(
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

    Ok((parent_root_hash, count, sum))
}
