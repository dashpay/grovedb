//! Verifier for `AggregateCountAndSumOnRange` proofs.
//!
//! Two-phase structure mirroring the single-axis siblings:
//!
//! 1. **Phase 1** — replay the prover's op stream through
//!    `execute_with_options`, allowlisting the two node types the
//!    honest prover ever emits on a PCPS host:
//!    `HashWithCountAndSum` (for collapsed Disjoint/Contained subtrees)
//!    and `KVDigestCountSum` (for boundary nodes). Other node types are
//!    rejected immediately — the structural count and sum that any
//!    in-range derivation would otherwise need from them would not be
//!    hash-bound.
//!
//! 2. **Phase 2** — walk the reconstructed tree and re-derive BOTH
//!    the in-range count and the in-range sum in parallel, asserting
//!    that each node's type matches the classification its inherited
//!    bounds imply. This is the type-shape binding that makes the
//!    proof non-malleable; re-arranging ops would change the bound
//!    classification at some node and that node's emitted type would
//!    no longer match.
//!
//! Sum arithmetic is performed in `i128` during the walk and narrowed
//! to `i64` once at the end so adversarial extremes like
//! `i64::MAX + i64::MAX` cleanly surface as overflow instead of
//! silently wrapping.

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};

use crate::{
    proofs::{
        query::{
            aggregate_common::{
                classify_subtree, key_strictly_inside, SubtreeClassification, NULL_HASH,
            },
            QueryItem,
        },
        tree::{execute_with_options, Tree as ProofTree},
        Decoder, Node,
    },
    CryptoHash, Error,
};

/// Verify a combined count+sum proof for an
/// `AggregateCountAndSumOnRange` query.
///
/// `proof_bytes` is the encoded `Vec<Op>` produced by
/// [`crate::Merk::prove_aggregate_count_and_sum_on_range`];
/// `inner_range` is the same `QueryItem` the prover aggregated over
/// (caller-supplied — typically extracted from the verifier's
/// `PathQuery`).
///
/// On success returns `(merk_root_hash, count, sum)`:
/// - `merk_root_hash` is the root hash of the reconstructed merk; the
///   caller must compare it against the expected root hash to complete
///   verification.
/// - `count` is the number of keys in the inner range, computed by
///   replaying the prover's classification walk against the
///   reconstructed proof tree.
/// - `sum` is the signed `i64` sum of those keys' contributions.
///
/// **Two-phase verification.** Same defensive structure as the
/// single-axis sibling verifiers — allowlisting node types alone is
/// unsound, so we both reject blatantly wrong types up front and then
/// run a structural shape walk that binds each leaf's type to the
/// (subtree_bounds × range) classification.
///
/// **PCPS-only.** The honest prover only produces this proof shape
/// against `ProvableCountProvableSumTree` hosts. The op stream is
/// byte-identical to a `prove_aggregate_count_on_range` proof against
/// the same PCPS host, but a verifier replaying this stream still
/// needs both axes to reconstruct
/// `node_hash_with_count_and_sum(kv, l, r, count, sum)` — so the
/// caller must independently know the leaf merk is PCPS (a
/// non-PCPS-rooted GroveDB envelope is rejected at the
/// `GroveDb::verify_aggregate_count_and_sum_query` level before
/// reaching this function).
///
/// **Overflow handling.** The shape walk accumulates the sum in
/// `i128` and narrows to `i64` at the end. If the i128 result doesn't
/// fit in i64 the verifier returns `Error::InvalidProofError`.
///
/// **Empty merk case.** An empty merk is represented by an empty
/// proof byte stream and yields `(NULL_HASH, 0, 0)`. Callers chaining
/// this in a multi-layer proof should recognize that shape
/// explicitly.
pub fn verify_aggregate_count_and_sum_on_range_proof(
    proof_bytes: &[u8],
    inner_range: &QueryItem,
) -> CostResult<(CryptoHash, u64, i64), Error> {
    if proof_bytes.is_empty() {
        // Empty merk → empty proof → count = 0, sum = 0, hash = NULL_HASH.
        return Ok((NULL_HASH, 0u64, 0i64)).wrap_with_cost(OperationCost::default());
    }

    let mut cost = OperationCost::default();
    let decoder = Decoder::new(proof_bytes);

    // Phase 1: reconstruct the proof tree. The honest combined-aggregate
    // prover emits exactly two node types on a PCPS host —
    // `HashWithCountAndSum` (collapsed Disjoint/Contained subtrees) and
    // `KVDigestCountSum` (Boundary nodes). Both bind count and sum into
    // the hash via `node_hash_with_count_and_sum`.
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| match node {
            Node::HashWithCountAndSum(_, _, _, _, _) | Node::KVDigestCountSum(_, _, _, _) => Ok(()),
            other => Err(Error::InvalidProofError(format!(
                "unexpected node type in aggregate count+sum proof: {}",
                other
            ))),
        });
    let tree = cost_return_on_error!(&mut cost, tree_result);

    // Phase 2: shape-check + parallel walk of count and sum.
    let (count, sum_i128, _struct_count, _struct_sum) =
        match verify_count_and_sum_shape(&tree, inner_range, None, None) {
            Ok(quad) => quad,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };

    let sum: i64 = match i64::try_from(sum_i128) {
        Ok(v) => v,
        Err(_) => {
            return Err(Error::InvalidProofError(format!(
                "aggregate-count-and-sum proof: in-range sum overflowed i64 ({})",
                sum_i128
            )))
            .wrap_with_cost(cost);
        }
    };

    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok((root_hash, count, sum)).wrap_with_cost(cost)
}

