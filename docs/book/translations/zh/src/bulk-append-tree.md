# BulkAppendTree — 高吞吐量仅追加存储

BulkAppendTree 是 GroveDB 针对一个特定工程挑战的解决方案：如何构建一个高吞吐量的仅追加日志（append-only log），支持高效的范围证明，最小化每次写入的哈希开销，并生成适合 CDN 分发的不可变块快照？

虽然 MmrTree（第 13 章）非常适合单个叶子证明，但 BulkAppendTree 专为每个区块有数千个值到达、且客户端需要通过获取数据范围来同步的工作负载而设计。它通过**两级架构**实现这一目标：一个稠密默克尔树缓冲区（dense Merkle tree buffer）用于吸收传入的追加操作，以及一个块级 MMR 用于记录已完成的块根。

## 两级架构

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

**第 1 级 — 缓冲区。** 传入的值被写入一个 `DenseFixedSizedMerkleTree`（见第 16 章）。缓冲区容量为 `2^height - 1` 个位置。稠密树的根哈希（`dense_tree_root`）在每次插入后更新。

**第 2 级 — 块 MMR。** 当缓冲区填满（达到 `chunk_size` 个条目）时，所有条目被序列化为一个不可变的**块二进制对象（chunk blob）**，对这些条目计算一个稠密默克尔根，然后将该根作为叶子追加到块 MMR 中。随后清空缓冲区。

**状态根（state root）**将两级组合为一个 32 字节的承诺（commitment），每次追加都会变化，确保父 Merk 树始终反映最新状态。

## 值如何填充缓冲区

每次调用 `append()` 遵循以下序列：

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

**缓冲区本身就是一个 `DenseFixedSizedMerkleTree`**（见第 16 章）。它的根哈希在每次插入后变化，提供对所有当前缓冲区条目的承诺。这个根哈希就是流入状态根计算的内容。

## 块压缩

当缓冲区填满（达到 `chunk_size` 个条目）时，压缩自动触发：

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

压缩完成后，块二进制对象是**永久不可变的** — 它再也不会改变。这使得块二进制对象非常适合 CDN 缓存、客户端同步和归档存储。

**示例：4 次追加，chunk_power=2（chunk_size=4）**

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

## 状态根

状态根将两级绑定为一个哈希：

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

`total_count` 和 `chunk_power` **不**包含在状态根中，因为它们已经通过 Merk 值哈希进行了认证 — 它们是存储在父 Merk 节点中的序列化 `Element` 的字段。状态根仅捕获数据级承诺（`mmr_root` 和 `dense_tree_root`）。这个哈希作为 Merk 子哈希向上传播到 GroveDB 根哈希。

## 稠密默克尔根

当块压缩时，条目需要一个 32 字节的承诺。BulkAppendTree 使用**稠密（完全）二叉默克尔树**：

```text
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

因为 `chunk_size` 始终是 2 的幂（由构造保证：`1u32 << chunk_power`），树总是完全的（不需要填充或哑叶子）。哈希次数恰好为 `2 * chunk_size - 1`：
- `chunk_size` 个叶子哈希（每个条目一个）
- `chunk_size - 1` 个内部节点哈希

稠密默克尔根的实现位于 `grovedb-mmr/src/dense_merkle.rs`，提供两个函数：
- `compute_dense_merkle_root(hashes)` — 从预哈希的叶子构建
- `compute_dense_merkle_root_from_values(values)` — 先对值进行哈希，然后构建树

## 块二进制对象序列化

块二进制对象是压缩产生的不可变存档。序列化器根据条目大小自动选择最紧凑的传输格式：

**固定大小格式**（标志 `0x01`）— 当所有条目具有相同长度时：

```text
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**变长格式**（标志 `0x00`）— 当条目有不同长度时：

