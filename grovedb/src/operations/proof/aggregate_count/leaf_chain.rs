//! Leaf-chain walker: descends `path_query.path` via single-key existence
//! proofs and delegates to the merk-level count verifier at the leaf
//! merk. Used by the legacy single-`u64` entry point
//! [`crate::GroveDb::verify_aggregate_count_query`].
//!
//! The carrier-shape per-key walker in [`super::per_key`] reuses the
//! same single-key descent helpers (`verify_single_key_layer_proof_v0`,
//! `enforce_lower_chain`) for path-prefix layers and is structurally
//! parallel, but emits a `Vec<(outer_key, count)>` instead of one `u64`.

use grovedb_merk::CryptoHash;
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{
        aggregate_count::helpers::{
            enforce_lower_chain, expect_merk_bytes, verify_count_leaf,
            verify_single_key_layer_proof_v0,
        },
        LayerProof,
    },
    Error, PathQuery,
};

/// Walk `path_query.path` layer by layer through `layer.lower_layers`,
/// verifying a single-key existence proof at each non-leaf depth and
/// delegating to [`verify_count_leaf`] at the leaf. At each non-leaf
/// step, the chain check `combine_hash(H(value), lower_root) ==
/// parent_value_hash` ties the layer's count to the GroveDB root hash.
pub(super) fn verify_v1_leaf_chain(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    inner_range: &QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;

    if depth == path_keys.len() {
        return verify_count_leaf(merk_bytes, inner_range, path_query);
    }

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
    let (lower_hash, count) = verify_v1_leaf_chain(
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

    Ok((parent_root_hash, count))
}
