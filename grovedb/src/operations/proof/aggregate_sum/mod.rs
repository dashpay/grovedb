//! GroveDB-side prove/verify glue for `AggregateSumOnRange` queries.
//!
//! Mirror of [`super::aggregate_count`] for the `ProvableSumTree` flavor.
//! The merk-level pieces live in `grovedb_merk::proofs::query::aggregate_sum`
//! (proof generation in `Merk::prove_aggregate_sum_on_range`, proof
//! verification in `verify_aggregate_sum_on_range_proof`). This module adds
//! the GroveDB-level *envelope* handling: a verifier that walks the
//! multi-layer `GroveDBProof` chain (parent merk → ... → leaf merk),
//! verifies the path-element existence proofs at each non-leaf layer, and
//! delegates to the merk-level sum verifier at the leaf.
//!
//! The proof generator side is wired directly into
//! [`GroveDb::prove_subqueries`] / [`GroveDb::prove_subqueries_v1`] — see
//! the "Aggregate-sum short-circuit" branches there.
//!
//! ## Two shapes
//!
//! `AggregateSumOnRange` queries come in two flavors (mirror of the
//! count side):
//!
//! - **Leaf** — a single `AggregateSumOnRange(_)` item at the top level
//!   of the inner `Query`. The proof descends `path_query.path` via
//!   single-key existence checks and produces a single `i64` at the
//!   leaf merk. Surfaced through
//!   [`GroveDb::verify_aggregate_sum_query`].
//!
//! - **Carrier** — an outer query whose items are `Key(_)` / `Range*(_)`
//!   (one IN-style fan-out dimension) and whose
//!   `default_subquery_branch.subquery` resolves to a leaf
//!   `AggregateSumOnRange`. Each matched outer key produces its own
//!   sum. Surfaced through
//!   [`GroveDb::verify_aggregate_sum_query_per_key`].
//!
//! ## Module layout
//!
//! - [`classification`] — `AggregateSumClassification` struct and the
//!   `classify_aggregate_sum_path_query` function that distinguishes
//!   leaf vs. carrier shape.
//! - [`leaf_chain`] — the recursive walker used by the legacy
//!   single-`i64` entry point.
//! - [`per_key`] — the carrier-shape walker that drives both shapes
//!   through the new `(outer_key, sum)` entry point.
//! - [`helpers`] — shared utilities (envelope decode, single-key layer
//!   verification, chain enforcement, leaf sum verification, multi-key
//!   outer proof execution).

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
    /// Verify a serialized `prove_query` proof against an
    /// `AggregateSumOnRange` `PathQuery`, returning the GroveDB root hash
    /// and the verified signed sum.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_sum_on_range`] — a single
    /// `AggregateSumOnRange(_)` item, no subqueries, no pagination, and an
    /// inner range that isn't `Key`, `RangeFull`, another
    /// `AggregateSumOnRange`, or an `AggregateCountOnRange`. Any other
    /// shape is rejected up front with `Error::InvalidQuery` before any
    /// bytes are decoded.
    ///
    /// `AggregateSumOnRange` requires **V1 proof envelopes**
    /// (`GroveDBProofV1`). V0 (`GroveDBProofV0` / `MerkOnlyLayerProof`)
    /// envelopes predate the aggregate-sum feature and are only produced by
    /// grove versions older than the one used by Dash Platform v12; this
    /// entry point rejects them with `Error::InvalidProof`.
    ///
    /// Returns:
    /// - `root_hash` — the reconstructed GroveDB root hash. The caller is
    ///   responsible for comparing this against their trusted root hash.
    /// - `sum` — the signed `i64` sum of children with keys in the inner
    ///   range that were committed by the proof.
    ///
    /// Cryptographic guarantees:
    /// - At each non-leaf layer, a regular single-key merk proof
    ///   demonstrates that the next path element exists with the recorded
    ///   value bytes; the verifier checks the chain
    ///   `combine_hash(H(value), lower_hash) == parent_proof_hash` so a
    ///   forged path is impossible without a root-hash mismatch.
    /// - At the leaf layer, the sum is committed by `HashWithSum`'s
    ///   `node_hash_with_sum(kv_hash, left, right, sum)` recomputation —
    ///   tampering with the sum produces a different reconstructed merk
    ///   root, and the chain check above then fails.
    /// - The leaf-level verifier uses an `i128` accumulator and rejects
    ///   any result that doesn't fit in `i64`, so adversarial extremes
    ///   like two `i64::MAX` children cannot silently wrap.
    pub fn verify_aggregate_sum_query(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, i64), Error> {
        check_grovedb_v0!(
            "verify_aggregate_sum_query",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );

        // Strict-leaf validation so the legacy single-`i64` entry point
        // continues to reject carrier-shaped path queries. The dispatcher
        // `validate_aggregate_sum_on_range` (and its SizedQuery sibling)
        // now accepts both leaf and carrier shapes; carrier queries must
        // use `verify_aggregate_sum_query_per_key` instead.
        let inner_range = path_query.validate_leaf_aggregate_sum_on_range()?.clone();

        let grovedb_proof = super::decode_grovedb_proof_canonical(proof)?;
        let path_keys: Vec<&[u8]> = path_query.path.iter().map(|p| p.as_slice()).collect();

        let root_layer = require_v1_envelope(&grovedb_proof, path_query)?;
        leaf_chain::verify_v1_leaf_chain(
            root_layer,
            path_query,
            &path_keys,
            0,
            &inner_range,
            grove_version,
        )
    }

    /// Verify a serialized `prove_query` proof against an
    /// `AggregateSumOnRange` `PathQuery` in either the leaf or carrier
    /// shape, returning one `(outer_key, sum)` pair per matched outer
    /// key.
    ///
    /// For a **leaf** aggregate-sum query the returned vector contains
    /// exactly one entry whose key is an empty byte string and whose
    /// sum is the same `i64`
    /// [`GroveDb::verify_aggregate_sum_query`] would have returned.
    /// This makes carrier and leaf consumers symmetric: callers that
    /// always process a `Vec<(Vec<u8>, i64)>` don't need to branch on
    /// the shape.
    ///
    /// For a **carrier** aggregate-sum query the outer items must be
    /// `Key(_)` / `Range*(_)`, the `default_subquery_branch.subquery`
    /// must validate as a leaf `AggregateSumOnRange`, and the optional
    /// `subquery_path` is followed exactly (single-key descent per
    /// element) before the sum proof. The returned vector has one
    /// entry per matched outer key in **query-direction order**: when
    /// the carrier's `left_to_right` is `true` (the default) entries
    /// come back in ascending lexicographic key order; when
    /// `left_to_right` is `false` they come back in descending order,
    /// mirroring the merk proof's own emission order. Outer-key
    /// candidates that the prover proved as absent contribute no entry.
    ///
    /// Like [`GroveDb::verify_aggregate_sum_query`], this entry point
    /// requires **V1 proof envelopes**. V0 envelopes predate the
    /// aggregate-sum feature and are rejected with
    /// `Error::InvalidProof`.
    ///
    /// Cryptographic guarantees:
    /// - Every layer is committed via the same `combine_hash(H(value),
    ///   lower_hash) == parent_proof_hash` chain check used by the leaf
    ///   verifier, so a forged path through the carrier or
    ///   `subquery_path` produces a root-hash mismatch.
    /// - Each per-outer-key sum is committed by the leaf
    ///   `HashWithSum` / `KVDigestSum` recomputation; sums can't be
    ///   tampered with independently.
    pub fn verify_aggregate_sum_query_per_key(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<(Vec<u8>, i64)>), Error> {
        check_grovedb_v0!(
            "verify_aggregate_sum_query_per_key",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );

        // Classify the query and extract the leaf inner range plus the
        // optional carrier subquery_path. For leaf queries the carrier
        // descent below is skipped (carrier_outer_items is None).
        let classification = classification::classify_aggregate_sum_path_query(path_query)?;

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
/// the proof. `AggregateSumOnRange` requires V1 envelopes — the V0
/// (`MerkOnlyLayerProof`) envelope predates the aggregate-sum feature and
/// is only emitted by grove versions older than the one used by Dash
/// Platform v12, so it cannot legitimately contain an aggregate-sum proof.
fn require_v1_envelope<'a>(
    proof: &'a GroveDBProof,
    path_query: &PathQuery,
) -> Result<&'a LayerProof, Error> {
    match proof {
        GroveDBProof::V1(GroveDBProofV1 { root_layer }) => Ok(root_layer),
        GroveDBProof::V0(_) => Err(Error::InvalidProof(
            path_query.clone(),
            "AggregateSumOnRange proofs require V1 proof envelopes; V0 envelopes predate \
             this feature and cannot legitimately carry an aggregate-sum proof"
                .to_string(),
        )),
    }
}
