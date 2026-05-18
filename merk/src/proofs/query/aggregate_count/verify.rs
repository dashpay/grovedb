//! Verifier for `AggregateCountOnRange` proofs.
//!
//! Two-phase structure:
//!
//! 1. **Phase 1** — replay the prover's op stream through
//!    `execute_with_options`, allowlisting the two node types the honest
//!    prover ever emits (`HashWithCount` for collapsed Disjoint/Contained
//!    subtrees, `KVDigestCount` for boundary nodes). Plain `Hash(_)` is
//!    no longer used here because the structural count it would stand
//!    in for is needed by the verifier's `own_count` derivation and
//!    would not be hash-bound.
//!
//! 2. **Phase 2** — walk the reconstructed tree and re-derive the
//!    in-range count, asserting that each node's type matches the
//!    classification its inherited bounds imply. This is the
//!    type-shape binding that makes the proof non-malleable.

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

/// Verify a count-only proof for an `AggregateCountOnRange` query.
///
/// `proof_bytes` is the encoded `Vec<Op>` produced by
/// [`crate::Merk::prove_aggregate_count_on_range`]; `inner_range` is the same
/// `QueryItem` the prover counted over (caller-supplied — typically extracted
/// from the verifier's `PathQuery`).
///
/// On success returns `(merk_root_hash, count)`:
/// - `merk_root_hash` is the root hash of the reconstructed merk; the
///   caller must compare it against the expected root hash to complete
///   verification.
/// - `count` is the number of keys in the inner range, computed by replaying
///   the prover's classification walk against the reconstructed proof tree.
///
/// **Two-phase verification.** Allowlisting node types alone is unsound:
/// a malicious prover can substitute `Hash` for an in-range subtree (to
/// undercount), attach extra `KVDigestCount` children below a keyless
/// `Hash` / `HashWithCount` (to overcount, since their hash recomputation
/// ignores attached children and the root hash would still match), or send
/// a single `Push(Hash(expected_root))` for a non-empty tree (to receive a
/// count of 0 with the trusted root). To prevent all three, this function:
///
/// 1. Decodes the proof into a `ProofTree` via `execute_with_options` with
///    the AVL balance check disabled (count proofs intentionally collapse
///    one side to height 1) and **does not** count anything in the
///    `visit_node` callback.
/// 2. Walks the reconstructed tree with the same inherited exclusive
///    subtree-key bounds the prover used (`(None, None)` at the root).
///    At each position it calls `classify_subtree(bounds, inner_range)` and
///    requires the proof-tree node type to match the classification:
///    - `Disjoint` → must be a leaf `HashWithCount(_)`. Contributes 0.
///    - `Contained` → must be a leaf `HashWithCount(...)`. Contributes its
///      count.
///    - `Boundary` → must be `KVDigestCount(key, ...)` with `key` strictly
///      inside `bounds`. Recurse left with `(lo, key)` and right with
///      `(key, hi)`; add 1 if `inner_range.contains(key)`.
///
/// Counts are summed with `checked_add`; an overflow is treated as proof
/// corruption (`u64::MAX` keys is not a real merk shape). The caller is
/// still responsible for verifying the returned `merk_root_hash` against
/// their trusted root.
///
/// **Empty merk case.** An empty merk is represented by an empty proof byte
/// stream and yields `(NULL_HASH, 0)`. Callers chaining this in a
/// multi-layer proof should recognize that shape explicitly.
pub fn verify_aggregate_count_on_range_proof(
    proof_bytes: &[u8],
    inner_range: &QueryItem,
) -> CostResult<(CryptoHash, u64), Error> {
    if proof_bytes.is_empty() {
        // Empty merk → empty proof → count = 0, hash = NULL_HASH. This
        // matches the prover-side behavior of returning an empty op stream
        // for an empty subtree.
        return Ok((NULL_HASH, 0u64)).wrap_with_cost(OperationCost::default());
    }

    let mut cost = OperationCost::default();
    let decoder = Decoder::new(proof_bytes);

    // Phase 1: reconstruct the proof tree. The visit_node closure only
    // performs a coarse allowlist; the per-position type/shape check happens
    // in Phase 2 below. We still reject blatantly wrong node types here so
    // execute() bails early on garbage input.
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| match node {
            // The count proof emits four node types:
            // - For single-axis count-bearing host trees (ProvableCountTree,
            //   ProvableCountSumTree): `HashWithCount` (for collapsed
            //   Disjoint/Contained subtrees) and `KVDigestCount` (for
            //   Boundary nodes).
            // - For the dual-axis ProvableCountProvableSumTree:
            //   `HashWithCountAndSum` and `KVDigestCountSum`. Both axes are
            //   needed because the host tree's node hash is
            //   `node_hash_with_count_and_sum`; without the sum the
            //   verifier can't reconstruct it.
            //
            // Plain `Hash(_)` is never allowed here because the structural
            // count it would otherwise stand in for is needed by the
            // verifier's `own_count` derivation and would not be hash-bound.
            Node::HashWithCount(_, _, _, _)
            | Node::KVDigestCount(_, _, _)
            | Node::HashWithCountAndSum(_, _, _, _, _)
            | Node::KVDigestCountSum(_, _, _, _) => Ok(()),
            other => Err(Error::InvalidProofError(format!(
                "unexpected node type in aggregate count proof: {}",
                other
            ))),
        });
    let tree = cost_return_on_error!(&mut cost, tree_result);

    // Phase 2: shape-check + count by replaying the prover's classification
    // walk. This binds each leaf node's type to the (subtree_bounds × range)
    // classification, so the only valid count is the one a faithful prover
    // would have produced for this exact range.
    let (count, _structural) = match verify_count_shape(&tree, inner_range, None, None) {
        Ok(pair) => pair,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok((root_hash, count)).wrap_with_cost(cost)
}

