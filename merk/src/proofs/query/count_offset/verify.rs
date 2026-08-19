//! Verifier for offset-paginated count-tree range proofs.
//!
//! Same two-phase structure as [`super::super::aggregate_count::verify`]:
//!
//! 1. **Phase 1** — replay the prover's op stream through
//!    `execute_with_options` to rebuild the proof tree. The AVL balance
//!    check is disabled because offset proofs intentionally collapse
//!    one side to height 1 (a `HashWithCount` leaf can stand in for an
//!    arbitrarily tall subtree), and the `visit_node` callback only
//!    allowlists the node kinds an honest prover ever emits.
//!
//! 2. **Phase 2** — walk the reconstructed tree with the same
//!    classification + bound-tightening pattern the prover used, and
//!    independently re-derive:
//!    - `skipped` — number of in-range items the prover claims to have
//!      skipped via offset. Must equal the requested offset (or be ≤
//!      it iff the in-range population was smaller, see "Truncated
//!      offset" below).
//!    - `returned_items` — the actual values the verifier reconstructs
//!      from value-bearing nodes inside the limit window.
//!
//! ## Why we don't trust the prover's offset accounting
//!
//! A malicious prover could emit a `HashWithCount(count)` that
//! over-claims the skipped count (to hide an item from results) or
//! under-claims it (to leak an item that should have been past offset).
//! Both are caught because:
//!
//! - The count is fed into `node_hash_with_count` for hash
//!   reconstruction. A wrong count produces a wrong reconstructed root
//!   hash, which the caller compares against the trusted root and
//!   rejects.
//! - The verifier sums the structural counts of every collapsed
//!   subtree it visits and compares against the parent's
//!   aggregate-derived `own_count`. Mismatches surface as
//!   `InvalidProofError`.
//!
//! ## Truncated offset
//!
//! When the requested offset is greater than the total in-range
//! population, an honest prover skips everything it can and returns
//! zero items. The verifier should accept that case (it's not an
//! attack — the caller asked for a page past the end). We surface this
//! as `skipped < requested_offset` in the returned
//! `CountOffsetProofResult`; the caller can choose to treat it as an
//! error if their semantics require offset to be exactly satisfied.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};

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
    tree::value_hash as compute_value_hash,
    CryptoHash, Error,
};

/// One row of the verified result set: the matched key, the value
/// bytes the prover committed, the committed value-hash, and whether
/// the merk verifier independently confirmed a child-hash binding for
/// the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountOffsetReturnedItem {
    /// The matched key.
    pub key: Vec<u8>,
    /// The element's serialized value bytes, as emitted by the prover.
    ///
    /// For a `KVRefValueHash*` node these are the RESOLVED target's
    /// bytes, not the reference's: the caller's reference post-pass runs
    /// before the proof is encoded, so by verify time the dereferencing
    /// has already happened and `reference_element_hash` is what records
    /// that this row was a reference. (This is a change from the earlier
    /// contract, which said dereferencing happened at the GroveDB layer
    /// after verification — it never did, and reference rows were
    /// rejected outright instead.)
    pub value: Vec<u8>,
    /// The value-hash the proof's merk node committed for this entry.
    /// For `KVCount` nodes this is `H(value)` (the Item-flavored value
    /// hash). For `KVValueHashFeatureType` / `KVValueHash` it is the
    /// value-hash carried explicitly in the proof — which for
    /// tree-flavored entries is `combine_hash(H(value), child_root)`
    /// (or `combine_hash(H(value), NULL_HASH)` for empty trees).
    ///
    /// Callers building `ProvedPathKeyOptionalValue` must surface this
    /// value (not recompute via `value_hash(value)`) so downstream
    /// chain checks against the parent's recorded value-hash work
    /// correctly for non-Item entries.
    pub value_hash: CryptoHash,
    /// Whether the proof emitted a `KVValueHashFeatureTypeWithChildHash`
    /// node for this entry — i.e. the merk verifier independently
    /// confirmed `combine_hash(H(value), child_hash) == value_hash`.
    ///
    /// The current count-offset prover **never** emits
    /// `KVValueHashFeatureTypeWithChildHash`, so this is always
    /// `false`. The field exists so the GroveDB layer can route
    /// correctly into V1 strict-mode checks (which require
    /// `child_hash_verified = true` for non-empty trees); callers must
    /// not silently treat a `false` here as `true`.
    pub child_hash_verified: bool,
    /// For a resolved-reference row (`KVRefValueHash*`), the value hash
    /// of the REFERENCE element itself — the first half of the row's
    /// committed `combine_hash(reference_element_hash, H(value))`.
    /// `None` for a directly-valued row.
    ///
    /// This is what lets a caller authenticate the reference's own
    /// content without the proof carrying its bytes: a caller that knows
    /// the canonical shape a row must have can reconstruct those bytes
    /// and check `value_hash(reconstructed) == reference_element_hash`.
    /// GroveDB's indexed-axis verifier does exactly that, which is how a
    /// row's `SiblingReference(primary_key)` / hop budget / carried sum
    /// get authenticated rather than assumed.
    pub reference_element_hash: Option<CryptoHash>,
}

