# Count-Offset Paginated Queries

## Overview

A **count-offset paginated query** lets a caller paginate through the keys
inside a `ProvableCountTree` or `ProvableCountSumTree`, asking:

> "Skip the first *N* in-range items, then return the next *M* items."

…with a **single proof** whose size is proportional to `M + log(skipped)`,
not `M + skipped`. The skipped region collapses to one hash-bound
op per skipped subtree.

This is the missing pagination primitive for provable queries. Where
[aggregate count queries](aggregate-count-queries.md) ask "how many?",
count-offset paginated queries ask "give me page *N* of this range,
proving I skipped exactly the items between the start of the range and
the page".

Concretely the prover honors `SizedQuery::offset` and `SizedQuery::limit`
for the duration of a single-range query against a count tree:

```rust
let mut q = Query::new();
q.insert_range_inclusive(b"a".to_vec()..=b"z".to_vec());

let path_query = PathQuery::new(
    vec![b"my_count_tree".to_vec()],
    SizedQuery::new(q, /* limit  */ Some(20), /* offset */ Some(40)),
);

let proof_bytes = db.prove_query(&path_query, None, grove_version)?;
let (root_hash, results) = GroveDb::verify_query_raw(
    &proof_bytes, &path_query, grove_version,
)?;
// `results` holds in-range items 41..=60 (offset 40, then limit 20).
```

Inside the **offset window**, the prover never emits the skipped items' values,
only proofs that those items *exist* and contribute their full count to the
skipped total. Inside the **limit window** it emits the actual value-bearing
nodes. Past the limit it goes back to digest-only nodes. The verifier
independently re-derives the offset / limit accounting from the proof
shape — it never trusts the prover's numbers; it computes its own and
demands they match.

## Eligibility

Count-offset paginated proofs are accepted **only** for queries that pass the
`SizedQuery::validate_count_offset_paginated` gate:

1. **`SizedQuery::offset` is `Some(n)` with `n > 0`.** Offset = 0 is not
   pagination — the regular query path already covers that case.
2. **Exactly one item** in `Query::items`. Multi-item queries are out of
   scope for the initial implementation.
3. **The item is a true range variant** — `Range`, `RangeInclusive`,
   `RangeFrom`, `RangeFull`, `RangeTo`, `RangeToInclusive`, `RangeAfter`,
   `RangeAfterTo`, or `RangeAfterToInclusive`. `QueryItem::Key(_)` is
   **rejected** because it matches at most one key, so `offset > 0` is
   guaranteed to return zero items — a useless query that almost always
   indicates user error.
4. **No subqueries.** `default_subquery_branch.subquery.is_none()` /
   `subquery_path.is_none()` / no `conditional_subquery_branches`.
5. **No aggregate wrappers.** `AggregateCountOnRange` and
   `AggregateSumOnRange` have their own paginated semantics; we reject the
   combination so the two flows don't shadow each other.
6. **The `PathQuery::path` is non-empty.** The GroveDB root is always a
   `NormalTree`, never a count tree, so a root-level count-offset query
   has no valid target.
7. **The leaf merk's `tree_type` is `ProvableCountTree` or
   `ProvableCountSumTree`.** This is checked at the top of the proof
   generator by opening the merk and reading its tree type — anything
   else surfaces as `Error::InvalidQuery` with a clear "only valid against
   ProvableCountTree / ProvableCountSumTree" message.

Violating any of 1–6 returns `Error::InvalidQuery(...)` from
`SizedQuery::validate_count_offset_paginated`. Violating 7 returns the same
error from `check_count_offset_target_tree_type` (the prover's leaf-merk
precheck).

## V1-only — V0 proofs do **not** support this

This is a V1-proof-only feature. The V0 proof envelope is a shipped wire
format used by grove versions v1 and v2 in production; adding new
accepted query shapes there would be a consensus-breaking change for
already-deployed validators. V0's prover unconditionally rejects any
non-zero `offset`, and the verifier's V0/V1 split (in `verify_proof_internal`
and `verify_proof_raw_internal`) unconditionally rejects offset queries
against a V0 envelope while routing V1 envelopes through
`validate_count_offset_paginated`.

