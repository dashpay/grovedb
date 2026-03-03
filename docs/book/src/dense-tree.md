# The DenseAppendOnlyFixedSizeTree — Dense Fixed-Capacity Merkle Storage

The DenseAppendOnlyFixedSizeTree is a complete binary tree of a fixed height where
**every node** — both internal and leaf — stores a data value. Positions are filled
sequentially in level-order (BFS): root first (position 0), then left-to-right at each
level. No intermediate hashes are persisted; the root hash is recomputed on the fly by
recursively hashing from leaves to root.

This design is ideal for small, bounded data structures where the maximum capacity is
known in advance and you need O(1) append, O(1) retrieval by position, and a compact
32-byte root hash commitment that changes after every insert.

## Tree Structure

A tree of height *h* has capacity `2^h - 1` positions. Positions use 0-based level-order
indexing:

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

Values are appended sequentially: the first value goes to position 0 (root), then
position 1, 2, 3, and so on. This means the root always has data, and the tree fills
in level-order — the most natural traversal order for a complete binary tree.

## Hash Computation

The root hash is not stored separately — it is recomputed from scratch whenever needed.
The recursive algorithm visits only filled positions:

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

**Key properties:**
- All nodes (leaf and internal): `blake3(blake3(value) || H(left) || H(right))`
- Leaf nodes: left_hash and right_hash are both `[0; 32]` (unfilled children)
- Unfilled positions: `[0u8; 32]` (zero hash)
- Empty tree (count = 0): `[0u8; 32]`

**No leaf/internal domain separation tags are used.** The tree structure (`height`
and `count`) is externally authenticated in the parent `Element::DenseAppendOnlyFixedSizeTree`,
which flows through the Merk hierarchy. The verifier always knows exactly which
positions are leaves vs internal nodes from the height and count, so an attacker
cannot substitute one for the other without breaking the parent authentication chain.

This means the root hash encodes a commitment to every stored value and its exact
position in the tree. Changing any value (if it were mutable) would cascade through
all ancestor hashes up to the root.

**Hash cost:** Computing the root hash visits all filled positions plus any unfilled
children. For a tree with *n* values, worst case is O(*n*) blake3 calls. This is
acceptable because the tree is designed for small, bounded capacities (max height 16,
max 65,535 positions).

## The Element Variant

```rust
Element::DenseAppendOnlyFixedSizeTree(
    u16,                   // count — number of values stored (max 65,535)
    u8,                    // height — immutable after creation (1..=16)
    Option<ElementFlags>,  // flags — storage flags
)
```

| Field | Type | Description |
|---|---|---|
| `count` | `u16` | Number of values inserted so far (max 65,535) |
| `height` | `u8` | Tree height (1..=16), immutable after creation |
| `flags` | `Option<ElementFlags>` | Optional storage flags |

The root hash is NOT stored in the Element — it flows as the Merk child hash
via `insert_subtree`'s `subtree_root_hash` parameter.

**Discriminant:** 14 (ElementType), TreeType = 10

**Cost size:** `DENSE_TREE_COST_SIZE = 6` bytes (2 count + 1 height + 1 discriminant
+ 2 overhead)

## Storage Layout

Like MmrTree and BulkAppendTree, the DenseAppendOnlyFixedSizeTree stores data in the
**data** namespace (not a child Merk). Values are keyed by their position as a big-endian `u64`:

```text
Subtree path: blake3(parent_path || key)

Storage keys:
  [0, 0, 0, 0, 0, 0, 0, 0] → value at position 0 (root)
  [0, 0, 0, 0, 0, 0, 0, 1] → value at position 1
  [0, 0, 0, 0, 0, 0, 0, 2] → value at position 2
  ...
```

The Element itself (stored in the parent Merk) carries the `count` and `height`.
The root hash flows as the Merk child hash. This means:
- **Reading the root hash** requires recomputation from storage (O(n) hashing)
- **Reading a value by position is O(1)** — single storage lookup
- **Inserting is O(n) hashing** — one storage write + full root hash recomputation

