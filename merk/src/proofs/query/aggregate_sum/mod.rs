//! Proof generation and verification for `AggregateSumOnRange` queries.
//!
//! This module is the sum-only twin of [`super::aggregate_count`]. It
//! implements the proof shape described in the GroveDB book chapter
//! "Aggregate Sum Queries": instead of returning the number of keys in the
//! inner range, the query returns the **signed `i64` sum** of children with
//! keys in that range against a `ProvableSumTree`.
//!
//! Like its count sibling, this module is intentionally **separate** from
//! `create_proof_internal`: regular proofs always descend into a queried
//! subtree, but sum proofs *stop* at fully-inside subtree roots and emit a
//! single `HashWithSum` op for the entire collapsed subtree.
//!
//! The proof targets a `ProvableSumTree` exclusively (the `NotSummed`
//! wrapper variant only affects whether the tree contributes to its parent's
//! sum, not its own internal sum mechanics). On any other tree type the
//! entry point returns `Error::InvalidProofError`.
//!
//! ## Module layout
//!
//! - [`prove`] — `impl RefWalker` block holding the public prover entry
//!   points (`create_aggregate_sum_on_range_proof` and the no-proof
//!   `sum_aggregate_on_range`).
//! - [`emit`] — the recursive proof-emission engine (`emit_sum_proof`).
//! - [`walk`] — the no-proof equivalent walk (`walk_sum_only`).
//! - [`verify`] — the verifier (`verify_aggregate_sum_on_range_proof`)
//!   and its recursive shape-walker.
//! - [`tests`] — unit + integration tests.
//!
//! Range-bound classification is shared with the count side via
//! [`super::aggregate_common`].
//!
//! ## Negative-sum gotchas mirrored from the count side
//!
//! - The accumulator can legitimately reach zero with non-zero children
//!   (e.g. `+5` plus `-5`), so there is no "if sum == 0 → short-circuit"
//!   shortcut here — the count code uses `if count == 0` in a few places
//!   that would be unsound here. The only zero-skip pattern that's
//!   correct for sum is "subtree is fully outside range → contributes 0",
//!   driven purely by the bound classification.
//! - The verifier accumulates in `i128` and narrows to `i64` at the end so
//!   adversarial inputs like `i64::MAX + i64::MAX` are detected as
//!   overflow instead of silently wrapping.

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
pub use verify::verify_aggregate_sum_on_range_proof;

#[cfg(feature = "minimal")]
use crate::{
    tree::AggregateData,
    {Error, TreeType},
};

/// Returns true if `tree_type` is one that can host an `AggregateSumOnRange`
/// proof. Only `ProvableSumTree` is valid — the `Sum` / `BigSum` trees use
/// different hash dispatches (the inserted-value hash is not bound through
/// `node_hash_with_sum` for those) and can't produce verifiable sum proofs.
#[cfg(feature = "minimal")]
pub(super) fn is_provable_sum_bearing(tree_type: TreeType) -> bool {
    matches!(tree_type, TreeType::ProvableSumTree)
}

/// Pull the sum out of a `ProvableSum` aggregate. Returns
/// `Err(CorruptedData)` for any other variant — the entry point has
/// already gated `tree_type`, so reaching the error means the tree's
/// in-memory state disagrees with its declared type. This is a local
/// invariant failure on the prover side (we are walking *our own*
/// merk), so `CorruptedData` is the appropriate classification per the
/// repo error-handling convention.
#[cfg(feature = "minimal")]
pub(super) fn provable_sum_from_aggregate(data: AggregateData) -> Result<i64, Error> {
    match data {
        AggregateData::ProvableSum(s) => Ok(s),
        other => Err(Error::CorruptedData(format!(
            "expected ProvableSum aggregate data on a provable sum tree, got {:?}",
            other
        ))),
    }
}
