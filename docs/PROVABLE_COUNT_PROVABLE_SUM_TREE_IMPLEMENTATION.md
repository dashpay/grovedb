# Implementation Plan: `Element::ProvableCountProvableSumTree`

> **For the fresh agent:** start by reading PR [#661](https://github.com/dashpay/grovedb/pull/661) end-to-end (squashed as commit `352c2f55` on develop). Then read this doc. The 90% of your work is mirroring what #661 did for sums, but doing BOTH count and sum simultaneously. The 10% that's new is calling out below in **Phase 7 — Known pitfalls**.

## TL;DR

Add a new tree element variant that bakes **BOTH** the aggregate count
**AND** the aggregate sum into every node's cryptographic hash, enabling
both `AggregateCountOnRange` AND `AggregateSumOnRange` proofs against
the same tree.

This is the natural union of `Element::ProvableCountTree` (count
hash-bound) and `Element::ProvableSumTree` (sum hash-bound). The
existing `Element::ProvableCountSumTree` stores both but **only the
count is hash-bound** — sum proofs are NOT verifiable against it.

## Reference implementation

Two existing variants on the codebase implement adjacent halves of the
contract you're building:

| Existing variant | What it does | Hash-bound axes |
|---|---|---|
| `Element::ProvableCountTree` | Counts hash-bound | count only |
| `Element::ProvableCountSumTree` | Stores count + sum; **only count is hash-bound** | count only |
| `Element::ProvableSumTree` | Sums hash-bound | sum only |
| **`Element::ProvableCountProvableSumTree`** (new) | Counts + sums BOTH hash-bound | **count AND sum** |

PR #661 (just merged as `352c2f55`) added `ProvableSumTree` and is your
template for the sum side. The pre-existing `ProvableCountTree` is your
template for the count side. You're combining them.

Read these PRs first to absorb the patterns:

- **PR #661** (`feat: add Element::ProvableSumTree + AggregateSumOnRange query`) —
  the sum-side template. Pay particular attention to:
  - [merk/src/proofs/query/aggregate_sum/](../merk/src/proofs/query/aggregate_sum/) — proof generation +
    verification (your work will mirror this almost arm-for-arm)
  - [merk/src/tree/hash.rs::node_hash_with_sum](../merk/src/tree/hash.rs) — the hash function
    that bakes sum into the node hash
  - [merk/src/tree/mod.rs::hash_for_link](../merk/src/tree/mod.rs) — the fail-closed gate that
    asserts the right `AggregateData` variant is present at hash time
  - [grovedb/src/operations/proof/aggregate_sum/](../grovedb/src/operations/proof/aggregate_sum/) — the GroveDB
    envelope walker + terminal-type gate
  - **Commit `d3278c10`** — the crossover tests against
    `ReferenceWithSumItem`. Re-read these as a model for the
    `ProvableCountProvableSumTree × RWSI` crossover you'll write.

- **PRs #663 / #664 / #666 / #667** — recently-landed develop work the
  agent should understand. #666 in particular added
  `Element::NotCountedOrSummed`, whose allow-list needs to grow to
  include this new variant.

## Decisions to lock in up front

1. **Element enum slot:** append at the **end** of the `Element` enum
   in [grovedb-element/src/element/mod.rs](../grovedb-element/src/element/mod.rs).
   The file's header rule is `ONLY APPEND TO THIS LIST!!!` — load-bearing
   for bincode wire compat. As of develop `352c2f55`, the last variant is
   `ProvableSumTree` (slot 19 in the bincode order), so your new variant
   occupies **slot 20**.

   If a sibling PR lands first and uses slot 20, slide to slot 21 and
   recompute twin discriminants accordingly — see **Pitfall #3**.

2. **ElementType base discriminant:** `ProvableCountProvableSumTree = 20`.

3. **Twin discriminants:** the three wrapper twin schemes have
   *different* conventions in the merged code. Match each one exactly:

   | Wrapper | Convention | New twin |
   |---|---|---|
   | `NonCounted` | strict `0x80 \| base` formula | `0x80 \| 20 = 148` |
   | `NotSummed` | **hand-assigned** in `0xB0..=0xBF` | **178** (`0xB2`, currently free) |
   | `NotCountedOrSummed` | **hand-assigned** in `0xC0..=0xCF` | **194** (`0xC2`, currently free) |

   The `NotSummed` / `NotCountedOrSummed` twins are hand-assigned because
   the `prefix | base` formula only happens to align for bases `< 16`;
   above 16 the formula collides with existing low-base slots. Precedent:
   `NotSummedProvableSumTree = 177 (0xB1)` and
   `NotCountedOrSummedProvableSumTree = 193 (0xC1)` are both hand-assigned
   (base 19 → would collide with the SumTree-base-3 slot under a strict
   formula). Audit the file for free slots when you actually do the work
   — pick the lowest free byte in each `0xB?` / `0xC?` range.

4. **Hash function:** new
   `node_hash_with_count_and_sum(kv_hash, left_hash, right_hash, count: u64, sum: i64) -> CostContext<CryptoHash>`
   in [merk/src/tree/hash.rs](../merk/src/tree/hash.rs). Encode count as 8 bytes big-endian
   followed by sum as 8 bytes big-endian. Hash order:
   `Blake3(kv_hash || left || right || count_be8 || sum_be8)`. The fixed
   8-byte encodings (not varint) are what makes the hash deterministic
   for adversarial extremes — varint would expose the prover's choice of
   size and create a malleability surface.

5. **AggregateData variant:** new
   `AggregateData::ProvableCountAndProvableSum(u64, i64)`. The existing
   `AggregateData::ProvableCountAndSum(u64, i64)` is **already taken** by
   `ProvableCountSumTree` (which hashes only the count); a new variant
   is required so `hash_for_link` can distinguish (see the existing
   ProvableCountSumTree arm at [merk/src/tree/mod.rs:682](../merk/src/tree/mod.rs:682)
   which destructures `ProvableCountAndSum` but only uses the count).

6. **TreeFeatureType variant:** new
   `TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(u64, i64)`
   in [merk/src/tree/tree_feature_type.rs](../merk/src/tree/tree_feature_type.rs).
   Parallel to `ProvableCountedSummedMerkNode(u64, i64)` but maps to the
   new `AggregateData` variant. Tied to the new `TreeType` discriminant.

7. **TreeType variant:** new `TreeType::ProvableCountProvableSumTree`
   in [merk/src/tree_type/mod.rs](../merk/src/tree_type/mod.rs).
   Discriminant **12** (next free after `ProvableSumTree = 11`).

8. **Aggregate proofs:** `AggregateCountOnRange` AND `AggregateSumOnRange`
   must BOTH be valid against this tree type. Plain `Sum` / `BigSum` /
   `ProvableSum` / `ProvableCount` / etc. trees keep their existing
   exclusive contracts.

9. **Wrapper compatibility:**
   - `NonCounted(ProvableCountProvableSumTree)` — INSERTABLE (the tree
     is count-bearing, so the wrapper has count to suppress).
   - `NotSummed(ProvableCountProvableSumTree)` — INSERTABLE (sum-bearing
     allow-list grows by one).
   - `NotCountedOrSummed(ProvableCountProvableSumTree)` — INSERTABLE
     (count-AND-sum-bearing allow-list grows by one).

## Phase 1 — Element + Type machinery

### 1.1 `grovedb-element/src/element/mod.rs`

Append a new variant at the end of the `Element` enum:

```rust
/// Same as Element::ProvableCountSumTree but BOTH the per-node count
/// AND the per-node sum are baked into the cryptographic state.
/// Mirrors `ProvableCountTree` + `ProvableSumTree` simultaneously,
/// enabling both `AggregateCountOnRange` AND `AggregateSumOnRange`
/// range queries to be cryptographically verified.
///
/// Bincode slot 20 — appended after `ProvableSumTree` at 19.
ProvableCountProvableSumTree(
    Option<Vec<u8>>,
    CountValue,
    SumValue,
    Option<ElementFlags>,
),
```

**Important:** `ONLY APPEND TO THIS LIST!!!` — the file's header rule is
load-bearing for bincode wire compat. Don't reorder.

Wire up:
- `element_type()` → `ElementType::ProvableCountProvableSumTree`
- `Display` impl
- The serde `ElementShadow` enum + its `From<ElementShadow> for Element`
- `check_recursive_wrapper_invariants` — pass-through (it's not a wrapper)

### 1.2 `grovedb-element/src/element_type.rs`

Append four `ElementType` variants:

```rust
/// Provable count + provable sum tree - discriminant 20.
/// BOTH count and sum baked into node hashes.
ProvableCountProvableSumTree = 20,

// In the NonCounted twin block:
/// `NonCounted` wrapper around `ProvableCountProvableSumTree`
/// - discriminant 148 (`0x80 | 20`).
NonCountedProvableCountProvableSumTree = 148,

// In the NotSummed twin block (hand-assigned within 0xB0..=0xBF):
/// `NotSummed` wrapper around `ProvableCountProvableSumTree`
/// - discriminant 178 (`0xB2`).
NotSummedProvableCountProvableSumTree = 178,

// In the NotCountedOrSummed twin block (hand-assigned within 0xC0..=0xCF):
/// `NotCountedOrSummed` wrapper around `ProvableCountProvableSumTree`
/// - discriminant 194 (`0xC2`).
NotCountedOrSummedProvableCountProvableSumTree = 194,
```

**Before committing the discriminant choices, re-grep the file** for
any new wrapper twins that landed since this doc was written:

```bash
grep -nE "= [0-9]+," grovedb-element/src/element_type.rs | \
  grep -iE "NotSummed|NonCounted|NotCountedOrSummed"
```

If 178 or 194 are now taken, pick the next free slot in the same prefix
range.

Update every relevant match arm in:
- `TryFrom<u8> for ElementType` — add the 4 new discriminants
- `from_serialized_value` — add `20` to the NonCounted base allowlist,
  add `20` to the NotSummed inner-byte match (mapping to
  `NotSummedProvableCountProvableSumTree`), add `20` to the
  NotCountedOrSummed inner-byte match
- `as_str` → `"provable count provable sum tree"`, plus the three
  wrapper variants
- `base()` per-variant match → add 4 new arms mapping each twin back
  to `ProvableCountProvableSumTree`
- `is_tree()` → true
- Doc comments at the top of the file listing the layout

### 1.3 `merk/src/tree_type/mod.rs`

Append:

```rust
pub enum TreeType {
    // ...
    ProvableCountProvableSumTree,  // discriminant 12
}
```

Update:
- `to_u8` / `try_from(u8)` — discriminant 12
- `Display` → `"Provable Count Provable Sum Tree"`
- `is_count_bearing` → **true**
- `is_sum_bearing` → **true**
- `is_count_and_sum_bearing` → **true** (already derived from
  `is_count_bearing && is_sum_bearing`, so just update the two
  primitives; the combined predicate at [merk/src/tree_type/mod.rs:174](../merk/src/tree_type/mod.rs:174)
  follows automatically)
- `allows_sum_item` → **true**
- `uses_non_merk_data_storage` → false
- `to_element_type` → `ElementType::ProvableCountProvableSumTree`
- Extend the `#[test]` arms for `is_count_bearing`, `is_sum_bearing`,
  `is_count_and_sum_bearing`, `allows_sum_item` etc. — each test
  enumerates every variant and would fail-fast without an explicit case.

### 1.4 `merk/src/tree/tree_feature_type.rs`

Append:

```rust
pub enum TreeFeatureType {
    // ...
    ProvableCountedAndProvableSummedMerkNode(u64, i64),
}
```

Wire up:
- Add `AggregateData::ProvableCountAndProvableSum(u64, i64)` to the
  `AggregateData` enum
- `AggregateData::from(TreeFeatureType)` — new variant maps to
  `AggregateData::ProvableCountAndProvableSum(c, s)`
- `parent_tree_type` → `TreeType::ProvableCountProvableSumTree`
- `as_sum_i64` (returns `Some(sum)`), `as_count_u64` (returns `Some(count)`),
  `as_summed_i128` (returns `Some(sum as i128)`) — see how the existing
  arms handle `ProvableCountedSummedMerkNode` and mirror.
- Extend the `#[test]` parametric tables in the same file.

### 1.5 `merk/src/tree/hash.rs`

Add a new hash function:

```rust
/// Compute a node hash that binds both the count AND the sum into
/// the digest. Used by `ProvableCountProvableSumTree` so that both
/// `AggregateCountOnRange` and `AggregateSumOnRange` proofs are
/// verifiable against the same tree.
///
/// Layout (all big-endian):
///   Blake3( kv_hash || left || right || count_be8 || sum_be8 )
///
/// Fixed 8-byte encodings rather than varint so the hash is
/// independent of how large the count/sum happen to be (a varint
/// encoding would expose the prover's choice of size and create a
/// malleability surface).
pub fn node_hash_with_count_and_sum(
    kv_hash: &CryptoHash,
    left_child_hash: &CryptoHash,
    right_child_hash: &CryptoHash,
    count: u64,
    sum: i64,
) -> CostContext<CryptoHash> { /* ... */ }
```

Mirror the cost accounting of `node_hash_with_sum` / `node_hash_with_count`.
Write unit tests in `hash.rs::tests` covering:
- Determinism (same inputs → same output)
- Distinct from `node_hash`, `node_hash_with_count`, `node_hash_with_sum`
- Sensitivity to each of the 5 inputs (mutating any input changes the hash)
- Boundary values (`count=0`, `sum=0`, `sum=-1`, `sum=i64::MIN`,
  `sum=i64::MAX`, `count=u64::MAX`)

### 1.6 `merk/src/tree/mod.rs::hash_for_link`

Add a new arm. **Use the fail-closed pattern** — copy the structure
verbatim from [merk/src/tree/mod.rs:703-725](../merk/src/tree/mod.rs:703) (the `ProvableSumTree`
arm). The pattern:

```rust
TreeType::ProvableCountProvableSumTree => {
    let aggregate_data = self
        .aggregate_data()
        .expect("ProvableCountProvableSumTree::hash_for_link: aggregate_data() failed");
    if let AggregateData::ProvableCountAndProvableSum(count, sum) = aggregate_data {
        node_hash_with_count_and_sum(
            self.inner.kv.hash(),
            self.child_hash(true),
            self.child_hash(false),
            count,
            sum,
        )
    } else {
        panic!(
            "ProvableCountProvableSumTree::hash_for_link: expected \
             AggregateData::ProvableCountAndProvableSum, got {:?}; the node's \
             feature_type is inconsistent with its tree_type",
            aggregate_data
        );
    }
}
```

**Do NOT silently fall through to `self.hash()` on mismatch** — that
was an earlier draft of #661 that CodeRabbit flagged as a soundness
risk. Look for the `_ => self.hash()` fall-through at the bottom of
the match — your new arm must come BEFORE the fall-through.

Add a matching arm in the **commit-time dispatch** at the same file
(currently around lines 1267 and 1315 — `grep -n "AggregateData::ProvableCount(count) => node_hash_with_count"`
to find them). Add:

```rust
AggregateData::ProvableCountAndProvableSum(count, sum) => {
    node_hash_with_count_and_sum(
        tree.inner.kv.hash(),
        tree.child_hash(true),
        tree.child_hash(false),
        *count,
        *sum,
    )
    .unwrap_add_cost(&mut cost)
}
```

Add a `#[should_panic]` regression test mirroring
`provable_sum_tree_hash_for_link_panics_on_feature_type_mismatch` at
[merk/src/tree/mod.rs:1791](../merk/src/tree/mod.rs:1791) to pin the new fail-closed gate.

### 1.7 `merk/src/element/tree_type.rs` (Element ↔ TreeType conversions)

The conversion layer between merk's `TreeType` and grovedb's `Element`
needs the new variant. Walk through all match arms — search for
`Element::ProvableCountSumTree` and `Element::ProvableSumTree`; you'll
need the same handling for `Element::ProvableCountProvableSumTree`.

## Phase 2 — Proof generation (count side + sum side)

The new tree must support BOTH `AggregateCountOnRange` AND
`AggregateSumOnRange` proofs. Two separate prover/verifier modules
already exist as subdirectories — extend each.

### 2.1 `merk/src/proofs/query/aggregate_count/mod.rs`

Extend the tree-type allowlist
([merk/src/proofs/query/aggregate_count/mod.rs:55](../merk/src/proofs/query/aggregate_count/mod.rs:55)):

```rust
pub(super) fn is_provable_count_bearing(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableCountTree
            | TreeType::ProvableCountSumTree
            | TreeType::ProvableCountProvableSumTree  // NEW
    )
}
```

Extend `provable_count_from_aggregate`
([merk/src/proofs/query/aggregate_count/mod.rs:67](../merk/src/proofs/query/aggregate_count/mod.rs:67)) to
extract the count from the new `AggregateData::ProvableCountAndProvableSum`
variant. Add an arm `AggregateData::ProvableCountAndProvableSum(c, _) => Ok(c)`.

The rest of the count proof machinery (prove.rs, emit.rs, walk.rs,
verify.rs) is tree-type-agnostic once the allowlist accepts the
variant — no changes needed there.

### 2.2 `merk/src/proofs/query/aggregate_sum/mod.rs`

Symmetric: extend the tree-type allowlist
([merk/src/proofs/query/aggregate_sum/mod.rs:70](../merk/src/proofs/query/aggregate_sum/mod.rs:70)):

```rust
pub(super) fn is_provable_sum_bearing(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableSumTree | TreeType::ProvableCountProvableSumTree
    )
}
```

Extend `provable_sum_from_aggregate` to extract the sum from the new
`ProvableCountAndProvableSum` variant. Add an arm
`AggregateData::ProvableCountAndProvableSum(_, s) => Ok(s)`.

Also extend the existing test
`is_provable_sum_bearing_only_for_provable_sum_tree` at
[merk/src/proofs/query/aggregate_sum/tests.rs:658](../merk/src/proofs/query/aggregate_sum/tests.rs:658) — rename
it or add a parallel test that accepts both variants. Likewise update
the count side's equivalent test.

### 2.3 GroveDB-side proof envelopes

In [grovedb/src/operations/proof/aggregate_count/](../grovedb/src/operations/proof/aggregate_count/)
and [grovedb/src/operations/proof/aggregate_sum/](../grovedb/src/operations/proof/aggregate_sum/),
the path-traversal helpers have a **terminal-type gate** that requires
the path's final element to be a specific Element variant. Extend both
gates:

- Count: accept `ProvableCountTree`, `ProvableCountSumTree`, AND
  `ProvableCountProvableSumTree` at the terminal layer.
- Sum: accept `ProvableSumTree` AND `ProvableCountProvableSumTree` at
  the terminal layer.

The error messages name the allowed types — update those to include
the new variant. Grep for the existing terminal-type rejection error
strings to find every site.

## Phase 3 — Wrapper compatibility

### 3.1 `NonCounted` wrapper

`Element::new_non_counted` and the merk-side insert guard both check
`is_count_bearing`. `ProvableCountProvableSumTree` IS count-bearing
(you made `is_count_bearing` return true in Phase 1.3), so the
existing guard correctly accepts it — no allowlist edit needed beyond
the predicate flip.

### 3.2 `NotSummed` wrapper

`Element::new_not_summed`'s allowlist needs to grow. In
[grovedb-element/src/element/constructor.rs](../grovedb-element/src/element/constructor.rs):

```rust
pub fn new_not_summed(inner: Element) -> Result<Self, ElementError> {
    match inner {
        Element::SumTree(..)
        | Element::BigSumTree(..)
        | Element::CountSumTree(..)
        | Element::ProvableCountSumTree(..)
        | Element::ProvableSumTree(..)
        | Element::ProvableCountProvableSumTree(..)  // NEW
        => Ok(Element::NotSummed(Box::new(inner))),
        // ...
    }
}
```

Update the matching allow-lists in:
- `Element::validate_wrapper_invariants` (deserialize post-check)
- `Element::serialize` pre-check
- `Element::deserialize` post-check
- `ElementType::from_serialized_value`'s NotSummed inner-byte match
- The doc comments on `Element::NotSummed`

### 3.3 `NotCountedOrSummed` wrapper

Same allow-list extension in `Element::new_not_counted_or_summed`,
`validate_wrapper_invariants`, `serialize`, `deserialize`, and
`from_serialized_value`'s NotCountedOrSummed inner-byte match.

The merk-side insert guard requires `is_count_and_sum_bearing()` — your
Phase 1.3 update makes this true for `ProvableCountProvableSumTree`
(it derives from the two primitives), so the guard accepts it
automatically.

## Phase 4 — GroveDB integration

### 4.1 `grovedb-element/src/element/helpers.rs`

Walk through every per-variant match. The patterns to extend:

- `sum_value_or_default` — return the sum field
- `count_value_or_default` — return the count field
- `big_sum_value_or_default` — return the sum as `i128`
- `count_sum_value_or_default` — return `(count, sum)`
- `is_any_tree`, `is_non_empty_tree`, `is_non_empty_merk_tree`,
  `is_basic_tree` (false), `is_sum_tree` (false — sum-bearing but
  not the basic SumTree), `is_provable_count_tree` (false — distinct
  variant), `is_provable_sum_tree` (false — distinct variant), new
  `is_provable_count_provable_sum_tree` (true)
- `get_flags` / `get_flags_mut` / `get_flags_owned` / `set_flags` —
  extract the `Option<ElementFlags>` field
- `as_provable_count_provable_sum_tree_value` (new borrowed accessor)
  → return `(count, sum)`
- `into_provable_count_provable_sum_tree_value` (new owning accessor)

### 4.2 `grovedb-element/src/element/constructor.rs`

Add constructors mirroring `ProvableCountSumTree` and `ProvableSumTree`:

```rust
pub fn empty_provable_count_provable_sum_tree() -> Self { /* ... */ }
pub fn empty_provable_count_provable_sum_tree_with_flags(
    flags: Option<ElementFlags>,
) -> Self { /* ... */ }
pub fn new_provable_count_provable_sum_tree(root_key: Option<Vec<u8>>) -> Self { /* ... */ }
pub fn new_provable_count_provable_sum_tree_with_flags(
    root_key: Option<Vec<u8>>,
    flags: Option<ElementFlags>,
) -> Self { /* ... */ }
pub fn new_provable_count_provable_sum_tree_with_flags_and_sum_and_count_value(
    root_key: Option<Vec<u8>>,
    count: u64,
    sum: i64,
    flags: Option<ElementFlags>,
) -> Self { /* ... */ }
```

### 4.3 `grovedb-element/src/element/serialize.rs` + `visualize.rs`

- `serialize`: nothing special — bincode derives handle it.
- `visualize`: add an `Element::ProvableCountProvableSumTree(...)` arm
  producing something like
  `"provable_count_provable_sum_tree: <root_key> count: <c>, sum: <s>"`.

### 4.4 `grovedb/src/operations/insert/mod.rs`

The insert path delegates to merk's `insert_subtree`. Walk every
`match element { ... }` arm and add the new variant:

- Cost computation
- Feature-type assignment when inserting into a count/sum-bearing parent
- The `should_propagate_as_summed_tree` / similar predicates

Search for `Element::ProvableCountSumTree` and add a parallel arm
for the new variant.

### 4.5 `grovedb/src/batch/mod.rs`

`GroveOp::InsertTreeWithRootHash` is constructed inside the batch
propagation logic. `grep -n "ProvableCountSumTree" grovedb/src/batch/mod.rs`
finds **8 sites** (verified against develop `352c2f55`) that need a
sibling arm for the new variant. Each builds a
`GroveOp::InsertTreeWithRootHash { hash, root_key, flags, aggregate_data,
non_counted, not_summed, not_counted_or_summed }` — preserve all field
assignments.

### 4.6 `grovedb/src/estimated_costs/{average_case_costs.rs,worst_case_costs.rs}`

Cost models need the new variant. Mirror the `ProvableCountSumTree` arms.

### 4.7 `grovedb/src/operations/get/{mod.rs,query.rs}`

Query result handling — search for `ProvableCountSumTree` and parallel
the arms.

### 4.8 `grovedb/src/operations/proof/{generate.rs,verify.rs}`

Proof generation dispatches to the merk-level aggregate prover/verifier
modules. As long as `is_provable_count_bearing` and
`is_provable_sum_bearing` (Phase 2) accept the new tree type, the
GroveDB layer should "just work" for both aggregate proofs. Verify
this by inspection — look for any direct `match tree_type` against
specific variants and add the new arm if needed.

### 4.9 `grovedb/src/reference_path.rs` + `debugger.rs` + `lib.rs`

Search for `ProvableCountSumTree`; add parallel arms where pattern
matches require exhaustiveness.

### 4.10 `grovedbg-types/src/lib.rs`

The debugger types crate has a parallel `Element` enum for the web
visualizer. Add a variant if applicable.

## Phase 5 — Tests

### 5.1 Discriminant pinning

In `grovedb-element/src/element_type.rs::tests`:
- `test_element_type_from_discriminant`: add assertions for byte 20
  (base) and 148 (NonCounted twin), 178 (NotSummed), 194 (NotCountedOrSummed).
- `test_element_serialization_discriminants_match_element_type`: add
  the new variant to the table; bump the `test_cases.len()` assertion
  if there's one.
- `test_not_summed_wrapper_discriminant_pinned`: add a case for
  `NotSummed(ProvableCountProvableSumTree)` mapping to inner byte
  20 → twin discriminant 178.
- `test_from_serialized_value_not_summed_paths`: add
  `20 => NotSummedProvableCountProvableSumTree`; update the bad-bytes
  rejection list.
- `test_from_serialized_value_not_counted_or_summed_paths`: same
  treatment.
- `test_from_serialized_value_not_counted_paths`: include `20` in
  the legal-base allowlist; add the discriminant `[15, 20]` →
  `NonCountedProvableCountProvableSumTree` assertion (or whatever
  the file's actual encoding is — verify against the existing
  `ProvableSumTree` case).

### 5.2 Constructor / helper tests

Mirror existing `ProvableSumTree` tests in
`grovedb-element/tests/element_constructors_helpers.rs`. Add a
`provable_count_provable_sum_tree_constructors_and_helpers` function
that exercises:

- Every constructor variant
- `is_provable_count_provable_sum_tree`, `is_any_tree`, etc. predicates
- Value accessors (borrowed + owned)
- Negative / zero / boundary sum values
- Maximal count + minimal sum simultaneously (tests the boundary
  arithmetic in the hash function)
- Flag round-trip

### 5.3 Display / serialize round-trip

In `grovedb-element/tests/element_display_and_serialization.rs`:
- Display string assertion
- Bincode round-trip
- Discriminant byte check (first byte == 20)

### 5.4 Merk-level prove + verify

Two sets of tests, one in each aggregate dir:

**`merk/src/proofs/query/aggregate_count/tests.rs`**: extend the
existing tests to also exercise `ProvableCountProvableSumTree` as the
host tree type. Easiest: parametrize the existing `make_15_key_*`
builder to take a `TreeType` and run the same range queries against
both `ProvableCountTree` and `ProvableCountProvableSumTree`. The
verifier should return the same counts in both cases.

**`merk/src/proofs/query/aggregate_sum/tests.rs`**: same —
parametrize the 15-key builder to take a `TreeType`, run range queries
against `ProvableSumTree` AND `ProvableCountProvableSumTree`.

**Headline crossover test:** build a single tree of
`ProvableCountProvableSumTree`, run BOTH a count proof AND a sum proof
against the SAME root hash, and verify both succeed. This is the
defining test for the whole feature.

### 5.5 GroveDB end-to-end tests

Create `grovedb/src/tests/provable_count_provable_sum_tree_tests.rs`
modeled after `provable_sum_tree_tests.rs` + parallel sections from
`provable_count_tree_structure_test.rs`. Cover:

- Insert into a normal tree (works)
- Insert items + verify aggregate is `ProvableCountAndProvableSum(c, s)`
- Aggregate-count proof round-trip
- Aggregate-sum proof round-trip
- **BOTH proofs against the same tree state** — the headline test
- Negative sums + non-trivial counts
- Empty tree → both proofs return `(NULL_HASH, 0)`
- Wrapper variants:
  - `NonCounted(ProvableCountProvableSumTree)` — accepted inside
    count-bearing parents
  - `NotSummed(ProvableCountProvableSumTree)` — accepted inside
    sum-bearing parents
  - `NotCountedOrSummed(ProvableCountProvableSumTree)` — accepted
    inside CountSumTree / ProvableCountSumTree /
    ProvableCountProvableSumTree parents
- **Crossover with `ReferenceWithSumItem`:** insert RWSI into a
  `ProvableCountProvableSumTree` parent; aggregate-sum proof should sum
  the RWSI weights; aggregate-count proof should count each RWSI as
  contributing 1. (See PR #661 commit `d3278c10` for the RWSI ×
  ProvableSumTree crossover test pattern — model your tests on those.)

### 5.6 Terminal-type gate tests

In `grovedb/src/tests/aggregate_count_query_tests.rs` and
`aggregate_sum_query_tests.rs`: add tests verifying that a count or
sum proof against an honest `ProvableCountProvableSumTree` leaf
succeeds, while a forged proof claiming the leaf is a
`ProvableCountProvableSumTree` when it's actually a plain `NormalTree`
is rejected by the terminal-type gate (V1 envelope).

### 5.7 Insert-guard rejection tests

In `merk/src/element/insert.rs::tests`: insert each wrapper variant
(`NonCounted`, `NotSummed`, `NotCountedOrSummed`) into a
`ProvableCountProvableSumTree` parent and verify acceptance per the
matrix above. Insert the new tree variant into a `NormalTree`,
`SumTree`, `CountTree`, etc. and verify it's accepted in all of them
(this variant is a tree itself, not a wrapper — anything that allows
tree children allows this).

## Phase 6 — Documentation

- [docs/book/src/appendix-a.md](book/src/appendix-a.md): add row(s)
  for the new Element / ElementType / TreeType triple.
- [CLAUDE.md](../CLAUDE.md): the Element System section currently says
  `// 8 element types with specific use cases:` (line 77) and lists 8
  example variants. This list has been stale for a while (the real
  count is now ~20). Either bump to the correct count and add this
  variant, or leave the list as illustrative-only — but DO add a
  mention of the new variant in the surrounding prose.
- Each new function gets a Rust doc-comment explaining its contract.
- `merk/src/tree/hash.rs::node_hash_with_count_and_sum` doc-comment
  must explain the encoding choice (fixed 8-byte BE, not varint, for
  determinism).
- `merk/src/tree/mod.rs::hash_for_link` doc-comment block on the
  fail-closed invariant — extend to mention the new arm.

## Phase 7 — Known pitfalls (from PR #661 hindsight)

1. **AggregateData wire compat — make a NEW variant.** The existing
   `AggregateData::ProvableCountAndSum` is used by `ProvableCountSumTree`,
   which hashes **only the count** (see [merk/src/tree/mod.rs:687](../merk/src/tree/mod.rs:687)
   where the sum is destructured but discarded). Reusing it for the
   new variant would conflate two distinct hash semantics. Create a
   new `AggregateData::ProvableCountAndProvableSum(u64, i64)` so the
   hash dispatch can pattern-match on the variant tag.

2. **`hash_for_link` must fail closed.** PR #661 reverted an earlier
   silent-downgrade-to-plain-hash pattern after CodeRabbit flagged it
   as a soundness risk: if the feature_type didn't match the
   tree_type's expectation, the old code would silently call
   `self.hash()` instead of the specialized hash, producing a wrong
   root that's hard to debug. Copy the panic pattern from
   [merk/src/tree/mod.rs:717-723](../merk/src/tree/mod.rs:717). Make sure your new
   arm comes BEFORE the `_ => self.hash()` fall-through.

3. **Discriminant rotation when sibling PRs land.** If a parallel PR
   lands first and uses slot 20, your new variant slides to slot 21
   (and twins shift accordingly). Match develop's positioning at
   merge time; don't insist on slot 20 if the slot is taken. PR #661
   had to rotate twice (PR #666 took its initial slot, then PR #667
   took the next one). The pattern:
   - Element enum: append at the end, take whatever the next free
     slot is.
   - `NonCounted` twin: use the formula `0x80 | base`.
   - `NotSummed` / `NotCountedOrSummed` twins: **hand-assign** the
     lowest free byte in the `0xB?` / `0xC?` ranges. Precedent:
     `NotSummedProvableSumTree = 0xB1` (base 19 hand-assigned, not
     `0xB0 | 19 = 0xB3` because the formula doesn't apply for
     base ≥ 16).

4. **Crossover tests are mandatory.** PR #667 (`ReferenceWithSumItem`)
   and PR #661 (`ProvableSumTree`) shipped in parallel without
   crossover tests until commit `d3278c10` was added late in #661's
   life. The motivating use case for RWSI was exactly the
   proof-bearing tree, so the crossover gap was embarrassing. Write
   the `ProvableCountProvableSumTree × RWSI` crossover tests **up
   front**, in the same PR — not as a follow-up.

5. **`pub(super)` for sibling helpers.** The existing
   `provable_count_from_aggregate` /
   `provable_sum_from_aggregate` are `pub(super)` so the sibling
   `walk.rs`/`emit.rs`/`verify.rs` inside the same aggregate
   subdirectory can call them. When you add a new
   `provable_count_and_provable_sum_from_aggregate` helper (if
   needed — you might not), match that visibility.

6. **Two phases of verification — both required.** The verifier in
   each `*/verify.rs` does Phase 1 (allowlist node types in the op
   stream) AND Phase 2 (shape-walk that re-asserts the bound
   classification). Don't skip Phase 2 — it's what makes the proof
   non-malleable. The shared `classify_subtree` /
   `key_strictly_inside` / `NULL_HASH` / `SubtreeClassification` in
   `merk/src/proofs/query/aggregate_common.rs` are already factored
   out; both your extended aggregate sides should import them.

7. **Per-variant explicit mapping for `NotSummed` / `NotCountedOrSummed`
   twins.** The bitwise `prefix | base` formula only happens to work
   for bases `< 16`; above 16 the low nibble overflows and collides
   with low-base slots already in the table. So use per-variant
   explicit matching in `ElementType::base()` and `try_from()` like
   the existing handling for `NotSummedProvableSumTree = 0xB1` and
   `NotCountedOrSummedProvableSumTree = 0xC1`. Don't try to derive
   the new twin from the formula.

8. **Update doc comments that enumerate variants.** Every time a
   doc-comment lists "the four sum-bearing tree variants" or "the
   five sum-bearing tree variants", bump the count and the variant
   list. Use:
   ```bash
   grep -rn -iE "sum-bearing tree variant|count-bearing tree variant" .
   ```
   to find them. There are ~20–30 such strings.

9. **Test count assertion.** Discriminant-pinning tests like
   `test_element_serialization_discriminants_match_element_type`
   often have a `test_cases.len() == N` assertion at the end as a
   trip-wire. Bump N when you add a new case, otherwise the test
   will reject the additional entry.

10. **NEVER push directly to develop.** Create a branch and open a
    PR. Run pre-commit (`cargo fmt` runs as a hook) — let the hook
    fix formatting and re-commit if it does. See `MEMORY.md` for the
    project conventions the agent should follow. After pushing, check
    for CodeRabbit comments and address them.

## Phase 8 — Submission

- Branch from current develop. **Verify the branch is up to date** —
  if more PRs landed after this doc was written, re-check the
  discriminant choices and the 8-site batch grep count.
- Commit in logical chunks (one per phase, roughly): types → hash →
  merk allow-lists → wrappers → grovedb wiring → tests → docs.
- PR description should link back to PR #661 as the reference
  implementation and call out the BOTH-hash-bound difference.
- Tag for review: ask for the same reviewer who reviewed #661 — they
  have full context on the wrapper-discriminant flow.

## Verification checklist before opening PR

- [ ] `cargo build --workspace` clean
- [ ] `cargo clippy --workspace --all-features` clean (`-D warnings`)
- [ ] `cargo test --workspace` all green
- [ ] New `ProvableCountProvableSumTree` discriminants documented in
      `docs/book/src/appendix-a.md`
- [ ] Both `AggregateCountOnRange` AND `AggregateSumOnRange` proof
      round-trips pass against the **same** `ProvableCountProvableSumTree`
      root (the headline crossover test)
- [ ] All 3 wrapper compatibility scenarios tested
- [ ] Crossover with `ReferenceWithSumItem` tested
- [ ] `#[should_panic]` regression test for the new fail-closed
      `hash_for_link` arm
- [ ] All 8 grep-able `ProvableCountSumTree` sites in `grovedb/src/batch/mod.rs`
      checked for parallel handling of the new variant
- [ ] Existing tests still pass unchanged (the new variant is purely
      additive)
- [ ] `grep -rn "sum-bearing tree variant\|count-bearing tree variant"`
      doc-comments updated where they enumerate variants
