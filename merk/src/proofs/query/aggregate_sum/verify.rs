//! Verifier for `AggregateSumOnRange` proofs.
//!
//! Two-phase structure, mirroring the count side:
//!
//! 1. **Phase 1** — replay the prover's op stream through
//!    `execute_with_options`, allowlisting the two node types the honest
//!    prover ever emits (`HashWithSum` for collapsed Disjoint/Contained
//!    subtrees, `KVDigestSum` for boundary nodes). Anything else is
//!    rejected up front — including plain `Hash(_)`, whose sum is not
//!    hash-bound and would let a malicious prover skew the boundary
//!    arithmetic.
//!
//! 2. **Phase 2** — walk the reconstructed tree and re-derive the
//!    in-range sum, asserting that each node's type matches the
//!    classification its inherited bounds imply
//!    (Disjoint/Contained → leaf `HashWithSum`; Boundary →
//!    `KVDigestSum` whose key is strictly inside the inherited window).
//!    This is the type-shape binding that makes the proof
//!    non-malleable — re-arranging the ops would change the bound
//!    classification at some node and that node's emitted type would no
//!    longer match.
//!
//! All accumulation is done in `i128`. The narrow to `i64` happens once
//! at the very end so adversarial inputs like `i64::MAX + i64::MAX`
//! cleanly surface as overflow instead of silently wrapping.

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

/// Verify a sum-only proof for an `AggregateSumOnRange` query.
///
/// `proof_bytes` is the encoded `Vec<Op>` produced by
/// [`crate::Merk::prove_aggregate_sum_on_range`]; `inner_range` is the same
/// `QueryItem` the prover summed over (caller-supplied — typically extracted
/// from the verifier's `PathQuery`).
///
/// On success returns `(merk_root_hash, sum)`:
/// - `merk_root_hash` is the root hash of the reconstructed merk; the
///   caller must compare it against the expected root hash to complete
///   verification.
/// - `sum` is the signed `i64` sum of keys' contributions in the inner
///   range, computed by replaying the prover's classification walk against
///   the reconstructed proof tree.
///
/// **Two-phase verification.** Same defensive structure as the count proof
/// verifier — allowlisting node types alone is unsound, so we both reject
/// blatantly wrong types up front and then run a structural shape walk that
/// binds each leaf's type to the (subtree_bounds × range) classification.
///
/// **Overflow handling.** The shape walk accumulates in `i128` (so two
/// `i64::MAX` children sum cleanly to `2 * i64::MAX` rather than wrapping)
/// and narrows to `i64` at the end. If the i128 result doesn't fit in i64,
/// the verifier returns `Error::InvalidProofError` — this is the safety net
/// against adversarial proofs that compose extremes into a sum that
/// can't be represented in the on-the-wire `i64` field.
///
/// **Empty merk case.** An empty merk is represented by an empty proof byte
/// stream and yields `(NULL_HASH, 0)`. Callers chaining this in a
/// multi-layer proof should recognize that shape explicitly.
pub fn verify_aggregate_sum_on_range_proof(
    proof_bytes: &[u8],
    inner_range: &QueryItem,
) -> CostResult<(CryptoHash, i64), Error> {
    if proof_bytes.is_empty() {
        // Empty merk → empty proof → sum = 0, hash = NULL_HASH.
        return Ok((NULL_HASH, 0i64)).wrap_with_cost(OperationCost::default());
    }

    let mut cost = OperationCost::default();
    let decoder = Decoder::new(proof_bytes);

    // Phase 1: reconstruct the proof tree. Allowlist the only two node types
    // the honest prover emits — `HashWithSum` (collapsed Disjoint/Contained
    // subtrees) and `KVDigestSum` (Boundary nodes). Plain `Hash(_)` is not
    // accepted: the structural sum it would carry must be hash-bound, and
    // only `HashWithSum` provides that.
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| match node {
            Node::HashWithSum(_, _, _, _) | Node::KVDigestSum(_, _, _) => Ok(()),
            other => Err(Error::InvalidProofError(format!(
                "unexpected node type in aggregate sum proof: {}",
                other
            ))),
        });
    let tree = cost_return_on_error!(&mut cost, tree_result);

    // Phase 2: shape-check + sum by replaying the prover's classification
    // walk. The accumulator is i128 so adversarial extremes don't wrap;
    // we narrow to i64 at the end below.
    let (sum_i128, _structural) = match verify_sum_shape(&tree, inner_range, None, None) {
        Ok(pair) => pair,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Final overflow gate: narrow the i128 accumulator to i64. A
    // well-formed `ProvableSumTree` maintains its aggregate as i64 at every
    // level, so an honest verify lands here with a value already inside
    // i64's range. Anything outside is a forgery or a tree that violates
    // its invariants.
    let sum: i64 = match i64::try_from(sum_i128) {
        Ok(v) => v,
        Err(_) => {
            return Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: in-range sum overflowed i64 ({})",
                sum_i128
            )))
            .wrap_with_cost(cost);
        }
    };

    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok((root_hash, sum)).wrap_with_cost(cost)
}