/// Recursive shape-walk over the reconstructed proof tree. Returns the
/// pair `(in_range_count, structural_count)`:
///
/// - `in_range_count` — number of keys in the subtree that fall inside the
///   inner range AND have a non-zero own-count (i.e. are not
///   `NonCounted`-wrapped). This is what bubbles up to the verifier's
///   return value.
/// - `structural_count` — the merk-recorded aggregate count of this subtree
///   (counting normal entries as 1 and `NonCounted` entries as 0). The
///   parent uses it to compute its own `own_count` as
///   `parent_node_count − left_struct − right_struct` (since
///   `parent_node_count = own + left_struct + right_struct`).
///
/// The structural count of every child is **cryptographically bound** to
/// the parent's hash chain because every count-bearing node in a count
/// proof (`KVDigestCount`, `HashWithCount`) has its count fed into
/// `node_hash_with_count` for hash recomputation. Plain `Hash(_)` would
/// not carry a bound count and is therefore not allowed in count proofs;
/// see the prover-side comment in `emit_count_proof` for the full
/// justification.
///
/// At each node:
///
/// - Compute the expected classification from the inherited subtree bounds
///   and the inner range.
/// - Require the node's type to match the classification (and reject any
///   children attached under a leaf-shape classification — a malicious
///   prover could otherwise hide counted children under a `HashWithCount`
///   leaf, since its hash recomputation ignores reconstructed children).
/// - Recurse with tightened bounds at `Boundary` nodes, summing with
///   `checked_add` and computing `own_count` via `checked_sub`.
fn verify_count_shape(
    tree: &ProofTree,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<(u64, u64), Error> {
    let class = classify_subtree(lo, hi, range);
    match class {
        SubtreeClassification::Disjoint => {
            // Disjoint subtree contributes 0 to the in-range count but its
            // full structural count to the parent's `own_count` computation.
            // Both single-axis (`HashWithCount`) and dual-axis
            // (`HashWithCountAndSum`) variants are accepted — they carry
            // the same count field, just with the sum additionally bound
            // into the hash for ProvableCountProvableSumTree hosts.
            let count = match &tree.node {
                Node::HashWithCount(_, _, _, count) => *count,
                Node::HashWithCountAndSum(_, _, _, count, _) => *count,
                other => {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count proof: expected HashWithCount or HashWithCountAndSum \
                         at Disjoint position, got {}",
                        other
                    )));
                }
            };
            if tree.left.is_some() || tree.right.is_some() {
                return Err(Error::InvalidProofError(
                    "aggregate-count proof: leaf hash-with-count node at a Disjoint position \
                     must be a leaf"
                        .to_string(),
                ));
            }
            Ok((0, count))
        }
        SubtreeClassification::Contained => {
            // Contained subtree's structural count (which excludes
            // NonCounted entries because their stored aggregate is 0) is
            // exactly its in-range count. Accept both single- and
            // dual-axis variants.
            let count = match &tree.node {
                Node::HashWithCount(_, _, _, count) => *count,
                Node::HashWithCountAndSum(_, _, _, count, _) => *count,
                other => {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count proof: expected HashWithCount or HashWithCountAndSum \
                         at Contained position, got {}",
                        other
                    )));
                }
            };
            if tree.left.is_some() || tree.right.is_some() {
                return Err(Error::InvalidProofError(
                    "aggregate-count proof: leaf hash-with-count node at a Contained position \
                     must be a leaf"
                        .to_string(),
                ));
            }
            Ok((count, count))
        }
        SubtreeClassification::Boundary => {
            // Boundary nodes: accept KVDigestCount (single-axis) or
            // KVDigestCountSum (dual-axis). Both carry the count we need;
            // the sum field in KVDigestCountSum is only used during hash
            // reconstruction (already done by `execute_with_options`'s
            // node-hash recomputation in Phase 1).
            let (key, aggregate) = match &tree.node {
                Node::KVDigestCount(key, _, aggregate) => (key, *aggregate),
                Node::KVDigestCountSum(key, _, aggregate, _) => (key, *aggregate),
                other => {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count proof: expected KVDigestCount or KVDigestCountSum at \
                         Boundary position, got {}",
                        other
                    )));
                }
            };
            if !key_strictly_inside(key.as_slice(), lo, hi) {
                return Err(Error::InvalidProofError(format!(
                    "aggregate-count proof: boundary key {} falls outside its inherited \
                     subtree bounds (lo={:?}, hi={:?})",
                    hex::encode(key),
                    lo.map(hex::encode),
                    hi.map(hex::encode),
                )));
            }
            let key_slice = key.as_slice();
            let (left_in, left_struct) = match &tree.left {
                Some(child) => verify_count_shape(&child.tree, range, lo, Some(key_slice))?,
                None => (0, 0),
            };
            let (right_in, right_struct) = match &tree.right {
                Some(child) => verify_count_shape(&child.tree, range, Some(key_slice), hi)?,
                None => (0, 0),
            };
            // own_count = aggregate − left_struct − right_struct.
            // Saturating sub here would silently mask a malformed proof
            // (children claiming more keys than the parent's aggregate),
            // so use checked_sub and reject.
            let own_count = aggregate
                .checked_sub(left_struct)
                .and_then(|s| s.checked_sub(right_struct))
                .ok_or_else(|| {
                    Error::InvalidProofError(format!(
                        "aggregate-count proof: child structural counts ({} + {}) exceed \
                         parent's aggregate count ({}) at key {}",
                        left_struct,
                        right_struct,
                        aggregate,
                        hex::encode(key)
                    ))
                })?;
            let self_contribution = if range.contains(key_slice) {
                own_count
            } else {
                0
            };
            let in_range = left_in
                .checked_add(right_in)
                .and_then(|s| s.checked_add(self_contribution))
                .ok_or_else(|| {
                    Error::InvalidProofError(
                        "aggregate-count proof: in-range count overflowed u64".to_string(),
                    )
                })?;
            Ok((in_range, aggregate))
        }
    }
}
