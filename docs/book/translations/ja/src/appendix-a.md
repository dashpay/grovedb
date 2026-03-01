# 付録A：エレメント型完全リファレンス

| 判別子 | バリアント | TreeType | フィールド | コストサイズ | 目的 |
|---|---|---|---|---|---|
| 0 | `Item` | N/A | `(value, flags)` | 可変 | 基本的なキーバリューストレージ |
| 1 | `Reference` | N/A | `(path, max_hop, flags)` | 可変 | エレメント間のリンク |
| 2 | `Tree` | 0 (NormalTree) | `(root_key, flags)` | TREE_COST_SIZE | サブツリーのコンテナ |
| 3 | `SumItem` | N/A | `(value, flags)` | 可変 | 親の合計に寄与 |
| 4 | `SumTree` | 1 (SumTree) | `(root_key, sum, flags)` | SUM_TREE_COST_SIZE | 子孫の合計を管理 |
| 5 | `BigSumTree` | 4 (BigSumTree) | `(root_key, sum128, flags)` | BIG_SUM_TREE_COST_SIZE | 128ビット合計ツリー |
| 6 | `CountTree` | 2 (CountTree) | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | エレメントカウントツリー |
| 7 | `CountSumTree` | 3 (CountSumTree) | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | カウント + 合計の複合 |
| 8 | `ItemWithSumItem` | N/A | `(value, sum, flags)` | 可変 | 合計寄与付きアイテム |
| 9 | `ProvableCountTree` | 5 | `(root_key, count, flags)` | COUNT_TREE_COST_SIZE | 証明可能カウントツリー |
| 10 | `ProvableCountSumTree` | 6 | `(root_key, count, sum, flags)` | COUNT_SUM_TREE_COST_SIZE | 証明可能カウント + 合計 |
| 11 | `CommitmentTree` | 7 | `(total_count: u64, chunk_power: u8, flags)` | 12 | ZK フレンドリーな Sinsemilla + BulkAppendTree |
| 12 | `MmrTree` | 8 | `(mmr_size: u64, flags)` | 11 | 追記専用 MMR ログ |
| 13 | `BulkAppendTree` | 9 | `(total_count: u64, chunk_power: u8, flags)` | 12 | 高スループット追記専用ログ |
| 14 | `DenseAppendOnlyFixedSizeTree` | 10 | `(count: u16, height: u8, flags)` | 6 | 密な固定容量マークルストレージ |

**注記：**
- 判別子 11〜14 は**非 Merk ツリー**です：データは子 Merk サブツリーの外に存在します
  - 4つすべてが非 Merk データを**データ**カラムに格納します
  - `CommitmentTree` は Sinsemilla フロンティアを BulkAppendTree エントリと同じデータカラムに格納します（キー `b"__ct_data__"`）
- 非 Merk ツリーは `root_key` フィールドを持ちません — 型固有のルートハッシュは `insert_subtree` を通じて Merk の子ハッシュとして流れます
- `CommitmentTree` は Sinsemilla ハッシュ（Pallas 曲線）を使用します；他のすべては Blake3 を使用します
- 非 Merk ツリーのコスト動作は `NormalTree` に従います（BasicMerkNode、集約なし）
- `DenseAppendOnlyFixedSizeTree` のカウントは `u16`（最大65,535）であり、高さは1..=16に制限されます

---