## Operations

### `dense_tree_insert(path, key, value, tx, grove_version)`

Appends a value to the next available position. Returns `(root_hash, position)`.

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

Retrieves the value at a given position. Returns `None` if position >= count.

### `dense_tree_root_hash(path, key, tx, grove_version)`

Returns the root hash stored in the element. This is the hash computed during the
most recent insert — no recomputation needed.

### `dense_tree_count(path, key, tx, grove_version)`

Returns the number of values stored (the `count` field from the element).

## Batch Operations

The `GroveOp::DenseTreeInsert` variant supports batch insertion through the standard
GroveDB batch pipeline:

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

**Preprocessing:** Like all non-Merk tree types, `DenseTreeInsert` ops are preprocessed
before the main batch body executes. The `preprocess_dense_tree_ops` method:

1. Groups all `DenseTreeInsert` ops by `(path, key)`
2. For each group, executes the inserts sequentially (reading the element, inserting
   each value, updating the root hash)
3. Converts each group into a `ReplaceNonMerkTreeRoot` op that carries the final
   `root_hash` and `count` through the standard propagation machinery

Multiple inserts to the same dense tree within a single batch are supported — they
are processed in order and the consistency check allows duplicate keys for this op type.

**Propagation:** The root hash and count flow through the `NonMerkTreeMeta::DenseTree`
variant in `ReplaceNonMerkTreeRoot`, following the same pattern as MmrTree and
BulkAppendTree.

## Proofs

DenseAppendOnlyFixedSizeTree supports **V1 subquery proofs** via the `ProofBytes::DenseTree`
variant. Individual positions can be proved against the tree's root hash using inclusion
proofs that carry ancestor values and sibling subtree hashes.

### Auth Path Structure

Because internal nodes hash their **own value** (not just child hashes), the
authentication path differs from a standard Merkle tree. To verify a leaf at position
`p`, the verifier needs:

1. **The leaf value** (the proved entry)
2. **Ancestor value hashes** for every internal node on the path from `p` to the root (only the 32-byte hash, not the full value)
3. **Sibling subtree hashes** for every child that is NOT on the path

Because all nodes use `blake3(H(value) || H(left) || H(right))` (no domain tags),
the proof only carries 32-byte value hashes for ancestors — not full values. This
keeps proofs compact regardless of how large individual values are.

```rust
pub struct DenseTreeProof {
    pub entries: Vec<(u16, Vec<u8>)>,            // proved (position, value) pairs
    pub node_value_hashes: Vec<(u16, [u8; 32])>, // ancestor value hashes on the auth path
    pub node_hashes: Vec<(u16, [u8; 32])>,       // precomputed sibling subtree hashes
}
```

> **Note:** `height` and `count` are not in the proof struct — the verifier gets them from the parent Element, which is authenticated by the Merk hierarchy.

### Walkthrough Example

Tree with height=3, capacity=7, count=5, proving position 4:

```text
        0
       / \
      1   2
     / \ / \
    3  4 5  6
```

Path from 4 to root: `4 → 1 → 0`. Expanded set: `{0, 1, 4}`.

The proof contains:
- **entries**: `[(4, value[4])]` — the proved position
- **node_value_hashes**: `[(0, H(value[0])), (1, H(value[1]))]` — ancestor value hashes (32 bytes each, not full values)
- **node_hashes**: `[(2, H(subtree_2)), (3, H(node_3))]` — siblings not on the path

Verification recomputes the root hash bottom-up:
1. `H(4) = blake3(blake3(value[4]) || [0;32] || [0;32])` — leaf (children are unfilled)
2. `H(3)` — from `node_hashes`
3. `H(1) = blake3(H(value[1]) || H(3) || H(4))` — internal uses value hash from `node_value_hashes`
4. `H(2)` — from `node_hashes`
5. `H(0) = blake3(H(value[0]) || H(1) || H(2))` — root uses value hash from `node_value_hashes`
6. Compare `H(0)` against expected root hash