/// The verifier's reconstructed view of an offset-paginated count-tree
/// proof. The caller is still responsible for comparing `root_hash`
/// against their trusted root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountOffsetProofResult {
    /// Root hash of the reconstructed merk. Caller compares this
    /// against the expected root hash to complete verification.
    pub root_hash: CryptoHash,
    /// Items the prover returned, in the order the verifier
    /// encountered them during the directional walk.
    pub returned_items: Vec<CountOffsetReturnedItem>,
    /// Number of in-range items the prover skipped via offset, as
    /// independently derived from the proof. ≤ the offset the caller
    /// passed to verify; equal to it unless the in-range population
    /// was exhausted before the offset finished consuming.
    pub skipped: u64,
}

/// Verify an offset-paginated count-tree proof.
///
/// `proof_bytes` is the encoded `Vec<Op>` the prover produced.
/// `inner_range`, `offset`, `limit`, and `left_to_right` must match
/// what the prover used; the verifier uses them to drive the same
/// classification + accounting walk.
///
/// On success returns a [`CountOffsetProofResult`] containing the
/// reconstructed root hash, the returned items, and the
/// independently-derived skipped count.
pub fn verify_count_offset_on_range_proof(
    proof_bytes: &[u8],
    inner_range: &QueryItem,
    offset: u64,
    limit: Option<u64>,
    left_to_right: bool,
) -> CostResult<CountOffsetProofResult, Error> {
    if proof_bytes.is_empty() {
        // Empty merk → empty proof → no items, no skips.
        return Ok(CountOffsetProofResult {
            root_hash: NULL_HASH,
            returned_items: Vec::new(),
            skipped: 0,
        })
        .wrap_with_cost(OperationCost::default());
    }

    let mut cost = OperationCost::default();
    let decoder = Decoder::new(proof_bytes);

    // Phase 1: reconstruct the proof tree. Allowlist only the node
    // kinds an honest offset-paginated proof ever emits. Anything else
    // is treated as proof corruption.
    //
    // Two flavors coexist in the allowlist:
    // - **Single-axis** (`ProvableCountTree` / `ProvableCountSumTree`
    //   hosts): `HashWithCount` (collapsed) / `KVDigestCount`
    //   (boundary) / `KVCount` (returned Item) /
    //   `KVValueHashFeatureType` (returned Tree/Reference).
    // - **Dual-axis** (`ProvableCountProvableSumTree` PCPS hosts):
    //   `HashWithCountAndSum` (collapsed) / `KVDigestCountSum`
    //   (boundary) / `KVCountSum` (returned Item). PCPS Tree/Reference
    //   children still emit via `KVValueHashFeatureType` whose
    //   feature_type encodes both axes (no separate Node variant).
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| match node {
            Node::HashWithCount(_, _, _, _)
            | Node::KVDigestCount(_, _, _)
            | Node::KVCount(_, _, _)
            | Node::KVValueHash(_, _, _)
            | Node::KVValueHashFeatureType(_, _, _, _)
            | Node::HashWithCountAndSum(_, _, _, _, _)
            | Node::KVDigestCountSum(_, _, _, _)
            | Node::KVCountSum(_, _, _, _)
            // Resolved-reference returns. GroveDB's post-pass rewrites a
            // reference row's `KVValueHashFeatureType` into these before
            // encoding, so the value bytes are the dereferenced target's
            // and the node's own hash field is the reference element's.
            | Node::KVRefValueHashCount(_, _, _, _)
            | Node::KVRefValueHashCountSum(_, _, _, _, _)
            | Node::KVRefValueHashCountSumWithTargetChildHash(_, _, _, _, _, _) => Ok(()),
            other => Err(Error::InvalidProofError(format!(
                "unexpected node type in count-offset proof: {}",
                other
            ))),
        });
    let tree = cost_return_on_error!(&mut cost, tree_result);

    // Phase 2: walk the reconstructed tree, re-deriving offset/limit
    // accounting from the proof shape. Bounds start at (None, None) to
    // match the prover.
    let mut state = VerifyState {
        offset_remaining: offset,
        limit_remaining: limit,
        skipped: 0,
        returned: Vec::new(),
        left_to_right,
    };
    // `verify_count_offset_shape` returns a plain `Result<u64, Error>`
    // (no internal cost accumulation), so we use the no-add variant of
    // the project-standard cost-return macro.
    cost_return_on_error_no_add!(
        cost,
        verify_count_offset_shape(&tree, inner_range, None, None, &mut state)
    );

    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok(CountOffsetProofResult {
        root_hash,
        returned_items: state.returned,
        skipped: state.skipped,
    })
    .wrap_with_cost(cost)
}

