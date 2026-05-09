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

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use crate::{
    proofs::{
        query::QueryItem,
        tree::{execute_with_options, Tree as ProofTree},
        Decoder, Node, Op,
    },
    tree::{kv::ValueDefinedCostType, AggregateData, Fetch, RefWalker},
    CryptoHash, Error, TreeType,
};

/// All-zero `CryptoHash`, used in `Node::HashWithCount` for missing children.
const NULL_HASH: CryptoHash = [0u8; 32];

/// How a subtree's possible-key window relates to the inner range we're
/// counting over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubtreeClassification {
    /// Every possible key in this subtree falls **outside** the range.
    Disjoint,
    /// Every possible key in this subtree falls **inside** the range.
    Contained,
    /// The subtree straddles a range boundary (or directly contains one).
    Boundary,
}

/// Classify a subtree relative to the inner range.
///
/// `subtree_lo_excl` and `subtree_hi_excl` are the **exclusive** bounds on
/// what keys can appear under the subtree (derived from ancestors during the
/// walk; both `None` at the root). The range bounds come from the inner
/// `QueryItem`'s `lower_bound` / `upper_bound`.
///
/// The comparisons treat `subtree_hi_excl` as exclusive (subtree keys are
/// strictly < `subtree_hi_excl`) and `subtree_lo_excl` as exclusive (subtree
/// keys are strictly > `subtree_lo_excl`). For the range bounds, the
/// inclusivity flag returned by `lower_bound`/`upper_bound` is **not**
/// load-bearing for the disjoint/contained tests below — see the inline
/// proofs.
fn classify_subtree(
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    range: &QueryItem,
) -> SubtreeClassification {
    let (range_lo, _range_lo_excl) = range.lower_bound();
    let (range_hi, _range_hi_incl) = range.upper_bound();

    // Disjoint-LEFT: subtree entirely below the range.
    //
    // Subtree keys are < subtree_hi_excl. If subtree_hi_excl <= range_lo,
    // every subtree key < subtree_hi_excl <= range_lo is also < range_lo,
    // so excluded regardless of whether range_lo is inclusive or exclusive.
    if let (Some(s_hi), Some(r_lo)) = (subtree_hi_excl, range_lo)
        && s_hi <= r_lo
    {
        return SubtreeClassification::Disjoint;
    }

    // Disjoint-RIGHT: subtree entirely above the range.
    //
    // Subtree keys are > subtree_lo_excl. If subtree_lo_excl >= range_hi,
    // every subtree key > subtree_lo_excl >= range_hi is also > range_hi,
    // so excluded regardless of whether range_hi is inclusive or exclusive.
    if let (Some(s_lo), Some(r_hi)) = (subtree_lo_excl, range_hi)
        && s_lo >= r_hi
    {
        return SubtreeClassification::Disjoint;
    }

    // Contained: subtree (s_lo, s_hi) ⊆ range.
    //
    // Lower side: every subtree key > s_lo. If s_lo >= r_lo, every subtree
    // key > s_lo >= r_lo, so > r_lo, satisfying both inclusive and exclusive
    // r_lo. If subtree has no lower bound (s_lo = -inf) but range does, the
    // subtree could include arbitrarily small keys → not contained.
    let lower_contained = match range_lo {
        None => true,
        Some(r_lo) => match subtree_lo_excl {
            Some(s_lo) => s_lo >= r_lo,
            None => false,
        },
    };
    // Upper side: every subtree key < s_hi. If s_hi <= r_hi, every subtree
    // key < s_hi <= r_hi, so < r_hi, satisfying both inclusive and exclusive
    // r_hi. (We forgo the slightly tighter "s_hi <= r_hi+1" optimization for
    // inclusive r_hi because we don't have key arithmetic.)
    let upper_contained = match range_hi {
        None => true,
        Some(r_hi) => match subtree_hi_excl {
            Some(s_hi) => s_hi <= r_hi,
            None => false,
        },
    };

    if lower_contained && upper_contained {
        SubtreeClassification::Contained
    } else {
        SubtreeClassification::Boundary
    }
}

/// Returns true if `tree_type` is one of the four tree types that can host an
/// `AggregateCountOnRange` proof. Wrapper types are accepted by stripping
/// down to the inner tree type via `is_provable_count_bearing`.
fn is_provable_count_bearing(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableCountTree | TreeType::ProvableCountSumTree
    )
}

