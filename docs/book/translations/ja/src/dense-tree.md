# DenseAppendOnlyFixedSizeTree — 密な固定容量マークルストレージ

DenseAppendOnlyFixedSizeTree は固定高さの完全二分木であり、**すべてのノード** — 内部ノードとリーフの両方 — がデータ値を格納します。位置はレベル順（BFS）で順番に埋められます：ルートが最初（位置0）、次に各レベルで左から右へ。中間ハッシュは永続化されず、ルートハッシュはリーフからルートへ再帰的にハッシュすることでその場で再計算されます。

この設計は、最大容量が事前にわかっている小さな固定データ構造に最適であり、O(1) 追加、O(1) 位置指定取得、そして挿入ごとに変化するコンパクトな32バイトルートハッシュコミットメントが必要な場合に適しています。

## ツリー構造

高さ *h* のツリーは容量 `2^h - 1` 位置を持ちます。位置は0ベースのレベル順インデックスを使用します：

```text
Height 3 tree (capacity = 7):

              pos 0          ← root (level 0)
             /     \
          pos 1    pos 2     ← level 1
         /   \    /   \
       pos 3 pos 4 pos 5 pos 6  ← level 2 (leaves)

Navigation:
  left_child(i)  = 2i + 1
  right_child(i) = 2i + 2
  parent(i)      = (i - 1) / 2
  is_leaf(i)     = 2i + 1 >= capacity
```

値は順番に追加されます：最初の値は位置0（ルート）に、次に位置1、2、3と続きます。これはルートが常にデータを持ち、ツリーがレベル順に埋まることを意味します — 完全二分木にとって最も自然な走査順序です。

## ハッシュ計算

ルートハッシュは別途格納されず、必要に応じてゼロから再計算されます。再帰アルゴリズムは埋められた位置のみを訪問します：

```text
hash(position, store):
  value = store.get(position)

  if position is unfilled (>= count):
    return [0; 32]                                    ← empty sentinel

  value_hash = blake3(value)
  left_hash  = hash(2 * position + 1, store)
  right_hash = hash(2 * position + 2, store)
  return blake3(value_hash || left_hash || right_hash)
```

**主要な特性：**
- すべてのノード（リーフと内部）：`blake3(blake3(value) || H(left) || H(right))`
- リーフノード：left_hash と right_hash は両方 `[0; 32]`（未充填の子）
- 未充填位置：`[0u8; 32]`（ゼロハッシュ）
- 空のツリー（count = 0）：`[0u8; 32]`

**リーフ/内部ノードのドメイン分離タグは使用されません。** ツリー構造（`height` と `count`）は親の `Element::DenseAppendOnlyFixedSizeTree` で外部的に認証され、Merk 階層を通じて流れます。検証者は高さとカウントからどの位置がリーフで内部ノードかを常に正確に把握できるため、攻撃者は親の認証チェーンを破ることなく一方を他方に置き換えることはできません。

これにより、ルートハッシュはすべての格納値とツリー内のその正確な位置に対するコミットメントをエンコードします。いずれかの値を変更すると（変更可能であった場合）、すべての祖先ハッシュがルートまでカスケードします。

**ハッシュコスト：** ルートハッシュの計算はすべての充填位置と未充填の子を訪問します。*n* 個の値を持つツリーでは、最悪ケースは O(*n*) 回の blake3 呼び出しです。これはツリーが小さな固定容量（最大高さ16、最大65,535位置）向けに設計されているため許容されます。

## エレメントバリアント

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| フィールド | 型 | 説明 |
|---|---|---|
| `count` | `u16` | これまでに挿入された値の数（最大65,535） |
| `height` | `u8` | ツリーの高さ（1..=16）、作成後は不変 |
| `flags` | `Option<ElementFlags>` | オプションのストレージフラグ |

ルートハッシュはエレメントには格納されません — `insert_subtree` の `subtree_root_hash` パラメータを通じて Merk の子ハッシュとして流れます。

**判別子：** 14（ElementType）、TreeType = 10

**コストサイズ：** `DENSE_TREE_COST_SIZE = 6` バイト（2 count + 1 height + 1 判別子 + 2 オーバーヘッド）

## ストレージレイアウト

MmrTree や BulkAppendTree と同様に、DenseAppendOnlyFixedSizeTree はデータを**データ**名前空間に格納します（子 Merk ではありません）。値は位置をビッグエンディアン `u64` としてキーにします：

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