/// Verifier-side mutable state — the mirror of the prover's
/// `EmitState`. We track `skipped` (incremented every time we
/// independently observe a count-bound skip in the proof) instead of
/// the prover's `returned` counter because the verifier collects the
/// actual items in a `Vec`; the cardinality is len().
struct VerifyState {
    offset_remaining: u64,
    limit_remaining: Option<u64>,
    skipped: u64,
    returned: Vec<CountOffsetReturnedItem>,
    left_to_right: bool,
}

/// Read the aggregate count out of a proof-tree node in O(1). Every
/// node type the count-offset proof flow emits carries the aggregate
/// in its count field; for `KVValueHashFeatureType` (used for
/// tree/reference children of a count tree) we read it out of the
/// `ProvableCountedMerkNode` / `ProvableCountedSummedMerkNode` feature
/// type. Returns `None` if the node is `KVValueHash` (a non-count
/// fallback we accept on the allowlist for raw merk usage but where
/// own_count can't be derived structurally; the caller treats this as
/// own_count = aggregate of the immediate node, which is 0 for our
/// purposes).
fn aggregate_of_proof_tree_node(tree: &ProofTree) -> Result<u64, Error> {
    use crate::TreeFeatureType;
    match &tree.node {
        Node::HashWithCount(_, _, _, c) => Ok(*c),
        Node::KVDigestCount(_, _, c) => Ok(*c),
        Node::KVCount(_, _, c) => Ok(*c),
        // Dual-axis (PCPS) variants — count is at the same conceptual
        // position; the sum field is used during Phase-1 hash
        // reconstruction (which `execute_with_options` already
        // performed before this function runs) and plays no role in
        // offset/limit accounting.
        Node::HashWithCountAndSum(_, _, _, c, _) => Ok(*c),
        Node::KVDigestCountSum(_, _, c, _) => Ok(*c),
        Node::KVCountSum(_, _, c, _) => Ok(*c),
        // Resolved-reference returns carry their aggregates in the same
        // conceptual position as the value-bearing variants.
        Node::KVRefValueHashCount(_, _, _, c) => Ok(*c),
        Node::KVRefValueHashCountSum(_, _, _, c, _) => Ok(*c),
        Node::KVRefValueHashCountSumWithTargetChildHash(_, _, _, c, _, _) => Ok(*c),
        Node::KVValueHashFeatureType(_, _, _, ft) => match ft {
            TreeFeatureType::ProvableCountedMerkNode(c) => Ok(*c),
            TreeFeatureType::ProvableCountedSummedMerkNode(c, _) => Ok(*c),
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(c, _) => Ok(*c),
            other => Err(Error::InvalidProofError(format!(
                "count-offset proof: KVValueHashFeatureType carries non-count feature type \
                 {:?} — expected ProvableCountedMerkNode / ProvableCountedSummedMerkNode / \
                 ProvableCountedAndProvableSummedMerkNode",
                other
            ))),
        },
        // The empty fallback. KVValueHash has no count; an honest
        // count-offset prover wouldn't emit it (count-tree returned
        // items are always count-bearing). Treat as aggregate 0 — the
        // outer dispatch rejects this node outside of empty-tree edge
        // cases.
        Node::KVValueHash(..) => Ok(0),
        // Truly unreachable: the `execute_with_options` allowlist
        // earlier in `verify_count_offset_on_range_proof` rejects any
        // node kind that isn't one of the eight matched above before
        // this function is ever called. Keeping the arm as
        // `unreachable!()` is both correct (it would only ever fire
        // if the allowlist were widened without updating this
        // function — a fail-loud safety net) and removes a dead
        // branch from coverage counting.
        _ => unreachable!(
            "aggregate_of_proof_tree_node: execute_with_options allowlist makes this branch \
             unreachable"
        ),
    }
}

