//! Proof generation and verification for offset-paginated range queries
//! against `ProvableCountTree` and `ProvableCountSumTree` merks.
//!
//! ## What this module is for
//!
//! Regular [`super::create_proof`] cannot support a non-zero
//! `SizedQuery::offset` because the protocol has no way to attest "I
//! skipped exactly N in-range items before returning these ones" — a
//! malicious prover could just drop arbitrary items, and a regular merk
//! proof has nothing hash-bound that says otherwise. The
//! [`AggregateCountOnRange`] proof solved this for *count-only* answers
//! by leaning on the count-bound `HashWithCount` node, which commits a
//! subtree's structural count into the parent's hash chain via
//! `node_hash_with_count`.
//!
//! This module extends that same machinery to *paginated retrieval*:
//! offset+limit on a single range query over a count tree. Skipped
//! subtrees collapse to a single `HashWithCount` op (same as
//! AggregateCountOnRange) so the offset region pays O(log n) proof size
//! per skipped subtree rather than O(skipped). Returned items inside
//! the limit window emit as normal count-bearing value nodes (`KVCount`
//! / `KVCountSum` for directly-valued entries, `KVValueHashFeatureType`
//! for tree- and reference-shaped ones), so the result shape is
//! byte-identical to what a regular merk verifier would produce for the
//! same range without offset.
//!
//! Reference rows arrive here as `KVValueHashFeatureType` carrying the
//! REFERENCE's bytes. The layer above rewrites those into the
//! `KVRefValueHash{Count,CountSum}` family carrying the resolved target
//! — the same post-pass the regular count-tree flow runs — and this
//! verifier accepts and authenticates the rewritten variants.
//!
//! ## Why `ProvableCountSumTree` only commits the count (not the sum)
//!
//! `ProvableCountSumTree` nodes hash via `node_hash_with_count` — the
//! sum is stored on the node but is **not** bound to the node hash (see
//! `merk/src/tree/mod.rs`). This is the same shape `AggregateCountOnRange`
//! relies on, and the same property `HashWithCount` exploits: a single
//! count-bearing op suffices to verify a collapsed subtree regardless of
//! whether the tree variant is `ProvableCountTree` or
//! `ProvableCountSumTree`. Offset accounting therefore only commits the
//! count; the sum (if any) plays no role here.
//!
//! ## Dual-axis `ProvableCountProvableSumTree` hosts
//!
//! `ProvableCountProvableSumTree` (PCPS) hashes via
//! `node_hash_with_count_and_sum` — BOTH the count AND the sum are
//! committed into every node hash. A plain `HashWithCount(count)` would
//! not recompute the right node hash for a PCPS host (its hash function
//! takes a sum input the count-only op doesn't carry), so this module
//! emits the dual-axis `HashWithCountAndSum` / `KVDigestCountSum`
//! variants for PCPS hosts, parallel to the dispatch in
//! [`super::aggregate_count::emit`]. Offset accounting itself is still
//! count-only — the sum is committed alongside purely for hash
//! reconstruction; it plays no role in offset/limit consumption.
//!
//! ## Scope
//!
//! - **Tree type**: `ProvableCountTree`, `ProvableCountSumTree`, or
//!   `ProvableCountProvableSumTree` (PCPS). Other tree types are
//!   rejected at the entry point.
//! - **Query shape**: a single `QueryItem` range. Multi-item queries,
//!   subqueries, and conditional branches are out of scope (callers
//!   producing those must fall back to the regular proof path, which
//!   continues to reject offset).
//! - **Direction**: both ascending (`left_to_right = true`) and
//!   descending (`left_to_right = false`) are supported. The descending
//!   walk is a structural mirror: walk the right child first, emit
//!   inverted ops, treat "the first N in-range keys" as the N highest
//!   keys.
//!
//! ## Module layout
//!
//! - [`emit`] — recursive proof emitter (`emit_count_offset_proof`).
//! - [`prove`] — public entry point on [`crate::tree::RefWalker`].
//! - [`verify`] — verifier (`verify_count_offset_on_range_proof`) +
//!   recursive shape-walk that re-derives the offset/limit accounting
//!   from the reconstructed proof tree.
//! - [`tests`] — round-trip unit tests covering both directions, empty
//!   trees, offset/limit composition, `NonCounted` entries, and
//!   `ProvableCountSumTree`.
//!
//! The range-bound classifier (`classify_subtree`) is shared with the
//! aggregate-count and aggregate-sum sides via
//! [`super::aggregate_common`].

