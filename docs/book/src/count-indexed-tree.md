# The CountIndexedTree

> **Status:** implemented. All protocol-observable design points
> (element layout, hash composition, storage prefix derivation, query
> semantics, subquery handling) are finalized below. Two implementation-
> detail items (C1, W1) carry recommended defaults that may be revisited
> in follow-up work.

## Motivation

`CountTree` and `ProvableCountTree` both store a per-element **count value**
(`u64`) and aggregate the sum of those values up the tree. The aggregate is
useful for "how many?" questions, but the underlying Merk is keyed by the
user's key, not by the count. So a question like

> "Give me the ten elements with the highest count, with a proof"

requires scanning every element under the tree — `O(n)` work and an `O(n)`
proof — even though the answer is a tiny prefix of a count-sorted view.

A **CountIndexedTree** makes count-ordered access a first-class operation by
maintaining a **secondary, count-keyed** Merk alongside the primary
key-ordered Merk. Top-k-by-count becomes `O(log n + k)` with a standard Merk
range proof.

Two new element types are introduced:

| Element | Aggregation flavor |
|---|---|
| `CountIndexedTree` | Count aggregated through `CountedMerkNode` (count not in node hash) |
| `ProvableCountIndexedTree` | Count aggregated through `ProvableCountedMerkNode` (count baked into node hash) |

These mirror the existing pair `CountTree` / `ProvableCountTree` exactly —
the only addition is the secondary index. Existing `CountTree` /
`ProvableCountTree` behavior is unchanged.

Both element types ship together. They share virtually all
infrastructure — the only divergence is which feature type their primary
Merk nodes use (`CountedMerkNode` vs `ProvableCountedMerkNode`) and the
matching `node_hash` vs `node_hash_with_count` choice. No new grove
version is needed: GroveDB has not yet shipped, so this lands as part of
the current in-development version alongside the existing element types.

## What the index orders

The secondary index orders elements by their `count_value` field — the
same field used for `CountedMerkNode` / `ProvableCountedMerkNode`
aggregation in the primary Merk.

`count_value` carries different meanings depending on what's stored:

| Element being indexed | What `count_value` is |
|---|---|
| Leaf `Item` in a count-aware Merk | A per-element value (default 1 for plain inserts) |
| `Tree`, `CountTree`, `SumTree`, … (any subtree element) | The **aggregated descendant count** of that child subtree, propagated upward |
| `CountIndexedTree`, `ProvableCountIndexedTree` | The aggregated descendant count of the child's primary Merk |

The index does not distinguish these cases. It indexes whatever
`count_value` is, and it stays in sync with `count_value` because **every
write that mutates `count_value` also mutates the secondary entry for
that key**. This includes the case where `count_value` changes because
something *deeper in the grove* was updated — the existing GroveDB
aggregation propagation already rewrites the element at each ancestor
level for exactly that reason, and the CountIndexedTree handler just
observes that rewrite and emits the corresponding secondary del+put
alongside it.