エレメント自体（親 Merk に格納）は `count` と `height` を持ちます。ルートハッシュは Merk の子ハッシュとして流れます。これは：
- **ルートハッシュの読み取り**にはストレージからの再計算が必要（O(n) ハッシュ）
- **位置による値の読み取りは O(1)** — 単一のストレージルックアップ
- **挿入は O(n) ハッシュ** — 1回のストレージ書き込み + 完全なルートハッシュ再計算

## 操作

### `dense_tree_insert(path, key, value, tx, grove_version)`

次の利用可能な位置に値を追加します。`(root_hash, position)` を返します。

```text
Step 1: Read element, extract (count, height)
Step 2: Check capacity: if count >= 2^height - 1 → error
Step 3: Build subtree path, open storage context
Step 4: Write value to position = count
Step 5: Reconstruct DenseFixedSizedMerkleTree from state
Step 6: Call tree.insert(value, store) → (root_hash, position, hash_calls)
Step 7: Update element with new root_hash and count + 1
Step 8: Propagate changes up through Merk hierarchy
Step 9: Commit transaction
```

### `dense_tree_get(path, key, position, tx, grove_version)`

指定された位置の値を取得します。position >= count の場合は `None` を返します。

### `dense_tree_root_hash(path, key, tx, grove_version)`

エレメントに格納されたルートハッシュを返します。これは最後の挿入時に計算されたハッシュであり、再計算は不要です。

### `dense_tree_count(path, key, tx, grove_version)`

格納された値の数（エレメントの `count` フィールド）を返します。

## バッチ操作

`GroveOp::DenseTreeInsert` バリアントは標準的な GroveDB バッチパイプラインによるバッチ挿入をサポートします：

```rust
let ops = vec![
    QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"my_dense_tree".to_vec(),
        b"value_data".to_vec(),
    ),
];
db.apply_batch(ops, None, None, grove_version)?;
```

**前処理：** すべての非 Merk ツリー型と同様に、`DenseTreeInsert` 操作はメインのバッチ本体が実行される前に前処理されます。`preprocess_dense_tree_ops` メソッドは：

1. すべての `DenseTreeInsert` 操作を `(path, key)` でグループ化
2. 各グループについて、挿入を順番に実行（エレメントの読み取り、各値の挿入、ルートハッシュの更新）
3. 各グループを `ReplaceNonMerkTreeRoot` 操作に変換し、最終的な `root_hash` と `count` を標準伝播機構を通じて運ぶ

同じ密ツリーへの単一バッチ内の複数挿入がサポートされています — 順番に処理され、一貫性チェックはこの操作タイプの重複キーを許可します。

**伝播：** ルートハッシュとカウントは `ReplaceNonMerkTreeRoot` 内の `NonMerkTreeMeta::DenseTree` バリアントを通じて流れ、MmrTree や BulkAppendTree と同じパターンに従います。

## 証明

DenseAppendOnlyFixedSizeTree は `ProofBytes::DenseTree` バリアントによる **V1 サブクエリ証明**をサポートします。個別の位置はツリーのルートハッシュに対して、祖先の値と兄弟サブツリーハッシュを含む包含証明を使用して証明できます。

### 認証パス構造

内部ノードが（子ハッシュだけでなく）**自身の値**をハッシュするため、認証パスは標準的なマークル木と異なります。位置 `p` のリーフを検証するために、検証者は以下が必要です：

1. **リーフ値**（証明対象エントリ）
2. **祖先値ハッシュ** — `p` からルートまでのパス上のすべての内部ノード（完全な値ではなく32バイトハッシュのみ）
3. **兄弟サブツリーハッシュ** — パス上にないすべての子

すべてのノードが `blake3(H(value) || H(left) || H(right))`（ドメインタグなし）を使用するため、証明は祖先の32バイト値ハッシュのみを運びます — 完全な値ではありません。これにより個々の値のサイズに関係なく証明はコンパクトに保たれます。

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **注意：** `height` と `count` は証明構造体にありません — 検証者はこれらを Merk 階層で認証された親エレメントから取得します。

### ウォークスルー例

height=3、capacity=7、count=5 のツリーで位置4を証明：

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

位置4からルートへのパス：`4 → 1 → 0`。展開集合：`{0, 1, 4}`。

