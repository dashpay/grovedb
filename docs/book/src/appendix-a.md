# Appendix A: Complete Element Type Reference

> **Reading the table.** "Element disc" is the bincode discriminant of
> the `Element` enum (one byte; persisted at the start of every
> serialized element). "TreeType disc" is the discriminant of the
> *separate* `TreeType` enum in `merk/src/tree_type/mod.rs` — it is NOT
> the same numbering. Most rows list both the TreeType disc and its
> variant name (e.g. `0 (NormalTree)`) to keep the distinction obvious;
> `N/A` means the Element variant is not a tree.

| Element disc | Variant | TreeType disc | Fields | Cost Size | Purpose |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | varies | Basic key-value storage |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | varies | Link between elements |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | Container for subtrees |
| 3 | `SumItem` | N/A | `(value, flags)` | varies | Contributes to parent sum |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | Maintains sum of descendants |
| 5 | `BigSumTree` | 2 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128-bit sum tree |
| 6 | `CountTree` | 3 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Element counting tree |
| 7 | `CountSumTree` | 4 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Combined count + sum |
| 8 | `ProvableCountTree` | 5 (ProvableCountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | Provable count tree |
| 9 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | varies | Item with sum contribution |
| 10 | `ProvableCountSumTree` | 6 (ProvableCountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | Provable count + sum (only count in hash) |
| 11 | `CommitmentTree` | 7 (CommitmentTree) | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK-friendly Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 (MmrTree) | `(mmr_size: u64, flags)` | 11 | Append-only MMR log |
| 13 | `BulkAppendTree` | 9 (BulkAppendTree) | `(total_count: u64, chunk_power: u8, flags)` | 12 | High-throughput append-only log |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 (DenseAppendOnlyFixedSizeTree) | `(count: u16, height: u8, flags)` | 6 | Dense fixed-capacity Merkle storage |
| 15 | *(NonCounted wrapper byte)* | — | inner element bytes follow | varies | On-disk wrapper for `Element::NonCounted`; `from_serialized_value` reads the next byte to resolve the inner type and returns the matching `NonCountedXxx` synthetic discriminant (high bit set). |
| 16 | *(NotSummed wrapper byte)* | — | inner element bytes follow | varies | On-disk wrapper for `Element::NotSummed`; analogous to byte 15 but resolves to a `NotSummedXxx` twin (`0xb0 \| base`) for the four sum-bearing tree variants only. |
| 17 | *(NotCountedOrSummed wrapper byte)* | — | inner element bytes follow | varies | On-disk wrapper for `Element::NotCountedOrSummed`; resolves to a `NotCountedOrSummedXxx` twin for the four sum-bearing tree variants only. |
| 18 | `ReferenceWithSumItem` | N/A | `(path, max_hop, sum, flags)` | varies | Reference + explicit sum contribution |
| 19 | `ProvableSumTree` | 11 (ProvableSumTree) | `(root_key, sum: i64, flags)` | SUM_TREE_COST_SIZE | Sum baked into hash (see [Aggregate Sum on Range Queries](aggregate-sum-on-range-queries.md)) |
| 20 | `CountIndexedTree` | 12 (CountIndexedTree) | `(primary_root_key, secondary_root_key, count, flags)` | 13 | CountTree-shaped primary + count-ordered secondary index — see chapter "The CountIndexedTree" |
| 21 | `ProvableCountIndexedTree` | 13 (ProvableCountIndexedTree) | `(primary_root_key, secondary_root_key, count, flags)` | 13 | ProvableCountTree-shaped primary + count-ordered secondary index |

**Notes:**
- Discriminants 11–14 are **non-Merk trees**: data lives outside a child Merk subtree
  - All four store non-Merk data in the **data** column
  - `CommitmentTree` stores its Sinsemilla frontier alongside BulkAppendTree entries in the same data column (key `b"__ct_data__"`)
- Non-Merk trees do NOT have a `root_key` field — their type-specific root hash flows as the Merk child hash via `insert_subtree`
- `CommitmentTree` uses Sinsemilla hashing (Pallas curve); all others use Blake3
- Cost behavior for non-Merk trees follows `NormalTree` (BasicMerkNode, no aggregation)
- `DenseAppendOnlyFixedSizeTree` count is `u16` (max 65,535); heights restricted to 1..=16
- Discriminants 17 and 18 (`CountIndexedTree` / `ProvableCountIndexedTree`) carry **two** root keys (primary + secondary) and use the three-input value-hash composition `Blake3(actual_value_hash ‖ primary_root_hash ‖ secondary_root_hash)` — see chapter "The CountIndexedTree". Their variant index in the `Element` enum is positioned AFTER the `NotSummed` wrapper variant so the bincode-encoded variant index matches the ElementType discriminant.
- `NonCounted` synthetic discriminants (`NonCountedXxx`) are 128–142 (= 0x80 | base) for inner discriminants 0–14, plus 145 (`NonCountedCountIndexedTree` = 0x80 \| 17) and 146 (`NonCountedProvableCountIndexedTree` = 0x80 \| 18). Discriminants 143 and 144 are unallocated by construction (their inner-byte slots are the wrapper bytes 15 and 16).
- `NotSummed` synthetic discriminants (`NotSummedXxx`) are 180, 181, 183, 186 (= 0xb0 | base) for the four sum-bearing tree variants only (`SumTree`, `BigSumTree`, `CountSumTree`, `ProvableCountSumTree`).
- `CountIndexedTree` item keys (the keys inserted into a cidx primary's content) are capped at **247 bytes**. The secondary key is `count_be (8 bytes) ‖ item_key`, and Merk's invariant requires keys `< 256 bytes` — so cidx primaries have an 8-byte stricter ceiling than the generic 255-byte limit.

---
