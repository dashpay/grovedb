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
//!   `default_subquery_branch.subquery` resolves to a leaf
//!   `AggregateCountOnRange`. The proof descends `path_query.path` via
//!   single-key checks, then at the carrier merk it produces a multi-key
//!   proof over the outer items; each matched outer key recurses through
//!   the `subquery_path` (if any) to a leaf merk that produces its own
//!   count. The verifier returns one `(outer_key, count)` pair per
//!   matched outer key. Surfaced through
//!   [`GroveDb::verify_aggregate_count_query_per_key`].
//!
//! The same leaf/carrier shape will apply to forthcoming aggregate
//! variants (sum, average) — each will get its own sibling module under
//! `grovedb/src/operations/proof/` with parallel naming.
//!
//! ## Module layout
//!
//! - [`classification`] — `AggregateCountClassification` struct and the
//!   `classify_aggregate_count_path_query` function that distinguishes
//!   leaf vs. carrier shape.
//! - [`leaf_chain`] — the recursive walker used by the legacy
//!   single-`u64` entry point.
//! - [`per_key`] — the carrier-shape walker that drives both shapes
//!   through the new `(outer_key, count)` entry point.
//! - [`helpers`] — shared utilities (envelope decode, single-key
//!   layer verification, chain enforcement, multi-key outer proof
//!   execution).

mod classification;
mod helpers;
mod leaf_chain;
mod per_key;

use grovedb_merk::CryptoHash;
use grovedb_version::{check_grovedb_v0, version::GroveVersion};

use crate::{
    operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof},
    Error, GroveDb, PathQuery,
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
    /// `RangeFull`, or another `AggregateCountOnRange`. Carrier-shape
    /// aggregate-count queries (outer `Keys` + `AggregateCountOnRange`
    /// subquery) must use
    /// [`GroveDb::verify_aggregate_count_query_per_key`] instead.
    ///
    /// `AggregateCountOnRange` requires **V1 proof envelopes**
    /// (`GroveDBProofV1`). V0 (`GroveDBProofV0` / `MerkOnlyLayerProof`)
    /// envelopes predate the aggregate-count feature and are only
    /// produced by grove versions older than the one used by Dash
    /// Platform v12; this entry point rejects them with
    /// `Error::InvalidProof`.
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
        // (which aggregate-count explicitly forbids) are enforced
        // alongside the inner-Query shape rules.
        let inner_range = path_query.validate_leaf_aggregate_count_on_range()?.clone();

        let grovedb_proof = super::decode_grovedb_proof_canonical(proof)?;
        let path_keys: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();

        let root_layer = require_v1_envelope(&grovedb_proof, path_query)?;
        let (root_hash, count) = leaf_chain::verify_v1_leaf_chain(
            root_layer,
            path_query,
            &path_keys,
            0,
            &inner_range,
            grove_version,
        )?;
        Ok((root_hash, count))
    }

    /// Verify a serialized `prove_query` proof against an
    /// `AggregateCountOnRange` `PathQuery` in either the leaf or carrier
    /// shape, returning one `(outer_key, count)` pair per matched outer
    /// key.
    ///
    /// For a **leaf** aggregate-count query the returned vector contains
    /// exactly one entry whose key is an empty byte string and whose
    /// count is the same `u64`
    /// [`GroveDb::verify_aggregate_count_query`] would have returned.
    /// This makes carrier and leaf consumers symmetric: callers that
    /// always process a `Vec<(Vec<u8>, u64)>` don't need to branch on
    /// the shape.
    ///
    /// For a **carrier** aggregate-count query the outer items must be
    /// `Key(_)` / `Range*(_)`, the `default_subquery_branch.subquery`
    /// must validate as a leaf `AggregateCountOnRange`, and the optional
    /// `subquery_path` is followed exactly (single-key descent per
    /// element) before the count proof. The returned vector has one
    /// entry per matched outer key in **query-direction order**: when
    /// the carrier's `left_to_right` is `true` (the default, matching
    /// the merk prover's natural walk) entries come back in ascending
    /// lexicographic key order; when `left_to_right` is `false` they
    /// come back in descending order, mirroring the merk proof's own
    /// emission order. Outer-key candidates that the prover proved as
    /// absent contribute no entry.
    ///
    /// Like [`GroveDb::verify_aggregate_count_query`], this entry point
    /// requires **V1 proof envelopes**. V0 envelopes predate the
    /// aggregate-count feature and are rejected with
    /// `Error::InvalidProof`.
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
        let classification = classification::classify_aggregate_count_path_query(path_query)?;

        let grovedb_proof = super::decode_grovedb_proof_canonical(proof)?;
        let path_keys: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();

        let root_layer = require_v1_envelope(&grovedb_proof, path_query)?;
        per_key::verify_v1_with_classification(
            root_layer,
            path_query,
            &path_keys,
            &classification,
            grove_version,
        )
    }
}

/// Extract the V1 root layer from a `GroveDBProof` envelope, or refuse
/// the proof. `AggregateCountOnRange` (both leaf and carrier) requires
/// V1 envelopes — the V0 (`MerkOnlyLayerProof`) envelope predates the
/// aggregate-count feature and is only emitted by grove versions older
/// than the one used by Dash Platform v12, so it cannot legitimately
/// contain an aggregate-count proof.
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
