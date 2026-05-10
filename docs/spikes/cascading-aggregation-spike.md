# Spike: Cascading Aggregation for CountIndexedTree

> **Status:** read-only investigation. No code changes; this note exists so
> that PR 2 (write path) can be planned with eyes open about how invasive
> the cascading-aggregation hook is.

## Goal

Identify the exact code path where a *descendant* write today causes a
**parent** count-tree element's `count_value` field to be rewritten, and
work out the cleanest way to bolt the secondary-Merk update onto that
path for `CountIndexedTree` / `ProvableCountIndexedTree`.

## Today's propagation pass — call graph

For an existing `CountTree` / `ProvableCountTree`, when a leaf changes
under a deeply nested subtree, the cascade up to its root flows through
the **batch apply pass** in [grovedb/src/batch/mod.rs](grovedb/src/batch/mod.rs).
Walking the relevant call sites:

```text
GroveDb::apply_batch_with_options
└─ apply_batch_structure                                  [batch/mod.rs:2539]
   └─ for each level, bottom-up:
      └─ TreeCacheMerkByPath::execute_ops_on_path         [batch/mod.rs:1128]
         └─ applies all ops at this path's Merk
         └─ returns (root_hash, root_key, aggregate_data) [merk/src/merk/mod.rs:132]

   └─ propagate to level above:                           [batch/mod.rs:2622-2870]
      └─ insert/merge GroveOp::ReplaceTreeRootKey { hash, root_key, aggregate_data }
         at parent_path[child_key]                        [batch/mod.rs:2635]

   └─ next iteration of the level loop processes that parent path:
      └─ execute_ops_on_path on parent_path
         └─ inside, for op == ReplaceTreeRootKey:
            └─ GroveDb::update_tree_item_preserve_flag_into_batch_operations
                                                          [grovedb/src/lib.rs:693]
               └─ reads existing element bytes from parent_tree
               └─ Element::reconstruct_with_root_key      [merk/src/element/reconstruct.rs:22]
                  └─ rewrites the element with new root_key + new aggregate
                     (preserves flags, preserves NonCounted wrapper)
               └─ tree.insert_subtree_into_batch_operations [merk/src/element/insert.rs:578]
                  └─ pushes Op::PutLayeredReference / Op::ReplaceLayeredReference
                     onto the parent Merk's pending ops
```

The end result: the parent's element bytes are rewritten with the new
`count_value`, the parent's KV-hash is recomputed, and propagation
continues upward.

## Where the rewrite knows the count change

The single place that has both **old** and **new** count values in scope
is [grovedb/src/lib.rs:693](grovedb/src/lib.rs:693), inside
`update_tree_item_preserve_flag_into_batch_operations`:

```rust
Self::get_element_from_subtree(parent_tree, key.as_ref(), grove_version)
    .flat_map_ok(|element| {                         // ← `element` carries the OLD count_value
        match element.reconstruct_with_root_key(maybe_root_key, aggregate_data) {
                                                      // ← `aggregate_data` carries the NEW count_value
            Some(tree) => { tree.insert_subtree_into_batch_operations(...) }
            None       => { /* not a tree element */ }
        }
    })
```

This is the natural hook. From here:

- old count = `element` (deserialize the existing primary-Merk bytes).
- new count = `aggregate_data.as_count_u64()`.
- key = `key`.
- the "primary Merk" we are about to rewrite = `parent_tree`.

