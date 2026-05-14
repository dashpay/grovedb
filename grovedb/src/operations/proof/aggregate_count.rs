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
//!
//! ## Two shapes
//!
//! `AggregateCountOnRange` queries come in two flavors:
//!
//! - **Leaf** — a single `AggregateCountOnRange(_)` item at the top level
//!   of the inner `Query`. The proof descends `path_query.path` via
//!   single-key existence checks and produces a single `u64` at the leaf
//!   merk. Surfaced through [`GroveDb::verify_aggregate_count_query`].
//!
//! - **Carrier** — an outer query whose items are `Key(_)` / `Range*(_)`
//!   (one IN-style fan-out dimension) and whose
//!   `default_subquery_branch.subquery` resolves to a leaf ACOR query.
//!   The proof descends `path_query.path` via single-key checks, then at
//!   the carrier merk it produces a multi-key proof over the outer items;
//!   each matched outer key recurses through the `subquery_path` (if any)
//!   to a leaf merk that produces its own count. The verifier returns one
//!   `(outer_key, count)` pair per matched outer key. Surfaced through
//!   [`GroveDb::verify_aggregate_count_query_per_key`].

use grovedb_merk::{
    proofs::{
        query::{aggregate_count::verify_aggregate_count_on_range_proof, QueryProofVerify},
        Query as MerkQuery,
    },
    tree::{combine_hash, value_hash},
    CryptoHash,
};
use grovedb_query::QueryItem;
use grovedb_version::{check_grovedb_v0, version::GroveVersion};

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes},
    Element, Error, GroveDb, PathQuery,
};

impl GroveDb {
    /// Verify a serialized `prove_query` proof against a leaf
    /// `AggregateCountOnRange` `PathQuery`, returning the GroveDB root hash
    /// and the verified count.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_count_on_range`] and additionally must
    /// be the **leaf** shape — a single `AggregateCountOnRange(_)` item, no
    /// subqueries, no pagination, and an inner range that isn't `Key`,
    /// `RangeFull`, or another `AggregateCountOnRange`. Carrier-shape ACOR
    /// queries (outer `Keys` + ACOR subquery) must use
    /// [`GroveDb::verify_aggregate_count_query_per_key`] instead.
    ///
    /// `AggregateCountOnRange` requires **V1 proof envelopes**
    /// (`GroveDBProofV1`). V0 (`GroveDBProofV0` / `MerkOnlyLayerProof`)
    /// envelopes predate the ACOR feature and are only produced by grove
    /// versions older than the one used by Dash Platform v12; this entry
    /// point rejects them with `Error::InvalidProof`.
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

        // Validate at the PathQuery level so SizedQuery::limit / offset
        // (which ACOR explicitly forbids) are enforced alongside the
        // inner-Query shape rules.
        let inner_range = path_query.validate_leaf_aggregate_count_on_range()?.clone();

        let grovedb_proof = decode_grovedb_proof(proof)?;
        let path_keys: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();

        let root_layer = require_v1_envelope(&grovedb_proof, path_query)?;
        let (root_hash, count) = verify_v1_leaf_chain(
            root_layer,
            path_query,
            &path_keys,
            0,
            &inner_range,
            grove_version,
        )?;
        Ok((root_hash, count))
    }

    /// Verify a serialized `prove_query` proof against an ACOR `PathQuery`
    /// in either the leaf or carrier shape, returning one
    /// `(outer_key, count)` pair per matched outer key.
    ///
    /// For a **leaf** ACOR query the returned vector contains exactly one
    /// entry whose key is an empty byte string and whose count is the same
    /// `u64` [`GroveDb::verify_aggregate_count_query`] would have returned.
    /// This makes carrier and leaf consumers symmetric: callers that always
    /// process a `Vec<(Vec<u8>, u64)>` don't need to branch on the shape.
    ///
    /// For a **carrier** ACOR query the outer items must be `Key(_)` /
    /// `Range*(_)`, the `default_subquery_branch.subquery` must validate as a
    /// leaf ACOR, and the optional `subquery_path` is followed exactly
    /// (single-key descent per element) before the count proof. The returned
    /// vector has one entry per matched outer key in **query-direction
    /// order**: when the carrier's `left_to_right` is `true` (the default,
    /// matching the merk prover's natural walk) entries come back in
    /// ascending lexicographic key order; when `left_to_right` is `false`
    /// they come back in descending order, mirroring the merk proof's own
    /// emission order. Outer-key candidates that the prover proved as
    /// absent contribute no entry.
    ///
    /// Like [`GroveDb::verify_aggregate_count_query`], this entry point
    /// requires **V1 proof envelopes**. V0 envelopes predate ACOR and are
    /// rejected with `Error::InvalidProof`.
    ///
    /// Cryptographic guarantees:
    /// - Every layer is committed via the same `combine_hash(H(value),
    ///   lower_hash) == parent_proof_hash` chain check used by the leaf
    ///   verifier, so a forged path through the carrier or
    ///   `subquery_path` produces a root-hash mismatch.
    /// - Each per-outer-key count is committed by the leaf
    ///   `HashWithCount` / `KVDigestCount` recomputation;
    ///   counts can't be tampered with independently.
    pub fn verify_aggregate_count_query_per_key(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
        check_grovedb_v0!(
            "verify_aggregate_count_query_per_key",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );

        // Classify the query and extract the leaf inner range plus the
        // optional carrier subquery_path. For leaf queries the carrier
        // descent below is skipped (carrier_outer_items is None).
        let classification = classify_path_query(path_query)?;

        let grovedb_proof = decode_grovedb_proof(proof)?;
        let path_keys: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();

        let root_layer = require_v1_envelope(&grovedb_proof, path_query)?;
        verify_v1_with_classification(
            root_layer,
            path_query,
            &path_keys,
            &classification,
            grove_version,
        )
    }
}