/// Recursive shape-walk over the reconstructed proof tree. Returns the
/// quadruple `(in_range_count, in_range_sum_i128, structural_count,
/// structural_sum_i128)`:
///
/// - `in_range_count` / `in_range_sum_i128` — count and signed sum of
///   keys in the subtree that fall inside the inner range. The sum is
///   accumulated in i128; the outer entry point narrows it back to i64
///   once.
/// - `structural_count` / `structural_sum_i128` — the merk-recorded
///   aggregate count and sum of this subtree, both used by the parent
///   to derive its `own_count` / `own_sum`.
///
/// The structural count and sum of every child are **cryptographically
/// bound** to the parent's hash chain because every node in the
/// combined-aggregate proof (`KVDigestCountSum`, `HashWithCountAndSum`)
/// has both fields fed into `node_hash_with_count_and_sum` for hash
/// recomputation.
///
/// At each node:
///
/// - Compute the expected classification from the inherited subtree
///   bounds and the inner range.
/// - Require the node's type to match the classification (and reject
///   any children attached under a leaf-shape classification).
/// - Recurse with tightened bounds at `Boundary` nodes, summing with
///   `checked_add` (count) / saturating i128 arithmetic (sum). The
///   `own_count` derivation uses `checked_sub` to catch the
///   "children's structural count exceeds parent's" inconsistency
///   exactly like the single-axis count verifier. The corresponding
///   sum check is **not** performed (a negative `own_sum` is legal),
///   matching the single-axis sum verifier; the hash chain catches
///   any wrong arithmetic via root-hash mismatch.
fn verify_count_and_sum_shape(
    tree: &ProofTree,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<(u64, i128, u64, i128), Error> {
    let class = classify_subtree(lo, hi, range);
    match class {
        SubtreeClassification::Disjoint => {
            // Disjoint subtree contributes 0 to in-range count and sum;
            // its full structural count/sum feed the parent's
            // `own_count` / `own_sum` derivations.
            let (count, sum) = match &tree.node {
                Node::HashWithCountAndSum(_, _, _, c, s) => (*c, *s as i128),
                other => {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count-and-sum proof: expected HashWithCountAndSum at \
                         Disjoint position, got {}",
                        other
                    )));
                }
            };
            if tree.left.is_some() || tree.right.is_some() {
                return Err(Error::InvalidProofError(
                    "aggregate-count-and-sum proof: leaf hash-with-count-and-sum node at a \
                     Disjoint position must be a leaf"
                        .to_string(),
                ));
            }
            Ok((0, 0i128, count, sum))
        }
        SubtreeClassification::Contained => {
            // Contained subtree's structural count and sum equal its
            // in-range contributions.
            let (count, sum) = match &tree.node {
                Node::HashWithCountAndSum(_, _, _, c, s) => (*c, *s as i128),
                other => {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count-and-sum proof: expected HashWithCountAndSum at \
                         Contained position, got {}",
                        other
                    )));
                }
            };
            if tree.left.is_some() || tree.right.is_some() {
                return Err(Error::InvalidProofError(
                    "aggregate-count-and-sum proof: leaf hash-with-count-and-sum node at a \
                     Contained position must be a leaf"
                        .to_string(),
                ));
            }
            Ok((count, sum, count, sum))
        }
        SubtreeClassification::Boundary => {
            // Boundary nodes must be KVDigestCountSum and their key must
            // fall strictly inside the inherited subtree window.
            let (key, agg_count, agg_sum) = match &tree.node {
                Node::KVDigestCountSum(key, _, c, s) => (key, *c, *s as i128),
                other => {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count-and-sum proof: expected KVDigestCountSum at Boundary \
                         position, got {}",
                        other
                    )));
                }
            };
            if !key_strictly_inside(key.as_slice(), lo, hi) {
                return Err(Error::InvalidProofError(format!(
                    "aggregate-count-and-sum proof: boundary key {} falls outside its \
                     inherited subtree bounds (lo={:?}, hi={:?})",
                    hex::encode(key),
                    lo.map(hex::encode),
                    hi.map(hex::encode),
                )));
            }
            let key_slice = key.as_slice();
            let (left_in_c, left_in_s, left_struct_c, left_struct_s) = match &tree.left {
                Some(child) => verify_count_and_sum_shape(&child.tree, range, lo, Some(key_slice))?,
                None => (0u64, 0i128, 0u64, 0i128),
            };
            let (right_in_c, right_in_s, right_struct_c, right_struct_s) = match &tree.right {
                Some(child) => verify_count_and_sum_shape(&child.tree, range, Some(key_slice), hi)?,
                None => (0u64, 0i128, 0u64, 0i128),
            };
            // own_count: same checked_sub as the single-axis count
            // verifier — children claiming more keys than the parent's
            // aggregate signals a malformed proof.
            let own_count = agg_count
                .checked_sub(left_struct_c)
                .and_then(|s| s.checked_sub(right_struct_c))
                .ok_or_else(|| {
                    Error::InvalidProofError(format!(
                        "aggregate-count-and-sum proof: child structural counts ({} + {}) \
                         exceed parent's aggregate count ({}) at key {}",
                        left_struct_c,
                        right_struct_c,
                        agg_count,
                        hex::encode(key)
                    ))
                })?;
            // own_sum: signed i128 arithmetic; same rationale as the
            // single-axis sum verifier — no "child exceeds parent"
            // check makes sense for signed sums (a negative own_sum is
            // legal). The hash chain catches any wrong arithmetic via
            // root-hash mismatch.
            let own_sum = agg_sum - left_struct_s - right_struct_s;
            let (self_count_contribution, self_sum_contribution) = if range.contains(key_slice) {
                (own_count, own_sum)
            } else {
                (0u64, 0i128)
            };
            let in_range_count = left_in_c
                .checked_add(right_in_c)
                .and_then(|s| s.checked_add(self_count_contribution))
                .ok_or_else(|| {
                    Error::InvalidProofError(
                        "aggregate-count-and-sum proof: in-range count overflowed u64".to_string(),
                    )
                })?;
            let in_range_sum = left_in_s + right_in_s + self_sum_contribution;
            Ok((in_range_count, in_range_sum, agg_count, agg_sum))
        }
    }
}