If `parent_tree.tree_type` is `CountIndexedTree` or
`ProvableCountIndexedTree`, we additionally need to emit
`del(old_count_be ‖ key) + put(new_count_be ‖ key, ())` against the
**secondary Merk** (located at the prefix derived per S2-B from the
primary's prefix).

## The architectural mismatch

The propagation pass is built around the assumption that **each path
contributes one root_key to its parent's element**:

- `execute_ops_on_path` returns `(CryptoHash, Option<Vec<u8>>, AggregateData)`
  — one root hash, **one** root key.
- `GroveOp::ReplaceTreeRootKey { hash, root_key, aggregate_data }` carries
  one root key.
- `Element::reconstruct_with_root_key(&self, maybe_root_key, aggregate_data)`
  takes one root key.

A `CountIndexedTree` element carries **two** root keys (primary,
secondary), and its KV bytes hash both into the parent. None of the
infrastructure above accommodates that today.

The propagation also assumes one Merk per path: `TreeCacheMerkByPath`
holds `merks: HashMap<Vec<Vec<u8>>, Merk<S>>`
[batch/mod.rs:1106](grovedb/src/batch/mod.rs:1106). For
CountIndexedTree, *one path* needs to hold *two* Merks — the primary
(at `path`'s usual prefix) and the secondary (at the `Blake3(primary
prefix ‖ 0x01)` prefix).

## Recommended approach: sidecar secondary in TreeCache

Treat the secondary as a **sidecar** of the primary — same path, same
batch level, but a second Merk handle held alongside the primary.
Concretely:

### 1. Extend `TreeCacheMerkByPath`

```rust
struct TreeCacheMerkByPath<S, F> {
    merks: HashMap<Vec<Vec<u8>>, MerkBundle<S>>,
    get_merk_fn: F,
}

struct MerkBundle<S> {
    primary: Merk<S>,
    secondary: Option<Merk<S>>,    // Some only when primary is a CountIndexedTree variant
}
```

### 2. Extend `RootHashKeyAndAggregateData`

```rust
// merk/src/merk/mod.rs:132
pub type RootHashKeyAndAggregateData = (
    CryptoHash,
    Option<Vec<u8>>,
    AggregateData,
);
// → augment to optionally carry secondary info:
pub struct PathAggregateResult {
    pub primary_hash: CryptoHash,
    pub primary_root_key: Option<Vec<u8>>,
    pub aggregate_data: AggregateData,
    pub secondary: Option<(CryptoHash, Option<Vec<u8>>)>,
}
```

The two-tuple return is fine for a transition period, but at minimum
`execute_ops_on_path` needs a way to surface the secondary's
`(hash, root_key)` to the propagation pass.

### 3. Synthesize secondary ops inline in `execute_ops_on_path`

For each op processed at a CountIndexedTree path:

| Op kind                      | Effect on secondary                              |
|------------------------------|--------------------------------------------------|
| `InsertOrReplace { element }` (new key) | `put(new_count_be ‖ key, ())` |
| `InsertOrReplace { element }` (existing key, same count) | nothing |
| `InsertOrReplace { element }` (existing key, count changed) | `del(old_count_be ‖ key) + put(new_count_be ‖ key, ())` |
| `Replace { element }` | same as InsertOrReplace existing-key case |
| `Patch { element, change_in_bytes }` | same as Replace |
| `Delete` | `del(old_count_be ‖ key)` |
| `ReplaceTreeRootKey { aggregate_data, .. }` (cascade case) | `del(old_count_be ‖ key) + put(new_count_be ‖ key, ())` |
| `InsertTreeWithRootHash { aggregate_data, .. }` (first-time tree insert) | `put(new_count_be ‖ key, ())` |

The "old count" is always read from the primary Merk's existing element
bytes for that key, *before* the new element is written. The "new
count" is either explicit (`element.count_value()`) or derived from
`aggregate_data` (cascade cases).

After all primary ops are applied, the synthesized secondary ops are
applied to `bundle.secondary.as_mut().unwrap()` and the secondary's new
`(hash, root_key)` is included in the result.

### 4. New propagation op for CountIndexedTree elements

Either:

- Extend `GroveOp::ReplaceTreeRootKey` to carry an optional second root
  key + hash for CountIndexedTree, OR
- Add a sibling variant
  `GroveOp::ReplaceCountIndexedTreeRootKeys { primary_hash,
  primary_root_key, secondary_hash, secondary_root_key, aggregate_data }`.

The sibling-variant option is cleaner — the existing variant stays
focused on single-Merk trees and pattern matches don't need to handle
optionality.

### 5. Extend `reconstruct_with_root_key`

Add a sibling method for CountIndexedTree elements:

```rust
fn reconstruct_with_two_root_keys(
    &self,
    primary_root_key: Option<Vec<u8>>,
    secondary_root_key: Option<Vec<u8>>,
    aggregate_data: AggregateData,
) -> Option<Element>;
```

`Element::CountIndexedTree(.., flags)` and
`Element::ProvableCountIndexedTree(.., flags)` get arms that produce
the right element with both root keys.

### 6. Glue at the parent layer

`update_tree_item_preserve_flag_into_batch_operations` gains a sibling
function for the two-root-keys variant; both call into the same KV
write path but with the right reconstruct method and the right
combine-hash machinery (H1-A: `combine_hash_three`).

## Where Phase 1 (PR 1) work surfaces

These items already need to land in PR 1 because they're pure data /
hash / storage work that doesn't depend on propagation:

- `Element::CountIndexedTree`, `Element::ProvableCountIndexedTree`
  variants and their `NonCounted*` wrappers
- `TreeType::CountIndexedTree`, `TreeType::ProvableCountIndexedTree`
- `combine_hash_three` and its KV constructor
- Storage prefix derivation (S2-B helper)
- Element serialization round-trips

What's **not** in PR 1: any of the propagation plumbing changes above.
That belongs in PR 2.

## Specific code sites PR 2 will touch

| File | Site | Change |
|---|---|---|
| [grovedb/src/batch/mod.rs:208](grovedb/src/batch/mod.rs:208) | `GroveOp` enum | Add `ReplaceCountIndexedTreeRootKeys` variant (`#[non_exhaustive]`, internal-only). |
| [grovedb/src/batch/mod.rs:1106](grovedb/src/batch/mod.rs:1106) | `TreeCacheMerkByPath::merks` | Change value type to `MerkBundle<S>` (primary + optional secondary). |
| [grovedb/src/batch/mod.rs:1128](grovedb/src/batch/mod.rs:1128) | `execute_ops_on_path` | Behavior: when bundle has a secondary, synthesize secondary ops inline; surface secondary `(hash, root_key)` in the return. |
| [grovedb/src/batch/mod.rs:2622](grovedb/src/batch/mod.rs:2622) | upward propagation block | When a child path is a CountIndexedTree, emit `ReplaceCountIndexedTreeRootKeys` instead of `ReplaceTreeRootKey`. |
| [grovedb/src/batch/mod.rs:2201](grovedb/src/batch/mod.rs:2201) | parent-level handler for `ReplaceTreeRootKey` | Add sibling case for `ReplaceCountIndexedTreeRootKeys`. |
| [grovedb/src/lib.rs:693](grovedb/src/lib.rs:693) | `update_tree_item_preserve_flag_into_batch_operations` | Add a sibling fn that takes both root keys; route based on parent tree type. |
| [merk/src/element/reconstruct.rs:22](merk/src/element/reconstruct.rs:22) | `reconstruct_with_root_key` | Add `reconstruct_with_two_root_keys` (or generalize). |
| [merk/src/merk/mod.rs:132](merk/src/merk/mod.rs:132) | `RootHashKeyAndAggregateData` | Replace tuple alias with a struct that has an optional `secondary` field, OR add a parallel `PathAggregateResult` for CountIndexedTree paths. |
| [merk/src/element/insert.rs](merk/src/element/insert.rs) (~138, ~578) | `insert_subtree_into_batch_operations` | New variant that takes both root hashes for CountIndexedTree elements. Uses `combine_hash_three` to build the layered value hash. |

The user-write entry path (e.g.
[grovedb/src/operations/insert/mod.rs](grovedb/src/operations/insert/mod.rs))
mostly just needs to know "if I'm inserting/updating/deleting an element
under a `CountIndexedTree` primary, the batch processor will synthesize
the secondary update for me". No bespoke logic required at the user-write
sites — the heavy lifting stays inside the batch pass.

## Edge cases to write tests for

1. **First-time creation of a CountIndexedTree element** — both Merks
   empty, both root_keys = `None`, both root_hashes = `NULL_HASH`. Verify
   `combined_value_hash = Blake3(actual_value_hash || NULL_HASH ||
   NULL_HASH)` matches what the parent stores.

2. **First-time insert under a CountIndexedTree primary** — single
   secondary `put`, secondary's aggregate count goes 0 → 1.

3. **Cascading from depth ≥ 2** — leaf insert under a `CountTree` that
   sits under a `CountIndexedTree`. The CountTree's aggregate change
   propagates up; at the CountIndexedTree level, the secondary entry for
   the CountTree's key is rewritten with the new aggregate. Verify both
   the count_value in the primary's element bytes AND the secondary key
   match.

4. **Nested CountIndexedTree** — leaf insert under
   `CountIndexedTree → CountIndexedTree`. Two cascading secondary
   updates, one at each level.

5. **NonCountedCountIndexedTree as a child** — its aggregate is 0 at the
   parent, secondary entry for it sits at `(0x00..00 ‖ key)`.

6. **Delete that empties a primary** — primary becomes empty (root_key =
   `None`), secondary becomes empty (root_key = `None`), CountIndexedTree
   element rewritten with both root_keys = `None`,
   `combined_value_hash = Blake3(av_hash || NULL_HASH || NULL_HASH)`.

7. **DeleteTree on a CountIndexedTree element** — both Merks must be
   dropped (extension of existing drop logic in [batch/mod.rs](grovedb/src/batch/mod.rs)).

8. **Mixed batch with primary writes from multiple keys** — verify that
   `execute_ops_on_path` correctly accumulates primary writes and
   secondary mirror writes without ordering bugs.

9. **`InsertIfNotExists` skip path** — when an existing key with the
   same content is "inserted" with `error_if_exists: false`, no
   secondary update should be emitted. Add an explicit test (the
   prior M6 audit already flagged InsertIfNotExists edge cases).

## Risks identified

| ID | Risk | Plan |
|---|---|---|
| C-R1 | The `RootHashKeyAndAggregateData` tuple alias is used in many places. Changing its shape ripples. | Keep the alias for non-CountIndexedTree paths; introduce a parallel struct only for the new code paths. |
| C-R2 | Pattern-match exhaustiveness: dozens of `match GroveOp::*` and `match Element::*` sites will need new arms. | This is expected toil; PR 1 already covers the Element side, PR 2 covers GroveOp. |
| C-R3 | Cascading from a CountIndexedTree whose **parent** is also a CountIndexedTree — cascading op inside cascading op. | Should fall out automatically: each level's `execute_ops_on_path` synthesizes its own secondary ops; propagation collates per-level. Test #4 covers this. |
| C-R4 | NonCounted-wrapped CountIndexedTree element (`NonCountedCountIndexedTree`) — the wrapper bypasses parent's count aggregation, but the inner element still has two Merks. | `Element::NonCounted::reconstruct_with_two_root_keys` recurses on the inner element and re-wraps, mirroring the existing `reconstruct_with_root_key` NonCounted arm at [reconstruct.rs:75](merk/src/element/reconstruct.rs:75). |
| C-R5 | Storage layer drop / clear logic doesn't know about secondary prefixes. | Extend [grovedb/src/operations/delete/mod.rs](grovedb/src/operations/delete/mod.rs) and any "drop subtree" helpers to drop both prefixes for CountIndexedTree elements. Surface in PR 2. |
| C-R6 | Replication / chunk-restoration paths. The chunking machinery in [merk/src/merk/restore.rs](merk/src/merk/restore.rs) iterates a single Merk; it needs to know to also iterate the secondary. | Out of scope for PR 2 (replication is its own subsystem); flag as follow-up work and document the limitation. |
| C-R7 | `apply_batch_structure` adds an op at the parent level by either inserting or merging into an existing entry [batch/mod.rs:2633](grovedb/src/batch/mod.rs:2633). The merge logic for `ReplaceCountIndexedTreeRootKeys` needs a careful look. | Mirror the existing `ReplaceTreeRootKey` merge logic; add tests that exercise the merge case (e.g. an explicit `Insert` followed by a propagation update at the same parent key). |

## Recommendation for PR 2 sequencing

The propagation restructuring is real work — the size estimate I gave
earlier (~1,800 LOC) holds, but the **shape** of the work is now
clearer:

1. **First commit:** introduce `MerkBundle<S>` and the parallel struct
   for `RootHashKeyAndAggregateData`'s extension. No behavior change yet.
2. **Second commit:** extend `execute_ops_on_path` to handle `MerkBundle`s
   with a secondary, synthesizing secondary ops based on the table in
   §3 above. Extension is a no-op when `bundle.secondary == None`.
3. **Third commit:** add `GroveOp::ReplaceCountIndexedTreeRootKeys` and
   the corresponding propagation-block branch.
4. **Fourth commit:** `reconstruct_with_two_root_keys` +
   `update_tree_item_preserve_flag_into_batch_operations` sibling for
   CountIndexedTree.
5. **Fifth commit:** drop / clear logic extension.
6. **Tests** (per-commit or in a final commit, reviewer's preference).

Each commit compiles (key invariant: at no point in the stack are there
hard-coded panics for the new variants — they go through the new
machinery as soon as introduced).

## Deferred to a separate follow-up

- Replication / chunk restoration support for CountIndexedTree
  (C-R6). Requires teaching the restorer about two-Merk subtrees;
  likely as much work as PR 2 itself. Don't bundle.
- Estimated-cost / worst-case-cost coverage in
  [grovedb/src/batch/estimated_costs/](grovedb/src/batch/estimated_costs/).
  Should land in PR 2 for parity, but if it bloats, can spin off.

## Bottom line

The cascading update is **not** a small hook — it requires extending the
batch propagation pass to support multi-Merk subtrees. The good news:
the change is mechanical (no new algorithmic invariants, just plumbing),
the natural injection points are all identified above, and there are no
cross-cutting refactors. Plan ~6 commits within PR 2; budget 2–3 days
plus tests.