So a `CountIndexedTree` whose children are themselves `CountTree` (or
any count-aggregating subtree) **does** support "which sub-bucket has
the most stuff?" queries, with the index always reflecting current
state. The cost of this is described in
[Cascading aggregation update](#cascading-aggregation-update) below.

## Element layout

```rust
// Conceptual; final field order follows existing convention.
Element::CountIndexedTree(
    primary_root_key:   Option<Vec<u8>>,
    secondary_root_key: Option<Vec<u8>>,
    count_value:        u64,                  // aggregated, like CountTree
    flags:              Option<ElementFlags>,
)

Element::ProvableCountIndexedTree(
    primary_root_key:   Option<Vec<u8>>,
    secondary_root_key: Option<Vec<u8>>,
    count_value:        u64,                  // aggregated, baked into node hash
    flags:              Option<ElementFlags>,
)
```

Discriminants are appended to the existing element-type list (next free
slots; see Appendix A). The `count_value` field has the same semantics as
in `CountTree` / `ProvableCountTree`: it is the aggregate count for this
subtree, used by the parent Merk's aggregation.

## Two Merks, one element

A CountIndexedTree element points at **two** physical Merk trees living at
**two** distinct storage prefixes:

```mermaid
graph TD
    PARENT["Parent Merk Node<br/>Element::CountIndexedTree<br/>primary_root_key = pk<br/>secondary_root_key = sk"]

    subgraph primary["Primary Merk — keyed by user key"]
        PK["pk (root)"]
        PA["alice → Item(...)"]
        PB["bob → Item(...)"]
        PK --> PA
        PK --> PB
    end

    subgraph secondary["Secondary Merk — keyed by (count_be ‖ user_key)"]
        SK["sk (root)"]
        SA["00..05 ‖ alice → ()"]
        SB["00..0c ‖ bob → ()"]
        SK --> SA
        SK --> SB
    end

    PARENT -.->|"primary portal"| PK
    PARENT -.->|"secondary portal"| SK

    style PARENT fill:#fef9e7,stroke:#f39c12,stroke-width:2px
    style primary fill:#d5f5e3,stroke:#27ae60,stroke-width:2px
    style secondary fill:#e8daef,stroke:#8e44ad,stroke-width:2px
```

> **Each Merk is unmodified.** They use the existing tree machinery, the
> existing aggregation, the existing proof system. The secondary index is a
> *use* of Merk, not an extension to it.

### Primary Merk

The primary Merk is exactly what a `CountTree` / `ProvableCountTree`
contains today. Its node feature type is:

| Element | Primary node feature type |
|---|---|
| `CountIndexedTree` | `CountedMerkNode(count_value)` |
| `ProvableCountIndexedTree` | `ProvableCountedMerkNode(count_value)` |

User reads-by-key are served entirely by the primary Merk. They are
indistinguishable in cost and proof shape from a query against a
`CountTree` / `ProvableCountTree` of the same size.

### Secondary Merk

The secondary Merk holds one entry per element in the primary, keyed by:

```text
secondary_key = count_be_bytes(8) ‖ original_key
secondary_val = ()        // empty; the original_key is encoded in the key
```

- **`count_be_bytes`** is the element's `count_value` encoded big-endian, 8
  bytes. Big-endian gives natural numeric order under lexicographic
  comparison, so right-to-left iteration yields highest-count-first.
- **`original_key`** is appended to break ties among elements with equal
  counts and to make each secondary key unique and reversible.

The secondary Merk uses node feature type `ProvableCountedMerkNode(1)` —
every entry contributes a count of `1`, so the aggregated count at the
secondary's root equals the total number of indexed entries (which also
equals the number of entries in the primary).

