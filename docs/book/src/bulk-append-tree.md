# The BulkAppendTree — High-Throughput Append-Only Storage

The BulkAppendTree is GroveDB's answer to a specific engineering challenge: how do you
build a high-throughput append-only log that supports efficient range proofs, minimises
per-write hashing, and produces immutable chunk snapshots suitable for CDN distribution?

While an MmrTree (Chapter 13) is ideal for individual leaf proofs, the BulkAppendTree
is designed for workloads where thousands of values arrive per block and clients need
to sync by fetching ranges of data. It achieves this with a **two-level architecture**:
a dense Merkle tree buffer that absorbs incoming appends, and a chunk-level MMR that
records finalized chunk roots.

## The Two-Level Architecture

```
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

**Level 1 — The Buffer.** Incoming values are written to a `DenseFixedSizedMerkleTree`
(see Chapter 16). The buffer capacity is `2^height - 1` positions. The dense tree's
root hash (`dense_tree_root`) updates after every insert.

**Level 2 — The Chunk MMR.** When the buffer fills (reaches `chunk_size` entries),
all entries are serialized into an immutable **chunk blob**, a dense Merkle root is
computed over those entries, and that root is appended as a leaf to the chunk MMR.
The buffer is then cleared.

The **state root** combines both levels into a single 32-byte commitment that changes
on every append, ensuring the parent Merk tree always reflects the latest state.

## How Values Fill the Buffer

Each call to `append()` follows this sequence:

```
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

The **buffer IS a DenseFixedSizedMerkleTree** (see Chapter 16). Its root hash
changes after every insert, providing a commitment to all current buffer entries.
This root hash is what flows into the state root computation.

## Chunk Compaction

When the buffer fills (reaches `chunk_size` entries), compaction fires automatically:

```
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

After compaction, the chunk blob is **permanently immutable** — it never changes
again. This makes chunk blobs ideal for CDN caching, client sync, and archival
storage.

**Example: 4 appends with chunk_power=2 (chunk_size=4)**

```
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

## The State Root

The state root binds both levels into one hash:

```rust
fn compute_state_root(
    mmr_root: &[u8; 32],         // Chunk MMR root (or [0;32] if empty)
    dense_tree_root: &[u8; 32],  // Root hash of current buffer (dense tree)
) -> [u8; 32] {
    blake3("bulk_state" || mmr_root || dense_tree_root)
}
```

The `total_count` and `chunk_power` are **not** included in the state root because
they are already authenticated by the Merk value hash — they are fields of the
serialized `Element` stored in the parent Merk node. The state root captures only the
data-level commitments (`mmr_root` and `dense_tree_root`). This is the hash that
flows as the Merk child hash and propagates up to the GroveDB root hash.

## The Dense Merkle Root

When a chunk compacts, the entries need a single 32-byte commitment. The
BulkAppendTree uses a **dense (complete) binary Merkle tree**:

```
Given entries [e_0, e_1, e_2, e_3]:

Level 0 (leaves):  blake3(e_0)  blake3(e_1)  blake3(e_2)  blake3(e_3)
                      \__________/              \__________/
Level 1:           blake3(h_0 || h_1)       blake3(h_2 || h_3)
                          \____________________/
Level 2 (root):    blake3(h_01 || h_23)  ← this is the dense Merkle root
```

Because `chunk_size` is always a power of 2 (by construction: `1u32 << chunk_power`),
the tree is always complete (no padding or dummy leaves needed). The hash count is
exactly `2 * chunk_size - 1`:
- `chunk_size` leaf hashes (one per entry)
- `chunk_size - 1` internal node hashes

The dense Merkle root implementation lives in `grovedb-mmr/src/dense_merkle.rs` and
provides two functions:
- `compute_dense_merkle_root(hashes)` — from pre-hashed leaves
- `compute_dense_merkle_root_from_values(values)` — hashes values first, then builds
  the tree

## Chunk Blob Serialization

Chunk blobs are the immutable archives produced by compaction. The serializer
auto-selects the most compact wire format based on entry sizes:

**Fixed-size format** (flag `0x01`) — when all entries have the same length:

```
┌──────┬──────────┬─────────────┬─────────┬─────────┬─────────┐
│ 0x01 │ count    │ entry_size  │ entry_0 │ entry_1 │ ...     │
│ 1B   │ 4B (BE)  │ 4B (BE)     │ N bytes │ N bytes │         │
└──────┴──────────┴─────────────┴─────────┴─────────┴─────────┘
Total: 1 + 4 + 4 + (count × entry_size) bytes
```

**Variable-size format** (flag `0x00`) — when entries have different lengths:

```
┌──────┬──────────┬─────────┬──────────┬─────────┬─────────────┐
│ 0x00 │ len_0    │ entry_0 │ len_1    │ entry_1 │ ...         │
│ 1B   │ 4B (BE)  │ N bytes │ 4B (BE)  │ M bytes │             │
└──────┴──────────┴─────────┴──────────┴─────────┴─────────────┘
Total: 1 + Σ(4 + len_i) bytes
```

