# BulkAppendTree — 高スループットの追記専用ストレージ

BulkAppendTree は、特定のエンジニアリング課題に対する GroveDB の解決策です：効率的な範囲証明をサポートし、書き込みあたりのハッシュを最小化し、CDN 配信に適した不変チャンクスナップショットを生成する高スループットの追記専用ログをどのように構築するか？

MmrTree（第13章）は個別のリーフ証明に理想的ですが、BulkAppendTree はブロックあたり数千の値が到着し、クライアントがデータの範囲をフェッチして同期する必要があるワークロード向けに設計されています。これは**2レベルアーキテクチャ**で実現されます：受信した追記を吸収する密なマークル木バッファと、確定したチャンクルートを記録するチャンクレベルの MMR です。

## 2レベルアーキテクチャ

```text
┌────────────────────────────────────────────────────────────────┐
│                      BulkAppendTree                            │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Chunk MMR                                               │  │
│  │  ┌────┐ ┌────┐ ┌────┐ ┌────┐                            │  │
│  │  │ R0 │ │ R1 │ │ R2 │ │ H  │ ← Dense Merkle roots      │  │
│  │  └────┘ └────┘ └────┘ └────┘   of each chunk blob       │  │
│  │                     peak hashes bagged together = MMR root│  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Buffer (DenseFixedSizedMerkleTree, capacity = 2^h - 1) │  │
│  │  ┌───┐ ┌───┐ ┌───┐                                      │  │
│  │  │v_0│ │v_1│ │v_2│ ... (fills in level-order)           │  │
│  │  └───┘ └───┘ └───┘                                      │  │
│  │  dense_tree_root = recomputed root hash of dense tree     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                │
│  state_root = blake3("bulk_state" || mmr_root || dense_tree_root) │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

**レベル1 — バッファ。** 受信した値は `DenseFixedSizedMerkleTree`（第16章参照）に書き込まれます。バッファ容量は `2^height - 1` 位置です。密なツリーのルートハッシュ（`dense_tree_root`）は挿入のたびに更新されます。

**レベル2 — チャンク MMR。** バッファが満杯になると（`chunk_size` エントリに達すると）、すべてのエントリが不変の**チャンクブロブ**にシリアライズされ、それらのエントリの密なマークルルートが計算され、そのルートがチャンク MMR にリーフとして追加されます。その後バッファはクリアされます。

**ステートルート**は両方のレベルを1つの32バイトコミットメントに結合し、追記のたびに変更されるため、親 Merk ツリーが常に最新の状態を反映します。

## 値がバッファにどのように充填されるか

`append()` の各呼び出しは以下のシーケンスに従います：

```text
Step 1: Write value to dense tree buffer at next position
        dense_tree.insert(value, store)

Step 2: Increment total_count
        total_count += 1

Step 3: Check if buffer is full (dense tree at capacity)
        if dense_tree.count() == capacity:
            → trigger compaction (§14.3)

Step 4: Compute new state root (+1 blake3 call)
        dense_tree_root = dense_tree.root_hash(store)
        state_root = blake3("bulk_state" || mmr_root || dense_tree_root)
```

**バッファは DenseFixedSizedMerkleTree そのもの**です（第16章参照）。そのルートハッシュは挿入のたびに変更され、すべての現在のバッファエントリへのコミットメントを提供します。このルートハッシュがステートルート計算に流れ込みます。

## チャンクコンパクション

バッファが満杯になると（`chunk_size` エントリに達すると）、コンパクションが自動的に発火します：

```text
Compaction Steps:
─────────────────
1. Read all chunk_size buffer entries

2. Compute dense Merkle root
   - Hash each entry: leaf[i] = blake3(entry[i])
   - Build complete binary tree bottom-up
   - Extract root hash
   Hash cost: chunk_size + (chunk_size - 1) = 2 * chunk_size - 1

3. Serialize entries into chunk blob
   - Auto-selects fixed-size or variable-size format (§14.6)
   - Store as: store.put(chunk_key(chunk_index), blob)

4. Append dense Merkle root to chunk MMR
   - MMR push with merge cascade (see Chapter 13)
   Hash cost: ~2 amortized (trailing_ones pattern)