/// Recursive shape-walk over the reconstructed proof tree. Returns the
/// **structural** count of this subtree.
///
/// The recursion does in-order directional traversal: for ascending
/// (`left_to_right = true`) it walks left, processes self, walks right;
/// for descending it walks right, processes self, walks left. This
/// matches the prover's emission order, so the offset/limit state
/// machine plays out identically on both sides.
///
/// `own_count` is derived in O(1) from the immediate children's
/// count fields (via `aggregate_of_proof_tree_node`), so it's known
/// *before* the second-direction child is walked — which is what
/// makes the in-order state machine work without a separate pre-pass.
/// The recursive return values are then used to validate that the
/// claimed aggregate counts are self-consistent across the proof tree.
fn verify_count_offset_shape(
    tree: &ProofTree,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
    state: &mut VerifyState,
) -> Result<u64, Error> {
    let class = classify_subtree(lo, hi, range);

    // ─── Collapsed-subtree leaves (HashWithCount / HashWithCountAndSum) ─
    //
    // Both single-axis and dual-axis (PCPS) hosts use a leaf collapsed
    // op. We treat them identically here — offset accounting cares
    // only about the count axis; the sum (in the dual-axis variant) is
    // already consumed by Phase-1 hash reconstruction.
    if let Some(count) = match &tree.node {
        Node::HashWithCount(_, _, _, c) => Some(*c),
        Node::HashWithCountAndSum(_, _, _, c, _) => Some(*c),
        _ => None,
    } {
        let count = &count;
        match class {
            SubtreeClassification::Disjoint => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "count-offset proof: HashWithCount(AndSum) at Disjoint position must \
                         be a leaf"
                            .to_string(),
                    ));
                }
                // No in-range items, no state mutation. Disjoint
                // contributes 0 to all running totals; the structural
                // count still has to bubble up so the parent's
                // own_count derivation works.
                return Ok(*count);
            }
            SubtreeClassification::Contained => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "count-offset proof: HashWithCount(AndSum) at Contained position must \
                         be a leaf"
                            .to_string(),
                    ));
                }
                // Two legitimate Contained-collapse cases (the prover's
                // emit logic chooses between them):
                //
                //   1. `offset_remaining > 0` → subtree fits inside the
                //      offset window. The prover's collapse rule is
                //      `count ≤ offset_remaining`; we enforce the same
                //      here and decrement offset.
                //
                //   2. `offset_remaining == 0 && limit_remaining == Some(0)`
                //      → past-limit collapse. No state change.
                //
                // Anything else is a malformed proof — an honest prover
                // would have descended to emit per-element data.
                if state.offset_remaining > 0 {
                    if *count > state.offset_remaining {
                        return Err(Error::InvalidProofError(format!(
                            "count-offset proof: HashWithCount(AndSum) at Contained position \
                             has count {} but only {} offset remaining — collapse is only \
                             valid when count ≤ offset_remaining",
                            count, state.offset_remaining
                        )));
                    }
                    state.offset_remaining -= *count;
                    state.skipped = state.skipped.checked_add(*count).ok_or_else(|| {
                        Error::InvalidProofError(
                            "count-offset proof: skipped count overflowed u64".to_string(),
                        )
                    })?;
                } else if state.limit_remaining != Some(0) {
                    return Err(Error::InvalidProofError(
                        "count-offset proof: HashWithCount(AndSum) collapse at Contained \
                         position is only valid when in the offset window or past the limit; \
                         prover should have descended"
                            .to_string(),
                    ));
                }
                return Ok(*count);
            }
            SubtreeClassification::Boundary => {
                return Err(Error::InvalidProofError(
                    "count-offset proof: HashWithCount(AndSum) cannot appear at a Boundary \
                     position — an honest prover would have descended into the boundary \
                     subtree"
                        .to_string(),
                ));
            }
        }
    }

    // ─── Per-element (boundary / descended-Contained) nodes ───────
    //
    // From here down, the node MUST carry a key (KVDigestCount, KVCount,
    // KVValueHashFeatureType, or KVValueHash). The key is required for
    // child-bound derivation; nodes without keys cannot legally appear
    // at non-collapsed positions in this proof.
    let node_key: &[u8] = match &tree.node {
        Node::KVDigestCount(key, _, _) => key.as_slice(),
        Node::KVCount(key, _, _) => key.as_slice(),
        Node::KVValueHashFeatureType(key, _, _, _) => key.as_slice(),
        Node::KVValueHash(key, _, _) => key.as_slice(),
        // Dual-axis (PCPS) per-element variants.
        Node::KVDigestCountSum(key, _, _, _) => key.as_slice(),
        Node::KVCountSum(key, _, _, _) => key.as_slice(),
        // Resolved-reference per-element variants.
        Node::KVRefValueHashCount(key, _, _, _) => key.as_slice(),
        Node::KVRefValueHashCountSum(key, _, _, _, _) => key.as_slice(),
        Node::KVRefValueHashCountSumWithTargetChildHash(key, _, _, _, _, _) => key.as_slice(),
        // Reaching here would require:
        //   - the `execute_with_options` allowlist accepted a node
        //     that doesn't carry a key (only `HashWithCount` /
        //     `HashWithCountAndSum` fit), and
        //   - the collapsed-subtree branch above didn't short-circuit
        //     (impossible — it returns from every match arm).
        // So in practice the only way to enter this arm is a code
        // refactor that widens the allowlist without updating this
        // match. Use `unreachable!()` as a fail-loud guard.
        _ => unreachable!(
            "verify_count_offset_shape: per-element switch unreachable for node {:?}",
            class
        ),
    };

    // The bound check rejects forged proofs that place a boundary key
    // outside its inherited subtree window.
    if !key_strictly_inside(node_key, lo, hi) {
        return Err(Error::InvalidProofError(format!(
            "count-offset proof: boundary key {} falls outside inherited subtree bounds \
             (lo={:?}, hi={:?})",
            hex::encode(node_key),
            lo.map(hex::encode),
            hi.map(hex::encode),
        )));
    }

    // Bounds for this node's children: left gets (lo, key), right gets
    // (key, hi). Direction-independent — these are tree-structural
    // bounds, not iteration bounds.
    let left_lo = lo;
    let left_hi = Some(node_key);
    let right_lo = Some(node_key);
    let right_hi = hi;

    // Derive aggregate / own_count BEFORE the directional recursion so
    // the in-order self-step has the disposition it needs. The
    // children's "aggregate" reads are O(1) lookups of their count
    // fields; we validate them against the recursive returns at the
    // end of this function.
    let aggregate = aggregate_of_proof_tree_node(tree)?;
    let left_aggregate = match &tree.left {
        Some(c) => aggregate_of_proof_tree_node(&c.tree)?,
        None => 0,
    };
    let right_aggregate = match &tree.right {
        Some(c) => aggregate_of_proof_tree_node(&c.tree)?,
        None => 0,
    };
    let own_count = aggregate
        .checked_sub(left_aggregate)
        .and_then(|s| s.checked_sub(right_aggregate))
        .ok_or_else(|| {
            Error::InvalidProofError(format!(
                "count-offset proof: immediate child aggregate counts ({} + {}) exceed \
                 parent's aggregate count ({})",
                left_aggregate, right_aggregate, aggregate
            ))
        })?;
    if own_count > 1 {
        return Err(Error::InvalidProofError(format!(
            "count-offset proof: own_count {} is impossible for a single tree node \
             (expected 0 or 1)",
            own_count
        )));
    }

    let in_range = range.contains(node_key);

    // Per-node-type eligibility check. Lets us reject obviously-malformed
    // proofs (value at out-of-range, etc.) before doing any recursion.
    let disposition = classify_self(&tree.node, in_range, own_count)?;

    // ─── Directional in-order recursion ─────────────────────────
    //
    // The recursive return values are *tautologically* equal to
    // `left_aggregate` / `right_aggregate` — both read the child's
    // count field via `aggregate_of_proof_tree_node`, which is
    // referentially transparent for a given `ProofTree` — so we
    // discard them. The recursive call is invoked for its
    // state-mutation side effects (offset/limit accounting on items
    // deeper in the subtree), not for the return value.
    let visit_left_first = state.left_to_right;
    if visit_left_first {
        if let Some(c) = &tree.left {
            verify_count_offset_shape(&c.tree, range, left_lo, left_hi, state)?;
        }
        apply_self_state(&disposition, state)?;
        if let Some(c) = &tree.right {
            verify_count_offset_shape(&c.tree, range, right_lo, right_hi, state)?;
        }
    } else {
        if let Some(c) = &tree.right {
            verify_count_offset_shape(&c.tree, range, right_lo, right_hi, state)?;
        }
        apply_self_state(&disposition, state)?;
        if let Some(c) = &tree.left {
            verify_count_offset_shape(&c.tree, range, left_lo, left_hi, state)?;
        }
    }

    Ok(aggregate)
}