The fixed-size format saves 4 bytes per entry compared to variable-size, which adds up
significantly for large chunks of uniform-size data (like 32-byte hash commitments).
For 1024 entries of 32 bytes each:
- Fixed: `1 + 4 + 4 + 32768 = 32,777 bytes`
- Variable: `1 + 1024 × (4 + 32) = 36,865 bytes`
- Savings: ~11%

## Storage Key Layout

All BulkAppendTree data lives in the **data** namespace, keyed with single-character prefixes:

| Key pattern | Format | Size | Purpose |
|---|---|---|---|
| `M` | 1 byte | 1B | Metadata key |
| `b` + `{index}` | `b` + u32 BE | 5B | Buffer entry at index |
| `e` + `{index}` | `e` + u64 BE | 9B | Chunk blob at index |
| `m` + `{pos}` | `m` + u64 BE | 9B | MMR node at position |

**Metadata** stores `mmr_size` (8 bytes BE). The `total_count` and `chunk_power` are
stored in the Element itself (in the parent Merk), not in data namespace metadata.
This split means reading the count is a simple element lookup without opening the
data storage context.

Buffer keys use u32 indices (0 to `chunk_size - 1`) because the buffer capacity is
limited by `chunk_size` (a u32, computed as `1u32 << chunk_power`). Chunk keys use u64
indices because the number of completed chunks can grow indefinitely.

## The BulkAppendTree Struct

```rust
pub struct BulkAppendTree<S> {
    pub total_count: u64,                        // Total values ever appended
    pub dense_tree: DenseFixedSizedMerkleTree<S>, // The buffer (dense tree)
}
```

The buffer IS a `DenseFixedSizedMerkleTree` — its root hash is `dense_tree_root`.

**Accessors:**
- `capacity() -> u16`: `dense_tree.capacity()` (= `2^height - 1`)
- `epoch_size() -> u64`: `capacity + 1` (= `2^height`, the number of entries per chunk)
- `height() -> u8`: `dense_tree.height()`

**Derived values** (not stored):

| Value | Formula |
|---|---|
| `chunk_count` | `total_count / epoch_size` |
| `buffer_count` | `dense_tree.count()` |
| `mmr_size` | `leaf_count_to_mmr_size(chunk_count)` |

## GroveDB Operations

The BulkAppendTree integrates with GroveDB through six operations defined in
`grovedb/src/operations/bulk_append_tree.rs`:

### bulk_append

The primary mutating operation. Follows the standard GroveDB non-Merk storage pattern:

```
1. Validate element is BulkAppendTree
2. Open data storage context
3. Load tree from store
4. Append value (may trigger compaction)
5. Update element in parent Merk with new state_root + total_count
6. Propagate changes up through Merk hierarchy
7. Commit transaction
```

The `AuxBulkStore` adapter wraps GroveDB's `get_aux`/`put_aux`/`delete_aux` calls and
accumulates `OperationCost` in a `RefCell` for cost tracking. Hash costs from the
append operation are added to `cost.hash_node_calls`.

### Read operations

| Operation | What it returns | Aux storage? |
|---|---|---|
| `bulk_get_value(path, key, position)` | Value at global position | Yes — reads from chunk blob or buffer |
| `bulk_get_chunk(path, key, chunk_index)` | Raw chunk blob | Yes — reads chunk key |
| `bulk_get_buffer(path, key)` | All current buffer entries | Yes — reads buffer keys |
| `bulk_count(path, key)` | Total count (u64) | No — reads from element |
| `bulk_chunk_count(path, key)` | Completed chunks (u64) | No — computed from element |

The `get_value` operation transparently routes by position:

```
if position < completed_chunks × chunk_size:
    chunk_idx = position / chunk_size
    intra_idx = position % chunk_size
    → read chunk blob, deserialize, return entry[intra_idx]
else:
    buffer_idx = position % chunk_size
    → read buffer_key(buffer_idx)
```

## Batch Operations and Preprocessing

BulkAppendTree supports batch operations through the `GroveOp::BulkAppend` variant.
Since `execute_ops_on_path` doesn't have access to the data storage context, all BulkAppend ops
must be preprocessed before `apply_body`.

The preprocessing pipeline:

```
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

The `append_with_mem_buffer` variant avoids read-after-write issues: buffer entries
are tracked in a `Vec<Vec<u8>>` in memory, so compaction can read them even though
the transactional storage hasn't committed yet.

## The BulkStore Trait

```rust
pub trait BulkStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn delete(&self, key: &[u8]) -> Result<(), String>;
}
```

Methods take `&self` (not `&mut self`) to match GroveDB's interior mutability pattern
where writes go through a batch. The GroveDB integration implements this via
`AuxBulkStore` which wraps a `StorageContext` and accumulates `OperationCost`.

The `MmrAdapter` bridges `BulkStore` to the ckb MMR's `MMRStoreReadOps`/
`MMRStoreWriteOps` traits, adding a write-through cache for read-after-write
correctness.

## Proof Generation

BulkAppendTree proofs support **range queries** over positions. The proof structure
captures everything needed for a stateless verifier to confirm that specific data
exists in the tree:

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

**Generation steps** for a range `[start, end)` (with `chunk_size = 1u32 << chunk_power`):

```
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

