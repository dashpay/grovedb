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
| 20 | `ProvableCountProvableSumTree` | 12 (ProvableCountProvableSumTree) | `(root_key, count: u64, sum: i64, flags)` | COUNT_SUM_TREE_COST_SIZE | Count and sum both baked into the hash |
| 21 | `ProvableSumIndexedTree` | 13 (ProvableSumIndexedTree) | `(primary_root_key, secondary_root_key, sum: i64, flags)` | 13 | ProvableSumTree-shaped primary + sum-ordered secondary index — see chapter "Indexed Trees" |
| 22 | `ProvableCountIndexedTree` | 14 (ProvableCountIndexedTree) | `(primary_root_key, secondary_root_key, count: u64, flags)` | 13 | ProvableCountTree-shaped primary + count-ordered secondary index |
| 23 | `ProvableCountProvableSumIndexedTree` | 15 (ProvableCountProvableSumIndexedTree) | `(primary_root_key, count: u64, sum: i64, axes: TLV, flags)` | 13 | ProvableCountProvableSumTree-shaped primary + 1..=3 secondary indexes (count / sum / avg) |

**Notes:**
- Discriminants 11–14 are **non-Merk trees**: data lives outside a child Merk subtree
  - All four store non-Merk data in the **data** column
  - `CommitmentTree` stores its Sinsemilla frontier alongside BulkAppendTree entries in the same data column (key `b"__ct_data__"`)
- Non-Merk trees do NOT have a `root_key` field — their type-specific root hash flows as the Merk child hash via `insert_subtree`
- `CommitmentTree` uses Sinsemilla hashing (Pallas curve); all others use Blake3
- Cost behavior for non-Merk trees follows `NormalTree` (BasicMerkNode, no aggregation)
- `DenseAppendOnlyFixedSizeTree` count is `u16` (max 65,535); heights restricted to 1..=16
- Discriminants 21–23 are the **indexed trees**. `ProvableSumIndexedTree` and `ProvableCountIndexedTree` carry **two** root keys (primary + secondary) and use the three-input value-hash composition `Blake3(actual_value_hash ‖ primary_root_hash ‖ secondary_root_hash)`. `ProvableCountProvableSumIndexedTree` carries one primary root key plus a canonical axes TLV (1..=3 entries, sorted by tag, tags in 0..=2 = count/sum/avg), and composes `Blake3(actual_value_hash ‖ primary_root_hash ‖ axes_digest)` where `axes_digest = Blake3(axis_count_u8 ‖ (axis_tag_u8 ‖ secondary_root_hash_32)*)`. See chapter "Indexed Trees". Their variant index in the `Element` enum is positioned AFTER the `NotSummed` wrapper variant so the bincode-encoded variant index matches the ElementType discriminant.
- There is no `CountIndexedTree` (non-provable) variant: an earlier draft introduced one at byte 20, but indexed trees ship provable-only and byte 20 now holds `ProvableCountProvableSumTree`.
- `NonCounted` synthetic discriminants (`NonCountedXxx`) are 128–142 (= 0x80 | base) for inner discriminants 0–14, plus 146–151 (= 0x80 | base) for inner discriminants 18–23 (`NonCountedReferenceWithSumItem` = 146 through `NonCountedProvableCountProvableSumIndexedTree` = 151). Discriminants 143, 144 and 145 are unallocated by construction (their inner-byte slots are the wrapper bytes 15, 16 and 17).
- `NotSummed` synthetic discriminants (`NotSummedXxx`) are 180, 181, 183, 186 (= 0xb0 | base) for `SumTree`, `BigSumTree`, `CountSumTree` and `ProvableCountSumTree`, plus the two hand-assigned twins 177 (`NotSummedProvableSumTree`) and 178 (`NotSummedProvableCountProvableSumTree`), which cannot use the `0xb0 | base` formula because their base discriminants would collide.
- Indexed-tree item keys (the keys inserted into an indexed primary's content) are capped below the generic 255-byte limit because the secondary key prefixes a sort key: **247 bytes** for the count axis (`count_be`, 8 bytes) and the sum axis (`sum` sign-flipped big-endian, 8 bytes), and **239 bytes** for the avg axis (`avg` fixed-point i128, 16 bytes). Merk's invariant requires keys `< 256 bytes`.

---
