# Aggregate Sum on Range Queries

## Overview

An **Aggregate Sum on Range Query** lets a caller ask:

> "What is the total sum of children whose keys fall in this range, in this
> `ProvableSumTree`?"

The answer is a signed `i64`, and on a `ProvableSumTree` it comes back with a
cryptographic proof. A verifier holding the tree's root hash can compute the
total from the proof in `O(log n + |boundary|)` work — without ever
materializing the `SumItem` values themselves.

This is the parallel to [Aggregate Count on Range](aggregate-count-queries.md)
for sum trees. The two query types are orthogonal: an aggregate-sum query
returns a sum, an aggregate-count query returns a count, and a single
`PathQuery` may not contain both.

> **Not to be confused with [Aggregate Sum Queries](aggregate-sum-queries.md).**
> That existing API is a sum-budget iterator — it walks a SumTree returning
> `(key, sum_value)` pairs until a running total is reached. `AggregateSumOnRange`
> is a different feature: it answers "what is the verified total for keys in
> this range?" without returning any values, and only against the
> `ProvableSumTree` element type.

The feature is implemented as a `QueryItem` variant:

```rust
pub enum QueryItem {
    Key(Vec<u8>),
    Range(Range<Vec<u8>>),
    // ... existing variants ...
    AggregateCountOnRange(Box<QueryItem>),

    /// Sum the per-node sum contributions of children matched by the inner
    /// range, without returning them. Only valid on ProvableSumTree (and its
    /// `NonCounted` / `NotSummed` wrapper variants).
    AggregateSumOnRange(Box<QueryItem>),
}
```

The wrapped `QueryItem` is the **range to sum over**. As with
`AggregateCountOnRange`, it must be one of the true range variants:
`Range`, `RangeInclusive`, `RangeFrom`, `RangeTo`, `RangeToInclusive`,
`RangeAfter`, `RangeAfterTo`, `RangeAfterToInclusive`. The single-key
(`Key`), full-range (`RangeFull`), and self-nested (`AggregateSumOnRange`)
variants are rejected — and `AggregateSumOnRange` may not wrap an
`AggregateCountOnRange` either.

> **Why are `Key` and `RangeFull` rejected?**
>
> - **`Key(k)`** would return either `0` or the single child's sum
>   contribution — degenerate cases the existing `get_raw` /
>   `verify_query_with_options` paths already handle more cheaply.
> - **`RangeFull`** has its answer already exposed by the parent's
>   `Element::ProvableSumTree(_, sum, _)` bytes, which are hash-verified by
>   the parent Merk's proof. Going through `AggregateSumOnRange(RangeFull)`
>   would always produce a strictly heavier proof for an answer the caller
>   can read directly.

## Why this works only on ProvableSumTree

GroveDB has several tree types that track a sum:

| Tree type                        | Sum tracked? | Sum in node hash? | AggregateSumOnRange allowed? |
|----------------------------------|:------------:|:-----------------:|:---------------------------:|
| `SumTree`                        | yes          | no                | **no**                      |
| `BigSumTree`                     | yes (i128)   | no                | **no**                      |
| `CountSumTree`                   | yes          | no                | **no**                      |
| `ProvableCountSumTree`           | yes          | no (count only)   | **no**                      |
| `ProvableSumTree`                | yes          | **yes**           | **yes**                     |
| `NonCountedProvableSumTree`      | yes (inner)  | yes (inner)       | **yes**                     |
| `NotSummedProvableSumTree`       | yes (inner)  | yes (inner)       | **yes**                     |

Only `ProvableSumTree` bakes the per-node sum into the node hash via
`node_hash_with_sum(kv_hash, left, right, sum)`. Because every node's sum
participates in the Merkle root, a verifier holding only the root hash can
reconstruct enough of the tree from a proof to **trust** the sums embedded
in it.

`SumTree`, `BigSumTree`, `CountSumTree`, and `ProvableCountSumTree` all
track sums in storage, but those sums are not committed in the node hash
chain. (For `ProvableCountSumTree`, the count is in the hash but the sum
is not.) A "proof" of those sums would be unverifiable, so we reject
`AggregateSumOnRange` against them at query-construction time.

