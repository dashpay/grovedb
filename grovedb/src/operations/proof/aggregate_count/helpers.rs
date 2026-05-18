//! Aggregate-count-specific helpers. Items shared with the sum and
//! combined axes (the multi-key outer walker, the single-key descent,
//! the merk-bytes unwrapper, the `OuterMatch` row type) live in
//! [`super::super::aggregate_common`] — this file re-exports them via
//! thin wrappers that supply the axis-specific diagnostic label and
//! holds only the genuinely axis-specific helpers below.
//!
//! - [`verify_count_leaf`] — delegate to the merk-level count verifier.
//! - [`enforce_lower_chain`] — `combine_hash(H(value), lower_root) ==
//!   parent_value_hash` plus the terminal-type gate that limits the
//!   final element to `ProvableCountTree` / `ProvableCountSumTree`. The
//!   axis-specific terminal-type set is why this helper stays per-axis.

use grovedb_merk::{
    proofs::query::aggregate_count::verify_aggregate_count_on_range_proof,
    tree::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

// Re-export axis-agnostic helpers from the shared module so existing
// callers in `leaf_chain.rs` / `per_key.rs` keep their `use
// super::helpers::*` imports unchanged.
pub(super) use super::super::aggregate_common::{verify_single_key_layer_proof_v0, OuterMatch};

use crate::{operations::proof::ProofBytes, Element, Error, PathQuery};

/// Verify the leaf layer: bytes are the encoded count-proof Op stream;
/// the inner range is the same one the prover counted over.
pub(super) fn verify_count_leaf(
    leaf_bytes: &[u8],
    inner_range: &QueryItem,
    path_query: &PathQuery,
) -> Result<(CryptoHash, u64), Error> {
    let (root_hash, count) = verify_aggregate_count_on_range_proof(leaf_bytes, inner_range)
        .unwrap()
        .map_err(|e| {
            Error::InvalidProof(
                path_query.clone(),
                format!("aggregate-count leaf proof failed to verify: {}", e),
            )
        })?;
    Ok((root_hash, count))
}

/// Aggregate-count axis label used by the shared diagnostic-prefix
/// helpers in `aggregate_common`.
const AXIS_LABEL: &str = "aggregate-count";

/// Thin wrapper around [`super::super::aggregate_common::expect_merk_bytes`]
/// that supplies the aggregate-count axis label.
pub(super) fn expect_merk_bytes<'a>(
    proof_bytes: &'a ProofBytes,
    path_query: &PathQuery,
) -> Result<&'a [u8], Error> {
    super::super::aggregate_common::expect_merk_bytes(proof_bytes, path_query, AXIS_LABEL)
}

/// Thin wrapper around
/// [`super::super::aggregate_common::execute_carrier_layer_proof`] that
/// supplies the aggregate-count axis label.
pub(super) fn execute_carrier_layer_proof(
    merk_bytes: &[u8],
    outer_items: &[QueryItem],
    left_to_right: bool,
    outer_limit: Option<u16>,
    path_query: &PathQuery,
) -> Result<(CryptoHash, Vec<OuterMatch>), Error> {
    super::super::aggregate_common::execute_carrier_layer_proof(
        merk_bytes,
        outer_items,
        left_to_right,
        outer_limit,
        path_query,
        AXIS_LABEL,
    )
}

/// Enforce the layer-chain hash equality: the parent merk's recorded
/// value_hash for the tree element must equal `combine_hash(H(value),
/// lower_layer_root_hash)`. This is what makes the count cryptographically
/// bound to the GroveDB root hash — the leaf count proof's reconstructed
/// `lower_hash` must agree with the parent's commitment, transitively up to
/// the root.
///
/// Intermediate path elements may be any tree type — the GroveDB grove can
/// route through Normal/Sum/Count/etc. trees on the way down to the
/// provable-count leaf. At the terminal layer (passed `is_terminal =
/// true` when the next descent goes into the actual leaf merk), the
/// element MUST deserialize to `ProvableCountTree` or
/// `ProvableCountSumTree`. Without this, an empty Merk-backed tree of any
/// other type at the leaf accepts a forged empty leaf proof — every
/// empty Merk-backed tree has `inner_root = NULL_HASH` and so its stored
/// `value_hash = combine_hash(H(bytes), NULL_HASH)` matches uniformly,
/// and the verifier would silently return `count = 0` for a
/// non-ProvableCount* leaf (type-confusion soundness gap).
pub(super) fn enforce_lower_chain(
    path_query: &PathQuery,
    target_key: &[u8],
    proven_value_bytes: &[u8],
    lower_hash: &CryptoHash,
    parent_proof_hash: &CryptoHash,
    is_terminal: bool,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    let element = Element::deserialize(proven_value_bytes, grove_version)
        .map_err(|e| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "non-leaf proof's element at key {} failed to deserialize: {}",
                    hex::encode(target_key),
                    e
                ),
            )
        })?
        .into_underlying();
    if is_terminal {
        if !matches!(
            element,
            Element::ProvableCountTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableCountProvableSumTree(..)
        ) {
            return Err(Error::InvalidProof(
                path_query.clone(),
                format!(
                    "aggregate-count proof's terminal path element at key {} must be a \
                     ProvableCountTree, ProvableCountSumTree, or ProvableCountProvableSumTree \
                     (got {}); a count aggregate is only meaningful against a tree that binds \
                     its count into the node hash",
                    hex::encode(target_key),
                    element.type_str()
                ),
            ));
        }
    } else if !element.is_any_tree() {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-count proof's intermediate path element at key {} is not a tree \
                 element (got {}); count queries can only descend through tree elements",
                hex::encode(target_key),
                element.type_str()
            ),
        ));
    }

    let value_h = value_hash(proven_value_bytes).value().to_owned();
    let combined = combine_hash(&value_h, lower_hash).value().to_owned();
    if combined != *parent_proof_hash {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-count proof chain mismatch at key {}: parent recorded value_hash \
                 {} but combine_hash(H(value), lower_root) is {}",
                hex::encode(target_key),
                hex::encode(parent_proof_hash),
                hex::encode(combined)
            ),
        ));
    }
    Ok(())
}