5. Reset the dense tree (clear all buffer entries from storage)
   - Dense tree count reset to 0
```

コンパクション後、チャンクブロブは**永久に不変**です — 二度と変更されません。これによりチャンクブロブは CDN キャッシング、クライアント同期、アーカイブストレージに最適になります。

**例：chunk_power=2（chunk_size=4）での4回の追記**

```text
Append v_0: dense_tree=[v_0],       dense_root=H(v_0), total=1
Append v_1: dense_tree=[v_0,v_1],   dense_root=H(v_0,v_1), total=2
Append v_2: dense_tree=[v_0..v_2],  dense_root=H(v_0..v_2), total=3
Append v_3: dense_tree=[v_0..v_3],  dense_root=H(v_0..v_3), total=4
  → COMPACTION:
    chunk_blob_0 = serialize([v_0, v_1, v_2, v_3])
    dense_root_0 = dense_merkle_root(v_0..v_3)
    mmr.push(dense_root_0)
    dense tree cleared (count=0)

Append v_4: dense_tree=[v_4],       dense_root=H(v_4), total=5
  → state_root = blake3("bulk_state" || mmr_root || dense_root)
```

## ステートルート

ステートルートは両レベルを1つのハッシュにバインドします：

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` と `chunk_power` はステートルートに含まれません。それらは既に Merk バリューハッシュで認証されているためです — 親 Merk ノードに格納されたシリアライズされた `Element` のフィールドです。ステートルートはデータレベルのコミットメント（`mmr_root` と `dense_tree_root`）のみを捕捉します。これが Merk 子ハッシュとして流れ、GroveDB ルートハッシュまで伝播するハッシュです。

## 密なマークルルート

チャンクがコンパクトされると、エントリには単一の32バイトコミットメントが必要です。BulkAppendTree は**密な（完全な）二分マークル木**を使用します：

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

`chunk_size` は常に2の冪（構造上：`1u32 << chunk_power`）であるため、ツリーは常に完全です（パディングやダミーリーフは不要）。ハッシュ数は正確に `2 * chunk_size - 1` です：
- `chunk_size` 個のリーフハッシュ（エントリごとに1つ）
- `chunk_size - 1` 個の内部ノードハッシュ

密なマークルルートの実装は `grovedb-mmr/src/dense_merkle.rs` にあり、2つの関数を提供します：
- `compute_dense_merkle_root(hashes)` — 事前ハッシュされたリーフから
- `compute_dense_merkle_root_from_values(values)` — 値を最初にハッシュしてからツリーを構築

## チャンクブロブのシリアライズ

チャンクブロブはコンパクションによって生成される不変アーカイブです。シリアライザはエントリサイズに基づいて最もコンパクトなワイヤフォーマットを自動選択します：

**固定サイズフォーマット**（フラグ `0x01`）— すべてのエントリが同じ長さの場合：

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**可変サイズフォーマット**（フラグ `0x00`）— エントリの長さが異なる場合：

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

固定サイズフォーマットは可変サイズと比較してエントリあたり4バイトを節約します。均一サイズのデータ（32バイトのハッシュコミットメントなど）の大きなチャンクでは大幅に積み上がります。32バイトの1024エントリの場合：
- 固定：`1 + 4 + 4 + 32768 = 32,777 bytes`
- 可変：`1 + 1024 × (4 + 32) = 36,865 bytes`
- 節約：~11%

## ストレージキーレイアウト

すべての BulkAppendTree データは単一文字プレフィックスをキーとして**データ**名前空間に存在します：

| キーパターン | フォーマット | サイズ | 目的 |
|---|---|---|---|
| `M` | 1バイト | 1B | メタデータキー |
| `b` + `{index}` | `b` + u32 BE | 5B | インデックスのバッファエントリ |
| `e` + `{index}` | `e` + u64 BE | 9B | インデックスのチャンクブロブ |
| `m` + `{pos}` | `m` + u64 BE | 9B | 位置の MMR ノード |