/// Extract the V1 root layer from a `GroveDBProof` envelope, or refuse
/// the proof. ACOR (both leaf and carrier) requires V1 envelopes — the
/// V0 (`MerkOnlyLayerProof`) envelope predates ACOR and is only emitted
/// by grove versions older than the one used by Dash Platform v12, so
/// it cannot legitimately contain an ACOR proof.
fn require_v1_envelope<'a>(
    proof: &'a GroveDBProof,
    path_query: &PathQuery,
) -> Result<&'a LayerProof, Error> {
    match proof {
        GroveDBProof::V1(GroveDBProofV1 { root_layer }) => Ok(root_layer),
        GroveDBProof::V0(_) => Err(Error::InvalidProof(
            path_query.clone(),
            "AggregateCountOnRange proofs require V1 proof envelopes; V0 envelopes predate \
             this feature and cannot legitimately carry an aggregate-count proof"
                .to_string(),
        )),
    }
}

/// Classification of an ACOR `PathQuery`. Encodes either the leaf-only
/// inner range (no carrier descent) or the carrier outer items + leaf
/// inner range + optional subquery_path that the verifier must follow
/// per outer key.
struct AcorClassification {
    /// The inner range that the leaf merk count proof must satisfy.
    leaf_inner_range: QueryItem,
    /// Carrier outer items. `None` for leaf-only queries.
    carrier_outer_items: Option<Vec<QueryItem>>,
    /// Carrier subquery_path (the keys between each outer match and the
    /// leaf merk). Empty `Vec` if no subquery_path was set. `None` for
    /// leaf-only queries.
    carrier_subquery_path: Option<Vec<Vec<u8>>>,
    /// Whether the outer query is left-to-right. Affects which results the
    /// merk_proof returns when the outer items are ranges. Always `true`
    /// for leaf-only.
    carrier_left_to_right: bool,
}

fn classify_path_query(path_query: &PathQuery) -> Result<AcorClassification, Error> {
    // Validate at the PathQuery level so SizedQuery::limit / offset
    // (which ACOR explicitly forbids) are enforced alongside the
    // inner-Query shape rules — for both the leaf and the carrier branch
    // below.
    let leaf_inner = path_query.validate_aggregate_count_on_range()?.clone();
    let q = &path_query.query.query;
    if q.aggregate_count_on_range().is_some() {
        // Leaf shape: top-level ACOR item. The top-level
        // `validate_aggregate_count_on_range` dispatcher above routed
        // through the leaf validator, so we already know `leaf_inner` is
        // the inner range of the top-level ACOR item.
        return Ok(AcorClassification {
            leaf_inner_range: leaf_inner,
            carrier_outer_items: None,
            carrier_subquery_path: None,
            carrier_left_to_right: true,
        });
    }
    // Carrier shape: validation above routed through the carrier
    // validator, so `leaf_inner` is the *subquery's* inner range. We just
    // need to extract the outer items and the optional subquery_path.
    let outer_items = q.items.clone();
    let subquery_path = q
        .default_subquery_branch
        .subquery_path
        .clone()
        .unwrap_or_default();
    Ok(AcorClassification {
        leaf_inner_range: leaf_inner,
        carrier_outer_items: Some(outer_items),
        carrier_subquery_path: Some(subquery_path),
        carrier_left_to_right: q.left_to_right,
    })
}

fn decode_grovedb_proof(proof: &[u8]) -> Result<GroveDBProof, Error> {
    // Decode the GroveDBProof envelope using the same config the prover
    // uses on the way out (matches `prove_query`).
    let config = bincode::config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (proof, _) = bincode::decode_from_slice(proof, config)
        .map_err(|e| Error::CorruptedData(format!("unable to decode proof: {}", e)))?;
    Ok(proof)
}

