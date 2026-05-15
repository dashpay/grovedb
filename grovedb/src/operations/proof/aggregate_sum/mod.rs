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
//! ## Module layout
//!
//! - [`leaf_chain`] — the recursive walker that descends `path_query.path`
//!   layer by layer and delegates to the merk-level sum verifier at the
//!   leaf.
//! - [`helpers`] — shared utilities (envelope decode, single-key layer
//!   verification, chain enforcement, leaf sum verification).
//!
//! Unlike [`super::aggregate_count`], `AggregateSumOnRange` only supports
//! the leaf shape (a single `AggregateSumOnRange(_)` item at the top level
//! of the inner `Query`). The carrier shape (outer `Key`/`Range*` items
//! routing to an aggregate-sum subquery) is not yet wired in the merk-level
//! prover, so there is no per-key entry point or classification module.

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

        let inner_range = path_query.validate_aggregate_sum_on_range()?.clone();

        let grovedb_proof = helpers::decode_grovedb_proof(proof)?;
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