### Multi-Position Proofs

When proving multiple positions, the expanded set merges overlapping auth paths. Shared
ancestors are included only once, making multi-position proofs more compact than
independent single-position proofs.

### V0 Limitation

V0 proofs cannot descend into dense trees. If a V0 query matches a
`DenseAppendOnlyFixedSizeTree` with a subquery, the system returns
`Error::NotSupported` directing the caller to use `prove_query_v1`.

### Query Key Encoding

Dense tree positions are encoded as **big-endian u16** (2-byte) query keys, unlike
MmrTree and BulkAppendTree which use u64. All standard `QueryItem` range types
are supported.

## Comparison with Other Non-Merk Trees

| | DenseTree | MmrTree | BulkAppendTree | CommitmentTree |
|---|---|---|---|---|
| **Element discriminant** | 14 | 12 | 13 | 11 |
| **TreeType** | 10 | 8 | 9 | 7 |
| **Capacity** | Fixed (`2^h - 1`, max 65,535) | Unlimited | Unlimited | Unlimited |
| **Data model** | Every position stores a value | Leaf-only | Dense tree buffer + chunks | Leaf-only |
| **Hash in Element?** | No (flows as child hash) | No (flows as child hash) | No (flows as child hash) | No (flows as child hash) |
| **Insert cost (hashing)** | O(n) blake3 | O(1) amortized | O(1) amortized | ~33 Sinsemilla |
| **Cost size** | 6 bytes | 11 bytes | 12 bytes | 12 bytes |
| **Proof support** | V1 (Dense) | V1 (MMR) | V1 (Bulk) | V1 (CommitmentTree) |
| **Best for** | Small bounded structures | Event logs | High-throughput logs | ZK commitments |

**When to choose DenseAppendOnlyFixedSizeTree:**
- The maximum number of entries is known at creation time
- You need every position (including internal nodes) to store data
- You want the simplest possible data model with no unbounded growth
- O(n) root hash recomputation is acceptable (small tree heights)

**When NOT to choose it:**
- You need unlimited capacity → use MmrTree or BulkAppendTree
- You need ZK compatibility → use CommitmentTree

## Usage Example

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

## Implementation Files

| File | Contents |
|---|---|
| `grovedb-dense-fixed-sized-merkle-tree/src/lib.rs` | `DenseTreeStore` trait, `DenseFixedSizedMerkleTree` struct, recursive hash |
| `grovedb-dense-fixed-sized-merkle-tree/src/proof.rs` | `DenseTreeProof` struct, `generate()`, `encode_to_vec()`, `decode_from_slice()` |
| `grovedb-dense-fixed-sized-merkle-tree/src/verify.rs` | `DenseTreeProof::verify()` — pure function, no storage needed |
| `grovedb-element/src/element/mod.rs` | `Element::DenseAppendOnlyFixedSizeTree` (discriminant 14) |
| `grovedb-element/src/element/constructor.rs` | `empty_dense_tree()`, `new_dense_tree()` |
| `merk/src/tree_type/mod.rs` | `TreeType::DenseAppendOnlyFixedSizeTree = 10` |
| `merk/src/tree_type/costs.rs` | `DENSE_TREE_COST_SIZE = 6` |
| `grovedb/src/operations/dense_tree.rs` | GroveDB operations, `AuxDenseTreeStore`, batch preprocessing |
| `grovedb/src/operations/proof/generate.rs` | `generate_dense_tree_layer_proof()`, `query_items_to_positions()` |
| `grovedb/src/operations/proof/verify.rs` | `verify_dense_tree_lower_layer()` |
| `grovedb/src/operations/proof/mod.rs` | `ProofBytes::DenseTree` variant |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs` | Average case cost model |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs` | Worst case cost model |
| `grovedb/src/tests/dense_tree_tests.rs` | 22 integration tests |

---