The wrapper variants are accepted because the wrapper only changes how the
**parent** aggregates this element — the inner is still a fully-fledged
`ProvableSumTree`.

> **Why not `BigSumTree`?** `BigSumTree` uses `i128` sums and would need a
> separate hash dispatch (`node_hash_with_big_sum`) plus a different verify
> path. It is a documented follow-up, not part of this PR.

## Query-Level Constraints

`AggregateSumOnRange` is a **terminal** query item. Its presence reduces
the enclosing `Query` to a single, well-defined operation: "sum, then
return."

If any `QueryItem::AggregateSumOnRange(_)` appears in `Query::items`, the
query is well-formed only when:

1. `items.len() == 1` — no other items, no other sums, no mixing with
   `AggregateCountOnRange`.
2. The inner `QueryItem` is **not** `Key`, `RangeFull`, or another
   `AggregateSumOnRange` / `AggregateCountOnRange`.
3. `default_subquery_branch.subquery.is_none()` and
   `subquery_path.is_none()`.
4. `conditional_subquery_branches.is_none()` (or empty).
5. The targeted subtree's `TreeType` is `ProvableSumTree`.
6. The enclosing `SizedQuery` does not set `limit` or `offset`. Summing
   is aggregate over the matched range — pagination would silently change
   the answer and is rejected.
7. `left_to_right` is **ignored** (summing is direction-agnostic).

Violations return `Error::InvalidQuery(...)` before any I/O.

## API Surface

```rust
// Prove side — unchanged from regular queries:
GroveDb::prove_query(&path_query, prove_options, grove_version)
    -> CostResult<Vec<u8>, Error>

// Verify side — dedicated, returns (root_hash, sum):
GroveDb::verify_aggregate_sum_query(proof, &path_query, grove_version)
    -> Result<(CryptoHash, i64), Error>
```

A bare tuple is used rather than a wrapper struct: the sum is already an
`i64` and the `path_query` echoes the inner range.

> **Note on `NonCounted` and `NotSummed` children.** An
> `Element::NotSummed(child)` wrapper tells the parent sum tree to skip the
> wrapped element when aggregating its own sum. `AggregateSumOnRange`
> honors this: every node in a `ProvableSumTree` carries an own-sum equal
> to its own `SumItem` value or `0` if `NotSummed`-wrapped. The verifier
> credits only the **own-sum** to the in-range total when the boundary key
> falls in range. `NonCounted` is orthogonal to sums — it suppresses count
> aggregation, not sum aggregation — so a `NonCounted` `SumItem` still
> contributes its sum value normally.

## Proof Node Vocabulary

For `ProvableSumTree`, every node hash commits to its subtree's aggregate
sum via `node_hash_with_sum(kv_hash, left, right, sum)`. The proof-node
vocabulary is parallel to the count family, with new variants carrying an
`i64` sum field in place of the `u64` count:

| Role in proof              | Proof node type                                                              | What it carries                                                |
|----------------------------|------------------------------------------------------------------------------|----------------------------------------------------------------|
| **On-path / boundary**     | `KVDigestSum(key, value_hash, sum)`                                          | key + value digest + subtree sum                              |
| **Fully-inside / outside** | `HashWithSum(kv_hash, left_hash, right_hash, sum)`                           | the four fields needed to recompute `node_hash_with_sum`       |
| **Queried boundary item**  | `KVSum(key, value, sum)`                                                     | leaf value at a boundary key, with subtree sum                 |
| **Empty side**             | (the empty-tree sentinel, no `Push` needed)                                  | —                                                              |

Wire format tag bytes (V1 only): `0x30..=0x3D` for the push and
push-inverted variants. The on-the-wire sum field is `varint i64` (not
fixed-width) for compactness; the **hash input** to `node_hash_with_sum`
uses fixed 8-byte big-endian — wire and hash are deliberately decoupled.