The `prove_count_offset_on_range` method on `Merk` is gated on
`MerkProofVersions::prove_count_offset_on_range` (initial implementation
version 0, set across all grove versions). The version field exists so a
coordinated prover/verifier change in a future grove version can bump it
to 1+ without breaking older callers.

## Why this works only on count trees that bind count into the hash

Same reasoning as [aggregate count queries](aggregate-count-queries.md):
only `ProvableCountTree` and `ProvableCountSumTree` use
`node_hash_with_count(kv_hash, left, right, count)` for their node-hash
computation, so a proof that asserts a particular count for a skipped
subtree is **cryptographically bound** — a forged count produces a
different reconstructed root hash and the chain check fails.

For `ProvableCountSumTree` the node hash binds only the **count** (not the
sum) — the sum is stored on the node but isn't in the hash, by the same
design choice that makes `AggregateCountOnRange` work uniformly for both
variants. So count-offset paginated proofs commit only the count too; the
sum is not part of this feature.

Plain `CountTree` / `CountSumTree` track counts but don't bind them to the
hash. Pagination proofs against them would be unverifiable.
`NormalTree`, `SumTree`, etc. don't even track counts. All are rejected at
the prover's leaf-merk precheck.

## How the proof is built

The proof generator carries two pieces of state through the recursion:

- `offset_remaining: u64` — how many in-range items the prover still needs
  to skip.
- `limit_remaining: Option<u64>` — how many in-range items the prover
  can still return (`None` = unlimited).

