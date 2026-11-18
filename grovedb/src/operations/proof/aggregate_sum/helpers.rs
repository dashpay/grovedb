//! Aggregate-sum-specific helpers. Items shared with the count and
//! combined axes (the multi-key outer walker, the single-key descent,
//! the merk-bytes unwrapper, the `OuterMatch` row type) live in
//! [`super::super::aggregate_common`] — this file re-exports them via
//! thin wrappers that supply the axis-specific diagnostic label and
//! holds only the genuinely axis-specific helpers below.
//!
//! - [`verify_sum_leaf`] — delegate to the merk-level sum verifier.
//! - [`enforce_lower_chain`] — `combine_hash(H(value), lower_root) ==
//!   parent_value_hash`, the binding that ties each layer's sum to the
//!   GroveDB root hash, plus the terminal-type gate that requires the
//!   leaf-target element to be a `ProvableSumTree` or
//!   `ProvableCountProvableSumTree`. The axis-specific terminal-type
//!   set is why this helper stays per-axis.

use grovedb_merk::{
    proofs::query::{
        aggregate_sum::{
            verify_aggregate_big_sum_on_range_proof, verify_aggregate_sum_on_range_proof,
        },
        ProofLimitMode,
    },
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

/// The width of the sum an aggregate-sum proof is verified for. The width
/// decides which leaf verifier reads the proof (and so which proof nodes it
/// accepts), the type of the sum it returns, and which tree the terminal
/// path element must be. Everything else about the verification is shared.
pub(in crate::operations::proof) trait SumWidth {
    /// The verified in-range sum.
    type Sum;
    /// The trees a proof of this width terminates at, for diagnostics.
    const TERMINAL_TREES: &'static str;
    /// Verify a leaf aggregate proof, returning its root hash and sum.
    fn verify_leaf(
        leaf_bytes: &[u8],
        inner_range: &QueryItem,
    ) -> Result<(CryptoHash, Self::Sum), grovedb_merk::Error>;
    /// Whether `element` is a tree a proof of this width terminates at.
    fn is_terminal_tree(element: &Element) -> bool;
}

/// An `i64` sum, proved against a `ProvableSumTree` or a
/// `ProvableCountProvableSumTree`.
pub(in crate::operations::proof) struct NarrowSum;

impl SumWidth for NarrowSum {
    type Sum = i64;

    const TERMINAL_TREES: &'static str = "ProvableSumTree or ProvableCountProvableSumTree";

    fn verify_leaf(
        leaf_bytes: &[u8],
        inner_range: &QueryItem,
    ) -> Result<(CryptoHash, i64), grovedb_merk::Error> {
        verify_aggregate_sum_on_range_proof(leaf_bytes, inner_range).unwrap()
    }

    fn is_terminal_tree(element: &Element) -> bool {
        matches!(
            element,
            Element::ProvableSumTree(..) | Element::ProvableCountProvableSumTree(..)
        )
    }
}

/// An `i128` sum, proved against a `ProvableBigSumTree` or a
/// `ProvableCountProvableBigSumTree`.
pub(in crate::operations::proof) struct WideSum;

impl SumWidth for WideSum {
    type Sum = i128;

    const TERMINAL_TREES: &'static str = "ProvableBigSumTree or ProvableCountProvableBigSumTree";

    fn verify_leaf(
        leaf_bytes: &[u8],
        inner_range: &QueryItem,
    ) -> Result<(CryptoHash, i128), grovedb_merk::Error> {
        verify_aggregate_big_sum_on_range_proof(leaf_bytes, inner_range).unwrap()
    }

    fn is_terminal_tree(element: &Element) -> bool {
        matches!(
            element,
            Element::ProvableBigSumTree(..) | Element::ProvableCountProvableBigSumTree(..)
        )
    }
}

/// Verify the leaf layer: bytes are the encoded sum-proof Op stream;
/// the inner range is the same one the prover summed over.
pub(super) fn verify_sum_leaf<W: SumWidth>(
    leaf_bytes: &[u8],
    inner_range: &QueryItem,
    path_query: &PathQuery,
) -> Result<(CryptoHash, W::Sum), Error> {
    let (root_hash, sum) = W::verify_leaf(leaf_bytes, inner_range).map_err(|e| {
        Error::InvalidProof(
            path_query.clone(),
            format!("aggregate-sum leaf proof failed to verify: {}", e),
        )
    })?;
    Ok((root_hash, sum))
}

/// Aggregate-sum axis label used by the shared diagnostic-prefix
/// helpers in `aggregate_common`.
const AXIS_LABEL: &str = "aggregate-sum";

/// Thin wrapper around [`super::super::aggregate_common::expect_merk_bytes`]
/// that supplies the aggregate-sum axis label.
pub(super) fn expect_merk_bytes<'a>(
    proof_bytes: &'a ProofBytes,
    path_query: &PathQuery,
) -> Result<&'a [u8], Error> {
    super::super::aggregate_common::expect_merk_bytes(proof_bytes, path_query, AXIS_LABEL)
}

/// Thin wrapper around
/// [`super::super::aggregate_common::execute_carrier_layer_proof`] that
/// supplies the aggregate-sum axis label.
pub(super) fn execute_carrier_layer_proof(
    merk_bytes: &[u8],
    outer_items: &[QueryItem],
    left_to_right: bool,
    outer_limit: Option<u16>,
    limit_mode: ProofLimitMode,
    path_query: &PathQuery,
) -> Result<(CryptoHash, Vec<OuterMatch>, bool), Error> {
    super::super::aggregate_common::execute_carrier_layer_proof(
        merk_bytes,
        outer_items,
        left_to_right,
        outer_limit,
        limit_mode,
        path_query,
        AXIS_LABEL,
    )
}

/// Enforce the layer-chain hash equality plus, at the terminal layer,
/// the leaf-tree-type invariant.
///
/// At intermediate depths the only requirement is that the element be
/// *some* tree (we have to descend further). At the terminal depth — the
/// last path element, whose inner Merk is the actual aggregate target —
/// the element MUST deserialize to `Element::ProvableSumTree` (after
/// wrapper unwrapping). Without this check, an empty Merk-backed tree of
/// any other type at the leaf accepts a forged empty leaf proof, because
/// every empty Merk-backed tree has `inner_root = NULL_HASH` and so its
/// stored `value_hash = combine_hash(H(bytes), NULL_HASH)` — the chain
/// check passes uniformly. The honest prover-side gate in
/// `Merk::prove_aggregate_sum_on_range` already rejects non-ProvableSumTree
/// inputs; this is the matching verifier-side gate.
pub(super) fn enforce_lower_chain<W: SumWidth>(
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
        if !W::is_terminal_tree(&element) {
            return Err(Error::InvalidProof(
                path_query.clone(),
                format!(
                    "aggregate-sum proof's terminal path element at key {} must be a \
                     {} (got {}); a sum aggregate \
                     is only meaningful against a tree that binds its sum into the node hash",
                    hex::encode(target_key),
                    W::TERMINAL_TREES,
                    element.type_str()
                ),
            ));
        }
    } else if !element.is_any_tree() {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-sum proof's intermediate path element at key {} is not a tree \
                 element (got {}); sum queries can only descend through tree elements",
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
                "aggregate-sum proof chain mismatch at key {}: parent recorded value_hash \
                 {} but combine_hash(H(value), lower_root) is {}",
                hex::encode(target_key),
                hex::encode(parent_proof_hash),
                hex::encode(combined)
            ),
        ));
    }
    Ok(())
}