/// Decide what *this* boundary node represents, given its on-the-wire
/// shape, the result of the in-range check, and the structurally
/// derived own_count. The returned `BoundaryKind` then drives the
/// state mutation in `apply_self_state`.
fn classify_self<'a>(
    node: &'a Node,
    in_range: bool,
    own_count: u64,
) -> Result<BoundaryKind<'a>, Error> {
    match node {
        Node::KVDigestCount(_, _, _) | Node::KVDigestCountSum(_, _, _, _) => {
            // KVDigestCount / KVDigestCountSum sit at four allowed
            // positions:
            //   - Out-of-range path node (own=0 OR own=1 — the value
            //     happens to be out of the range — both fine, no
            //     mutation)
            //   - In-range counted entry, offset window (own=1, consume
            //     offset slot)
            //   - In-range counted entry, past limit (own=1, no
            //     mutation)
            //   - In-range counted entry, limit window — ILLEGAL, the
            //     prover would have emitted a value-bearing node
            //     instead. apply_self_state catches this case via the
            //     "digest at offset=0 with limit slots remaining"
            //     check.
            //
            // The dual-axis `KVDigestCountSum` variant behaves
            // identically to `KVDigestCount` from the offset-accounting
            // perspective (the sum field is only used during Phase-1
            // hash reconstruction of `node_hash_with_count_and_sum`).
            //
            // **Rejected**: in-range with `own_count == 0` (a
            // NonCounted-wrapped entry inside the range). The
            // count-offset prover refuses to descend through these and
            // surfaces `NotSupported` instead — see the rejection in
            // `emit_count_offset_proof`. Encountering one here means
            // either a corrupt prover output or a forged proof
            // attempting to slip a NonCounted item through.
            if in_range && own_count == 1 {
                Ok(BoundaryKind::InRangeCountedDigest)
            } else if in_range && own_count == 0 {
                Err(Error::InvalidProofError(
                    "count-offset proof: KVDigestCount(Sum) at in-range position with \
                     own_count=0 (NonCounted-wrapped entry) — count-offset proofs \
                     don't yet support these; an honest prover refuses to descend \
                     through them"
                        .to_string(),
                ))
            } else {
                Ok(BoundaryKind::PathLikeOrNonCounted)
            }
        }
        Node::KVCount(key, value, _) => {
            // Value-bearing for Item-flavored entries on a single-axis
            // host. Must be in_range && own=1; the prover wouldn't emit
            // a value at any other position. The committed value-hash
            // for Item-flavored entries is just `H(value)` — `KVCount`
            // doesn't carry an explicit value-hash because the merk
            // hash chain recomputes it from the value bytes via
            // `kv_digest_to_kv_hash`.
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVCount at an out-of-range position".to_string(),
                ));
            }
            if own_count != 1 {
                return Err(Error::InvalidProofError(format!(
                    "count-offset proof: KVCount at own_count={} (expected 1)",
                    own_count
                )));
            }
            let vh = compute_value_hash(value.as_slice()).unwrap();
            Ok(BoundaryKind::ValueReturned {
                key: key.as_slice(),
                value: value.as_slice(),
                value_hash: vh,
                reference_element_hash: None,
            })
        }
        Node::KVCountSum(key, value, _, _) => {
            // Dual-axis (PCPS) Item-flavored value-bearing variant.
            // Identical contract to `KVCount` except the proof node
            // also carries the per-node sum so the verifier can
            // reconstruct `node_hash_with_count_and_sum` in Phase 1.
            // The sum plays no role in offset/limit accounting; we
            // simply surface the value bytes for the GroveDB layer.
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVCountSum at an out-of-range position".to_string(),
                ));
            }
            if own_count != 1 {
                return Err(Error::InvalidProofError(format!(
                    "count-offset proof: KVCountSum at own_count={} (expected 1)",
                    own_count
                )));
            }
            let vh = compute_value_hash(value.as_slice()).unwrap();
            Ok(BoundaryKind::ValueReturned {
                key: key.as_slice(),
                value: value.as_slice(),
                value_hash: vh,
                reference_element_hash: None,
            })
        }
        Node::KVValueHashFeatureType(key, value, vh, _) => {
            // Value-bearing for Tree/Reference children of a count
            // tree. Same eligibility rules as KVCount. The proof
            // carries the committed value-hash directly — for
            // tree-flavored entries this is `combine_hash(H(value),
            // child_root)` (or `combine_hash(H(value), NULL_HASH)` for
            // empty trees), so surfacing it unchanged lets the GroveDB
            // layer pass it through into the V1 strict-mode chain
            // checks faithfully.
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVValueHashFeatureType at an out-of-range position"
                        .to_string(),
                ));
            }
            if own_count != 1 {
                return Err(Error::InvalidProofError(format!(
                    "count-offset proof: KVValueHashFeatureType at own_count={} (expected 1)",
                    own_count
                )));
            }
            // V1 strict-mode KV→KVValueHash forgery guard (mirrors the
            // regular `execute_proof` check at `verify.rs:427`).
            //
            // Without this, an attacker can replace an honest
            // `KVCount(k, real_value, count)` (where `real_value` is an
            // Item) with `KVValueHashFeatureType(k, serialized_forged_Item,
            // H(real_value), ProvableCountedMerkNode(count))`: the merk
            // tree-hash chain still reconstructs because the proof
            // carries the committed `value_hash` directly rather than
            // recomputing it from `value`, but the surfaced bytes are
            // the attacker's forged Item. The downstream GroveDB
            // filter at `grovedb/src/operations/proof/verify.rs:523`
            // only blacklists NonCounted / Reference / non-empty Tree
            // shapes — it cannot tell a forged Item-in-tree-shape from
            // an honest tree return.
            //
            // KVValueHashFeatureType is the right proof-node type ONLY
            // for elements with `combine_hash`-composed value_hash
            // (subtrees, references, indexed-tree elements). Element
            // types with a simple `H(value)` value_hash (`Item`,
            // `SumItem`, `ItemWithSumItem`) MUST use `KVCount` /
            // `KVCountSum` (count tree) or the plain `KV` / `KVValueHash`
            // family (other trees), where the verifier recomputes the
            // value_hash from the value bytes via
            // `kv_digest_to_kv_hash` and forgery is structurally
            // blocked.
            let element_type = grovedb_element::ElementType::from_serialized_value(
                value.as_slice(),
            )
            .map_err(|e| {
                Error::InvalidProofError(format!(
                    "count-offset proof: cannot determine element type in \
                             KVValueHashFeatureType node: {e}"
                ))
            })?;
            if element_type.has_simple_value_hash() {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVValueHashFeatureType node must not contain a \
                     simple-value Element type (Item / SumItem / ItemWithSumItem) — these \
                     use a simple H(value) value-hash and an honest prover would emit \
                     KVCount or KVCountSum instead. Rejected to prevent KV→KVValueHash \
                     forgery (the proof's tree-hash chain only verifies the proof-carried \
                     value_hash, not that `value_hash == H(value)`)"
                        .to_string(),
                ));
            }
            Ok(BoundaryKind::ValueReturned {
                key: key.as_slice(),
                value: value.as_slice(),
                value_hash: *vh,
                reference_element_hash: None,
            })
        }
        // ─── Resolved-reference returns ──────────────────────────────
        //
        // GroveDB's post-pass rewrote a reference row into one of these
        // before encoding, so `value` is the RESOLVED target's bytes and
        // the node's hash field is the REFERENCE element's own value
        // hash. Phase 1 already bound both together: the tree-hash
        // reconstruction for these variants computes
        // `combine_hash(reference_element_hash, H(value))`, so a forged
        // target value or a forged reference hash breaks the root.
        //
        // Note the KV→KVValueHash forgery guard that `KVValueHashFeatureType`
        // needs does NOT apply here, and its absence is not a gap: that
        // guard exists because a proof-carried `value_hash` is not checked
        // against `H(value)`. For these variants the value hash IS
        // recomputed from the value bytes as part of the combine, so a
        // substituted `value` cannot survive.
        Node::KVRefValueHashCount(key, value, reference_element_hash, _) => {
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVRefValueHashCount at an out-of-range position"
                        .to_string(),
                ));
            }
            if own_count != 1 {
                return Err(Error::InvalidProofError(format!(
                    "count-offset proof: KVRefValueHashCount at own_count={} (expected 1)",
                    own_count
                )));
            }
            Ok(BoundaryKind::ValueReturned {
                key: key.as_slice(),
                value: value.as_slice(),
                // The committed value hash for a combined reference is
                // `combine_hash(reference_element_hash, H(target))` —
                // recomputed here so the surfaced hash is the one the
                // secondary root actually binds, not just half of it.
                value_hash: crate::tree::combine_hash(
                    reference_element_hash,
                    &compute_value_hash(value.as_slice()).unwrap(),
                )
                .unwrap(),
                reference_element_hash: Some(*reference_element_hash),
            })
        }
        Node::KVRefValueHashCountSum(key, value, reference_element_hash, _, _) => {
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVRefValueHashCountSum at an out-of-range position"
                        .to_string(),
                ));
            }
            if own_count != 1 {
                return Err(Error::InvalidProofError(format!(
                    "count-offset proof: KVRefValueHashCountSum at own_count={} (expected 1)",
                    own_count
                )));
            }
            Ok(BoundaryKind::ValueReturned {
                key: key.as_slice(),
                value: value.as_slice(),
                value_hash: crate::tree::combine_hash(
                    reference_element_hash,
                    &compute_value_hash(value.as_slice()).unwrap(),
                )
                .unwrap(),
                reference_element_hash: Some(*reference_element_hash),
            })
        }
        Node::KVRefValueHashCountSumWithTargetChildHash(
            key,
            value,
            reference_element_hash,
            _,
            _,
            target_child_hash,
        ) => {
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVRefValueHashCountSumWithTargetChildHash at an \
                     out-of-range position"
                        .to_string(),
                ));
            }
            if own_count != 1 {
                return Err(Error::InvalidProofError(format!(
                    "count-offset proof: KVRefValueHashCountSumWithTargetChildHash at \
                     own_count={} (expected 1)",
                    own_count
                )));
            }
            // Two combines, mirroring `Tree::hash` for this variant: the
            // target is layered, so its committed hash folds its child
            // commitment in before the reference folds over it.
            let target_committed = crate::tree::combine_hash(
                &compute_value_hash(value.as_slice()).unwrap(),
                target_child_hash,
            )
            .unwrap();
            Ok(BoundaryKind::ValueReturned {
                key: key.as_slice(),
                value: value.as_slice(),
                value_hash: crate::tree::combine_hash(reference_element_hash, &target_committed)
                    .unwrap(),
                reference_element_hash: Some(*reference_element_hash),
            })
        }
        Node::KVValueHash(key, value, _) => {
            // Non-count fallback. Only legitimate if the prover hit a
            // raw / unknown element type and fell back to the regular
            // Kv flow. Same eligibility rules as KVCount.
            if !in_range {
                return Err(Error::InvalidProofError(
                    "count-offset proof: KVValueHash at an out-of-range position".to_string(),
                ));
            }
            // own_count is structurally 0 here because aggregate_of's
            // KVValueHash branch returns 0 — meaning the prover
            // genuinely tracked this as an uncounted entry. Accept as
            // ValueReturned but with the understanding that no
            // offset/limit slot is consumed. This path is exercised
            // only by raw Merk users — every real GroveDB count tree
            // uses count-bearing value nodes.
            let _ = key;
            let _ = value;
            Err(Error::InvalidProofError(
                "count-offset proof: KVValueHash inside a count tree is unexpected; an honest \
                 prover would have emitted a count-bearing variant"
                    .to_string(),
            ))
        }
        // Same fail-loud reasoning as the per-element switch in
        // `verify_count_offset_shape`: only the five allowlisted node
        // kinds reach `classify_self`, and the four key-bearing ones
        // are handled above. The only way here is a refactor that
        // widens the allowlist without updating this match.
        _ => unreachable!("classify_self: dispatch unreachable for non-allowlisted node"),
    }
}