fn verify_v1_leaf_chain(
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

// ── per-key entry-point traversal (V1 only — V0 envelopes are
// rejected at the entry-point gate above, since they predate the
// ACOR feature and cannot legitimately carry an aggregate-count proof)

fn verify_v1_with_classification(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    classification: &AcorClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
    verify_v1_per_key(
        layer,
        path_query,
        path_keys,
        0,
        classification,
        grove_version,
    )
}

fn verify_v1_per_key(
    layer: &LayerProof,
    path_query: &PathQuery,
    path_keys: &[&[u8]],
    depth: usize,
    classification: &AcorClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;

    if depth < path_keys.len() {
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
        let (lower_hash, results) = verify_v1_per_key(
            lower_layer,
            path_query,
            path_keys,
            depth + 1,
            classification,
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
        return Ok((parent_root_hash, results));
    }

    match &classification.carrier_outer_items {
        None => {
            let (root, count) =
                verify_count_leaf(merk_bytes, &classification.leaf_inner_range, path_query)?;
            Ok((root, vec![(Vec::new(), count)]))
        }
        Some(outer_items) => verify_v1_carrier_layer(
            layer,
            merk_bytes,
            path_query,
            outer_items,
            classification,
            grove_version,
        ),
    }
}

fn verify_v1_carrier_layer(
    layer: &LayerProof,
    merk_bytes: &[u8],
    path_query: &PathQuery,
    outer_items: &[QueryItem],
    classification: &AcorClassification,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, Vec<(Vec<u8>, u64)>), Error> {
    let (carrier_root, matched) = execute_carrier_layer_proof(
        merk_bytes,
        outer_items,
        classification.carrier_left_to_right,
        path_query,
    )?;

    let subquery_path = classification
        .carrier_subquery_path
        .as_ref()
        .expect("carrier subquery_path is set when carrier_outer_items is Some");

    let mut results = Vec::with_capacity(matched.len());
    for OuterMatch {
        outer_key,
        value_bytes,
        commitment_hash,
    } in matched
    {
        let lower_layer = layer.lower_layers.get(&outer_key).ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier ACOR proof missing lower layer for outer key {}",
                    hex::encode(&outer_key)
                ),
            )
        })?;

        let (lower_root, count) = verify_v1_subquery_path(
            lower_layer,
            path_query,
            subquery_path,
            0,
            &classification.leaf_inner_range,
            grove_version,
        )?;

        enforce_lower_chain(
            path_query,
            &outer_key,
            &value_bytes,
            &lower_root,
            &commitment_hash,
            grove_version,
        )?;
        results.push((outer_key, count));
    }

    Ok((carrier_root, results))
}

fn verify_v1_subquery_path(
    layer: &LayerProof,
    path_query: &PathQuery,
    subquery_path: &[Vec<u8>],
    depth: usize,
    inner_range: &QueryItem,
    grove_version: &GroveVersion,
) -> Result<(CryptoHash, u64), Error> {
    let merk_bytes = expect_merk_bytes(&layer.merk_proof, path_query)?;
    if depth == subquery_path.len() {
        return verify_count_leaf(merk_bytes, inner_range, path_query);
    }
    let next_key = subquery_path[depth].clone();
    let (proven_value_bytes, parent_root_hash, parent_proof_hash) =
        verify_single_key_layer_proof_v0(merk_bytes, &next_key, path_query)?;
    let lower_layer = layer.lower_layers.get(&next_key).ok_or_else(|| {
        Error::InvalidProof(
            path_query.clone(),
            format!(
                "carrier ACOR proof missing subquery_path layer for key {}",
                hex::encode(&next_key)
            ),
        )
    })?;
    let (lower_hash, count) = verify_v1_subquery_path(
        lower_layer,
        path_query,
        subquery_path,
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

// ── shared helpers ─────────────────────────────────────────────────────────

/// Verify the leaf layer: bytes are the encoded count-proof Op stream;
/// the inner range is the same one the prover counted over.
fn verify_count_leaf(
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

fn expect_merk_bytes<'a>(
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
struct OuterMatch {
    /// The matched outer key bytes.
    outer_key: Vec<u8>,
    /// The serialized tree element bytes for the matched outer key (a
    /// non-empty tree element of some flavor).
    value_bytes: Vec<u8>,
    /// The value_hash the parent merk committed for this outer key — the
    /// hash that must equal `combine_hash(H(value), lower_layer_root)`.
    commitment_hash: CryptoHash,
}

/// Execute the carrier-layer multi-key merk proof for `outer_items`,
/// returning `(carrier_merk_root_hash, matched_outer_keys)`. Each
/// `OuterMatch` carries the value bytes and the parent-recorded value_hash
/// that the chain check will validate.
fn execute_carrier_layer_proof(
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
                format!("carrier ACOR multi-key proof failed to verify: {}", e),
            )
        })?;

    let mut matched = Vec::with_capacity(merk_result.result_set.len());
    for proved in &merk_result.result_set {
        let value = proved.value.clone().ok_or_else(|| {
            Error::InvalidProof(
                path_query.clone(),
                format!(
                    "carrier ACOR proof returned a result row without value bytes for key {}",
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
