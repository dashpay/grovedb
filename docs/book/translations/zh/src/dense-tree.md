# DenseAppendOnlyFixedSizeTree — 稠密固定容量默克尔存储

DenseAppendOnlyFixedSizeTree 是一个固定高度的完全二叉树，其中**每个节点** — 包括内部节点和叶子 — 都存储数据值。位置按层序（BFS）顺序依次填充：先根（位置 0），然后每层从左到右。不持久化中间哈希；根哈希通过从叶子到根的递归哈希即时重新计算。

这种设计非常适合最大容量预先已知的小型有界数据结构，需要 O(1) 追加、O(1) 按位置检索，以及每次插入后变化的紧凑 32 字节根哈希承诺。

## 树结构

高度为 *h* 的树有 `2^h - 1` 个位置容量。位置使用 0 基层序索引：

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

值按顺序追加：第一个值进入位置 0（根），然后是位置 1、2、3，依此类推。这意味着根始终有数据，树按层序填充 — 这是完全二叉树最自然的遍历顺序。

## 哈希计算

根哈希不单独存储 — 每次需要时都从头重新计算。递归算法只访问已填充的位置：

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

**关键属性：**
- 所有节点（叶子和内部）：`blake3(blake3(value) || H(left) || H(right))`
- 叶子节点：left_hash 和 right_hash 都是 `[0; 32]`（未填充的子节点）
- 未填充位置：`[0u8; 32]`（零哈希）
- 空树（count = 0）：`[0u8; 32]`

**不使用叶子/内部域分离标签。** 树结构（`height` 和 `count`）在父 `Element::DenseAppendOnlyFixedSizeTree` 中被外部认证，它通过 Merk 层级流动。验证器始终可以从高度和计数准确知道哪些位置是叶子、哪些是内部节点，因此攻击者无法在不破坏父认证链的情况下将一个替换为另一个。

这意味着根哈希编码了对每个存储值及其在树中确切位置的承诺。更改任何值（如果它是可变的）将级联传播到根的所有祖先哈希。

**哈希开销：** 计算根哈希访问所有已填充位置及其未填充的子节点。对于有 *n* 个值的树，最坏情况是 O(*n*) 次 blake3 调用。这是可以接受的，因为该树设计用于小型有界容量（最大高度 16，最多 65,535 个位置）。

## 元素变体

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| 字段 | 类型 | 描述 |
|---|---|---|
| `count` | `u16` | 到目前为止插入的值数量（最大 65,535） |
| `height` | `u8` | 树高度（1..=16），创建后不可变 |
| `flags` | `Option<ElementFlags>` | 可选的存储标志 |

根哈希不存储在 Element 中 — 它通过 `insert_subtree` 的 `subtree_root_hash` 参数作为 Merk 子哈希流动。

**判别值：** 14（ElementType），TreeType = 10

**开销大小：** `DENSE_TREE_COST_SIZE = 6` 字节（2 count + 1 height + 1 判别值 + 2 开销）

## 存储布局

与 MmrTree 和 BulkAppendTree 一样，DenseAppendOnlyFixedSizeTree 将数据存储在**数据**命名空间中（而不是子 Merk）。值以其位置的大端序 `u64` 作为键：

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

Element 本身（存储在父 Merk 中）携带 `count` 和 `height`。根哈希作为 Merk 子哈希流动。这意味着：
- **读取根哈希**需要从存储重新计算（O(n) 次哈希）
- **按位置读取值是 O(1)** — 单次存储查找
- **插入是 O(n) 次哈希** — 一次存储写入 + 完整的根哈希重新计算

## 操作

### `dense_tree_insert(path, key, value, tx, grove_version)`

将值追加到下一个可用位置。返回 `(root_hash, position)`。

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

检索给定位置的值。如果 position >= count 则返回 `None`。

### `dense_tree_root_hash(path, key, tx, grove_version)`

返回存储在元素中的根哈希。这是最近一次插入时计算的哈希 — 不需要重新计算。

### `dense_tree_count(path, key, tx, grove_version)`

返回存储的值数量（元素中的 `count` 字段）。

## 批量操作

`GroveOp::DenseTreeInsert` 变体通过标准 GroveDB 批量管线支持批量插入：

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

**预处理：** 与所有非 Merk 树类型一样，`DenseTreeInsert` 操作在主批处理体执行之前进行预处理。`preprocess_dense_tree_ops` 方法：

1. 按 `(path, key)` 对所有 `DenseTreeInsert` 操作进行分组
2. 对每组按顺序执行插入（读取元素、插入每个值、更新根哈希）
3. 将每组转换为一个 `ReplaceNonMerkTreeRoot` 操作，通过标准传播机制携带最终的 `root_hash` 和 `count`

