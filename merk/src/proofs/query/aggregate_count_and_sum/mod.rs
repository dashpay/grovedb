//! Proof generation and verification for `AggregateCountAndSumOnRange`
//! queries.
//!
//! This module implements the **combined** count+sum proof shape for
//! `ProvableCountProvableSumTree` (PCPS) hosts. It is the dual-axis
//! sibling of [`super::aggregate_count`] and [`super::aggregate_sum`].
//!
//! PCPS commits BOTH the running count and the running sum into every
//! node's hash via `node_hash_with_count_and_sum(kv, l, r, count, sum)`.
//! That means a single proof can carry both aggregates with the same
//! ops the count proof on PCPS already emits — `HashWithCountAndSum`
//! for fully-inside / fully-outside collapsed subtrees and
//! `KVDigestCountSum` for boundary nodes. The combined-variant
//! verifier walks the reconstructed tree once and accumulates BOTH
//! axes in parallel.
//!
//! ## Why a separate module rather than reusing count's prover
//!
//! The proof bytes for `AggregateCountAndSumOnRange` against PCPS are
//! byte-identical to `AggregateCountOnRange` against PCPS — both
//! emitters output the same dual-axis variants when the host tree is
//! PCPS. We could technically reuse `prove_aggregate_count_on_range`
//! for the prover side and only ship a new verifier. But:
//!
//! 1. The prover side computes the count axis. Adding a second pass
//!    to compute the sum is wasteful when a single walk can track
//!    both axes simultaneously.
//! 2. PCPS-only is a cleaner gate when the entry point is dedicated
//!    rather than borrowed from count's three-host gate.
//!
//! So this module keeps a near-clone of `emit_count_proof` that
//! tracks both axes and only accepts PCPS hosts, plus a verifier that
//! walks both axes in parallel.
//!
//! On any non-PCPS tree type the entry points return
//! `Error::InvalidProofError`.
//!
//! ## Module layout
//!
//! - [`prove`] — `impl RefWalker` block holding the public prover
//!   entry point (`create_aggregate_count_and_sum_on_range_proof`).
//! - [`emit`] — the recursive proof-emission engine
//!   (`emit_count_and_sum_proof`).
//! - [`verify`] — the verifier
//!   (`verify_aggregate_count_and_sum_on_range_proof`) and its
//!   recursive shape-walker.
//! - [`tests`] — unit + integration tests.
//!
//! Range-bound classification is shared with the single-axis siblings
//! via [`super::aggregate_common`].

#[cfg(feature = "minimal")]
mod emit;
#[cfg(feature = "minimal")]
mod prove;
#[cfg(test)]
mod tests;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod verify;
#[cfg(feature = "minimal")]
mod walk;

#[cfg(any(feature = "minimal", feature = "verify"))]
pub use verify::verify_aggregate_count_and_sum_on_range_proof;

#[cfg(feature = "minimal")]
use crate::{tree::AggregateData, Error, TreeType};

/// Returns true if `tree_type` is a host that can serve an
/// `AggregateCountAndSumOnRange` proof. Only
/// `ProvableCountProvableSumTree` qualifies — it is the only tree type
/// whose node hash binds BOTH a count and a sum. The single-axis hosts
/// (`ProvableCountTree`, `ProvableCountSumTree`, `ProvableSumTree`)
/// cannot host this query: their node hashes only bind one of the two
/// aggregates, so the verifier could not cryptographically reconstruct
/// both.
#[cfg(feature = "minimal")]
pub(super) fn is_provable_count_and_sum_bearing(tree_type: TreeType) -> bool {
    matches!(tree_type, TreeType::ProvableCountProvableSumTree)
}

/// Pull the `(count, sum)` pair out of a
/// `ProvableCountAndProvableSum` aggregate. Returns `Err(CorruptedData)`
/// for any other variant — the entry point has already gated
/// `tree_type`, so reaching the error means the tree's in-memory state
/// disagrees with its declared type. This is a local invariant failure
/// on the prover side (we are walking *our own* merk), so
/// `CorruptedData` is the appropriate classification per the repo
/// error-handling convention.
#[cfg(feature = "minimal")]
pub(super) fn provable_count_and_sum_from_aggregate(
    data: AggregateData,
) -> Result<(u64, i64), Error> {
    match data {
        AggregateData::ProvableCountAndProvableSum(c, s) => Ok((c, s)),
        other => Err(Error::CorruptedData(format!(
            "expected ProvableCountAndProvableSum aggregate data on a \
             ProvableCountProvableSumTree, got {:?}",
            other
        ))),
    }
}
