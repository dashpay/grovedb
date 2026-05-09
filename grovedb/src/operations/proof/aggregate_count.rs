//! GroveDB-side prove/verify glue for `AggregateCountOnRange` queries.
//!
//! The merk-level pieces live in `grovedb_merk::proofs::query::aggregate_count`
//! (proof generation in `Merk::prove_aggregate_count_on_range`, proof
//! verification in `verify_aggregate_count_on_range_proof`). This module
//! adds the GroveDB-level *envelope* handling: a verifier that walks the
//! multi-layer `GroveDBProof` chain (parent merk → ... → leaf merk),
//! verifies the path-element existence proofs at each non-leaf layer, and
//! delegates to the merk-level count verifier at the leaf.
//!
//! The proof generator side is wired directly into
//! [`GroveDb::prove_subqueries`] / [`GroveDb::prove_subqueries_v1`] — see
//! the "Aggregate-count short-circuit" branches there.

use grovedb_merk::{
    proofs::{
        query::{aggregate_count::verify_aggregate_count_on_range_proof, QueryProofVerify},
        Query as MerkQuery,
    },
    tree::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_version::{check_grovedb_v0, version::GroveVersion};

use crate::{
    operations::proof::{
        GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof, ProofBytes,
    },
    Element, Error, GroveDb, PathQuery,
};

impl GroveDb {
    /// Verify a serialized `prove_query` proof against an
    /// `AggregateCountOnRange` `PathQuery`, returning the GroveDB root hash
    /// and the verified count.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_count_on_range`] — a single
    /// `AggregateCountOnRange(_)` item, no subqueries, no pagination, and an
    /// inner range that isn't `Key`, `RangeFull`, or another
    /// `AggregateCountOnRange`. Any other shape is rejected up front with
    /// `Error::InvalidQuery` before any bytes are decoded.
    ///
    /// Returns:
    /// - `root_hash` — the reconstructed GroveDB root hash. The caller is
    ///   responsible for comparing this against their trusted root hash.
    /// - `count` — the number of keys in the inner range that were committed
    ///   by the proof.
    ///
    /// Cryptographic guarantees:
    /// - At each non-leaf layer, a regular single-key merk proof
    ///   demonstrates that the next path element exists with the recorded
    ///   value bytes; the verifier checks the chain
    ///   `combine_hash(H(value), lower_hash) == parent_proof_hash` so a
    ///   forged path is impossible without a root-hash mismatch.
    /// - At the leaf layer, the count is committed by `HashWithCount`'s
    ///   `node_hash_with_count(kv_hash, left, right, count)` recomputation —
    ///   tampering with the count produces a different reconstructed merk
    ///   root, and the chain check above then fails.
    pub fn verify_aggregate_count_query(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, u64), Error> {
        check_grovedb_v0!(
            "verify_aggregate_count_query",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );

        let inner_range = path_query.validate_aggregate_count_on_range()?.clone();

        // Decode the GroveDBProof envelope using the same config the prover
        // uses on the way out (matches `prove_query`).
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let grovedb_proof: GroveDBProof = bincode::decode_from_slice(proof, config)
            .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?
            .0;

        let path_keys: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();

        match grovedb_proof {
            GroveDBProof::V0(GroveDBProofV0 { root_layer, .. }) => verify_v0_layer(
                &root_layer,
                path_query,
                &path_keys,
                0,
                &inner_range,
                grove_version,
            ),
            GroveDBProof::V1(GroveDBProofV1 { root_layer }) => verify_v1_layer(
                &root_layer,
                path_query,
                &path_keys,
                0,
                &inner_range,
                grove_version,
            ),
        }
    }
}

/// Walk a V0 (`MerkOnlyLayerProof`) envelope. At each non-leaf depth we
/// verify the single-key existence proof for `path[depth]` and descend into
/// the matching lower layer; at the leaf depth we delegate to the merk
/// count verifier.
fn verify_v0_layer(
    layer: &MerkOnlyLayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    inner_range: &grovedb_merk::proofs::query::QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64), Error> {
    if depth == path_keys.len() {
        // Leaf layer: count proof.
        return verify_count_leaf(&layer.merk_proof, inner_range, path_query);
    }

    // Non-leaf: build a single-key merk query and verify.
    let next_key = path_keys[depth].to_vec();
    let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
        verify_single_key_layer_proof_v0(&layer.merk_proof, &next_key, path_query)?;

    // Descend.
    let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "aggregate-count proof missing lower layer for path key {}",
                hex::encode(&next_key)
            ),
        )
    })?;
    let (lower_hash, count) = verify_v0_layer(
        lower_layer,
        path_query,
        path_keys,
        depth + 1,
        inner_range,
        grove_version,
    )?;

    // Chain check: combine_hash(H(tree_value), lower_hash) must equal the
    // value_hash recorded by the parent merk for this tree element.
    enforce_lower_chain(
        path_query,
        &next_key,
        &proven_value_bytes,
        &lower_hash,
        &parent_proof_hash,
        grove_version,
    )?;

    Ok((parent_root_hash, count))
}

/// Walk a V1 (`LayerProof`) envelope. Mirrors `verify_v0_layer`; the V1
/// envelope wraps merk proof bytes in `ProofBytes::Merk(_)` and we reject
/// any other tree-specific proof variant for count queries (they're not
/// applicable to provable count trees).
fn verify_v1_layer(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    inner_range: &grovedb_merk::proofs::query::QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64), Error> {
    let merk_bytes = match &layer.merk_proof {
        ProofBytes::Merk(b) => b.as_slice(),
        other => {
            return Err(Error::InvalidProof(
                path_query.clone(),
                format!(
                    "aggregate-count proof has unexpected non-merk leaf bytes: {:?}",
                    std::mem::discriminant(other)
                ),
            ));
        }
    };

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
    let (lower_hash, count) = verify_v1_layer(
        lower_layer,
        path_query,
        path_keys,
        depth + 1,
        inner_range,
        grove_version,
    )?;

    enforce_lower_chain(
        path_query,
        &next_key,
        &proven_value_bytes,
        &lower_hash,
        &parent_proof_hash,
        grove_version,
    )?;

    Ok((parent_root_hash, count))
}

/// Verify the leaf layer: bytes are the encoded count-proof Op stream;
/// the inner range is the same one the prover counted over.
fn verify_count_leaf(
    leaf_bytes: &[u8],
    inner_range: &grovedb_merk::proofs::query::QueryItem,
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

/// Verify a non-leaf layer that should contain a single-key proof for
/// `target_key`. Returns `(proven_value_bytes, this_layer_root_hash,
/// proof_hash_recorded_for_target)`.
///
/// The "proof_hash" is the value_hash committed by the merk proof for the
/// target key — this is the hash the verifier will compare against
/// `combine_hash(H(child_tree_value), lower_layer_root_hash)` to enforce
/// the chain.
fn verify_single_key_layer_proof_v0(
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

    // Find the result row for our target key and pull the value + proof_hash.
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
fn enforce_lower_chain(
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