支持在单个批次中对同一稠密树进行多次插入 — 它们按顺序处理，一致性检查允许此操作类型的重复键。

**传播：** 根哈希和计数通过 `ReplaceNonMerkTreeRoot` 中的 `NonMerkTreeMeta::DenseTree` 变体流动，遵循与 MmrTree 和 BulkAppendTree 相同的模式。

## 证明

DenseAppendOnlyFixedSizeTree 通过 `ProofBytes::DenseTree` 变体支持 **V1 子查询证明**。可以使用包含祖先值和兄弟子树哈希的包含证明（inclusion proof）来证明单个位置与树的根哈希的一致性。

### 认证路径结构

因为内部节点哈希其**自身的值**（而不仅仅是子哈希），认证路径与标准默克尔树不同。要验证位置 `p` 处的叶子，验证器需要：

1. **叶子值**（被证明的条目）
2. **祖先值哈希**：从 `p` 到根的路径上每个内部节点的值哈希（仅 32 字节哈希，不是完整值）
3. **兄弟子树哈希**：不在路径上的每个子节点的哈希

因为所有节点使用 `blake3(H(value) || H(left) || H(right))`（没有域标签），证明只携带祖先的 32 字节值哈希 — 而不是完整值。这使证明保持紧凑，无论单个值有多大。

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **注意：** `height` 和 `count` 不在证明结构中 — 验证器从父 Element 获取它们，父 Element 由 Merk 层级进行认证。

### 详解示例

高度为 3、容量为 7、count 为 5 的树，证明位置 4：

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

从 4 到根的路径：`4 → 1 → 0`。扩展集合：`{0, 1, 4}`。

证明包含：
- **entries**：`[(4, value[4])]` — 被证明的位置
- **node_value_hashes**：`[(0, H(value[0])), (1, H(value[1]))]` — 祖先值哈希（各 32 字节，不是完整值）
- **node_hashes**：`[(2, H(subtree_2)), (3, H(node_3))]` — 不在路径上的兄弟节点

验证从底向上重新计算根哈希：
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — 叶子（子节点未填充）
2. `H(3)` — 来自 `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — 内部节点使用来自 `node_value_hashes` 的值哈希
4. `H(2)` — 来自 `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — 根使用来自 `node_value_hashes` 的值哈希
6. 将 `H(0)` 与预期根哈希比较

### 多位置证明

当证明多个位置时，扩展集合合并重叠的认证路径。共享的祖先只包含一次，使多位置证明比独立的单位置证明更紧凑。

### V0 限制

V0 证明无法下降到稠密树中。如果 V0 查询匹配到带子查询的 `DenseAppendOnlyFixedSizeTree`，系统返回 `Error::NotSupported`，指导调用者使用 `prove_query_v1`。

### 查询键编码

稠密树位置编码为**大端序 u16**（2 字节）查询键，与使用 u64 的 MmrTree 和 BulkAppendTree 不同。支持所有标准的 `QueryItem` 范围类型。

## 与其他非 Merk 树的比较

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **元素判别值** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **容量** | 固定（`2^h - 1`，最大 65,535） | 无限 | 无限 | 无限 |
| **数据模型** | 每个位置存储值 | 仅叶子 | 稠密树缓冲区 + 块 | 仅叶子 |
| **哈希在 Element 中？** | 否（作为子哈希流动） | 否（作为子哈希流动） | 否（作为子哈希流动） | 否（作为子哈希流动） |
| **插入哈希开销** | O(n) blake3 | O(1) 摊销 | O(1) 摊销 | 约 33 Sinsemilla |
| **开销大小** | 6 字节 | 11 字节 | 12 字节 | 12 字节 |
| **证明支持** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **最适用于** | 小型有界结构 | 事件日志 | 高吞吐量日志 | ZK 承诺 |

**何时选择 DenseAppendOnlyFixedSizeTree：**
- 创建时最大条目数已知
- 需要每个位置（包括内部节点）都存储数据
- 需要最简单的数据模型，没有无界增长
- O(n) 根哈希重新计算可以接受（树高度较小）

**何时不选择它：**
- 需要无限容量 → 使用 MmrTree 或 BulkAppendTree
- 需要 ZK 兼容性 → 使用 CommitmentTree

## 使用示例

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

## 实现文件

| 文件 | 内容 |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore` trait、`DenseFixedSizedMerkleTree` 结构体、递归哈希 |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof` 结构体、`generate()`、`encode_to_vec()`、`decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — 纯函数，不需要存储 |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree`（判别值 14） |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`、`new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB 操作、`AuxDenseTreeStore`、批量预处理 |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`、`query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree` 变体 |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | 平均情况开销模型 |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | 最坏情况开销模型 |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 个集成测试 |

---
