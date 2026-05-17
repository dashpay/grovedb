//! Shared helpers used by the combined-aggregate leaf-chain walker.
//!
//! Mirror of [`super::super::aggregate_sum::helpers`] for the
//! dual-axis PCPS host.
//!
//! - [`verify_count_and_sum_leaf`] — delegate to the merk-level
//!   combined-aggregate verifier.
//! - [`expect_merk_bytes`] — unwrap a `ProofBytes::Merk(_)` or reject.
//! - [`verify_single_key_layer_proof_v0`] — verify a non-leaf merk
//!   proof for one expected key and recover its value bytes + chain
//!   commitment hash.
//! - [`enforce_lower_chain`] — `combine_hash(H(value), lower_root) ==
//!   parent_value_hash`, the binding that ties each layer's
//!   `(count, sum)` to the GroveDB root hash, plus the terminal-type
//!   gate that requires the leaf-target element to be a PCPS host.

use grovedb_merk::{
    proofs::{
        query::{
            aggregate_count_and_sum::verify_aggregate_count_and_sum_on_range_proof,
            QueryProofVerify,
        },
        Query as MerkQuery,
    },
    tree::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{operations::proof::ProofBytes, Element, Error, PathQuery};

/// Verify the leaf layer: bytes are the encoded combined-aggregate
/// proof Op stream; the inner range is the same one the prover
/// aggregated over.
pub(super) fn verify_count_and_sum_leaf(
    leaf_bytes: &[u8],
    inner_range: &QueryItem,
    path_query: &PathQuery,
) -> Result<(CryptoHash, u64, i64), Error> {
    let (root_hash, count, sum) =
        verify_aggregate_count_and_sum_on_range_proof(leaf_bytes, inner_range)
            .unwrap()
            .map_err(|e| {
                Error::InvalidProof(
                    path_query.clone(),
                    format!("combined-aggregate leaf proof failed to verify: {}", e),
                )
            })?;
    Ok((root_hash, count, sum))
}

/// Unwrap a `ProofBytes::Merk(_)` or reject the proof —
/// combined-aggregate envelopes are always merk-flavored at every layer.
pub(super) fn expect_merk_bytes<'a>(
    proof_bytes: &'a ProofBytes,
    path_query: &PathQuery,
) -> Result<&'a [u8], Error> {
    match proof_bytes {
        ProofBytes::Merk(b) => Ok(b.as_slice()),
        other => Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "combined-aggregate proof has unexpected non-merk layer bytes: {:?}",
                std::mem::discriminant(other)
            ),
        )),
    }
}

/// Verify a non-leaf layer that should contain a single-key proof for
/// `target_key`. Returns `(proven_value_bytes, this_layer_root_hash,
/// proof_hash_recorded_for_target)`.
pub(super) fn verify_single_key_layer_proof_v0(
    merk_bytes: &[u8],
    target_key: &[u8],
    path_query: &PathQuery,
) -> Result<(Vec<u8>, CryptoHash, CryptoHash), Error> {
    let level_query = MerkQuery {
        items: vec![grovedb_merk::proofs::query::QueryItem::Key(
            target_key.to_vec(),
        )],
        left_to_right: true,
        ..Default::default()
    };

    let (root_hash, merk_result) = level_query
        .execute_proof(merk_bytes, None, true, 0)
        .unwrap()
        .map_err(|e| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "non-leaf single-key proof for {} failed to verify: {}",
                    hex::encode(target_key),
                    e
                ),
            )
        })?;

    let proved = merk_result
        .result_set
        .iter()
        .find(|p| p.key == target_key)
        .ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "non-leaf proof did not contain the expected key {}",
                    hex::encode(target_key)
                ),
            )
        })?;

    let value_bytes = proved.value.clone().ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "non-leaf proof for key {} returned no value bytes",
                hex::encode(target_key)
            ),
        )
    })?;

    Ok((value_bytes, root_hash, proved.proof))
}

/// Enforce the layer-chain hash equality plus, at the terminal layer,
/// the leaf-tree-type invariant.
///
/// At intermediate depths the only requirement is that the element be
/// *some* tree (we have to descend further). At the terminal depth — the
/// last path element, whose inner Merk is the actual combined-aggregate
/// target — the element MUST deserialize to
/// `Element::ProvableCountProvableSumTree` (after wrapper unwrapping).
/// The honest prover-side gate in
/// `Merk::prove_aggregate_count_and_sum_on_range` already rejects
/// non-PCPS inputs; this is the matching verifier-side gate.
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
        if !matches!(element, Element::ProvableCountProvableSumTree(..)) {
            return Err(Error::InvalidProof(
                path_query.clone(),
                format!(
                    "combined-aggregate proof's terminal path element at key {} must be a \
                     ProvableCountProvableSumTree (got {}); a combined count+sum aggregate is \
                     only meaningful against a tree that binds both axes into the node hash",
                    hex::encode(target_key),
                    element.type_str()
                ),
            ));
        }
    } else if !element.is_any_tree() {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "combined-aggregate proof's intermediate path element at key {} is not a tree \
                 element (got {}); combined-aggregate queries can only descend through tree \
                 elements",
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
                "combined-aggregate proof chain mismatch at key {}: parent recorded \
                 value_hash {} but combine_hash(H(value), lower_root) is {}",
                hex::encode(target_key),
                hex::encode(parent_proof_hash),
                hex::encode(combined)
            ),
        ));
    }
    Ok(())
}