```text
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

固定大小格式相比变长格式每个条目节省 4 字节，对于大量统一大小数据（如 32 字节哈希承诺）的块，这累积起来非常显著。对于 1024 个 32 字节条目：
- 固定：`1 + 4 + 4 + 32768 = 32,777 字节`
- 变长：`1 + 1024 × (4 + 32) = 36,865 字节`
- 节省：约 11%

## 存储键布局

所有 BulkAppendTree 数据存储在**数据**命名空间中，使用单字符前缀作为键：

| 键模式 | 格式 | 大小 | 用途 |
|---|---|---|---|
| `M` | 1 字节 | 1B | 元数据键 |
| `b` + `{index}` | `b` + u32 BE | 5B | 索引处的缓冲区条目 |
| `e` + `{index}` | `e` + u64 BE | 9B | 索引处的块二进制对象 |
| `m` + `{pos}` | `m` + u64 BE | 9B | 位置处的 MMR 节点 |

**元数据**存储 `mmr_size`（8 字节大端序）。`total_count` 和 `chunk_power` 存储在 Element 本身中（在父 Merk 中），而不是在数据命名空间的元数据中。这种分离意味着读取计数只需简单的元素查找，无需打开数据存储上下文。

缓冲区键使用 u32 索引（0 到 `chunk_size - 1`），因为缓冲区容量受限于 `chunk_size`（一个 u32，计算为 `1u32 << chunk_power`）。块键使用 u64 索引，因为已完成块的数量可以无限增长。

## BulkAppendTree 结构体

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

缓冲区就是一个 `DenseFixedSizedMerkleTree` — 它的根哈希就是 `dense_tree_root`。

**访问器：**
- `capacity() -> u16`：`dense_tree.capacity()`（= `2^height - 1`）
- `epoch_size() -> u64`：`capacity + 1`（= `2^height`，每块的条目数）
- `height() -> u8`：`dense_tree.height()`

**派生值**（不存储）：

| 值 | 公式 |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## GroveDB 操作

BulkAppendTree 通过 `grovedb/src/operations/bulk_append_tree.rs` 中定义的六个操作与 GroveDB 集成：

### bulk_append

主要的变更操作。遵循标准 GroveDB 非 Merk 存储模式：

```text
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

`AuxBulkStore` 适配器封装了 GroveDB 的 `get_aux`/`put_aux`/`delete_aux` 调用，并在 `RefCell` 中累积 `OperationCost` 以进行开销追踪。追加操作的哈希开销被添加到 `cost.hash_node_calls` 中。

### 读取操作

| 操作 | 返回内容 | 是否使用辅助存储？ |
|---|---|---|
| `bulk_get_value(path, key, position)` | 全局位置处的值 | 是 — 从块二进制对象或缓冲区读取 |
| `bulk_get_chunk(path, key, chunk_index)` | 原始块二进制对象 | 是 — 读取块键 |
| `bulk_get_buffer(path, key)` | 所有当前缓冲区条目 | 是 — 读取缓冲区键 |
| `bulk_count(path, key)` | 总计数（u64） | 否 — 从元素读取 |
| `bulk_chunk_count(path, key)` | 已完成块数（u64） | 否 — 从元素计算 |

`get_value` 操作根据位置透明路由：

```text
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## 批量操作和预处理

BulkAppendTree 通过 `GroveOp::BulkAppend` 变体支持批量操作。由于 `execute_ops_on_path` 无法访问数据存储上下文，所有 BulkAppend 操作必须在 `apply_body` 之前进行预处理。

预处理管线：

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

`append_with_mem_buffer` 变体避免了写后读问题：缓冲区条目在内存中的 `Vec<Vec<u8>>` 中跟踪，因此压缩可以读取它们，即使事务存储尚未提交。

## BulkStore Trait

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

方法使用 `&self`（而非 `&mut self`），以匹配 GroveDB 的内部可变性模式（interior mutability pattern），其中写操作通过批处理进行。GroveDB 集成通过 `AuxBulkStore` 实现，它封装了 `StorageContext` 并累积 `OperationCost`。

`MmrAdapter` 桥接 `BulkStore` 到 ckb MMR 的 `MMRStoreReadOps`/`MMRStoreWriteOps` trait，添加写穿透缓存以确保写后读的正确性。

## 证明生成

BulkAppendTree 的证明支持对位置的**范围查询**。证明结构包含无状态验证器确认特定数据存在于树中所需的全部信息：

```rust
pub struct BulkAppendTreeProof {
    pub chunk_power: u8,
    pub total_count: u64,
    pub chunk_blobs: Vec<(u64, Vec<u8>)>,         // Full chunk blobs
    pub chunk_mmr_size: u64,
    pub chunk_mmr_proof_items: Vec<[u8; 32]>,      // MMR sibling hashes
    pub chunk_mmr_leaves: Vec<(u64, [u8; 32])>,    // (leaf_idx, dense_root)
    pub buffer_entries: Vec<Vec<u8>>,               // ALL buffer entries
    pub chunk_mmr_root: [u8; 32],
}
```

**生成步骤**：对于范围 `[start, end)`（其中 `chunk_size = 1u32 << chunk_power`）：

```text
1. Determine overlapping chunks
   first_chunk = start / chunk_size
   last_chunk  = min((end-1) / chunk_size, completed_chunks - 1)

2. Read chunk blobs for overlapping chunks
   For each chunk_idx in [first_chunk, last_chunk]:
     chunk_blobs.push((chunk_idx, store.get(chunk_key(idx))))

