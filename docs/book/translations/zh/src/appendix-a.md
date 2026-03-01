# 附录 A：完整元素类型参考

| 判别值 | 变体 | TreeType | 字段 | 开销大小 | 用途 |
|---|---|---|---|---|---|
| 0 | `Item` | 不适用 | `(value, flags)` | 可变 | 基本键值存储 |
| 1 | `Reference` | 不适用 | `(path, max_hop, flags)` | 可变 | 元素间的链接 |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | 子树容器 |
| 3 | `SumItem` | 不适用 | `(value, flags)` | 可变 | 贡献给父级求和 |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | 维护后代元素之和 |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128 位求和树 |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | 元素计数树 |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | 计数 + 求和组合 |
| 8 | `ItemWithSumItem` | 不适用 | `(value, sum, flags)` | 可变 | 带求和贡献的项 |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | 可证明计数树 |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | 可证明计数 + 求和 |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK 友好的 Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | 仅追加 MMR 日志 |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | 高吞吐量仅追加日志 |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | 稠密固定容量默克尔存储 |

**注意事项：**
- 判别值 11-14 是**非 Merk 树**：数据存储在子 Merk 子树之外
  - 所有四种类型都将非 Merk 数据存储在**数据**列中
  - `CommitmentTree` 将其 Sinsemilla 前沿与 BulkAppendTree 条目一起存储在同一数据列中（键 `b"__ct_data__"`）
- 非 Merk 树没有 `root_key` 字段 — 它们的类型特定根哈希通过 `insert_subtree` 作为 Merk 子哈希流动
- `CommitmentTree` 使用 Sinsemilla 哈希（Pallas 曲线）；其他所有类型使用 Blake3
- 非 Merk 树的开销行为遵循 `NormalTree`（BasicMerkNode，无聚合）
- `DenseAppendOnlyFixedSizeTree` 的 count 是 `u16`（最大 65,535）；高度限制为 1..=16

---
