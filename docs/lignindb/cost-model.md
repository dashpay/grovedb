# Cost model: what today's code charges

LigninDB task BASE-06 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

This document specifies what GroveDB charges today, as an `OperationCost`, for each logical event: opening a subtree, fetching a node, writing or removing a record, moving a cursor. Each charge is given as a function of logical sizes (lengths of keys and values, the fields of a node), not of bytes on disk. SEAM-05 moves the storage part of these rules into versioned functions that both engines call. FMT-02 and MERK-03 build an engine that keeps no node record and still has to produce every one of these numbers.

It builds on the [storage surface inventory](storage-surface.md) (BASE-04), the [FlatStore seam](flat-store.md) (SEAM-03), the [NodeStore seam](node-store.md) (SEAM-01) and rule GM0 of the [transaction rules](transactions.md) (TXN-01). The cost codes of the inventory (`P-path`, `R-tx`, `B-commit` and so on) are named next to each rule.

**Pinned commit:** the engine sources of `feat/LigninDB` at `54e36fb4` ("Merge BASE-01: the remaining cost-bearing operations"), which are those of `develop` `22f163c2`. Functions are cited by name, with the line at the pinned commit. Since then `develop` has been merged three times, up to `f1016919`, up to `2a79d035` and up to `2cfc1ce1`, and the golden corpus recorded again from each (`lignin-testkit/corpus/README.md`). The rules below predict every cost of the corpus recorded from `2cfc1ce1` as they did of the first, and the citations have not been moved.

**Since SEAM-05** the storage rules of section 5 are the functions of `grovedb_costs::cost_model`, and the RocksDB engine charges only through them. Section 14 says which function is which rule. Section 15 says how the LigninDB store, which keeps no node record, charges through the same functions (MERK-03). The citations of section 5 are of the code at the pinned commit, where the rules were written out in the storage contexts.

**Grove version 6** changes which operation is charged for carrying a changed Merk into the element that hosts it. Its rules are section 16 (GROVE-13). Sections 2 to 15 are of grove versions 1 to 5.

**Result:** the storage rules of section 5, applied to a logical state that holds no node record, reproduce every `OperationCost` of the golden corpus: 430 traces and 235,858 steps on grove versions 1 to 5 at the standard scale, and the five reference traces of 1,050,008 operations each. Section 12 says how that is run, what it shows and what it does not. One thing it does not show: the corpus cannot tell whether a delete with no writer's cost is charged from the committed state or from the transaction's view (rule D2), because the two differ only for a record the same transaction wrote, and the corpus deletes such records only in an operation whose cost GroveDB drops. The storage crate's tests pin that instead.

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

