//! GroveDB-side prove/verify glue for `AggregateCountAndSumOnRange`
//! queries.
//!
//! Mirror of [`super::aggregate_count`] and [`super::aggregate_sum`]
//! for the dual-axis `ProvableCountProvableSumTree` (PCPS) host. The
//! merk-level pieces live in
//! `grovedb_merk::proofs::query::aggregate_count_and_sum` (proof
//! generation in `Merk::prove_aggregate_count_and_sum_on_range`,
//! proof verification in
//! `verify_aggregate_count_and_sum_on_range_proof`). This module adds
//! the GroveDB-level *envelope* handling: a verifier that walks the
//! multi-layer `GroveDBProof` chain (parent merk → ... → leaf merk),
//! verifies the path-element existence proofs at each non-leaf layer,
//! and delegates to the merk-level combined-aggregate verifier at the
//! leaf.
//!
//! The proof generator side is wired directly into
//! [`GroveDb::prove_subqueries_v1`] — see the
//! "Combined-aggregate short-circuit" branch there. Only V1 envelopes
//! support this proof; V0 is locked (see [`crate::operations::proof`]).
//!
//! ## Two shapes
//!
//! `AggregateCountAndSumOnRange` queries come in two flavors (mirror
//! of the count and sum sides):
//!
//! - **Leaf** — a single `AggregateCountAndSumOnRange(_)` item at the
//!   top level of the inner `Query`. The proof descends
//!   `path_query.path` via single-key existence checks and produces a
//!   single `(u64, i64)` at the leaf merk. Surfaced through
//!   [`GroveDb::verify_aggregate_count_and_sum_query`].
//!
//! - **Carrier** — an outer query whose items are `Key(_)` / `Range*(_)`
//!   and whose `default_subquery_branch.subquery` resolves to a leaf
//!   `AggregateCountAndSumOnRange`. Each matched outer key produces
//!   its own `(count, sum)`. Surfaced through
//!   [`GroveDb::verify_aggregate_count_and_sum_query_per_key`].
//!
//! Both shapes' terminal merk MUST be a `ProvableCountProvableSumTree`
//! host — the verifier rejects any other terminal element type, since
//! only PCPS hosts bind BOTH a count and a sum into the node hash.
//!
//! ## Module layout
//!
//! - [`classification`] — `AggregateCountAndSumClassification` struct
//!   and the `classify_aggregate_count_and_sum_path_query` function
//!   that distinguishes leaf vs. carrier shape.
//! - [`helpers`] — shared utilities (envelope decode, single-key
//!   layer verification, chain enforcement, leaf-level combined
//!   verification, multi-key outer proof execution).
//! - [`leaf_chain`] — the recursive walker used by the legacy
//!   single-`(u64, i64)` entry point.
//! - [`per_key`] — the carrier-shape walker that drives both shapes
//!   through the new `(outer_key, count, sum)` entry point.

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
    /// `AggregateCountAndSumOnRange` `PathQuery`, returning the GroveDB
    /// root hash plus BOTH the verified count AND the verified signed
    /// sum from a single proof.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_count_and_sum_on_range`] — a
    /// single `AggregateCountAndSumOnRange(_)` item, no subqueries, no
    /// pagination, and an inner range that isn't `Key`, `RangeFull`,
    /// or any aggregate variant. Any other shape is rejected up
    /// front with `Error::InvalidQuery` before any bytes are decoded.
    ///
    /// `AggregateCountAndSumOnRange` requires **V1 proof envelopes**
    /// (`GroveDBProofV1`). V0 envelopes predate the combined-aggregate
    /// feature and are rejected with `Error::InvalidProof`.
    ///
    /// Returns:
    /// - `root_hash` — the reconstructed GroveDB root hash. The caller
    ///   is responsible for comparing this against their trusted root
    ///   hash.
    /// - `count` — the number of keys in the inner range that were
    ///   committed by the proof.
    /// - `sum` — the signed `i64` sum of children with keys in the
    ///   inner range that were committed by the proof.
    ///
    /// Cryptographic guarantees:
    /// - At each non-leaf layer, a regular single-key merk proof
    ///   demonstrates that the next path element exists with the
    ///   recorded value bytes; the verifier checks the chain
    ///   `combine_hash(H(value), lower_hash) == parent_proof_hash` so a
    ///   forged path is impossible without a root-hash mismatch.
    /// - At the leaf layer, both count and sum are committed via
    ///   `node_hash_with_count_and_sum(kv_hash, left, right, count,
    ///   sum)` recomputation — tampering with either axis produces a
    ///   different reconstructed merk root, and the chain check above
    ///   then fails.
    /// - The leaf-level verifier uses an `i128` accumulator for the
    ///   sum and rejects any result that doesn't fit in `i64`, so
    ///   adversarial extremes cannot silently wrap.
    pub fn verify_aggregate_count_and_sum_query(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, u64, i64), Error> {
        check_grovedb_v0!(
            "verify_aggregate_count_and_sum_query",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );

        // Strict-leaf validation so the legacy single-`(u64, i64)`
        // entry point continues to reject carrier-shaped path queries.
        // The dispatcher `validate_aggregate_count_and_sum_on_range`
        // (and its SizedQuery sibling) now accepts both leaf and
        // carrier shapes; carrier queries must use
        // `verify_aggregate_count_and_sum_query_per_key` instead.
        let inner_range = path_query
            .validate_leaf_aggregate_count_and_sum_on_range()?
            .clone();

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
    /// `AggregateCountAndSumOnRange` `PathQuery` in either the leaf or
    /// carrier shape, returning one `(outer_key, count, sum)` triple per
    /// matched outer key.
    ///
    /// For a **leaf** combined-aggregate query the returned vector
    /// contains exactly one entry whose key is an empty byte string and
    /// whose `(count, sum)` matches the `(count, sum)`
    /// [`GroveDb::verify_aggregate_count_and_sum_query`] would have
    /// returned. This makes carrier and leaf consumers symmetric.
    ///
    /// For a **carrier** combined-aggregate query the outer items must
    /// be `Key(_)` / `Range*(_)`, the
    /// `default_subquery_branch.subquery` must validate as a leaf
    /// `AggregateCountAndSumOnRange`, and the optional `subquery_path`
    /// is followed exactly (single-key descent per element) before the
    /// combined-aggregate proof. The returned vector has one entry per
    /// matched outer key in **query-direction order**. Outer-key
    /// candidates that the prover proved as absent contribute no entry.
    ///
    /// **Dual-axis invariant:** Only `ProvableCountProvableSumTree`
    /// hosts can ground a combined-aggregate proof. The terminal-type
    /// gate rejects every other tree type, both at the leaf-only
    /// terminal and at each carrier outer-key match's terminal.
    ///
    /// Like [`GroveDb::verify_aggregate_count_and_sum_query`], this
    /// entry point requires **V1 proof envelopes**.
    pub fn verify_aggregate_count_and_sum_query_per_key(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<(Vec<u8>, u64, i64)>), Error> {
        check_grovedb_v0!(
            "verify_aggregate_count_and_sum_query_per_key",
            grove_version
                .grovedb_versions
                .operations
                .proof
                .verify_query_with_options
        );

        let classification =
            classification::classify_aggregate_count_and_sum_path_query(path_query)?;

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
/// the proof. `AggregateCountAndSumOnRange` requires V1 envelopes —
/// the V0 (`MerkOnlyLayerProof`) envelope predates the
/// combined-aggregate feature and is only emitted by grove versions
/// older than the one used by Dash Platform v12, so it cannot
/// legitimately contain a combined-aggregate proof.
fn require_v1_envelope<'a>(
    proof: &'a GroveDBProof,
    path_query: &PathQuery,
) -> Result<&'a LayerProof, Error> {
    match proof {
        GroveDBProof::V1(GroveDBProofV1 { root_layer }) => Ok(root_layer),
        GroveDBProof::V0(_) => Err(Error::InvalidProof(
            path_query.clone(),
            "AggregateCountAndSumOnRange proofs require V1 proof envelopes; V0 envelopes \
             predate this feature and cannot legitimately carry a combined-aggregate proof"
                .to_string(),
        )),
    }
}