/// Recursive shape-walk over the reconstructed proof tree. Returns the
/// pair `(in_range_sum_i128, structural_sum_i128)`:
///
/// - `in_range_sum_i128` — signed sum of keys in the subtree that fall
///   inside the inner range AND have a non-zero own-sum (i.e. are not
///   `NotSummed`-wrapped). Accumulated in i128; narrowed to i64 once at
///   the outer entry point.
/// - `structural_sum_i128` — the merk-recorded aggregate sum of this
///   subtree (counting normal entries as their value and `NotSummed`
///   entries as 0). The parent uses it to compute its own `own_sum` as
///   `parent_node_sum − left_struct − right_struct` (since
///   `parent_node_sum = own + left_struct + right_struct`). Also kept in
///   i128 throughout.
///
/// The structural sum of every child is **cryptographically bound** to
/// the parent's hash chain because every sum-bearing node in a sum proof
/// (`KVDigestSum`, `HashWithSum`) has its sum fed into
/// `node_hash_with_sum` for hash recomputation. Plain `Hash(_)` would
/// not carry a bound sum and is therefore not allowed in sum proofs.
///
/// At each node we run the same type ↔ classification binding as the
/// count side:
///
/// - `Disjoint` → must be a leaf `HashWithSum`. Contributes 0 to
///   in_range_sum, full sum to structural_sum.
/// - `Contained` → must be a leaf `HashWithSum`. Contributes its sum to
///   both.
/// - `Boundary` → must be `KVDigestSum(key, ...)` with `key` strictly
///   inside `bounds`. Recurse left with `(lo, key)` and right with
///   `(key, hi)`; add `own_sum` if `inner_range.contains(key)`.
///
/// **Negative-sum caveat:** unlike count's `checked_sub` (where
/// `parent_aggregate < left_struct + right_struct` would indicate
/// corruption), the sum arithmetic is naturally signed and *cannot* be
/// detected by sign alone — a negative own_sum is perfectly legal. We
/// just compute `node_sum - left_struct - right_struct` in i128 and trust
/// the final overflow gate to catch any meaningful corruption (it's hash-
/// bound regardless, so a mismatch in own_sum's arithmetic would change
/// the reconstructed root hash and the caller's root check catches it).
fn verify_sum_shape(
    tree: &ProofTree,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<(i128, i128), Error> {
    let class = classify_subtree(lo, hi, range);
    match class {
        SubtreeClassification::Disjoint => match &tree.node {
            Node::HashWithSum(_, _, _, sum) => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "aggregate-sum proof: HashWithSum node at a Disjoint position \
                         must be a leaf"
                            .to_string(),
                    ));
                }
                // Disjoint subtree contributes 0 to the in-range sum but
                // its full structural sum to the parent's `own_sum`
                // computation.
                Ok((0i128, *sum as i128))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: expected HashWithSum at Disjoint position, got {}",
                other
            ))),
        },
        SubtreeClassification::Contained => match &tree.node {
            Node::HashWithSum(_, _, _, sum) => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "aggregate-sum proof: HashWithSum node at a Contained position \
                         must be a leaf"
                            .to_string(),
                    ));
                }
                // Contained subtree's structural sum (which excludes
                // NotSummed entries because their stored aggregate is 0)
                // is exactly its in-range sum.
                Ok((*sum as i128, *sum as i128))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: expected HashWithSum at Contained position, got {}",
                other
            ))),
        },
        SubtreeClassification::Boundary => match &tree.node {
            Node::KVDigestSum(key, _, aggregate) => {
                if !key_strictly_inside(key.as_slice(), lo, hi) {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-sum proof: KVDigestSum key {} falls outside its \
                         inherited subtree bounds (lo={:?}, hi={:?})",
                        hex::encode(key),
                        lo.map(hex::encode),
                        hi.map(hex::encode),
                    )));
                }
                let key_slice = key.as_slice();
                let (left_in, left_struct) = match &tree.left {
                    Some(child) => verify_sum_shape(&child.tree, range, lo, Some(key_slice))?,
                    None => (0i128, 0i128),
                };
                let (right_in, right_struct) = match &tree.right {
                    Some(child) => verify_sum_shape(&child.tree, range, Some(key_slice), hi)?,
                    None => (0i128, 0i128),
                };
                // own_sum = aggregate − left_struct − right_struct, in
                // i128. There's no "child sum exceeds parent" check that
                // makes sense for signed sums — any combination of
                // children's structural sums is plausible (one positive,
                // one negative, etc.). The hash chain binds the values
                // regardless, so any wrong arithmetic here would change
                // the reconstructed root hash.
                let aggregate_i128 = *aggregate as i128;
                let own_sum = aggregate_i128 - left_struct - right_struct;
                let self_contribution = if range.contains(key_slice) {
                    own_sum
                } else {
                    0
                };
                let in_range = left_in + right_in + self_contribution;
                Ok((in_range, aggregate_i128))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: expected KVDigestSum at Boundary position, got {}",
                other
            ))),
        },
    }
}