証明の内容：
- **entries**：`[(4, value[4])]` — 証明対象位置
- **node_value_hashes**：`[(0, H(value[0])), (1, H(value[1]))]` — 祖先値ハッシュ（各32バイト、完全な値ではない）
- **node_hashes**：`[(2, H(subtree_2)), (3, H(node_3))]` — パス上にない兄弟

検証はボトムアップでルートハッシュを再計算します：
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — リーフ（子は未充填）
2. `H(3)` — `node_hashes` から
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — 内部ノードは `node_value_hashes` の値ハッシュを使用
4. `H(2)` — `node_hashes` から
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — ルートは `node_value_hashes` の値ハッシュを使用
6. `H(0)` を期待されるルートハッシュと比較

### 複数位置証明

複数の位置を証明する場合、展開集合は重複する認証パスをマージします。共有される祖先は一度だけ含まれるため、複数位置証明は独立した単一位置証明よりもコンパクトになります。

### V0 の制限

V0 証明は密ツリーに降りることができません。V0 クエリがサブクエリ付きの `DenseAppendOnlyFixedSizeTree` にマッチした場合、システムは `Error::NotSupported` を返し、呼び出し元に `prove_query_v1` の使用を指示します。

### クエリキーエンコーディング

密ツリーの位置は**ビッグエンディアン u16**（2バイト）クエリキーとしてエンコードされます。MmrTree や BulkAppendTree が u64 を使用するのとは異なります。すべての標準 `QueryItem` 範囲型がサポートされています。

## 他の非 Merk ツリーとの比較

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element 判別子** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **容量** | 固定（`2^h - 1`、最大65,535） | 無制限 | 無制限 | 無制限 |
| **データモデル** | すべての位置に値を格納 | リーフのみ | 密ツリーバッファ + チャンク | リーフのみ |
| **エレメント内ハッシュ？** | なし（子ハッシュとして流れる） | なし（子ハッシュとして流れる） | なし（子ハッシュとして流れる） | なし（子ハッシュとして流れる） |
| **挿入コスト（ハッシュ）** | O(n) blake3 | O(1) 償却 | O(1) 償却 | 約33 Sinsemilla |
| **コストサイズ** | 6バイト | 11バイト | 12バイト | 12バイト |
| **証明サポート** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **最適な用途** | 小さな固定構造 | イベントログ | 高スループットログ | ZK コミットメント |

**DenseAppendOnlyFixedSizeTree を選択すべき場合：**
- エントリの最大数が作成時にわかっている
- すべての位置（内部ノードを含む）がデータを格納する必要がある
- 制限なし成長のない最もシンプルなデータモデルが必要
- O(n) ルートハッシュ再計算が許容される（小さなツリー高さ）

**選択すべきでない場合：**
- 無制限の容量が必要 → MmrTree または BulkAppendTree を使用
- ZK 互換性が必要 → CommitmentTree を使用

## 使用例

```rust
use grovedb::Element;
use grovedb_version::version::GroveVersion;

let grove_version = GroveVersion::latest();

// Create a dense tree of height 4 (capacity = 15 values)
db.insert(
    &[b"state"],
    b"validator_slots",
    Element::empty_dense_tree(4),
    None,
    None,
    grove_version,
)?;

// Append values — positions filled 0, 1, 2, ...
let (root_hash, pos) = db.dense_tree_insert(
    &[b"state"],
    b"validator_slots",
    validator_pubkey.to_vec(),
    None,
    grove_version,
)?;
// pos == 0, root_hash = blake3(validator_pubkey)

// Read back by position
let value = db.dense_tree_get(
    &[b"state"],
    b"validator_slots",
    0,     // position
    None,
    grove_version,
)?;
assert_eq!(value, Some(validator_pubkey.to_vec()));

// Query metadata
let count = db.dense_tree_count(&[b"state"], b"validator_slots", None, grove_version)?;
let hash = db.dense_tree_root_hash(&[b"state"], b"validator_slots", None, grove_version)?;
```

## 実装ファイル

| ファイル | 内容 |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore` トレイト、`DenseFixedSizedMerkleTree` 構造体、再帰ハッシュ |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof` 構造体、`generate()`、`encode_to_vec()`、`decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — ストレージ不要の純粋関数 |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree`（判別子14） |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`、`new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB 操作、`AuxDenseTreeStore`、バッチ前処理 |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`、`query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree` バリアント |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | 平均ケースコストモデル |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | 最悪ケースコストモデル |
| `grovedb/src/tests/dense_tree_tests.rs` | 22の統合テスト |

---
