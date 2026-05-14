//! Shared helpers used by both the leaf-chain walker and the per-key
//! carrier walker.
//!
//! - [`decode_grovedb_proof`] — parse the bincode envelope.
//! - [`verify_count_leaf`] — delegate to the merk-level count verifier.
//! - [`expect_merk_bytes`] — unwrap a `ProofBytes::Merk(_)` or reject.
//! - [`verify_single_key_layer_proof_v0`] — verify a non-leaf merk
//!   proof for one expected key and recover its value bytes + chain
//!   commitment hash.
//! - [`OuterMatch`] + [`execute_carrier_layer_proof`] — verify the
//!   carrier's multi-key merk proof, collect one `OuterMatch` per
//!   matched outer key.
//! - [`enforce_lower_chain`] — `combine_hash(H(value), lower_root) ==
//!   parent_value_hash`, the binding that ties each layer's count to
//!   the GroveDB root hash.

use grovedb_merk::{
    proofs::{
        query::{aggregate_count::verify_aggregate_count_on_range_proof, QueryProofVerify},
        Query as MerkQuery,
    },
    tree::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_query::QueryItem;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{GroveDBProof, ProofBytes},
    Element, Error, PathQuery,
};

/// Decode a serialized `GroveDBProof` envelope using the same bincode
/// configuration the prover writes out.
///
/// Decoding is canonical: trailing bytes beyond the encoded envelope
/// are rejected. Without this check the same `(RootHash, count)` could
/// be reconstructed from many different proof byte-strings (a proof and
/// the same proof with arbitrary suffix bytes), which is harmless for
/// the chain-bound correctness guarantee but breaks any
/// equality-by-bytes assumption a caller might rely on (caching,
/// deduplication, hashing the proof itself).
pub(super) fn decode_grovedb_proof(proof: &[u8]) -> Result<GroveDBProof, Error> {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (decoded, consumed) = bincode::decode_from_slice(proof, config)
        .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?;
    if consumed != proof.len() {
        return Err(Error::CorruptedData(format!(
            "aggregate-count proof has {} trailing bytes after the encoded envelope",
            proof.len() - consumed
        )));
    }
    Ok(decoded)
}

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

/// Unwrap a `ProofBytes::Merk(_)` or reject the proof — aggregate-count
/// envelopes are always merk-flavored at every layer.
pub(super) fn expect_merk_bytes<'a>(
    proof_bytes: &'a ProofBytes,
    path_query: &PathQuery,
) -> Result<&'a [u8], Error> {
    match proof_bytes {
        ProofBytes::Merk(b) => Ok(b.as_slice()),
        other => Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-count proof has unexpected non-merk layer bytes: {:?}",
                std::mem::discriminant(other)
            ),
        )),
    }
}

/// Verify a non-leaf layer that should contain a single-key proof for
/// `target_key`. Returns `(proven_value_bytes, this_layer_root_hash,
/// proof_hash_recorded_for_target)`.
///
/// The "proof_hash" is the value_hash committed by the merk proof for the
/// target key — this is the hash the verifier will compare against
/// `combine_hash(H(child_tree_value), lower_layer_root_hash)` to enforce
/// the chain.
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

/// One matched outer key in the carrier layer's multi-key merk proof.
pub(super) struct OuterMatch {
    /// The matched outer key bytes.
    pub(super) outer_key: Vec<u8>,
    /// The serialized tree element bytes for the matched outer key (a
    /// non-empty tree element of some flavor).
    pub(super) value_bytes: Vec<u8>,
    /// The value_hash the parent merk committed for this outer key — the
    /// hash that must equal `combine_hash(H(value), lower_layer_root)`.
    pub(super) commitment_hash: CryptoHash,
}

/// Execute the carrier-layer multi-key merk proof for `outer_items`,
/// returning `(carrier_merk_root_hash, matched_outer_keys)`. Each
/// `OuterMatch` carries the value bytes and the parent-recorded value_hash
/// that the chain check will validate.
pub(super) fn execute_carrier_layer_proof(
    merk_bytes: &[u8],
    outer_items: &[QueryItem],
    left_to_right: bool,
    path_query: &PathQuery,
) -> Result<(CryptoHash, Vec<OuterMatch>), Error> {
    // The grovedb_query::QueryItem and grovedb_merk::proofs::query::QueryItem
    // types are identical (the merk crate re-exports the grovedb-query one).
    let level_query = MerkQuery {
        items: outer_items.to_vec(),
        left_to_right,
        ..Default::default()
    };

    // Walk direction must match the prover's; otherwise the merk
    // walker stops at the first out-of-order boundary and only the
    // last key in the proof is returned.
    let (root_hash, merk_result) = level_query
        .execute_proof(merk_bytes, None, left_to_right, 0)
        .unwrap()
        .map_err(|e| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier aggregate-count multi-key proof failed to verify: {}",
                    e
                ),
            )
        })?;

    let mut matched = Vec::with_capacity(merk_result.result_set.len());
    for proved in &merk_result.result_set {
        let value = proved.value.clone().ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier aggregate-count proof returned a result row without value bytes \
                     for key {}",
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

/// Enforce the layer-chain hash equality: the parent merk's recorded
/// value_hash for the tree element must equal `combine_hash(H(value),
/// lower_layer_root_hash)`. This is what makes the count cryptographically
/// bound to the GroveDB root hash — the leaf count proof's reconstructed
/// `lower_hash` must agree with the parent's commitment, transitively up to
/// the root.
///
/// Intermediate path elements may be any tree type — the GroveDB grove can
/// route through Normal/Sum/Count/etc. trees on the way down to the
/// provable-count leaf. The leaf-level tree-type check is enforced by the
/// merk prover (`Merk::prove_aggregate_count_on_range`); here we only
/// require that each non-leaf element on the path *is* some non-empty tree,
/// since only trees have a lower layer to chain into.
pub(super) fn enforce_lower_chain(
    path_query: &PathQuery,
    target_key: &[u8],
    proven_value_bytes: &[u8],
    lower_hash: &CryptoHash,
    parent_proof_hash: &CryptoHash,
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
    if !element.is_any_tree() {
        return Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-count proof's path element at key {} is not a tree element \
                 (got {:?}); count queries can only descend through tree elements",
                hex::encode(target_key),
                std::mem::discriminant(&element)
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
