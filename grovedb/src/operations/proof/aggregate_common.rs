//! Helpers shared verbatim by all three aggregate-proof verifier
//! subtrees: [`super::aggregate_count`], [`super::aggregate_sum`], and
//! [`super::aggregate_count_and_sum`].
//!
//! These items would otherwise be byte-identical copies across each
//! axis (except for an axis-label substring in the error messages).
//! Centralizing them here keeps the three axes from drifting and
//! means future per-axis additions only have to be wired once.
//!
//! - [`OuterMatch`] — a single matched outer-key row from a carrier's
//!   multi-key merk proof. Pure type — axis-agnostic.
//! - [`AggregateClassification`] — leaf-vs-carrier classification
//!   descriptor produced by each axis's classify function. Pure type —
//!   axis-agnostic (every axis classifies into the same four
//!   fields, only the carried inner range's type varies cosmetically).
//! - [`classify_aggregate_path_query`] — generic classification driver,
//!   parameterized by closures supplying the axis-specific validator
//!   and "owns an aggregate-X item at top level" predicate.
//! - [`verify_single_key_layer_proof_v0`] — verify a non-leaf merk
//!   proof for one expected key and recover its value bytes + chain
//!   commitment hash. Axis-agnostic.
//! - [`expect_merk_bytes`] — unwrap a `ProofBytes::Merk(_)` or reject
//!   with an axis-labelled error.
//! - [`execute_carrier_layer_proof`] — verify the carrier's multi-key
//!   merk proof and collect one [`OuterMatch`] per matched outer key.
//! - [`require_v1_envelope`] — extract the V1 root layer from a
//!   `GroveDBProof` or reject the proof. All aggregate axes require
//!   V1 envelopes; V0 predates the feature.
//!
//! The functions that produce diagnostic strings take an
//! `axis_label: &'static str` (and `query_type_name: &'static str` for
//! `require_v1_envelope`) so each axis's per-axis module can supply
//! its own prefix ("aggregate-count" / "aggregate-sum" /
//! "combined-aggregate", and "AggregateCountOnRange" / etc.) through a
//! thin wrapper, preserving the original error text.

use grovedb_merk::{
    proofs::{query::QueryProofVerify, Query as MerkQuery},
    CryptoHash,
};
use grovedb_query::{Query, QueryItem};

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes},
    Error, PathQuery,
};

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

/// Classification of any aggregate-on-range `PathQuery`. Encodes
/// either the leaf-only inner range (no carrier descent) or the
/// carrier outer items + leaf inner range + optional `subquery_path`
/// that the verifier must follow per outer key.
///
/// Axis-agnostic: every aggregate axis classifies into these same four
/// fields. Per-axis behaviour (which leaf verifier to call, which
/// terminal-type set to accept) lives downstream in each axis's
/// `per_key` walker — the classification descriptor only carries
/// structural information.
pub(in crate::operations::proof) struct AggregateClassification {
    /// The inner range that the leaf merk aggregate proof must
    /// satisfy.
    pub(in crate::operations::proof) leaf_inner_range: QueryItem,
    /// Carrier outer items. `None` for leaf-only queries.
    pub(in crate::operations::proof) carrier_outer_items: Option<Vec<QueryItem>>,
    /// Carrier subquery_path (the keys between each outer match and the
    /// leaf merk). Empty `Vec` if no subquery_path was set. `None` for
    /// leaf-only queries.
    pub(in crate::operations::proof) carrier_subquery_path: Option<Vec<Vec<u8>>>,
    /// Whether the outer query is left-to-right. Affects which results
    /// the merk_proof returns when the outer items are ranges. Always
    /// `true` for leaf-only.
    pub(in crate::operations::proof) carrier_left_to_right: bool,
}

/// Classify an aggregate-on-range path query and validate it at the
/// PathQuery level. The shape-specific pagination rules are enforced
/// through the axis's `validate` callback (leaf queries reject both
/// `SizedQuery::limit` and `SizedQuery::offset`; carrier queries
/// accept `SizedQuery::limit` but still reject `SizedQuery::offset`).
///
/// `validate` runs the axis's PathQuery-level validator and returns
/// the inner range (the same one for both leaf and carrier shapes —
/// for carriers it's the *subquery's* inner range).
///
/// `is_leaf` reports whether the top-level `Query` owns an aggregate-X
/// item directly (leaf shape) or only nested through subqueries
/// (carrier shape). Each axis supplies its own predicate so the shared
/// driver doesn't need to know which `QueryItem` variants count as the
/// axis's aggregate marker.
pub(in crate::operations::proof) fn classify_aggregate_path_query<F, G>(
    path_query: &PathQuery,
    validate: F,
    is_leaf: G,
) -> Result<AggregateClassification, Error>
where
    F: FnOnce(&PathQuery) -> Result<&QueryItem, Error>,
    G: Fn(&Query) -> bool,
{
    let leaf_inner = validate(path_query)?.clone();
    let q = &path_query.query.query;
    if is_leaf(q) {
        // Leaf shape: top-level aggregate item.
        return Ok(AggregateClassification {
            leaf_inner_range: leaf_inner,
            carrier_outer_items: None,
            carrier_subquery_path: None,
            carrier_left_to_right: true,
        });
    }
    // Carrier shape: validation above routed through the carrier
    // validator, so `leaf_inner` is the *subquery's* inner range. We
    // just need to extract the outer items and the optional
    // subquery_path.
    let outer_items = q.items.clone();
    let subquery_path = q
        .default_subquery_branch
        .subquery_path
        .clone()
        .unwrap_or_default();
    Ok(AggregateClassification {
        leaf_inner_range: leaf_inner,
        carrier_outer_items: Some(outer_items),
        carrier_subquery_path: Some(subquery_path),
        carrier_left_to_right: q.left_to_right,
    })
}

/// Extract the V1 root layer from a `GroveDBProof` envelope, or refuse
/// the proof. All three aggregate axes require V1 envelopes — V0
/// (`MerkOnlyLayerProof`) predates each aggregate feature and cannot
/// legitimately carry such a proof.
///
/// `query_type_name` is the user-facing aggregate type name (e.g.
/// `"AggregateCountOnRange"`, `"AggregateSumOnRange"`,
/// `"AggregateCountAndSumOnRange"`) and is interpolated into the
/// rejection message so callers can identify which axis rejected
/// their proof.
pub(in crate::operations::proof) fn require_v1_envelope<'a>(
    proof: &'a GroveDBProof,
    path_query: &PathQuery,
    query_type_name: &'static str,
) -> Result<&'a LayerProof, Error> {
    match proof {
        GroveDBProof::V1(GroveDBProofV1 { root_layer }) => Ok(root_layer),
        GroveDBProof::V0(_) => Err(Error::InvalidProof(
            path_query.clone(),
            format!(
                "{} proofs require V1 proof envelopes; V0 envelopes predate this feature and \
                 cannot legitimately carry such a proof",
                query_type_name
            ),
        )),
    }
}
