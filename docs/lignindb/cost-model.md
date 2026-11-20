# Cost model: what today's code charges

LigninDB task BASE-06 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

This document specifies what GroveDB charges today, as an `OperationCost`, for each logical event: opening a subtree, fetching a node, writing or removing a record, moving a cursor. Each charge is given as a function of logical sizes (lengths of keys and values, the fields of a node), not of bytes on disk. SEAM-05 moves the storage part of these rules into versioned functions that both engines call. FMT-02 and MERK-03 build an engine that keeps no node record and still has to produce every one of these numbers.

It builds on the [storage surface inventory](storage-surface.md) (BASE-04), the [FlatStore seam](flat-store.md) (SEAM-03), the [NodeStore seam](node-store.md) (SEAM-01) and rule GM0 of the [transaction rules](transactions.md) (TXN-01). The cost codes of the inventory (`P-path`, `R-tx`, `B-commit` and so on) are named next to each rule.

**Pinned commit:** the engine sources of `feat/LigninDB` at `54e36fb4` ("Merge BASE-01: the remaining cost-bearing operations"), which are those of `develop` `22f163c2`. Functions are cited by name, with the line at the pinned commit. Since then `develop` has been merged three times, up to `f1016919`, up to `2a79d035` and up to `2cfc1ce1`, and the golden corpus recorded again from each (`lignin-testkit/corpus/README.md`). The rules below predict every cost of the corpus recorded from `2cfc1ce1` as they did of the first, and the citations have not been moved.

**Since SEAM-05** the storage rules of section 5 are the functions of `grovedb_costs::cost_model`, and the RocksDB engine charges only through them. Section 14 says which function is which rule. Section 15 says how the LigninDB store, which keeps no node record, charges through the same functions (MERK-03). The citations of section 5 are of the code at the pinned commit, where the rules were written out in the storage contexts.

**Result:** the storage rules of section 5, applied to a logical state that holds no node record, reproduce every `OperationCost` of the golden corpus: 380 traces and 229,526 steps on grove versions 1 to 5 at the standard scale, and the five reference traces of 1,050,008 operations each. Section 12 says how that is run, what it shows and what it does not. One thing it does not show: the corpus cannot tell whether a delete with no writer's cost is charged from the committed state or from the transaction's view (rule D2), because the two differ only for a record the same transaction wrote, and the corpus deletes such records only in an operation whose cost GroveDB drops. The storage crate's tests pin that instead.

## 1. How to read this document

A cost has three sources, in three layers:

| Layer | Crates | What it charges | After LigninDB |
| --- | --- | --- | --- |
| Storage | `grovedb-storage` (`rocksdb_storage`), with one rule in `grovedb-costs` | Seeks, loaded bytes, the prefix hash of a subtree handle, and the storage cost of each record written or removed | Replaced. The rules of section 5 become functions both engines call (SEAM-05). |
| Merk | `grovedb-merk` | Which nodes are fetched, every hash of a node, and the writer's account of what a node's key and value cost | Stays. Merk's AVL logic, hash rules and cost rules remain the source of truth (plan, "Architecture"). |
| Above Merk | `grovedb`, the append-only tree crates | Which subtrees an operation opens, what it reads and writes, and a small number of charges of their own | Stays. |

Sections 5 and 4 are the specification of the first layer. They are what a new engine has to reproduce, and they are what the check of section 12 computes from sizes alone. Sections 6 and 7 describe the other two layers: what they pass down, and every place where they add to a cost themselves. That code runs unchanged on both engines, so it is described here, not re-specified.

Section 9 is for FMT-02: for each charged quantity, the sizes it depends on and what an engine has to keep. Section 10 is for SEAM-05, and section 14 is what SEAM-05 did with it.

## 2. The cost record

`OperationCost` (`costs/src/lib.rs`) has five parts:

| Field | Counts |
| --- | --- |
| `seek_count` | Point reads, writes and cursor moves, one each unless a rule says otherwise |
| `storage_loaded_bytes` | Bytes read: the length of a record, of a stored key, or a fixed figure |
| `storage_cost` | `added_bytes`, `replaced_bytes` and `removed_bytes` of the records written or removed |
| `hash_node_calls` | Blake3 compressions, counted in 64-byte blocks |
| `sinsemilla_hash_calls` | Sinsemilla hashes of a commitment tree |

Costs add field by field, saturating. `removed_bytes` is one of three forms: none, a plain number (`BasicStorageRemoval`), or a number per owner and epoch (`SectionedStorageRemoval`). The storage layer only ever produces the first two; a sectioned removal reaches it inside a writer's cost and is added as it is. Adding a plain removal to a sectioned one is not the same in every grouping and order (`costs/src/storage_cost/removal.rs`, and the version switch of section 8), so where a rule adds several storage costs it says in which order they meet the running cost.

A fallible call returns its cost whether it succeeds or not (`CostResult`). Where a rule charges part of a cost only on success, it says so.

## 3. Terms

- **Subtree.** One Merk or one append-only tree. It is named by a 32-byte **prefix**. The root subtree has the all-zero prefix. Any other subtree's prefix is the Blake3 hash of its path (rule P1), and the secondary of an indexed tree has a prefix derived from its primary's (rule P3).
- **Keyspace.** A subtree has four: **data** (Merk nodes, or the records of an append-only tree), **aux**, **roots** (the root key of a Merk that is not under a parent element) and **meta**.
- **Key.** The caller's bytes. **Stored key** is the prefix followed by the key, so its length is `32 + key_len`. Today's engine stores records under stored keys, and several charges are defined on that length whatever an engine stores.
- **Record.** What a keyspace holds under a key. In the data keyspace of a Merk it is a node; everywhere else it is the caller's bytes, of length `value_len`.
- **Record length of a node.** The number of bytes today's engine stores a node in. Section 4 gives it as a function of the node's fields. No rule needs the bytes.
- **Deferred handle** and **immediate handle.** The two ways a subtree is opened inside a transaction (inventory: transactional and immediate context). A deferred handle stages its writes in a batch that is charged when it is applied. An immediate handle writes at once. GroveDB uses immediate handles only in state-sync restore.
- **Committed state.** What the database holds outside any open transaction at the moment of a call. A **snapshot read transaction** reads the committed state of the moment it was started, whatever is committed later, and refuses writes.
- `varint(n)` is the length of `n` as an unsigned LEB128 varint: 1 below 128, 2 below 16,384, and so on. A signed number is zigzag-encoded first, so `varint_signed(s) = varint((s << 1) ^ (s >> 63))`.
- `blocks(n)` is the number of 64-byte Blake3 blocks of an `n`-byte message: 1 for `n = 0`, otherwise `1 + (n - 1) / 64`.

## 4. The record length of a node (N1)

Today a node is stored as one record under its key (`TreeNode::encode`, `merk/src/tree/encoding.rs`):

```text
record    = side(left) side(right) feature kv_hash value_hash value
side      = 0x00                                   no child on this side
          | 0x01 link
link      = key_len:1 key hash:32 left_height:1 right_height:1 aggregate
feature   = aggregate                              the node's own contribution
aggregate = tag:1 payload
kv_hash, value_hash = 32 bytes each
value     = the rest of the record
```

The node's own key is not in the record. The aggregate of a link and the feature of a node use the same tags:

| Tag | Aggregate (`AggregateData`), feature (`TreeFeatureType`) | Payload | Cost class |
| --- | --- | --- | --- |
| 0 | none, `BasicMerkNode` | empty | none |
| 1 | `Sum`, `SummedMerkNode` | signed varint | one varint, 8 |
| 2 | `BigSum`, `BigSummedMerkNode` | 16 bytes | wide, 16 |
| 3 | `Count`, `CountedMerkNode` | varint | one varint, 8 |
| 4 | `CountAndSum`, `CountedSummedMerkNode` | varint, signed varint | two varints, 16 |
| 5 | `ProvableCount`, `ProvableCountedMerkNode` | varint | one varint, 8 |
| 6 | `ProvableCountAndSum`, `ProvableCountedSummedMerkNode` | varint, signed varint | two varints, 16 |
| 7 | `ProvableSum`, `ProvableSummedMerkNode` | signed varint | one varint, 8 |
| 8 | `ProvableCountAndProvableSum`, `ProvableCountedAndProvableSummedMerkNode` | varint, signed varint | two varints, 16 |
| 9 | `ProvableCountAndProvableBigSum`, `ProvableCountedAndProvableBigSummedMerkNode` | varint, 16 bytes | varint and wide, 24 |
| 10 | `CountAndBigSum`, `CountedBigSummedMerkNode` | varint, 16 bytes | varint and wide, 24 |
| 11 | `ProvableBigSum`, `ProvableBigSummedMerkNode` | 16 bytes | wide, 16 |

`payload_len` is the length of the payload: it depends on the values of the counts and sums, through their varints. The **cost class** is the fixed number of bytes the feature is charged as in a storage cost, whatever its varints take (`TreeCostType`, rule K2).

So, with `link_len(child) = 36 + child_key_len + payload_len(child aggregate)` and 0 for a side with no child:

```text
record_len = 2
           + link_len(left) + link_len(right)
           + 1 + payload_len(feature)
           + 64
           + value_len
```

The inputs are the value's length, the kind and numbers of the node's feature, and for each child the length of its key and the kind and numbers of the aggregate of the subtree under it. A leaf with no feature and a one-byte value is 68 bytes.

The record length changes when nothing about the node's own value does: a child appears or goes, a rotation gives it another child with a key of another length, or a count or sum below it crosses a varint boundary.