**Why include ALL buffer entries?** The buffer is a dense Merkle tree whose root hash
commits to every entry. The verifier must rebuild the tree from all entries to verify
the `dense_tree_root`. Since the buffer is bounded by `capacity` (at most 65,535
entries), this is a reasonable cost.

## Proof Verification

Verification is a pure function — no database access needed. It performs five checks:

```
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

After verification succeeds, the `BulkAppendTreeProofResult` provides a
`values_in_range(start, end)` method that extracts specific values from the verified
chunk blobs and buffer entries.

## How It Ties to the GroveDB Root Hash

The BulkAppendTree is a **non-Merk tree** — it stores data in the data namespace,
not in a child Merk subtree. In the parent Merk, the element is stored as:

```
Element::BulkAppendTree(total_count, chunk_power, flags)
```

The state root flows as the Merk child hash. The parent Merk node hash is:

```
combine_hash(value_hash(element_bytes), state_root)
```

The `state_root` flows as the Merk child hash (via `insert_subtree`'s
`subtree_root_hash` parameter). Any change to the state root propagates up through
the GroveDB Merk hierarchy to the root hash.

In V1 proofs (§9.6), the parent Merk proof proves the element bytes and the child
hash binding, and the `BulkAppendTreeProof` proves that the queried data is consistent
with the `state_root` used as the child hash.

## Cost Tracking

Each operation's hash cost is tracked explicitly:

| Operation | Blake3 calls | Notes |
|---|---|---|
| Single append (no compaction) | 3 | 2 for buffer hash chain + 1 for state root |
| Single append (with compaction) | 3 + 2C - 1 + ~2 | Chain + dense Merkle (C=chunk_size) + MMR push + state root |
| `get_value` from chunk | 0 | Pure deserialization, no hashing |
| `get_value` from buffer | 0 | Direct key lookup |
| Proof generation | Depends on chunk count | Dense Merkle root per chunk + MMR proof |
| Proof verification | 2C·K - K + B·2 + 1 | K chunks, B buffer entries, C chunk_size |

**Amortized cost per append**: For chunk_size=1024 (chunk_power=10), the compaction overhead of ~2047
hashes (dense Merkle root) is amortized over 1024 appends, adding ~2 hashes per
append. Combined with the 3 per-append hashes, the amortized total is **~5 blake3
calls per append** — very efficient for a cryptographically authenticated structure.

## Comparison with MmrTree

| | BulkAppendTree | MmrTree |
|---|---|---|
| **Architecture** | Two-level (buffer + chunk MMR) | Single MMR |
| **Per-append hash cost** | 3 (+ amortized ~2 for compaction) | ~2 |
| **Proof granularity** | Range queries over positions | Individual leaf proofs |
| **Immutable snapshots** | Yes (chunk blobs) | No |
| **CDN-friendly** | Yes (chunk blobs cacheable) | No |
| **Buffer entries** | Yes (need all for proof) | N/A |
| **Best for** | High-throughput logs, bulk sync | Event logs, individual lookups |
| **Element discriminant** | 13 | 12 |
| **TreeType** | 9 | 8 |

Choose MmrTree when you need individual leaf proofs with minimal overhead. Choose
BulkAppendTree when you need range queries, bulk synchronization, and chunk-based
snapshots.

## Implementation Files

| File | Purpose |
|------|---------|
| `grovedb-bulk-append-tree/src/lib.rs` | Crate root, re-exports |
| `grovedb-bulk-append-tree/src/tree/mod.rs` | `BulkAppendTree` struct, state accessors, metadata persistence |
| `grovedb-bulk-append-tree/src/tree/append.rs` | `append()`, `append_with_mem_buffer()`, `compact_entries()` |
| `grovedb-bulk-append-tree/src/tree/hash.rs` | `compute_state_root` |
| `grovedb-bulk-append-tree/src/tree/keys.rs` | `META_KEY`, `buffer_key`, `chunk_key`, `mmr_node_key` |
| `grovedb-bulk-append-tree/src/tree/query.rs` | `get_value`, `get_chunk`, `get_buffer` |
| `grovedb-bulk-append-tree/src/tree/mmr_adapter.rs` | `MmrAdapter` with write-through cache |
| `grovedb-bulk-append-tree/src/chunk.rs` | Chunk blob serialization (fixed + variable formats) |
| `grovedb-bulk-append-tree/src/proof.rs` | `BulkAppendTreeProof` generation and verification |
| `grovedb-bulk-append-tree/src/store.rs` | `BulkStore` trait |
| `grovedb-bulk-append-tree/src/error.rs` | `BulkAppendError` enum |
| `grovedb/src/operations/bulk_append_tree.rs` | GroveDB operations, `AuxBulkStore`, batch preprocessing |
| `grovedb/src/tests/bulk_append_tree_tests.rs` | 27 integration tests |

---