- **Opening.** A layered Merk (a subtree whose root key its parent element names) takes its root key from its parent element and fetches that node: one R1. A base Merk (the root subtree, and the secondary of an indexed tree whose element names no root for its axis) first reads its root key from the roots keyspace, one R1 with `value_len` the key's length, then fetches the node. A layered Merk with no root key fetches nothing. A base Merk pays the read of its root key whether or not there is a record. Where there is none it fetches nothing; where a record names a node that the store does not hold, as the record that an earlier tree under the prefix left does, it pays the fetch of that node too, one seek.
- **A write** walks from the root to the key, fetching each node on the way that is not loaded. Rebalancing fetches the child a rotation moves. Removing a node with two children walks down the edge of the taller side and fetches each node there.
- **A read by key** (`Merk::get` and its relatives; `get_node_fn`, `merk/src/merk/get.rs:425`) walks the loaded part of the tree and, at the first link that is not loaded, reads the key directly: one R1 for the key itself, found or not, and none for the nodes between.
- **A proof** or a chunk walks the tree through the same fetches. A proof, and the producer of a restore's chunks, asks for a node with `NodeStore::fetch_node_without_value`, which a store may answer without the node's value: it is **one R1 all the same, with the length of the whole record** (section 15.1). A node that comes without its value is not kept in the Merk's tree. The Merk keeps its key instead and is not charged for that node again, by a later proof, by a write that walks over it or by a read of the nodes in memory, so that a Merk is charged the fetches it would be charged if it kept every node (node-store.md, "Proofs").
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
golden corpus: 430 traces replayed on the cost model, 430 as recorded (235858 steps), 0 not
```

The replay is BASE-02's: it compares the cost, the result, the root hash and the proof of every step with the committed corpus, and names the first step and part that differ. A green replay on the model therefore also says that the model gave the same answers as RocksDB wherever the corpus looks: every result read, every root hash and every proof. It does not say that the two states are equal record by record; an aux or meta record that nothing reads would not be noticed.

At the reference scale a trace has a digest every 1,000 steps and not a line per step, so a difference is named by its interval of 1,000 steps. The step is found by dumping that interval on both sides and comparing: `check.sh dump --reference --grove-version N --steps FROM..TO --out DIR` on the model, and `lignin-golden dump` with the same arguments on RocksDB.

Everything that was run on the model, at the pinned commit:

| What | Command | Result |
| --- | --- | --- |
| The corpus at the standard scale: 86 traces on each of grove versions 1 to 5, recorded from `develop` `2cfc1ce1`. Run again when `develop` was merged up to that commit | `check.sh` | 430 traces as recorded, 235,858 steps |
| The corpus at the reference scale: 1,050,008 operations on each grove version | `check.sh replay --reference` | 5 traces as recorded, 5,250,055 steps |
| The storage crate's tests | `check.sh test -p grovedb-storage --features rocksdb_storage -- --skip cost_model::` | 51 pass. The crate's other 14 are unit tests inside `rocksdb_storage/storage.rs`, the file the model replaces. |
| Merk's tests | `check.sh test -p grovedb-merk` | 872 and 9 doc tests pass |
| GroveDB's tests, with its default features | `check.sh test -p grovedb --no-fail-fast` | 3,782 of the library's tests pass, 8 are ignored as they are on RocksDB, and 1 fails: `test_checkpoint_wal_under_threshold_is_preserved` looks for RocksDB's WAL files in a checkpoint directory, and the model writes none. The 80 tests of the eight integration binaries and the 3 doc tests pass. |
| Each rule against RocksDB, in the workspace's own tests | `cargo test -p lignin-testkit --lib cost_model` | 37 pass |

The three test suites are the engine's own, unchanged. They assert costs in many places, so a rule the corpus does not reach is still compared with numbers that were written against RocksDB. The `--skip` leaves out the model's own unit tests, which the stand-in crate also compiles.

The last row runs inside the real workspace: `cost_model/against_rocksdb.rs` performs the same batches, reads and cursor walks on a RocksDB storage and on the model and compares every cost and every result, and `cost_model/against_merk.rs` compares the record layout of section 4 with Merk's encoder for every feature and link.

**Sensitivity.** A check that cannot fail shows nothing, so each rule was changed in the model, one change at a time, and the corpus replayed. The table and the three paragraphs after it are of the corpus as it was then, 290 traces. They were not made again when the corpus grew to 310, to 375, to 380 and to 430: each of the thirty changes is a hand edit of the model, a release build of its workspace from `grovedb-costs` up, and a replay:

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

The model counts every application of a rule, by case. `check.sh` prints the counts of a replay and keeps those of any run in `target/cost-model/rule-hits.tsv`. The first column is the corpus at the standard scale, the 430 traces recorded from `develop` `2cfc1ce1`; the others are the three test suites of section 12.3 as they were run at the pinned commit, added up over their test processes.

| Rule | Case | Corpus | Storage tests | Merk tests | GroveDB tests |
| --- | --- | ---: | ---: | ---: | ---: |
| P1 | a subtree handle opened by the empty path | 641,496 | 0 | 3,541 | 227,613 |
| P1 | a subtree handle opened by a path of one segment or more | 1,851,545 | 132 | 3 | 801,434 |
| P2 | a subtree handle opened by a prefix | 43,288 | 13 | 0 | 33,547 |
| P3 | the prefix of an indexed tree's secondary derived | 40,651 | 0 | 0 | 18,362 |
| R1 | a data record read through a deferred handle | 4,959,160 | 24 | 217,766 | 1,607,100 |
| R1 | a data key with no record read through a deferred handle | 36,636 | 13 | 1 | 9,623 |
| R1 | an aux record read through a deferred handle | 290 | 12 | 1 | 19 |
| R1 | an aux key with no record read through a deferred handle | 40 | 10 | 0 | 178 |
| R1 | a root key record read through a deferred handle | 416,138 | 11 | 3,008 | 117,923 |
| R1 | an absent root key record read through a deferred handle | 4,421 | 4 | 421 | 12,733 |
| R1 | a meta record read through a deferred handle | 0 | 10 | 0 | 8 |
| R1 | a meta key with no record read through a deferred handle | 0 | 4 | 2 | 22 |
| R2 | a read through an immediate handle | 0 | 461 | 29,226 | 16,366 |
| W1 | the stored key added to the key cost of a new data record | 250,275 | 1 | 108,684 | 250,138 |
| W2 | a data put with the sizes of a node's links and feature | 2,987,875 | 0 | 162,830 | 915,178 |
| W2 | a data put with no link sizes | 14,873 | 78 | 23,135 | 39,798 |
| W2 | an aux put | 1,155 | 34 | 2 | 21 |
| W2 | a meta put | 522 | 33 | 2 | 35 |
| W3 | a root key put with a cost | 0 | 2 | 0 | 0 |
| W3 | a root key put with no cost | 141,514 | 23 | 3,100 | 28,687 |
| W4 | a prepaid put | 1,213 | 14 | 0 | 745 |
| K2 | a value cost worked out from the record length | 938,468 | 132 | 181,551 | 505,444 |
| K2 | a value cost taken from the writer as given | 2,065,957 | 15 | 4,418 | 449,588 |
| K3 | a writer's value cost checked against the record length | 928,892 | 2 | 158,412 | 488,502 |
| K3 | a writer's key cost checked against the stored key | 250,261 | 1 | 108,382 | 250,061 |
| K3 | a write refused because the writer's cost does not match | 6 | 0 | 0 | 3 |
| D1 | a data delete with the writer's cost | 73,016 | 0 | 1,852 | 10,543 |
| D1 | an aux delete with the writer's cost | 0 | 0 | 0 | 0 |
| D1 | a root key delete with the writer's cost | 0 | 0 | 1 | 65 |
| D1 | a meta delete with the writer's cost | 0 | 0 | 0 | 0 |
| D2 | a delete with no cost of a record in the committed state | 26,220 | 24 | 0 | 1,011 |
| D2 | a delete with no cost of a key the committed state lacks | 3,039 | 11 | 0 | 23 |
| M1 | a write through an immediate handle | 0 | 481 | 5,192 | 2,407 |
| M2 | a put in the batch of an immediate handle | 0 | 7 | 24,501 | 12,024 |
| M2 | a delete in the batch of an immediate handle | 0 | 1 | 3 | 0 |
| C1 | a cursor moved to the first record | 75,998 | 86 | 1,630 | 26,191 |
| C1 | a cursor moved to the last record | 963 | 10 | 12 | 434 |
| C1 | a cursor moved to the first key at or after a key | 3,198 | 3 | 307 | 5,050 |
| C1 | a cursor moved to the last key at or before a key | 98 | 2 | 0 | 233 |
| C1 | a cursor stepped forward | 206,557 | 106 | 43,047 | 49,207 |
| C1 | a cursor stepped back | 8,485 | 28 | 3,348 | 4,839 |
| C2 | a cursor asked for its key or validity on a record | 767,215 | 388 | 148,950 | 214,692 |
| C2 | a cursor asked for its key or validity off its subtree | 65,588 | 92 | 208 | 18,794 |
| C3 | a cursor asked for the value of a record | 185,047 | 110 | 46,699 | 56,654 |
| B1 | a batch applied to a transaction | 193,627 | 43 | 3,413 | 88,148 |
| B1 | a batch whose writes were refused | 0 | 1 | 0 | 2 |
| N1 | a data record kept as the fields of a node | 2,992,137 | 0 | 191,097 | 942,062 |
| N1 | a data record that is not a node, kept as bytes | 10,611 | 207 | 2 | 14,839 |
| N2 | the link and feature sizes Merk gave with a node are the node's | 2,987,875 | 0 | 162,830 | 915,178 |
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

The storage contexts of `grovedb-storage` are opened with no grove version: nothing in the `Storage` trait carries one, and `build_prefix` and `secondary_prefix_for` are associated functions. So the RocksDB engine charges by one fixed version (`rocksdb_storage::storage::COST_MODEL`, `V0`). That is correct while every grove version maps to `V0`. A grove version that is given other storage rules has to reach those contexts as well, if RocksDB is still an engine then. Grove version 6 is given none: its rules are of which operation makes a charge, above the storage layer (section 16).

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
- **GROVE-13.** Done: section 16. It needed no new storage rule, so `StorageCostModel` has the one variant still. A later grove version that changes what a put, a fetch or a cursor is charged adds a variant, and `storage_cost_model` maps that grove version to it (section 14.3).

### 14.7 Estimates (SEAM-08)

LigninDB task SEAM-08 took the one storage number of the estimated-cost helpers out of the engine.

`get_storage_context_cost` was an associated function of the `Storage` trait, so an estimate of what a subtree handle costs to open had to name a storage engine to ask. It is now a free function, `grovedb_storage::worst_case_costs::get_storage_context_cost(model, path)`, next to `WorstKeyLength`, the trait that gives the longest length a path segment may have. It is one call of `open_by_path` (P1) with those lengths. The module is built without the `rocksdb_storage` feature, and its tests run with no RocksDB crate among the crates that are built. The `Storage` trait has no estimate left, so a second engine implements none.

The function takes the `StorageCostModel` and names no version. GroveDB's six callers (`add_worst_case_get_merk_at_path`, `add_worst_case_has_raw_cost`, `add_worst_case_get_cost` and their three average-case forms) pass `storage_cost_model(grove_version)`, so an estimate follows the rules of the grove version it is made for. No number changed: for grove versions 1 to 5 that is `V0`, the rules the RocksDB engine reported the estimate by.

What is left as it was:

- **The `S: Storage` parameter** of the fifteen public estimate helpers of `GroveDb` (the eleven `add_worst_case_*` and `add_average_case_*` functions and the four delete estimators). No helper takes anything from it any more: it is passed on or left unused. It stays because Platform calls two of the delete estimators with `::<RocksDbStorage>` (P-32 of [platform-requirements.md](platform-requirements.md)), so five call sites inside GroveDB's estimate code still name `RocksDbStorage` to fill it. Removing the parameter changes the public interface and is made in step with Platform (SEAM-07, OPS-03). Until then GroveDB's estimate code is built with an engine's type in scope, though no number comes from it.
- **Merk's and GroveDB's own estimates** (`merk::estimated_costs`, `grovedb::estimated_costs`, `grovedb::batch::estimated_costs`): the seeks, loaded bytes, storage bytes and hash counts they add from estimated sizes are estimates of sections 6 and 7 and name no engine.
- **Two estimates that restate a storage rule in their own arithmetic.** `average_case_indexed_secondary_mirror` adds one hash for the prefix of a secondary (P3), and `add_flat_drop_record_put_estimate` (`grovedb/src/operations/delete/flat_drop.rs`) works out the blocks of P1, three hashes of P3 and the bytes of the record's put. Neither needs an engine. A version of the storage rules that changes P1 or P3 has to change them, or route them through the model then. Grove version 6 changes neither.

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
| C1 to C3 | the cursor of a handle (`LigninCursor`) | `cursor_move()` for each move, `cursor_key` with the stored key's length for `key()` and `valid()`, and for `value()` `cursor_value` with that and N1 of the record, or `cursor_key` alone where it gives no record (node-store.md, "The cursor"). |

The store adds nothing to a cost the model returns and drops none. It states no number of its own: a length is measured, and the model turns it into a charge.

### 15.2 Sizes without a record

- **A node that is read.** N1 is `grovedb_merk::cost_model::node_record_len_from_sizes`: the length of the value, the feature, and for each child the length of its key and the aggregate of the subtree under it. A cell holds all of them: the length of a value that is kept out of line, the feature and each aggregate as Merk's tag and numbers, and in each link the length of the child's key ([tree-page.md](tree-page.md) TP10, TP12). No record of a node is serialized for it and no child's cell is read. The feature and the aggregates are measured by Merk's own encoders, which write the few numbers of one to count them, so a kind Merk gains is measured as Merk encodes it. A link encodes its child's key after one length byte, so the function measures a link with an empty key and adds the key's length; a test holds that to Merk's encoder for every kind of aggregate and every key length a link can hold.
- **A node that is written.** Merk holds the node, and N1 is `grovedb_merk::cost_model::node_record_len`. A node has no record length where Merk could not encode it: with a link to a child whose key is longer than 255 bytes. `put_node` and `write_node` refuse it with the encoder's error, write nothing and charge nothing, and a node with no modified link is measured before it is taken apart. Merk cannot encode a node with a link to a child that is modified and not committed either, because such a link holds no hash. The store writes that node with the hash left to the engine (`node-store.md`, "Hashes"), and its N1 is that of the record Merk has once the hash is made, from the lengths of its parts: the hash of a link takes 32 bytes whatever it is. A child key longer than 255 bytes is refused in it as in any other node. Such a node is written only with its child: `write_node` refuses it and charges nothing, and the commit of a change set that does not write the child refuses it, and neither writes anything. The refused commit is charged what its change set was: nothing on a deferred handle, and on an immediate one the puts as they joined it (M2). Merk commits a child before it writes the parent, so no commit of Merk's hands over such a node.
- **A node that is removed with no cost.** N1 of the committed cell, by the first function. The transaction's own version of the node is not looked at.
- **The record length is never kept.** It moves when a descendant changes (section 9), so it is measured at each charge.
- **One seek for one fetch.** A cell names a child by its address, and Merk's `Link` needs the child's key, so the store reads the cell of each child with its parent: up to three reads of the transaction for one node, and more when the location of the node's cell ends between two of them and the node is read again. The charge is one R1 for the operation Merk made. A link is followed to where its node was last seen, and that fetch costs what a lookup by the key costs.

### 15.3 One version

A handle is opened with no grove version on every engine: `Engine::subtree` takes none, and of the operations of a `NodeStore` only two are given one. So the store charges by one version of the rules, `grovedb_merk::cost_model::HANDLE_COST_MODEL`, as the RocksDB engine charges by one fixed version (section 14.3). A test holds that `storage_cost_model` gives that version for every grove version, grove version 6 among them (section 16). A grove version that charges by other storage rules fails the test, and has to reach the handles of both engines before it can pass.

### 15.4 What the store does not charge, and where it is not RocksDB

1. **The nodes of a dropped subtree** are charged by the removals that are staged for them, and not by the drop. On RocksDB a tree is removed node by node, each a D2, and GroveDB stages the same removals on the store: `FlatStore::clear` on a handle whose Merk has a node walks the node cursor (C1 to C3) and stages `delete_node(key, None)` for each key, and a delete that keeps backward references up removes each node through its change set. The batch is settled with them, so each is charged `delete_uncosted` by D2, the removal of a record that no link of the tree leads to among them (item 6). Where the element that hosts the subtree is removed in the same group, the engine drops the subtree and hands the transaction none of those removals (`LigninSettled::drop_subtree`, `leave_out`, GROVE-05): the drop itself charges nothing. `a_merk_cleared_as_the_records_of_its_keyspace_costs_what_it_costs_on_rocksdb` holds the walk and the batch to RocksDB's charges, and the histories of `grovedb/src/engine/lignin/tests/deletes.rs` the deletes of GroveDB.
2. **A Merk that more than one change set of a batch writes** is charged as a storage batch charges it: the batch holds one outcome for each key, a node's as any record's, and charges the call that stands for it (B1; node-store.md, "The batch of an operation"). That holds with a boundary between two operations that share the batch (`LigninBatch::next_operation`) and with none.
3. **A cell whose feature or aggregate is no kind of Merk's** is `CorruptedData`, charged as a read that found nothing. RocksDB charges a record that does not decode at its length. No history of the engine's own writes leads to either.
4. **A node written with `write_node`** is a written node as any other, and `write_node` states no root. Where the tree does not reach it once its write is applied, by the rule of `lignindb::txn` (`MerkWrite`, "Unlinked records"), it is kept as an unlinked record (item 6). A lookup by its key finds it and is charged as item 6 says. A chunk restore corrects a parent's link with `write_node` on a store that takes the chunks as they arrive. The store takes none: it is handed the restored Merk in one change set that states its root (item 7).
5. **A change set names a key once** (node-store.md, rule 5 of the change sets), and the store charges each entry it is given. A storage batch that is given two puts of a key keeps the later and charges that one, keeps a put against a delete of the same key in the same operation, whichever came first, and of two deletes keeps the first. The store's batch keeps every record by the same rule, a node as any other record (item 2), and the history of seam operations has each of the four cases for a record that is no node.
6. **A record that no link of the tree leads to** is a thing RocksDB can hold under a Merk's prefix, and the store holds it too where a change set wrote it: a written node that the tree does not reach once its batch is applied is kept as an unlinked record, by the rule of `lignindb::txn` (`MerkWrite`, "Unlinked records"; node-store.md, "Unlinked records"; MERK-09). An earlier repair of an indexed tree's ranking left such records, and GroveDB #1063 removes them with `delete_node(key, Some(cost))` in a change set that states no root. Each rule of section 15.1 charges such a record as it charges a node, on the handle the rule is of:
   - A read of its key through a deferred or a read-only handle is charged by R1 with the record's length, and a cursor that walks over it by C1 to C3. The length is worked out from the parts the record holds, as it is from a cell (N1). A read through an immediate handle is charged by R2, which measures nothing.
   - Its put in a batch is charged as the put of any node (W1, W2), with the writer's cost or with none, and a put over it as a put over a record. On an immediate handle its put is charged by M1 or M2, as the put of any node there.
   - Its removal in a batch with a writer's cost is charged by D1, which measures nothing: one seek and the bytes the writer says it frees, the same whether or not the key holds a record. Its removal in a batch with no cost is charged by D2 from the committed record, which is the unlinked record where the committed state holds one. Its removal on an immediate handle is charged by M2, which measures no record.
   - The transaction takes a removal of a key that holds nothing as one write of the key (ST5) and changes no page.
   - Where the store is not RocksDB: a cell that `MerkWrite` leaves where it is with no link to it ("Only written nodes are looked at") is no such record, as the cell of a node that a change set neither puts nor removes and that loses the link to it because the node above is written without that link. It stays in its page, reached by nothing, and a read of its key finds nothing and loads nothing, where RocksDB finds the record. No commit of a Merk leaves one: it removes every node it takes out of its tree. `write_node` can (`a_node_written_outside_a_commit_leaves_the_root_where_it_is`).
7. **A restore makes other calls of the store than of a storage context, and returns no cost from either.** `Restorer::process_chunk` and `finalize` return no `OperationCost`: they drop the cost of every call they make of a store. Which calls those are differs (node-store.md, "Chunks and restore"). On an immediate storage context each chunk is a change set of puts with no cost (M2, K2 by the record length), the root key and each corrected parent are writes made directly (M1), the parent is read first (R2), and `finalize` reads the tree, writes it again in one change set where a height the chunks gave is wrong, and for a tree type with aggregates writes it again in another with the aggregates it computes. On an immediate handle of the store a restore makes no charged call before `finalize`, which moves a cursor to the first node and asks whether it is on one (C1, C2), commits one change set that puts every node once with no cost and the root key record (M2, K2), and reads the root key record and the root (R2).

### 15.5 How the store is checked

| What | Where | Holds |
| --- | --- | --- |
| A Merk on both engines | `merk/src/store/lignin/tests/cost.rs`, `a_merk_on_lignindb_costs_what_it_costs_on_rocksdb_on_every_grove_version` | On each of the five grove versions and for twelve tree types, a Merk on the store and a Merk on RocksDB are opened (as a base Merk, a layered one and one that stands alone), written by the same random batches and read, on deferred handles with a batch and on immediate handles. The reads are every read of a Merk that walks no cursor: by key in each of Merk's ways, the two reads that keep the nodes they fetch (`observe_old_value`, `observe_value_and_value_hash`), an aux and a meta record, a proof, the aggregates of a range and their proofs, the queries of a trunk and of a branch, the proofs of a page that is skipped to by count, the walk of every link (`linked_keys`), and the root hash. A read that walks the tree is charged only the fetches of the nodes no earlier read loaded, so the order of those reads changes from round to round, and the test holds that each kind of read was charged a fetch of its own, but for the root hash, which fetches nothing. On deferred handles the walk of every link is made on a Merk that was opened for it, where it costs one fetch for each node but the root and gives the keys the batches left, and again on the Merk it loaded, where it costs none. On immediate handles it is made on the Merk that stays open, and compared. The batches issue every operation of a Merk batch, which the test holds by name: ordinary values, values with a cost of their own (as sum items and trees have, hashed with one root or with two), values with a combined hash and with a provided one, values with flags, each kind of removal, and elements that move to another key as GroveDB renames one (`DeleteMoved` and `PutMoved`): to a longer key, to a shorter one, and two into each other's place. A move is made with the calls GroveDB makes for one. The element is read at the key it leaves and the key it moves to is looked at, by the reads that keep their nodes, and the batch is then applied to the Merk those reads left. On deferred handles that Merk was just opened and has loaded nothing but its root; on immediate handles it is the Merk that stays open. An element with flags moves to a key of another length here too, which GroveDB refuses and Merk bills: the bytes a shorter key frees are sectioned by the client's callback. The test counts those moves, to a longer key and to a shorter one, with the others. A client's two callbacks keep the epoch of a replaced value and section the bytes it frees. Every call, the application of every batch and every read of a meta record cost the same on both, with the removal arithmetic of the grove version. |
| Each operation of the seam | the same file, `every_operation_of_the_store_costs_what_it_costs_on_rocksdb` | One history is played on handles of both engines, and each step and each applied batch must cost and end the same. On LigninDB, in the parts that are played on handles which write (those that stage into a batch and those that write at once), each read of a root-key record is made through a handle of its own, which stages into no batch or writes nothing: the history keeps the record as any other, and where a handle that writes read a record which does not name the root of its Merk, no node of that Merk is written through it or with its batch ([node-store.md](node-store.md#opening-a-merk)). The history holds: every read, found and not; puts with no cost, with a cost that is checked, with one that is taken as given and with a prepaid one; removals with a cost, plain and sectioned, and with none; the root key, aux and meta records in the same ways; a record a batch writes twice, by two puts, a put and a delete in either order, and two deletes with different costs; a batch of two subtrees whose removals add up to another sum when they are charged in another order; a put whose cost does not match, with writes before it in the order of a batch and after it a removal that would load a record; a batch the transaction does not take; a handle with no batch; and an immediate handle, whose change set also writes a record twice (a removal and then a put, and two removals with different costs): there the later write stands and each is charged as it is given, on both engines. It is played under both removal arithmetics: under the first, the order in which a batch is charged, by keyspace and by subtree, shows in the bytes removed. |
| D2 | `a_delete_with_no_cost_is_charged_from_the_committed_record`, `the_nodes_of_a_merk_removed_with_no_cost_are_charged_as_committed` | A node the transaction rewrote is removed at the length of the committed record, and one it created as an empty record, with the numbers of rule D2 and equal to RocksDB. Every node of a Merk whose shape the transaction changed is removed with no cost, as `Merk::clear` and the removal of a whole subtree do: the bytes loaded are those of the committed records. |
| One seek for one fetch | `a_fetch_is_one_seek_and_the_record_length_however_it_is_served` | A fetch that reads three cells, one that is read again because the location of its cell ended, one that follows a link and a lookup that reads one cell each cost one seek and the record length. |
| The ledger | `the_store_reports_what_the_model_hands_out_and_nothing_else` | The same history on the store alone, under both removal arithmetics: after each part, the costs the store reported add up to what the model handed out (section 14.2). The costs are added as the ledger adds them, each to the sum before it: under the first arithmetic a section of the owner that plain bytes are counted to, followed by plain bytes, adds up to another sum by `+`, and the history has that order. |
| A conversion to a 128-bit sum | `a_conversion_to_a_big_sum_tree_costs_what_it_costs_on_rocksdb` | See below. |
| Proofs (MERK-06) | `merk/src/store/lignin/tests/proof.rs` | Every kind of proof, and the reads and writes that follow one on the same Merk, on both engines, on every grove version, twelve tree types and both kinds of handle: each call is charged the same. A proof on the store fetches its nodes without their values and keeps none in the tree, and the calls that follow it are charged as on a Merk that kept them (node-store.md, "Proofs" and its table of tests). |
| A removal of a key that is no node | `a_removal_of_a_key_that_is_no_node_is_charged_and_changes_nothing` | Item 6 of section 15.4, on both engines. Through a deferred handle: the removal with a cost and the removal with none, where the key holds a record that no node links and where it holds nothing, the one with no cost charged from the committed record where there is one; the put of such a record, and a lookup of it in the transaction that wrote it and after a commit. Through an immediate handle: both removals where the key holds nothing. One write of the key; the tree and the root in the directory as they were. |
| Records that no node links | `merk/src/store/lignin/tests/unlinked.rs` | For twelve tree types, a Merk on both engines holds such records among its nodes, committed and written by the transaction, before its first node of eight bytes and after its last: every read by key (Merk's and those of the seam), a cursor that is moved about by the numbers of a seed, scans in both directions and of ranges, the emptiness checks and the walk of every link answer and cost the same, on deferred and on immediate handles. So do, through a deferred handle, a put over such a record, its removal with a cost and with none, a node that Merk links under its key and then removes, and `Merk::clear`, which takes it with the nodes; and through an immediate handle a node that is written outside a change set, its read, and its removal with no cost. |
| N1 from sizes | `merk/src/cost_model.rs` | The length is that of the record Merk encodes, for every kind of feature and of aggregate and every key length of a link. |
| The corpus | `lignin-testkit/tests/merk_store.rs` | See below. |

**The conversion of a tree to a 128-bit sum** (`Merk::convert_to_big_sum_tree`, GroveDB #1063) reads every node of a tree and writes each again with a wider feature and wider aggregates in its links. No key, value or value hash changes. The test tries the conversion of the same Merk on both engines, on every grove version, for the twelve tree types of the store's other tests and the two indexed trees that keep a 64-bit sum: those two and the five plain trees that keep one are converted on grove version 5 (an indexed tree as its primary Merk, whose nodes are those of the tree it mirrors), and every other conversion that is tried is refused alike. On a deferred handle the tree holds nodes of the committed state and nodes its transaction wrote.

- **The charges are RocksDB's**: the fetches of the walk, the conversion itself, the batch it stages (one put for each node, charged by W2 with the cost Merk gives, which adds up to between 16 and 17 added bytes a node over the tree), the conversion through an immediate handle, and every read of the converted tree. The records read back are RocksDB's, byte for byte.
- **No value is written.** The seam has no kind for "the feature changed", so Merk reports every node as `ValueWritten` and states its value. The store compares the stated bytes with the bytes it holds and keeps them: for a tree of n nodes, n values stated, none written.
- **What the comparison costs** is one read of the transaction for each node, and with it the bytes of each value, also of one that is kept out of line. The read goes through the location the conversion's own fetch of the node left in the handle, so no key is looked up while the transaction serves its nodes from its frames, which is the case the test pins: it counts n reads through a location and none by key while the batch is applied. The count is of this placement and not of the contract: a location that a commit made stale, or a node the handle was given no location for, is looked up by key at the same charge. None of it is in the `OperationCost`, which is of sizes. It is work a kind that names the feature alone would spare (MERK-02), and PERF-03 moves the comparison to where the cell is at hand.
- **The subtree directory is told by the conversion, and that is charged nothing.** The conversion compared here is Merk's: it writes nodes, and then states the Merk anew in the directory through its handle (`NodeStore::state_tree_type`, which on this store is `LigninNodeStore::convert`; node-store.md, "The tree type" and "The batch of an operation"). Every Merk that the test converts has nodes, so the statement is made on the store's side of every conversion of the test that is not refused: staged in the batch of the conversion on a deferred handle, and after the conversion's writes on an immediate one, as a write of its own. The conversion still costs what it costs on RocksDB. The entry then names the wide tree and the root of the Merk, and `Merk::open_from_directory` opens the converted tree by it (`merk/src/store/lignin/open_tests.rs`, `a_converted_merk_opens_from_the_directory_as_the_wide_tree`).

**What is not compared.** The reads through a pinned view (`read_value_from_view`, `local_count_offset_page_from_view`) walk a cursor, which their caller gives them. Chunk production and restore return no cost; what restore writes is MERK-07's. A read-only handle opens a Merk and reads it, and refuses every write. No Merk is opened on one here: what is compared on a read-only handle is the read of every committed node, in the corpus replay.

**The corpus.** GroveDB does not run on LigninDB before GROVE-01, so the corpus is replayed on RocksDB and what each operation did to the nodes of every Merk is handed to the store (node-store.md, "How it is checked"). Since MERK-03 the same change sets also go, call for call, to a second RocksDB storage that holds nothing else, and the replay compares:

- **Every batch.** Staging a change set costs the same on both, and so does the application of the batch. Each written node is given one of the costs a writer gives, by turns: none, that of a new node and that of an update, whose value cost the storage layer checks against the record (K3), and a cost that is taken as given, with bytes freed as plain numbers and by owner and epoch. Each removed node is given a cost or none. The replay itself hands every node over with no cost; the change set it fills (`Billed`) gives the costs, the same to both engines. The second storage is committed whenever the LigninDB transaction is, so a removal with no cost meets the same committed state on both.
- **Every read.** Each node that is read back, after the write that changed it and after each commit, is read on both engines, and each read by itself costs the same: a seek and the length of RocksDB's record, which the store works out from the cell. The RocksDB of these reads is the one GroveDB wrote. A lookup of a written node that reads no value (`has_node`) costs the same again. A lookup of a removed node finds nothing and costs what it costs on the second storage, which finds nothing either.
- **A row that no node links.** The corpus holds the state of item 6 of section 15.4: on grove versions 1 to 4 the hand-written case `legacy-ranking-drift` leaves, under GroveDB, rows of a ranking whose records RocksDB keeps and no node links. The replay hands the store and the second storage what RocksDB holds: where the node of a key left its tree and RocksDB holds a record under the key still, the put of that record, which the root that is stated does not reach. It keeps every record it handed over so and hands over what a later operation does to it. The put is charged the same on both, the record is read back by its key on the store and on the RocksDB that GroveDB wrote at the same cost, and at each commit a cursor over its Merk walks the same keys and records at the same cost on both, forwards and backwards. At the end of a trace the cursor of every Merk is walked so, and at every commit the keys RocksDB holds under the prefix of each Merk are those of the tree and of the records handed over: the replay missed none.

On the standard corpus, 430 traces on grove versions 1 to 5, replayed 645 times: 334,020 batches of 5,234,105 written and 136,462 removed nodes were charged the same on both (5,438,884 seeks, 3.0 GB of stored bytes), 68,317 of the removals with no cost (9.2 MB loaded from the committed state), and 14,781,178 reads (10.8 GB loaded).

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
- **MERK-07.** Done: item 7 of section 15.4.
- **MERK-08.** Merk's own tests assert costs in many places; they are the check of the cursor's charges.
- **GROVE-02.** Propagation writes the parent's element with the cost Merk gives, through `put_node`; nothing in the store depends on which Merk a write belongs to.
- **GROVE-03.** A node that is copied to another subtree is a put with no link sizes, charged by its record length: `node_record_len_from_sizes` of the cell it is copied from.
- **GROVE-05.** Done: item 1 of section 15.4.
- **MERK-09.** Item 6 of section 15.4: once the store holds a record that no link leads to, a removal of it with no cost, and a cursor over it, have to charge its length as RocksDB does. `a_removal_of_a_key_that_is_no_node_is_charged_and_changes_nothing` has RocksDB's numbers for one such record.
- **GROVE-13.** Done: section 16. Section 15.3 holds for grove version 6 as it stands.

## 16. Stop points and finalize: the rules of grove version 6 (GROVE-13)

LigninDB task GROVE-13 ([task register 2](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583128)). Iteration two changes who carries a changed Merk into the element that hosts it: a write stops at a stop point, and finalize carries every changed Merk to the root of the grove once (GROVE-11). This section says what each of the two is charged under grove version 6, the first grove version with these rules. Sections 2 to 15 describe grove versions 1 to 5 and are not changed by it: on those versions every write still carries to the root and is charged for it.

Three things are decided here. They are stated first, and the rules follow from them.

1. **Nothing is charged by a new rule that an existing rule can charge.** The storage rules of section 5, Merk's rules of section 6 and the charges of section 7 stay as they are, on grove version 6 too. `storage_cost_model` gives `V0` for it, so the handles of both engines charge by the one version they have (sections 14.3 and 15.3). What grove version 6 changes is which operation performs which event, and so which operation is charged for it.
2. **A write is charged for what it does in the subtrees it holds, and to carry them for nothing above its stop points.** It is not charged a share of the work that finalize does later, by any formula. What it reads for a reason of its own, as the target of a reference, it is charged wherever that is (SP2). A write therefore costs less than on grove version 5 by everything above its stop points.
3. **The charge of a finalize belongs to the finalize.** The call returns it as its own `OperationCost`. No write is charged any part of it, before or after. Whether that cost is paid by anyone is for the application to decide: GroveDB reports it with the operation that did the work.

### 16.1 Terms

- **Write.** One application of GroveDB that changes the state: one operation or one batch. Every write of a transaction goes through a write scope ([transactions.md](transactions.md), SC1). Which writes share a scope is GROVE-12's to say; the rules below hold for a write whatever scope it is in.
- **Unit.** A column of a subtree, the unit of exclusion of write scopes (SC2). A Merk is one unit. A scope holds a claim on each unit it reads or writes, and a write claim is compatible with no claim of another open scope (SC3).
- **Host.** The element that names a subtree: the tree element at the subtree's path, in the Merk above it. For a Merk it names the root. The root Merk has none.
- **Stop point.** For a subtree that a write changed: the topmost subtree on its path that the same write carries the change into. By default it is the changed subtree itself (GROVE-11). A write that carries a subtree into its host itself has its stop point at the host's Merk or above: a batch does so for a tree it creates and fills, and every write does so under an indexed tree, up to the primary of the outermost one (SP5).
- **Mark.** A pending mark of the transaction on a host (SC9). A write leaves one on the host of each of its stop points. A mark is not a claim, and adding one writes no record. A mark goes when a finalize has rewritten its host, or with the host (SP6).
- **To carry** a subtree is to write what its host commits to into the host: for a Merk its root key, its root hash and its aggregate, which is what `ReplaceTreeRootKey` does in a batch today; for a tree that keeps its data flat its state root and its counts (`ReplaceNonMerkTreeRoot`); for an indexed tree the roots of its primary and of its secondaries (`ReplaceAggregateIndexedTreeRootKeys`).
- **Finalize.** The operation that carries every marked subtree, deepest first, up to the root of the grove, and removes the marks (GROVE-11). A commit refuses a transaction with a mark left (CM1), so a finalize is always a call of its own.
- **Disjoint writes.** Writes whose scopes could be open at the same time: no unit that one of them writes is read or written by another.

### 16.2 What a write is charged

**SP1. A write is charged the events it performs, by the rules of sections 5 to 7, and it carries nothing above a stop point.** Inside the subtrees it writes it is charged as on grove version 5: the prefix of each handle (P1 to P3), every fetch (R1), every hash, the batch it applies (B1, with W1 to W4, D1, D2 and K), and what the append-only trees charge. A Merk below a stop point is carried into its host by the write, and that is charged to the write as it is today. To carry what it changed, the write opens no Merk above a stop point, reads no element there, hashes nothing and writes nothing, so it is charged nothing for a level above one. What it reads for a reason of its own is charged where it is: the target of a reference is in whatever Merk it is in, above a stop point or not, and that Merk is opened as SP2 says.

**SP2. A write opens a Merk by the Merk's own path and root, whether it changes it or reads from it.** It is charged P1 for the path and one R1 for the root node, and for the root Merk the read of its root key record before that (section 6.1). A Merk with no node is charged P1 alone, and the root Merk the read of its root key record as well, which finds none. The host is not read to open the Merk, and a write is charged nothing for the host of a Merk it opens, to change it or to read from it, whether or not the code reads it: not the prefix of the host's path, not the read of the element (section 6.5). The root key and the tree type come from the Merk's entry in the subtree directory, which belongs to the Merk's own unit (SC2).

On grove versions 1 to 5 the open reads the host: P1 for the parent's path, one seek and the element's bytes. That read is of the unit above. A scope that made it would hold a read claim on the Merk above, and could not run beside a scope that writes there.

A Merk that a write opens only to read from it, as the Merk a reference's target is in, is opened the same way. The write is charged P1 for its path, the fetch of its root, the read of the target and the hash of the target as it is committed. The host is not the way to the root for a second reason there: while a host is marked it is behind, and the root is the one the directory has (SC9). So what a write is charged for a target does not follow whether the host of the target's Merk is marked. Such a read is of the unit it reads, so a write that puts a reference is disjoint from no write of the Merk its target is in. On grove versions 1 to 5 that Merk too is opened through its host.

**SP3. At a stop point the write asks the Merk for its root, and marks the host for nothing.** The hash of the root node is charged to the write (section 6.2: two hash calls for a Merk with a node, none for an empty one). The mark is free. The root Merk has no host and gets no mark: a write that changes it is not charged for its root, and writes its root key record itself where the root key changed, as today (section 6.4).

**SP4. A write begins with nothing loaded.** What an earlier write of the same transaction had loaded saves it nothing: an implementation that keeps a Merk open from one write to the next charges as if it did not. Within a write, a node that the write walks to in a Merk is fetched once. A read of an element by its key that does not walk the Merk is an event of its own, whether or not a node of that record is loaded: a write that puts a reference to the one element of another Merk is charged the fetch of that Merk's root at its open and the read of the target by its key, two seeks for one record, as on grove version 5.

**SP5. Under an indexed tree a write stops at the primary of the outermost indexed tree on its path.** The write itself carries what it changed up to that primary, level by level, and is charged for every level as on grove version 5, the rows of every indexed tree on the way among them. So the stop point of a subtree is the topmost primary of an indexed tree on its path, itself included, and the subtree itself where there is none (`PropagationCostModel::stop_point`, which is given the tree type of every subtree on the path). The host of that primary is marked as any other host is (SP3).

The reason is the rows. An indexed tree keeps a row for each entry in the secondary of each axis, bound to the entry's aggregate and hash. What changes under an entry, at whatever depth, is carried into the entry, and the entry's rows are written with it: on grove version 5 the same insert two levels under an entry costs five seeks, 206 replaced bytes and eight hash calls more where the tree at the top is indexed than where it is a count tree (`sp5_a_carry_into_an_entry_of_an_indexed_tree_writes_its_rows`). Two things follow from that.

- The secondaries are single Merks that every entry of the tree shares. Whatever carries into two entries writes the same secondary, so two writes under one indexed tree are not disjoint whoever is charged, and a write that changes anything under an indexed tree claims its secondaries.
- A row that moves is removed at its old sort key and added at its new one. A batch accounts that as a removal of the old row and an addition of the new, and rebills a lower bound of both as replaced bytes (`reclassify_indexed_mirror_rekey_churn` in `grovedb/src/batch/mod.rs`, with the bound from `build_axis_mirror_batch` in `grovedb/src/batch/indexed_tree/mirror.rs`). What is over the bound stays added or removed bytes, and those belong to a write (FZ4). No test of this section makes a row change its length.

The stop is at the outermost indexed tree and not at the nearest, because an indexed tree can stand under an entry of another, and the rows of the outer one follow what is carried into that entry as well. Leaving the rows to a finalize, so that a write could stop under an indexed tree, is another rule and not this one: a finalize would then write the shared secondaries and could add and remove bytes.

**SP6. A write that removes a host, or puts another element in its place, takes the mark on it and every mark under it, for nothing.** What was under such a host is no part of the grove any more, so no finalize has anything to carry there, and a mark left behind would name a host that is not there. The marks go with the write that removed the host, which is charged nothing for them (`PropagationCostModel::marks_under_a_removed_host`). The removal itself, with what it removes below the host, is charged to the write as on grove version 5. A clear of a Merk removes every host in it, and SP6 holds for each.

What a write is no longer charged, compared with grove version 5, follows from SP1 and SP2. For each Merk above a stop point: the prefix of its path, the read of its own host, the fetch of its root and of the nodes on the way to the host it rewrites, the hashes of those nodes, their puts, and in a batch the put of the root key record of the root Merk. And for the stop point itself, as for a Merk the write reads the target of a reference in, the read of its host.

### 16.3 What finalize is charged

**FZ1. The charge of a finalize is the cost of the finalize call.** It is returned by that call and by no other. It is not divided among the writes that left the marks, and no later write is charged for a mark an earlier one left.

A finalize is a write of the Merks that hold marked hosts all the same, and a write that comes after it reads the records it left. A host that a finalize rewrote names the root key of its Merk, where the host of a Merk that was empty named none. So where a finalize stands among the writes of a transaction is part of what the writes after it read, as the place of any other write of those Merks is: after a finalize that put a root key of 100 bytes into a host, the same write of the root Merk loads 101 bytes more, and seeks, hashes and stores the same (`fz1_a_write_after_a_finalize_reads_the_records_the_finalize_left`).

**FZ2. Finalize is charged the events it performs, by the rules of sections 5 to 7. It performs these, once for each Merk that hosts a marked element, deepest path first:**

1. It opens the Merk by SP2.
2. For each marked element, in key order, it reads the element through the Merk (one R1 where the node is not loaded, section 6.1) and rewrites it with the root key, the root hash and the aggregate of the Merk below: the fetches on the way to the key, the hashes of the new value (section 6.2), and at the commit the hashes of every changed node. All marked elements of one Merk are rewritten in one application, so a node on the way to several of them is fetched, hashed and put once.
3. It asks the Merk for its root (two hash calls) and marks the Merk's own host, which is free. That host is rewritten when its level is reached. The root Merk has no host: its root is not asked for, and its root key record is not written, because a finalize inserts and removes nothing and so leaves the root key of every Merk where it was.

The batch of a finalize is applied once, at the end (B1). So an element that hosts a Merk which k writes changed is rewritten once and charged once, and so is every node above it.

**FZ3. Finalize is charged nothing to learn the root of a marked Merk.** The root key, the root hash and the aggregate are those the last write of the Merk left at its stop point, and SP3 charged that write for them. Finalize opens no Merk to learn its root. A marked Merk that itself hosts a marked element is opened as every Merk that hosts one is, to rewrite that element (FZ2).

**FZ4. Finalize adds no bytes and removes none, and calls no callback of a client.** The value cost of a tree element is fixed by its tree type, its wrapper and its flags (section 6.3, the layered cost), and a finalize changes none of them. Every put of a finalize is therefore an update with the old cost equal to the new one: its storage cost is `replaced_bytes` alone. The flags of a host stay as they are, and no bytes are sectioned by owner and epoch (section 6.6), because a finalize is no client's write. So every added byte and every removed byte of a transaction is charged to a write, with that write's flags. SP5 is what keeps this true under an indexed tree, and SP6 where a host is gone.

### 16.4 Why no charge depends on the order in which threads ran

**OI1. The cost of a write is a function of the write and of the units it claims. The cost of a finalize is a function of the marks and of the state when it runs.**

For a write:

- By SP1 and SP2 every event of a write is inside a unit its scope claims, or is a read of the committed state (D2, which no write of the transaction changes). SP2 is what makes this true for the open: without it a write would read its host, which is in a unit that a sibling's host shares.
- A scope reads its own writes, then the transaction's merged writes, then the committed state, and never the writes of a scope that is still open (SC4). Two scopes that are open at the same time have written nothing the other read or wrote (SC3). So what a write reads is the same whether a disjoint write ran before it, beside it or after it.
- A mark is not a claim, changes no record and is free (SP3). Whether the host was already marked by another write changes nothing the write is charged.
- By SP4 nothing is charged less because an earlier write on the same thread had loaded it.
- The batch of a write is its own, and B1 charges it in the order of its stored keys, which is no order of time.

For a finalize: the rules take it to run with no write scope open, as a savepoint and a commit do (SC8). The set of marks is a set, the same for every order in which disjoint writes merged. The state it reads is the same for every such order (SC7). It handles the marked Merks in a fixed order, by the length of the path and then by the path, and the marked elements of a Merk in key order. By FZ4 its storage cost has no removed bytes, so the order in which its puts meet the running cost cannot show (section 5.3, B1).

So for disjoint writes with no finalize between them, each write's cost and the cost of the finalize that follows are the same in every order, and with them the total. The total is the sum of those costs as the grove version adds costs. Where one write removes bytes as a plain number and another by owner, the sum of the two is made by the arithmetic the grove version selects (`storage_costs.add_basic_storage_removal_to_sectioned_storage_removal`), which is a sum from grove version 4. A caller that adds the costs of its writes outside an operation selects that arithmetic itself (`use_basic_sectioned_removal_addition_version`): by the first arithmetic, which is what the costs crate adds by where none is selected, the two orders of such a pair give different removed bytes (`oi1_the_total_is_added_by_the_arithmetic_of_the_grove_version`).

What is not claimed: that two writes of the same Merk cost the same in either order. They are not disjoint. Their scopes cannot be open together, and the order in which they run is the order the application gives them. Nor that a write costs the same before a finalize and after it (FZ1). A finalize runs with no scope open, so it is before or after each write and beside none: its place among the writes is the application's to give, as the order of two writes of one Merk is, and no order of threads.

**What the rules add no state for.** The rules keep nothing of their own between a write and a finalize: the marks are the transaction's (SC9), and what a write left at its stop point is the Merk's entry in the subtree directory and its root. So what becomes of them when a scope is rolled back, a savepoint is rolled back to or a transaction is given up is what the transaction rules say (SC6, SC9), and the charges follow:

- A write that fails returns the cost of what it did, as today. Its marks go with its scope, so a finalize after it is charged nothing for it.
- A finalize with no mark performs no event and charges nothing. A finalize that fails returns the cost of the events it made.
- A finalize in the middle of a transaction and another at its end are two calls with a charge each. A host that is marked again after the first is rewritten again by the second.
- A write that removes a host or puts another element over it takes the marks at it and under it (SP6). For a move of a host the rules add nothing of their own: a mark that is taken off or put on is free (SP3, SP6), and a carry that a write makes is that write's (SP1). Whether a subtree is moved while a mark stands under it is for the transaction rules and GROVE-11 to say. [transactions.md](transactions.md) has a refusal there: a scope whose writes are laid out refuses a batch with a move, with `MoveBeside`, while a pending mark stands under a prefix that the move names.

**What no rule leans on.** No charge of this section is taken from what an engine read to serve an event: every one is an event of sections 5 to 7, charged from sizes. And no rule reads under the prefix of a tree whose host is gone. A write opens a Merk that a host names, and a finalize opens the Merks that hold marked hosts. After a tree that still held elements is removed by a batch's plain delete, RocksDB can still find records under its prefix and LigninDB cannot, so a raw probe there answers and costs differently on the two. No trace of the corpus of grove versions 1 to 5 reaches that, and nothing here adds a probe that would.

### 16.5 Estimates

Estimates are replay-critical: an application admits a write against its estimate. So each estimate that the new rules change is a new version of its function, selected by a slot of grove version 6, and grove versions 1 to 5 estimate what they did. The files of the versions they select are as they were, with exceptions that change nothing they give: the bodies of the estimate of an open and of the estimate of a patch, which had no versions, moved into files of their own; and the worst-case estimates that still have none (of a removal, and of a batch's own carries) call the worst case of a level by grove version, as the average-case ones already call theirs.

**ES1. The estimate of the open of a Merk follows SP2.** It no longer has the seek and the loaded bytes of the read of the host: it is the seek of the root's fetch, for a Merk that is not expected to be empty, and P1. The root Merk has no host and is opened by its root key record: its estimate is the read of that record and the fetch of the root, as before, and the bytes of the record, which the estimate of grove versions 1 to 5 does not have. They are the root key: at the length the caller gives (`add_average_case_get_merk_at_path_with_root_key`), and where it gives none, and in the worst case, the 256 bytes the worst case takes a key to be (`MERK_BIGGEST_KEY_SIZE`, a byte more than a key can have). A root Merk that is expected to be empty has no root key in its record (`es1_the_open_of_the_root_merk_has_the_bytes_of_its_root_key_record`).

**ES2. The estimate of a write is its own level, and holds for that level by itself.** It has the open (ES1), each operation, and the nodes on the way to the root of the Merk. It has no level above a stop point. Nothing is added for the stop itself: the root's hash is among the hashes of the level, and the mark is free. A write whose stop point is above the Merk it changes, as under an indexed tree (SP5), has every level up to its stop point, each estimated as a batch estimates a level above today. Which levels those are is for the batch's estimate to say (section 16.9, 7), and no test of this section estimates such a write. Where one of these refuses, nothing of it is added: Merk's two estimates of a level at their versions of grove version 6 add to the cost they are given in full or not at all, and the estimates of the insert, the replacement and the patch of an element at theirs return no cost where they refuse. The estimate of an open adds its seeks to the cost it is given as on every grove version, by a sum that is not checked; the bytes of the root key record it adds in full, or refuses with nothing added.

On grove versions 1 to 5 a batch's estimate adds the levels above to this, up to the root of the grove. A write that stops has this level alone, and four parts of it are below what the apply charges. Grove version 6 selects a version of each that is not:

| Estimate | Grove versions 1 to 5 | Grove version 6 |
| --- | --- | --- |
| `add_average_case_merk_propagate` | one seek for each node it rewrites, which is its put: a node for each level, and beyond one level a node beside the way, which a rotation rewrites. The levels of a layer that is given by its count are a logarithm in floating point, which is one too few for a count that is a power of two from 2^21 on. The bytes of a node times the nodes, multiplied in 32 bits and by a height that is not checked | v3: also the fetch of each node it rewrites but the root, which the open fetched: a seek for each level below the first, and one for the node beside the way. The levels of a count in whole numbers (`EstimatedLayerCount::fewest_levels`). For a layer of one kind of element the bytes of one node are taken from v2 and multiplied by the nodes in the width of each field. A layer of several kinds is sized by v2 as it is, which adds up in 64 bits. A height no Merk can have (more than 91 levels), a value and flags of one element that together come within 65,536 bytes of what 32 bits hold (`EstimatedLayerSizes::check_sizes`), and a product that does not fit its field are refused |
| `EstimatedSumTrees::estimated_size`, the bytes the aggregates of a tree add to its element | the feature of a node of the tree's own Merk: 8 bytes for a count or a sum | v3: what the element's cost has (`TreeType::cost_size`): 9 bytes for a count or a sum |
| `EstimatedLayerSizes::value_with_feature_and_flags_size` and `subtree_with_feature_and_flags_size`, the bytes of a value and of a tree of a layer | the aggregates of a tree, its flags and the bytes of the kind, added in 32 bits unchecked | v2 and v1: the same, behind `check_sizes`. The aggregates are a byte longer here, so a layer the versions before could just count is refused, with nothing added |
| the worst-case propagation | `add_worst_case_merk_propagate`: the fewest levels a Merk of the given size can have, the nodes on the way and one or two beside it, one seek for each | `add_worst_case_merk_propagate_versioned` v1: the most levels an AVL tree of that size can have; five nodes for each level and one more; the fetch and the put of each node |
| the hash calls of the put of an element: `average_case_merk_insert_element`, `average_case_merk_replace_element`, `average_case_merk_patch_element`, and the worst-case three | the blocks of a length that is the element's cost, without the length a value is hashed with; in the worst case no hash to combine a tree with its root for the kinds of tree it prices as items; no hash for the secondaries of an indexed tree, and none to combine a reference with its target | v3, v2 and v1 of the three, in both cases: the version before, with its hash calls set to `element_put_hash_calls`, which counts them from the bytes of the element. The patch is also sized from the bytes of the item as it is serialized, which have its flags, where the version before adds the flags to them once more |

With the version of grove version 5 in the place of any of these, tests of ES2 and ES3 in section 16.7 fail (section 16.7, "Planted faults"). Whether the levels above make up for these on grove versions 1 to 5 was not examined: those versions keep the estimates they were admitted with.

Five nodes a level in the worst case are from how a Merk rebalances (`Walker::maybe_balance`, `rotate`): at a level the write changes the node on its way, and a double rotation there detaches and attaches four more, the child on the taller side, that child's child and both children of that one. An insertion rebalances at one level. A removal can rebalance at every level on its way back up, and the one more is the other child of a removed node. The removal of the first key of the leanest AVL tree of nine levels, 88 nodes, seeks 80 times. Grove version 6 has 94 seeks for it and grove version 5 has 12 (`es2_the_worst_case_of_a_level_holds_for_a_removal_that_rebalances_at_every_level`).

The hash calls of the put of an element are these (`element_put_hash_calls`):

- the hash of the value, which is the bytes of the element with their length before them: a value that ends within its length of the end of a block takes one call more than its bytes alone, as one of 64 bytes does, of 128, of 191 and of 192. An element that keeps a list of its referrers is hashed without the list;
- for a tree the hash that combines it with the root of what it holds; for an indexed tree one more, which combines the root of its primary with what its secondaries give, and where the tree names its axes the digest of their roots, one byte and 33 for each, an element that names more than the three a tree can being refused; for a private document store the two of its empty root;
- for a reference the hash that combines it with the hash of its target;
- the hash of the key with the value's hash, and the two of the node.

They are counted from the element that is put, so the estimate of an insert has the hash calls of the write for an element of any length and for a tree of every tree type. A replacement and a patch count the same for the element they put.

**Where ES2 stops.** The average case is an average over the Merks that fit what a layer says, and no bound. A write costs more than it where the write rewrites more nodes than the estimate has, by a deeper Merk or by a rebalance, or where a node it rewrites is longer than the layer's sizes make it. The worst case is the bound, and is held for each write below. Each of these is the estimate of every grove version and is not changed here. A test pins each but the last:

- **The average case is an average.** It is held to be no less than the write for the insert of a new last key, for a replacement by an element of the size that was there, and for the removal of the first, the middle and the last element, in Merks of 1, 7 and 30 elements that were filled in the order of their keys. Those Merks are as shallow as their counts allow, and a write of them turns the tree once at the most.
- **The average case has the nodes of one rotation.** The estimate of a level has one node beside the way of a write, with its fetch, which is what one rotation rewrites. Merk makes a double rotation of rotations that each rebalance what they leave, and those fetch and rewrite more nodes beside the way: the insert of a twelfth item into a Merk of eleven in four levels rewrites three, and seeks 15 times where the estimate, given the four levels, has 11, with 889 replaced bytes against 635 and 19 hash calls against 15 (`es2_the_average_case_has_the_nodes_of_one_rotation`). A removal can rebalance at every level. For both the worst case is held, with five nodes for each level (`es2_the_worst_case_of_a_level_holds_for_a_removal_that_rebalances_at_every_level`), and the average case is not claimed. On grove version 5 the levels above a write hide this in a batch's estimate, as they hide a deeper Merk. Whether the average case of grove version 6 should have more nodes beside the way is the same choice as the one of its levels, and is not made here.
- **The average case takes a Merk that is given by its count to be as shallow as that count allows**: 54 elements in six levels. The leanest AVL tree of 54 has eight, and a write under its deepest node seeks 17 times where the estimate has 15, as a finalize of a host of seven trees in four levels seeks twelve times where the estimate has ten. A caller that knows the levels gives them (`EstimatedLevel`), and for those two Merks the estimate then holds; the worst case takes the most levels a count allows (`es2_the_average_case_takes_a_merk_to_be_as_shallow_as_its_count_allows`). On grove version 5 the levels above a write hide this in a batch's estimate. Whether the average case of grove version 6 should take more levels than the fewest is a choice for whoever switches it on, and is not made here.
- **On grove versions 1 to 5 the estimate of a patch counts the flags of the item twice.** It adds them to the bytes of the element, which have them. Where that carries the length of the record over 63 bytes or back, the added bytes are off: an item with one flag byte that grows from 62 bytes to 63 adds two, and the estimate of grove versions 1 to 5 has one. Grove version 6 sizes the patch from the bytes of the item, in the average case and in the worst, and has the two (`es2_a_patch_is_sized_from_the_bytes_of_the_item`). The versions before take the bytes the item grew by from their size in unsigned arithmetic, unchecked: on grove version 6 `average_case_merk_patch_element` and `worst_case_merk_patch_element` refuse a growth of more bytes than the item has (`es2_a_patch_by_more_bytes_than_the_item_has_is_refused`). A batch's worst case does not give its estimate the growth, and estimates such a patch on every version. The growth an estimate is given is that of the element's bytes, not of what the item holds, on every grove version: an item that grows from 250 bytes to 251 has its length in three bytes where it had one, and a caller that says it grew by one byte is estimated one added byte where three are added (`es2_a_patch_is_estimated_by_the_growth_of_the_element_as_it_is_serialized`).
- **A replacement is estimated as one by an element of the same size.** `average_case_merk_replace_element` has no added bytes for a longer element. The worst case estimates the replacement of an item as one that may grow. Neither estimates that a sum item grows: its sum is of a fixed length and its flags are not, and a sum item whose flags grow by 16 bytes adds 17 that no estimate of its replacement has (`es2_a_sum_item_whose_flags_grow_adds_bytes_that_no_estimate_of_its_replacement_has`). Nor that a tree is put where an item was: a tree with 128 bytes of flags over an item with no bytes adds 99, and both estimates of the replacement have none (`es2_a_tree_put_where_an_item_was_adds_bytes_that_no_estimate_of_its_replacement_has`).
- **An estimate of a removal has no removed bytes**, in the average case or in the worst. What a removal frees is the caller's to say. The estimate of the removal of a tree (`average_case_merk_delete_tree`) has one version for every grove version and adds the flags of its layer in 32 bits unchecked on each; the estimates grove version 6 selects refuse such sizes (`es2_no_size_of_a_layer_overflows_an_estimate_of_an_operation_on_grove_version_6`).
- **The worst case sizes a value at 65,535 bytes and flags at 16,386** (`MERK_BIGGEST_VALUE_SIZE`, `WORST_CASE_FLAGS_LEN`). Those are no limits of GroveDB. Beside a value of 500,000 bytes in the same Merk a write replaces more bytes than the worst case of its level has (`es2_the_worst_case_takes_a_value_to_be_no_longer_than_its_biggest`). The worst case of a finalize sizes the bytes its nodes load and replace by the first, and is told by its caller how long a host can be, for the hash of a carried element (ES3). A bound of the bytes needs their sizes from the caller too, which no worst-case estimate is given.
- **The put of the root key record of the root Merk is no part of the estimate of a level.** A batch's estimate adds that seek where it estimates the root Merk (section 16.9, 7), and `es2_a_write_of_the_root_merk_is_estimated_with_the_put_of_its_root_key_record` adds it as a batch does. The estimate of a level also has the two hash calls of the root, which a write of the root Merk is not charged (SP3): two over.
- **The reads of the target of a reference are no part of the estimate of a level.** A write that puts a reference reads its target for the hash it commits to, and is charged for that (SP2): for a target one hop away in another Merk the prefix of the target's path (P1, hash calls that grow with the path), the fetch of the root of its Merk and the read of the target, and the hash of the target as it is committed: its bytes, without the list of its referrers where it keeps one. The estimate of one operation has the hash call that combines the two hashes and nothing of the read (`es2_a_reference_is_estimated_without_the_reads_of_its_target`).
- **The estimates a batch makes of its own carries are those of grove version 5**: `average_case_merk_insert_tree`, `average_case_merk_replace_tree`, and the worst-case two. A finalize has an estimate of its own (ES3). A write that carries below its stop point is for the batch's estimate to say (section 16.9, 7).

**ES3. The estimate of a finalize is the sum, over every Merk that hosts a marked element, of:** the open (ES1); for each marked element the replacement of a tree element, as a batch estimates `ReplaceTreeRootKey`, with the byte of its wrapper where it has one; and the nodes above the marked elements. It is given the stop points and a layer for every Merk, and two things a layer does not say: in the average case the Merks, among the stop points and above them, whose element is wrapped, and in the worst case the most bytes an element that hosts a marked Merk serializes to. The marked elements are taken to be at the last level of the Merk, where their ways are longest. A Merk of `L` levels with `m` marked elements then has at most `sum over i from 0 to L-2 of min(m, 2^i)` nodes above them (`nodes_above_marked`), which is the count the estimate uses. The marks are derived from the stop points: the host of each, and then the host of each Merk that hosts one, up to the root. The sum is made in full or the estimate is refused, with nothing added, in GroveDB's estimate and in Merk's, which adds to the cost it is given: a cost adds up to the most its fields hold and stops there, and a sum that stopped is no count.

The average case has, for each node above a marked element, its fetch, its put with the layer's average value cost as replaced bytes, and its two hash calls; and for each marked element below the root its read by key and its fetch on the way down. The fetch of the root is the open's. The root Merk has no host and is opened by its root key record: the open has its seek and its bytes (ES1), and the estimate of a finalize gives it the root key at the size the layer gives the keys of the root Merk, where the worst case takes the 256 bytes it takes a key to be (`es3_the_root_merk_is_opened_by_its_root_key_record`). A node's bytes are sized as the Merk's propagation estimate sizes one node, by its cost: its key under its prefix, its value at its cost, and one link. That is 111 bytes for a tree under a key of two bytes (`bytes_of_a_tree_node` in the tests has the sum). The record a fetch loads is something else: it has no key, it has the element as it is serialized, which for a tree names the root key of the Merk below, it has the links to the node's own children, and the varints of the aggregates around it. So the loaded bytes are an estimate in every shape, as they are in the estimate of a level on every grove version: over for a tree whose root key is short, and short for one whose root key is longer than its own key with a prefix and a link (`es3_the_bytes_a_node_loads_are_sized_by_its_cost_and_not_by_its_record`).

The hash of a marked element is counted from what the element is when it is carried. Its bytes are the byte of its wrapper, where the estimate is told that it is wrapped; its cost size, and one more for a tree with a sum of 128 bits, which is 17 bytes at its longest where its cost has 16; its flags, with their length as an element is serialized, which is one byte below 251 bytes, three to 65,535 and five beyond; the root key of its Merk, taken at the size of the keys of that Merk, which for a layer of several kinds is the longest of the kinds it gives a weight, with its length likewise; and for an indexed tree the root key of each secondary, which is the key of an entry behind the sort key of the axis (8 bytes for a count or a sum, 16 for an average or a big sum), with the entry's key taken at the size the layer gives its keys. The whole is hashed with its length before it. A tree of one Merk combines the hash of its value with its root, 64 bytes in one block. An indexed tree combines the hash of its value with the root of its primary and a third hash, 96 bytes in two blocks, which is one call more. The third is the root of its secondary, and for a tree that names its axes the digest of their roots, which is hashed from one byte and 33 for each axis: one call more for one axis and two for two or three. The estimate of a finalize takes such a tree to name three. A tree that names its axes is taken to name the three it may. A Merk that its layer estimates to be empty is taken to name no root, and a tree that keeps its data flat names none whatever its layer says (`es3_a_tree_that_keeps_its_data_flat_names_no_root_key`).

The worst case charges each changed node a fetch, a put and its two hash calls at the biggest record, and each marked element its read by key and the hashes of its carry: the hash of an element of as many bytes as the caller gives with their length, two calls to combine it with the roots below it and two for the digest of the roots of several axes, and the hash of its key with it. No rule of GroveDB bounds the flags of a tree, so the caller says how long a host can be, and one with no bound of its own gives the 65,535 bytes the worst case takes every value to be: 32,768 bytes of flags are 513 hash calls, which is more than a host of 17,486 bytes has, the flags of `WORST_CASE_FLAGS_LEN` with four root keys (`es3_the_worst_case_hashes_a_host_at_the_size_its_caller_gives`). It takes the levels of a Merk given by its number of elements as the most an AVL tree of that size can have.

**Where ES3 stops.** The estimate is given the stop points, a layer for every Merk and the Merks whose element is wrapped, and they say less than the grove holds. Its average case is an average as that of a write is: a finalize costs more than it where the Merk is deeper than the layer says, or where a node on the way is longer than the layer's sizes make it. Each of these is pinned by a test of section 16.7:

- **The estimate is of the marks that stand when the finalize begins.** Its stop points are the subtrees whose hosts are marked then: those of the writes since the last finalize, less those whose mark a later write took with its host (SP6). Its layers describe the Merks as they are then. Given a stop point that was given up, it estimates a carry that no finalize makes.
- **Each root key is taken at the size the layer gives its keys.** A layer gives one size for the keys of a Merk, and a root key is one of them. Where the keys differ in length and the root is a longer one, the hash of the carried element is short by the blocks of the difference: two for a root key of 199 bytes in a layer whose keys are 67. The root of a secondary is the row of whichever entry is the root there, which need not be the entry that is the root of the primary: with a primary root key of 67 bytes, as the layer says, and a secondary whose root is the row of an entry of 133, one (`es3_the_root_key_is_taken_at_the_size_the_layer_gives_its_keys`). The worst case takes each root key at the longest a key can be.
- **Where every root key is of the size the layer gives, the hash of a carried indexed tree is no less than is applied, and is over for a tree that names fewer axes than three.** The estimate takes the aggregates of the tree at their longest, which is a block over at some lengths. It takes a tree that names its axes to name three: one that names two is estimated with the root key of the third, up to five calls over, and one that names one with the root keys of the other two and a longer digest, up to ten (`es3_the_hash_of_a_carried_indexed_tree_has_the_root_keys_of_its_secondaries`).
- **A node is sized with one link, and loaded with the links to its children.** A leaf has none and a node with two children two, so on a way down a full Merk every record but the last is longer than the estimate has it. In a host of seven trees the estimate is over all the same, because it takes each marked element to be read twice at that size. In a full host of 31 trees with the leftmost marked the finalize loads 801 bytes and the estimate has 785, with the seeks, the replaced bytes and the hash calls of the finalize (`es3_a_node_is_loaded_with_the_links_to_its_children_and_sized_with_one`). The estimate of a level sizes a node so on grove versions 3 to 5 as well. The estimate of a write has the node beside its way to spare, which a write that turns nothing does not load: in full Merks of five, six and seven levels it loads no less than the write, and a level more is 166 bytes loaded and 162 estimated (`es2_the_loaded_bytes_of_a_write_hold_in_a_full_merk`).
- **A layer has no word for a tree that keeps its data flat, or for an indexed tree.** It describes the trees it holds by what their aggregates take (`EstimatedSumTrees`). The marked element itself is sized by its own tree type, which the estimate has from the layer of the Merk below it. A node above a marked element is sized by the layer alone: where it is such a tree it is sized as a tree with no aggregate, short by what the element of such a tree costs over that.
- **A layer does not say that a tree it holds is wrapped, so the estimate is told.** A wrapper is one byte of the element: a replaced byte, and a hash call where that byte carries the element with its length over a block. The estimate has both for the Merks its caller names as wrapped: a tree under each of the three wrappers, at a stop point and above one, is estimated to its replaced bytes and its hash calls (`es3_a_wrapped_host_is_estimated_with_the_byte_of_its_wrapper`), and an indexed tree by its sum with a root key of 20 bytes, a secondary root key of 28 and the longest sum, which is 64 bytes with its length, is carried in ten hash calls and in eleven wrapped, as estimated (`es3_a_wrapped_indexed_tree_is_hashed_with_the_byte_of_its_wrapper`). For a wrapped Merk its caller leaves out the estimate is short by that byte, and by that call. A caller that does not know which are wrapped names every one, and is a byte over for those that are not. A node above a marked element is sized by the layer alone: where it is a wrapped tree it is a byte short, as in the estimate of a level on every grove version.
- **A node above a marked element is sized at the average of the kinds of its layer.** A layer of several kinds of tree gives their weights, and the aggregates of a node are sized at the average those give. Where a node on the way is of a costlier kind than the average, the replaced bytes are short: in a host of seven trees whose root is one with a sum of 128 bits, the other six plain, a finalize replaces 322 bytes and the estimate has 310, with its seeks and its hash calls (`es3_a_node_above_a_marked_element_is_sized_at_the_average_of_the_kinds_of_its_layer`). The estimate of a level sizes a node so on every grove version.
- **A layer of several kinds sizes the flags of a tree without the byte of their length.** The bytes of a node above a marked element are those the estimate of a level gives a node, and for a `Mix` layer that estimate is the one of grove versions 3 to 5: a byte short for each such node whose tree has flags (`es3_a_layer_of_several_kinds_sizes_the_flags_of_a_tree_without_their_length`).
- **The estimate follows the stop points and not their order.** A stop point named twice is one mark. Two keys of the same bytes, one known and one described by its longest length, are two marks, as the caller's map of layers has two layers for them.
- **A stop point `d` levels deep has `d` marks**, one in every Merk above it, and the estimate asks for the layer of each by its whole path, as a batch's estimate does: `d (d - 1) / 2` path segments in all. It refuses at the first Merk it is given no layer for.

### 16.6 The rules as code

| Rule | Where | What it is |
| --- | --- | --- |
| the version of the rules | `merk_versions.cost_model.propagation`; `grovedb_merk::cost_model::propagation_cost_model(grove_version)` | 0 on grove versions 1 to 5 (`PropagationCostModel::V0`), 1 on grove version 6 (`V1`). What each version answers is in `merk/src/cost_model/v0.rs` and `v1.rs` |
| SP1, FZ1 | `PropagationCostModel::carrier()` | `Carrier::Write` or `Carrier::Finalize`: the operation that carries a subtree above a stop point and is charged for it |
| SP2 | `PropagationCostModel::host_read_at_open(read)` | what a write is charged of the read of the host of a Merk it opens: all of it on `V0`, nothing on `V1` |
| SP3 | `PropagationCostModel::mark()` | the charge of a mark: nothing. `V0` has no mark and refuses |
| SP4 | `cost_model::fetch_as_not_loaded(node)` | the fetch of a node that an earlier write left loaded: R1 at its record length |
| SP5 | `PropagationCostModel::stop_point(merks_on_the_path)` | the place, among the subtrees on the path of a changed subtree, of the one the write carries into and stops at: the root Merk on `V0`; on `V1` the topmost primary of an indexed tree, or the changed subtree |
| SP6 | `PropagationCostModel::marks_under_a_removed_host()` | the charge of taking the marks at and under a removed host away: nothing. `V0` has no mark and refuses |
| FZ1, FZ2, FZ4 | `PropagationCostModel::finalize()`, `FinalizeCharge::event(cost)`, `finish()` | the charge of one finalize as its events add up. An event that adds or removes bytes is refused, and nothing of it is added. An event that removes none, however it says so, leaves no kind of removal in the charge |
| FZ3 | no function | a finalize makes no event to learn a marked root, so there is nothing to add |
| ES1 | `GroveDb::add_average_case_get_merk_at_path`, `add_worst_case_get_merk_at_path` (`grovedb/src/estimated_costs/*/get_merk_at_path/`); `add_average_case_get_merk_at_path_with_root_key`, for a caller that knows how long the root key of the root Merk is | v0 on grove versions 1 to 5, v1 on grove version 6 |
| ES2 | the functions of the table of section 16.5; `checked_levels`, `most_levels`, `EstimatedLayerCount::fewest_levels`, `EstimatedLayerSizes::check_sizes`; `element_put_hash_calls` in `grovedb/src/estimated_costs/mod.rs` | selected by `add_average_case_merk_propagate` 3, `sum_tree_estimated_size` 3, `value_with_feature_and_flags_size` 2, `subtree_with_feature_and_flags_size` 1, `worst_case_costs.add_worst_case_merk_propagate` 1, and under `operations.average_case` and `operations.worst_case` the slots of the insert (2), the replacement (1) and the patch (1) of an element |
| ES3 | `GroveDb::average_case_finalize`, `worst_case_finalize`; Merk's `add_average_case_merk_finalize`, `add_worst_case_merk_finalize`, `nodes_above_marked`, `most_levels`, `checked_levels` | refuse a grove version whose writes carry to the root, a height no Merk can have, sizes an estimate cannot count, and a sum that does not fit its field (`add_in_full`, `marked_count`). Their stop points are those whose marks stand when the finalize begins. The average case is told the Merks whose element is wrapped, and the worst case the most bytes an element that hosts a marked Merk serializes to |

Where a rule charges nothing it is a function all the same, as in section 14.1, so that a later version can charge it. `GroveDb::average_case_finalize` and `worst_case_finalize` are visible in the crate and no further: no version in force has a finalize to estimate, and a public operation of GroveDB has a place in the test kit's list of them (`lignin-testkit/src/workload/surface.rs`), which is no file of this task.

**Grove version 6 is defined and not listed.** `GROVE_V6` (`grovedb-version/src/version/v6.rs`) is grove version 5 with the slots above. It is not in `GROVE_VERSIONS`, so `GroveVersion::latest()` is grove version 5 still, and nothing that names no version runs on it. The slots decide what is charged and estimated. What a write does is decided by the code that applies it, which carries every write to the root on every version until GROVE-11. Listed before then, grove version 6 would estimate a write as one that stops and apply it as one that does not. GROVE-11 is what lists it, in the change in which a write first stops (section 16.9, 1).

### 16.7 How the rules are held

GROVE-11 builds stop points and finalize. Until then no code of GroveDB applies a write that stops, so the rules are held to numbers on a model: `grovedb/src/estimated_costs/stop_point_tests/model.rs`. It is test code, made of the calls GroveDB makes of Merk today and of nothing else that charges.

- **A write** changes one key of a Merk that keeps its elements in it, and is GroveDB's direct insert or direct delete without its walk to the root: the Merk is opened by its path and root (SP2), the old value is read and the element put by `add_element_to_cached_merk`, or removed by `Element::delete`, the Merk is asked for its root (SP3), and the write's own batch is applied. Three tests hold it to GroveDB (the two of SP1 that are named for it, and the one of SP2 for a reference): a put and a removal, each with its finalize and the reads of the hosts, cost what grove version 5 charges the same insert and the same delete, and leave the same root hash. A removal whose bytes a client gives to an owner is made as GroveDB makes it: the client is asked for the owner of an element by its flags, and the bytes of an element that has none are a plain number. The model refuses, before it stages anything, every write that GroveDB makes by more than that put or that removal: a write of two keys; a write whose stop point is not the Merk it changes, and a write of the primary of an indexed tree (SP5); a write into a tree that keeps its data flat; the removal of an element that is not there, which fails; a put of a tree that names a root, which GroveDB holds to the Merk under it; a put of an element that keeps a list of its referrers, or over one, which GroveDB follows into other Merks; the removal of a tree, and a put over one. A write that puts a reference reads its target as GroveDB's insert does, through the host of the target's Merk, and the model takes the read of that host out of what it charges (SP2). It refuses a reference whose target is under a marked host, where it has no way to the root but the host, one whose target is in the Merk it writes, one whose target is a reference, and a reference of any kind but the plain one.
- **The removal of a host** is a fixture and no write (`host_removed_by_a_write`). SP6 is of the marks at a removed host and under it, and the fixture answers what a write is charged for those: it takes the element of the host out, or puts an item over it, drops the marks, and returns the charge of the marks alone. What else such a write is charged the model does not have: GroveDB's removal of a tree opens it, frees what it holds and follows the referrers of what it held. The fixture refuses a tree that holds an element with a list of referrers, where the grove it left would not be GroveDB's.
- **A finalize** is the walk a batch makes for `ReplaceTreeRootKey`: for each Merk that hosts a marked element, deepest first, one open, the elements rewritten in one application, and one batch at the end. An element is rewritten by the function a batch rewrites it with: `update_tree_item_preserve_flag_into_batch_operations`, for an indexed tree `update_indexed_tree_item_preserve_flag_into_batch_operations`, and for a tree that keeps its data flat `insert_subtree_into_batch_operations`, on the element made again from the counts its write left, as a batch makes it (`ReplaceNonMerkTreeRoot`). Its cost is added up in a `FinalizeCharge`. Each of the three is held to the hash the host commits to: a tree of one Merk by the root hash of the grove, which is that of grove version 5, and the other two by the hash of the host's value.
- **The directory and the marks** are maps of the model: what the subtree directory and the pending marks of a transaction are in the engine. Reading them charges nothing. Whether a key holds a tree, which SP6 asks before a write, is answered from them and from the elements the model put. Where the transaction has not written a Merk, the model takes its entry from the host, by a read of its own that is charged to nobody: it does so for every Merk on the path of a write, to find the stop point. The tests see costs and not claims, so they do not see those reads (section 16.9, 8).

What ties the model to GroveDB is the first row below: for a write and the finalize after it, the model's costs add up to exactly what grove version 5 charges for the same insert, less the reads of hosts, and the root hash is the same.

Each rule has tests named after it. In `grovedb/src/estimated_costs/stop_point_tests/` (`mod.rs`, and `estimates.rs` for ES1 to ES3), `merk/src/cost_model.rs`, `merk/src/estimated_costs/` and `grovedb-version/src/tests.rs`:

| Rule | Test | Holds |
| --- | --- | --- |
| SP1 | `sp1_a_write_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts`; `sp1_a_removal_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts` | For the twelve kinds of tree that keep their elements in one Merk, one to three levels deep, into a Merk with no node and into one with nodes, for a new key and for a longer and a shorter value under a key that is there, for an element with flags under a new key and under one that is there, and for a new tree of the kind: the write, its finalize and the reads of the hosts on the way add up to the cost of the same insert on grove version 5, in every field, and both groves have the same root hash. 180 cases. The same for the removal of an element of Merks of 3, 4 and 10 elements, the last of them one with flags (the first, one in the middle, the last without flags, and the one with flags), against the same delete on grove version 5, and for the removal of the first and of the one with flags by a client that gives the bytes to an owner, against `delete_with_sectional_storage_function`: 216 cases. The bytes go to the owner for the element with flags and are a plain number for the other, in both. |
| SP1 | `sp1_a_write_is_charged_nothing_above_its_stop_point`; `sp1_two_removals_of_one_merk_free_what_grove_version_5_frees`; `sp1_the_model_changes_one_key_in_a_write`; `sp1_the_model_writes_no_tree_that_keeps_its_data_flat`; `sp1_the_model_keeps_no_list_of_referrers`; `sp1_the_model_removes_an_element_that_is_there`; `sp1_the_model_puts_no_tree_that_names_a_root`; `sp1_a_write_carries_to_the_root_on_grove_versions_1_to_5_and_stops_on_6` (Merk) | A write into the same Merk one level deep and eight levels deep costs the same in every part and leaves one mark. The version of the rules is `V0` for each grove version in force and `V1` for grove version 6. Two writes of one Merk that each remove an element leave neither readable and free the bytes grove version 5 frees for the two. The model refuses a write of two keys, a write into a tree that keeps its data flat, a write over an item that has a referrer, the removal of an element that is not there, and a put of a tree that names a root. |
| SP2 | `sp2_a_write_opens_a_merk_by_its_path_and_root_and_is_charged_nothing_for_its_host`; `sp2_the_merk_of_a_references_target_is_opened_by_its_own_path_and_root`; `sp2_a_reference_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts`; `sp2_the_model_reads_no_target_of_a_reference_under_a_marked_host`; `sp2_the_model_reads_no_target_of_a_reference_in_the_merk_it_writes`; `sp2_the_model_follows_one_hop_of_a_reference`; `sp2_the_model_puts_no_reference_but_a_plain_one`; `sp2_the_root_merk_is_opened_by_its_root_key_record_and_its_root`; `sp2_a_write_is_charged_nothing_for_the_host_of_a_merk_it_opens` (Merk) | The open of a write and the read of the host add up to what GroveDB's open through the host costs (`open_transactional_merk_at_path`). The open is one seek for a Merk with nodes and none for one without, and no storage cost. The root Merk opens as before, in two seeks. A write that puts a reference into another Merk seeks three times, for its put, the root of the target's Merk and the target, and seeks and loads the same whether the host of that Merk is 99 bytes longer or not. Such a write, its finalize, the reads of the hosts on its way and the read of the host of its target's Merk add up to the cost of the same insert on grove version 5, with the same root hash, one to three levels deep. The model refuses a reference whose target is under a marked host, one whose target is in the Merk it writes, one whose target is a reference, and a reference of another kind. |
| SP3 | `sp3_a_write_is_charged_the_root_of_its_stop_point_and_nothing_for_the_mark`; `sp3_the_root_merk_has_no_host_and_gets_no_mark`; `sp3_a_mark_is_free_and_a_version_without_stop_points_has_none` (Merk) | The root costs two hash calls, and none where the write left the Merk with no node. The mark costs nothing and is on the host. A write of the root Merk is charged no root, leaves no mark, and the finalize after it charges nothing and opens nothing. |
| SP4 | `sp4_a_write_is_charged_every_fetch_whatever_an_earlier_write_loaded`; `sp4_a_node_an_earlier_write_left_loaded_is_charged_the_read_of_its_record` (Merk) | The second of two writes of a Merk in one transaction costs what it costs alone on a grove that was committed with the first in it. The fetch of a loaded node is one seek and its record length, for every kind of feature. |
| SP5 | `sp5_a_write_under_an_indexed_tree_has_its_stop_point_at_the_primary`; `sp5_the_model_stops_no_write_below_an_indexed_tree`; `sp5_the_model_writes_no_primary_of_an_indexed_tree`; `sp5_a_carry_into_an_entry_of_an_indexed_tree_writes_its_rows`; in Merk `sp5_a_write_stops_at_the_subtree_it_changed_where_no_indexed_tree_is_above_it`, `sp5_a_write_under_an_indexed_tree_stops_at_the_primary_of_the_outermost`, `sp5_a_write_of_a_version_that_carries_to_the_root_stops_at_the_root_merk` | In a grove with an indexed tree, a tree under an entry of it and a tree under that: all three have their stop point at the primary, and with a count tree in the place of the indexed tree each stops at itself. The model refuses a write whose stop point is not its own Merk, and a write of the primary. On grove version 5 the same insert two levels under an entry costs 19 seeks, 625 replaced bytes and 34 hash calls under the indexed tree and 14, 419 and 26 under the count tree, with the same added bytes. `stop_point` for every tree type, for indexed trees one inside the other, and for `V0`. |
| SP6 | `sp6_a_write_that_removes_a_host_takes_its_mark_with_it`; `sp6_a_write_that_puts_an_element_over_a_host_takes_its_mark_with_it`; `sp6_the_models_write_removes_no_tree`; `sp6_the_fixture_removes_no_tree_that_holds_an_element_with_referrers`; `sp6_the_marks_under_a_removed_host_go_for_nothing` (Merk) | Three hosts are marked, one of them in the Merk of another. The outer of those two is removed from the root Merk, and in the second test an item is put over it: the charge for the marks is nothing, the mark on it and the mark under it are gone, the mark on the third host stays, the finalize rewrites that host alone and adds and removes no bytes, and the grove has the root hash grove version 5 gives. The model's write removes no tree, and its fixture none that holds an element with a referrer. |
| FZ1 | `fz1_the_charge_of_a_finalize_is_no_part_of_a_write`; `fz1_a_write_after_a_finalize_reads_the_records_the_finalize_left`; `fz1_the_charge_of_a_finalize_is_the_sum_of_its_events_and_nothing_else` (Merk) | 36 writes with one finalize after them, and the same writes with a finalize after each: every write has the same seeks, hash calls and storage cost in both, and a write of a Merk that hosts nothing the same loaded bytes too. The finalizes are what differs. A write of the root Merk after a finalize that put a root key of 100 bytes into a host loads 101 bytes more than the same write before it, and is the same in every other field. |
| FZ2 | `fz2_a_host_of_many_changed_merks_is_rewritten_once_deepest_first`; `fz2_a_tree_that_keeps_its_data_flat_is_carried_with_its_counts`; `fz2_an_indexed_tree_is_carried_with_the_roots_its_write_left`; `fz2_an_mmr_is_carried_at_the_size_of_one_leaf_more`; `fz2_a_carried_tree_is_counted_by_a_host_that_counts` | Eight changed Merks under one host: the finalize opens that host, the host above it and the root Merk, once each and in that order, and puts each of the eight elements once (`8 * 76 + 77 + 79 + 77` replaced bytes). A tree of each of the five kinds that keep their data flat, with flags, and one of them wrapped in a host that counts, left with one entry more than its host says: the finalize adds and removes no bytes, the host is then the element of one entry written out in the test, with its flags and what the tree was made with (a chunk power, an entry size), and it commits to the root the write left: the hash of its value is the hash of that element combined with that root. An indexed tree of one axis, by its count and by its sum, and one that names two, each left with a root key for its primary, a root and a root key of its own for each axis, and an aggregate: the finalize adds and removes no bytes, the host is the element those make, written out in the test, and the hash of its value is the hash of that element combined with the root of the primary and the root of the secondary, or the digest of the roots of the axes. An MMR of one leaf is carried at the size of two leaves, which is three. A host that counts its elements counts a carried tree as GroveDB's insert left it counted: a tree that keeps its data flat as one, and an indexed tree by the count its write left, none or three. |
| FZ3 | `fz3_a_finalize_opens_no_merk_to_learn_its_root` | The Merks a finalize opened are those that hold a marked host: the twelve hosts and the root Merk, and none of the 24 Merks that were only written, whether or not the hosts were written too. Its seeks, its hash calls and its storage cost are the same whether the marked Merks hold one element or forty. |
| FZ4 | `fz4_a_finalize_adds_and_removes_no_bytes_whatever_the_writes_did`; `fz4_an_event_of_a_finalize_that_adds_or_removes_bytes_is_refused`, `fz4_an_event_that_removes_no_bytes_leaves_no_kind_of_removal_in_the_charge` (Merk) | After writes that add elements, lengthen and shorten values, remove elements and empty Merks, in every kind of tree: the finalize has no added and no removed bytes, and the grove has the root hash grove version 5 gives. `FinalizeCharge::event` refuses added bytes, plain removed bytes and removed bytes by owner, and keeps nothing of them. An event that says it removed none, as a plain removal of none or as a removal of none from an owner, is taken, and the charge has no removal of either kind: added to a write's plain removal of five bytes it leaves a plain removal of five. |
| OI1 | `oi1_disjoint_writes_cost_the_same_in_every_order_of_a_small_set`; `oi1_the_total_is_added_by_the_arithmetic_of_the_grove_version` | Five disjoint writes, to two Merks under one host, to that host, to a Merk under another host and to the root Merk, one of them a removal whose bytes are a plain number and one a removal of an element with flags, whose bytes go to an owner, in each of their 120 orders: each write's cost, the finalize's, and the total added up as the writes ran with the finalize last, as grove version 6 adds, are the same, and the root hash is the one grove version 5 gives. Of such a pair of removals: added as grove version 6 adds, both orders give every removed byte; added by the first arithmetic, one order gives none. |
| OI1 | `oi1_disjoint_writes_cost_the_same_in_random_orders_of_a_large_set` | 37 disjoint writes, to 24 Merks, their 12 hosts and the root Merk, across the twelve kinds of tree (new elements, longer and shorter values, removals of both kinds, new trees in a host): in 30 random orders, in the order every Merk comes before its host and in its reverse, the same holds. |
| OI1 | `oi1_two_writes_of_one_merk_are_not_disjoint_and_their_costs_follow_their_order` | What the rule does not claim. |
| ES1 | `es1_the_estimate_of_an_open_has_no_read_of_the_host`; `es1_the_estimate_of_an_open_has_the_seeks_and_the_hashes_of_the_open`; `es1_the_open_of_the_root_merk_has_the_bytes_of_its_root_key_record`; `the_open_estimates_follow_the_propagation_slot` (version) | The exact estimate on each grove version in force and on grove version 6, average and worst, below the root and at it. Against the open of a write: the same seeks, hash calls and storage cost, for twelve kinds of tree. The open of a root Merk whose root is under a key of 200 bytes loads 271 bytes, the 200 of the root key record and the 71 of the node: the estimate of grove version 6 has the 200 where it is told the length, and 256 where it is not and in the worst case; that of grove version 5 has none. Given a cost whose loaded bytes are full, both refuse and leave it as it was. |
| ES2 | `es2_the_estimates_of_a_write_hold_for_its_own_level_for_every_tree_type`; `es2_the_estimates_of_a_write_hold_for_the_shapes_of_the_corpus`; `es2_the_estimate_of_a_write_has_no_level_above_its_stop_point`; `es2_the_loaded_bytes_of_a_write_hold_in_a_full_merk` | For twelve kinds of tree and Merks of 0, 1, 7 and 30 elements: the average estimate has the bytes the write adds, is no less than the write in any field, and is the write in every field for a Merk with no node. The worst case is no less in any field. The same for the writes of the shapes of section 16.8. Eight levels deeper the estimates are the same. In full Merks of 31, 63 and 127 items both estimates are no less than the insert of a new last key in any field: it loads 754, 920 and 1,086 bytes, and the average estimate has 972, 1,134 and 1,296. |
| ES2 | `es2_the_estimates_of_a_replacement_and_of_a_removal_hold_for_their_own_level`; `es2_the_worst_case_of_a_level_holds_for_a_removal_that_rebalances_at_every_level` | For twelve kinds of tree and Merks of 1, 7 and 30 elements: both estimates of a replacement by an element of the same size are no less than the write in any field, and the average case of a replacement by a longer element has no added bytes; both estimates of the removal of the first, the middle and the last element are no less in seeks, hash calls, replaced bytes and loaded bytes, and have no removed bytes. In the leanest AVL trees of 1, 2, 3, 5, 7 and 9 levels the worst case is no less than the removal of every key of the smaller ones and of the first, the root and the last of the larger, in seeks, hash calls, loaded bytes and replaced bytes. |
| ES2 | `es2_the_hash_calls_of_an_insert_are_counted_from_the_bytes_of_the_element`; `es2_the_creation_of_a_tree_of_every_tree_type_is_estimated_to_its_hash_calls`; `es2_a_replacement_and_a_patch_count_the_hash_calls_of_an_insert`; `es2_an_indexed_tree_that_names_more_axes_than_one_can_is_refused`; `es2_the_referrers_of_an_element_are_no_part_of_its_hash`; `the_estimates_of_an_operation_are_gated_to_v6` (version) | The insert into a Merk with no node of an item of every length from 0 to 330 bytes and about 16,384, of an item of three bytes with flags of every length to 330, and into a sum tree of sum items and items with a sum whose flags or bytes are about 64, 128, 191, 251 and 313 bytes long: the average estimate is the write in every field and the worst case has its hash calls. The estimate of grove version 5 is a hash call short where the element is 64, 128, 191 or 192 bytes. A tree of every tree type, and one that names one, two and three axes, with no flags and with flags of those lengths, in a normal tree, a count tree and a sum tree, and wrapped in a count tree: the average estimate is the write in every field, and the worst case has its hash calls. One exception: a private document store is estimated with the check that it is not there yet as a read that finds its element, and the check finds nothing, so its loaded bytes are over. A replacement and a patch of an item of every length to 330 bytes are estimated the hash calls of its insert, and the estimate of a replacement's level is two hash calls over the write at every length, the two of its node, which it has twice. An item that keeps the list of its referrers, 64 bytes with one referrer and 6 without, is put by Merk's own insert with the hash calls of its form without the list, and both estimates have those and the two of the node: four, where a plain item of 64 bytes has five. |
| ES2 | `es2_a_write_of_the_root_merk_is_estimated_with_the_put_of_its_root_key_record`; `es2_the_worst_case_takes_a_value_to_be_no_longer_than_its_biggest`; `es2_a_reference_is_estimated_without_the_reads_of_its_target`; `es2_a_sum_item_whose_flags_grow_adds_bytes_that_no_estimate_of_its_replacement_has`; `es2_the_average_case_takes_a_merk_to_be_as_shallow_as_its_count_allows`; `es2_a_patch_is_sized_from_the_bytes_of_the_item`; `es2_the_rows_of_an_indexed_tree_are_estimated_with_a_hash_call_twice`; `es2_the_average_case_has_the_nodes_of_one_rotation`; `es2_a_patch_by_more_bytes_than_the_item_has_is_refused`; `es2_a_patch_is_estimated_by_the_growth_of_the_element_as_it_is_serialized`; `es2_a_removal_in_a_layer_too_long_to_count_is_refused`; `es2_the_removal_of_a_tree_in_a_layer_too_long_to_count_is_refused`; `es2_a_tree_put_where_an_item_was_adds_bytes_that_no_estimate_of_its_replacement_has`; `es2_no_size_of_a_layer_overflows_an_estimate_of_an_operation_on_grove_version_6` | Limits of section 16.5, and one of section 16.9: with the seek a batch's estimate adds for the root key record, both estimates of a write of the root Merk are no less than the write, and two hash calls over for an empty one. Beside a value of 60,000 bytes the worst case holds, and beside one of 500,000 it does not. A reference is estimated as an item of its length is, with one hash call more, and its write costs two seeks, the hash calls of the prefix of its target's path and the hash of its target's bytes beyond that: one block for a target of 20 bytes, four for one of 200. A sum item whose flags grow by 16 bytes adds 17, and both estimates of its replacement add none, on grove versions 5 and 6. An insert under the deepest node of the leanest AVL tree of 54 elements, eight levels, seeks 17 times where the estimate by its count has 15, and a finalize of a host of seven trees in four levels twelve where the estimate has ten; given the levels, both estimates hold. An insert that turns an AVL tree of five elements in three levels seeks nine times, and the estimate by those levels has nine. A patch that grows an item with a flag byte from 62 bytes to 63 adds two bytes: both estimates of grove version 6 have the two, and those of every grove version in force have one. The estimate of the rows of an indexed tree has one hash call more for each axis on grove version 6 than on grove version 5. The insert of a twelfth item into a Merk of eleven in four levels makes a double rotation, and seeks 15 times, replaces 889 bytes, loads 934 and hashes 19 times where the estimate by those levels has 11, 635, 810 and 15. A patch that says an item of four bytes grew by five, or by 100, is refused on grove version 6, in the average case and in the worst; one that grew by four, or shrank, is estimated. The removal of an element from a layer of trees with a count and flags of `u32::MAX - 11` bytes is estimated on grove version 5 and refused on grove version 6, with nothing added. Over sizes on both sides of what an estimate refuses, for every kind of layer, each estimate of an operation that grove version 6 selects is counted or refused, and a refusal of one at a version of grove version 6 has no cost. A patch of an item from 250 bytes to 251 adds three bytes, and is estimated one where the caller says it grew by one and three where it says three. The same of a removal up the tree, whose estimate sizes a tree of the layer. A tree with 128 bytes of flags put where an item with no bytes was adds 99 bytes, and both estimates of its replacement have none, on grove versions 5 and 6. |
| ES2 | in Merk: `es2_a_level_has_the_fetch_of_each_node_below_its_root_on_grove_version_6`, `es2_a_layer_of_trees_is_sized_as_the_cost_of_their_elements_on_grove_version_6`, `es2_a_worst_case_level_is_what_it_was_on_every_grove_version_in_force`, `es2_a_worst_case_level_has_the_most_levels_and_two_seeks_a_node_on_grove_version_6`, `es2_a_merk_of_more_levels_than_a_merk_can_have_is_refused`, `es2_the_levels_of_a_count_are_the_fewest_a_tree_of_that_count_has`, `es2_a_level_of_long_elements_is_counted_in_full_or_refused`, `es2_the_size_of_a_value_too_long_to_count_is_refused_on_grove_version_6`, `es2_no_size_of_a_layer_overflows_an_estimate_of_grove_version_6`, `es2_a_refused_level_adds_nothing_to_the_cost_it_was_given`, `es2_a_level_of_several_kinds_of_element_is_sized_as_the_version_before_sizes_it`; `the_estimates_of_one_level_are_gated_to_v6` (version) | Each of the versions of section 16.5 against the one before it, that grove versions 1 to 5 select what they did, and that a height of more than 91 levels is refused where grove version 6 estimates. A level of 33 nodes of 130,150,400 bytes is counted in full, 4,294,966,731 replaced bytes and 4,294,967,886 loaded; one node more is refused. One node of an element 65,536 bytes short of what 32 bits hold is counted in every kind of layer, and one byte more is refused, with nothing added. A layer of several kinds has the bytes the version before gives it at every height to twelve levels. The levels of a count are the fewest a tree of that count has at every power of two to 2^31 and the counts beside it, and 2,097,152 elements are estimated in 22 levels where grove version 5 has 21. |
| ES3 | `es3_the_estimate_of_a_finalize_is_its_seeks_its_bytes_and_its_hashes_in_full_merks` | A host of seven trees, a full Merk of three levels, with one, two and four of its leaves marked, for twelve kinds of tree: the average estimate has the seeks, the storage cost and the hash calls of the finalize, and its loaded bytes are the nodes it loads at the bytes of a node of that kind of tree, worked out in the test. The worst case is no less in any field, and its loaded bytes are its reads at the biggest record. |
| ES3 | `es3_the_estimate_of_a_finalize_is_its_cost_for_a_tree_of_every_tree_type`; `es3_a_node_above_a_marked_element_is_sized_by_what_a_layer_can_say_of_it`; `es3_a_wrapped_host_is_estimated_with_the_byte_of_its_wrapper` | A tree of each of GroveDB's 22 tree types as the tree a write stopped at, the trees that keep their data flat and the indexed trees among them, carried by the function a batch carries it with. As the only tree of its host: the finalize adds and removes no bytes, and the average estimate has its seeks, its storage cost and its hash calls. As the first of three, with a tree of its type above it: the seeks and the hash calls, and the replaced bytes but for what a layer has no word for, to the byte. A tree under each of the three wrappers, at the stop point and above it: told which Merks are wrapped, the average estimate has the storage cost, the hash calls and the seeks of the finalize; told of none it is a replaced byte short, and told of one that is not wrapped a byte over. The worst case is no less in any field in each. |
| ES3 | `es3_the_estimates_of_a_finalize_hold_for_the_shapes_of_the_corpus`; `es3_the_marks_of_a_finalize_are_the_hosts_above_its_stop_points_deepest_first`; `es3_the_estimate_of_a_finalize_does_not_follow_the_order_of_its_stop_points`; `es3_a_stop_point_far_down_takes_no_more_than_its_path`; `es3_the_estimate_of_a_finalize_is_of_the_marks_that_stand_when_it_runs`; `es3_a_grove_version_with_no_finalize_has_no_estimate_of_one`; in Merk `es3_a_finalize_in_one_merk_is_the_nodes_above_its_marked_elements`, `es3_a_worst_case_finalize_in_one_merk_bounds_every_changed_node`, `es3_the_nodes_above_marked_nodes_are_those_of_a_full_tree`, `es3_the_levels_of_a_worst_case_merk_are_the_most_an_avl_tree_can_have`, `es3_a_finalize_counts_the_levels_of_a_count_in_whole_numbers`, `es3_a_refused_estimate_adds_nothing_to_the_cost_it_was_given`, `es3_a_grove_version_with_no_finalize_has_no_estimate_of_one_in_a_merk` | For the eight shapes of section 16.8, both estimates are no less than the finalize in any field, and for `deep-nesting` the average has its seeks, storage cost and hash calls and loads `8 * 111 + 2` bytes, the two of the root key record. The marks from stop points, the same in either order of two stop points, and 200 of them for a stop point 200 levels down. The refusals on grove versions 1 to 5. In Merk the whole estimate of a finalize in one Merk against numbers worked out in the test, and the counts of nodes and of levels: a finalize in a Merk of 2,097,152 trees is one in 22 levels. A finalize of the 65,536 leaves of a full Merk of 17 levels is refused in both cases, and each of Merk's two estimates of a finalize, given a cost that is all but full in a field, refuses and leaves the cost as it was given, as its two estimates of a level do. |
| ES3 | `es3_the_hash_of_a_carried_element_is_counted_with_the_roots_it_names`; `es3_the_hash_of_a_carried_indexed_tree_has_the_root_keys_of_its_secondaries`; `es3_the_root_key_of_a_merk_is_sized_by_any_kind_of_element_its_layer_names`; `es3_a_kind_of_no_weight_sizes_no_root_key`; `es3_a_tree_that_keeps_its_data_flat_names_no_root_key`; `es3_a_sum_of_128_bits_is_hashed_at_its_longest` | A tree with no flags and with flags of 1, 62, 250, 251 and 313 bytes, carried with a root key of every length from 1 to 255 bytes: the average estimate has the hash calls, the seeks and the storage cost of the finalize. An indexed tree of each of the five kinds, carried with a root key of every length to 239 bytes and the root key of each secondary: the estimate is no less in hash calls and at the most one over for a tree of one axis and for one that names three, five over for one that names two of three, and nine or ten over for one that names one of three: the estimate takes such a tree to name three. A layer of several kinds that names keys of 128 bytes under either kind of element with a sum sizes the root key as a layer of that kind alone does. A tree whose sum is beyond 64 bits, under a root key of 43 bytes: four hash calls, and the estimate has them. A kind that such a layer gives no weight sizes no root key: with keys of 255 bytes under it the estimate is what it is without it, and four hash calls more once it has a weight. An MMR of one leaf is carried in the hash calls of the estimate, for a layer that says it holds an element as for one that says none. |
| ES3 | `es3_the_root_key_is_taken_at_the_size_the_layer_gives_its_keys`; `es3_a_node_is_loaded_with_the_links_to_its_children_and_sized_with_one`; `es3_a_node_above_a_marked_element_is_sized_at_the_average_of_the_kinds_of_its_layer`; `es3_a_layer_of_several_kinds_sizes_the_flags_of_a_tree_without_their_length`; `es3_a_layer_longer_than_an_estimate_can_count_is_refused`; `es3_a_finalize_too_long_to_count_is_refused`; `es3_a_sum_that_does_not_fit_leaves_the_total_as_it_was`; `es3_the_root_merk_is_opened_by_its_root_key_record`; `es3_the_bytes_a_node_loads_are_sized_by_its_cost_and_not_by_its_record`; `es3_the_worst_case_hashes_a_host_at_the_size_its_caller_gives`; `es3_a_wrapped_indexed_tree_is_hashed_with_the_byte_of_its_wrapper` | Limits of section 16.5, and a refusal: a root key of 199 bytes in a layer whose keys are 67 is two hash calls more than the estimate, and the root key of a secondary that is the row of an entry of 133 bytes one more; a finalize in a full host of 31 trees loads 801 bytes where the estimate has 785; in a host of six plain trees under one with a sum of 128 bits a finalize replaces 322 bytes where the estimate has 310; a node above a marked element in a layer of several kinds, whose tree has one byte of flags, is one replaced byte more; a layer whose trees have flags within 65,536 bytes of what 32 bits hold is refused before anything is added. A finalize 70 levels down whose trees have flags of four thousand million bytes, more hash calls than 32 bits count, is refused with nothing added, and so are more marked elements in one Merk than 32 bits count. The sum of a finalize estimate that does not fit a field of a cost, any of them, leaves the total as it was, and so does a part that removes bytes. A finalize under a root Merk of one tree with a key of 80 bytes loads the 80 bytes of the root key record with the node, 347 in all, and the estimate has them. The node's 267 are equal in the two by their numbers alone: with a root key of 200 bytes in the tree's Merk where it had 196 the finalize loads four more and the estimate the same, and with one of a single byte it loads 152 where the estimate has 347. A host with 32,768 bytes of flags is carried in more than 513 hash calls: the worst case holds where it is told 65,535 bytes or the bytes of that host, has fewer than the finalize where it is told 17,486, and refuses a size whose length does not fit beside it, with no cost. An indexed tree whose element is 64 bytes with its length is carried in ten hash calls, as estimated, and in eleven when it is wrapped: as estimated where the estimate is told so, and one more than estimated where it is not. |

The model runs on the engine the suite is built for, so these tests run on RocksDB and on LigninDB alike (`GROVEDB_TEST_ENGINE=lignin`), with the same numbers.

**Planted faults.** A test that cannot fail holds nothing, so 113 faults were put in, one at a time, and the tests of the three crates run on each (`cargo test -p grovedb-version`, `cargo test -p grovedb-merk --lib -- cost_model estimated_costs`, `cargo test -p grovedb --lib stop_point_tests`; a fault in GroveDB's own files on the last alone). Seventy-seven are in the code of the rules and of the estimates, eleven of them a slot of grove version 6 set to what grove version 5 has. Thirty-six are in the model and the helpers of the tests: twenty-three make the model a grove that breaks a rule or make it a write it refuses, eleven make its finalize carry something wrong (a root, a root key, a count, an aggregate, flags, a wrapper, what a tree was made with), one adds the total of the writes by the wrong arithmetic, and one makes the helper that tells an estimate which Merks are wrapped name none. Fourteen of the 113 are of what the review of the pull request changed, and the eighteen of the rest that are in a file it changed were put in again after it, seven of them written for the changed code. With each fault that breaks a rule, at least one test named after that rule fails. The second column has those tests, or the first of them. Other tests fail with most of the faults as well, and are not listed.

| Fault | Tests of the rule that fail |
| --- | --- |
| SP1: the model's write opens the Merk above its stop point | `sp1_a_write_is_charged_nothing_above_its_stop_point` and the two `sp1_` tests that hold a write to grove version 5 |
| SP1: the model's write changes several keys; or writes into a tree that keeps its data flat; or over an element with referrers; or removes an element that is not there; or puts a tree that names a root: five faults | `sp1_the_model_changes_one_key_in_a_write`; `sp1_the_model_writes_no_tree_that_keeps_its_data_flat`; `sp1_the_model_keeps_no_list_of_referrers`; `sp1_the_model_removes_an_element_that_is_there`; `sp1_the_model_puts_no_tree_that_names_a_root` |
| SP1: the model asks a client for the owner of an element that has no flags | `sp1_a_removal_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts` |
| SP2: `host_read_at_open` charges the read on `V1` | `sp2_a_write_is_charged_nothing_for_the_host_of_a_merk_it_opens`, `sp2_a_write_opens_a_merk_by_its_path_and_root_and_is_charged_nothing_for_its_host` |
| SP2: the model charges a write the read of its host | `sp2_a_write_opens_a_merk_by_its_path_and_root_and_is_charged_nothing_for_its_host` |
| SP2: the model charges a write the read of the host of the Merk a reference's target is in | `sp2_the_merk_of_a_references_target_is_opened_by_its_own_path_and_root`, `sp2_a_reference_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts` |
| SP2: the model reads the target of a reference through a marked host; or in the Merk it writes; or follows a reference to a reference; or puts a reference of another kind: four faults | `sp2_the_model_reads_no_target_of_a_reference_under_a_marked_host`; `sp2_the_model_reads_no_target_of_a_reference_in_the_merk_it_writes`; `sp2_the_model_follows_one_hop_of_a_reference`; `sp2_the_model_puts_no_reference_but_a_plain_one` |
| SP3: `mark` charges a seek | `sp3_a_mark_is_free_and_a_version_without_stop_points_has_none`, `sp3_a_write_is_charged_the_root_of_its_stop_point_and_nothing_for_the_mark` |
| SP4: `fetch_as_not_loaded` charges a read that found nothing | `sp4_a_node_an_earlier_write_left_loaded_is_charged_the_read_of_its_record` |
| SP4: the model charges no open for a Merk an earlier write opened | `sp4_a_write_is_charged_every_fetch_whatever_an_earlier_write_loaded` |
| SP5: `stop_point` answers the changed subtree, whatever is above it | `sp5_a_write_under_an_indexed_tree_stops_at_the_primary_of_the_outermost`, `sp5_a_write_under_an_indexed_tree_has_its_stop_point_at_the_primary`, `sp5_the_model_stops_no_write_below_an_indexed_tree` |
| SP5: `stop_point` answers the primary of the nearest indexed tree | `sp5_a_write_under_an_indexed_tree_stops_at_the_primary_of_the_outermost` |
| SP5: the model lets a write stop under an indexed tree | `sp5_the_model_stops_no_write_below_an_indexed_tree` |
| SP5: the model writes the primary of an indexed tree, and none of its rows | `sp5_the_model_writes_no_primary_of_an_indexed_tree` |
| SP6: `marks_under_a_removed_host` charges a seek | the three `sp6_` tests |
| SP6: the model keeps every mark of a removed host | both `sp6_` tests of GroveDB |
| SP6: the model takes the mark on a removed host and keeps the marks under it | both `sp6_` tests of GroveDB |
| SP6: the model's write removes a tree; or its fixture removes a tree that holds an element with referrers: two faults | `sp6_the_models_write_removes_no_tree`; `sp6_the_fixture_removes_no_tree_that_holds_an_element_with_referrers` |
| FZ1: `FinalizeCharge::event` adds an event twice | `fz1_the_charge_of_a_finalize_is_the_sum_of_its_events_and_nothing_else` |
| FZ2: the model's finalize rewrites each marked element in an application of its own | `fz2_a_host_of_many_changed_merks_is_rewritten_once_deepest_first` |
| a wrong carry of a tree that keeps its data flat: with the counts it had; without its flags; without its wrapper; without the root its write left; with another entry size than it was made with: five faults | `fz2_a_tree_that_keeps_its_data_flat_is_carried_with_its_counts`, with each |
| a wrong carry of an MMR: one node more, and not one leaf more | `fz2_an_mmr_is_carried_at_the_size_of_one_leaf_more` |
| a wrong carry of an indexed tree: without the root its write left; with the root of the first axis for every axis; with other root keys for its secondaries; with another aggregate: four faults | `fz2_an_indexed_tree_is_carried_with_the_roots_its_write_left`, with each |
| FZ3: the model's finalize opens the Merk below each marked element | `fz3_a_finalize_opens_no_merk_to_learn_its_root` |
| FZ4: `FinalizeCharge::event` takes an event that adds or removes bytes | `fz4_an_event_of_a_finalize_that_adds_or_removes_bytes_is_refused` |
| FZ4: a finalize charge keeps the kind of removal of an event that removed no bytes | `fz4_an_event_that_removes_no_bytes_leaves_no_kind_of_removal_in_the_charge` |
| a wrong carry: the model's finalize writes a root key into the host of a Merk with no node | `fz4_a_finalize_adds_and_removes_no_bytes_whatever_the_writes_did`, by its comparison of the root hash |
| OI1: the model charges a write a seek when another write has left a mark | both `oi1_` tests of disjoint writes |
| OI1: the total of the writes is added by the first arithmetic of the costs crate | `oi1_the_total_is_added_by_the_arithmetic_of_the_grove_version`, `oi1_disjoint_writes_cost_the_same_in_random_orders_of_a_large_set` |
| ES1: the average open keeps the seek of the host's read | `es1_the_estimate_of_an_open_has_no_read_of_the_host`, `es1_the_estimate_of_an_open_has_the_seeks_and_the_hashes_of_the_open` |
| ES1: the worst-case open keeps the seek of the host's read | `es1_the_estimate_of_an_open_has_no_read_of_the_host` |
| ES1: the open of the root Merk has the seek of its root key record and not its bytes, in the average case and, as another fault, in the worst | `es1_the_estimate_of_an_open_has_no_read_of_the_host`, `es1_the_open_of_the_root_merk_has_the_bytes_of_its_root_key_record`, with each |
| ES1: the average open takes the root key at 256 bytes whatever length it is told; or has the bytes of a root key for a root Merk with no node; or adds its seeks before it knows that the bytes of the record fit: three faults | `es1_the_open_of_the_root_merk_has_the_bytes_of_its_root_key_record`; `es1_the_estimate_of_an_open_has_no_read_of_the_host`; `es1_the_open_of_the_root_merk_has_the_bytes_of_its_root_key_record` |
| ES2: grove version 6 selects `add_average_case_merk_propagate` 2 | `es2_a_level_has_the_fetch_of_each_node_below_its_root_on_grove_version_6` and more `es2_` tests |
| ES2: grove version 6 selects `sum_tree_estimated_size` 2 | `es2_a_layer_of_trees_is_sized_as_the_cost_of_their_elements_on_grove_version_6` |
| ES2: grove version 6 selects the worst-case propagation that takes no version | `es2_a_worst_case_level_has_the_most_levels_and_two_seeks_a_node_on_grove_version_6`, `es2_the_worst_case_of_a_level_holds_for_a_removal_that_rebalances_at_every_level` and more `es2_` tests |
| ES2: grove version 6 selects the insert estimate of grove version 5, in the average case and, as another fault, in the worst | `es2_the_hash_calls_of_an_insert_are_counted_from_the_bytes_of_the_element`, `es2_the_creation_of_a_tree_of_every_tree_type_is_estimated_to_its_hash_calls`, `es2_a_replacement_and_a_patch_count_the_hash_calls_of_an_insert`, `es2_a_reference_is_estimated_without_the_reads_of_its_target`, with each |
| ES2: grove version 6 selects the replace estimate of grove version 5, or the patch estimate, in either case: four faults | `es2_a_replacement_and_a_patch_count_the_hash_calls_of_an_insert`, with each |
| ES2: the hash of an element is counted without its length | `es2_the_hash_calls_of_an_insert_are_counted_from_the_bytes_of_the_element`, `es2_the_creation_of_a_tree_of_every_tree_type_is_estimated_to_its_hash_calls`, `es2_a_replacement_and_a_patch_count_the_hash_calls_of_an_insert` |
| ES2: a tree is put without the hash that combines it with its root | `es2_the_creation_of_a_tree_of_every_tree_type_is_estimated_to_its_hash_calls` |
| ES2: an indexed tree is put without the hashes of its secondaries | the same |
| ES2: a reference is put without the hash that combines it with its target's | `es2_a_reference_is_estimated_without_the_reads_of_its_target` |
| ES2: the hash of an element is counted with the list of its referrers | `es2_the_referrers_of_an_element_are_no_part_of_its_hash` |
| ES2: the patch of grove version 6 takes a growth of more bytes than the item has, in the average case and, as another fault, in the worst | `es2_a_patch_by_more_bytes_than_the_item_has_is_refused`, with each |
| ES2: the patch of grove version 6 adds the flags of the item to its bytes once more, in the average case and, as another fault, in the worst | `es2_a_patch_is_sized_from_the_bytes_of_the_item`, with each |
| ES2: grove version 6 selects the size of a value of grove version 5, or of a tree of a layer; or the version it selects adds to sizes it did not check: four faults | `es2_a_removal_in_a_layer_too_long_to_count_is_refused`, `es2_the_removal_of_a_tree_in_a_layer_too_long_to_count_is_refused`, `es2_the_size_of_a_value_too_long_to_count_is_refused_on_grove_version_6`, `es2_no_size_of_a_layer_overflows_an_estimate_of_grove_version_6`, `es2_no_size_of_a_layer_overflows_an_estimate_of_an_operation_on_grove_version_6` |
| ES2: the average estimate of the removal of an element loads nothing | `es2_the_estimates_of_a_replacement_and_of_a_removal_hold_for_their_own_level` |
| ES2: the worst case of a level sets a field before the others are known to fit | `es2_a_refused_level_adds_nothing_to_the_cost_it_was_given` |
| ES2: the insert estimate of grove version 6 returns part of its cost where it refuses | `es2_no_size_of_a_layer_overflows_an_estimate_of_an_operation_on_grove_version_6` |
| ES2: the worst case of a level has one node for each level and one more | `es2_the_worst_case_of_a_level_holds_for_a_removal_that_rebalances_at_every_level`, `es2_a_worst_case_level_has_the_most_levels_and_two_seeks_a_node_on_grove_version_6`, `es2_the_worst_case_takes_a_value_to_be_no_longer_than_its_biggest` |
| ES2: the average propagation of grove version 6 takes a height it did not check | `es2_a_level_has_the_fetch_of_each_node_below_its_root_on_grove_version_6`, by a panic in its own arithmetic |
| ES2: it takes the levels of a count from a logarithm in floating point | `es2_the_levels_of_a_count_are_the_fewest_a_tree_of_that_count_has` |
| ES2: it has no fetch of the node beside the way | `es2_a_level_has_the_fetch_of_each_node_below_its_root_on_grove_version_6` and more `es2_` tests |
| ES2: it takes sizes it did not check | `es2_a_level_of_long_elements_is_counted_in_full_or_refused`, by a panic in the arithmetic of the version it sizes a node by |
| ES2: it multiplies its loaded bytes in 32 bits | `es2_a_level_of_long_elements_is_counted_in_full_or_refused` |
| ES2: it rounds a layer of several kinds to one node before the nodes multiply it | `es2_a_level_of_several_kinds_of_element_is_sized_as_the_version_before_sizes_it` |
| ES2: an indexed tree that names more axes than one can is counted | `es2_an_indexed_tree_that_names_more_axes_than_one_can_is_refused` |
| ES3: `nodes_above_marked` counts the last level too | `es3_the_nodes_above_marked_nodes_are_those_of_a_full_tree` and more `es3_` tests |
| ES3: the hash of a marked element leaves out the roots it names | `es3_the_hash_of_a_carried_element_is_counted_with_the_roots_it_names` and more `es3_` tests |
| ES3: the length of a root key is counted as one byte whatever the key | `es3_the_hash_of_a_carried_element_is_counted_with_the_roots_it_names` |
| ES3: the length of the flags is counted as a cost counts it | the same |
| ES3: an indexed tree is carried without the root keys of its secondaries | `es3_the_hash_of_a_carried_indexed_tree_has_the_root_keys_of_its_secondaries` |
| ES3: the keys of a layer of several kinds leave out the kinds with a sum | `es3_the_root_key_of_a_merk_is_sized_by_any_kind_of_element_its_layer_names` |
| ES3: a sum of 128 bits is hashed at the 16 bytes its cost has | `es3_a_sum_of_128_bits_is_hashed_at_its_longest` |
| ES3: the sizes of a layer are added to before they are checked | `es3_a_layer_longer_than_an_estimate_can_count_is_refused` |
| ES3: an indexed tree that names its axes is estimated without the digest of their roots | `es3_the_estimate_of_a_finalize_is_its_cost_for_a_tree_of_every_tree_type`, `es3_a_node_above_a_marked_element_is_sized_by_what_a_layer_can_say_of_it`, `es3_the_hash_of_a_carried_indexed_tree_has_the_root_keys_of_its_secondaries` |
| ES3: the marks are the hosts of the stop points alone | `es3_the_marks_of_a_finalize_are_the_hosts_above_its_stop_points_deepest_first` and more `es3_` tests |
| ES3: the root Merk is estimated the hash of its root | `es3_the_estimate_of_a_finalize_is_its_seeks_its_bytes_and_its_hashes_in_full_merks` and more `es3_` tests of GroveDB |
| ES3: the average finalize gives the open of the root Merk a root key of no bytes | `es3_the_root_merk_is_opened_by_its_root_key_record` and more `es3_` tests |
| ES3: the average finalize has no replaced byte for the wrapper of a host; or hashes a wrapped host without the byte of its wrapper; or takes every host to be wrapped: three faults | `es3_a_wrapped_host_is_estimated_with_the_byte_of_its_wrapper`; `es3_a_wrapped_indexed_tree_is_hashed_with_the_byte_of_its_wrapper`; the first and more `es3_` tests |
| ES3: the helper of the tests names no wrapped Merk | `es3_a_wrapped_host_is_estimated_with_the_byte_of_its_wrapper` |
| ES3: the average finalize takes the levels of a count from a logarithm in floating point | `es3_a_finalize_counts_the_levels_of_a_count_in_whole_numbers` |
| ES3: a kind that a layer gives no weight sizes the root key | `es3_a_kind_of_no_weight_sizes_no_root_key` |
| ES3: the finalize adds a host's cost to a sum that stops at the most a field holds; or counts more marked elements than 32 bits from their low bits: two faults | `es3_a_finalize_too_long_to_count_is_refused`, with each |
| ES3: the sum of a finalize estimate takes a part that removes bytes | `es3_a_sum_that_does_not_fit_leaves_the_total_as_it_was` |
| ES3: Merk's finalize of one Merk sets a field before the others are known to fit, in the average case and, as another fault, in the worst | `es3_a_refused_estimate_adds_nothing_to_the_cost_it_was_given`, with each |
| ES3: the average finalize loads every node twice | `es3_a_finalize_in_one_merk_is_the_nodes_above_its_marked_elements`, `es3_the_estimate_of_a_finalize_is_its_seeks_its_bytes_and_its_hashes_in_full_merks`, `es3_the_estimates_of_a_finalize_hold_for_the_shapes_of_the_corpus` |
| ES3: the worst-case finalize takes the fewest levels of a Merk | `es3_a_worst_case_finalize_in_one_merk_bounds_every_changed_node`, `es3_the_estimate_of_a_finalize_is_its_seeks_its_bytes_and_its_hashes_in_full_merks` |
| ES3: the worst-case finalize leaves out the reads by key | those two, and `es3_the_estimates_of_a_finalize_hold_for_the_shapes_of_the_corpus` |
| ES3: the worst-case finalize loads every record twice | the same three |
| ES3: the worst-case finalize hashes a host at 17,486 bytes whatever its caller gives; or adds the length of a host's bytes in a sum that stops at the most 32 bits hold: two faults | `es3_the_worst_case_hashes_a_host_at_the_size_its_caller_gives`, with each |

What the faults showed of the tests:

- Eleven of the faults are the estimates of grove version 5 in the place of those grove version 6 selects. With nine of them a test that compares an estimate with what the model applies fails: section 16.5 is from there. The other two are the sizes of a value and of a tree of a layer, which differ from those of grove version 5 only in what they refuse: the tests of the refusals fail with them, and no write is applied.
- A wrong carry leaves the cost of the finalize as it was, because the cost of a tree element does not follow its root key, its root or what it counts. For a tree of one Merk only the root hash finds it, which is why the tests of the model compare the root hash with the one grove version 5 gives. For an indexed tree and a tree that keeps its data flat, which the model does not write, the host is compared with the whole element the write's roots and counts make, written out in the test: a comparison with an element read back from the host passes with the fault in.
- The loaded bytes of an estimate are held only where a test has their number: with the nodes of a finalize loaded twice, every comparison of the kind "no less than" still passes.
- A mark under a removed host is seen only if it is still there when the host goes: a test that empties the host first holds the mark on the host and nothing under it.
- A total that is added by the wrong arithmetic is seen by the 32 orders of 37 writes, and by the test of the two arithmetics, and not by the 120 orders of five.

**What the tests do not show.**

- **Threads.** The orders of OI1 are orders of writes on one thread. That scopes on two threads give the costs of one of those orders is SC7 of the transaction rules, which the engine's own tests hold (TXN-09), and GROVE-12's acceptance.
- **A batch as a write.** The model's write is one key of one Merk: the direct insert, and the removal of an element. Each is an application of the Merk of its own, and two in one write would not read what the other staged, so the model refuses a write of two keys. A batch that changes several keys of a Merk in one application, changes several Merks, creates a tree and fills it, moves an element or writes references is charged by SP1 as it is on grove version 5 inside its stop points. Nothing here applies one.
- **A reference into a Merk whose host is marked, or into the Merk the write changes; a reference to a reference; a reference of another kind than the plain one.** SP2 says what the write is charged for each Merk it opens to read a target, and the model has no way to the root of a Merk under a marked host but that host. For a target in the Merk the write changes, GroveDB's direct insert opens the Merk a second time to read it, and a batch reads it in the Merk it has open. Which of the two a write of grove version 6 does is GROVE-11's, and by SP2 neither reads the host. The model refuses all four (`sp2_the_model_reads_no_target_of_a_reference_under_a_marked_host`, `sp2_the_model_reads_no_target_of_a_reference_in_the_merk_it_writes`, `sp2_the_model_follows_one_hop_of_a_reference`, `sp2_the_model_puts_no_reference_but_a_plain_one`).
- **A write under an indexed tree, a write of one, and a write of a tree that keeps its data flat.** The model refuses all three (`sp5_the_model_stops_no_write_below_an_indexed_tree`, `sp5_the_model_writes_no_primary_of_an_indexed_tree`, `sp1_the_model_writes_no_tree_that_keeps_its_data_flat`). For the first two (SP5): such a write is charged as on grove version 5 up to the primary, with the rows it writes, and nothing here applies one on grove version 6. That a row of an indexed tree can change its length when it moves is from reading the batch's accounting, not from a test. The finalize of an indexed tree and of a tree that keeps its data flat is in the tests, as the rewrite of its element by the function a batch rewrites it with, with a root that the model makes up, for an indexed tree the roots of its axes, and for a tree that keeps its data flat one entry more than it had: what it costs does not depend on a hash, and the hash the host then commits to is held to those roots.
- **A write of an element that keeps a list of its referrers.** GroveDB's insert over such an element keeps the list and writes the referrers, which are in other Merks. By SP1 the write is charged for each of them where it is, and by SP2 it opens each Merk by its own path and root. The model makes no such write (`sp1_the_model_keeps_no_list_of_referrers`).
- **A put of a tree that names a root, and a write that fails.** GroveDB's insert of a tree that names a root opens the Merk under it and holds the element to it. A removal of an element that is not there fails. The model makes neither (`sp1_the_model_puts_no_tree_that_names_a_root`, `sp1_the_model_removes_an_element_that_is_there`).
- **What a write that removes a host is charged.** SP6 is held for the marks, by a fixture that takes the element of the host out or puts an item over it. The removal itself is charged to the write as on grove version 5 (SP6): GroveDB opens the tree to see that it is empty or to free what it holds, and follows the referrers of what it held. The model makes no such write, and its fixture says nothing of that charge. A batch that deletes a tree with what is under it, clears a Merk or moves a subtree is not applied here.
- **A write or a finalize that fails.** Section 16.4 says what each returns. No test makes one fail.
- **The client's callbacks.** Elements of the tests have flags, and no write of the tests is given a client's callback for an update of flags. One kind of write is given a callback that gives its removed bytes to an owner. FZ4 says a finalize calls no callback. The test of it is that the finalize of the model has none to call.
- **The engine's marks.** The model keeps its marks itself. On LigninDB a write of a host after a Merk it hosts was emptied by a write that stopped, and before the finalize, fails in the engine (`no node at`): the place of the hosted root in the host is behind, and only the pending marks of SC9 tell the engine so. `fz4_a_finalize_adds_and_removes_no_bytes_whatever_the_writes_did` writes its hosts first for that reason. On RocksDB either order passes.

### 16.8 What moves

For groves shaped as traces of the golden corpus are, and writes that each put one new element: what grove version 5 charges the writes together, what grove version 6 charges them, and what it charges the one finalize after them. `the_costs_that_move_from_the_writes_to_the_finalize_are_those_of_the_document` holds every number of the table. Each cell is seeks / loaded bytes / replaced bytes / hash calls. The bytes the writes add are the same on both versions and are in the second column. No bytes are removed.

| Shape | Writes, added bytes | Grove version 5: the writes | Grove version 6: the writes | Grove version 6: the finalize |
| --- | --- | --- | --- | --- |
| `balances`: a sum item into a sum tree of 40 at the root | 1, 166 | 17 / 1,056 / 877 / 20 | 13 / 838 / 786 / 17 | 3 / 127 / 91 / 3 |
| `documents`: a document five levels deep and three index entries six levels deep | 4, 789 | 121 / 7,804 / 4,373 / 214 | 24 / 1,444 / 1,514 / 44 | 29 / 1,988 / 1,132 / 58 |
| `tokens`: a balance into each of two count-sum trees under a big sum tree | 2, 364 | 34 / 2,666 / 1,698 / 48 | 14 / 700 / 882 / 22 | 11 / 1,083 / 564 / 18 |
| `element-zoo`: an element into each of 36 trees of twelve kinds, 24 of them hosted by the other 12 | 36, 6,837 | 696 / 43,732 / 27,824 / 822 | 228 / 12,544 / 13,856 / 372 | 96 / 6,578 / 3,892 / 190 |
| `deep-nesting`: an element eight levels deep | 1, 182 | 30 / 1,490 / 902 / 61 | 5 / 296 / 294 / 9 | 17 / 586 / 608 / 45 |
| `tiny-subtrees`: an element into each of 40 trees of one element under one host | 40, 6,480 | 725 / 42,764 / 22,108 / 966 | 120 / 3,600 / 5,080 / 280 | 122 / 8,732 / 3,118 / 204 |
| `long-keys`: an element into three of five trees under keys of 255 bytes | 3, 546 | 49 / 8,090 / 4,381 / 151 | 15 / 654 / 762 / 51 | 16 / 2,331 / 1,974 / 47 |
| `wide-sums`: a sum item into each of three sum trees under a sum tree | 3, 498 | 42 / 2,533 / 1,552 / 61 | 15 / 792 / 786 / 27 | 11 / 635 / 390 / 19 |

How the rows read:

- **One write.** In `deep-nesting` the three columns add up but for the reads of eight hosts, which nobody is charged any more (SP2): 30 seeks are 5, 17 and 8. A write eight levels deep is charged a sixth of the seeks it is charged today, the finalize 17, and 8 are of reads that nobody is charged.
- **Many writes under one host.** In `tiny-subtrees` the 40 writes were charged 725 seeks: 120 in their own Merks, 80 to read the hosts on their ways, and 525 to carry the host and the root Merk 40 times. One finalize carries them once, in 122.
- **What a write still pays** is its own Merk: every added byte, every byte it replaces there, every fetch and hash on the way to its key. A removal's bytes are a write's too (FZ4).
- **What nobody pays unless the application charges it** is the last column. It has no added and no removed bytes in any row, so no storage fee and no refund depends on it. It is seeks, loaded bytes, replaced bytes and hash calls.

### 16.9 For GROVE-11

What these rules ask of the code that builds stop points and finalize, in the order it will meet them.

1. **List grove version 6** in `GROVE_VERSIONS` in the change in which a write first stops, and record the corpus for it then (section 16.6).
2. **Open a Merk from its directory entry, to change it or to read from it** (SP2). If the code still reads the host in order to open the Merk, that read is passed through `host_read_at_open`, which on grove version 6 charges nothing of it. That read alone is. A read of the host that a write makes for another reason, because the operation asks for that element, is an event of the write like any other (SP1): it is charged as the read it is, and is not passed to `host_read_at_open`, which would take its charge away. On RocksDB there is no directory, and RocksDB is removed before these rules are in force.
3. **Charge a write as if nothing were loaded** (SP4): a Merk cache that outlives a write adds `fetch_as_not_loaded` for a node it did not fetch in that write, or the cache is the write's own.
4. **Ask `stop_point` for the stop point** (SP5), with the tree type of every subtree on the path, and under an indexed tree carry in the write up to the primary of the outermost one. Such a write claims the secondaries of every indexed tree it carries through.
5. **Take the marks at and under a host with the write that removes it or puts another element over it** (SP6), through `marks_under_a_removed_host`. What a batch's delete of a tree, a clear and a move do with marks under them is to be decided there; these rules charge nothing for a mark that goes.
6. **Add up a finalize in a `FinalizeCharge`** (FZ1, FZ4), one application for each Merk, in the order of FZ2, with closures that leave flags alone and remove bytes as plain numbers. A finalize has a place among the writes of a transaction, which the application gives it: the writes after it read what it wrote (FZ1). Where the costs of writes and of a finalize are added outside an operation, they are added by the arithmetic the grove version selects for removed bytes (OI1).
7. **The batch estimators stop where the batch stops** (ES2). `grovedb/src/batch/estimated_costs` walks every level up to the root for every grove version, and that directory is not in this task's files. Four things there are open until then, each on grove version 6 alone: the walk itself; the seek it adds for the root key record of the root Merk (`update_base_merk_root_key`), which a write that stops below the root Merk does not write; its three calls of `worst_case_merk_propagate`, which takes no grove version, where the estimates of `grovedb::estimated_costs` call `add_worst_case_merk_propagate_versioned`; and its own calls of `add_cost_case_merk_replace_same_size` and `add_cost_case_merk_replace_layered` for the fan-out of backward references, with the estimates of its own carries (`average_case_merk_insert_tree`, `average_case_merk_replace_tree` and the worst-case two), which count the blocks of a value without its length. Until that change, a caller that names `GROVE_V6` itself gets from a batch's estimate the open of grove version 6, with no read of the host, for an apply that still reads it: the version is not listed for that reason (section 16.6), and whether the batch's estimate refuses it meanwhile is for `grovedb::batch` to say. No estimate of a batch has the reads of the target of a reference it puts. The estimate of the rows of an indexed tree (`average_case_indexed_secondary_mirror`, and the worst case that `grovedb::batch` computes itself) adds the hash call that combines a row with its target, which the estimate of the row's insert has on grove version 6: one call over for each axis until the estimate of the rows has a version of its own.
8. **Hold the acceptance to these numbers, and to the claims beside them.** The model's tests take a write and a finalize as functions of a grove. Run on GROVE-11's own write and finalize in the model's place, they are its acceptance for costs: the same assertions, with nothing of the model left. They are assertions of costs alone, and a write can pass every one of them and still read its ancestors. The model does: to find its stop point it looks up the entry of every Merk on the path, and where the transaction has not written a Merk it reads the host for that, charged to nobody (section 16.7). In the engine such a look is a claim. The directory entry of a Merk is in that Merk's unit (`transactions.md`, SC2), a read of it is a read claim, and a read claim is refused beside a writer of the unit (SC3): a write of a child that looked at its ancestors so would fail with `ScopeOverlap` beside a writer of an ancestor that touches nothing of the child's host. So GROVE-11 defines how a write is given its boundary, the tree types on its path and its root, without a claim on a unit above its stop point, opens the Merk it changes and the Merk of a reference's target from their own entries alone (2), and adds to the acceptance a case of claims: a write of a child and a write of an ancestor that does not touch the child's host, side by side, both ending. A read of a host that a write makes for another reason than to open a Merk keeps its charge, and is a claim like any other read (2).
9. **Make the estimates of a finalize public** with the version: `GroveDb::average_case_finalize` and `worst_case_finalize` lose their `pub(crate)` and the attribute over it, and each gets its line as an estimate in `PUBLIC_OPERATIONS` of the test kit, which `every_public_operation_is_classified` asks for.