> **Why `HashWithSum` is self-verifying.** The `sum` value carried by a
> `HashWithSum` op is *bound* to the parent merk's hash chain, not
> trusted on faith. The verifier recomputes
> `node_hash_with_sum(kv_hash, left, right, sum)` from the four fields
> and uses the result as the subtree's committed `node_hash` for the
> parent's hash recomputation. If the prover lies about `sum`, the
> recomputed `node_hash` diverges from what the parent committed, and the
> parent's Merkle-root check fails.

The walk-by-example diagrams from
[Aggregate Count on Range Queries](aggregate-count-queries.md) apply
unchanged — substitute `KVDigestCount` → `KVDigestSum` and
`HashWithCount` → `HashWithSum`.

## Signed-Sum Arithmetic

Two correctness points differ from the count machinery:

### Negative sums

A `ProvableSumTree` can hold negative `SumItem` values, and a range can
sum to a negative or zero total. Two consequences:

- **No `if sum == 0` short-circuit.** The count generator can skip an
  empty subtree (count = 0 means "no elements"), but `sum == 0` does
  **not** mean "no elements" — it can mean "+5 and -5 cancelled". The
  sum prover descends regardless.
- **No `own_sum = aggregate − left_struct − right_struct` overflow
  check.** Count uses `checked_sub` to catch "children claim more than
  parent" as corruption. Signed sums can naturally have children's
  structural sums in any combination (`+200 + -150 = +50`), so the
  subtraction is allowed to wrap. The hash chain still binds every
  node, so arithmetic corruption changes the reconstructed root hash
  and the caller's root check catches it.

### i64 overflow at extremes

A sum of two `i64::MAX` children does **not** fit in `i64`. The verify
path accumulates in `i128` end-to-end:

- The prover's internal recursion (`emit_sum_proof`) returns
  `CostResult<i128, Error>`.
- The verifier's `verify_sum_shape` accumulates into an `i128`.
- Both narrow to `i64` at the **outermost entry point** via
  `i64::try_from(sum_i128)`, returning `Error::InvalidProofError` if
  the i128 result doesn't fit.

Tests cover the two interesting overflow shapes:

- `i64::MAX + i64::MAX` → overflows i64, verify rejects with
  `InvalidProofError`.
- `i64::MAX + i64::MIN` → `-1`, fits i64, verify succeeds. The
  intermediate i128 carries the difference safely.

## Tests and Examples

See:

- `grovedb/src/tests/aggregate_sum_query_tests.rs` — 21 end-to-end
  GroveDB tests.
- `merk/src/proofs/query/aggregate_sum.rs` — 14 Merk-level tests
  (classification, prover internals, single-`Hash` rejection,
  disjoint-with-children rejection, overflow at i64::MAX).
- `grovedb/src/operations/proof/aggregate_sum.rs` — V0/V1 envelope walker
  with layer-chain validation.

The marquee scenarios:

| Scenario                                              | Result                              |
|-------------------------------------------------------|-------------------------------------|
| Full range over `[1..=15]`                            | sum = 120                           |
| Subrange `[5..=10]`                                   | sum = 45                            |
| Mixed `+50, -100, +30, -50`                           | sum = -70                           |
| All-negative subrange                                 | sum = -10                           |
| `+5, -5` (non-zero children, zero sum)                | sum = 0 (no short-circuit)          |
| `i64::MAX + i64::MAX`                                 | `Error::InvalidProofError`          |
| `i64::MAX + i64::MIN`                                 | sum = -1                            |
| Tampered `HashWithSum::sum`                           | rejected (root-hash divergence)     |
| `NotSummed(SumItem)` in range                         | excluded (matches tree's aggregate) |
| Query with subquery / pagination / mixed aggregates   | rejected at validation              |

## See Also

- [Element System](element-system.md) — the `ProvableSumTree` element
  variant and `ProvableSummedMerkNode` feature type.
- [Aggregate Count on Range Queries](aggregate-count-queries.md) — the
  symmetric count-only feature; most of the proof-shape walk diagrams
  apply unchanged.
- [Aggregate Sum Queries](aggregate-sum-queries.md) — the existing
  sum-budget iterator (a different feature with a similar name).
- [Hashing](hashing.md) — `node_hash_with_sum` and the broader
  hash-binding scheme.
