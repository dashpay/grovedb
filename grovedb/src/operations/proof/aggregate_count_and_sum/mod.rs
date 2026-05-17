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
//! ## Shape
//!
//! `AggregateCountAndSumOnRange` queries only support the **leaf**
//! shape: a single `AggregateCountAndSumOnRange(_)` item at the top
//! level of the inner `Query`. The proof descends `path_query.path`
//! via single-key existence checks and produces a single `(u64, i64)`
//! at the leaf merk. The terminal merk MUST be a PCPS host — the
//! verifier rejects any other terminal element type.
//!
//! ## Module layout
//!
//! - [`helpers`] — shared utilities (envelope decode, single-key
//!   layer verification, chain enforcement, leaf-level combined
//!   verification).
//! - [`leaf_chain`] — the recursive walker that descends
//!   `path_query.path` layer by layer.

mod helpers;
mod leaf_chain;

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

        let inner_range = path_query
            .validate_aggregate_count_and_sum_on_range()?
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