At each subtree, the prover [classifies it](aggregate-count-queries.md#verifier-shape-walk)
against the inner range — **Disjoint**, **Contained**, or **Boundary** — and
decides whether to **collapse** the entire subtree into a single
`HashWithCount` op or **descend** into it per-element. The decision is
direction-aware: for ascending walks (left-to-right) the prover visits
the left child first, then self, then right; for descending walks
(right-to-left) it visits right, then self, then left, so "the first N
in-range keys" matches the user-facing iteration order.

### Collapse rules

| Classification | Condition                                              | Emitted op                              | State mutation                          |
|----------------|--------------------------------------------------------|-----------------------------------------|-----------------------------------------|
| Disjoint       | always                                                 | `HashWithCount(kv_hash, l_h, r_h, c)`   | none — no in-range items                |
| Contained      | `subtree_count ≤ offset_remaining`                     | `HashWithCount(...)`                    | `offset_remaining −= subtree_count`     |
| Contained      | `offset_remaining == 0 && limit_remaining == Some(0)`  | `HashWithCount(...)` (past-limit)       | none                                    |
| Contained      | otherwise (partial-skip or partial-limit)              | **descend per-element**                 | —                                       |
| Boundary       | always                                                 | **descend per-element**                 | —                                       |

The first row is shared with `AggregateCountOnRange`: a Disjoint subtree
contributes zero to the in-range total but its structural count still has
to be hash-bound for the parent's own-count derivation (see "Why
`HashWithCount` is self-verifying" in the aggregate-count chapter).

The middle two rows are what's new for offset queries:

- **Whole-subtree skip** (`subtree_count ≤ offset_remaining`): the prover
  emits **one** `HashWithCount` op for an entire subtree and decrements
  `offset_remaining` by that subtree's count. This is the optimization
  the feature exists for — an offset of, say, 10,000 over a tree of
  100,000 items pays log-of-skipped proof size, not 10,000 ops.
- **Whole-subtree past-limit collapse**: once the limit is exhausted, any
  remaining Contained subtree is emitted as one `HashWithCount` with no
  state change. The verifier reaches it via the same collapse rule and
  accepts.

### Per-element emission inside a descent

When the prover descends into a Boundary node (or a Contained subtree
that's too big to fully skip), each node it visits emits one of:

| Per-node disposition                                                    | Emitted op                                            |
|-------------------------------------------------------------------------|-------------------------------------------------------|
| Out-of-range key (Boundary path node)                                   | `KVDigestCount(key, value_hash, count)`               |
| In-range `NonCounted`-wrapped entry (own_count = 0)                     | `KVDigestCount(key, value_hash, count)`               |
| In-range counted entry, **inside the offset window**                    | `KVDigestCount(key, value_hash, count)` (skip)        |
| In-range counted entry, **past the limit**                              | `KVDigestCount(key, value_hash, count)` (no return)   |
| In-range counted entry, **inside the limit window** — *returned*        | `KVCount(key, value, count)` or `KVValueHashFeatureType(key, value, value_hash, ft)` |

The same `KVDigestCount` op is used for four conceptually-different
positions; the verifier disambiguates by re-running the prover's state
machine in directional order and matching the op shape to the disposition
its current state implies. Out-of-range keys and `NonCounted` entries
naturally contribute `own_count = 0` to the state machine and the
verifier expects no state mutation for them.

The returned-item flavor depends on what's stored in the count tree:
- **Items / SumItems** → `KVCount(key, value, count)`.
- **Trees / References** → `KVValueHashFeatureType(key, value, value_hash, feature_type)`.
- **References under a count tree** get the same merk-level shape as
  trees; GroveDB's reference-resolution post-pass rewrites them to
  `KVRefValueHashCount` with the dereferenced value.

## Verifier shape walk

The verifier mirrors the prover's state machine in directional order. It
maintains:

- `offset_remaining` — initialized to the caller-passed offset, decremented
  whenever the verifier independently observes a count-bound skip.
- `limit_remaining` — initialized to the caller-passed limit, decremented
  for each returned item.
- `skipped` — running count of items the prover skipped, computed entirely
  from the proof shape (the prover's claimed number is not trusted).
- `returned: Vec<CountOffsetReturnedItem>` — items the verifier
  reconstructs from value-bearing nodes inside the limit window.

For each node it visits:

1. **Classify** the position (Disjoint / Contained / Boundary) using the
   same inherited-bounds logic the prover used.
2. **Validate the op shape** against the classification + own state. For
   example:
   - A `HashWithCount` at a Disjoint position must be a leaf in the proof
     (no attached children). A child here would mean the prover is
     hiding counted entries under a hash-only node.
   - A `HashWithCount` at a Contained position is valid only when
     `state.offset_remaining > 0` (skip-mode) or
     `state.limit_remaining == Some(0)` (past-limit). Anywhere else means
     the prover should have descended.
   - A value-bearing node (`KVCount`, `KVValueHashFeatureType`) is valid
     only when `state.offset_remaining == 0 && state.limit_remaining != Some(0)`.
3. **Derive `own_count`** for the current node in O(1) from its immediate
   children's count fields: `own = aggregate − left_aggregate − right_aggregate`.
   This is what lets the in-order state machine know — *before* recursing
   into the second-direction child — whether this position contributes a
   slot to offset or limit.
4. **Apply the per-position state mutation** in directional order:
   left → self → right for ascending, right → self → left for descending.
5. **Bubble the structural count** back up so the parent can re-derive its
   own `own_count`.

At the end the verifier returns `(root_hash, returned_items, skipped)`. The
caller compares `root_hash` against their trusted root and trusts the rest.

### Why we don't trust the prover's offset accounting

A malicious prover could try several attacks:

| Attack                                                          | Detection                                                                                                       |
|-----------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------|
| Forge `HashWithCount.count` to under-count a skipped subtree    | `node_hash_with_count` recomputation gives a different reconstructed root hash → chain check fails              |
| Forge `HashWithCount.count` to over-count a skipped subtree     | Same as above                                                                                                   |
| Substitute a value-bearing node for `KVDigestCount` mid-skip    | Verifier sees a value at a position where `state.offset_remaining > 0` → "value emitted with offset remaining"  |
| Substitute `KVDigestCount` for a value mid-limit                | Verifier sees a digest at `offset_remaining == 0 && limit_remaining > 0` → "digest at offset=0 with limit free" |
| Attach children to a leaf-position `HashWithCount`              | Shape-walk check rejects: "HashWithCount at Contained/Disjoint must be a leaf"                                  |
| Emit a `KVDigestCount` with a key outside its inherited bounds  | `key_strictly_inside` check rejects                                                                             |
| Emit children whose aggregates exceed the parent's              | `own_count = aggregate − left − right` underflow rejects                                                        |
| Inject a non-count node kind (e.g. `Hash`, `KVHash`)            | `execute_with_options` visit-node allowlist rejects                                                             |

These rejection branches all have dedicated forging tests in
`merk/src/proofs/query/count_offset/tests.rs`.

## Unsupported in-range value shapes (P1 / P2)

The count-offset proof flow's scope is **plain `Item` / `SumItem` /
`ItemWithSumItem` and empty trees inside a count tree**. Three shapes
are explicitly rejected by both the prover and the verifier:

| Rejected shape                              | Primary defense                                                                                                                                                                                                                                              |
|---------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **`NonCounted`-wrapped entry**              | Rejected at **insert time** by PR [#672](https://github.com/dashpay/grovedb/pull/672) — `NonCounted` cannot be stored inside a `ProvableCountTree` / `ProvableCountSumTree` at all. The merk-level prover and the verifier still reject defensively (against pre-#672 data on disk or any lower-level builder that bypasses the insert restriction). |
| **`Reference` / `ReferenceWithSumItem`**    | The regular flow's reference post-pass dereferences these to the target's value bytes. The count-offset short-circuit returns *before* that post-pass, so a verified result would expose the raw `Element::Reference` rather than the dereferenced target. Prover rejects at descent; verifier rejects in returned items. |
| **Non-empty tree** (any tree variant)       | V1 strict-mode requires a `KVValueHashFeatureTypeWithChildHash` proof node for these, which the count-offset prover doesn't emit. Accepting one without that node would silently bypass the child-hash invariant the regular flow enforces.                    |

### Why the `NonCounted` rejection is enforced at insert time

A `ProvableCountTree` binds its count aggregate into every node hash
via `node_hash_with_count`. The `HashWithCount` collapse rule in the
prover (`Contained` subtree + `subtree_count ≤ offset_remaining`) folds
an entire subtree into one self-verifying op whose committed count
field is what consumes the offset budget.

`NonCounted` children contribute `own_count = 0`, so they don't show up
in `subtree_count` — but they *are* visible to regular GroveDB
pagination. Allowing them in a `ProvableCountTree` would mean a
contained subtree like `[counted-a, NonCounted-b, counted-c]` with
`offset = 2, limit = 1` could collapse as `HashWithCount(count = 2)`
and verify with `returned = []`, while regular pagination would return
`[c]`. That's a silent semantic divergence.

#672 closes the gap at the only place it can be closed without changing
the proof wire format: the `Element::insert` / batch path refuses to
store `NonCounted` inside a `Provable*` count parent. With that
invariant in place, `subtree_count` always equals the actual entry
count for these trees, and the collapse rule is safe.

### Lifting the remaining restrictions (follow-up work)

- **Non-empty trees**: emit `KVValueHashFeatureTypeWithChildHash` (mirroring
  the regular V1 prover) and drop the verifier-side rejection.
- **References**: apply the same reference-post-pass the regular V1
  prover uses, rewriting `Reference` / `ReferenceWithSumItem` value
  nodes into `KVRefValueHashCount` with the dereferenced target's bytes.
- **NonCounted entries** are unlikely to become legal here, since the
  whole `ProvableCountTree` model depends on `subtree_count == entry
  count`. If that semantic is ever wanted, the right path is a
  different tree type, not relaxing the insert rule.

## API surface

Count-offset paginated queries go through the **same** `prove_query` /
`verify_query_raw` / `verify_query_with_options` entry points as every
other path query — there is no dedicated entry point. The query envelope
(`PathQuery` with a `SizedQuery` carrying a non-zero `offset`) is what
selects the count-offset dispatch.

**Prover side:**

```rust
// Same entry point as every other path query.
GroveDb::prove_query(&path_query, prove_options, grove_version)
    -> CostResult<Vec<u8>, Error>
```

Internally `prove_subqueries_v1` short-circuits at the leaf when
`path_query.path.len() == current_path.len() && path_query.has_non_zero_offset()`,
calls `Merk::prove_count_offset_on_range`, and wraps the bytes in a
`LayerProof` with empty `lower_layers`.

**Verifier side:**

```rust
// Same entry points as every other path query.
GroveDb::verify_query_with_options(proof, &path_query, options, grove_version)
GroveDb::verify_query_raw(proof, &path_query, grove_version)
```

`verify_proof_internal` / `verify_proof_raw_internal` enforce the V0/V1
split on offset, then `verify_layer_proof_v1` short-circuits at the leaf
to `run_count_offset_layer_dispatch`, which:

1. Rejects unexpected `lower_layers` (an honest count-offset leaf proof
   has none — the validator forbade subqueries).
2. Calls `verify_count_offset_on_range_proof` for the merk-level shape
   walk.
3. Rejects any non-empty tree returned item.
4. Translates each surviving returned item into a `ProvedPathKeyOptionalValue`
   using the merk-surfaced value-hash and `child_hash_verified` flag
   (not synthesized — see comment in `CountOffsetReturnedItem`).

The merk-level type that the verifier emits per item is

```rust
pub struct CountOffsetReturnedItem {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    /// `H(value)` for `KVCount`; proof-carried value_hash for
    /// `KVValueHashFeatureType` / `KVValueHash` (tree-flavored entries
    /// store `combine_hash(H(value), child_root)`).
    pub value_hash: CryptoHash,
    /// Always `false` for now — the current prover never emits the
    /// with-child-hash variant.
    pub child_hash_verified: bool,
}
```

## Comparison table

|                              | Regular query                       | Aggregate count                    | **Count-offset paginated**          |
|------------------------------|-------------------------------------|-------------------------------------|-------------------------------------|
| Return type                  | items                               | `u64` count                         | items (subset of range)             |
| Honors `SizedQuery::offset`? | yes (but proofs reject it)          | no                                  | **yes** (V1 only, count trees only) |
| Honors `SizedQuery::limit`?  | yes                                 | leaf: no, carrier: yes              | yes                                 |
| Direction-aware?             | yes                                 | no (counting is direction-agnostic) | yes                                 |
| Supported on V0 proofs?      | yes                                 | yes                                 | **no** (V1 only)                    |
| Allowed tree types           | any                                 | ProvableCount / ProvableCountSum    | ProvableCount / ProvableCountSum    |
| Proof size vs offset         | O(offset + limit)                   | n/a                                 | **O(log(offset) + limit)**          |

## Future work

- **Multi-item queries / subqueries.** Currently rejected by
  `validate_count_offset_paginated`. The merk machinery extends naturally
  if the per-level state-machine accounting can be re-derived correctly.
- **Non-empty tree returns.** Requires the prover to emit
  `KVValueHashFeatureTypeWithChildHash` for tree children of a count tree,
  same as the regular V1 prover does. The verifier's child-hash check
  would then accept those entries instead of rejecting them in the
  GroveDB layer.
- **Aggregate-style multi-layer paginated proofs.** The
  `AggregateCountOnRange` carrier shape (an outer multi-key walk with
  inner-aggregate leaves) could be extended to paginated outer walks
  with count-offset inner leaves. Out of scope for the initial PR.