**メタデータ**は `mmr_size`（8バイト BE）を格納します。`total_count` と `chunk_power` はデータ名前空間のメタデータではなく、エレメント自体（親 Merk 内）に格納されます。この分割により、カウントの読み取りがデータストレージコンテキストを開くことなくシンプルなエレメントルックアップで済みます。

バッファキーは u32 インデックス（0 から `chunk_size - 1`）を使用します。バッファ容量が `chunk_size`（u32、`1u32 << chunk_power` として計算）で制限されるためです。チャンクキーは u64 インデックスを使用します。完了したチャンクの数は無限に増加する可能性があるためです。

## BulkAppendTree 構造体

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

バッファは `DenseFixedSizedMerkleTree` そのもの — そのルートハッシュが `dense_tree_root` です。

**アクセサ：**
- `capacity() -> u16`：`dense_tree.capacity()`（= `2^height - 1`）
- `epoch_size() -> u64`：`capacity + 1`（= `2^height`、チャンクあたりのエントリ数）
- `height() -> u8`：`dense_tree.height()`

**導出値**（格納されない）：

| 値 | 式 |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## GroveDB 操作

BulkAppendTree は `grovedb/src/operations/bulk_append_tree.rs` で定義された6つの操作を通じて GroveDB と統合されます：

### bulk_append

主要な変更操作です。標準的な GroveDB 非 Merk ストレージパターンに従います：

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

### 読み取り操作

| 操作 | 返すもの | 補助ストレージ？ |
|---|---|---|
| `bulk_get_value(path, key, position)` | グローバル位置の値 | はい — チャンクブロブまたはバッファから読み取り |
| `bulk_get_chunk(path, key, chunk_index)` | 生のチャンクブロブ | はい — チャンクキーを読み取り |
| `bulk_get_buffer(path, key)` | すべての現在のバッファエントリ | はい — バッファキーを読み取り |
| `bulk_count(path, key)` | 合計カウント (u64) | いいえ — エレメントから読み取り |
| `bulk_chunk_count(path, key)` | 完了チャンク (u64) | いいえ — エレメントから計算 |

`get_value` 操作は位置によって透過的にルーティングします：

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## バッチ操作と前処理

BulkAppendTree は `GroveOp::BulkAppend` バリアントを通じてバッチ操作をサポートします。`execute_ops_on_path` はデータストレージコンテキストへのアクセスがないため、すべての BulkAppend 操作は `apply_body` の前に前処理される必要があります。

前処理パイプライン：

```text
Input: [BulkAppend{v1}, Insert{...}, BulkAppend{v2}, BulkAppend{v3}]
                                     ↑ same (path,key) as v1

Step 1: Group BulkAppend ops by (path, key)
        group_1: [v1, v2, v3]

Step 2: For each group:
        a. Read existing element → get (total_count, chunk_power)
        b. Open transactional storage context
        c. Load BulkAppendTree from store
        d. Load existing buffer into memory (Vec<Vec<u8>>)
        e. For each value:
           tree.append_with_mem_buffer(store, value, &mut mem_buffer)
        f. Save metadata
        g. Compute final state_root

Step 3: Replace all BulkAppend ops with one ReplaceNonMerkTreeRoot per group
        carrying: hash=state_root, meta=BulkAppendTree{total_count, chunk_power}

Output: [ReplaceNonMerkTreeRoot{...}, Insert{...}]
```

`append_with_mem_buffer` バリアントはリードアフターライトの問題を回避します：バッファエントリはメモリ内の `Vec<Vec<u8>>` で追跡されるため、トランザクショナルストレージがまだコミットされていなくても、コンパクションがそれらを読み取ることができます。

## BulkStore トレイト

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

メソッドは `&mut self` ではなく `&self` を取ります。これは書き込みがバッチを通じて行われる GroveDB の内部可変性パターンに合わせるためです。GroveDB 統合は `AuxBulkStore` を通じてこれを実装し、`StorageContext` をラップして `OperationCost` を蓄積します。

`MmrAdapter` は `BulkStore` を ckb MMR の `MMRStoreReadOps`/`MMRStoreWriteOps` トレイトにブリッジし、リードアフターライトの正確性のためにライトスルーキャッシュを追加します。

## 証明生成

