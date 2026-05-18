//! Proof generation and verification for `AggregateCountOnRange` queries.
//!
//! This module implements the count-only proof shape described in the GroveDB
//! book chapter "Aggregate Count Queries". It is intentionally **separate**
//! from `create_proof_internal`: regular proofs always descend into a queried
//! subtree, but count proofs *stop* at fully-inside subtree roots and emit a
//! single `HashWithCount` op for the entire collapsed subtree.
//!
//! The proof targets a `ProvableCountTree` or `ProvableCountSumTree` (or
//! their `NonCounted*` wrapper variants — wrappers only affect whether the
//! tree contributes to its parent's count, not its own internal count
//! mechanics). On any other tree type the entry point returns
//! `Error::InvalidProofError`.
//!
//! ## Module layout
//!
//! - [`prove`] — `impl RefWalker` block holding the public prover entry
//!   points (`create_aggregate_count_on_range_proof` and the no-proof
//!   `count_aggregate_on_range`).
//! - [`emit`] — the recursive proof-emission engine (`emit_count_proof`).
//! - [`walk`] — the no-proof equivalent walk (`walk_count_only`).
//! - [`verify`] — the verifier (`verify_aggregate_count_on_range_proof`)
//!   and its recursive shape-walker.
//! - [`tests`] / [`verify_only_tests`] — unit + integration tests.
//!
//! Range-bound classification is shared with the sum side via
//! [`super::aggregate_common`].

#[cfg(feature = "minimal")]
mod emit;
#[cfg(feature = "minimal")]
mod prove;
#[cfg(test)]
mod tests;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod verify;
#[cfg(test)]
mod verify_only_tests;
#[cfg(feature = "minimal")]
mod walk;

#[cfg(any(feature = "minimal", feature = "verify"))]
pub use verify::verify_aggregate_count_on_range_proof;

#[cfg(feature = "minimal")]
use crate::{
    tree::AggregateData,
    {Error, TreeType},
};

/// Returns true if `tree_type` is one of the tree types that can host an
/// `AggregateCountOnRange` proof. Wrapper types are accepted by stripping
/// down to the inner tree type via `is_provable_count_bearing`.
#[cfg(feature = "minimal")]
pub(super) fn is_provable_count_bearing(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableCountTree
            | TreeType::ProvableCountSumTree
            | TreeType::ProvableCountProvableSumTree
    )
}

/// Pull the count out of a `ProvableCount` / `ProvableCountAndSum` /
/// `ProvableCountAndProvableSum` aggregate. Returns `Err(CorruptedData)`
/// for any other variant — the entry point has already gated `tree_type`,
/// so reaching the error means the tree's in-memory state disagrees with
/// its declared type. This is a local invariant failure on the prover
/// side (we are walking *our own* merk), so `CorruptedData` is the
/// appropriate classification per the repo error-handling convention.
#[cfg(feature = "minimal")]
pub(super) fn provable_count_from_aggregate(data: AggregateData) -> Result<u64, Error> {
    match data {
        AggregateData::ProvableCount(c) => Ok(c),
        AggregateData::ProvableCountAndSum(c, _) => Ok(c),
        AggregateData::ProvableCountAndProvableSum(c, _) => Ok(c),
        other => Err(Error::CorruptedData(format!(
            "expected ProvableCount aggregate data on a provable count tree, got {:?}",
            other
        ))),
    }
}