**Where it is charged.** Rule R1 (every node fetch), rule C3 (a cursor reads a node), rule D2 (a node removed with no writer's cost), rule K2 for a node written with no link sizes (a raw copy, or chunk restore), and the pinned-view read of section 6.5. A node written by a Merk commit is not charged by its record length: the links cancel out of rule K2.

**N2.** With a node, Merk passes the storage layer the sizes of its links and feature (`NodeWriteCost`, `ChildrenSizesWithIsSumTree`): for each child `(child_key_len + 36, payload_len)`, and for the feature its cost class and `payload_len`. They are the node's own sizes as defined above. An engine that holds the node's fields does not need them handed over. The check of section 12 compares the two on every put that comes with sizes, and counts a record that comes with sizes and is not a node as a difference.

## 5. Storage rules

Each rule names the code that implements it today and the inventory's cost code. `lignin-testkit/src/cost_model/rules.rs` has each rule as a function with the same name as here.

### 5.1 Subtree handles

**P1. A prefix derived from a path** (`P-path`; `RocksDbStorage::build_prefix`, `storage/src/rocksdb_storage/storage.rs:437`).

```text
hash_node_calls += 0                                    for the empty path
                   blocks(sum(segment_len) + 8 + n)     for a path of n >= 1 segments
```

The hashed message is the segments, then the number of segments as a native-endian machine word (8 bytes on every supported platform), then one length byte per segment. A segment is at most 255 bytes. Charged every time a handle is opened by path, deferred or immediate, and every time GroveDB derives a prefix without opening a handle. Nothing is cached: opening the same path twice charges twice.

**P2. A handle opened by a prefix** the caller already holds (`zero`): nothing.

**P3. The prefix of a secondary of an indexed tree** (`P-sec`; `RocksDbStorage::secondary_prefix_for`, `:475`): `hash_node_calls += 1`. The message is the primary prefix and one axis tag byte.

Opening a handle reads nothing. A Merk that is then opened on it fetches its root (section 6.1).

### 5.2 Point reads

**R1. A read through a deferred handle** (`R-tx`; `get`, `get_aux`, `get_root`, `get_meta` in `storage_context/context_tx.rs:308`), in any keyspace:

```text
seek_count           += 1
storage_loaded_bytes += the record's length, 0 if there is none
```

For a node the length is `record_len` of section 4. A read of a key with no record still charges the seek. The read sees the transaction's applied writes over the committed state (for a snapshot read transaction, the state of its start), and never a write staged in a batch that has not been applied.

**R2. A read through an immediate handle** (`R-imm`): nothing.

### 5.3 Writes through a deferred handle

A write is staged in the batch its handle was opened with and charges nothing at the call (`W-tx`). It is charged when the batch is applied (`B-commit`; `RocksDbStorage::continue_write_batch`, `storage.rs:542`). A write through a deferred handle that was opened with no batch is refused and charges nothing.

**W1. The stored key of a new data record.** When a put into the **data** keyspace carries a writer's cost with `new_node` set, the writer left the key's cost at zero, and the stored key is added at staging:

```text
cost_info.key_storage_cost.added_bytes += (32 + key_len) + varint(32 + key_len)
```

This is done by the handle's `put` (`context_tx.rs:124`) and by the `put` of both batch types (`storage_context/batch.rs:36`, `:158`). It is not done for aux, meta or roots puts, and not for an update.

**W2. A put into data, aux or meta.**

```text
seek_count   += 1
storage_cost += K(32 + key_len, value_len, link sizes, cost_info)     on success
```

`value_len` is the record's length, for a node `record_len`. Link sizes are passed only with a node written by a Merk commit; aux and meta puts never have them.

**W3. A put into roots.** `seek_count += 1`. The storage cost K is added only when the writer gave a cost; with none the put is free of storage cost.

**W4. A prepaid put.** When the writer's cost is `KeyValueStorageCost::prepaid()`, the put charges nothing, the seek included, in any keyspace. Only the append-only trees issue such puts, from grove version 4 (section 7.3).

**The storage cost of a put, K** (`OperationCost::add_key_value_storage_costs`, `costs/src/lib.rs:191`). `stored_key_len = 32 + key_len`.

The key's cost is added to the running cost of the batch first, then the value's, each as a `StorageCost` of its own.

- **K1, the key.** With no writer's cost: `added_bytes += stored_key_len + varint(stored_key_len)`. With one, the writer's `key_storage_cost` is added as it is.
- **K2, the value.** The paid length of the value is

  ```text
  with no link sizes:   value_len + varint(value_len)
  with link sizes:      body + varint(body) + stored_key_len + 4 + class
        body  = value_len - 2 - link_len(left) - link_len(right) - payload_len(feature) + class
        class = the feature's cost class, 0 with no feature
  ```

  Every subtraction saturates at zero. For a node, `value_len` is `record_len`, so `body = 65 + class + value_len(of the node's value)`: the links cancel. The trailing terms are what the parent's link to this node takes. With no writer's cost the paid length is `added_bytes`. With one, the writer's `value_storage_cost` is added as it is.
- **K3, the checks.** With a writer's cost:
  - if `new_node`, `key.added_bytes + key.replaced_bytes` must equal `stored_key_len + varint(stored_key_len)`;
  - `value.added_bytes + value.replaced_bytes` must equal the paid length of K2. When the writer cleared `needs_value_verification`, the paid length is taken to be that sum, so the check always passes and the record's length is not looked at.

  A failed check refuses the batch with `CostError(StorageCostMismatch)`.

**D1. A delete with a writer's cost.** `seek_count += 1`; on success `removed_bytes += key.removed_bytes + value.removed_bytes` of that cost: the two are added to each other first, and their sum to the running cost. Nothing is read.

**D2. A delete with no writer's cost.** It is charged from the **committed state**, not from the transaction:

```text
seek_count           += 2
storage_loaded_bytes += v
removed_bytes        += (32 + key_len) + v + varint(32 + key_len) + varint(v)     on success
      v = the length of the record the committed state holds under the key, 0 if it holds none
```

What the transaction itself has written does not count. A key it created and has not committed is absent: two seeks, no loaded bytes, and the stored key with its varint plus one byte removed. A record it replaced is charged at the length of the committed record it replaced, not of the one it wrote. For a node, `v` is the `record_len` of the node as committed. This is the one read of the write path that does not go through the transaction. It is the committed read of the transaction rules ([transactions.md](transactions.md), RD10; GM0 says where it is made).

It is what these are charged on a deferred handle: every record removed by `clear` (C4), which is how GroveDB empties the keyspace of a deleted or cleared append-only tree and of an indexed tree's secondaries; every node removed by `Merk::clear`; every node removed by the visitor that deletes a subtree without opening its Merk (`delete_internal_on_transaction/v2.rs`); `delete_aux` when the caller gives no cost; the meta deletes; and the removal of a base root key outside a commit (`set_base_root_key(None)`).

**B1. Applying a batch** (`commit_multi_context_batch`, `storage.rs:1110`).

- A batch holds one final operation per keyspace and stored key, not a log (`StorageBatch`, `storage/src/storage.rs`). Within one operation a put wins over a delete of the same key. After `next_operation`, a later delete replaces an earlier put. A later put always replaces. Only the surviving operation is charged.
- Operations are charged in the order meta, aux, roots, data, and by stored key within each. Their storage costs are added to one running cost in that order, a put's key cost before its value cost. The order shows in the result: on grove versions 1 to 3 a sectioned removal added to a plain one drops the default owner's section, and a plain one added to a sectioned one keeps it (`costs/src/storage_cost/removal.rs`, `AddAssign`).
- Seeks and loaded bytes are charged as each operation is taken from the batch. Storage costs are held back and added only if every operation was accepted and the engine took the writes. A refused put (K3) keeps the seeks and loaded bytes charged up to and including its own, and none of the storage costs.
- Applying the batch is itself free, and so is merging a Merk commit's part into it (`B-part`). An empty batch charges nothing, so an operation that writes nothing ([transactions.md](transactions.md), GM0: a conditional insert that finds its key taken, an operation that fails before it applies its batch, any read) is charged its reads and hashes only.

### 5.4 Writes through an immediate handle

**M1. A put or delete made directly** (`W-imm`): nothing. The writer's cost is ignored.

**M2. The batch of an immediate handle** (`B-imm`; `PrefixedRocksDbBatch`, `batch.rs:36`) charges each operation as it is added and returns the sum when it is committed:

- a put is W1, W2, W3 or W4 with both parts charged at once;
- a delete is `seek_count += 1`, and the writer's removed bytes when it gave a cost. With no cost nothing more is charged and nothing is read. This differs from D2.

`Merk::commit` returns that sum only when `merk_versions.batch.commit >= 1`, which is grove version 3 and later. On grove versions 1 and 2 it drops what `commit` returned (`merk/src/merk/mod.rs:498`). On a deferred handle the dropped cost is zero (the writes are charged by B1), so the version makes no difference there. On an immediate handle it is the whole write cost.

### 5.5 Cursors

A cursor (`RawIterator`; `storage_context/raw_iterator.rs`) walks one keyspace in the order of stored keys, across subtrees. It is **on a record** while it stands on a key of the subtree it is scoped to. Opening one is free.

**C1. Any move**: `seek_to_first`, `seek_to_last`, `seek`, `seek_for_prev`, `next`, `prev` (`I-move`): `seek_count += 1`.

**C2. `key()` and `valid()`** (`I-key`):

```text
storage_loaded_bytes += 32 + key_len     on a record
                        288              otherwise
```

288 is charged alike when the cursor stands on another subtree's key and when it has run off either end, so the charge does not depend on which subtrees are neighbours.

**C3. `value()`** (`I-value`): C2, and on a record also the record's length (for a node, `record_len`).

**C4. `clear`** (`CLR`; `FlatStore::clear`, `storage/src/flat.rs:110`) is defined by the others: one C1 to the first record, then for each record C2 twice (`valid`, `key`), one delete with no writer's cost and one C1, and a last C2 for the `valid` that ends the walk.

- It clears the keyspace of the handle it is called on and no other: the data keyspace for a subtree handle, the aux or the meta keyspace for the handles a subtree handle gives out (`storage_context/flat.rs`).
- On a deferred handle each delete is staged and charged by D2 when the batch is applied.
- On an immediate handle each delete is made at once and is free (M1), so `clear` costs the walk and nothing else.

A cursor sees the committed state of the moment it was opened, under the writes its transaction has applied. In a snapshot read transaction it sees the state of the transaction's start instead, so a commit between the start and the opening of the cursor does not change what it reads or what it loads.

### 5.6 What is free

Starting, committing and rolling back a transaction, savepoints, flush, checkpoints, `wipe`, `delete_prefix_ranges`, listing and removing pending-drop records, and the `StorageBatch` bookkeeping charge nothing (`zero`, `DB`). `get_storage_context_cost` (`EST`) charges nothing either: it returns P1 computed from the longest length each segment may have, for estimates. It is a function of `grovedb_storage::worst_case_costs` and of no engine (section 14.7).

## 6. Merk's rules

These are made above the storage layer, in `grovedb-merk`, and stay where they are. They are listed so that the storage rules can be read in context, and so that every origin of a cost is written down.

### 6.1 Which nodes are fetched

A Merk keeps the nodes it has loaded in memory and fetches each other node once, through `NodeStore::fetch_node` or `get_node`: **one R1 per fetch**. No store answers a fetch from a node it kept (node-store.md, "Reads").

- **Opening.** A layered Merk (every subtree but the root) takes its root key from its parent element and fetches that node: one R1. A base Merk (the root subtree) first reads its root key from the roots keyspace, one R1 with `value_len` the key's length, then fetches the node. An empty Merk fetches nothing; a base Merk still pays the read of the absent root key.
- **A write** walks from the root to the key, fetching each node on the way that is not loaded. Rebalancing fetches the child a rotation moves. Removing a node with two children walks down the edge of the taller side and fetches each node there.
- **A read by key** (`Merk::get` and its relatives; `get_node_fn`, `merk/src/merk/get.rs:425`) walks the loaded part of the tree and, at the first link that is not loaded, reads the key directly: one R1 for the key itself, found or not, and none for the nodes between.
- **A proof** or a chunk walks the tree through the same fetches. A proof asks for a node with `NodeStore::fetch_node_without_value`, which a store may answer without the node's value: it is **one R1 all the same, with the length of the whole record** (section 15.1). A node that comes without its value is not kept in the Merk's tree. The Merk keeps its key instead and is not charged for that node again, by a later proof, by a write that walks over it or by a read of the nodes in memory, so that a Merk is charged the fetches it would be charged if it kept every node (node-store.md, "Proofs").
- **Pruning.** After a commit a Merk keeps its loaded nodes (`MerkCommitter::prune` keeps the top 100 levels), so a Merk that stays open does not fetch them again. GroveDB opens a Merk per operation, or per batch through a cache, so what is loaded is decided by the code above Merk.

Which nodes that is depends on the shape of the tree: the keys, the links and the heights the links carry. Those are part of the logical state.

### 6.2 Hashes

`hash_node_calls` is added by the hash functions themselves (`merk/src/tree/hash.rs`):

| Function | Charge |
| --- | --- |
| `value_hash(value)` | `blocks(varint(value_len) + value_len)` |
| `kv_digest_to_kv_hash(key, value_hash)` | `blocks(varint(key_len) + key_len + 32)` |
| `kv_hash(key, value)` | the two above |
| `node_hash`, `node_hash_with_count`, `_with_sum`, `_with_count_and_sum`, `_with_count_and_big_sum` | 2 |
| `combine_hash` | 1 |
| `combine_hash_three` | 2 |
| `axes_digest(n axes)` | `ceil((1 + 33 n) / 64)` |

A value put on a node hashes the value and the key-value pair. A value that refers to something else (a tree's root hash, a reference's target) also combines. A commit hashes every node the write changed below the root once (`TreeNode::commit`, `merk/src/tree/mod.rs:1964`); the root's own hash is computed when it is asked for. These depend on key and value lengths only.

### 6.3 The writer's cost of a node

For every node a commit writes, Merk works out a `KeyValueStorageCost` and passes it down (`MerkCommitter::write`, `TreeNode::kv_with_parent_hook_size_and_storage_cost`, `merk/src/tree/mod.rs:285`).

**The value's cost** is one of three (`merk/src/tree/kv.rs`). The kind is normally that of the element; what decides it exactly is said below the table. `class` is the feature's cost class, and `link = key_len + 36 + class`:

| Kind | Cost | Checked by K3 |
| --- | --- | --- |
| ordinary (no value-defined cost: items, references) | `v + varint(v) + link`, with `v = value_len + 65 + class` (`node_value_byte_cost_size`, `:366`) | yes |
| specialized (sum items and the like: the element defines `c` in place of `value_len`) | the same with `value_len = c` | no |
| layered (trees: the element defines `c`) | `c + class + 35 + link` (`layered_value_byte_cost_size_for_key_and_value_lengths`, `:427`) | no |

The ordinary cost is exactly the paid length K2 derives from the record, which is why K3 can check it. A value-defined cost is taken as given (`needs_value_verification` is false): the element's own cost size (`merk/src/tree_type/costs.rs`) stands in for its length.

**Which kind applies** is decided by the node as Merk holds it, by whether it carries a value-defined cost (`KV::value_defined_cost`), and not always by the element that is written:

- A specialized or a layered put stamps the node with the cost the operation names.
- A node fetched from storage is stamped from the element it holds: a sum item or a tree gives it a value-defined cost, an item or a reference none.
- An ordinary put (`Op::Put`, `Op::PutCombinedReference`) clears the stamp from grove version 4 (`merk_versions.tree.put_value` 1). On grove versions 1 to 3 it keeps the one the node was fetched with (`merk/src/tree/put_value/v0.rs`). An item or a reference written over a sum item or a tree is then charged from the predecessor's fixed cost size, and its cost is not checked.

`needs_value_verification` is set exactly when the node carries no value-defined cost. The rest of this document says "ordinary" for such a node.

**New or update.** A node with no mutation baseline is new: its key cost is left at zero for W1 to complete, `new_node` is set, and the value's cost is `added_bytes`. A node that was loaded is an update: the key costs nothing, and the value's cost is split against the cost of the value it replaces (`storage_cost_for_update`, `:257`):

```text
old == new:   replaced = old
old >  new:   replaced = new,  removed = old - new
old <  new:   replaced = old,  added   = new - old
```

The old cost is worked out from the old value by the same three formulas (the callback `old_specialized_cost` for value-defined costs). A node whose value did not change but whose links or hashes did is such an update with `old == new`: it is charged a seek and its whole value cost as `replaced_bytes`.

**Flags.** The client's callbacks may rewrite the flags of the new value according to the cost, split removed bytes by owner and epoch, or have an update accounted as a removal and an insertion. Section 6.6 says where they enter.

**Removal.** A removed node is deleted with a writer's cost (D1): the key `stored_key_len + varint(stored_key_len)` and the value's cost as above, both as removed bytes, possibly sectioned (`merk/src/tree/ops.rs:744`).

**Moved nodes.** A node moved to another key (`PutMoved`, `DeleteMoved`) carries a cost that moves the bytes without adding or removing them (`moved_storage_cost`).

### 6.4 The root key and side records

- A base Merk's root key record is written through W3 with no cost, so it is charged a seek and nothing else. A standalone Merk gives `KeyValueStorageCost::for_updated_root_cost` (key 34 bytes; value the root key's length with its varint, split as an update against the previous root key's). GroveDB opens no standalone Merk.
- When a base Merk becomes empty its root key record is deleted, with an empty writer's cost (D1, one seek, nothing removed) when `base_root_storage_is_free`, otherwise with none (D2).
- Aux records of a commit pass the caller's cost through. `set_base_root_key` and the meta records write with no cost (W3 with none, W2 with none, D2).
- `Merk::clear` (`merk/src/merk/clear.rs:17`) is the walk of C4 with a `valid`, a `key` and a delete with no cost per node. The deletes go into a change set that is committed at the end. On a deferred handle each node is therefore charged by D2 at its committed record length. On an immediate handle the change set is the handle's batch, so each node is charged by M2: one seek, nothing loaded and nothing removed.
- `is_empty_tree` is one C1 and one C2.

### 6.5 Reads that replace or skip the storage charge

- **An element read by key** (`Element::get_optional_from_storage`, `merk/src/element/get.rs:109`). It makes one R1 and then **replaces** `storage_loaded_bytes` with a figure worked out from the element, by its kind. `L` is the element's serialized length, and `link = key_len + 36 + class` as in section 6.3:

  | Element | Loaded bytes |
  | --- | --- |
  | items and references | `L + varint(L) + link` (`value_byte_cost_size_for_key_and_value_lengths`). This is not the writer's cost of section 6.3: the 65 bytes of the node's hashes and feature tag are not in it. |
  | sum items | the specialized cost of section 6.3, with `c = 11 + flags` (`flags` is the flags' length with its varint) |
  | items with a sum item | from grove version 2 the specialized cost with `c = item_len + varint(item_len) + 11 + flags`; on grove version 1, as an item of length `L + 11` |
  | trees of every kind | the layered cost of section 6.3, with `c` the tree type's cost size plus `flags` |

  A wrapped element (`NonCounted` and the like) adds one byte to `c`. On grove version 1 the node is taken to have no feature, whatever its tree; from grove version 2 its own feature's class is used. The seek stays. A key with no record keeps R1 as it is. So this read does not depend on the node's links. GroveDB reads this way the parent element from which it opens a subtree's Merk (`open_transactional_merk_at_path`) and the rows of an indexed tree. An element read through an open Merk, as `GroveDb::get` reads the element it returns, is not: it is charged as section 6.1 says, R1 as it stands.
- **A pinned-view read** (`stored_node_bytes`, `merk/src/proofs/query/count_offset/pinned_view.rs:88`; inventory finding 10). It seeks a cursor to the key and reads the key and value without billing the cursor's own C2 and C3. It charges the seek (C1) and, when the key is there, the record's length: the same as R1 on a found record.
- **Costs a caller drops.** The inventory marks the library sites that discard a returned cost. `Merk::commit` on grove versions 1 and 2 is one (M2). `GroveDb::clear_subtree` is another (section 11).

### 6.6 The client's two callbacks: storage flags and sectioned removals

`apply_batch_with_element_flags_update` and `apply_partial_batch_with_element_flags_update` take two closures from the client. The other entry points pass closures that leave flags alone and remove bytes as plain numbers.

- `flags_update(cost, old_flags, new_flags)` may rewrite the flags of the element being written, knowing the storage cost of its value, and says whether it did. Drive merges its epoch-based storage flags here.
- `split_removal_bytes(flags, key_bytes, value_bytes)` turns the bytes a write frees into removals by owner and epoch. Drive splits them by the epochs the flags name.

**Where they run.** Inside Merk, while a subtree's operations are applied and before anything is handed to the storage layer: on a node whose value is replaced (`just_in_time_tree_node_value_update`, `merk/src/tree/just_in_time_value_update/`) and on a node that is removed (`merk/src/tree/ops.rs:744`). GroveDB wraps the client's closures (`grovedb/src/batch/just_in_time_value_update.rs`): it decodes the old and the new element to reach their flags and encodes the new element again when its flags changed. A node that is new does not go through the update. The flags callback is not called for a new element that has no flags, and the split is not called for an old element that has none: its bytes are removed as plain numbers.

**What they are given.** The old value is the one that came with the node Merk fetched on its way to the key (one R1, section 6.1). The cost is the value's storage cost of section 6.3, computed from the lengths of the old and the new value. Nothing is read for a callback and nothing is charged for calling it: no seek, no loaded byte, no hash.

**What they change, all of it above the storage layer:**

1. **The bytes of the new value**, through its flags, and with them its length and its cost (the loop below). The value hash is taken over the final bytes (section 6.2).
2. **The form of the removed bytes.** A plain removal of the value's bytes goes through `split_removal_bytes` with the old element's flags and comes back plain or sectioned. On a delete both halves are split: the key's `stored_key_len + varint(stored_key_len)` and the old value's cost.
3. **From grove version 5, how an update is accounted**: the callback may answer `AccountAsReplacement`, and the update is then charged as the removal of the old element and the insertion of the new one. On grove versions 1 to 4 Merk refuses that answer. On every version GroveDB refuses it, and the batch with it, for a write whose flags are byte for byte the stored ones (`batch_flags_update`).

**The loop** (`v0.rs`, `v1.rs`; `merk_versions.tree.just_in_time_value_update`, section 8). Merk first measures the value's cost. On a node with no value-defined cost (section 6.3, "Which kind applies") that first measurement is taken with the new value carrying the *old* flags, when the old value has any. On a node with one it is that value-defined cost, which on grove versions 1 to 3 may be the predecessor's. Then, in rounds bounded by `MAX_UPDATE_VALUE_BASED_ON_COSTS_TIMES` (8), a plain removal in the cost is split and the flags callback is asked with the cost.

- **If it answers that it changed the flags:** when the value with those flags has the size last measured, the loop ends and the value keeps those flags. Otherwise Merk measures again from those bytes, puts the new value back as it was given, and asks again with the new cost.
- **If it answers that it changed nothing,** the loop ends, and the versions differ in the measurement they keep. Version 0, on grove versions 1 to 3, keeps the last one as it is; in the first round that is the one taken with the old flags. Version 1, on grove version 4, measures once more from the bytes it stores when their size is not the one last measured, and does not ask again. Version 2, on grove version 5, is version 1 with the third point above.

So on grove versions 1 to 3 a new value that comes with flags of another length than the stored ones, and whose flags the callback leaves alone, is handed down with a cost that is not the cost of the bytes written. When the node has no value-defined cost the storage layer notices: K3 compares `added + replaced` with the bytes and refuses the commit (GroveDB's `grove_v3_refuses_a_replacement_keeping_flags_of_another_length`). When it has one, the cost is not checked (`needs_value_verification` is false), so whatever the update measured is charged. On grove versions 1 to 3 that includes an item or a reference written over a sum item or a tree.

The same update is run ahead of the apply, on a detached node, wherever a batch has to know the final bytes of a value before it writes them (`predict_put_final_bytes`; on grove versions 1 to 3 a hand-written copy in `batch/just_in_time_reference_update/v0.rs`). Two places do: the preprocessor of backward references, and a reference to an item that the same batch writes, which commits to the item's value hash. Both call the client's callbacks again, on values that were already read. The prediction itself charges nothing; the second place charges the value hash of the predicted bytes (section 6.2). The storage cost worked out on the way is only what the callback is shown.

**What the storage layer is asked for.** Only the result: the final bytes of the value and one `KeyValueStorageCost`, with the put (W2) or the delete (D1). The storage layer then

- checks `added + replaced` of the value against the paid length of K2 when the cost asks for it (K3: `needs_value_verification`, set when the node carries no value-defined cost); that is what catches version 0's stale measurement on such a node;
- adds the removed bytes as they come, plain or sectioned, in the order of B1;
- never calls a callback, never produces a sectioned removal itself, and removes only plain numbers of its own (D2).

So for SEAM-05 nothing of the callbacks moves: the cost functions take the writer's cost as an input, and what they have to preserve is K3 and the order in which storage costs meet the running cost (B1), because of the arithmetic of grove versions 1 to 3. For GROVE-03 the rule is that the callbacks need, from an engine, the old value's bytes with the node that is fetched for a write, and nothing else (section 9).

## 7. Charges made above Merk

A search of the library code of every crate above the storage layer for statements that create a cost, rather than pass one on, finds 109 sites in 28 files (the pattern is in section 12.5). Besides the Merk rules above they are:

### 7.1 GroveDB

| Site | Charge |
| --- | --- |
| `GroveDb::add_element_to_cached_merk` (`operations/insert/add_element_on_transaction/v0.rs`, `v1.rs`, `v2.rs`) | `hash_node_calls += 2` when a private-document store is created: its config hash and empty state root |
| `non_merk_tree_child_hash` (`operations/proof/bind_terminal_non_merk_tree/v1.rs`) | `hash_node_calls += 2` for the same two hashes of an empty store |
| the range walk of a path query (`element/query.rs`, `path_query_push`) and of a sum-budget walk (`element/aggregate_sum_query/mod.rs`) | `seek_count += 1` after each cursor step, on top of the step's own C1 |
| `moved_record_cost`, `copy_next_record` (`batch/move_element.rs`) | the writer's cost of a record copied from one subtree to another: the stored key and the record, each with its varint, as `replaced_bytes`. The value half is checked (K3). The copy is a W2 put with no link sizes, so a node is charged by its `record_len`. The walk charges, per record, C2 twice, C3 once and C1 once. The source record is deleted with an empty writer's cost: D1, one seek and nothing removed, not D2. |
| the prediction of a reference's target in the same batch (`batch/just_in_time_reference_update/v0.rs`, `v1.rs`) | `hash_node_calls` of one `value_hash` over the bytes the target will hold after the flags callback (section 6.6). The storage cost it works out on the way is shown to the callback and not charged. |
| `GroveDb::bulk_append`, `preprocess_bulk_append_ops` (`operations/bulk_append_tree.rs`), `mmr_tree_append`, `preprocess_mmr_tree_ops` (`operations/mmr_tree.rs`) | add the hash counts the trees report, and one hash for a state root |

`grovedb/src/operations/delete/flat_drop.rs` also builds a cost from sizes, for an estimate; it is not charged to an operation.

Everything else GroveDB charges is the sum of what it calls: P1 for every handle it opens, R1 for every element and node it reads, the cursor rules for every range it walks, B1 for every batch it applies.

### 7.2 Storage flags

`grovedb-epoch-based-storage-flags` turns added and removed bytes into flags and sectioned removals. It creates no cost of its own; it decides how a removal is divided.

### 7.3 The append-only trees

The MMR, bulk-append, dense, commitment and private-document trees keep their records in a flat data keyspace and charge in two ways.

- **Through the storage rules.** Their reads are R1 and their puts W2, with a writer's cost they work out themselves: the MMR's `write_cost_info` (`grovedb-merkle-mountain-range/src/storage_adapter.rs`), the commitment frontier's `frontier_save_cost_info` (`grovedb-commitment-tree/src/commitment_tree/cost/mod.rs`), `KeyValueStorageCost::for_in_place_value_rewrite`.
- **By a model of their own.** From grove version 4 (`append_storage_accounting`, `frontier_cost_model`, `root_maintenance`, `push`, `get_root`, `gen_proof`, `compaction_hash_count`) an append charges a fixed figure for the work its epoch will do: seeks, loaded bytes, replaced and added bytes and hashes computed from the tree's height, the value's length and the epoch size (`grovedb-bulk-append-tree/src/tree/append.rs`, `grovedb-dense-fixed-sized-merkle-tree/src/tree/root_maintenance/v1.rs`). The puts that carry out that work are prepaid (W4) and charge nothing when they happen.
- **Reads served from memory** are charged like the storage read they stand in for: one seek and the record's length (`MmrStore::element_at_position`, `DenseFixedSizedMerkleTree::get_value` and `resolve_record`, the cached path record of the dense root).
- **Hashes.** Each tree counts its own Blake3 calls, and the commitment tree its Sinsemilla calls.

None of these goes through an engine-specific path. A new engine reproduces them by reproducing R1, W2 and W4.

## 8. Grove versions

The storage rules of section 5 are the same on every grove version. One switch decides whether a storage charge reaches the caller:

| Switch | Grove versions | Effect |
| --- | --- | --- |
| `merk_versions.batch.commit` | 0 on 1 and 2; 1 from 3 | Whether `Merk::commit` returns the cost of committing its change set (M2). Zero either way on a deferred handle. |

Above the storage layer, these switches change what an operation is charged. They are listed so that a difference between grove versions in the corpus can be traced:

| Switch | Grove versions | Changes |
| --- | --- | --- |
| `grovedb_versions.element.get_optional_from_storage` | 0 on 1; 1 from 2 | The loaded bytes of an element read (section 6.5) |
| `operations.insert.add_element_on_transaction` | 0, 0, 1, 2, 2 | Which subtrees an insert opens and what it reads |
| `operations.delete.delete_internal_on_transaction`, `clear_subtree` | 2 and 1 from 4 | The same for a delete and a clear |
| `merk_versions.tree.put_value` | 1 from 4 | Whether an ordinary put keeps the value-defined cost of the node it replaces (section 6.3) |
| `merk_versions.tree.just_in_time_value_update` | 0, 0, 0, 1, 2 | The flags update and, on 5, replacement accounting (section 6.6) |
| `apply_batch.same_batch_reference_target_prediction` | 1 from 4 | How the final bytes of a reference's target are predicted (section 6.6) |
| `add_basic_storage_removal_to_sectioned_storage_removal` | 1 from 4 | How a plain removal adds to a sectioned one |
| the append-only tree switches of section 7.3 | 1 from 4 | The trees' own cost model and prepaid puts |
| `path_query_push`, `merge`, the read modes | 1 from 4, 2 on 5 | What a query walks |

## 9. What an engine has to keep (for FMT-02)

Every charged quantity, the sizes it is a function of, and where an engine gets them. "Has" means the engine holds the field for another reason and no extra field is needed.

| Quantity | Charged by | Function of | An engine without node records |
| --- | --- | --- | --- |
| Prefix hash | P1, P3 | Path segment lengths and their number | Nothing to keep. The caller gives the path. |
| Seek | R1, W2, W3, D1, D2, C1 | The event itself | Nothing to keep. One per logical event, whatever the engine does to serve it. |
| Loaded bytes of a node fetch | R1, pinned view | `record_len`: value length; the feature's kind and numbers; per child, its key length and its aggregate's kind and numbers | Has the value, the feature and each child's aggregate (it must store the aggregate to serve Merk). **Must keep the length of each child's key in the parent's link**, also for a child on another page: without it a fetch of the parent would have to read the child. |
| Loaded bytes of a flat, aux, meta or root record | R1 | Value length | Has |
| Loaded bytes of an element read | section 6.5 | Key length, value bytes (to tell the element's kind and flags), the node's feature class | Has. Does not use the links. |
| Key cost of a new node | W1, K1, K3 | Key length | Has |
| Value cost of a node write, checked | K2, K3 | Key length, value length, feature class. The links cancel. | Has. An engine can compute `65 + class + value_len`, its varint, and `key_len + 36 + class` directly, with no record length. |
| Value cost of a node write, as given | K2 | The writer's cost | Nothing. Merk supplies it. |
| Old cost of a replaced or removed value | section 6.3, D1 | Key length; the old value's length or the cost its element defines; the feature class; the old value's flags | **Must hand Merk the old value** with a node it fetches for a write, as the mutation baseline is today. The length alone is not enough: value-defined costs and the split by owner and epoch read the element. A value kept out of line has to be read for this. |
| Removed bytes of a delete with no cost | D2, C4, `Merk::clear` | Key length; the **committed** record's length, for a node its committed `record_len` | **Must be able to read the committed version of a node's sizes inside a write transaction** that has changed or created the node: its value length, feature and links as committed, and whether the key exists in the committed state at all. |
| Cursor key | C2 | Key length, and whether the position is inside the subtree | Has. The 288 figure is a constant. |
| Cursor value | C3 | `record_len` for a node, value length otherwise | As for a node fetch |
| Raw copy of a node (`move_namespace`), chunk restore | W2 with no link sizes | `record_len` of the node written | As for a node fetch |
| Hashes | section 6.2 | Key length, value length | Has |
| Which nodes are fetched | section 6.1 | The tree's shape: each link's child key and heights | Has. Heights are in the link today and Merk balances from them without a fetch. |

So an engine keeps, per node: the key, the value (with its length readable without the value), the feature's kind and numbers, the two hashes, and per child the child's key, hash, two heights and aggregate. That is the list of the plan's tree page cell with one addition made explicit: **the child's key, or at least its length, belongs in every child pointer**, because `record_len` and the link sizes depend on it and Merk's `Link` carries the key. Nothing else has to be stored for costs. In particular an engine does not need the old record, a stored record length, or the varint lengths of the aggregates: they follow from the numbers.

Two things are not sizes of one node and are easy to miss:

1. **`record_len` moves when a descendant changes.** A count or sum that crosses a varint boundary changes the aggregate's `payload_len` in the parent's link, and with it the parent's `record_len`, though nothing in the parent's own value changed. An engine that cached a record length per node would have to update it with every hash-only change. Computing it from the fields avoids that.
2. **D2 needs the committed view, not the transaction's.** A delete with no cost is charged from what was committed when the batch is applied, even when the same transaction rewrote the node first. An engine that only keeps a transaction's newest version of a page cannot answer it.

## 10. For SEAM-05

- **What moves.** Rules P1 to P3, R1, R2, W1 to W4, D1, D2, B1, M1, M2 and C1 to C4, and N1 as the definition of a node's length. Rule K is already engine-neutral code in `grovedb-costs`; it needs the record length as an input and nothing else from an engine.
- **Signatures.** `lignin-testkit/src/cost_model/rules.rs` has one function per rule, taking lengths and returning an `OperationCost` (a pair for the writes of a batch: what is owed at once and what is owed on success). They can be taken as they are or used as the reference for the versioned functions. Their unit tests pin each number.
- **What does not move.** Sections 6 and 7. In particular the stored key's 32 bytes are counted by Merk's own cost rules too (`KV::node_key_byte_cost_size`, the removal cost in `ops.rs`); that is a fee rule, whatever an engine stores.
- **Ordering that is observable.** In two ways. In a batch that is applied, through its removed bytes: the storage costs of its operations are added to one running cost in batch order, and on grove versions 1 to 3 that addition depends on the order when plain and sectioned removals meet (B1). `cost_model/against_rocksdb.rs` has the case, in both orders and across two keyspaces. So the versioned functions have to keep the order of B1, the key before the value within a put, and the batch's storage cost as one sum that joins the caller's cost after the writes are taken. And through a refused put: the cost returned with the error holds the seeks and loaded bytes of the operations before it in batch order. The corpus refuses no put. One engine test does (GroveDB's `grove_v3_refuses_a_replacement_keeping_flags_of_another_length`), and it asserts the error, not the cost. The cost is compared with RocksDB for one batch in `cost_model/against_rocksdb.rs`: a meta put, a refused aux put and a data put that is never reached.
- **Where a cost is reported** stays as [flat-store.md](flat-store.md) and [node-store.md](node-store.md) have it: reads at the call, staged writes when the batch is applied, an immediate handle's batch when it is committed.
- **A check for the moved functions.** `lignin-testkit/cost-model-storage/check.sh` replays the corpus on the model and fails on the first step that differs. After SEAM-05 the same replay on RocksDB and on the model says whether a rule changed in the move.

## 11. Operations

An operation's cost is the sum of the events it performs. The code above the storage layer decides the events; this section says what they are for the common operations, as measured on the model at the pinned commit, and then lists what the corpus does not check.

### 11.1 The events of common operations

`check.sh profile` runs a few operations on the model and prints, for each, the cost GroveDB reports and how often each storage rule was applied to produce it. On grove version 5, with a root tree, a tree `tree` under it and 100-byte items under 4-byte keys in that tree:

| Operation | Seeks | Loaded | Added | Replaced | Removed | Hashes | Storage rules applied |
| --- | --- | --- | --- | --- | --- | --- | --- |
| insert an empty tree into the empty root tree | 3 | 0 | 115 | 0 | 0 | 3 | B1×1 K2×1 K3×1 P1×1 R1×1 W1×1 W2×1 W3×1 |
| insert a second empty tree next to it | 5 | 74 | 117 | 78 | 0 | 5 | B1×1 K2×2 K3×1 P1×1 R1×2 W1×1 W2×2 W3×1 |
| insert a 100-byte item into the first tree, which is empty | 5 | 193 | 247 | 78 | 0 | 9 | B1×1 K2×2 K3×2 P1×3 R1×3 W1×1 W2×2 |
| insert an eighth item | 11 | 868 | 247 | 708 | 0 | 15 | B1×1 K2×5 K3×5 P1×3 R1×6 W1×1 W2×5 |
| replace an item by one 20 bytes longer | 6 | 448 | 20 | 288 | 0 | 9 | B1×1 K2×2 K3×1 P1×3 R1×4 W2×2 |
| get the item at the root of its tree | 2 | 348 | 0 | 0 | 0 | 1 | P1×2 R1×2 |
| get an item further down | 3 | 558 | 0 | 0 | 0 | 1 | P1×2 R1×3 |
| get a key that is not there | 3 | 348 | 0 | 0 | 0 | 1 | P1×2 R1×3 |
| delete an item | 12 | 1058 | 0 | 728 | 247 | 10 | B1×1 D1×1 K2×4 K3×3 P1×3 R1×7 W2×4 |
| read the seven items with a range query | 15 | 2746 | 0 | 0 | 0 | 1 | C1×8 C2×29 C3×7 P1×1 |
| commit the transaction | 0 | 0 | 0 | 0 | 0 | 0 | |
| prove the same range query | 10 | 1648 | 0 | 0 | 0 | 1 | P1×3 R1×10 |
| delete the tree with its seven committed items | 50 | 8668 | 0 | 79 | 1838 | 8 | B1×1 C1×28 C2×63 C3×14 D1×1 D2×7 K2×1 P1×7 P2×3 P3×3 R1×5 W2×1 W3×1 |
| delete a tree with seven items the same transaction wrote | 50 | 7160 | 0 | 79 | 383 | 8 | B1×1 C1×28 C2×63 C3×14 D1×1 D2×7 K2×1 P1×7 P2×3 P3×3 R1×5 W2×1 W3×1 |

How the rows read:

- **The first insert.** The root Merk is opened by the empty path (P1, free) and its root key is read and found absent (R1, one seek). The node is put (W2, one seek) and the root key recorded (W3, one seek). The 115 added bytes are the stored key with its varint (W1: 36 + 1) and the tree element's layered value cost (section 6.3: 3 + 35 + 40). The three hashes are the value hash, its combination with the empty subtree's root hash, and the key-value hash.
- **An insert one level down** reads the parent element in the root subtree, opens the subtree, and opens the root Merk to carry the new root hash up. It writes two nodes. The item's ordinary cost is checked by K3 against its record: the element is 103 bytes, so the value costs 168 + 2 + 40 = 210 and the key 37. The parent's tree element changes only in its hashes and is charged its 78 bytes as replaced.
- **An insert into a tree of seven** fetches the nodes on the way down and rewrites every node on the way up: five node puts, four of them updates charged as replaced bytes.
- **A get** reads the parent element in the root subtree, which is an element read charged the element's value cost (section 6.5: 78), and opens the subtree's Merk, which fetches its root node at its record length (R1: 270 for a 123-byte element with two children). A key at the root is then in memory. Any other key is read directly: one more R1, with its record's length when it is there (210) and nothing loaded when it is not.
- **A range query** opens the subtree's handle and walks a cursor: one C1 to the first record and one per step, a C3 for each value, and the C2 calls of the walk. Seven of the fifteen seeks are GroveDB's own extra seek per step (section 7.1).
- **Deleting a tree** walks its nodes with cursors and removes them with no writer's cost, so each is charged by D2 from the committed state: two seeks and its committed record length. The delete also derives three secondary prefixes and opens each by prefix (P3, P2).
- **Deleting a tree the same transaction filled** performs the same events and is charged less. The cursors read the transaction's records, but D2 finds none of the seven keys in the committed state: each is charged two seeks, nothing loaded, and 38 bytes removed (the 37 of the stored key with its varint, and one byte for the length of an empty record). 383 is those 266 and the 117 of the tree's own element.

Costs differ between grove versions where section 8 says so: on grove version 1 the eighth insert makes one more read, deleting an item costs 15 seeks and deleting the tree 44. `check.sh profile 1` prints the table for grove version 1.

### 11.2 Operations the corpus cannot check

- **`GroveDb::clear_subtree`** returns `Result<bool, Error>`: it computes a cost (`clear_subtree_with_costs`, `operations/delete/clear_subtree/v0.rs` and `v1.rs`) and drops it, so a `ClearSubtree` step records zero. From the code, the dropped cost is:
  - On every grove version, P1 and the fetches to open the Merk at the path.
  - **For a Merk:** when the options ask for it, a cursor walk over its elements to look for subtrees, with a `delete` of each subtree it may delete; then `Merk::clear` (section 6.4), the new root propagated to the ancestors as for any write, and B1, where every removed node is charged by D2. From grove version 4 a check for backward references may come first.
  - **For an append-only tree on grove versions 1 to 3** (`clear_subtree` version 0): a second handle on the path (P1), `clear` of its data keyspace (C4), and B1 with one D2 per record. Nothing else: the parent element is not read or written.
  - **For an append-only tree on grove versions 4 and 5** (version 1): the parent element is read first, then the same `clear`; then the parent's Merk is opened, the element is written back in its empty state and the change propagated to the root; then B1.

  Its effect on the state is checked: the root hash after it is recorded, and later steps read what it left.
- **The typed append methods** (`mmr_tree_append`, `bulk_append`, `dense_tree_insert`, `private_document_store_insert`) return a cost, which the corpus has always recorded and the model reproduces. What `workload::apply` used to drop is their returned value (the new root and position). The corpus now records it (`result appended`), so this blind spot is closed.

### 11.3 What the corpus does not contain yet

The corpus now has the third unit of BASE-01: conditional inserts and deletes, raw existence checks, direct writes to indexed trees, typed reads of the append-only trees, chunk proofs, the remaining query entry points, and transactions with rollbacks, savepoints and snapshot reads. The model reproduces all of it (section 12.3). Two rules of this document that those operations exercise:

- `has_raw` opens a handle by path and reads the key in the data keyspace: P1 and one R1, charged as it stands (the record length, not the element rule of section 6.5). A key that is not there still pays P1 and the seek.
- Rollbacks and savepoints are free (section 5.6). They undo writes of the transaction and leave the committed state, which D2 reads, as it was.

Still to come, in a fourth unit: batches applied with Drive's two closures, and the deletes that section removed bytes. Section 6.6 is their rule. They are made of the same storage events. What they add is flags that change the bytes written and removed bytes in sectioned form; the steps the corpus holds in full so far show plain removals only. The check is to be run again on those traces when they exist.

What no workload reaches: writes through immediate handles (chunk restore and state sync: M1, M2, R2), reads of the meta keyspace, a root key put with a cost, a refused write, and D1 outside the data keyspace. Section 12.4 lists every rule with how often the corpus applies it.

## 12. The check

"The spec predicts every `OperationCost` in the golden corpus from logical inputs alone" is shown by running it.

### 12.1 What runs

`lignin-testkit/src/cost_model` is the storage rules as code, with a logical state to apply them to:

- `rules.rs`: one pure function per rule of section 5.
- `node.rs`: a node as its fields, with `record_len` computed from sizes (section 4). The layout is written from this document; the module's tests compare it with Merk's own encoding for every feature and link.
- `state.rs`: for every subtree four keyspaces of ordered records, the writes of a transaction over the committed state, savepoints, cursors and batch charging. A node is kept as its fields; its record is never stored.

`lignin-testkit/cost-model-storage` gives that model the interface of `grovedb-storage`: the same trait definitions, multi-context batch and flat keyspace seam (the real source files, taken by path), with the RocksDB implementation replaced. `check.sh` lays out the workspace a second time with `storage` replaced by that crate and runs Cargo there. GroveDB cannot be given another store inside the real workspace before the `Engine` seam (SEAM-04) is cut, because it names the RocksDB types; replacing the crate needs no change to any engine source.

In that workspace GroveDB, Merk and the append-only trees are compiled from this checkout unchanged, and RocksDB is not among the crates a replay builds (`check.sh` stops if it is). GroveDB's own test build still compiles the `rocksdb` crate, which it names as a dependency of its tests for writing SST files behind the `unsafe-dump-load` feature; its storage is the model all the same.

`check.sh` replays whatever `lignin-testkit/corpus` holds, on every grove version it was recorded for, so it runs unchanged when the corpus grows. `cargo test -p lignin-testkit --test cost_model -- --ignored` runs the same replay as a test.

### 12.2 Logical inputs

What the prediction takes as input:

1. The operation stream of a workload and the grove version.
2. The logical state: per subtree and keyspace, ordered keys with their records. A Merk node is its value, feature, two hashes and, per child, key, hash, heights and aggregate. That includes the shape of every Merk.
3. The code above the storage layer, unchanged: it turns an operation into storage events and adds the charges of sections 6 and 7.

What it does not take: a storage engine, a file, or a stored node record. Every seek, loaded byte, prefix hash and storage cost of the storage layer is computed by the functions of `rules.rs` from lengths.

The rules of sections 6 and 7 are therefore executed by the code that owns them, not by a second implementation. That is deliberate: the plan keeps that code as the source of truth on both engines, and a second implementation of GroveDB's operations would be a second GroveDB. What is independent of today's engine is everything an engine replaces.

### 12.3 Results

```text
$ lignin-testkit/cost-model-storage/check.sh
golden corpus: 380 traces replayed, 380 as recorded (229526 steps), 0 not
```

The replay is BASE-02's: it compares the cost, the result, the root hash and the proof of every step with the committed corpus, and names the first step and part that differ. A green replay on the model therefore also says that the model gave the same answers as RocksDB wherever the corpus looks: every result read, every root hash and every proof. It does not say that the two states are equal record by record; an aux or meta record that nothing reads would not be noticed.

At the reference scale a trace has a digest every 1,000 steps and not a line per step, so a difference is named by its interval of 1,000 steps. The step is found by dumping that interval on both sides and comparing: `check.sh dump --reference --grove-version N --steps FROM..TO --out DIR` on the model, and `lignin-golden dump` with the same arguments on RocksDB.

Everything that was run on the model, at the pinned commit:

| What | Command | Result |
| --- | --- | --- |
| The corpus at the standard scale: 76 traces on each of grove versions 1 to 5, recorded from `develop` `2cfc1ce1`. Run again when `develop` was merged up to that commit | `check.sh` | 380 traces as recorded, 229,526 steps |
| The corpus at the reference scale: 1,050,008 operations on each grove version | `check.sh replay --reference` | 5 traces as recorded, 5,250,055 steps |
| The storage crate's tests | `check.sh test -p grovedb-storage --features rocksdb_storage -- --skip cost_model::` | 51 pass. The crate's other 14 are unit tests inside `rocksdb_storage/storage.rs`, the file the model replaces. |
| Merk's tests | `check.sh test -p grovedb-merk` | 872 and 9 doc tests pass |
| GroveDB's tests, with its default features | `check.sh test -p grovedb --no-fail-fast` | 3,782 of the library's tests pass, 8 are ignored as they are on RocksDB, and 1 fails: `test_checkpoint_wal_under_threshold_is_preserved` looks for RocksDB's WAL files in a checkpoint directory, and the model writes none. The 80 tests of the eight integration binaries and the 3 doc tests pass. |
| Each rule against RocksDB, in the workspace's own tests | `cargo test -p lignin-testkit --lib cost_model` | 37 pass |

The three test suites are the engine's own, unchanged. They assert costs in many places, so a rule the corpus does not reach is still compared with numbers that were written against RocksDB. The `--skip` leaves out the model's own unit tests, which the stand-in crate also compiles.

The last row runs inside the real workspace: `cost_model/against_rocksdb.rs` performs the same batches, reads and cursor walks on a RocksDB storage and on the model and compares every cost and every result, and `cost_model/against_merk.rs` compares the record layout of section 4 with Merk's encoder for every feature and link.

**Sensitivity.** A check that cannot fail shows nothing, so each rule was changed in the model, one change at a time, and the corpus replayed. The table and the three paragraphs after it are of the corpus as it was then, 290 traces. They were not made again when the corpus grew to 310, to 375 and to 380: each of the thirty changes is a hand edit of the model, a release build of its workspace from `grovedb-costs` up, and a replay:

| Rule | Change made to the model | Traces that then differ, of 290 | First difference the replay names |
| --- | --- | ---: | --- |
| P1 | block length 64 -> 63 | 25 | `v1/element-zoo.trace` step 459: cost |
| P1 | no length byte per segment | 30 | `v1/element-zoo.trace` step 280: cost |
| P1 | segment count word 8 -> 4 bytes | 25 | `v1/element-zoo.trace` step 280: cost |
| P2 | a hash for a handle opened by prefix | 153 | `v1/element-zoo.trace` step 53: cost |
| P3 | secondary prefix 1 -> 2 hashes | 143 | `v1/element-zoo.trace` step 53: cost |
| R1 | loaded bytes of a found record + 1 | 290 | `v1/documents.trace` step 1: cost |
| R1 | no seek for an absent key | 290 | `v1/documents.trace` step 0: cost |
| W1 | stored key without its varint | 285 | `v1/documents.trace` step 0: cost and error and root hash |
| W2 | no seek for a data put | 285 | `v1/documents.trace` step 0: cost |
| W3 | no seek for a free root key put | 285 | `v1/documents.trace` step 0: cost |
| W4 | a seek for a prepaid put | 14 | `v4/element-zoo.trace` step 40: cost |
| K1 | key with no writer's cost without its varint | 56 | `v1/element-zoo.trace` step 35: cost |
| K2 | link option bytes 2 -> 1 | 259 | `v1/documents.trace` step 2: cost and error and root hash |
| K2 | flat value without its varint | 60 | `v1/element-zoo.trace` step 35: cost |
| K2 | link hook 4 -> 5 | 259 | `v1/documents.trace` step 2: cost and error and root hash |
| K2 | one-varint feature class 8 -> 9 | 155 | `v1/tokens.trace` step 13: cost and error and root hash |
| D1 | 2 seeks for a delete with a cost | 180 | `v1/documents.trace` step 908: cost |
| D1 | the writer's removed bytes not charged | 174 | `v1/documents.trace` step 908: cost |
| D2 | charged from the transaction's view | 0 | none (see below) |
| D2 | 1 seek for a delete with no cost | 116 | `v1/element-zoo.trace` step 1236: cost |
| D2 | removed bytes without the value's varint | 116 | `v1/element-zoo.trace` step 1236: cost |
| D2 | no loaded bytes | 116 | `v1/element-zoo.trace` step 1236: cost |
| D2 | the committed record taken as absent | 116 | `v1/element-zoo.trace` step 1236: cost |
| C1 | no seek for a step back | 38 | `v1/tokens.trace` step 36: cost |
| C1 | no seek for a step forward | 249 | `v1/documents.trace` step 31: cost |
| C2 | boundary 288 -> 287 | 234 | `v1/documents.trace` step 31: cost |
| C2 | key without the prefix | 249 | `v1/documents.trace` step 31: cost |
| C3 | value length + 1 | 249 | `v1/documents.trace` step 31: cost |
| C3 | value without the key's bytes | 249 | `v1/documents.trace` step 31: cost |
| N1 | feature payload left out | 235 | `v1/balances.trace` step 4: cost |
| N1 | link + 1 byte | 270 | `v1/documents.trace` step 3: cost and error and root hash |
| N1 | child aggregate payload left out | 220 | `v1/balances.trace` step 5: cost |

Every change is found but one, and for each the replay names the trace, the step and what differs. Where a changed cost makes the engine refuse a write, the result and the root hash differ too.

The one that is not found is the word *committed* in D2. The corpus does apply D2 to records that the same open transaction wrote: 2,000 times to a record it created and 160 times to a node it rewrote. All of them are in `delete-heavy` and in `suite`, which contains it: they insert 50 items into each of four trees and clear the tree before they commit. Every one is therefore inside a `ClearSubtree` step, whose cost GroveDB does not return (section 11.2). The workloads added since, with their rollbacks, savepoints and writes inside an open block, add no such delete. An operation that returns its cost shows the difference (the last two rows of section 11.1), and the corpus has none that deletes, with no writer's cost, a record its own transaction wrote. So the corpus does not tell the committed state from the transaction's view in D2.

Merk's and GroveDB's test suites do not tell them apart either: with this change both pass on the model exactly as before. What pins the rule is the storage crate's `flat_store::a_deferred_handle_stages_writes_and_costs_what_the_context_costs` and `flat_store::clear_costs_the_walk_and_one_uncosted_delete_per_record`, which fail on the model with the change, and the comparison with RocksDB in `against_rocksdb.rs`. Section 13 proposes a workload that would put it in the corpus.

### 12.4 Which rules the corpus applies

The model counts every application of a rule, by case. `check.sh` prints the counts of a replay and keeps those of any run in `target/cost-model/rule-hits.tsv`. The first column is the corpus at the standard scale, the 380 traces recorded from `develop` `2cfc1ce1`; the others are the three test suites of section 12.3 as they were run at the pinned commit, added up over their test processes.

| Rule | Case | Corpus | Storage tests | Merk tests | GroveDB tests |
| --- | --- | ---: | ---: | ---: | ---: |
| P1 | a subtree handle opened by the empty path | 627,609 | 0 | 3,541 | 227,613 |
| P1 | a subtree handle opened by a path of one segment or more | 1,812,575 | 132 | 3 | 801,434 |
| P2 | a subtree handle opened by a prefix | 39,223 | 13 | 0 | 33,547 |
| P3 | the prefix of an indexed tree's secondary derived | 36,586 | 0 | 0 | 18,362 |
| R1 | a data record read through a deferred handle | 4,866,026 | 24 | 217,766 | 1,607,100 |
| R1 | a data key with no record read through a deferred handle | 35,258 | 13 | 1 | 9,623 |
| R1 | an aux record read through a deferred handle | 290 | 12 | 1 | 19 |
| R1 | an aux key with no record read through a deferred handle | 40 | 10 | 0 | 178 |
| R1 | a root key record read through a deferred handle | 407,502 | 11 | 3,008 | 117,923 |
| R1 | an absent root key record read through a deferred handle | 3,726 | 4 | 421 | 12,733 |
| R1 | a meta record read through a deferred handle | 0 | 10 | 0 | 8 |
| R1 | a meta key with no record read through a deferred handle | 0 | 4 | 2 | 22 |
| R2 | a read through an immediate handle | 0 | 461 | 29,226 | 16,366 |
| W1 | the stored key added to the key cost of a new data record | 245,085 | 1 | 108,684 | 250,138 |
| W2 | a data put with the sizes of a node's links and feature | 2,954,239 | 0 | 162,830 | 915,178 |
| W2 | a data put with no link sizes | 14,873 | 78 | 23,135 | 39,798 |
| W2 | an aux put | 1,155 | 34 | 2 | 21 |
| W2 | a meta put | 522 | 33 | 2 | 35 |
| W3 | a root key put with a cost | 0 | 2 | 0 | 0 |
| W3 | a root key put with no cost | 139,668 | 23 | 3,100 | 28,687 |
| W4 | a prepaid put | 1,213 | 14 | 0 | 745 |
| K2 | a value cost worked out from the record length | 932,369 | 132 | 181,551 | 505,444 |
| K2 | a value cost taken from the writer as given | 2,038,420 | 15 | 4,418 | 449,588 |
| K3 | a writer's value cost checked against the record length | 922,793 | 2 | 158,412 | 488,502 |
| K3 | a writer's key cost checked against the stored key | 245,083 | 1 | 108,382 | 250,061 |
| K3 | a write refused because the writer's cost does not match | 6 | 0 | 0 | 3 |
| D1 | a data delete with the writer's cost | 72,504 | 0 | 1,852 | 10,543 |
| D1 | an aux delete with the writer's cost | 0 | 0 | 0 | 0 |
| D1 | a root key delete with the writer's cost | 0 | 0 | 1 | 65 |
| D1 | a meta delete with the writer's cost | 0 | 0 | 0 | 0 |
| D2 | a delete with no cost of a record in the committed state | 26,205 | 24 | 0 | 1,011 |
| D2 | a delete with no cost of a key the committed state lacks | 3,039 | 11 | 0 | 23 |
| M1 | a write through an immediate handle | 0 | 481 | 5,192 | 2,407 |
| M2 | a put in the batch of an immediate handle | 0 | 7 | 24,501 | 12,024 |
| M2 | a delete in the batch of an immediate handle | 0 | 1 | 3 | 0 |
| C1 | a cursor moved to the first record | 74,667 | 86 | 1,630 | 26,191 |
| C1 | a cursor moved to the last record | 850 | 10 | 12 | 434 |
| C1 | a cursor moved to the first key at or after a key | 2,957 | 3 | 307 | 5,050 |
| C1 | a cursor moved to the last key at or before a key | 98 | 2 | 0 | 233 |
| C1 | a cursor stepped forward | 201,705 | 106 | 43,047 | 49,207 |
| C1 | a cursor stepped back | 8,196 | 28 | 3,348 | 4,839 |
| C2 | a cursor asked for its key or validity on a record | 746,329 | 388 | 148,950 | 214,692 |
| C2 | a cursor asked for its key or validity off its subtree | 64,279 | 92 | 208 | 18,794 |
| C3 | a cursor asked for the value of a record | 179,773 | 110 | 46,699 | 56,654 |
| B1 | a batch applied to a transaction | 190,895 | 43 | 3,413 | 88,148 |
| B1 | a batch whose writes were refused | 0 | 1 | 0 | 2 |
| N1 | a data record kept as the fields of a node | 2,958,501 | 0 | 191,097 | 942,062 |
| N1 | a data record that is not a node, kept as bytes | 10,611 | 207 | 2 | 14,839 |
| N2 | the link and feature sizes Merk gave with a node are the node's | 2,954,239 | 0 | 162,830 | 915,178 |
| N2 | the link and feature sizes Merk gave with a node are not the node's | 0 | 0 | 0 | 0 |

### 12.5 What the check does not show

- **Rules the corpus does not reach** (zero in the first column above) are specified from the code: reads of the meta keyspace, R2, a root key put with a cost (W3), a batch whose writes are refused (B1), D1 outside the data keyspace, M1 and M2. A write refused because the writer's cost does not match (K3) is in the corpus since the hand-written case `legacy-flags-of-another-length`, twice on each of grove versions 1 to 3. The engine's suites reach all of them but D1 in the aux and meta keyspaces. `cost_model/against_rocksdb.rs` compares every one of them with RocksDB.
- **N2** has to stay at zero in its second row, and does: on 4,032,247 node writes of the four runs, and on 235,356,610 of the reference scale, the sizes Merk passed were the node's own. This row is also what keeps the model from charging a Merk's node from stored bytes: a record that comes with sizes and that the model cannot keep as a node's fields counts here. With the model's node parser made to give up on records over 100 bytes, every step of the corpus still matches, because the bytes it falls back to have the engine's length, and the check fails on this row with 2,260,997 such puts.
- **D2 on a record the same transaction wrote** is applied by the corpus only inside operations whose cost is dropped (section 12.3, Sensitivity).
- **Commit conflicts** are not modelled: the model commits every transaction. No workload has two open write transactions.
- **Persistence** is in memory: a database lives as long as the process and is found again through a mark left in its directory, and a checkpoint is a database of its own that starts as the committed state of the moment it was taken.
- **The list of section 7** comes from a pattern search, not from the compiler. The pattern matches an assignment to a field of `OperationCost` or `StorageCost` and the `with_*` constructors, in library code outside test modules and the estimate modules. A charge made in another form would not be found by the search. It would still be executed by the check.

## 13. Open points

1. **The child's key in a cross-page link** (section 9). The plan's cell for FMT-02 lists the child's hash, height and aggregate. The key length is needed for `record_len`, and Merk's `Link` needs the key itself. Settled (owner ruling D-041, plan draft 21): a child link stores the length of the child's key and not the key. FMT-02 builds it.
2. **Reading the committed version inside a write transaction** (section 9, D2). TXN-03 and MERK-05 have to offer it for `clear` and `Merk::clear`. Settled (owner ruling D-041): a write transaction offers that read, the committed read of rule RD10 in [transactions.md](transactions.md). It is the read whose outcome `BatchCharge::delete_uncosted` takes (section 14.1).
3. **`GroveDb::clear_subtree` returns no cost.** Whether it should is outside this task; while it does not, nothing can compare its cost across engines.
4. **The committed view of D2 is not in the corpus** (section 12.3), nor in Merk's or GroveDB's tests. A workload that fills a tree and deletes it whole in the same transaction, on every grove version, would record it: `GroveDb::delete` returns its cost, and section 11.1 shows that the cost differs. It fits the unit of BASE-01 that is still to be merged, or a follow-up to it, since BASE-02 records again then.
5. **The order of a batch** (B1) shows in the removed bytes of a batch that is applied, on grove versions 1 to 3, and in the cost that comes back with a refused put. No test of the engine asserted either; the comparisons with RocksDB are in `cost_model/against_rocksdb.rs`. SEAM-05 pinned both in the storage crate's own tests (section 14.5). Closed.

## 14. Where the storage rules are now (SEAM-05)

LigninDB task SEAM-05 ([task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)) moved the rules of section 5 out of the RocksDB storage contexts. They are functions of logical sizes in `grovedb_costs::cost_model` (`costs/src/cost_model/`). The RocksDB engine calls them for every cost it reports and works out none itself. No rule changed: sections 2 to 13 describe the same charges as before.

### 14.1 The functions

`StorageCostModel` is the version of the rules, and every rule is a method of it. It has one version, `V0`: what grove versions 1 to 5 charge (`cost_model/v0.rs`). A caller holds a `StorageCostModel` and never names a version at a call site, so a second version is a new variant, and the compiler lists every rule that has to say what it charges.

| Rule | Function of `StorageCostModel` | Where the RocksDB engine calls it |
| --- | --- | --- |
| P1 | `open_by_path(segment_lens)` | `RocksDbStorage::build_prefix`. The estimate `grovedb_storage::worst_case_costs::get_storage_context_cost` calls it with the longest length each segment may have, and is no part of the engine (section 14.7). |
| P2 | `open_by_prefix()` | the two openers that take a prefix |
| P3 | `secondary_prefix()` | `secondary_prefix_for` |
| R1 | `read(found)` | the four reads of a deferred context |
| R2 | `read_immediate()` | the four reads of an immediate context |
| `W-tx`, `B-part` | `staged_write()` | every write staged through a deferred context, and its `commit_batch` |
| a write refused for want of a batch | `refused_write()` | the same nine methods |
| W1 | `complete_new_key_cost(cost_info, stored_key_len)` | the data `put` of a deferred context and of both batch types |
| W2, W3, W4, with K | `batch()`, then `BatchCharge::put(keyspace, stored_key_len, value_len, children_sizes, cost_info)` | `continue_write_batch`, for each put of the batch |
| D1 | `BatchCharge::delete(cost_info)` | the same, for each delete with a writer's cost |
| D2 | `BatchCharge::delete_uncosted(stored_key_len, committed_value_len)` | the same, for each delete with none. The engine reads the committed record and hands over the outcome of the read. |
| B1 | `batch_order()`; `BatchCharge::finish(written)`; `BatchCharge::into_parts()` with `batch_written(on_success, written)` | `commit_multi_context_batch`; `build_write_batch`, `continue_write_batch` and `commit_db_write_batch`, which GroveDB's partial batches call apart |
| M1 | `write_immediate()` | the eight writes of an immediate context |
| M2 | `immediate_batch()`, then `ImmediateBatchCharge::put`, `delete` and `finish` | `PrefixedRocksDbBatch`, and `commit_batch` of an immediate context |
| C1 | `cursor_move()` | the six moves of `PrefixedRocksDbRawIterator` |
| C2 | `cursor_key(on_record)` | `key` and `valid` |
| C3 | `cursor_value(on_record)` | `value` |
| C4 | `clear_walk(stored_key_lens)` | not called. `FlatStore::clear` makes the walk, and each step is charged by C1, C2 and D2. The function is the same sum for an engine that clears a keyspace in one step. |
| N1 | `node_record_len(value_len, feature_len, left_link_len, right_link_len)`, and `grovedb_merk::cost_model::node_record_len(model, node)` for a node Merk holds | not called. On RocksDB the record length is the length of the stored bytes, which is the same number. |
| section 5.6 | `transaction_commit()` | `commit_transaction` |

How the functions are to be read:

- **Lengths.** A function takes the length of the **stored key**, the 32-byte prefix and the caller's key, because that is what the rules are defined on. RocksDB has that key. An engine that keeps only the caller's key calls `stored_key_len(key_len)`. A record length is a number of bytes, for a node the result of N1.
- **Rule K** stays where it was, `OperationCost::add_key_value_storage_costs`. `BatchCharge::put` and `ImmediateBatchCharge::put` are the only callers left outside tests.
- **A batch** is charged through a `BatchCharge`, which keeps the two parts of B1 apart: what is owed at once (seeks, loaded bytes) and the storage cost, owed once the writes are taken. The engine adds the operations in the order of `batch_order()` and by stored key, and ends with `finish(written)`. A put that is refused returns the error; `finish(false)` is then what is owed.
- **The committed read of D2** is the one thing a rule needs that is not a size the engine has at hand. The engine makes the read outside the transaction and passes its outcome; the function charges the two seeks whether it succeeded or not. On LigninDB it is the committed read of rule RD10 in [transactions.md](transactions.md).
- **What charges nothing** is a function too (`open_by_prefix`, `read_immediate`, `write_immediate`, `staged_write`, `refused_write`, `transaction_commit`), so that a later version can charge it and no engine decides by itself that an event is free.
- **N1 knows no kind of feature or aggregate.** The function takes the encoded length of the node's feature (its tag byte and what follows) and of each link (the child's key and 36 bytes, and its aggregate after the tag byte), and adds the fixed parts of the layout of section 4. `grovedb_merk::cost_model::node_record_len` measures the feature and the links of a node by Merk's own encoders, so a kind that Merk gains is measured as Merk encodes it and there is no list of kinds to keep beside the encoders. The two sizes Merk passes for a link with a node write (N2) add up to the link's length, and the feature's size there is its length without the tag byte.
- **No rule looks at a hash.** Every function takes lengths, a keyspace and the cost the writer gives with the call. Since GroveDB #1059 a record can change while no hash does: the referrer list of an element that takes backward references is in no hash, so the element is written with the hash it had, and its ancestors are written again with the bytes they had. Each of those writes is a put like any other, charged by its lengths and its writer's cost. An engine that sees an unchanged hash, or unchanged bytes, still owes the charge of the put.

### 14.2 The ledger

`grovedb-costs` has a feature for tests, `cost-model-ledger`. With it, every cost a function of the model hands out to be reported is added to a total kept per thread (`cost_model::ledger::issued`, `take`). A batch is in it once, when `finish` or `into_parts` and `batch_written` hand its parts out. A test of an engine adds up the costs the engine's calls returned and compares: the sums are equal when the engine reported what the model gave it, all of it and nothing else. That holds whatever the engine's code looks like, which no reading of the source can promise. Without the feature the functions are unchanged and nothing is recorded.

### 14.3 Versions

`grovedb_merk::cost_model::storage_cost_model(grove_version)` gives the version of the storage rules a grove version charges by. It is `V0` for all five. `grovedb_merk::cost_model` re-exports the functions, so Merk and the stores under it name one module.

The storage contexts of `grovedb-storage` are opened with no grove version: nothing in the `Storage` trait carries one, and `build_prefix` and `secondary_prefix_for` are associated functions. So the RocksDB engine charges by one fixed version (`rocksdb_storage::storage::COST_MODEL`, `V0`). That is correct while every grove version maps to `V0`. A grove version that is given other storage rules (GROVE-13) has to reach those contexts as well, if RocksDB is still an engine then.

### 14.4 What did not move

Sections 6 and 7, as section 10 says: Merk's account of what a key and a value cost, which nodes are fetched, the hash counts, the element read that replaces loaded bytes, the pinned-view read, the client's callbacks, and the charges GroveDB and the append-only trees make themselves. `FlatStore::clear` still performs its walk. `StorageBatch` still decides the order of a batch by the order it is iterated in; `batch_order()` states that order and a test holds the two together.

### 14.5 How the move was checked

| What | Where | Holds |
| --- | --- | --- |
| The corpus on RocksDB | `cargo test -p lignin-testkit --test golden` | Every trace replays as recorded with the engine charging through the functions |
| The corpus on the model | `lignin-testkit/cost-model-storage/check.sh` | The second implementation of the rules (`lignin-testkit/src/cost_model/rules.rs`), which SEAM-05 left as it was, still predicts every step |
| The engine against the second implementation | `cargo test -p lignin-testkit --lib cost_model` | `against_rocksdb.rs` runs the same batches, reads and cursor walks on RocksDB and on `rules.rs` |
| Each number | `costs/src/cost_model/tests.rs` | One test per rule, and that a charge meets a running cost exactly as the parts were added before the move, for every combination of removal forms and both removal arithmetics |
| The order of a batch | `storage/src/rocksdb_storage/tests.rs`, module `charging` | By keyspace (all six pairs), by key within a keyspace, by prefix across subtrees, and the key of a put before its value, each through the removed bytes of an applied batch |
| A refused put | the same module | For a refusal in each keyspace: the cost returned holds the seeks and loaded bytes of every operation before it in batch order and of the refused put, and no storage cost; nothing was written |
| A batch that is not written | the same module | The storage cost is not owed, what was charged at once is. The batch of an immediate handle is owed in full either way. |
| A write with no batch | the same module | Each of the nine is refused and charged `refused_write()` |
| The cursor | the same module | Every move, and `key`, `valid` and `value` on a record, on the next subtree's key, past the end and before the first seek, cost what C1 to C3 give |
| C4 and P1 | the same module | `FlatStore::clear` costs `clear_walk`; a handle, a prefix and the estimate cost `open_by_path` |
| N1 | `merk/src/cost_model.rs` | The length is the length of the record Merk encodes, for every kind of feature and of link aggregate, and the sizes Merk passes with a node write add up to it. The kinds are not taken from a list alone: a kind Merk gains does not compile until the test gives it values, and every tag Merk's decoders accept, for a feature and for a link's aggregate, has to be one the test measured. |
| That the engine reports what the model hands out | `storage/tests/model_ledger.rs` | Every operation of the RocksDB engine that reports a cost is called, on deferred and immediate handles, with batches applied whole and in parts, refused, and refused by a read-only transaction. After each group the sum of what the calls returned is the sum the model's ledger holds. A cost built beside the model, by whatever means, or a cost of the model that is dropped, leaves the two apart. |
| That the source states no charge | `storage/tests/charging.rs` | The storage crate is parsed and its code outside tests walked. No function body, constant or type alias, free or associated, names a type a cost is built from or a function that builds one, calls `Default::default`, touches a field of a cost, or calls a function or macro that adds costs up, as a method or by path; the engine adds nothing at all, and elsewhere no function that carries a cost does. The same names are looked for in the tokens of macros. Imports from `grovedb_costs` are held to a list and may not be renamed, so a cost type cannot be reached under another name. `FlatStore::clear` and `stage_pending_prefix_drop` are left out by name. |

The tests of the `charging` module were also run against the storage contexts as they were before the move, with only the functions added to `grovedb-costs`: they pass there unchanged, and the last two rows fail there.

The last three rows guard one property from three sides, and none of them is enough alone:

- The ledger shows that every cost reported came out of the model and every cost that came out was reported. It cannot tell an engine that adds two of the model's costs together where one was due: both were handed out and both are reported.
- The `charging` module and the corpus show that each operation is charged what the model gives for that event, so a charge taken twice or from the wrong function shows there.
- The check of the source names the line. It holds the code to the names and operators it lists, and such a list cannot be complete: a cost type reached through an associated type and a macro's metavariable both passed an earlier form of it. With such a charge planted in the engine, the unit tests pass, because the number is the same, and the ledger test fails. The check does not follow a call into another crate either, so a function added to `grovedb-costs` outside `cost_model` that builds a cost for the engine has to be seen in review.

### 14.6 For later tasks

- **SEAM-08.** Done: section 14.7.
- **MERK-03.** Done: section 15. `grovedb_merk::cost_model` is the module to extend. Every storage charge of the LigninDB path is a call of a function above: `read` with `node_record_len` for a fetch, `BatchCharge` for a commit, the cursor functions with `node_record_len` for a record under a cursor. A store that holds a `TreeNode` has its record length from `grovedb_merk::cost_model::node_record_len`. One that holds the fields only gives N1 the encoded lengths of the feature and of each link, measured by Merk's encoders and not by a table of kinds. The ledger is the test that such a store reports only what the model hands out: the same comparison as `storage/tests/model_ledger.rs`, on the store's operations.
- **GROVE-04, MERK-05, TXN-08.** A `FlatStore` on LigninDB is an adapter that takes each cost from these functions. A batch has to be charged in the order of `batch_order()` and by stored key, which is prefix first. A range delete reports `clear_walk` and stages its deletes for D2.
- **GROVE-13.** New storage rules are a new variant of `StorageCostModel`, and `storage_cost_model` maps the new grove version to it (section 14.3).

### 14.7 Estimates (SEAM-08)

LigninDB task SEAM-08 took the one storage number of the estimated-cost helpers out of the engine.

`get_storage_context_cost` was an associated function of the `Storage` trait, so an estimate of what a subtree handle costs to open had to name a storage engine to ask. It is now a free function, `grovedb_storage::worst_case_costs::get_storage_context_cost(model, path)`, next to `WorstKeyLength`, the trait that gives the longest length a path segment may have. It is one call of `open_by_path` (P1) with those lengths. The module is built without the `rocksdb_storage` feature, and its tests run with no RocksDB crate among the crates that are built. The `Storage` trait has no estimate left, so a second engine implements none.

The function takes the `StorageCostModel` and names no version. GroveDB's six callers (`add_worst_case_get_merk_at_path`, `add_worst_case_has_raw_cost`, `add_worst_case_get_cost` and their three average-case forms) pass `storage_cost_model(grove_version)`, so an estimate follows the rules of the grove version it is made for. No number changed: for grove versions 1 to 5 that is `V0`, the rules the RocksDB engine reported the estimate by.

What is left as it was:

- **The `S: Storage` parameter** of the fifteen public estimate helpers of `GroveDb` (the eleven `add_worst_case_*` and `add_average_case_*` functions and the four delete estimators). No helper takes anything from it any more: it is passed on or left unused. It stays because Platform calls two of the delete estimators with `::<RocksDbStorage>` (P-32 of [platform-requirements.md](platform-requirements.md)), so five call sites inside GroveDB's estimate code still name `RocksDbStorage` to fill it. Removing the parameter changes the public interface and is made in step with Platform (SEAM-07, OPS-03). Until then GroveDB's estimate code is built with an engine's type in scope, though no number comes from it.
- **Merk's and GroveDB's own estimates** (`merk::estimated_costs`, `grovedb::estimated_costs`, `grovedb::batch::estimated_costs`): the seeks, loaded bytes, storage bytes and hash counts they add from estimated sizes are estimates of sections 6 and 7 and name no engine.
- **Two estimates that restate a storage rule in their own arithmetic.** `average_case_indexed_secondary_mirror` adds one hash for the prefix of a secondary (P3), and `add_flat_drop_record_put_estimate` (`grovedb/src/operations/delete/flat_drop.rs`) works out the blocks of P1, three hashes of P3 and the bytes of the record's put. Neither needs an engine. A version of the storage rules that changes P1 or P3 (GROVE-13) has to change them, or route them through the model then.

An estimate hands out a cost of the model, so it is in the ledger of section 14.2 like any other, though no engine reports it. A test that holds an engine to the ledger therefore makes no estimate between taking the ledger and comparing. `storage/tests/model_ledger.rs` made one while the estimate was the engine's and makes none now.

The inventory of [storage-surface.md](storage-surface.md) lists `Storage::get_storage_context_cost` and its six call sites under `EST`. It describes the commit it is pinned to, as it does for everything the seams have moved since, and was not generated again.

How it was checked:

| What | Where | Holds |
| --- | --- | --- |
| The estimate with no engine | `cargo test -p grovedb-storage` (no feature: RocksDB is not built) | `worst_case_costs::tests`: the root costs nothing, a path is charged the hash blocks of its longest form at each block boundary, and the estimate is `open_by_path` of the longest lengths |
| The estimate against a real handle | `storage/src/rocksdb_storage/tests.rs`, module `charging` | A handle opened by path, deferred or immediate, costs what the estimate gives for the same lengths |
| That the crate still works out no charge | `storage/tests/charging.rs` | The source check reads `worst_case_costs.rs` and finds no charge stated: the function passes on what `open_by_path` gives |
| GroveDB's estimates on RocksDB | `cargo test -p grovedb` | Every estimate test passes as before |
| GroveDB's estimates with no engine | GroveDB's library tests in the workspace `lignin-testkit/cost-model-storage/check.sh` lays out, where the storage crate is the model | Every estimate test passes with no RocksDB crate among the crates that are built (`cargo tree -e normal,build,dev`). The stand-in for the storage crate has no estimate of its own any more, and a handle is charged by the second implementation of the rules, so the tests that compare an estimate with an operation compare the two implementations of P1. Two things in GroveDB's tests name the `rocksdb` crate. The dev-dependency one test uses under `unsafe-dump-load` was left out for the run. The test of the `Engine` seam that looks at the error RocksDB itself returns for a lost commit is not built in that workspace: `check.sh` sets the cfg `lignin_cost_model`, and the test is compiled only without it. For the run the stand-in was given the error types SEAM-04 added to the storage crate; it has had them itself since the sync with `develop` `2a79d035` |
| GroveDB's estimates with no engine, in this workspace (SEAM-07) | `cargo test -p grovedb --no-default-features --features estimated_costs --lib --tests`, which CI runs | The estimate tests that open no database pass with the real storage crate and its `rocksdb_storage` feature off, so with no RocksDB crate among the crates that are built (`cargo tree -e normal,build,dev`) and nothing left out: GroveDB's dev-dependency on the `rocksdb` crate is gone, and its `estimated_costs` feature turns no engine on. The tests that compare an estimate with an operation need a database and run with `rocksdb_storage`, or on the model as in the row above. [engine.md](engine.md#features-and-engines) has the features |
| That no number moved | a sweep run before and after the change | The fifteen helpers and the batch estimator, for 119 paths of every block boundary, seven tree types and the five grove versions: 86,460 results, the same bytes before and after. With one byte added to each segment in the new function, 7,075 of them differ |

## 15. The LigninDB store (MERK-03)

LigninDB task MERK-03 ([task register 2](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583128)). `merk::store::lignin` is the first store that keeps no node record: a node is a cell of a tree page. Every charge it makes is a call of a function of section 14 with sizes it holds. This section says which call is made for which event, how a size is come by where there is no record, and how the store is held to it.

### 15.1 Where each rule is charged

| Rule | Event on the store | What the store hands the model |
| --- | --- | --- |
| R1 | `get_node`, `get_node_for_read`, `has_node` and `fetch_node` through a deferred or a read-only handle | `read(found)`, once for the operation, with N1 of the cell it found (section 15.2). A read the transaction did not perform is charged as one that found nothing. |
| R1 | `fetch_node_without_value` through a deferred or a read-only handle (MERK-06) | `read(found)` with N1 of the cell, as `fetch_node`: the value's length is in the cell, and the value is not read. A link that leads to no node is charged as a read that found nothing. |
| nothing | `load_node_value`, on every handle (MERK-06) | no call: the fetch of the node was charged its whole record. A value that is refused, because the node's value hash or key-value hash is no longer the one that was fetched, is charged nothing either. |
| R1 | `root_key`, `get_aux_record`, `get_meta_record` | `read(found)` with the record's length |
| R2 | every read through an immediate handle | `read_immediate()` |
| `W-tx`, `B-part` | a commit, `write_node`, `set_root_key` and the meta writes on a deferred handle that has a batch | `staged_write()`. The writes are charged when `LigninBatch::apply` charges the batch. |
| a refused write | the same on a deferred handle with no batch and on a read-only handle | `refused_write()` |
| W1 | `put_node` with a writer's cost, on both kinds of handle, as the node joins the change set | `complete_new_key_cost(cost, stored_key_len)` |
| W2, W3, W4, with K | each put of a batch that is applied | `BatchCharge::put` with the keyspace, the stored key's length, the record length, and the sizes and the cost Merk gave. For a node the record length is N1 of the node Merk wrote, taken in `prepare_node`. |
| D1 | each removal with a writer's cost | `BatchCharge::delete(cost)` |
| D2 | each removal with none | `BatchCharge::delete_uncosted(stored_key_len, committed)`. `committed` is the committed read of the transaction (RD10): for a node, N1 of the cell the committed state holds under the key, and for any other record its length. |
| B1 | `LigninBatch::apply` | The keyspaces in the order of `batch_order()`, and within one by subtree and then by key, which is the order of stored keys. `finish(written)` once: `written` is whether the transaction took the batch. |
| M1 | `write_node`, `set_root_key` and the meta writes on an immediate handle | `write_immediate()` |
| M2 | a commit on an immediate handle | An `ImmediateBatchCharge` that each change is added to as it joins the change set, and `finish()` when the change set is committed, whether or not the transaction takes the writes. |
| C1 to C3 | the cursor | Not built (MERK-05). A cursor on a node will be charged `cursor_value` with N1 of the cell, as a read is. |

The store adds nothing to a cost the model returns and drops none. It states no number of its own: a length is measured, and the model turns it into a charge.

### 15.2 Sizes without a record

- **A node that is read.** N1 is `grovedb_merk::cost_model::node_record_len_from_sizes`: the length of the value, the feature, and for each child the length of its key and the aggregate of the subtree under it. A cell holds all of them: the length of a value that is kept out of line, the feature and each aggregate as Merk's tag and numbers, and in each link the length of the child's key ([tree-page.md](tree-page.md) TP10, TP12). No record of a node is serialized for it and no child's cell is read. The feature and the aggregates are measured by Merk's own encoders, which write the few numbers of one to count them, so a kind Merk gains is measured as Merk encodes it. A link encodes its child's key after one length byte, so the function measures a link with an empty key and adds the key's length; a test holds that to Merk's encoder for every kind of aggregate and every key length a link can hold.
- **A node that is written.** Merk holds the node, and N1 is `grovedb_merk::cost_model::node_record_len`. A node has no record length where Merk could not encode it: with a link to a child whose key is longer than 255 bytes. `put_node` and `write_node` refuse it with the encoder's error, write nothing and charge nothing, and a node with no modified link is measured before it is taken apart. Merk cannot encode a node with a link to a child that is modified and not committed either, because such a link holds no hash. The store writes that node with the hash left to the engine (`node-store.md`, "Hashes"), and its N1 is that of the record Merk has once the hash is made, from the lengths of its parts: the hash of a link takes 32 bytes whatever it is. A child key longer than 255 bytes is refused in it as in any other node. Such a node is written only with its child: `write_node` refuses it and charges nothing, and the commit of a change set that does not write the child refuses it, and neither writes anything. The refused commit is charged what its change set was: nothing on a deferred handle, and on an immediate one the puts as they joined it (M2). Merk commits a child before it writes the parent, so no commit of Merk's hands over such a node.
- **A node that is removed with no cost.** N1 of the committed cell, by the first function. The transaction's own version of the node is not looked at.
- **The record length is never kept.** It moves when a descendant changes (section 9), so it is measured at each charge.
- **One seek for one fetch.** A cell names a child by its address, and Merk's `Link` needs the child's key, so the store reads the cell of each child with its parent: up to three reads of the transaction for one node, and more when a commit lands between two of them and the node is read again. The charge is one R1 for the operation Merk made. A link is followed to where its node was last seen, and that fetch costs what a lookup by the key costs.

### 15.3 One version

A handle is opened with no grove version on every engine: `Engine::subtree` takes none, and of the operations of a `NodeStore` only two are given one. So the store charges by one version of the rules, `grovedb_merk::cost_model::HANDLE_COST_MODEL`, as the RocksDB engine charges by one fixed version (section 14.3). A test holds that `storage_cost_model` gives that version for every grove version. A grove version that charges by other storage rules (GROVE-13) fails the test, and has to reach the handles of both engines before it can pass.

### 15.4 What the store does not charge, and where it is not RocksDB

1. **The nodes of a dropped subtree** are not charged. On RocksDB a tree is removed node by node, each a D2. `LigninBatch::drop_subtree` takes the subtree out of the directory and charges nothing. GROVE-05 has to charge the removal as RocksDB does, from the committed sizes of the nodes: the same `delete_uncosted` for each.
2. **A batch holds one change set that writes or removes nodes of a Merk** and refuses a second. Change sets of the same Merk that hold only records that are no nodes are all taken. A storage batch keeps one final operation for a key and charges only that one (B1). Until the batch across subtrees says which of two stands (TXN-08), a history that writes one Merk twice in one batch cannot be charged on the store at all. The same holds for the boundary between two operations that share a batch (`StorageBatch::next_operation`, which GroveDB's Merk cache and the repair of a ranking mark): on RocksDB a delete after the boundary stands against a put before it and is the one that is charged. The store's batch has no boundary: for a record that is no node it keeps the put, and a node that is written again after the boundary is a second change set of its Merk, which it refuses.
3. **A cell whose feature or aggregate is no kind of Merk's** is `CorruptedData`, charged as a read that found nothing. RocksDB charges a record that does not decode at its length. No history of the engine's own writes leads to either.
4. **A node written with `write_node` into a Merk that has no root** is taken by the transaction, and a lookup by its key that starts at the root does not find it: `write_node` states no root. A handle may still be served the node while the transaction has the write where the handle left it; the store keeps no way to a node that no root leads to. RocksDB finds the record by its key. Chunk restore writes this way (MERK-07).
5. **A change set names a key once** (node-store.md, rule 5 of the change sets), and the store charges each entry it is given. A storage batch that is given two puts of a key keeps the later and charges that one, keeps a put against a delete of the same key in the same operation, whichever came first, and of two deletes keeps the first. The store's batch keeps a record that is no node by the same rule, and the history of seam operations has each of the four cases. For a node it has no such rule, and needs none: Merk writes no key twice in a change set.
6. **A record that no link of the tree leads to** is a thing RocksDB can hold under a Merk's prefix and the store cannot: a node is a cell that its parent's link or the subtree directory names. GroveDB #1063 removes such records, which an earlier repair of an indexed tree's ranking left, with `delete_node(key, Some(cost))` in a change set that states no root. What the store does with that call today:
   - It charges it as any removal with a writer's cost, by D1: one seek and the bytes the writer says it frees, on a deferred and on an immediate handle. RocksDB charges the same whether or not it holds the record, because D1 measures nothing.
   - The transaction takes it as one write of the key (ST5) and changes no cell. The Merk's root stays where it is and every node is read as before.
   - So the charge is RocksDB's, and the state differs only in that there was no record to remove. Where the store cannot give RocksDB's charge is everything that measures such a record: a read of its key (R1: a lookup loads the record's length on RocksDB and nothing on the store, one seek and 120 bytes against one seek and none for the record of the test), a removal with no cost (D2, which loads its committed length), a cursor that walks over it (C1 to C3), and with them the removal of a whole tree that holds one. Holding such records is MERK-09.

### 15.5 How the store is checked

| What | Where | Holds |
| --- | --- | --- |
| A Merk on both engines | `merk/src/store/lignin/tests/cost.rs`, `a_merk_on_lignindb_costs_what_it_costs_on_rocksdb_on_every_grove_version` | On each of the five grove versions and for twelve tree types, a Merk on the store and a Merk on RocksDB are opened (as a base Merk, a layered one and one that stands alone), written by the same random batches and read, on deferred handles with a batch and on immediate handles. The reads are every read of a Merk that walks no cursor: by key in each of Merk's ways, the two reads that keep the nodes they fetch (`observe_old_value`, `observe_value_and_value_hash`), an aux and a meta record, a proof, the aggregates of a range and their proofs, the queries of a trunk and of a branch, the proofs of a page that is skipped to by count, the walk of every link (`linked_keys`), and the root hash. A read that walks the tree is charged only the fetches of the nodes no earlier read loaded, so the order of those reads changes from round to round, and the test holds that each kind of read was charged a fetch of its own, but for the root hash, which fetches nothing. On deferred handles the walk of every link is made on a Merk that was opened for it, where it costs one fetch for each node but the root and gives the keys the batches left, and again on the Merk it loaded, where it costs none. On immediate handles it is made on the Merk that stays open, and compared. The batches issue every operation of a Merk batch, which the test holds by name: ordinary values, values with a cost of their own (as sum items and trees have, hashed with one root or with two), values with a combined hash and with a provided one, values with flags, each kind of removal, and elements that move to another key as GroveDB renames one (`DeleteMoved` and `PutMoved`): to a longer key, to a shorter one, and two into each other's place. A move is made with the calls GroveDB makes for one. The element is read at the key it leaves and the key it moves to is looked at, by the reads that keep their nodes, and the batch is then applied to the Merk those reads left. On deferred handles that Merk was just opened and has loaded nothing but its root; on immediate handles it is the Merk that stays open. An element with flags moves to a key of another length here too, which GroveDB refuses and Merk bills: the bytes a shorter key frees are sectioned by the client's callback. The test counts those moves, to a longer key and to a shorter one, with the others. A client's two callbacks keep the epoch of a replaced value and section the bytes it frees. Every call, the application of every batch and every read of a meta record cost the same on both, with the removal arithmetic of the grove version. |
| Each operation of the seam | the same file, `every_operation_of_the_store_costs_what_it_costs_on_rocksdb` | One history is played on handles of both engines, and each step and each applied batch must cost and end the same: every read, found and not; puts with no cost, with a cost that is checked, with one that is taken as given and with a prepaid one; removals with a cost, plain and sectioned, and with none; the root key, aux and meta records in the same ways; a record a batch writes twice, by two puts, a put and a delete in either order, and two deletes with different costs; a batch of two subtrees whose removals add up to another sum when they are charged in another order; a put whose cost does not match, with writes before it in the order of a batch and after it a removal that would load a record; a batch the transaction does not take; a handle with no batch; and an immediate handle, whose change set also writes a record twice (a removal and then a put, and two removals with different costs): there the later write stands and each is charged as it is given, on both engines. It is played under both removal arithmetics: under the first, the order in which a batch is charged, by keyspace and by subtree, shows in the bytes removed. |
| D2 | `a_delete_with_no_cost_is_charged_from_the_committed_record`, `the_nodes_of_a_merk_removed_with_no_cost_are_charged_as_committed` | A node the transaction rewrote is removed at the length of the committed record, and one it created as an empty record, with the numbers of rule D2 and equal to RocksDB. Every node of a Merk whose shape the transaction changed is removed with no cost, as `Merk::clear` and the removal of a whole subtree do: the bytes loaded are those of the committed records. |
| One seek for one fetch | `a_fetch_is_one_seek_and_the_record_length_however_it_is_served` | A fetch that reads three cells, one that is read again five times, one that follows a link and a lookup that reads one cell each cost one seek and the record length. |
| The ledger | `the_store_reports_what_the_model_hands_out_and_nothing_else` | The same history on the store alone, under both removal arithmetics: after each part, the costs the store reported add up to what the model handed out (section 14.2). The costs are added as the ledger adds them, each to the sum before it: under the first arithmetic a section of the owner that plain bytes are counted to, followed by plain bytes, adds up to another sum by `+`, and the history has that order. |
| A conversion to a 128-bit sum | `a_conversion_to_a_big_sum_tree_costs_what_it_costs_on_rocksdb` | See below. |
| Proofs (MERK-06) | `merk/src/store/lignin/tests/proof.rs` | Every kind of proof, and the reads and writes that follow one on the same Merk, on both engines, on every grove version, twelve tree types and both kinds of handle: each call is charged the same. A proof on the store fetches its nodes without their values and keeps none in the tree, and the calls that follow it are charged as on a Merk that kept them (node-store.md, "Proofs" and its table of tests). |
| A removal of a key that is no node | `a_removal_of_a_key_that_is_no_node_is_charged_and_changes_nothing` | Item 6 of section 15.4: the charge with a cost, on both kinds of handle, equal to RocksDB's with and without such a record there; the charge with no cost, equal to RocksDB's where it holds no such record; one write of the key; the tree and the root in the directory as they were. It also pins what RocksDB charges for such a record that is removed with no cost, which the store cannot charge yet. |
| N1 from sizes | `merk/src/cost_model.rs` | The length is that of the record Merk encodes, for every kind of feature and of aggregate and every key length of a link. |
| The corpus | `lignin-testkit/tests/merk_store.rs` | See below. |

**The conversion of a tree to a 128-bit sum** (`Merk::convert_to_big_sum_tree`, GroveDB #1063) reads every node of a tree and writes each again with a wider feature and wider aggregates in its links. No key, value or value hash changes. The test tries the conversion of the same Merk on both engines, on every grove version, for the twelve tree types of the store's other tests and the two indexed trees that keep a 64-bit sum: those two and the five plain trees that keep one are converted on grove version 5 (an indexed tree as its primary Merk, whose nodes are those of the tree it mirrors), and every other conversion that is tried is refused alike. On a deferred handle the tree holds nodes of the committed state and nodes its transaction wrote.

- **The charges are RocksDB's**: the fetches of the walk, the conversion itself, the batch it stages (one put for each node, charged by W2 with the cost Merk gives, which adds up to between 16 and 17 added bytes a node over the tree), the conversion through an immediate handle, and every read of the converted tree. The records read back are RocksDB's, byte for byte.
- **No value is written.** The seam has no kind for "the feature changed", so Merk reports every node as `ValueWritten` and states its value. The store compares the stated bytes with the bytes it holds and keeps them: for a tree of n nodes, n values stated, none written.
- **What the comparison costs** is one read of the transaction for each node, and with it the bytes of each value, also of one that is kept out of line. The read goes through the location the conversion's own fetch of the node left in the handle, so no key is looked up while the transaction serves its nodes from its frames, which is the case the test pins: it counts n reads through a location and none by key while the batch is applied. The count is of this placement and not of the contract: a location that a commit made stale, or a node the handle was given no location for, is looked up by key at the same charge. None of it is in the `OperationCost`, which is of sizes. It is work a kind that names the feature alone would spare (MERK-02), and PERF-03 moves the comparison to where the cell is at hand.
- **The subtree directory is not told.** The conversion compared here is Merk's: it writes nodes. The tree type that the directory holds of the Merk stays the one it was entered with, and `Merk::open_from_directory` would open the converted tree by it. The transaction offers no write of a tree type yet (node-store.md, choice 5 of MERK-04); stating it is for the task that runs GroveDB's conversion on LigninDB.

**What is not compared.** `Merk::clear`, `is_empty_tree` and `is_empty_tree_except` walk a cursor, which the store does not have yet (MERK-05); what `clear` then removes is a node with no cost, which the rows on D2 hold. The reads through a pinned view (`read_value_from_view`, `local_count_offset_page_from_view`) also walk a cursor, which their caller gives them. Chunk production and restore return no cost; what restore writes is MERK-07's. A read-only handle opens a Merk and reads it, and refuses every write. No Merk is opened on one here: what is compared on a read-only handle is the read of every committed node, in the corpus replay.

**The corpus.** GroveDB does not run on LigninDB before GROVE-01, so the corpus is replayed on RocksDB and what each operation did to the nodes of every Merk is handed to the store (node-store.md, "How it is checked"). Since MERK-03 the same change sets also go, call for call, to a second RocksDB storage that holds nothing else, and the replay compares:

- **Every batch.** Staging a change set costs the same on both, and so does the application of the batch. Each written node is given one of the costs a writer gives, by turns: none, that of a new node and that of an update, whose value cost the storage layer checks against the record (K3), and a cost that is taken as given, with bytes freed as plain numbers and by owner and epoch. Each removed node is given a cost or none. The replay itself hands every node over with no cost; the change set it fills (`Billed`) gives the costs, the same to both engines. The second storage is committed whenever the LigninDB transaction is, so a removal with no cost meets the same committed state on both.
- **Every read.** Each node that is read back, after the write that changed it and after each commit, is read on both engines, and each read by itself costs the same: a seek and the length of RocksDB's record, which the store works out from the cell. The RocksDB of these reads is the one GroveDB wrote. A lookup of a written node that reads no value (`has_node`) costs the same again. A lookup of a removed node finds nothing and costs what it costs on the second storage, which finds nothing either.
- **A row that no node links.** Since the sync with `develop` `2cfc1ce1` the corpus holds the state of item 6 of section 15.4: on grove versions 1 to 4 the hand-written case `legacy-ranking-drift` leaves, under GroveDB, a row of a ranking whose record RocksDB keeps and no node links. The replay hands the store what the tree saw, the removal of the node, and the second storage the same, so the two are charged alike and hold no record there. RocksDB under GroveDB does: a lookup of that key loads the row there and nothing on the store.

On the standard corpus, 380 traces on grove versions 1 to 5, replayed 570 times: 237,713 batches of 3,622,666 written and 99,656 removed nodes were charged the same on both (3,772,227 seeks, 2.2 GB of stored bytes), 49,905 of the removals with no cost (5.8 MB loaded from the committed state), and 9,541,396 reads (7.1 GB loaded).

What this shows and what it does not:

- The costs a writer gives are made up by the replay, so it shows that the store charges a node of the corpus as RocksDB does for the same sizes and the same cost, and not what GroveDB's Merks gave with each node. What Merk gives is the same on both engines, because Merk's rules run above the seam; the first row of the table runs them on both.
- It compares each operation of the store with RocksDB, and not the `OperationCost` of a GroveDB operation with the recorded one. That comparison needs GroveDB on LigninDB, and is GROVE-10's and VER-02's.

### 15.6 Sensitivity

A check that cannot fail shows nothing, so 31 faults were put into the store and into `node_record_len_from_sizes`, one at a time, and the checks run on each: the tests of `merk/src/store/lignin/tests/cost.rs` with the N1 tests of `merk/src/cost_model.rs`, and for the faults that reach a node the corpus replay on two traces, `v3/balances` (sum trees, no removal) and `v5/delete-heavy`. Every fault is found. The second column names the tests that fail: **N1** for the tests of the record length, **Merk** for the Merk on both engines, **seam** for the history of seam operations, **D2** and **D2 tree** for the two tests of removals with no cost, **fetch** for the one seek of a fetch, **conversion** for the conversion to a 128-bit sum, **unlinked** for the removal of a key that is no node, **ledger** for the ledger, and **refused** for the node that has no record length.

| Fault | Tests of the store that fail | Corpus replay |
| --- | --- | --- |
| N1 of a cell: one byte more | N1, Merk, seam, D2, D2 tree, fetch, conversion, unlinked | both traces fail |
| N1 of a cell: the child's aggregate left out | N1, Merk, D2 tree, conversion | both traces fail |
| N1 of a cell: the child's key length left out | N1, Merk, D2 tree, fetch, conversion | both traces fail |
| N1 of a cell: the feature measured as a basic node's | N1, Merk, seam, D2 tree, conversion | both traces fail |
| N1 of a written node: one byte more | Merk, seam, D2, D2 tree, fetch, conversion, unlinked, ledger | both traces fail |
| R1: a deferred handle's read charged nothing | Merk, seam, D2, D2 tree, fetch, conversion, unlinked, ledger | both traces fail |
| R1: a seek for each cell the transaction read for a fetch | Merk, seam, D2, D2 tree, fetch, conversion, unlinked | both traces fail |
| R1 of `has_node`: the value's length left out | seam, fetch, unlinked | both traces fail |
| R2: an immediate handle's read charged as a deferred one's | Merk, seam, fetch, conversion | not run |
| `W-tx`: a staged commit charged a seek | Merk, seam, D2, D2 tree, conversion, unlinked, refused | both traces fail |
| W1 left out: the stored key of a new node is not added | Merk, seam, D2, D2 tree, fetch, conversion, unlinked, ledger | both traces fail |
| W3: the root key charged as an aux record | Merk, seam | not run |
| The stored key of a record measured without the prefix | Merk, seam, D2, D2 tree, fetch, conversion, unlinked, ledger | both traces fail |
| D1: a removal with a cost charged as one with none | Merk, seam, conversion, unlinked | `delete-heavy` fails |
| D2 of a node: charged from the transaction's view | seam, D2, D2 tree | `delete-heavy` fails |
| D2 of a record that is no node: charged from the transaction's view | Merk, seam | not run |
| D2: a key the committed state lacks charged as a record of one byte | seam, D2, D2 tree, unlinked | `delete-heavy` fails |
| B1: the records that are no nodes charged after the nodes | Merk, seam | not run |
| B1: nodes charged by key and not by subtree first | seam | `balances` fails |
| B1: nodes charged in the order they were given, puts before removals | Merk, seam, conversion | `balances` fails |
| B1: storage cost charged for a batch the transaction did not take | seam | not run |
| B1: storage cost charged for a batch with a refused put | seam | not run |
| A batch: a delete stands against an earlier put of the same record | seam | not run |
| A batch: the later of two removals of a record stands | seam | not run |
| M1: a write made directly through an immediate handle charged a seek | Merk, seam | not run |
| M2: the cost of an immediate handle's commit dropped | Merk, seam, conversion, unlinked, ledger | not run |
| M2: an immediate change set charged without the sizes of a node's links | Merk, seam, conversion | not run |
| A refused node write charged a seek | refused | not run |
| The comparison that keeps a value left out: every stated value is written | conversion | not run |
| A charge made beside the model, with the model's number | ledger | not run |
| A charge of the model that is dropped: a read asks the model twice | ledger | not run |

How the table was made, and what it showed:

- The second column is of the tests as the cost conformance left them: the run was made on its last commit that changed `cost.rs` or the store. The test of a refused node has changed since, with the hash bridge (`node-store.md`, "Hashes"): it also holds the refused commit of an immediate handle to the charge of its put. A fault in M2 or in the length of a written node can therefore fail it as well, which the column does not show, and the faults were not run again on it.
- The third column is older. It is of the replay before two changes to it: the lookup of a removed node is compared with the second storage and no longer with GroveDB's, and each read of a committed node is compared by itself and no longer as a sum over its Merk. Each fault the replay found, it found by a comparison that is made as it was: what a batch is charged, what staging a change set is charged, the read of a written node, or the lookup of one. The first four rows were run again on the replay as it is, with the same result.
- The later of two removals of a record standing is what the store did until a review of this unit; the fault puts it back. No Merk and no trace of the corpus removes a record twice in one batch, so only the history of seam operations finds it.
- A fault counts as found only where a test fails with its own assertion. Unchanged, the store passes every one of these checks.
- The last two faults change no number the store reports, so no comparison with RocksDB sees them. The ledger does.
- The fault before them is no fault in a charge: with it the store charges what RocksDB charges and writes every value again. The conversion test finds it by counting the values written.
- The first run of the faults found two holes in the replay, which then passed with a seek charged for a staged commit and with `has_node` measured without the value. The replay compares both now.
- Charging from the transaction's view in D2 is found by the replay as well as by the tests of the store: on `delete-heavy` it removes, with no cost, nodes that its own transaction wrote, and compares what each batch is charged.

### 15.7 For later tasks

- **MERK-04.** Opening a Merk charges the reads it makes: the root key record of a base Merk and the fetch of the root node. A root that comes from the subtree directory is still one R1 for the node.
- **MERK-05.** The cursor charges C1 to C3: `cursor_move`, `cursor_key` with the stored key's length, and `cursor_value` with that and `node_record_len_from_sizes` of the cell. `Merk::clear` then removes each node with no cost, which the store charges by D2 already.
- **MERK-07.** Chunk restore writes through immediate handles (M1, M2), which the history above covers, and with `write_node` (item 4 of section 15.4).
- **MERK-08.** Merk's own tests assert costs in many places; they are the check of the cursor's charges.
- **GROVE-02.** Propagation writes the parent's element with the cost Merk gives, through `put_node`; nothing in the store depends on which Merk a write belongs to.
- **GROVE-03.** A node that is copied to another subtree is a put with no link sizes, charged by its record length: `node_record_len_from_sizes` of the cell it is copied from.
- **GROVE-05.** Item 1 of section 15.4.
- **MERK-09.** Item 6 of section 15.4: once the store holds a record that no link leads to, a removal of it with no cost, and a cursor over it, have to charge its length as RocksDB does. `a_removal_of_a_key_that_is_no_node_is_charged_and_changes_nothing` has RocksDB's numbers for one such record.
- **GROVE-13.** Section 15.3.
