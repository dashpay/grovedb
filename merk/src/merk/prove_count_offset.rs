//! Offset-paginated proof generation for `ProvableCountTree` /
//! `ProvableCountSumTree`. Split out of the main [`super::prove`]
//! file because the method is version-gated independently and the
//! split keeps the version contract immediately visible.
//!
//! The actual proof emission lives in
//! [`crate::proofs::query::count_offset`] — this file only owns the
//! `Merk::prove_count_offset_on_range` entry point + its version
//! check.

use std::collections::LinkedList;

use grovedb_costs::{CostResult, CostsExt};
use grovedb_storage::StorageContext;
use grovedb_version::{check_merk_v0_with_cost, version::GroveVersion};

use crate::{
    proofs::query::{count_offset::ProverCountOffsetResult, QueryItem},
    tree::RefWalker,
    Error, Merk,
};

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Generate an offset-paginated proof for a single-range query
    /// against a `ProvableCountTree` or `ProvableCountSumTree`.
    ///
    /// This is the count-tree analogue of the regular [`Self::prove`]
    /// path, with one key extension: a non-zero `offset` is honored.
    /// The proof commits the count of skipped items via the same
    /// `HashWithCount` infrastructure used by
    /// [`Self::prove_aggregate_count_on_range`], so the offset region
    /// pays O(log n) proof size per skipped subtree rather than
    /// O(skipped). Returned items inside the limit window emit as
    /// normal count-bearing value nodes, so the verifier-side result
    /// shape matches what a regular range query without offset would
    /// produce.
    ///
    /// `inner_range` is the single `QueryItem` to scan (already
    /// validated at the caller's `Query`/`PathQuery` level). `offset`
    /// is how many leading in-range items to skip (in directional
    /// order); `limit` is the maximum number of items to return after
    /// the offset (`None` means unlimited). `left_to_right` controls
    /// iteration direction.
    ///
    /// The merk's `tree_type` must be one of `ProvableCountTree`,
    /// `ProvableCountSumTree`, or `ProvableCountProvableSumTree`. Any
    /// other tree type is rejected with `Error::InvalidProofError`
    /// before any walking happens — count commitments are only
    /// meaningful against trees that bind their count into the node
    /// hash. For PCPS hosts the emit path additionally commits the sum
    /// into the collapsed-subtree ops (`HashWithCountAndSum`) so the
    /// verifier reconstructs `node_hash_with_count_and_sum` instead of
    /// the count-only flavor. Empty merk: returns an empty
    /// `ProverCountOffsetResult` (no ops, 0 returned, full offset
    /// remaining).
    ///
    /// # Versioning
    ///
    /// Gated on `MerkProofVersions::prove_count_offset_on_range`
    /// (`merk_versions.proof.prove_count_offset_on_range`). The
    /// initial implementation is version 0 — bump that field in a
    /// future grove version if the emitted op stream needs to change
    /// shape in a way that requires a coordinated verifier update.
    pub fn prove_count_offset_on_range(
        &self,
        inner_range: &QueryItem,
        offset: u64,
        limit: Option<u64>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<ProverCountOffsetResult, Error> {
        check_merk_v0_with_cost!(
            "prove_count_offset_on_range",
            grove_version
                .merk_versions
                .proof
                .prove_count_offset_on_range
        );

        let tree_type = self.tree_type;
        if !matches!(
            tree_type,
            crate::TreeType::ProvableCountTree
                | crate::TreeType::ProvableCountSumTree
                | crate::TreeType::ProvableCountProvableSumTree
        ) {
            return Err(Error::InvalidProofError(format!(
                "count-offset paginated proof is only valid against ProvableCountTree, \
                 ProvableCountSumTree, or ProvableCountProvableSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(Default::default());
        }
        self.use_tree_mut(|maybe_tree| match maybe_tree {
            None => Ok(ProverCountOffsetResult {
                ops: LinkedList::new(),
                returned: 0,
                offset_remaining: offset,
            })
            .wrap_with_cost(Default::default()),
            Some(tree) => {
                let mut ref_walker = RefWalker::new(tree, self.source());
                ref_walker.create_count_offset_on_range_proof(
                    inner_range,
                    offset,
                    limit,
                    left_to_right,
                    tree_type,
                    grove_version,
                )
            }
        })
    }
}
