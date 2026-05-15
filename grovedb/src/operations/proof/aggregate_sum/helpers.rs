//! Shared helpers used by the aggregate-sum leaf-chain walker.
//!
//! - [`decode_grovedb_proof`] — parse the bincode envelope.
//! - [`verify_sum_leaf`] — delegate to the merk-level sum verifier.
//! - [`expect_merk_bytes`] — unwrap a `ProofBytes::Merk(_)` or reject.
//! - [`verify_single_key_layer_proof_v0`] — verify a non-leaf merk
//!   proof for one expected key and recover its value bytes + chain
//!   commitment hash.
//! - [`enforce_lower_chain`] — `combine_hash(H(value), lower_root) ==
//!   parent_value_hash`, the binding that ties each layer's sum to the
//!   GroveDB root hash, plus the terminal-type gate that requires the
//!   leaf-target element to be a `ProvableSumTree`.

use grovedb_merk::{
    proofs::{
        query::{aggregate_sum::verify_aggregate_sum_on_range_proof, QueryProofVerify},
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
/// are rejected. Without this check the same `(RootHash, sum)` could be
/// reconstructed from many different proof byte-strings (a proof and the
/// same proof with arbitrary suffix bytes), which is harmless for the
/// chain-bound correctness guarantee but breaks any equality-by-bytes
/// assumption a caller might rely on (caching, deduplication, hashing
/// the proof itself).
pub(super) fn decode_grovedb_proof(proof: &[u8]) -> Result<GroveDBProof, Error> {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (decoded, consumed) = bincode::decode_from_slice(proof, config)
        .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?;
    if consumed != proof.len() {
        return Err(Error::CorruptedData(format!(
            "aggregate-sum proof has {} trailing bytes after the encoded envelope",
            proof.len() - consumed
        )));
    }
    Ok(decoded)
}

/// Verify the leaf layer: bytes are the encoded sum-proof Op stream;
/// the inner range is the same one the prover summed over.
pub(super) fn verify_sum_leaf(
    leaf_bytes: &[u8],
    inner_range: &QueryItem,
    path_query: &PathQuery,
) -> Result<(CryptoHash, i64), Error> {
    let (root_hash, sum) = verify_aggregate_sum_on_range_proof(leaf_bytes, inner_range)
        .unwrap()
        .map_err(|e| {
            Error::InvalidProof(
                path_query.clone(),
                format!("aggregate-sum leaf proof failed to verify: {}", e),
            )
        })?;
    Ok((root_hash, sum))
}

/// Unwrap a `ProofBytes::Merk(_)` or reject the proof — aggregate-sum
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
                "aggregate-sum proof has unexpected non-merk layer bytes: {:?}",
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
        if !matches!(element, Element::ProvableSumTree(..)) {
            return Err(Error::InvalidProof(
                path_query.clone(),
                format!(
                    "aggregate-sum proof's terminal path element at key {} must be a \
                     ProvableSumTree (got {}); a sum aggregate is only meaningful against \
                     a tree that binds its sum into the node hash",
                    hex::encode(target_key),
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