3. Compute dense Merkle root for each chunk blob
   For each blob:
     deserialize → values
     dense_root = compute_dense_merkle_root_from_values(values)
     chunk_mmr_leaves.push((chunk_idx, dense_root))

4. Generate MMR proof for those chunk positions
   positions = chunk_indices.map(|idx| leaf_to_pos(idx))
   proof = mmr.gen_proof(positions)
   chunk_mmr_proof_items = proof.proof_items().map(|n| n.hash)

5. Get chunk MMR root

6. Read ALL buffer entries (bounded by chunk_size)
   for i in 0..buffer_count:
     buffer_entries.push(store.get(buffer_key(i)))
```

**为什么要包含所有缓冲区条目？** 缓冲区是一个稠密默克尔树，其根哈希对每个条目进行承诺。验证器必须从所有条目重建树来验证 `dense_tree_root`。由于缓冲区受 `capacity` 限制（最多 65,535 个条目），这是合理的开销。

## 证明验证

验证是一个纯函数 — 不需要数据库访问。它执行五项检查：

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

验证成功后，`BulkAppendTreeProofResult` 提供 `values_in_range(start, end)` 方法，可从已验证的块二进制对象和缓冲区条目中提取特定值。

## 如何关联到 GroveDB 根哈希

BulkAppendTree 是一个**非 Merk 树** — 它将数据存储在数据命名空间中，而不是子 Merk 子树中。在父 Merk 中，元素存储为：

```text
Element::BulkAppendTree(total_count, chunk_power, flags)
```

状态根作为 Merk 子哈希流动。父 Merk 节点哈希为：

```text
combine_hash(value_hash(element_bytes), state_root)
```

`state_root` 作为 Merk 子哈希流动（通过 `insert_subtree` 的 `subtree_root_hash` 参数）。状态根的任何变化都通过 GroveDB Merk 层级向上传播到根哈希。

在 V1 证明（第 9.6 节）中，父 Merk 证明验证元素字节和子哈希绑定，而 `BulkAppendTreeProof` 证明查询的数据与用作子哈希的 `state_root` 一致。

## 开销追踪

每个操作的哈希开销被显式追踪：

| 操作 | Blake3 调用次数 | 备注 |
|---|---|---|
| 单次追加（无压缩） | 3 | 2 次缓冲区哈希链 + 1 次状态根 |
| 单次追加（有压缩） | 3 + 2C - 1 + ~2 | 链 + 稠密默克尔（C=chunk_size）+ MMR 推入 + 状态根 |
| 从块获取值 | 0 | 纯反序列化，无哈希 |
| 从缓冲区获取值 | 0 | 直接键查找 |
| 证明生成 | 取决于块数 | 每块的稠密默克尔根 + MMR 证明 |
| 证明验证 | 2C·K - K + B·2 + 1 | K 个块，B 个缓冲区条目，C 为 chunk_size |

**每次追加的摊销开销**：对于 chunk_size=1024（chunk_power=10），约 2047 次哈希的压缩开销被摊销到 1024 次追加中，每次追加增加约 2 次哈希。加上每次追加的 3 次哈希，摊销总计约为**每次追加约 5 次 blake3 调用** — 对于一个密码学认证结构来说非常高效。

## 与 MmrTree 的比较

| | BulkAppendTree | MmrTree |
|---|---|---|
| **架构** | 两级（缓冲区 + 块 MMR） | 单一 MMR |
| **每次追加哈希开销** | 3（+ 摊销约 2 次用于压缩） | 约 2 |
| **证明粒度** | 位置的范围查询 | 单个叶子证明 |
| **不可变快照** | 是（块二进制对象） | 否 |
| **CDN 友好** | 是（块二进制对象可缓存） | 否 |
| **缓冲区条目** | 是（证明需要全部） | 不适用 |
| **最适用于** | 高吞吐量日志、批量同步 | 事件日志、单个查找 |
| **元素判别值** | 13 | 12 |
| **TreeType** | 9 | 8 |

当你需要开销最小的单个叶子证明时选择 MmrTree。当你需要范围查询、批量同步和基于块的快照时选择 BulkAppendTree。

## 实现文件

| 文件 | 用途 |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Crate 根，重导出 |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree` 结构体、状态访问器、元数据持久化 |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`、`append_with_mem_buffer()`、`compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`、`buffer_key`、`chunk_key`、`mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`、`get_chunk`、`get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | 带写穿透缓存的 `MmrAdapter` |
| `grovedb-bulk-append-tree/src/chunk.rs` | 块二进制对象序列化（固定 + 变长格式） |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` 生成与验证 |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore` trait |
| `grovedb-bulk-append-tree/src/error.rs` | `BulkAppendError` 枚举 |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB 操作、`AuxBulkStore`、批量预处理 |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 个集成测试 |

---