/// Per-boundary-node disposition. Drives which state mutation (if any)
/// the verifier applies at the in-order self-step.
enum BoundaryKind<'a> {
    /// Out-of-range path node OR in-range `NonCounted`-wrapped entry
    /// (own_count = 0). Neither consumes offset nor limit; the
    /// `node_hash_with_count` chain still binds the structural count.
    PathLikeOrNonCounted,
    /// In-range counted entry (own_count = 1) that the prover did
    /// **not** emit as a value. State mutation chooses between
    /// "decrement offset_remaining and bump skipped" (offset window)
    /// and "no state change" (past-limit); a third combination
    /// (offset=0 with limit slots free) is illegal and rejected.
    InRangeCountedDigest,
    /// In-range counted entry (own_count = 1) the prover returned.
    /// Consumes one slot of `limit_remaining` and appends to the
    /// returned-items vec.
    ValueReturned {
        key: &'a [u8],
        value: &'a [u8],
        /// Committed value-hash for this entry, surfaced unchanged
        /// from the merk proof so the GroveDB layer can build a
        /// faithful `ProvedKeyOptionalValue`. For `KVCount` this is
        /// `H(value)`; for `KVValueHashFeatureType` it's the
        /// proof-carried value_hash (tree-flavored entries store
        /// `combine_hash(H(value), child_root)`).
        value_hash: CryptoHash,
        /// For a resolved-reference return, the reference element's own
        /// value hash; `None` for a directly-valued entry. See
        /// [`CountOffsetReturnedItem::reference_element_hash`].
        reference_element_hash: Option<CryptoHash>,
    },
}