#[cfg(feature = "minimal")]
mod emit;
#[cfg(feature = "minimal")]
mod prove;
#[cfg(test)]
mod tests;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod verify;

#[cfg(feature = "minimal")]
pub use prove::ProverCountOffsetResult;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use verify::{
    verify_count_offset_on_range_proof, CountOffsetProofResult, CountOffsetReturnedItem,
};

#[cfg(feature = "minimal")]
use crate::{
    tree::AggregateData,
    {Error, TreeType},
};

/// Returns true if `tree_type` is one of the three tree types that can host
/// an offset-paginated count-tree proof. `ProvableCountTree` /
/// `ProvableCountSumTree` use `node_hash_with_count` and emit the
/// single-axis `HashWithCount` / `KVDigestCount` skip ops;
/// `ProvableCountProvableSumTree` uses `node_hash_with_count_and_sum` and
/// emits the dual-axis `HashWithCountAndSum` / `KVDigestCountSum` variants
/// so the verifier can reconstruct the right node hash.
#[cfg(feature = "minimal")]
pub(super) fn is_provable_count_bearing(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableCountTree
            | TreeType::ProvableCountSumTree
            | TreeType::ProvableCountProvableSumTree
    )
}

/// Returns true when the host tree binds BOTH count and sum into its node
/// hash (i.e. `ProvableCountProvableSumTree`). When true, the count-offset
/// emit/verify paths use the dual-axis Node variants
/// (`HashWithCountAndSum`, `KVDigestCountSum`) so the verifier can
/// reconstruct `node_hash_with_count_and_sum`. For the single-axis
/// `ProvableCountTree` / `ProvableCountSumTree` hosts, this returns
/// false and the count-only `HashWithCount` / `KVDigestCount` ops
/// suffice.
#[cfg(feature = "minimal")]
#[inline]
pub(super) fn binds_sum_into_hash(tree_type: TreeType) -> bool {
    matches!(tree_type, TreeType::ProvableCountProvableSumTree)
}

/// Pull the count out of a `ProvableCount` / `ProvableCountAndSum` /
/// `ProvableCountAndProvableSum` aggregate. Returns `Err(InvalidProofError)`
/// for any other variant — the entry point gates `tree_type` so reaching
/// the error means the tree's in-memory state disagrees with its declared
/// type.
#[cfg(feature = "minimal")]
pub(super) fn provable_count_from_aggregate(data: AggregateData) -> Result<u64, Error> {
    match data {
        AggregateData::ProvableCount(c) => Ok(c),
        AggregateData::ProvableCountAndSum(c, _) => Ok(c),
        AggregateData::ProvableCountAndProvableSum(c, _) => Ok(c),
        other => Err(Error::InvalidProofError(format!(
            "expected ProvableCount aggregate data on a provable count tree, got {:?}",
            other
        ))),
    }
}

/// Pull the sum out of a `ProvableCountAndProvableSum` aggregate. Used by
/// the PCPS-host emit path to populate the dual-axis Node variants'
/// sum field, which the verifier needs to reconstruct
/// `node_hash_with_count_and_sum`. Returns `Err(InvalidProofError)` for
/// any other aggregate variant — the dual-axis emit path is only
/// reached when `binds_sum_into_hash(tree_type)` is true, which gates
/// the aggregate to `ProvableCountAndProvableSum`.
#[cfg(feature = "minimal")]
pub(super) fn provable_sum_from_dual_axis_aggregate(data: AggregateData) -> Result<i64, Error> {
    match data {
        AggregateData::ProvableCountAndProvableSum(_, s) => Ok(s),
        other => Err(Error::InvalidProofError(format!(
            "expected ProvableCountAndProvableSum aggregate data on a ProvableCountProvableSumTree, \
             got {:?}",
            other
        ))),
    }
}