BulkAppendTree 証明は位置に対する**範囲クエリ**をサポートします。証明構造は、ステートレスな検証者が特定のデータがツリー内に存在することを確認するために必要なすべてを含みます。

## 証明検証

検証は純粋な関数 — データベースアクセスは不要です。5つのチェックを実行します：

```text
Step 0: Metadata consistency
        - chunk_power <= 31
        - buffer_entries.len() == total_count % chunk_size
        - MMR leaf count == completed_chunks

Step 1: Chunk blob integrity
        For each (chunk_idx, blob):
          values = deserialize(blob)
          computed_root = dense_merkle_root(values)
          assert computed_root == chunk_mmr_leaves[chunk_idx]

Step 2: Chunk MMR proof
        Reconstruct MmrNode leaves and proof items
        proof.verify(chunk_mmr_root, leaves) == true

Step 3: Buffer (dense tree) integrity
        Rebuild DenseFixedSizedMerkleTree from buffer_entries
        dense_tree_root = compute root hash of rebuilt tree

Step 4: State root
        computed = blake3("bulk_state" || chunk_mmr_root || dense_tree_root)
        assert computed == expected_state_root
```

## GroveDB ルートハッシュへの接続

BulkAppendTree は**非 Merk ツリー**です — データ名前空間にデータを格納し、子 Merk サブツリーには格納しません。親 Merk では、エレメントは以下のように格納されます：

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

ステートルートは Merk 子ハッシュとして流れます。親 Merk ノードハッシュは：

```text
combine_hash(value_hash(element_bytes), state_root)
```

## コスト追跡

各操作のハッシュコストは明示的に追跡されます：

| 操作 | Blake3 呼び出し | 備考 |
|---|---|---|
| 単一追記（コンパクションなし） | 3 | バッファハッシュチェーン2 + ステートルート1 |
| 単一追記（コンパクションあり） | 3 + 2C - 1 + ~2 | チェーン + 密なマークル(C=chunk_size) + MMR push + ステートルート |
| チャンクからの `get_value` | 0 | 純粋なデシリアライズ、ハッシュなし |
| バッファからの `get_value` | 0 | 直接キールックアップ |

**追記あたりの償却コスト**：chunk_size=1024（chunk_power=10）の場合、~2047ハッシュのコンパクションオーバーヘッドは1024回の追記に償却され、追記あたり~2ハッシュを追加します。追記ごとの3ハッシュと合わせて、償却合計は**追記あたり~5回の blake3 呼び出し**です — 暗号学的に認証された構造としては非常に効率的です。

## MmrTree との比較

| | BulkAppendTree | MmrTree |
|---|---|---|
| **アーキテクチャ** | 2レベル（バッファ + チャンク MMR） | 単一 MMR |
| **追記あたりのハッシュコスト** | 3（+ 償却~2のコンパクション） | ~2 |
| **証明の粒度** | 位置に対する範囲クエリ | 個別リーフ証明 |
| **不変スナップショット** | あり（チャンクブロブ） | なし |
| **CDN対応** | あり（チャンクブロブはキャッシュ可能） | なし |
| **最適な用途** | 高スループットログ、一括同期 | イベントログ、個別検索 |
| **エレメント discriminant** | 13 | 12 |
| **TreeType** | 9 | 8 |

最小限のオーバーヘッドで個別リーフ証明が必要な場合は MmrTree を選択してください。範囲クエリ、一括同期、チャンクベースのスナップショットが必要な場合は BulkAppendTree を選択してください。

## 実装ファイル

| ファイル | 目的 |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | クレートルート、再エクスポート |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree` 構造体、状態アクセサ、メタデータ永続化 |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`、`append_with_mem_buffer()`、`compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`、`buffer_key`、`chunk_key`、`mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`、`get_chunk`、`get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | ライトスルーキャッシュ付き `MmrAdapter` |
| `grovedb-bulk-append-tree/src/chunk.rs` | チャンクブロブシリアライズ（固定 + 可変フォーマット） |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` の生成と検証 |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore` トレイト |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB 操作、`AuxBulkStore`、バッチ前処理 |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27の統合テスト |

---
