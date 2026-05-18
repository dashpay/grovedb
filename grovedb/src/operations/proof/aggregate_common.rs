//! Helpers shared verbatim by all three aggregate-proof verifier
//! subtrees: [`super::aggregate_count`], [`super::aggregate_sum`], and
//! [`super::aggregate_count_and_sum`].
//!
//! Before this module existed each axis carried its own private copy of
//! these (byte-identical except for an axis-label substring in the
//! error messages). Centralizing them keeps the three axes from
//! drifting and means future per-axis additions only have to be wired
//! once.
//!
//! - [`OuterMatch`] — a single matched outer-key row from a carrier's
//!   multi-key merk proof. Pure type — axis-agnostic.
//! - [`verify_single_key_layer_proof_v0`] — verify a non-leaf merk
//!   proof for one expected key and recover its value bytes + chain
//!   commitment hash. Axis-agnostic.
//! - [`expect_merk_bytes`] — unwrap a `ProofBytes::Merk(_)` or reject
//!   with an axis-labelled error.
//! - [`execute_carrier_layer_proof`] — verify the carrier's multi-key
//!   merk proof and collect one [`OuterMatch`] per matched outer key.
//!
//! The two functions that produce diagnostic strings (`expect_merk_bytes`,
//! `execute_carrier_layer_proof`) take an `axis_label: &'static str` so
//! each axis's per-axis `helpers.rs` can supply its own prefix
//! ("aggregate-count", "aggregate-sum", "combined-aggregate") through a
//! thin wrapper, preserving the original error text.

use grovedb_merk::{
    proofs::{query::QueryProofVerify, Query as MerkQuery},
    CryptoHash,
};
use grovedb_query::QueryItem;

use crate::{operations::proof::ProofBytes, Error, PathQuery};

/// Unwrap a `ProofBytes::Merk(_)` or reject the proof. All three
/// aggregate-axis envelopes are merk-flavored at every layer; a
/// non-`Merk` variant means the prover emitted something the verifier
/// can't interpret.
///
/// `axis_label` is interpolated into the rejection message (e.g.
/// "aggregate-count", "aggregate-sum", "combined-aggregate") so the
/// error string keeps the per-axis prefix the original duplicates had.
pub(in crate::operations::proof) fn expect_merk_bytes<'a>(
    proof_bytes: &'a ProofBytes,
    path_query: &PathQuery,
    axis_label: &'static str,
) -> Result<&'a [u8], Error> {
    match proof_bytes {
        ProofBytes::Merk(b) => Ok(b.as_slice()),
        other => Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "{} proof has unexpected non-merk layer bytes: {:?}",
                axis_label,
                std::mem::discriminant(other)
            ),
        )),
    }
}

/// Verify a non-leaf layer that should contain a single-key proof for
/// `target_key`. Returns `(proven_value_bytes, this_layer_root_hash,
/// proof_hash_recorded_for_target)`.
///
/// The "proof_hash" is the value_hash committed by the merk proof for
/// the target key — this is the hash the verifier will compare against
/// `combine_hash(H(child_tree_value), lower_layer_root_hash)` to enforce
/// the chain.
///
/// Axis-agnostic: a single-key merk proof has the same semantics
/// regardless of whether the leaf being descended toward is a count, sum,
/// or combined-aggregate target.
pub(in crate::operations::proof) fn verify_single_key_layer_proof_v0(
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

/// One matched outer key in the carrier layer's multi-key merk proof.
///
/// Axis-agnostic: the carrier's outer match is structural — it carries
/// the matched key, the parent-recorded value bytes for that key, and
/// the parent's recorded value_hash (the commitment the chain check
/// validates against). Per-axis logic is applied downstream by the
/// caller's `enforce_lower_chain` (which still lives per axis because
/// of axis-specific terminal-type acceptance sets).
pub(in crate::operations::proof) struct OuterMatch {
    /// The matched outer key bytes.
    pub(in crate::operations::proof) outer_key: Vec<u8>,
    /// The serialized tree element bytes for the matched outer key (a
    /// non-empty tree element of some flavor).
    pub(in crate::operations::proof) value_bytes: Vec<u8>,
    /// The value_hash the parent merk committed for this outer key — the
    /// hash that must equal `combine_hash(H(value), lower_layer_root)`.
    pub(in crate::operations::proof) commitment_hash: CryptoHash,
}

/// Execute the carrier-layer multi-key merk proof for `outer_items`,
/// returning `(carrier_merk_root_hash, matched_outer_keys)`.
///
/// `outer_limit` is the `SizedQuery::limit` that bounds the outer walk
/// (matching what the prover passed to
/// `Merk::prove_unchecked_query_items` when it generated the
/// carrier-layer merk proof). When the carrier query carries a
/// non-`None` `SizedQuery::limit`, the prover truncates the outer walk
/// after that many matched keys and emits structural Hash nodes for the
/// rest; the verifier must therefore execute the proof with the same
/// limit so that its merk walker stops at the same boundary instead of
/// demanding KV data for the un-walked tail.
///
/// `axis_label` is interpolated into the rejection messages so each
/// axis's wrapper can supply its own diagnostic prefix.
pub(in crate::operations::proof) fn execute_carrier_layer_proof(
    merk_bytes: &[u8],
    outer_items: &[QueryItem],
    left_to_right: bool,
    outer_limit: Option<u16>,
    path_query: &PathQuery,
    axis_label: &'static str,
) -> Result<(CryptoHash, Vec<OuterMatch>), Error> {
    // The grovedb_query::QueryItem and
    // grovedb_merk::proofs::query::QueryItem types are identical (the
    // merk crate re-exports the grovedb-query one).
    let level_query = MerkQuery {
        items: outer_items.to_vec(),
        left_to_right,
        ..Default::default()
    };

    // Walk direction must match the prover's; otherwise the merk
    // walker stops at the first out-of-order boundary and only the last
    // key in the proof is returned.
    let (root_hash, merk_result) = level_query
        .execute_proof(merk_bytes, outer_limit, left_to_right, 0)
        .unwrap()
        .map_err(|e| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier {} multi-key proof failed to verify: {}",
                    axis_label, e
                ),
            )
        })?;

    let mut matched = Vec::with_capacity(merk_result.result_set.len());
    for proved in &merk_result.result_set {
        let value = proved.value.clone().ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier {} proof returned a result row without value bytes for key {}",
                    axis_label,
                    hex::encode(&proved.key)
                ),
            )
        })?;
        matched.push(OuterMatch {
            outer_key: proved.key.clone(),
            value_bytes: value,
            commitment_hash: proved.proof,
        });
    }

    Ok((root_hash, matched))
}