/// Apply the per-disposition state mutation when the verifier reaches
/// "self" in the directional in-order recursion. The eligibility
/// checks (in_range correctness, own_count consistency) were done by
/// `classify_self` before this is called, so this function only sees
/// legitimate self positions and only handles the remaining
/// state-vs-disposition checks (offset-window vs limit-window vs
/// past-limit).
fn apply_self_state(disposition: &BoundaryKind<'_>, state: &mut VerifyState) -> Result<(), Error> {
    match disposition {
        BoundaryKind::PathLikeOrNonCounted => {
            // No offset/limit accounting for out-of-range path nodes
            // or in-range NonCounted entries (own_count = 0).
            Ok(())
        }
        BoundaryKind::InRangeCountedDigest => {
            if state.offset_remaining > 0 {
                state.offset_remaining -= 1;
                state.skipped = state.skipped.checked_add(1).ok_or_else(|| {
                    Error::InvalidProofError(
                        "count-offset proof: skipped count overflowed u64".to_string(),
                    )
                })?;
                Ok(())
            } else if state.limit_remaining != Some(0) {
                Err(Error::InvalidProofError(
                    "count-offset proof: KVDigestCount at offset=0 with limit slots \
                     remaining — an honest prover would have emitted a value-bearing node"
                        .to_string(),
                ))
            } else {
                // Past-limit digest emission — accept, no state change.
                Ok(())
            }
        }
        BoundaryKind::ValueReturned {
            key,
            value,
            value_hash,
            reference_element_hash,
        } => {
            if state.offset_remaining > 0 {
                return Err(Error::InvalidProofError(
                    "count-offset proof: value node emitted with offset slots still remaining \
                     — an honest prover would have emitted KVDigestCount"
                        .to_string(),
                ));
            }
            if state.limit_remaining == Some(0) {
                return Err(Error::InvalidProofError(
                    "count-offset proof: value node emitted past the limit — an honest prover \
                     would have emitted KVDigestCount"
                        .to_string(),
                ));
            }
            if let Some(ref mut l) = state.limit_remaining {
                *l -= 1;
            }
            state.returned.push(CountOffsetReturnedItem {
                key: key.to_vec(),
                value: value.to_vec(),
                value_hash: *value_hash,
                // The current count-offset prover never emits
                // `KVValueHashFeatureTypeWithChildHash` (it has no need
                // to — Items in count trees don't have child merks to
                // verify, and tree/reference children rely on the
                // count-tree merk's hash chain). Setting this `false`
                // makes the GroveDB layer's downstream V1 strict-mode
                // checks reject non-empty tree returns, which is the
                // right behavior given that we don't carry a child
                // hash to validate.
                child_hash_verified: false,
                reference_element_hash: *reference_element_hash,
            });
            Ok(())
        }
    }
}