/// Pull the count out of a `ProvableCount` / `ProvableCountAndSum` aggregate.
/// Returns `Err(InvalidProofError)` for any other variant — the entry point
/// has already gated `tree_type`, so reaching the error means the tree's
/// in-memory state disagrees with its declared type.
fn provable_count_from_aggregate(data: AggregateData) -> Result<u64, Error> {
    match data {
        AggregateData::ProvableCount(c) => Ok(c),
        AggregateData::ProvableCountAndSum(c, _) => Ok(c),
        other => Err(Error::InvalidProofError(format!(
            "expected ProvableCount aggregate data on a provable count tree, got {:?}",
            other
        ))),
    }
}

impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    /// Generate a count-only proof for an `AggregateCountOnRange` query.
    ///
    /// `inner_range` is the `QueryItem` wrapped by `AggregateCountOnRange`
    /// (already stripped at the caller). `tree_type` must be one of
    /// `ProvableCountTree` or `ProvableCountSumTree`; any other tree type is
    /// rejected with `Error::InvalidProofError` before any walking happens.
    ///
    /// The returned tuple is `(proof_ops, count)`:
    /// - `proof_ops` is the linear stream the verifier will replay to
    ///   reconstruct the tree's root hash.
    /// - `count` is the prover-side computed count (the verifier independently
    ///   recomputes it from the proof and compares against the expected root
    ///   hash; this value is returned as a convenience, not as ground truth).
    pub fn create_aggregate_count_on_range_proof(
        &mut self,
        inner_range: &QueryItem,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(LinkedList<Op>, u64), Error> {
        if !is_provable_count_bearing(tree_type) {
            return Err(Error::InvalidProofError(format!(
                "AggregateCountOnRange is only valid against ProvableCountTree or \
                 ProvableCountSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(OperationCost::default());
        }

        let mut cost = OperationCost::default();
        let mut ops = LinkedList::new();
        let count = cost_return_on_error!(
            &mut cost,
            emit_count_proof(
                self,
                inner_range,
                tree_type,
                None,
                None,
                &mut ops,
                grove_version
            )
        );
        Ok((ops, count)).wrap_with_cost(cost)
    }
}

/// Recursive proof emitter. Always called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both `None`
/// at the root call).
fn emit_count_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    tree_type: TreeType,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    ops: &mut LinkedList<Op>,
    grove_version: &GroveVersion,
) -> CostResult<u64, Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    // Step 1: classify the current subtree against the inner range.
    let class = classify_subtree(subtree_lo_excl, subtree_hi_excl, range);

    match class {
        SubtreeClassification::Disjoint => {
            // Whole subtree is outside the range: emit one opaque hash.
            let node_hash = walker
                .tree()
                .hash_for_link(tree_type)
                .unwrap_add_cost(&mut cost);
            ops.push_back(Op::Push(Node::Hash(node_hash)));
            return Ok(0).wrap_with_cost(cost);
        }
        SubtreeClassification::Contained => {
            // Whole subtree is inside the range: emit one HashWithCount
            // carrying enough material to reconstruct the subtree's
            // node_hash from `(kv_hash, left_child_hash, right_child_hash,
            // count)`. The verifier recomputes
            // node_hash_with_count(...) and uses that as the subtree's
            // committed hash; if the prover's `count` is wrong the recomputed
            // hash diverges and the parent's Merkle-root check fails.
            let aggregate = match walker.tree().aggregate_data() {
                Ok(a) => a,
                Err(e) => {
                    return Err(Error::InvalidProofError(format!("aggregate_data: {}", e)))
                        .wrap_with_cost(cost);
                }
            };
            let subtree_count = match provable_count_from_aggregate(aggregate) {
                Ok(c) => c,
                Err(e) => return Err(e).wrap_with_cost(cost),
            };
            let kv_hash = *walker.tree().kv_hash();
            let left_child_hash = walker
                .tree()
                .link(true)
                .map(|l| *l.hash())
                .unwrap_or(NULL_HASH);
            let right_child_hash = walker
                .tree()
                .link(false)
                .map(|l| *l.hash())
                .unwrap_or(NULL_HASH);
            ops.push_back(Op::Push(Node::HashWithCount(
                kv_hash,
                left_child_hash,
                right_child_hash,
                subtree_count,
            )));
            return Ok(subtree_count).wrap_with_cost(cost);
        }
        SubtreeClassification::Boundary => {
            // Boundary case: descend, emit the current node as KVDigestCount,
            // and recurse into both children.
        }
    }

    // Step 2: snapshot what we need from the current node before walking.
    // walk(true/false) takes &mut self.tree, so we must drop any existing
    // borrows on walker.tree() before calling it.
    let node_key: Vec<u8> = walker.tree().key().to_vec();
    let node_value_hash: CryptoHash = *walker.tree().value_hash();
    let node_count: u64 = match walker
        .tree()
        .aggregate_data()
        .map_err(|e| Error::InvalidProofError(format!("aggregate_data: {}", e)))
    {
        Ok(data) => match provable_count_from_aggregate(data) {
            Ok(c) => c,
            Err(e) => return Err(e).wrap_with_cost(cost),
        },
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Snapshot link presence + hash so we can short-circuit fully-outside
    // children without paying the I/O cost of walk(). A Contained child
    // still requires a walk because the new `HashWithCount` shape needs the
    // child's `kv_hash` and grandchild hashes — material the parent's link
    // doesn't carry. The recursive call's own Contained arm will emit the
    // HashWithCount in a single op.
    let (left_link_present, left_link_hash): (bool, CryptoHash) = match walker.tree().link(true) {
        Some(link) => (true, *link.hash()),
        None => (false, NULL_HASH),
    };
    let (right_link_present, right_link_hash): (bool, CryptoHash) = match walker.tree().link(false)
    {
        Some(link) => (true, *link.hash()),
        None => (false, NULL_HASH),
    };

    let mut total: u64 = 0;

    // Step 3: handle the LEFT child.
    let left_emitted = if left_link_present {
        let left_lo = subtree_lo_excl;
        let left_hi: Option<&[u8]> = Some(node_key.as_slice());
        let left_class = classify_subtree(left_lo, left_hi, range);
        match left_class {
            SubtreeClassification::Disjoint => {
                ops.push_back(Op::Push(Node::Hash(left_link_hash)));
                true
            }
            SubtreeClassification::Contained | SubtreeClassification::Boundary => {
                let walked = cost_return_on_error!(
                    &mut cost,
                    walker.walk(
                        true,
                        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                        grove_version,
                    )
                );
                let mut left_walker = match walked {
                    Some(lw) => lw,
                    None => {
                        return Err(Error::CorruptedState(
                            "tree.link(true) was Some but walk(true) returned None",
                        ))
                        .wrap_with_cost(cost)
                    }
                };
                let n = cost_return_on_error!(
                    &mut cost,
                    emit_count_proof(
                        &mut left_walker,
                        range,
                        tree_type,
                        left_lo,
                        left_hi,
                        ops,
                        grove_version,
                    )
                );
                total = total.saturating_add(n);
                true
            }
        }
    } else {
        false
    };

    // Step 4: emit the current node as a boundary KVDigestCount + attach left
    // as its left child.
    ops.push_back(Op::Push(Node::KVDigestCount(
        node_key.clone(),
        node_value_hash,
        node_count,
    )));
    if left_emitted {
        ops.push_back(Op::Parent);
    }
    if range.contains(&node_key) {
        total = total.saturating_add(1);
    }

    // Step 5: handle the RIGHT child. Same pattern as LEFT — only Disjoint
    // is short-circuited at the link level; Contained walks one level into
    // the child so the recursive Contained arm can emit a self-verifying
    // HashWithCount with the child's own kv_hash and grandchild hashes.
    let right_emitted = if right_link_present {
        let right_lo: Option<&[u8]> = Some(node_key.as_slice());
        let right_hi = subtree_hi_excl;
        let right_class = classify_subtree(right_lo, right_hi, range);
        match right_class {
            SubtreeClassification::Disjoint => {
                ops.push_back(Op::Push(Node::Hash(right_link_hash)));
                true
            }
            SubtreeClassification::Contained | SubtreeClassification::Boundary => {
                let walked = cost_return_on_error!(
                    &mut cost,
                    walker.walk(
                        false,
                        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                        grove_version,
                    )
                );
                let mut right_walker = match walked {
                    Some(rw) => rw,
                    None => {
                        return Err(Error::CorruptedState(
                            "tree.link(false) was Some but walk(false) returned None",
                        ))
                        .wrap_with_cost(cost)
                    }
                };
                let n = cost_return_on_error!(
                    &mut cost,
                    emit_count_proof(
                        &mut right_walker,
                        range,
                        tree_type,
                        right_lo,
                        right_hi,
                        ops,
                        grove_version,
                    )
                );
                total = total.saturating_add(n);
                true
            }
        }
    } else {
        false
    };

    if right_emitted {
        ops.push_back(Op::Child);
    }

    Ok(total).wrap_with_cost(cost)
}

/// Verify a count-only proof for an `AggregateCountOnRange` query.
///
/// `proof_bytes` is the encoded `Vec<Op>` produced by
/// [`Merk::prove_aggregate_count_on_range`]; `inner_range` is the same
/// `QueryItem` the prover counted over (caller-supplied — typically extracted
/// from the verifier's `PathQuery`).
///
/// On success returns `(merk_root_hash, count)`:
/// - `merk_root_hash` is the root hash of the reconstructed merk; the
///   caller must compare it against the expected root hash to complete
///   verification.
/// - `count` is the number of keys in the inner range, accumulated from
///   the proof's `HashWithCount` and in-range `KVDigestCount` nodes.
///
/// The function rejects:
/// - empty proof bytes (treated as count = 0 only when accompanied by a
///   trivial empty-tree marker — see below);
/// - any proof node whose type is not legal for this proof shape
///   (`Hash`, `HashWithCount`, `KVDigestCount` — plus the structural
///   `Parent` / `Child` ops, which `execute` consumes implicitly);
/// - a proof that decodes to multiple roots or zero roots (handled by
///   `execute`'s usual error path);
/// - trailing bytes after the proof's last op (likely-malicious input).
///
/// Note on the "empty merk" case: an empty merk is represented by an empty
/// proof byte stream and yields `(NULL_HASH, 0)`. Callers chaining this in
/// a multi-layer proof should recognize that shape explicitly.
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
    let mut count: u64 = 0;
    let decoder = Decoder::new(proof_bytes);

    // execute propagates the visit_node Err directly through its CostResult,
    // so the only allowlist enforcement we need lives inside the closure.
    // We disable the AVL balance check (`verify_avl_balance = false`) because
    // count proofs intentionally collapse fully-inside subtrees into a single
    // op, producing a reconstructed tree whose child heights routinely differ
    // by more than one.
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| {
            // Only the three node types listed below are allowed in an aggregate
            // count proof. Anything else (KV, KVValueHash, KVHash, etc.) is
            // treated as proof corruption — the prover should never emit them in
            // this mode.
            match node {
                Node::Hash(_) => Ok(()),
                Node::HashWithCount(_, _, _, c) => {
                    count = count.saturating_add(*c);
                    Ok(())
                }
                Node::KVDigestCount(key, _, _) => {
                    if inner_range.contains(key.as_slice()) {
                        count = count.saturating_add(1);
                    }
                    Ok(())
                }
                other => Err(Error::InvalidProofError(format!(
                    "unexpected node type in aggregate count proof: {}",
                    other
                ))),
            }
        });

    let tree = cost_return_on_error!(&mut cost, tree_result);
    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok((root_hash, count)).wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range_inclusive(lo: &[u8], hi: &[u8]) -> QueryItem {
        QueryItem::RangeInclusive(lo.to_vec()..=hi.to_vec())
    }

    fn range_full() -> QueryItem {
        QueryItem::RangeFull(std::ops::RangeFull)
    }

    fn range_from(lo: &[u8]) -> QueryItem {
        QueryItem::RangeFrom(lo.to_vec()..)
    }

    fn range_after(lo: &[u8]) -> QueryItem {
        QueryItem::RangeAfter(lo.to_vec()..)
    }

    #[test]
    fn classify_disjoint_below() {
        let r = range_inclusive(b"d", b"f");
        // subtree (None, b"c") — keys < "c", entirely below ["d", "f"].
        assert_eq!(
            classify_subtree(None, Some(b"c"), &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_disjoint_above() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"g", None) — keys > "g", entirely above ["d", "f"].
        assert_eq!(
            classify_subtree(Some(b"g"), None, &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_disjoint_at_lower_boundary_inclusive() {
        let r = range_inclusive(b"d", b"f");
        // subtree (None, b"d") — keys < "d", just below the inclusive bound.
        assert_eq!(
            classify_subtree(None, Some(b"d"), &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_disjoint_at_upper_boundary_inclusive() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"f", None) — keys > "f", just above the inclusive bound.
        assert_eq!(
            classify_subtree(Some(b"f"), None, &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_contained_simple() {
        let r = range_inclusive(b"a", b"z");
        // subtree (b"d", b"f") — keys in ("d", "f"), all in ["a", "z"].
        assert_eq!(
            classify_subtree(Some(b"d"), Some(b"f"), &r),
            SubtreeClassification::Contained,
        );
    }

    #[test]
    fn classify_contained_full_range_full_subtree() {
        let r = range_full();
        // The full range matches everything — even an unbounded subtree is
        // contained.
        assert_eq!(
            classify_subtree(None, None, &r),
            SubtreeClassification::Contained,
        );
    }

    #[test]
    fn classify_boundary_overlapping_lower() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"c", b"e") — keys in ("c", "e"), straddles the lower bound.
        assert_eq!(
            classify_subtree(Some(b"c"), Some(b"e"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_boundary_overlapping_upper() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"e", b"g") — keys in ("e", "g"), straddles the upper bound.
        assert_eq!(
            classify_subtree(Some(b"e"), Some(b"g"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_boundary_unbounded_below_with_bounded_range() {
        let r = range_from(b"d");
        // subtree (None, b"e") — could include keys < "d", so boundary.
        assert_eq!(
            classify_subtree(None, Some(b"e"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_contained_range_after_exclusive() {
        let r = range_after(b"b");
        // RangeAfter(b"b") = (b, +inf). subtree (b"b", b"e") — keys > "b" and
        // < "e", all in (b, +inf). Contained.
        assert_eq!(
            classify_subtree(Some(b"b"), Some(b"e"), &r),
            SubtreeClassification::Contained,
        );
    }

    // ---------- end-to-end integration tests on a real merk ----------
    //
    // These tests build a small ProvableCountTree, generate count proofs
    // through the merk-level API, then verify them with the count verifier.
    // They cover the four documented categories: open-range (lower-only and
    // upper-only) and closed-range (inclusive and after-to-inclusive). Empty
    // tree and single-bound edge cases are also exercised.

    use grovedb_costs::CostsExt as _;
    use grovedb_version::version::GroveVersion;

    use crate::{
        proofs::{encode_into, Op as ProofOp},
        test_utils::TempMerk,
        tree::{Op, TreeFeatureType::ProvableCountedMerkNode},
        Merk, TreeType,
    };

    /// Build a fresh `ProvableCountTree` populated with single-byte keys
    /// "a".."o" (15 keys) — same shape as the running example in the book
    /// chapter's "Closed ranges" section. Returns the merk and its current
    /// root hash.
    fn make_15_key_provable_count_tree(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountTree);
        let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
        let entries: Vec<(Vec<u8>, Op)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                (
                    k.clone(),
                    Op::Put(vec![i as u8], ProvableCountedMerkNode(1)),
                )
            })
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
            .unwrap()
            .expect("apply should succeed");
        merk.commit(grove_version);
        let root_hash = merk.root_hash().unwrap();
        (merk, root_hash)
    }

    /// Encode a `LinkedList<Op>` into the wire format that the verifier
    /// consumes.
    fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        encode_into(ops.iter(), &mut bytes);
        bytes
    }

    /// Round-trip helper: prove the inner range, encode the proof, verify it,
    /// assert the recovered root hash matches and the recovered count matches
    /// `expected_count`.
    fn round_trip(
        merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
        expected_root: [u8; 32],
        inner_range: QueryItem,
        expected_count: u64,
        grove_version: &GroveVersion,
    ) {
        let (ops, prover_count) = merk
            .prove_aggregate_count_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("prove should succeed");
        assert_eq!(
            prover_count, expected_count,
            "prover count mismatch for range {:?}",
            inner_range
        );
        let bytes = encode_proof(&ops);
        let (root, verifier_count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify should succeed");
        assert_eq!(
            root, expected_root,
            "verifier reconstructed wrong root for range {:?}",
            inner_range
        );
        assert_eq!(
            verifier_count, expected_count,
            "verifier count mismatch for range {:?}",
            inner_range
        );
    }

    #[test]
    fn integration_open_range_from() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeFrom("c"..) → keys c..o (13 keys).
        round_trip(&merk, root, QueryItem::RangeFrom(b"c".to_vec()..), 13, v);
    }

    #[test]
    fn integration_open_range_after() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeAfter(("b", ..)) → keys c..o (13 keys), same set as RangeFrom("c"..)
        // but proof shape differs — the boundary lands on "b" exclusive.
        round_trip(&merk, root, QueryItem::RangeAfter(b"b".to_vec()..), 13, v);
    }

    #[test]
    fn integration_open_range_to() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeTo(..b"e") → keys a..d (4 keys, exclusive upper).
        round_trip(&merk, root, QueryItem::RangeTo(..b"e".to_vec()), 4, v);
    }

    #[test]
    fn integration_open_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeToInclusive(..=b"e") → keys a..e (5 keys, inclusive upper).
        round_trip(
            &merk,
            root,
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            5,
            v,
        );
    }

    #[test]
    fn integration_closed_range_inclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeInclusive("c"..="l") → 10 keys.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn integration_closed_range_exclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // Range("c".."l") → c..k (9 keys, exclusive upper).
        round_trip(
            &merk,
            root,
            QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn integration_closed_range_after_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeAfterToInclusive(("c", "l")) → keys d..l (9 keys: d..=l excluding c).
        round_trip(
            &merk,
            root,
            QueryItem::RangeAfterToInclusive(b"c".to_vec()..=b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn integration_closed_range_after_to_exclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeAfterTo(("c", "l")) → keys d..l (8 keys, both exclusive).
        round_trip(
            &merk,
            root,
            QueryItem::RangeAfterTo(b"c".to_vec()..b"l".to_vec()),
            8,
            v,
        );
    }

    #[test]
    fn integration_range_below_all_keys() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // Entire range below the smallest key — should produce count = 0
        // and a Disjoint proof at the root level.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn integration_range_above_all_keys() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // Entire range above the largest key.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(b"z".to_vec()..=vec![0xff]),
            0,
            v,
        );
    }

    #[test]
    fn integration_empty_merk() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
        let (ops, prover_count) = merk
            .prove_aggregate_count_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap()
            .expect("prove on empty merk should succeed");
        assert_eq!(prover_count, 0);
        // Empty proof means the verifier returns NULL_HASH and count = 0.
        let bytes = encode_proof(&ops);
        let (root, verifier_count) = verify_aggregate_count_on_range_proof(
            &bytes,
            &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )
        .unwrap()
        .expect("verify on empty merk should succeed");
        assert_eq!(root, NULL_HASH);
        assert_eq!(verifier_count, 0);
    }

    #[test]
    fn integration_rejected_on_normal_tree() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new(v); // NormalTree
        let err = merk
            .prove_aggregate_count_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            err.is_err(),
            "expected an InvalidProofError on NormalTree, got Ok({:?})",
            err.ok().map(|(_, c)| c)
        );
    }

    #[test]
    fn integration_count_forgery_is_rejected() {
        // Demonstrates the cryptographic binding: tamper with the count in a
        // HashWithCount op and the verifier's root-hash recomputation must
        // diverge from the expected root.
        let v = GroveVersion::latest();
        let (merk, expected_root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (mut ops, _prover_count) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove should succeed");

        // Forge: bump the count on the first HashWithCount op we see.
        let mut tampered = false;
        for op in ops.iter_mut() {
            if let ProofOp::Push(Node::HashWithCount(_, _, _, count))
            | ProofOp::PushInverted(Node::HashWithCount(_, _, _, count)) = op
            {
                *count = count.saturating_add(1);
                tampered = true;
                break;
            }
        }
        assert!(
            tampered,
            "test setup: expected at least one HashWithCount op"
        );

        let bytes = encode_proof(&ops);
        let (root, _count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify should still complete (root mismatch is the caller's job)");
        assert_ne!(
            root, expected_root,
            "tampered count must produce a different reconstructed root hash"
        );
    }
}