The reason the secondary is a *provable* count tree (rather than the
simpler `BasicMerkNode`) is that this lets the existing
`AggregateCountOnRange` infrastructure (see chapter "Aggregate Count
Queries") be applied **directly to the secondary**:

> "How many entries have `count_value` in `[a, b]`?"

is answered in `O(log n)` via a single `AggregateCountOnRange` proof
against the secondary, with no need to enumerate matching keys. The
trivial query "how many entries does this CountIndexedTree contain?"
collapses to a single hash-bound read of the secondary's root node.

The cost is one extra Blake3 invocation per secondary node (the
`node_hash_with_count` baking) — the same overhead `ProvableCountTree`
already pays today, applied to the secondary's keyspace.

## Storage prefix derivation

A regular subtree is addressed by a single 32-byte `SubtreePrefix` derived
from the path:

```text
prefix(path) = Blake3(path_body)
where path_body = concat(reversed_segments)
                  ‖ segment_count_native_endian
                  ‖ length_byte_per_segment
```

A CountIndexedTree needs two prefixes. The **primary** prefix is exactly
what a regular `Tree` at the same path would use — unchanged from the
current derivation. The **secondary** prefix is derived from the primary:

```text
primary_prefix(path)   = Blake3(path_body)
secondary_prefix(path) = Blake3(primary_prefix ‖ 0x01)
```

This has three useful properties:

- **Primary parity with `Tree`.** A CountIndexedTree's primary Merk lives
  at exactly the prefix a `Tree` would have at the same path, so the
  storage layer's layout for the primary is indistinguishable from a
  normal subtree.
- **Domain-separated secondary.** The secondary prefix is a Blake3 of a
  fixed-length 33-byte input that no path-derived prefix can produce
  (path-derived prefixes hash a variable-length `path_body` that always
  ends with per-segment length bytes, never with a single trailing
  `0x01` tag after a 32-byte prefix block). Collision resistance still
  rests on Blake3's preimage / 2nd-preimage assumptions, like every
  other prefix in the database.
- **Empty-path safety.** A root-level CountIndexedTree (unusual but
  legal) has `primary_prefix = 0x00..00` and
  `secondary_prefix = Blake3(0x00..00 ‖ 0x01)`. Both well-defined.

The implication for the storage layer: when the engine encounters a
`CountIndexedTree` element while resolving a path, it materializes two
storage contexts (one per prefix), not one. Everything downstream
(iterators, batch ops, transactions) operates on each context
independently and unchanged.

## Hash composition

The element's serialized bytes — which are what the parent Merk hashes —
include **both root keys** and the element's `count_value` and flags,
just like any other element. The parent's KV-hash also has to incorporate
each child Merk's root hash. Today `Tree`, `SumTree`, … do this through a
single supplied child hash combined into the value hash:

```text
// Today, for Tree (one child):
combined_value_hash = combine_hash(actual_value_hash, child_root_hash)
                    = Blake3(actual_value_hash || child_root_hash)
```

For `CountIndexedTree` / `ProvableCountIndexedTree` there are two child
Merks, so the combine step takes two supplied hashes instead of one and
concatenates them in the same slot:

```text
// CountIndexedTree (two children):
combined_value_hash = combine_hash_three(
                          actual_value_hash,
                          primary_root_hash,
                          secondary_root_hash,
                      )
                    = Blake3(actual_value_hash
                          || primary_root_hash
                          || secondary_root_hash)
```

- **Order is `primary || secondary`**, and is normative.
- **No domain separator.** Element-type differentiation comes from the
  discriminant byte in the serialized element bytes (which feeds into
  `actual_value_hash`), exactly as it does for every other element type
  today. Adding a separator just here would be an inconsistency.
- **Empty children** plug `NULL_HASH` into their slot, the same way an
  empty `Tree`'s child hash is `NULL_HASH` today.
- **No new Merk feature type** is required. The change is localized to
  the helper that builds `combined_value_hash` for these two element
  types and to the corresponding verifier path.

A proof that touches only one of the two Merks must therefore carry the
*other* tree's root hash so the verifier can reconstruct
`combined_value_hash`. This is one extra 32-byte hash per query,
regardless of result size.

## Write semantics

Every user-visible write produces operations on **both** Merks atomically.
The existing GroveDB batch infrastructure already gives cross-subtree
atomicity, so the new ops compose with batches just like any other
`GroveDbOp`.

### Insert `(k, v, count = c)`

1. Primary: `put(k, serialize(v))` with feature type `CountedMerkNode(c)`
   (or `ProvableCountedMerkNode(c)`).
2. Secondary: `put(c_be ‖ k, ())` with feature type
   `ProvableCountedMerkNode(1)`.
3. Both ops emitted in the same batch; both root hashes change; the
   parent's `combined_value_hash` is recomputed once from the new
   `(actual_value_hash, primary_root_hash, secondary_root_hash)` triple.

### Update count `(k: c_old → c_new)`

1. Primary: `put(k, ...)` with new feature type carrying `c_new`.
2. Secondary: `del(c_old_be ‖ k)`, then `put(c_new_be ‖ k, ())`.
3. Single batch; single propagation up.

The caller must know `c_old` to emit the deletion. The engine reads the
old count from the primary (one extra Merk read per update) — analogous to
how a delete in any subtree reads the existing element.

### Delete `(k)`

1. Read the element from primary to discover `c_old`.
2. Primary: `del(k)`.
3. Secondary: `del(c_old_be ‖ k)`.
4. Single batch.

### Cascading aggregation update

When a write changes `count_value` on an element via GroveDB's existing
aggregation propagation — i.e. someone wrote into a deeper subtree and
the aggregate count of an ancestor subtree changed — the
CountIndexedTree handler at that ancestor level treats it as a
count-update and emits `del(old_count_be ‖ key) + put(new_count_be ‖ key)`
on its secondary, in the **same batch** as the primary rewrite that
aggregation already produces.

```mermaid
graph TD
    LEAF["Leaf insert at deepest level"]
    L0["Innermost CountTree: aggregate N → N+1"]
    L1["Mid CountIndexedTree: element for inner has count_value N → N+1<br/>primary write + secondary del+put"]
    L2["Outer CountIndexedTree: element for mid has count_value M → M+1<br/>primary write + secondary del+put"]
    LEAF --> L0 --> L1 --> L2
```

> Each layer that is a `CountIndexedTree` (or `ProvableCountIndexedTree`)
> emits one secondary del+put. Layers that are plain `CountTree` or
> `ProvableCountTree` only do the primary rewrite that aggregation
> already requires.

### Write amplification

Let `d` = number of GroveDB levels traversed by aggregation propagation
(i.e. nesting depth above the leaf), and `k` = number of those levels
that are `CountIndexedTree` / `ProvableCountIndexedTree`. Per
**single-leaf write** the cost is:

| Operation | Primary work (already paid by aggregation) | Extra secondary work | Total extra vs plain CountTree stack |
|---|---|---|---|
| Insert | `(d+1) · O(log n)` | `(k+1) · O(log n)` | one secondary write per CountIndexedTree level + one for the leaf's own level |
| Update count | `(d+1) · O(log n)` | `(k+1) · O(log n)` (del+put per affected level) | same |
| Update non-count | `O(log n)` | `0` | none |
| Delete | `(d+1) · O(log n)` | `(k+1) · O(log n)` | one secondary delete per CountIndexedTree level |

The takeaway: **each `CountIndexedTree` level on the path from leaf to
root adds one secondary del+put per leaf write**. There is no quadratic
or `n`-dependent term — it is linear in `k`, the number of indexed
levels you opted into.

For typical layouts (`d = 3..6`, `k = 1..3`) this is well within budget.
The user controls `k` directly: pick `CountIndexedTree` at the levels
where you actually want count-ordered queries, and plain `CountTree` at
intermediate levels where you don't.

> **Open question (W1):** whether the cascading secondary updates should
> all flow through the standard batch machinery (one `GroveDbOp` per
> secondary del+put per level) or be emitted by a specialized handler
> inside the propagation pass. The first is simpler and reuses
> well-tested infrastructure; the second avoids re-walking the path. I'd
> default to the first.

## Read semantics

### Lookup by user key

Identical to `CountTree` / `ProvableCountTree`: traverse the parent Merk
to the element, open the **primary** Merk, query as usual. The secondary
Merk is not touched. The verifier receives the primary's root hash plus a
*single* extra 32-byte secondary root hash (so it can reconstruct
`combined_value_hash`).

### Top-k by count

```rust
// Conceptual API; final field/method shape TBD.
let result = db.query_count_indexed(
    path,
    CountIndexedQuery::top_k(10),
    grove_version,
)?;
// result.entries: Vec<(count: u64, key: Vec<u8>)>
```

By default the query returns `(count, key)` pairs only. Resolving the
primary value is opt-in via `resolve_values: true`, in which case each
returned entry is `(count, key, Element)` and the proof grows with k
primary inclusion proofs in addition to the secondary range proof.

Internally:

1. Resolve path through parent Merks down to the `CountIndexedTree`
   element. Standard layer proofs.
2. Open the **secondary** Merk.
3. Run a **descending range query** with `limit = k` over the full
   secondary keyspace. This yields the k highest-count entries, with a
   standard Merk range proof.
4. *(only if `resolve_values: true`)* For each `(c_be ‖ k)` in the
   result, open the **primary** Merk and query for `k`. Each resolution
   is one extra Merk read with one extra Merk inclusion proof.

The default keeps the proof minimal: secondary range proof + a 32-byte
attestation of the primary's root hash. Workloads that don't need the
values (leaderboards, ranking views, "top N usernames") pay nothing for
data they wouldn't read.

### Range by count

```rust
CountIndexedQuery::count_range(min..=max, limit)
```

Backed by a Merk range query on the secondary over
`[min_be ‖ 0x00.., max_be ‖ 0xff..]`. Same proof shape as top-k.

### How many entries have count in `[a, b]`?

Because the secondary is a `ProvableCountTree`, this is answered in
`O(log n)` via the existing `AggregateCountOnRange` machinery applied
directly to the secondary — no per-entry enumeration is needed.

```rust
let (root_hash, count) = db.verify_aggregate_count_query_on_secondary(
    proof,
    path,
    a..=b,                                     // count range, in count_be_bytes
    grove_version,
)?;
```

The verifier returns the matched count and the GroveDB root hash. The
trivial "total entries" query (`a = 0`, `b = u64::MAX`) collapses to a
single hash-bound read of the secondary's root node and is also
answered in `O(1)` via the parent's `Element::CountIndexedTree`
`count_value` field, which already commits the size.

### Direction

`CountIndexedQuery` supports both ascending and descending iteration
through `left_to_right: bool`, mirroring the existing `Query` API. The
common case for top-k is `left_to_right: false` (highest counts first),
which is what `CountIndexedQuery::top_k(k)` produces by default.
Ascending traversal is also supported for "smallest counts first" /
"items with the lowest counts in [a, b]" patterns.

### Lookup of count for a key

`CountIndexedQuery::count_of(key)` returns the count without returning the
value. It can be answered by reading the primary node's feature type
(which carries `count_value`). No secondary lookup needed; one Merk read.

### Subqueries

A `CountIndexedQuery` may carry a default subquery and/or count-keyed
conditional subqueries. These run against the **primary** Merk at each
match — i.e. once the secondary has identified the top-k (or
in-range) keys, those keys serve as starting points for further query
descent into the primary subtree under each match.

Subqueries from the secondary are **not** supported and never will be —
the secondary's values are `()`, and its keyspace `(count_be ‖ key)`
is an internal index, not a user-visible structure.

```rust
pub struct CountIndexedQuery {
    pub items: Vec<CountQueryItem>,           // top-k / count-range / etc.
    pub left_to_right: bool,
    pub limit: Option<u16>,
    pub offset: Option<u16>,
    pub resolve_values: bool,                 // Q1: default false

    pub default_subquery_branch: SubqueryBranch,
    pub conditional_subquery_branches:
        Option<IndexMap<CountQueryItem, SubqueryBranch>>,
}
```

`CountQueryItem` mirrors the existing `QueryItem` but operates on `u64`
counts instead of `Vec<u8>` keys:

```rust
pub enum CountQueryItem {
    Equal(u64),
    Range(Range<u64>),
    RangeInclusive(RangeInclusive<u64>),
    RangeFrom(RangeFrom<u64>),
    RangeTo(RangeTo<u64>),
    RangeToInclusive(RangeToInclusive<u64>),
    GreaterThan(u64),
    LessThan(u64),
    RangeFull,
}
```

#### Branch selection

For each top-k / range result with count `c`:

1. Walk `conditional_subquery_branches` in **insertion order** (an
   `IndexMap`, not a `HashMap`).
2. The **first** branch whose `CountQueryItem` contains `c` wins; its
   subquery is applied against the primary at the result key.
3. If no conditional branch matches, `default_subquery_branch` is
   applied (it may be empty, in which case no subquery runs).

Insertion-order, first-match semantics are deliberately the same as a
Rust match arm. Overlapping conditions are allowed; the earlier one in
the IndexMap wins.

#### Worked example

> "Top 10 buckets by aggregate count. For buckets with count > 1000,
> include their full content. For the rest, just `(count, key)`."

```rust
let mut conditionals = IndexMap::new();
conditionals.insert(
    CountQueryItem::GreaterThan(1000),
    SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new())),    // empty Query = full descent
    },
);

let q = CountIndexedQuery {
    items: vec![CountQueryItem::RangeFull],
    left_to_right: false,                          // descending
    limit: Some(10),
    offset: None,
    resolve_values: false,
    default_subquery_branch: SubqueryBranch::default(),
    conditional_subquery_branches: Some(conditionals),
};
```

#### Proof shape with subqueries

For each match where a subquery applied, the proof carries a normal
GroveDB layer-proof for that subquery — exactly the shape the existing
`PathQuery` system already produces. Verifier obligations:

- Verify the secondary range proof to obtain the list of `(count, key)`
  pairs. Each pair's `count` comes from the secondary key, which is
  hash-verified.
- For each pair, **independently** evaluate the conditional branches
  in insertion order against `count` to determine which subquery (if
  any) applied. The prover cannot lie about the selected branch
  because the verifier reproduces the choice deterministically.
- Verify each subquery's layer-proof against the primary subtree at
  the pair's key.

The proof grows linearly with the number of matches that triggered a
subquery. Workloads that use a no-op `default_subquery_branch` and no
conditionals pay nothing extra over plain top-k.

### Proof shape

A composite proof for a count-indexed query is structured exactly like
existing GroveDB layer proofs, with these additions:

```mermaid
graph TD
    L0["Layer proof: root → … → CountIndexedTree element<br/><i>standard, unchanged</i>"]
    EL["Element bytes: (primary_root_key, secondary_root_key, count_value, flags)<br/>actual_value_hash = Blake3(varint(len) || element_bytes)"]
    L1A["Primary Merk proof<br/><i>only if primary values were touched</i>"]
    L1B["Secondary Merk range proof<br/><i>over (count_be ‖ key) keys</i>"]
    COMB["combined_value_hash = Blake3(actual_value_hash || primary_root_hash || secondary_root_hash)<br/><i>order is primary, then secondary</i>"]

    L0 --> EL
    EL --> L1A
    EL --> L1B
    L1A --> COMB
    L1B --> COMB
```

Verifier obligations:

- Parent layer verifies the element bytes (carrying both root keys) up to
  the GroveDB root.
- Each Merk proof produces its own root hash (`primary_root_hash` and/or
  `secondary_root_hash`).
- The verifier reconstructs `combined_value_hash` from
  `actual_value_hash`, `primary_root_hash`, `secondary_root_hash` (in
  that order) and checks it matches the value hash committed in the
  parent layer.

Both root hashes must be made available to the verifier — when a query
touches only one of the two trees, the proof carries the *other* tree's
root hash as a 32-byte attestation (it is hashed but not traversed).

## When to use which element type

- **`CountTree`** — aggregate counts only; never need ordered access.
  Same cost as today.
- **`ProvableCountTree`** — same, but you also need the aggregate count
  bound into the proof.
- **`CountIndexedTree`** — you frequently ask "top-k by count" or
  "elements with count in `[a, b]`" and want sub-linear queries with
  proofs. Includes the case "which child subtree has the largest
  aggregated count?" — the cascading aggregation update keeps the
  index in sync automatically.
- **`ProvableCountIndexedTree`** — same, and the per-element count is
  itself a security-critical quantity (e.g. a stake weight, vote
  weight, fee priority) so it should be hashed into the primary Merk's
  nodes rather than just stored beside them.

The `Provable` flavor pays a small per-node hash cost for the count
binding; the non-provable flavor does not. Pick `Provable` when the
count is part of the protocol invariant; pick non-provable when the
count is metadata.

**Mixing levels.** Use `CountIndexedTree` at the levels where you
actually issue ordered queries; use plain `CountTree` at intermediate
levels where you only need aggregation. Each `CountIndexedTree` on the
path from a leaf to the root adds one secondary del+put per leaf write
(see [Write amplification](#write-amplification)).

## Interaction with `Element::NonCounted`

A `NonCounted` wrapper opts the wrapped element out of its parent's
count aggregation — its stored aggregate is `0`, and the parent reads
`count_value = 0` for that element. CountIndexedTree honors this
faithfully: a `NonCounted` child appears in the secondary at
`(0x00..00 ‖ key)` (the bottom of the count ordering) and contributes
`+1` to the secondary's aggregate (which is "number of indexed
entries", not "sum of counts").

NonCounted entries are **not excluded** from the secondary index —
sparse indexing is a non-goal. They are simply at the bottom of the
ordering. Top-k descending iteration encounters them last.

## Limitations and non-goals

- **No cross-tree count ordering.** A query orders elements *within*
  one `CountIndexedTree`. To rank items across multiple sibling trees,
  the application must compose results manually.
- **No floating-point or signed counts.** `count_value` is `u64`.
  Big-endian encoding gives correct order only for unsigned magnitudes.
  Indexing signed quantities would require an offset-bias encoding and
  is out of scope.
- **Index reflects committed state only.** The secondary mirrors the
  primary atomically per batch; readers in transactions see consistent
  views. There is no "pending" or "snapshot" index lag.
- **No partial indexing.** Every primary entry has a corresponding
  secondary entry. Conditional or sparse indexing is not part of this
  feature.
- **Write cost scales with the number of `CountIndexedTree` levels on
  the path.** This is by design: each indexed level adds one secondary
  del+put per leaf write. If you cannot afford this at a given level,
  use plain `CountTree` there instead.
- **No in-place migration from `CountTree` / `ProvableCountTree`.** A
  `CountIndexedTree` element is created as such from the start; an
  existing non-indexed count tree cannot be promoted in place. To
  migrate, the application rebuilds the tree as a `CountIndexedTree`
  via standard batch operations.

## Implementation-detail items

The following are not protocol-observable and may be revisited during
implementation. The defaults are recommended; they will be confirmed or
revised when the corresponding code is written.

| ID | Question | Recommended default |
|---|---|---|
| C1 | Cost-tracking surface for double-Merk writes — one combined cost line item or two? | Two (one per Merk), aggregated at the element level |
| W1 | Cascading secondary updates: route through standard batch ops, or specialized propagation handler? | Standard batch ops |

## Summary

A `CountIndexedTree` is the existing `CountTree` plus a count-keyed
mirror Merk. The element points at two root keys, the parent's value
hash combines both root hashes (`Blake3(actual_value_hash ||
primary_root_hash || secondary_root_hash)`), and the secondary turns
"top-k by count" from `O(n)` into `O(log n + k)` while preserving
GroveDB's standard proof semantics. `ProvableCountIndexedTree` is the
same construction with the primary Merk's count baked into node hashes,
mirroring the existing `ProvableCountTree` / `CountTree` distinction.
