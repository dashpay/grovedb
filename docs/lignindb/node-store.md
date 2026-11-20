# NodeStore: the seam between Merk and its engine

LigninDB task SEAM-01 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

`NodeStore` is the handle through which a Merk reads and writes the nodes of one subtree. It lives in `merk/src/store/mod.rs`. Since SEAM-01, Merk reaches storage only through it. It has two implementations: the adapter over the storage contexts of `grovedb-storage`, in `merk/src/store/kv.rs`, and the store over a write transaction of LigninDB, in `merk/src/store/lignin` (MERK-01, [The LigninDB store](#the-lignindb-store)).

This document states the contract of the trait, how the RocksDB adapter meets it, how the trait gives an engine the write set and the reads that the transaction rules need (`transactions.md`, ST6 and RD5), and which operation serves each place where GroveDB reaches Merk's records without going through Merk ([findings 2 and 3 of the storage surface inventory](storage-surface.md#findings-for-the-seams)).

## The shape of the seam

```text
Merk<S: NodeStore<'db>>      AVL shape, hash rules, cost rules, proofs, chunks
  MerkSource   (Fetch)       fetch_node
  MerkCommitter (Commit)     prepare_node, then put_node in key order
---------------------------- NodeStore -----------------------------------
merk::store::kv              every StorageContext: one whole record per node
merk::store::lignin          LigninDB (MERK-01): cells, links and hash records
```

The seam speaks in nodes, not in records. A read returns a `TreeNode`. A write hands the store the `TreeNode` in memory and says which of its parts changed. Encoding a node into one record, and decoding it, is what the RocksDB adapter does; it is not part of the contract. The one exception is the cursor, whose values are node records (see [Cursor](#cursor)).

A handle belongs to one subtree. Opening a handle by path or by prefix, transactions, and the batch that spans several subtrees are the `Engine` seam (SEAM-04).

`'db` in `NodeStore<'db>` is the lifetime of the engine state the handle borrows: for RocksDB, the transaction. A cursor or a change set may borrow that state. It never borrows the handle, so it can outlive the borrow that made it, as RocksDB's raw iterators do today.

## Operations

| Operation | What it does | RocksDB adapter | Cost on RocksDB ([codes](storage-surface.md#the-api-and-what-each-call-charges)) |
| --- | --- | --- | --- |
| `get_node(key, ..)` | The node under `key`, ready to be modified: with its key, its value-defined cost and its mutation baseline. | `get`, then `TreeNode::decode_raw` | `R-tx / R-imm` |
| `get_node_for_read(key)` | The fields of the node under `key`, for a caller that only reads: no key, no value-defined cost, no baseline. | `get`, then `TreeNodeInner::decode` | `R-tx / R-imm` |
| `has_node(key)` | Whether the subtree holds a record under `key`. The record is not decoded, so one that does not decode counts as held. SEAM-04 added it for `has_raw`. | `get`, then `is_some` | `R-tx / R-imm` |
| `fetch_node(link, ..)` | The node a parent's link points to. Default: `get_node(link.key())`, an error if it is absent. | the default | `R-tx / R-imm` |
| `fetch_node_without_value(link)` | The node a parent's link points to, for a walk that only reads and asks for the values it needs: a store may leave the value out. Default: `fetch_node` with no value-defined cost. MERK-06 added it. | the default: the whole node | `R-tx / R-imm` |
| `load_node_value(node)` | Reads the value of a node that `fetch_node_without_value` returned without it. | never asked: no node comes without its value | zero |
| `write_node(node, change)` | Writes one node at once, outside a change set, uncharged. Default: a change set of that one node. | `put` with no cost info | `W-tx / W-imm` |
| `root_key()` | The recorded key of the root node. | `get_root(ROOT_KEY_KEY)` | `R-tx / R-imm` |
| `set_root_key(key)` | Records the root key at once, or removes the record. | `put_root` / `delete_root`, no cost info | `W-tx / W-imm` |
| `cursor()` | An ordered cursor over the subtree's nodes. | `raw_iter` | zero, then `I-move`, `I-key`, `I-value` |
| `change_set()` | An empty change set. | `new_batch` | zero |
| `commit(change_set)` | Applies a change set. | `commit_batch` | `B-part / B-imm` |
| `get_aux_record`, `get_meta_record` | A side record of the subtree. | `get_aux`, `get_meta` | `R-tx / R-imm` |
| `put_meta_record`, `delete_meta_record` | Writes or removes a meta record at once. | `put_meta`, `delete_meta`, no cost info | `W-tx / W-imm` |

A change set (`NodeChangeSet`) takes:

| Operation | What it does | RocksDB adapter |
| --- | --- | --- |
| `prepare_node(node, change)` | Takes from a node in memory what the store needs to write it. | encodes the whole record |
| `put_node(key, prepared, cost)` | Writes a prepared node. | `Batch::put` |
| `delete_node(key, cost)` | Removes a node. | `Batch::delete` |
| `state_root(key)` | States the key of the root node as the change set leaves the Merk, or that it leaves the Merk empty. Not a write. | nothing: the default |
| `put_root_key(key, cost)`, `delete_root_key(cost)` | Records or removes the root key. | `Batch::put_root`, `Batch::delete_root` on `ROOT_KEY_KEY` |
| `put_aux_record(key, value, cost)`, `delete_aux_record(key, cost)` | Writes or removes an aux record. | `Batch::put_aux`, `Batch::delete_aux` |

## Contract

### Reads

- `get_node` and `get_node_for_read` return `None` for a key the subtree does not hold, and `has_node` returns `false`. They look a key up; they do not need a tree in memory.
- `fetch_node` is the read of a tree walk: Merk has a parent's `Link` and wants the child. Merk's `Fetch` implementation for a Merk, `MerkSource`, calls only this. Its answer is defined by the link's key: it is the node `get_node(link.key())` returns at that moment. A link whose key has no node is an error (`KeyNotFoundError("Key not found for fetch")`), not `None`, and the read is charged as a lookup that found nothing.
- The link is the whole argument of `fetch_node` so that a store can keep a location in it and follow that instead of searching (MERK-01, PERF-03). The location is a shortcut to the same answer, never another answer. See [Following a location](#following-a-location) for what that still needs.
- A node from `get_node` or `fetch_node` may be modified and committed. A node from `get_node_for_read` has no mutation baseline and must never reach a commit: a commit would take it for a new node.
- `fetch_node_without_value` is the fetch of a walk that only reads the tree and takes the value of few of its nodes, which is the walk of a proof ([Proofs](#proofs)). A store may leave the value out, and says so in the node (`TreeNode::value_absent`). In everything else the node is the one `fetch_node(link)` returns at that moment with no value-defined cost, and the read is charged as that fetch: the whole record, whether or not the value comes with it. A link whose key has no node is the same error at the same charge. A node without its value has no mutation baseline. It must never reach a commit, and it is never kept in the tree of a Merk.
- `load_node_value` reads the value of such a node and puts it in the node. It is charged nothing: the fetch was charged the whole record. The value that is read is one the node's value hash and key-value hash are the hashes of, or the read fails with `Error::StorageError`: a store does not put a value under hashes that are not its own. What a value holds outside its hash is what the store holds when the value is read: an item that takes backward references is hashed with the list of its referrers emptied (#1059), so the list is the one of the moment of the read. A node that has its value is left as it is.
- A read returns what the engine shows the handle under that key at the moment of the call. On RocksDB that is the transaction's view: its own writes, and otherwise the latest committed record. Writes staged in a storage batch that has not been applied to the transaction are not visible to reads, as today. A store answers no read from a node it kept from an earlier read. Merk keeps the nodes it has loaded in its own tree, above the seam, on every engine alike.

### The root key

A Merk that is not layered under a parent element (base and standalone Merks) keeps the key of its root node in its store. `root_key` reads it on open. `put_root_key` and `delete_root_key` change it as part of a commit. `set_root_key` changes it at once (`Merk::set_base_root_key`).

`state_root` is not that record. `Merk::commit` calls it once in every change set, for every kind of Merk, after the commit walk and before the first `put_node`: with the key of the root node as the commit leaves the tree, whether or not it is another node than before, and with `None` when the commit leaves the Merk with no node. `Merk::clear` calls it too, with `None`, after its walk and before it commits the removals (MERK-05): it leaves the Merk with no node. Nothing else calls it: chunk restore and `write_node` state no root. It makes no write, is charged nothing and cannot fail. A store that finds every node by its key has no use for it, and the RocksDB adapter keeps the default, which does nothing. A store that keeps the links of the tree has to know where a walk begins: LigninDB keeps the Merk's entry in its subtree directory by it. MERK-01 added it, because the root cannot be worked out below the seam: `put_root_key` is issued only for a Merk with no parent element, and only when the key changed; the written node that no other written node links to is the root in a commit, but not in a chunk of a restore, and not when a record changes with no hash (#1059).

A layered Merk takes its root key from its parent element and does not use the record. On LigninDB the subtree directory names the root as well, and a Merk can be opened by it ([Opening a Merk](#opening-a-merk)). Loading the root node is `get_node` with that key: no separate operation exists, because a store that knows where the root is answers that lookup without a search.

### Cursor

`cursor()` returns an unpositioned ordered cursor over the nodes of the subtree. Its trait is `NodeCursor`, a re-export of `grovedb_storage::RawIterator`: seek to first, to last, to a key, to the last key not after a key, step forward and back, and read the key, the value and validity. The same trait is kept for three reasons:

- `QueryItem::seek_for_iter` and `iter_is_valid_for_type` in `grovedb-query` position cursors through it, and `grovedb-query` sits below Merk.
- GroveDB iterates raw records through it in 41 places (`Element::iterator`, `ElementsIterator`, replication).
- `FlatStore` (SEAM-03) returns the same trait for its cursor.

A second cursor trait in Merk would have to be bridged to all three. The trait names no engine.

A cursor's value is the node record in Merk's node encoding, the bytes `TreeNode::encode` produces. This is the one place where the seam shows the record form. The reason is `KVIterator`, which hands records to its callers, and those callers (in GroveDB) decode them. A store that keeps no records builds the record for the node under the cursor (MERK-05). Moving those callers to node views is PERF-05.

A cursor shows the state the handle's reads show. How long it stays stable under later writes is the engine's rule (`transactions.md`, RD6): a RocksDB iterator pins the committed state at its creation.

**A walk that ends is at the end of the nodes only if `status()` says so.** A move returns no error: a cursor whose engine could not make the move stands on no record, and `status()` gives the error. An engine can refuse a move (on LigninDB a read that meets another scope's write claim is refused, SC3), and a walk that takes "no record" for "no more nodes" would then act on part of a Merk. So the walks Merk makes itself ask `status()` before they draw a conclusion from their end, and the walk Merk hands to its callers says how it ended (MERK-05):

| Walk | On a cursor error |
| --- | --- |
| `Merk::clear` | fails with `Error::StorageError` before it removes anything: no removal is staged or written, and the Merk keeps its tree |
| `Merk::is_empty_tree`, `Merk::is_empty_tree_except` | answer `false`. Neither has an error to return, and "not empty" is the safe side: a caller removes a tree's element without removing its nodes on the answer "empty", and refuses or walks the tree on the other |
| `KVIterator` | `next_kv` gives the records before the error and then `None`, and stays ended: it does not seek the next query item, which could take the error off the cursor, and `KVIterator::status()` is then the error (both since GroveDB #1063). MERK-05 added what a cursor of a transaction needs: a key with no record under it, which such a cursor shows when the node it stands on was removed, is passed over and not unwrapped, and a record that could not be read ends the scan with that error as its status |
| the pinned-view read (`stored_node_bytes`, behind `read_value_from_view` and `PinnedViewSource`) | fails with `Error::StorageError`, after the seek (as before MERK-05) and after the read of the record: a record that could not be read is no missing node |

`status()` reads nothing and is charged nothing, so no cost changes. On RocksDB all of these behave as before unless its iterator reports an error.

**What is still owed, by the callers.** `KVIterator::next_kv` has no error to return, so a caller that concludes something from the end of a scan has to ask `KVIterator::status()`. GroveDB's scans outside its test code, by the function that makes them:

| Scan | Asks `status()` |
| --- | --- |
| `rebuild_indexed_tree_secondaries` (`grovedb/src/operations/indexed_tree.rs`): its walk of the primary and its walk of each secondary | yes, since #1063 |
| `clear_subtree_with_costs_v0` and `clear_subtree_with_costs_v1` (`grovedb/src/operations/delete/clear_subtree/`): the walk that deletes the subtrees nested in a Merk before the Merk is cleared | no |
| `verify_merk_and_submerks_in_transaction` (`grovedb/src/lib.rs`) | no |
| `verify_indexed_axis_content` (`grovedb/src/lib.rs`): its walk of the primary and its walk of the secondary | no |
| `indexed_axis_range_rows_generic` and `collect_top_k_via_iterator` (`indexed_tree.rs`): walks of a secondary | no |

Scans that are compiled for tests only are not in the table, because no operation concludes anything from their end: `legacy_linear_indexed_count_top_k_paginated` in `indexed_tree.rs` and the helper `stored_rows` in `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs`, neither of which asks. On RocksDB nothing changes for the scans of the table. On LigninDB a scan that a scope's claim refuses would end early there, so GROVE-05 and GROVE-06 make them ask. The same holds for the cursor loops of `grovedb-query` and `Element::iterator`.

### Change sets

A change set is the unit of one commit to one subtree. `commit` hands all of it to the engine, or nothing. On RocksDB that merges it into the storage batch of the surrounding operation, or, on an immediate context, writes it to the transaction. When the transaction sees it is the engine's rule, not this trait's.

A node has three parts:

| Part | Holds |
| --- | --- |
| value | the key, the value bytes, the feature type (the node's own aggregate contribution) |
| structure | which child, named by its key, hangs on each side |
| hash | the value hash, the key-value hash, and for each child its hash, its heights and its aggregate data |

Every node change is one of four kinds. The first three are the `NodeChange` passed to `prepare_node`. The fourth is `delete_node`.

| Kind | Parts that may differ from what the store holds under the key | The store may keep |
| --- | --- | --- |
| `ValueWritten` | value, structure, hash. The node may also be new to the store. | nothing |
| `StructureChanged` | structure, hash | the value part |
| `HashOnly` | hash | the value part and the structure part |
| removed | the node is gone | |

Rules:

1. **A kind never claims too little.** If a part differs from what the store holds, the kind names it. A store that keeps the parts a kind does not name holds exactly the node Merk wrote. `merk/src/store/mem.rs` is a test store that does this and checks the result against the node on every write.
2. **A kind never makes a write optional.** Every `put_node` is a write of the record under its key, whatever the kind. See [The write set](#the-write-set).
3. **A kind may claim too much.** Merk reports `ValueWritten` whenever it cannot prove a narrower kind. See the next section for the cases.
4. **Child heights and child aggregate data are in the hash part**, with the child's hash, not in the structure part. They change on the path to the root whenever a hash does: an insert below changes heights, and in a sum or count tree every write changes aggregates. The structure part changes only when a link names another child, or a child appears or goes.
5. **Each key appears at most once in a change set**, as a write or as a removal.
6. **Two steps per written node.** Merk calls `prepare_node` during the commit walk, while it holds the node, children before parents. It calls `put_node` after the walk. `Merk::commit` sorts by key in between, so a commit issues the root key first, then node writes and removals in ascending key order, then aux records. Chunk restore issues `prepare_node` and `put_node` together, in the order it visits the chunk. What `prepare_node` returns is the store's own type: the RocksDB adapter keeps the encoded record, and a store that writes cells can keep only the parts the kind names.
7. **`write_node`** is for the one place that writes a single node outside a change set (chunk restore, when it corrects a parent's link). It is uncharged.

### How Merk decides the kind

`TreeNode::take_node_change`, called by `MerkCommitter` for every node the commit walk writes. It reads the node's marks and clears them, so the committer calls it once nothing in the write can fail any more: a write that fails leaves the node as it was.

| Kind | When |
| --- | --- |
| `ValueWritten` | The node has no mutation baseline (it was created, not loaded), or a value was put on it since it was loaded or last committed, or its feature type was replaced in place since then. |
| `StructureChanged` | Otherwise, when a child slot was emptied or filled since the node was loaded or last committed, and the tree walk could not show that the same child came back. |
| `HashOnly` | Otherwise. |

The node is written at all only because it, or a node below it, changed. So `HashOnly` is the common case: the ancestors of a changed node.

What makes this sound:

- **Value.** In a tree that is committed, a value, a value hash or a key-value hash changes only in the `put_value` functions and the put of a moved node. On a loaded node each of them records a storage cost for the commit (`known_storage_cost`), and that is the mark the rule reads. A feature type changes there too, and in one more place: `TreeNode::set_feature_type` replaces it in place and leaves the value and its cost alone. The conversion of a tree to a 128-bit sum (`Merk::convert_to_big_sum_tree`, GroveDB #1063) does that to every node of the tree and commits them, so `set_feature_type` sets a third mark, `feature_type_changed`, and the rule reads it. Chunk restore changes nodes outside a commit and states its kinds itself. `take_node_change` asserts in debug builds that a node it does not report as `ValueWritten` still holds its baseline value.
- **Structure.** A node in memory carries two marks for its children, `children_changed` and `on_loan` (`NodeMarks`). They sit in the boxed part of the node, encode to nothing and do not take part in equality, so a stored node and the size of a `TreeNode` are what they were.
  - Everything that can put another child in a slot sets `children_changed`: `TreeNode::attach`, `take_link` (which `detach` and `Walker::detach` use), `set_link`, and `link_mut`, which hands out the link mutably.
  - The four walk functions (`TreeNode::walk`, `walk_expect`, `Walker::walk`, `walk_expect`) detach a child, lend it to a closure and attach what comes back. A walk does not set `children_changed` when it detaches. It marks the child it lends as `on_loan`, and attaching a node anywhere takes that mark off. When the walk attaches what its closure returned, the children count as unchanged only if that node still carries the mark and the slot held a child, or if the slot was empty and stays empty. A node that comes back with the mark is the node that was lent: any other node the closure can reach was attached, or was never lent.
  - Debug builds also keep the key of the child the slot held and assert that a node taken for the same child has it. Both test suites run with that check.
- The slot itself (`slot_mut`) is private to `merk::tree`. It is assigned without the mark in three places, each of which keeps the child's key: `TreeNode::load` (a pruned link becomes a loaded one), the commit walk (a modified link becomes a loaded one) and pruning (a loaded link becomes a pruned one). Three things are left to convention, because the type system does not enforce them. `child_mut` returns the child mutably and must not be used to replace it by a node with another key. `TreeNode::inner` is visible to the whole crate. And the closure of a walk must not return a node that another walk lent and that was never attached since; release builds would take it for the same child. No code does any of the three, and `set_key` takes the `on_loan` mark off.

Where the kind claims more than the truth, by choice:

- A put of the same bytes is `ValueWritten`. The largest case is a parent's tree element whose bytes do not change while its value hash does, on every write to the subtree below. Reporting it as `HashOnly` needs a comparison with the baseline inside the put, which belongs with MERK-01 ("a value is written only when it changed") and MERK-02. The contract already allows it: the hash part includes the value hash and the key-value hash.
- A node this Merk created stays `ValueWritten` in later commits for as long as the Merk keeps it in memory, because it has no baseline. This is the same reason `Merk::commit` bills such a node as new.
- A removal that takes a node from deep in a subtree (`remove_edge`) marks every node it passes as `StructureChanged`.

MERK-02 ("map `Link` states to dirty flags") can narrow these. The rules above stay as they are.

### Costs

Every operation returns the `OperationCost` the store charges for it. Merk adds what it is given and charges nothing for storage itself. On RocksDB the charges are exactly those of the storage calls in the table above: the adapter adds none and drops none.

What Merk passes down are logical sizes from its own cost rules, the same for every engine:

- `NodeWriteCost` for a charged node write: the feature data's cost type and length, the key length and aggregate length of each child link, and the `KeyValueStorageCost` Merk worked out (or took from the put).
- a `KeyValueStorageCost` for a removal, a root key and an aux record, where Merk has one.

`cost: None` means the write is not charged by Merk's rules: chunk restore, `clear`, `set_root_key`, the meta records. What the engine then charges is the engine's rule today.

Where each part of a charge is worked out today:

- In Merk, above the seam, and passed down: the `KeyValueStorageCost` of a node. Merk's rules already count the 32-byte prefix in a key's cost (`KV::node_key_byte_cost_size`, and the removal cost in `merk/src/tree/ops.rs`). That is a fee rule now, whatever the engine keeps on disk, and it stays in Merk.
- In `grovedb-storage`, below the adapter: seeks, loaded bytes, the addition of the prefixed key length to a new node's key cost (`Batch::put`), and the key-value storage rule that turns the sizes into `OperationCost` when the batch is applied.

So the signatures carry sizes and a cost Merk computed, and no rule of how RocksDB turns them into a charge. SEAM-05 moved the second group into versioned functions, `grovedb_costs::cost_model`, re-exported as `grovedb_merk::cost_model` ([cost-model.md](cost-model.md), section 14). No signature here changed. Of the two ways open to it, it took the first: a store calls the functions and returns their result. On RocksDB the storage contexts under the adapter make those calls, because GroveDB and the append-only trees use the same contexts without a Merk; the adapter still adds nothing and drops nothing. `merk::store::lignin` (MERK-01, MERK-03) will call the same functions itself. `NodeWriteCost` needed no new field: the model takes the sizes it already carries.

Three existing behaviours that a store has to keep, from the inventory:

- `Merk::commit` drops the cost `commit` returns when `merk_versions.batch.commit == 0` (finding 9). The drop is in `Merk::commit`, not in the store.
- Reads on an immediate context are free, and its batch charges as it is filled (finding 9). That is below the adapter.
- The pinned-view read in `merk/src/proofs/query/count_offset/pinned_view.rs` charges one seek and the record length itself and does not bill the cursor's own key and value charges (finding 10). It reads through a cursor, so this stays as it is.

### Errors

Every operation returns Merk's `Error`. The adapter wraps a storage error as `Error::StorageError`, a record that does not decode as `Error::EdError`, and a batch's cost error as `Error::CostsError`, which is what each call site did before. `Element::get_optional_from_storage` passes a storage error on as `Error::StorageError`, so that a caller can tell a read that failed from a key that is not there (GroveDB #1056), and still reports a record that does not decode as `CorruptedData` with the same text.

**A store reports a read it could not perform as `Error::StorageError`, and under no other error.** This is a rule of the contract for every store, not a habit of the adapter. Since GroveDB #1056, `open_merk` takes `Error::StorageError` from the read of a parent element as a hard error and every other error as "parent not found", which several readers answer with absence. A store that reported a failed read under another error would turn a storage fault into a subtree that is not there. GroveDB's test store for failed reads, `FaultableReads`, wraps any `NodeStore` and fails a read exactly this way.

`Error::StorageError` holds a `grovedb_storage::Error`. That type is the last item of `grovedb-storage` that Merk names outside `merk::store`.

### The write set

The transaction rules (`docs/lignindb/transactions.md`) need the engine to see exactly the writes RocksDB sees today, key by key. A commit fails when another commit wrote a key after this transaction first wrote it (CF2), so a write that the engine never hears of, or hears of under another key, changes which commits fail. ST6 puts that on this trait:

> One node in a Merk change set is one write of that node's record key, whatever its class. The change set must name exactly the node records that today's Merk hands to storage for the same operation: no node left out because only its hash changed, and none added.

The trait meets it as follows.

- **One call per record, with its key.** `Merk::commit` calls `put_node(key, ..)` once for every node the commit walk writes and `delete_node(key, ..)` once for every node the batch removed. These are the nodes and keys for which it called `Batch::put` and `Batch::delete` before SEAM-01: the walk, the list of removed keys and the loop over them are the same code. Chunk restore and `clear` are the same, call for call.
- **The kind is not a filter.** Merk works out the kind after it has decided to write the node, and nothing reads the kind before the call. A `HashOnly` ancestor is put like any other node. On LigninDB it is stored as a hash record, and it is a write of that ancestor's key for the conflict rule.
- **A write that changes no byte is still a write** (ST5). A commit always writes the root node of a non-empty Merk, also when the batch changed nothing. It reaches the store as one `put_node` with kind `HashOnly` (`a_commit_names_what_changed_in_each_node` in `merk/src/store/tests.rs`).
- **The other records are named too.** The base root key is `put_root_key` or `delete_root_key`, one record of the subtree. Aux and meta records carry their keys.
- **Nothing is merged or dropped below Merk either.** The adapter turns each call into one call on the context's batch with the same key, in the order Merk made them, and ignores the kind (`every_change_is_one_batch_call_under_its_key_whatever_its_kind` in `merk/src/store/kv.rs`).

What a store may not do, then: skip a `put_node` because the node it holds is equal, fold two keys into one write, or report a write under a location instead of a key. What it may do is choose how to store each write.

Two things are the engine's and not this trait's. A change set is not yet a write-set entry: on RocksDB it is merged into a `StorageBatch`, and the transaction's write set is what that batch ends up holding when it is applied, one entry per key (GM0, WT5). A storage batch shared by several Merk commits keeps one final entry per key, and needs its boundaries marked (`StorageBatch::next_operation`); that is the multi-context batch of SEAM-04 and TXN-08. And an operation that never commits a Merk writes nothing: `Merk::commit` is the only place a change set is made for an apply.

The reads that matter to the rule are covered in [Reads](#reads): every read is answered by key at the moment of the call (RD3, RD5), a store keeps no node of its own, and a child that is gone is reported as missing, as today (WT7b). The trait has no operation that pins a view for a handle. A cursor's stability is the engine's rule (RD6).

### Following a location

"Fetch a node by link" exists so that LigninDB can descend by page pointer. The seam as it is gives a store the link, and the link carries the child's key. What it does not yet carry is a location. This is what is needed, and where it belongs:

1. **A place for the location in `Link`.** It has no such field today. What a location holds (a page id or an in-page slot, and the version it is good at) is fixed by FMT-02, TREE-01 and TREE-02, so the field comes with MERK-01 or PERF-01. It is a change in `merk::tree`. It changes no signature here: `fetch_node` already takes the whole link.
2. **Fetched nodes.** A store builds the node it returns from `fetch_node` and `get_node`, links included, so it can fill in the location of each child. No signature changes.
3. **A check before use.** A location is good only at the version it was read at (PN8). The store checks it before it follows it and otherwise finds the node by the link's key, which the link always carries. Either way the answer is the one the key gives. No signature changes.
4. **Nodes this Merk wrote.** A written node gets its location from the engine, and it gets it late. A node is placed when its write is applied, for all change sets of an operation together, not when it is prepared or put ([P5 of the tree layout](tree-layout.md), owner ruling D-034). So the location cannot come back through `prepare_node`, which takes the node by shared reference, or through `put_node`, which returns nothing, and on a batch it does not exist yet when `NodeStore::commit` returns. It comes back from the application of the batch, which is an operation of the engine: `Engine::apply_batch` and `Engine::apply_write_group` return `Applied`, the engine's report of the nodes the write created and carried ([engine.md](engine.md#the-batch-across-subtrees)). Until a Merk that stays open takes that report (PERF-03), a link to a node it wrote since it was opened is followed by key, which is correct and is a keyed lookup. `NodeStore`'s reads and the change set stay as they are.

How MERK-01 settled the four, in `merk::store::lignin`:

- The location is kept by the store, not in `Link`: a table in the handle, from a node's key to where its cell was last seen (a `lignin_tree::node::NodeHandle`). `Link` and `merk::tree` are as they were. PERF-01 changes what a node in memory is, and can move the location into the link then.
- A read of a node fills each link's key from the child's cell, because a cell holds only the length of that key (`tree-page.md` TP10). The child's cell is therefore seen with its parent, and its location goes into the table. `fetch_node(link)` looks the link's key up in the table and reads through the location (`Scope::follow`).
- The check is the transaction's (PN8): a handle that is no longer good is not followed, and the node is found by its key.
- A written node's location comes back from the application of its write: for an immediate handle from its own commit, and for a deferred handle from `LigninBatch::apply`, whose report the handle takes in with `LigninNodeStore::take_locations`. A handle that does not take it in finds such a node by key the next time, and has its location again from that read.

A Merk that GroveDB opens for one batch and commits once never follows a link it wrote. The report matters for a Merk that is kept open and written again, which a cache of open Merks for a transaction would do (PERF-03).

### Threads

`Merk<S>` is `Send` whenever `S` is. A Merk holds its tree in a `Cell`, so it is not `Sync`: it can move between threads and be written from one at a time. `merk/src/store/tests.rs` proves this at compile time for every `S: Send` (`merk_is_send_when_its_node_store_is`), and for the in-memory test store with its change set and cursor, and moves a Merk to another thread and writes it there.

The trait does not require `Send`, because the RocksDB handle is not `Send` and cannot be made so in Merk. Both kinds of storage context hold a `&Transaction`, and a RocksDB transaction is `Send` but not `Sync`. A transactional context also holds `&StorageBatch`, which has a `RefCell`. The adapter adds nothing thread-bound of its own: its change set and its cursor are `Send` when the context's batch and iterator are (`the_adapter_is_send_when_the_storage_context_is` in `merk/src/store/kv.rs`), and for both RocksDB contexts they are (`the_change_sets_and_cursors_of_the_rocksdb_contexts_are_send`). A store whose handle is `Send`, as LigninDB's write scopes need (TXN-09, GROVE-12), gives a `Send` Merk.

What a `Send` bound on the trait would cost on RocksDB, for SEAM-04, which decides the bound for this seam and for `FlatStore`: `StorageBatch` would need a lock where it has a `RefCell`, taken on every staged write, and the RocksDB transaction would need a wrapper that is declared `Sync` in unsafe code, with "one thread at a time" as an invariant the compiler no longer checks. Nothing on RocksDB needs either: it allows one write scope at a time, on one thread. Code that moves a Merk between threads can ask for `S: NodeStore<'db> + Send` where it does so.

### Side records

A Merk also keeps aux records (read by key, written in a commit) and meta records (read and written at once, cached in the Merk). They are not nodes. They are operations of this trait for now because a Merk has one handle. `FlatStore` (SEAM-03) is where they belong: the RocksDB contexts already give out `aux()` and `meta()` handles that implement it. Once a Merk holds a handle that offers those, the four aux and meta operations here and the two aux operations of a change set can move to it; that is SEAM-04's to decide. MERK-05 puts them on LigninDB's flat keyspace.

## The RocksDB adapter

`merk::store::kv` implements `NodeStore<'db>` for every `S: StorageContext<'db>`. So `Merk<PrefixedRocksDbTransactionContext>` is the same type as before, GroveDB's code compiles unchanged, and so does every function that is generic over `S: StorageContext<'db>` and calls a Merk.

The adapter is an `impl`, not a wrapper type. A wrapper (`Merk<KvNodeStore<Context>>`) would have changed the Merk type that GroveDB names, and every place where GroveDB calls a storage context's own methods on `merk.storage`. The inventory counts 78 uses of `PrefixedRocksDbTransactionContext` in GroveDB. SEAM-04 and SEAM-07 change those sites. SEAM-02 left the adapter without a name, for the same reason: every storage context type is the handle already, and no caller has to write another type.

The blanket `impl` has a price, which Rust's coherence rules set. Next to it:

- A type that is a storage context cannot have another `NodeStore` implementation.
- Merk can implement `NodeStore` only for a type that Merk itself defines. For a type of another crate the compiler refuses the `impl` (E0119), because that crate could make the type a storage context later. SEAM-02 checked this with the compiler. So LigninDB's handle in `merk::store::lignin` has to be a type of Merk that wraps the engine's handle, and the handle type an `Engine` names for its subtrees has to be a storage context or such a type of Merk.
- No second blanket `impl` is possible, for example one for every `FlatStore`.

A named `KvNodeStore<S>` in place of the blanket `impl` would lift all three, at the cost of the change in GroveDB described above. `Engine` as SEAM-04's row describes it does not need that: its RocksDB subtree handle can be the context itself, and MERK-01 names a Merk type, `LigninNodeStore`. SEAM-04 decides it for good when it fixes the handle types.

Each operation is the storage call the code made before SEAM-01, with the same arguments, in the same order:

- A node fetch is one `get` and one decode. `get_node` is the body `TreeNode::get` had.
- A commit is one `new_batch`, the root key, the node puts and deletes in key order, the aux puts and deletes, and one `commit_batch`. `prepare_node` encodes the record where `MerkCommitter::write` encoded it. The kind is not used.
- `write_node` is the direct `put` chunk restore made.

The change set of a context is its `Batch`, wrapped in `KvChangeSet` so that the batch's own methods are not part of Merk's vocabulary.

### How the adapter is checked

The adapter keeps no state and has no rule of its own. What can go wrong in it is a storage call that is missing, made twice, made out of order or made with other arguments, an error that is wrapped differently, and a cost that is dropped or added. SEAM-02 checked each of these twice: once against the code before the seam, and with tests that stay.

**Against the code before the seam.** Two builds of the workspace were compared, the branch as it is and the branch with `merk/` as it was before SEAM-01 (the pinned engine of the golden corpus). In both, the RocksDB contexts, their batches and their cursors recorded every call made on them with its prefix, key, value, sizes and cost information, the result and the cost it returned, and so did the application of a storage batch with every entry it held (GM0). The calls of each test thread were reduced to one digest.

| Run | Compared | Result |
| --- | --- | --- |
| Golden corpus, standard scale as it was then (290 traces and 216,069 steps; it has 430 and 235,858 since), one trace at a time | 14,310,752 storage calls | one digest, equal |
| GroveDB suite (3,885 tests, all features) | 3,135 tests that reach storage, and 9 threads that are doc tests or are started by a test | 3,133 tests and the 9 threads equal, 4,889,177 calls. The other two tests draw random keys or a random seed and give another digest on every run of one build. |
| Merk suite, the 843 tests that existed before the seam | 296 tests that reach storage, and one thread a test starts | 292 tests and the thread equal, 448,406 calls. In the other four the test reads a record itself, and SEAM-01 changed that read from `get` to a cursor; every other call in them is equal. |
| Failure injection: twelve Merk workloads on two grove versions, on an in-memory storage context that makes one call fail, for every call in turn, as an error with a cost, as a record that does not decode and as a missing record | 46,636 runs, each with every storage call, its arguments and what it returned, the result of every Merk operation with its error and cost, and the records left behind | one digest, equal |

The last row covers commits, reads, proofs, cursors, the side records, the root key, `clear`, layered Merks, elements and chunk restore. The comparison has to be made again by hand when the storage calls are meant to change; the tests below are what holds them in between.

**With tests that stay.** `merk/src/store/kv.rs` runs the adapter on a storage context that records every call (`merk/src/store/recording.rs`). It wraps a RocksDB context, passes each call on, adds one hash node call to every cost a call returns, adds up the costs returned, and fails a call when told to. No storage call charges a hash node call, so a cost the adapter dropped or counted twice shows in the total.

| Test | What it holds |
| --- | --- |
| `every_operation_is_one_storage_call_and_costs_what_that_call_costs` | On a transactional and on an immediate context: each operation of the trait makes the one storage call of the table in [Operations](#operations), with the same key and value, and returns the cost that call returned, all of it and once. A fetched node carries the cost its value defines. A link whose node is missing is `KeyNotFoundError` and is charged as the lookup. |
| `every_change_is_one_batch_call_under_its_key_whatever_its_kind` | Each change is one call on the context's batch, in the order the changes were made, with the feature and child sizes and the cost information it was given. |
| `a_storage_call_that_fails_is_a_storage_error_with_the_cost_of_that_call` | A storage call that fails ends the operation as `Error::StorageError` with that call's error and that call's cost, and no other call follows it. |
| `a_put_the_batch_refuses_is_a_costs_error` | The batch of an immediate context refuses a put whose cost information does not verify. That is `Error::CostsError`, for a node, the root key and an aux record, and nothing of the refused put is written. The batch of a transactional context takes every put and leaves that check to the application of the storage batch. |
| `an_undecodable_record_is_an_encoding_error`, `a_direct_element_read_reports_an_undecodable_record_as_before` | A record that does not decode is `Error::EdError`, charged as the read, and a direct element read still reports it as `CorruptedData` with the same text. |
| `a_direct_element_read_reports_a_failed_read_as_a_storage_error` | A direct element read whose storage call fails is `Error::StorageError` with that call's error and that call's cost, under the first and the latest grove version, and the same read is `None` once the call succeeds. |
| `a_merk_makes_its_storage_calls_in_the_order_it_always_made_them` | The calls of a Merk on a storage context, by name, key and order (since MERK-05 with the `status()` a walk asks at its end, which is no storage call). For a base Merk: open, commit (the root key, then nodes in key order, then aux records), reads, the side records, the root key, `is_empty_tree`, `clear`, and a commit of an empty tree. For a layered Merk: open and commit, which read and write no root key, and `is_empty_tree_except`. The lists were read off the Merk before the seam. |
| `a_chunk_restore_writes_uncharged_and_corrects_a_parent_with_a_direct_put` | Chunk restore of a plain tree: one batch of uncharged puts per chunk, the root key put directly after the first chunk, a parent read and put directly after each later one, and one batch for the corrected heights. The corrected aggregates of `finalize_with_grovedb_elements` are held by the comparison above and by the restore tests, not by this one. |
| `what_a_storage_batch_holds_is_read_once_the_batch_is_applied` | On a transactional context nothing written through the adapter, in a change set or at once, is seen by a read or a cursor before the storage batch is applied to the transaction. This is the storage context's rule; the test holds that the adapter keeps nothing that would answer earlier. |
| `a_read_is_answered_by_the_transaction_when_it_is_made` | RD3, RD5 and RD6 at the seam: a fetch returns the transaction's own node, a node committed by another transaction as soon as it is committed, and nothing of an uncommitted one. A cursor opened before that commit does not show it and the next cursor does. These are RocksDB's rules; the test holds that the adapter pins no view and keeps no node. |
| `the_cursor_and_the_batch_are_the_storage_contexts_own` | The cursor of the adapter is the context's raw iterator and its change set holds the context's batch. This is checked by the compiler. |

## The LigninDB store

`merk::store::lignin` (MERK-01, behind the Cargo feature `lignin_storage`) implements `NodeStore<'db>` for `LigninNodeStore<'db>`, a type of Merk that reads and writes through a write transaction of `lignindb::txn`. Its module documentation says what each part does. This section says how it meets the contract above and which choices it made.

### Handles

A handle is of one subtree, named by its 32 bytes, opened on a view (`LigninView`) in one of three modes.

| Handle | Reads | Writes |
| --- | --- | --- |
| deferred (`LigninNodeStore::deferred`) | its view, and nothing that is staged | staged in the `LigninBatch` it was opened with; refused with no batch |
| immediate (`LigninNodeStore::immediate`) | its view, with its own writes | applied at once; each commit is a write of its own |
| read-only (`LigninNodeStore::read_only`) | its view | refused |

| View | Reads | Writes |
| --- | --- | --- |
| a write transaction (`&WriteTxn`) | the transaction from outside every scope: its own writes, and otherwise the latest committed state at each read (`transactions.md` RD3). A read of a unit that an open scope holds for writing is refused (SC3) | through a scope that is opened for the one write and merged when it is made, or rolled back if the transaction refuses the write. No scope is open between two writes |
| a writer (`&LigninWriter`) | as its scope reads | through its scope, which lives on |
| a snapshot (`&LigninSnapshot`) | the one version it pinned (RD1), each read in an operation of its own. A subtree is looked up in the directory by its first read, or by each of two first reads that race, and its entry is kept | a write that reaches the view is refused with `SnapshotReadOnlyTransaction`: the write of an immediate handle, and the application of a batch. A deferred handle stages into its batch whatever its view is, as a storage context of RocksDB does, and the batch is refused where it is applied to the snapshot |

A `LigninWriter` holds the write scope of one writer. A scope is one thread's at a time and is not `Sync`, and a handle has to be `Send`, so the writer keeps the scope behind a lock that each read and each application takes. A writer is meant for one thread at a time, and the lock is not waited for then; handles of one writer on two threads take it in turn. `LigninNodeStore`, a `Merk` on it, `LigninChangeSet`, `LigninBatch`, `LigninSettled`, `LigninWriter` and `LigninSnapshot` are `Send`, asserted at compile time in `merk/src/store/lignin/mod.rs`.

What the store asks of a write transaction is in one file, `merk/src/store/lignin/access.rs`: the reads of a node view, the batch a change set becomes, and the report of its application. What it asks of a snapshot is in `view.rs`, and the flat records of the three views are in `flat.rs`. The rest of the store reaches a view through the trait `Access`, as a trait object, which is what lets a handle have the one lifetime that `Engine::Subtree<'a>` has.

### Reads

- `get_node`, `get_node_for_read` and `fetch_node` read the node's cell, then the cell of each child for its key, and build a `TreeNode` with the value. That is up to three operations of the transaction, and a commit can land between two of them. The transaction then answers that the node's location is no longer good, and the node is read again, up to eight times. A link that leads to no node is an error.
- `has_node` reads the cell and no child.
- `fetch_node_without_value` makes the same reads of cells and copies no value: the node it builds has the key, the two hashes, the feature and the links, and is marked as having no value. `load_node_value` then reads the node's cell again, through the location the fetch left in the handle, and copies the value if the cell still has the value hash and the key-value hash of the node that was fetched. A node that was written or removed in between is refused, as `Error::StorageError`.
- A read is charged by `StorageCostModel::read` with the record length of the node (`cost-model.md` N1), which is measured from the cell: the value's length, the feature, and for each link the length of the child's key and the aggregate (`cost_model::node_record_len_from_sizes`). It is charged once for the operation, however many cells the transaction read for it, and the same whether or not the value was read. An immediate handle charges `read_immediate`. A read that failed is charged as one that found nothing. `load_node_value` is charged nothing.
- A read the transaction did not perform is `Error::StorageError`. A cell whose feature or aggregate is no feature of Merk's is `Error::CorruptedData`, as a record that does not decode is an encoding error on RocksDB.

### The cursor

`LigninCursor` (MERK-05, `merk/src/store/lignin/cursor.rs`) is the store's `NodeCursor`. Its module documentation is the contract; this is how it is made.

- **What it shows.** The nodes of the Merk in key order, as a cursor of the transaction shows them (`lignindb::txn::MerkCursor`, RD6): the committed nodes of the moment the cursor was opened, which it keeps pinned, merged with the transaction's own writes as they are at each call. A commit that lands after it was opened is not seen by it and is seen by the next cursor. Nothing that is staged in a `LigninBatch` is shown, as no read shows it.
- **It owns its place and borrows no scope.** A cursor of a scope borrows the scope, and the handles of a writer share their scope behind the writer's lock. So the store's cursor holds a parked cursor (`Scope::parked_merk_cursor`): its version pin, its lease and the key it stands on, and no page. Each move takes the writer's lock, resumes the cursor through the scope (`Scope::resume`, which gives the cursor's calls for the length of that borrow and never the cursor itself) and lets go. The lock order is always the writer's lock and then the transaction's. A handle that only reads holds a cursor of its transaction, which needs no lock. The cursor is `Send`, and a Merk can write through the same writer while its cursor is open.
- **A value is a record that is built when it is asked for.** A move copies the key it comes to and reads no node. `value()` reads the node once (`MerkCursor::node_with_children`) and encodes Merk's record of it: the cell's value, feature, hashes and links, and the key of each child, which a cell does not hold (`tree-page.md` TP10) and which is read from the child's cell in the same operation, for a committed node at the cursor's version. So a walk over keys, which is what `Merk::clear` and the emptiness checks make, reads no value and no child; a walk over values reads up to three cells for a node (the node's page, and a page for each child that is not in it). A node the transaction serves as a record of its write set names its children itself, and no cell is read for them. The record is read once for a place: a write to the key after `value()` was asked shows after the next move.
- **Costs** are those of a cursor of a storage context (`cost-model.md` C1 to C3): one seek for a move, the stored key's length for `key()` and `valid()`, 288 off a record, and for `value()` also the record's length. They are the same on a deferred and on an immediate handle.
- **Errors.** A move the transaction refuses leaves the cursor on no key with `status()` an error, until the cursor is sought again; a step from no key goes nowhere. A record that cannot be read leaves the cursor on its key with no value, and `status()` an error. A node with a stale hash ([Hashes](#hashes)) is such a record: a read of it by key is refused, and the cursor refuses it the same way, charged as `key()` is. A walk that reads keys only reads no hash and passes it.
- **A write to the key it stands on.** The cursor stands on a key, not on a record (`lignindb::txn`, choice 7): after the transaction removed the node, `key()` is still the key, `value()` is `None` and is charged as `key()` is, and a step goes on from the key.

What a cursor holds while it is open is a version pin, not a page. A pin keeps the versions from it on from being reclaimed, so a cursor that a caller keeps for long holds reclaiming back for as long: a Merk gives its cursors to `KVIterator` and to the pinned-view reads, which live for one operation of GroveDB.

`Merk::clear` on this store walks a cursor and removes each node by its key, in a change set that states the root as gone. A clear of a Merk with no node removes nothing, and such a change set hands the transaction nothing. The store passes no clear of a subtree on to the transaction (see the next section). The aux and meta records of the Merk, and the record of a base Merk's root key, stay, as on RocksDB.

### Writes

- `prepare_node` copies the node's parts as a cell has them: the feature and each link's aggregate as the bytes of `tree-page.md` TP12 (`merk/src/store/lignin/feature.rs`), the two hashes, the links by child key, and the value only for `ValueWritten`.
- Every `put_node` and every `delete_node` is one node of the engine's change set, under its key (ST6). A `HashOnly` or `StructureChanged` node says that it keeps its value (`ValuePut::Keep`).
- **A value is written only when it changed.** A `ValueWritten` node states its value. When the write is applied, the stated bytes are compared with the bytes the transaction holds under that key, and equal bytes are kept where they are. The bytes are compared, never a hash. Merk reports the tree element of every Merk on the way up as written on every write below it, and such a put now hands the transaction no value. What the put is charged does not change: the cost rules charge the length of the whole record, as on RocksDB. The comparison is one read for each stated value, made in the operation before the application. `LigninApplied` counts the values written and kept.
- The root is what `state_root` stated, and the root that was otherwise. A change set that states a root and writes and removes no node, which is the commit of an empty Merk that stays empty, hands the transaction no change set: a root changes only with a node.
- The root key of a Merk with no parent element, the aux records and the meta records are flat records of the subtree in the columns of those names.
- A deferred handle charges a staged write nothing. `LigninBatch::apply` charges as the application of a storage batch does (`cost-model.md` B1): the keyspaces in `batch_order()`, each by stored key, a delete with no cost from the committed record, and the storage cost only if the transaction took the writes. An immediate handle charges each change as it joins the change set (M2). [Section 15 of cost-model.md](cost-model.md#15-the-lignindb-store-merk-03) has the call of the cost model for each event, and how the charges are checked (MERK-03).

### The batch of an operation

`LigninBatch` holds what the deferred handles of one or more operations wrote. It is a `StagedBatch` of the engine (`lignindb::batch`, TXN-08) whose calls attach what the write is charged by: every put and delete of a node, every put and delete of a record, the root a commit states and what an element hosts are the calls of that batch, and it holds one outcome for each key by the rule of a storage batch (`transactions.md` GM0). So:

- Inside one operation a put stands against a delete of its key, and of two deletes the first stands. `LigninBatch::next_operation` marks the boundary between two operations, and across it the later one stands. `take_pending` and `merge_overwriting` are the storage batch's. Which subtrees the batch writes into, and by which handle, is kept under one lock with the writes, so a write staged on another thread while the batch is taken goes whole into one of the two.
- **A Merk may be written by more than one change set of a batch.** Each node has the outcome a storage batch would hold, and is charged once, for the call that stands.
- A node that keeps its value keeps the one an earlier put of the batch stated, and with none the transaction's.

A handle is opened with the element that hosts its Merk, if one does (`LigninSubtree::host`): the host's subtree and key, and every Merk that element hosts by position. A change set that writes nodes says it of that element to the engine's batch (`StagedBatch::host`). When the batch is resolved, a node that the batch puts names the Merks its element was said to host (`Hosted::Set`), so that its cell points at their roots, and a Merk whose host the batch does not put is `anchored`. So is a hosted Merk that an immediate handle commits by itself.

A batch also says what its operation does to whole subtrees, which the transaction keeps in its subtree directory:

| Call | What it says |
| --- | --- |
| `LigninBatch::enter(subtree, tree_type)` | The subtree is there from this batch on, with no node. Its tree type is bytes the directory keeps. Its host is `LigninSubtree::host`, and its position among the subtrees of that element is the position of its prefix in `LigninHost::hosted`. |
| `LigninBatch::drop_subtree(subtree)` | The subtree goes with every subtree below it. The outermost one is named. The batch deletes the element that hosted it or writes it without the subtree, and `LigninSubtree::host` says which subtrees that element hosts afterwards. |

A batch is applied in two steps, which the partial batch of GroveDB needs apart (`engine.md`, "The batch across subtrees"):

1. `LigninBatch::settle(committed)` takes everything staged out of the batch, resolves it, and works out its charge (`LigninSettled`). What is owed at once is returned: the seeks, and for a delete that came with no cost the bytes of the record that the committed state holds, read through the view it is given. The storage cost is held back (`LigninSettled::pending_cost`).
2. `LigninSettled::apply(parts, view, enters)` hands the settled batches to the transaction, one operation each and in their order, through one scope: all of them or none. The storage cost is charged if the transaction took them. `enters` are subtrees that are entered with the first of them, among the subtrees that batch enters itself and each after the subtree its host is in; with no batch there is none to enter them with. A settled batch says what whoever applies it needs to work them out (`engine.md`, "Subtrees are entered by the first batch that writes into them"): `LigninSettled::written` is the subtrees it holds a write for once it is resolved, each once and with the host its handle named, where of two handles of one subtree the one that names a host is kept, and a handle by prefix names the host only of a subtree that is entered; `enters` says whether it enters a subtree itself; and `node` is what it holds for a key of a Merk, a put with the value it states or a removal (`LigninOutcome`). Through a writer's scope one settled batch is applied, and more are refused with `Error::NotSupported`: the scope lives on and could not take a part of them back.

`LigninBatch::apply(view)` is both steps for one batch that enters what it names itself. The batch is empty after `settle`, and a handle that stays open stages its next writes in it.

The transaction reads nothing under a subtree that is not entered, and refuses a change set and a side record for one. It applies the drops of a batch first, then its enters, then its change sets. So a Merk whose subtree the batch enters has no node before the write, whatever the transaction holds under the prefix until then: every value stated for it is written with no comparison. A location that a handle kept of a node which went is a hint the transaction finds stale. A deferred handle sees a staged drop no more than a staged write, so a Merk that is new under such a prefix is opened with no root.

Every `LigninHost` that names one element in one batch gives the same list, which is what the element hosts once the batch is applied. The list has no gap, so an element loses the subtrees it hosts from the last one or all together, as the elements of GroveDB do. The transaction also offers the clear of a subtree. The store does not pass one on: what a clear takes is GROVE-05's to state, and until then a Merk loses its nodes by key.

### Flat records

A handle is also the `FlatStore` of its subtree's data keyspace, for a tree that keeps no Merk there, and hands out its aux and meta keyspaces (`LigninNodeStore::aux`, `meta`, each a `LigninKeyspace`). A flat record is a record of its subtree's column in the transaction (`transactions.md` ST1):

- `get` is a point read of the view, charged as a read of a storage context (`cost-model.md` R1, R2).
- `put` and `delete` are staged in the batch of a deferred handle, with the writer's cost, and made at once on an immediate one, at the charge of a direct write (M1). A put into the data keyspace completes the cost of a new key, as a put of a node does.
- `clear` is the walk of the trait over the keyspace's records, at its cost, and fails with the cursor's error where the walk was refused or failed on its way: it has not come to every record then. On the data keyspace of a handle it is refused with `Error::NotSupported` where the subtree's Merk has a node as the handle's view has it: the nodes of a Merk are no flat records, the walk would pass them, and the clear would remove nothing and say that it did. A subtree whose Merk has no node, one that keeps its data flat and one that is not entered are walked.
- `cursor` (`LigninFlatCursor`) walks the records of the column in key order: a cursor of the write transaction, which keeps the committed state of the moment it was opened and shows the transaction's own writes (RD6), or a cursor of the snapshot. It is charged as a cursor of a storage context (C1 to C3). **The cursor of a writer's handle keeps no cursor of the transaction**: a cursor of a scope borrows the scope, which the handles of a writer share behind a lock, and the transaction offers no parked cursor over flat records. It keeps the key it stands on, and each move seeks to that key in a cursor of the scope and steps from there, so it reads the committed state of each move, as a point read of the scope does, and not that of the moment it was opened. A cursor that could not be opened, or whose move failed, stands on no record, and its `status` gives the error with its class where that is a refusal of a write scope or a limit of the engine.

### Opening a Merk

MERK-04. `Merk::open_base`, `open_standalone` and `open_layered_with_root_key` are the same code on every store, and on LigninDB they need no column of RocksDB: the store answers them from the write transaction.

| Open | Where the root key comes from | Where the tree type comes from | Charged |
| --- | --- | --- | --- |
| `open_layered_with_root_key` | the caller, who read it in the parent element | the caller | the fetch of the root node (R1), nothing for a Merk with no node |
| `open_base`, `open_standalone` | the record of the root key, a flat record of the subtree in the keyspace `roots` (`NodeStore::root_key`) | the caller | the read of that record (R1, with the key's length), then the fetch of the root node |
| `Merk::open_from_directory` (LigninDB only) | the subtree directory | the subtree directory | what the open of the same Merk by one of the two rows above is charged |

**The subtree directory holds the root of every Merk.** `Merk::commit` states the root in every change set (`state_root`), and the transaction keeps the Merk's entry by it: the key of the root node, and where its cell is. A node is read by descending from that cell, so the root node is found with no search, by the key the parent element holds or the key the record holds. The entry also holds the tree type the subtree was entered with and the element that hosts it.

**`LigninNodeStore::entry`** reads the entry of the handle's subtree (`WriteTxn::subtree`, which MERK-04 added to `lignindb::txn`). It is one lookup in the directory: in the directory of the transaction's frames, or for a transaction that has written nothing in the committed one. A transaction that a commit has overlapped has no frames, and answers from its write set over the committed entry, with one more lookup for each subtree it dropped or cleared; so does a scope that was opened beside another, whose writes are in the write set and not yet in the frames (`lignindb::scope`). The entry of a Merk is of the Merk's unit, so the read claims the subtree's data column as a read of a node does: it is refused where another open scope writes that Merk, or has dropped or cleared a subtree above it. It is the transaction's view, as every read of a handle is: its own enters, drops, clears and roots are in it, a staged enter is not, and a subtree that is not entered, or that went with a drop above it, has none. The read has no counterpart in the cost rules and is charged nothing.

**`Merk::open_from_directory`** opens a Merk with nothing but its prefix known, which is the open by prefix of the plan ("root key and tree type come from the parent element through the directory"). A Merk that an element hosts is opened as a layered Merk with the root key the directory names. A Merk with no host is opened as a base Merk, from its record, and the record and the directory have to name the same root, or the open fails with `CorruptedState`. A subtree that is not entered gives no Merk. The open is charged what the open it turns into is charged, so a caller that charges the read of the parent element, as GroveDB does on existing GroveVersions, charges it as before.

**The base root lives in the flat keyspace.** The root key of a Merk with no parent element is one record, `ROOT_KEY_KEY`, among the flat records of its subtree, in the keyspace the transaction calls `roots`: `put_root_key`, `delete_root_key` and `set_root_key` write it, with the charges of `cost-model.md` W3 and D1, and it is a key of the transaction's write set as it is on RocksDB (`transactions.md` 14.1). The directory names the same node after every commit of the Merk. The record alone does not make a root: a node is found from the directory's root, so a record written with no commit of the Merk (`Merk::set_base_root_key` after a chunk of a restore) names a node the store cannot reach until the root is stated. Restore is MERK-07's.

**Entering.** The transaction reads nothing of a subtree that has no entry and refuses every write to one. So a Merk opened where no subtree is entered is empty, at the cost the open has on RocksDB, and its first commit is refused unless the batch enters the subtree. A subtree under a parent element is entered by the batch that writes the element (`LigninBatch::enter`, or `LigninBatch::enter_tree` with a `TreeType`). A Merk with no host, the root Merk of a grove or a Merk that stands alone, has no element to enter it: whoever makes it enters it through its handle, `LigninNodeStore::enter(tree_type)`, staged in the batch of a deferred handle and made at once on an immediate one. The same batch may hold the Merk's first commit, because a batch applies its enters before its change sets and its records. An enter is charged nothing.

**The tree type in the directory** is bytes to the engine. The store writes the tree type's discriminant, followed by the parameter of a tree type that has one (`tree_type_bytes`, `tree_type_from_bytes`). A subtree entered with other bytes can be read and written, and is refused by `open_from_directory` as `UnknownTreeType`.

**A Merk too deep for the directory.** An entry holds the ids of the Merks above it, and the directory refuses a Merk more than 65,535 Merks deep (D-079). Where a write through the store is refused for it, the store reports `Error::NotSupported`: it is the one failure of the engine that is not `Error::StorageError`, so that a caller tells a limit that a write met from a fault of the store. No read returns it, so the rule of [Errors](#errors) for failed reads stands. The store sees the refusal only where the transaction applies the enter to its frames. A transaction that a commit has overlapped takes the enter into its write set unchecked, and the refusal then comes from its commit, as `TxnError::Directory(TooDeep)`: mapping that one is for whoever calls the commit (GROVE-01). No test enters 65,536 Merks one inside the other, which would take entries of 17 GB in all; the test holds the mapping of the engine's error.

### Proofs

MERK-06. A proof is made by a walk of the Merk's tree (`RefWalker`, `merk::proofs`), which fetches the nodes that are not in memory. On a store that keeps one record a node, every fetch reads the value with the node, and the walk keeps the node in the Merk's tree. Most nodes of a proof give it a hash and no value: the nodes on the way to a key, the two neighbours of a key that is not there, the nodes that the limit of a query proof hides, the roots of the subtrees a chunk ends at. On LigninDB the value of such a node is not read. One proof reads values it does not emit: a count-offset proof reads the value of every row of its range that it gives by its key, whether it returns the row or gives its digest, and of no row in a subtree that it gives as one hash (the table below).

**The walk says that it reads values on demand** (`RefWalker::values_on_demand`). Every proof generator of `merk::proofs` says so before it walks: the query proof (`create_proof`), the aggregate proofs of a count, a sum, a 128-bit sum and both together, the two count-offset proofs, and the trunk and the branch proof (`create_chunk`, `traverse_and_build_chunk`, and the two walks of `Merk::branch_query` that find the branch). Such a walk fetches a pruned child with `Fetch::fetch_without_value`, which `MerkSource` passes to `NodeStore::fetch_node_without_value`, and calls `RefWalker::load_value` before it reads the value of a node it came to. Where each generator does so:

| Proof | Where a value is read |
| --- | --- |
| a query proof | a node the query selects, once the limit has left it selected |
| an aggregate proof | nowhere: none of its nodes holds a value |
| a count-offset proof | every node of the range that the proof gives by its key, as a row it returns or as a digest: it reads the element to refuse a row that is a tree with nodes, as it did, and emits the value of the rows it returns. Not the nodes of a subtree that the offset or the limit hides whole: the proof gives one hash for it. Where a walk first checks every row of such a subtree (`ensure_every_row_is_a_unit`, for one the offset hides), it fetches the nodes and reads no value |
| a trunk or a branch proof | every node of the chunk down to its depth; not the nodes at the depth below, which give their hash, and not the nodes on the way to the root of a branch |

A walk that does not say so, and a walk on demand that is given a value-defined cost function at a node of the tree, fetches whole nodes with `Fetch::fetch` and keeps them in the tree, as before (below a node the walk owns, such a function is refused: choice 2 of MERK-06): the reads that keep the nodes they fetch (`observe_old_value`), the walk of every link, the heights of a restore, chunk production (MERK-07).

**On a store of whole nodes nothing changes.** The default of `fetch_node_without_value` is `fetch_node` with no value-defined cost, which is the call a proof walk made before. A node that comes with its value is put in the tree as `TreeNode::load` puts it, and `load_value` finds the value there. So on RocksDB a proof makes the storage calls it made, in the order it made them, and leaves the tree it left.

**A node that comes without its value is not kept in the Merk's tree.** The walker that came to it owns it (`RefWalker`), with every node the walk fetches below it, and drops them when the walk leaves the node: the link of the node above stays a `Link::Reference`. So no node with an empty value is ever in a Merk's tree, where a read or a commit would take the empty value for the node's. Debug builds assert in every accessor of a `TreeNode`'s value and in its encoder that the node has its value.

**What the Merk keeps instead is the key** (`PaidNodes`, `merk/src/merk/source.rs`). A Merk on RocksDB is charged for a node once: the walk of a proof keeps it in the tree, and what the Merk does afterwards reads memory. The same operations must cost the same on LigninDB, so the Merk remembers the keys of the nodes a walk was charged for and did not keep, and reads such a node again at no charge where the other Merk reads its tree:

| What follows on the same Merk | On RocksDB | On LigninDB |
| --- | --- | --- |
| a proof that comes to the node again | walks the node in memory | fetches it again without its value, at no charge |
| a write, or a read that keeps what it fetches, which comes to the node | takes the node from the tree, as the proof left it: fetched with no value-defined cost | fetches the whole node at no charge and with no value-defined cost, and keeps it in the tree from then on: the key is forgotten |
| `Merk::get` and its relatives, and `exists_by_traversing_tree`, which walk the nodes in memory | free while the walk stays on nodes in memory; a child that is not in memory ends it with a read by key, which is charged | the same walk over the remembered nodes, each read from the store at no charge, to the same three ends (`Merk::walk_paid_nodes`) |

The keys are forgotten where a Merk on RocksDB drops its tree: when the root is loaded again and when the Merk is cleared. One place is not mirrored: a commit prunes what is 100 levels below the root, and a Merk never has that many.

One difference remains, and no operation of GroveDB reaches it. A Merk on RocksDB holds a node as it was when the walk fetched it. A Merk on LigninDB reads it again when it is asked for, and so reads what the transaction holds then. The two differ only if the node was written through another handle while the Merk stayed open, which leaves a Merk on either engine with a tree that is not the stored one.

**The value-log reads of a proof** are those of the values the table above names: the values it emits, and for a count-offset proof also the value of every row of the range that it gives by its key, the rows it gives as a digest included. A value is in its cell, with the frame of its page until write-back has placed it, or in the value log (`lignin_tree::value`); a walk that copies no value reads none of the three. The test `a_point_proof_reads_no_value_from_the_value_log_but_the_targets` counts the reads of the files of the value log under a store whose values are all out of line and written back: a proof that a key is there reads one value, the key's, and a proof that a key is not there reads none.

**The root is read whole.** `Merk::open_layered_with_root_key` and `open_base` load the root node with `get_node`, value included, and the Merk holds it for as long as it is open. So the open of a Merk reads the root's value whether or not a proof then emits it. A proof through k Merks, as GroveDB makes one for a path, reads k root values besides the values the table above names. Not reading it needs a root that is held without its value, which is a node view in the Merk's tree (PERF-01).

### Hashes

`merk::store::lignin::hash` (MERK-02) is the bridge between the hashes Merk holds and the lazy hashing of the tree layer (`lignin_tree::hash`). Its module documentation has the rules in full. In short:

- **Stored as Merk holds them.** A cell stores the value hash, the key-value hash and the hash of each child, with the combined value hash of a reference and of a tree element as Merk made it. A write states them and a read returns them. The hash of a Merk's root is in no cell: it is the node hash of the root's cell.
- **`MerkHashRules`** is Merk's hash rules as the engine's `HashRules`: `kv_digest_to_kv_hash` for the key-value hash, and for the hash of a node the function `TreeNode::commit` uses for the link of a parent, by the node's feature, with the aggregate `TreeNode::aggregate_data` makes of the feature and the aggregates in the links. The value hash goes by the element the value is, read from its type bytes as Merk's proofs read it:

  | The value is | Its value hash |
  | --- | --- |
  | an item, a sum item, an item with a sum | `value_hash(value)` |
  | an item that takes backward references | `value_hash` of its serialization with the referrers emptied (#1059) |
  | a tree whose data is a Merk | `combine_hash(value_hash(value), root)`, with `NULL_HASH` for a Merk with no root |
  | an indexed tree | `combine_hash_three(value_hash(value), primary, secondary)`, or with `axes_digest` of the secondaries for a tree with a list of axes |
  | a reference of any kind; a tree whose data is no Merk | refused: its writer states it. Its cell hosts no Merk |
  | no element | refused |

  The hash of a tree is the one a write below it gives it, which is when it goes stale. The insert of the element itself is its writer's: `GROVE_V1` and `GROVE_V2` insert a count sum tree, a provable count tree and a provable count sum tree outside a batch as plain values (the insert names a count big sum tree too, which those versions refuse to create before it is reached), so such an element has `value_hash(value)` until something is written below it, and the combined hash afterwards, with the same bytes once it is empty again. `MerkHashRules::value_hash_at_insert` gives that first hash. A load or a migration of existing nodes therefore hands over the value hashes it has.

  The roots are those the cell hosts, by position: the primary Merk, then the secondaries in the order the element lists them, which is the order of `LigninHost::hosted`. A position the cell has no pointer for is a Merk with no root, as an empty pointer is: the cell of a tree whose Merk never had a node hosts none, and a cell points at the root of every Merk it hosts that has one (`tree-layout.md` C3). A cell that hosts more Merks than its element has is refused, so whoever writes another element over a tree drops the tree's Merks or names what the new element hosts. `MerkHashRules::for_check` answers a value hash that only its writer can make with the stored one, and so the hash of its insert for a tree with no root, whatever the version of the rules, since a cell does not say under which version its element was inserted. It is for `lignin_tree::hash::check` and never used for an evaluation.
- **The state of a link is the dirty flag of the child's hash.** `Reference`, `Loaded` and `Uncommitted` hold a hash, which is stated. `Modified` holds none: the hash is left to the engine (`NodePut::stale`), the link is written with the child's heights and with the aggregate `TreeNode::aggregate_data` makes of the child in memory, and where the cell had a link on that side its bytes are stated again, so that the write changes no hash. The child under a modified link is written in the same change set, as a commit of Merk writes it first: the engine makes the hash from the child's cell, so a change set that leaves the hash and does not write the child is refused. A value hash and a key-value hash are always held. On existing GroveVersions `TreeNode::commit` hashes every modified child before the node above is written, so no hash is left, nothing is marked and no rule runs at a commit.
- **A stale hash is not read as a hash.** A read of a node that has one is refused as `Error::StorageError`, since `Link` has no state for a child that is not in memory and whose hash is unknown. `has_node` answers for such a node.

How it is checked: `merk/src/store/lignin/hash/tests.rs` holds the rules against the hashes a commit of Merk makes, for every tree type and every kind of element. It writes a Merk that no commit of Merk has hashed through the store, has the commit of the engine make every hash that was left with `MerkHashRules`, and finds every committed hash, heights, aggregate and the root hash to be those of the same tree hashed by Merk. A Merk that is written by `Merk::commit` leaves no hash, and commits with rules that make none (`NoStaleHashes`). `lignin-testkit/tests/merk_store.rs` reads the hashes after a reload: after every commit of the corpus replay, write-back puts every page version into a segment, and the pages are loaded from there by a buffer pool that has never held them. Every cell that comes back stores the hashes the same cell has in the pool of the store, which the replay compared with RocksDB's records. Then every cell is checked with `lignin_tree::hash::check`; every hash the rules can make is overwritten in a copy of the pages and marked, and one `Hashes::evaluate_all` makes them all again, byte for byte the hashes RocksDB holds; and the hash the rules make of the grove's root is compared with GroveDB's root hash.

The stores of the tests of `merk/src/store/lignin` and of the replay give the physical invariant checker (`lignindb::verify`) the rules it checks their hashes by (`Db::check_hashes_by`). The checker runs when such a store is dropped, in a debug build, and in the replay after every commit as well. It visits every cell of a Merk in use and compares each hash the cell stores with what the rules make of it. A value hash that the rules take as stored, as below, is not recomputed, and the cells that a dropped Merk left are not hashed.

- **Where the values are elements** (`hash/tests.rs` and the replay), the rules are `MerkHashRules::for_check`, with the latest GroveVersion in the tests and the trace's in the replay. The version is how the rules read an element and, for an item that takes backward references, write it again without its referrers. No store of these tests holds elements written under more than one version, and one version would read them all the same: the encoding of an element is the same on every existing GroveVersion (`Element::serialize`). What does differ by version is the hash that the insert of a count sum tree, a provable count tree or a provable count sum tree gave it, and the check form takes either hash of such a tree while it has no root, whatever the version of the rules.
- **The store's other tests write values that are no elements**, so Merk's rule for the value hash of an element does not apply to them. Their stores are checked with Merk's rules for the key-value hash and the hash of a node, and with the hash of its bytes for a value (`Op::Put`). Where a test writes values with an operation that states their value hash, every value hash of its store is taken as stored (`Lignin::with_stated_value_hashes`).
- `the_check_of_a_dropped_store_recomputes_the_hashes_of_its_merks` commits a grove of items, a reference and a tree with its Merk, and plants a wrong hash in it, one at a time: a child's hash on either side, a key-value hash, the value hash of an item and that of a tree whose Merk has a root. With no rules the checker finds nothing. With Merk's rules it finds each once, where it is stored, and the drop of the store fails with the rule's name. The reference's value hash, which its writer stated, is taken as stored.

What MERK-02 left open:

1. **The commit takes the rules from its caller** (`lignindb::commit::Committer::commit`): `NoStaleHashes` on existing GroveVersions, `MerkHashRules` for a writer that leaves hashes. A commit whose rules refuse a stale hash has failed and its transaction is ended (CM6), so nothing may be left stale that the rules refuse.
2. **Nothing evaluates inside a scope.** A transaction that holds a stale hash cannot be read by Merk at that node before its commit. A writer that leaves hashes (iteration two) needs an evaluation it can ask for, with the caller's rules, or keeps what it wrote in memory.
3. **A Merk that holds other values than elements** has value hashes these rules do not know: they are for GroveDB's Merks.

### Choices MERK-01 made

1. **The location of a node is kept in the handle, by key**, and not in `Link` (see [Following a location](#following-a-location)).
2. **`NodeChangeSet::state_root`** is new on the seam (see [The root key](#the-root-key)).
3. **The comparison that keeps an unchanged value is made in the store**, one operation before the application. The place for it is the application itself, where the cell is at hand: PERF-03 can move it there.
4. **A batch holds one outcome for each key**, by the rule of the batch across subtrees (TXN-08, `lignindb::batch::StagedBatch`), for a node as for a side record: see [The batch of an operation](#the-batch-of-an-operation). An immediate handle makes its writes in the order they were given.
5. **The cursor was left to MERK-05** (see [The cursor](#the-cursor)). A cursor that lives across calls could not be taken from a `Send` handle as `lignindb::txn` stood, because `Scope::merk_cursor` borrows the scope; MERK-05 added the parked cursor for it.
6. **The store charges by the first version of the cost rules**, as the storage contexts do, with no grove version given to a handle. MERK-03 named that version `cost_model::HANDLE_COST_MODEL` and holds it to what every grove version charges by (`cost-model.md`, section 15.3).
7. **`lignindb` has a Cargo feature `testing`**, a test fixture and no API: its own test store with a stand-in commit, for tests outside the crate until the engine can be opened (TXN-07).

### Choices MERK-06 made

1. **The seam has two more reads**, `fetch_node_without_value` and `load_node_value`, both with a default, so the RocksDB adapter and the test stores are as they were. The first returns a `TreeNode` that is marked as having no value, and not another type: the walk of a proof reads `TreeNode`s, and PERF-01 replaces the type of what a read returns for every reader at once.
2. **A walk opts in** (`RefWalker::values_on_demand`). The fetch without a value could not be the walker's default: `observe_old_value`, the walk of every link and the heights of a restore read whole nodes through the same walker and keep them. A walk on demand that is given a value-defined cost function at a node of the tree fetches the whole node with it and keeps it, as before. Below a node the walk owns, where nothing is kept, the function is refused (`CorruptedCodeExecution`): the Merk could not hold the node as a store of whole nodes would have left it. No walk of a proof gives one.
3. **A node without its value is owned by the walk and never in the tree**, and the Merk keeps the keys it was charged for ([Proofs](#proofs)). The other way, a link state for a node that is in the tree without its value, changes `Link` and every match on it in `merk::tree`; PERF-01 can take it when the node in memory becomes a view.
4. **A value is read only under its own hashes.** `load_node_value` compares the value hash and the key-value hash of the cell with those of the node and refuses a node whose hashes changed since the fetch. On RocksDB a fetch is one read and cannot be split by a write; here it would otherwise emit a value under hashes it does not belong to. Bytes that no hash covers, the referrers of an item that takes backward references, are those of the moment the value is read; every proof node that carries such a value carries it with the list emptied (`hashed_value_bytes`), and the node of a restore chunk, which carries the list, is made by a walk of whole nodes.
5. **A count-offset proof still reads the value of every row of its range that it gives by its key**, because it refuses a row by what the element is. Telling a tree with nodes from the cell alone, by the Merks it hosts, would spare those reads and is not Merk's to know.
6. **The pinned-view read of a page** (`local_count_offset_page_from_view`) is no proof and reads records through a cursor, value included, as before. A cursor's value is the record (PERF-05).
7. **Chunk production is left a walk of whole nodes** (`traverse_and_build_chunk_with_features` with `for_restore`): every node of a chunk is emitted with its value but the nodes on the way to it. MERK-07 can make it a walk on demand with one call.

### Choices MERK-04 made

1. **"With no `roots` column" is read as: with no column of RocksDB.** The base root key stays a flat record of its subtree, in the flat keyspace that `lignindb::txn` names `roots`, as `transactions.md` ST1 and 14.1 have it and as MERK-01 built it. The directory's root is beside it and is the root a node is found from.
2. **`lignindb::txn` has one more read, `WriteTxn::subtree` (and `Scope::subtree`)**, which shows what the directory holds of a subtree (`SubtreeView`). The transaction had the entry and showed only where the root's cell is. It answers in each state of a transaction: from the frames' directory, and with no frames from the write set over the committed entry.
3. **A Merk with no host is entered through its handle**, by whoever makes it, and no open enters anything: an open is a read. GROVE-01 enters the root Merk of a grove when it creates a database.
4. **The open by the directory takes a Merk with no host for a base Merk.** The directory does not say whether such a Merk is a base Merk or stands alone. The two differ only in what a put of the root key record is charged, and a caller that wants the other opens it with `open_standalone` and the tree type of the entry.
5. **The tree type is two bytes at most**, the discriminant and a parameter. The directory has a write that changes the tree type of an entered Merk (`Directory::set_tree_type`) and the transaction offers none: what `Merk::convert_to_big_sum_tree` (#1063) needs of it is for the task that runs it on LigninDB.

### How it is checked

`merk/src/store/lignin/open_tests.rs` holds the opens, on a store on disk and beside RocksDB.

| Test | What it holds |
| --- | --- |
| `a_merk_with_no_parent_element_opens_as_it_does_on_rocksdb` | A base Merk and a Merk that stands alone, as a normal and as a sum tree, in one transaction and with a commit after every batch: opened where no subtree is entered, entered with its first batch, grown, given another root, emptied and written again. Every open, every `Merk::apply` and every application of a batch costs what it costs on RocksDB, and the root hash and root key are equal. After each batch the directory, the record of the root key and the open by the directory give the root the Merk has. |
| `a_merk_under_a_parent_element_opens_with_its_root_key_and_from_the_directory` | A sum tree under an element, written in one batch with the element: `open_layered_with_root_key` costs what it costs on RocksDB and gives the same root. The directory names the root key the element holds, the tree type and the host, and `open_from_directory` opens the same Merk at the same cost from a handle that names no host. A subtree that is not entered gives no Merk, and one entered with other bytes than a tree type's is refused. |
| `the_root_key_of_a_base_merk_is_a_flat_record_beside_the_directory` | The root key record is read back from the transaction's flat keyspace `roots`. A Merk with no host and no record is refused by the open by the directory. A rollback takes the enter, the directory's root and the record. |
| `a_merk_in_a_subtree_that_is_not_entered_is_empty_and_takes_no_write` | The open of a Merk where no subtree is entered, and the refusal of its commit. |
| `a_subtree_is_entered_through_its_handle` | `LigninNodeStore::enter` on each kind of handle, alone and under a host at its position, once, and what another transaction reads before and after the commit. |
| `every_tree_type_is_kept_in_the_directory_as_bytes_that_give_it_back` | Every tree type and parameter through `tree_type_bytes` and back, and through the directory. |
| `a_merk_that_is_too_deep_is_not_supported_and_no_storage_error` | The error of a Merk that is too deep. |

`lignindb/src/txn/tests/subtrees.rs`, `a_subtree_is_opened_through_the_directory_as_the_transaction_sees_it`, holds the read of the engine: a transaction with frames and one that serves its records by key make the same writes and are shown the same of every subtree after each. The writes are a root that changes, an enter, a clear with and without a change set, a write after a clear, a drop, a drop and an enter in one batch, a clear above subtrees that were entered and not written, and a drop of the subtree above. It ends with a subtree whose host left and was entered again under another element, and with a subtree that one batch enters with the subtree of its host. `a_transaction_with_no_frames_opens_the_subtrees_one_with_frames_opens` does the same at random: over 60 seeds, three transactions take the same enters under elements, writes, clears, drops, drops with an enter and rollbacks to a mark. One is met by a commit outside, and one writes through a scope that was opened beside another, so that its writes are not laid out. After each step each shows of every subtree what the run's own account of the grove has.

`merk/src/store/lignin/tests.rs` runs on a store on disk.

| Test | What it holds |
| --- | --- |
| `a_merk_on_lignindb_has_the_tree_it_has_on_rocksdb` | For twelve tree types and three ways of writing (a deferred handle with a batch, an immediate handle with a Merk that stays open, a transaction committed after every batch): random batches of puts, puts of the same bytes, references and deletes give the root hash and root key they give on RocksDB, and every node read back is, as a record, the record RocksDB holds. After each commit the subtree directory names the root key Merk has. On a deferred handle the Merk and the batch also charge what they charge on RocksDB. |
| `a_value_is_written_only_when_it_changed` | A value put again with the same bytes, in a cell or out of line, is not written, and other bytes of the same length are: the transaction's write set grows by the same nodes both times, and by the bytes of the values only the second time. |
| `every_node_merk_writes_is_one_write_of_its_key`, `a_commit_that_changes_nothing_writes_what_merk_writes` | The transaction's write set grows by exactly one write for each node Merk wrote or removed, whatever its kind. A commit that changes nothing writes the root of a Merk that has one, and for an empty Merk that keeps its own root key the delete of that record. |
| `a_link_is_followed_to_where_its_node_was_seen` | A walk down a tree looks no key up after the root, in a transaction's own frames and on committed pages, and a handle that takes in the report of a batch finds what it wrote without a lookup. |
| `a_host_names_the_merk_it_hosts_when_one_batch_writes_both` | A batch that enters a subtree and writes its Merk and its host names the Merk in the host, the engine refuses the same batch without the name, and a hosted Merk committed alone is anchored. |
| `a_merk_that_is_entered_with_no_node_is_written_by_a_later_batch` | A Merk is entered with no node by the batch that writes its host, gets its first nodes from a batch that does not write the host, and more from one that does. |
| `a_subtree_that_a_batch_drops_and_enters_again_is_written_whole` | A Merk under a subtree that one batch drops and enters again is written whole although the transaction held the same bytes under the same keys, and on a handle that had seen them. A batch that does not enter the subtree keeps them. Under a dropped subtree nothing is read, and a change set for it is refused. |
| `a_deferred_handle_reads_nothing_that_is_staged`, `the_root_key_and_the_side_records_are_records_of_their_subtree`, `a_node_written_outside_a_commit_leaves_the_root_where_it_is`, `a_committed_tree_is_read_and_written_by_the_next_transaction` | The write modes, the side records, `write_node` (after which the subtree directory still names the root the commit stated), and trees across commits and a rollback. |
| `a_handle_that_only_reads_refuses_every_write`, `a_node_whose_location_went_stale_between_two_reads_is_read_again` | The errors, and what each is charged. |
| `a_merk_on_lignindb_moves_to_another_thread_and_is_written_there` | A Merk on the store is sent to another thread and written there. |
| `merk/src/store/lignin/tests/cost.rs` (MERK-03) | What the store charges: a Merk on both engines on every grove version, each operation of the seam on handles of both engines, a delete with no cost, one seek for one fetch, the ledger of the cost model, a conversion to a 128-bit sum, a removal of a key that is no node, and a node that has no record length. `cost-model.md`, section 15.5, has the table. |

`merk/src/store/lignin/tests/cursor.rs` (MERK-05) makes the same calls on a Merk on the store and on a Merk on RocksDB, both holding the same committed nodes and the same uncommitted writes on top, and compares every answer and every cost, call by call.

| Test | What it holds |
| --- | --- |
| `a_cursor_shows_the_records_rocksdb_shows_and_charges_what_it_charges` | For twelve tree types, on a deferred and on an immediate handle, over nodes that are as committed, rewritten, made and removed by the transaction: a cursor that is sought and stepped at random (240 moves), full scans and range scans through `KVIterator` in both directions, and `is_empty_tree` show the keys and records RocksDB shows and charge what it charges. The walk itself names the edge each move came to, and the test holds that the walks came to all of them: a step to a record and off either end, a seek to a key, between two keys and beyond either end, each in both directions, and a record that was read once, twice and not at all. |
| `clear_removes_every_node_and_charges_what_it_charges_on_rocksdb` | For twelve tree types and both kinds of handle: `Merk::clear` over nodes that are committed, rewritten by the transaction and made by it charges what it charges on RocksDB, in the walk and in the batch, where a node removed with no cost is charged from its committed record (`cost-model.md` D2). Afterwards no node is read, the Merk opens empty, and the aux, meta and root key records are what RocksDB has. |
| `the_aux_and_meta_records_of_a_merk_are_what_they_are_on_rocksdb` | `get_aux`, aux puts and deletes in a commit, `get_meta`, `put_meta` and `delete_meta` answer and charge as on RocksDB, for records that are committed, written by the transaction, and removed with no cost. |
| `a_cleared_merk_has_no_root_and_is_written_again` | After a clear is committed the subtree directory names no root, the next transaction reads an empty Merk, and the Merk is written again. |
| `a_drop_takes_the_side_records_below_it_and_a_clear_keeps_those_of_its_merk` | Three Merks one inside the other, each with nodes and aux and meta records (D-083): a clear takes the nodes of its Merk and leaves its other records; a drop that names the middle subtree takes every record of it and of the one below; both subtrees are entered again under their prefixes and start with nothing, inside the transaction and for the next one after the commit. |
| `a_cursor_keeps_the_state_it_was_opened_on_and_sees_its_transactions_writes` | RD6 at the seam: a cursor does not see a commit that landed after it was opened, and the next cursor does; it sees its transaction's writes once they are applied and none that is staged; on the key of a node that was removed it shows no value and steps on, and a `KVIterator` passes over such a key. |
| `a_cursor_moves_to_another_thread_with_its_merk` | A cursor and a Merk of one writer are sent to another thread, where the Merk writes and the cursor reads the write. |
| `a_node_whose_hash_is_not_made_yet_has_no_record_under_the_cursor` | A node that is written with the hash of a child left to the engine has no record under the cursor: `value()` is `None` and is charged as `key()` is, `status()` gives the reason a read by key gives, until the next seek. The node beside it has its record, a walk over keys comes to its end, and a `KVIterator` gives the records before the node and says that it failed, in both directions. |
| `a_walk_that_fails_is_not_taken_for_the_end_of_the_nodes` | A cursor that cannot be opened, whose move fails after 0, 1, 7 or 20 moves, or whose record cannot be read: `status()` is the error, a `KVIterator` over two ranges gives the records before the failure and then says that it failed, the read of one node through a view is a storage error, `clear` fails with the error and stages nothing, and neither emptiness check answers "empty". A node whose child's key was not read with it is an error and no record. |
| `a_cursor_that_another_scopes_write_claim_refuses_is_no_empty_merk` | Two scopes of one transaction: the cursor of the first is refused where the second holds the Merk for writing (`TxnError::ScopeOverlap`), which `status()` shows; `clear` fails, the emptiness checks answer "not empty", and once the second scope has merged the same cursor walks the Merk with its writes. |
| `a_cursor_of_a_handle_that_only_reads_is_refused_beside_a_writing_scope` | The same for a handle that only reads, whose cursor is its transaction's. |

`merk/src/store/lignin/tests/proof.rs` (MERK-06) makes the same proofs on a Merk on the store and on a Merk on RocksDB, over the fixture of the cursor tests: nodes as committed, rewritten, made and removed by the transaction, and among them nodes whose values are the elements a proof tells apart (an item, a sum item, an item with a sum, three kinds of empty tree, a reference, an item that takes backward references, a value that is no element, an empty one), two of them out of line.

| Test | What it holds |
| --- | --- |
| `a_proof_is_the_bytes_rocksdb_gives_and_costs_what_it_costs_there` | For twelve tree types, on each of the five grove versions, on a deferred and on an immediate handle: one Merk on each engine makes the same calls one after the other, and each answers with the same bytes or the same error and is charged the same. The calls are query proofs (a key that is there, the same again, a key between two keys, below the first and above the last, two keys, ranges of each kind with and without a limit in both directions, each of the element nodes and all of them), the five aggregate proofs over three ranges, the count-offset proof and the local one with offsets and limits in both directions, trunk proofs with and without a least depth, branch proofs of a key that is there, again, and of one that is not, and between them the reads that a Merk answers from memory: `get` with and without the cache, `exists_by_traversing_tree`, `get_value_hash`, of keys on the way of a proof, of its target, of a key that is not there and of one far away. |
| `a_write_after_a_proof_costs_what_it_costs_on_rocksdb` | For twelve tree types and both kinds of handle: a proof, then writes of the Merk that made it (the proven node rewritten, its neighbour removed, a node made), with proofs and reads between them. The walk of each write goes over nodes the proof was charged for. |
| `a_point_proof_reads_no_value_from_the_value_log_but_the_targets` | The acceptance test. Under a file layer that counts the reads of the files in `val`, a Merk of 200 nodes whose values are all longer than a cell holds in line, committed and written back, on a handle that only reads, for twelve tree types: the open reads one value, the root's; the proof of a key reads one, the key's; the proof that a key is not there reads none; the proof of the root reads none; on the Merk that made it, the proof of the key reads the key's value again and no other, and the proof that a key is not there reads none. Each proof is the bytes RocksDB gives, at RocksDB's cost. |
| `a_proof_reads_the_values_it_emits_from_the_value_log` | The same count for a range of five rows, the first two rows of sixty, an aggregate proof (none), a trunk of three levels (its seven nodes, less the root), a branch of two levels (its three nodes and not the way to them) and a count-offset proof (the rows of its range that it gives by key, far fewer than the range holds). |
| `a_value_is_not_read_for_a_node_that_was_written_since` | The two reads of the seam on the store: the fetch without a value is charged what the whole fetch is and gives the same node but for the value; the value is read once, at no charge; a node that was rewritten or removed since is refused as a storage error and stays without a value; a link to no node is the error of a whole fetch at its charge. |
| `a_store_of_whole_nodes_fetches_whole_nodes_for_a_walk` | On RocksDB the fetch for a walk is the whole fetch, and the read of a value does nothing, or refuses a node that has none. |

`merk/src/tree/walk/ref_walker.rs` holds the walker with no engine, on a source that hands out the stored nodes of a tree whole or without values and keeps what it was asked: a walk keeps the whole nodes it fetches, on demand or not; a walk on demand keeps no node that came without its value, owns what it fetches below one, reads a value once where it is asked, and leaves the tree as it found it; a walk with a cost function fetches whole nodes at a node of the tree, and is refused below a node the walk owns; a proof whose value is refused fails at the charge of its fetches and leaves the tree as it was; and for every key of a tree the proof made from nodes without values is the proof made from whole nodes, at the same cost, with the same nodes fetched and one value read.

`merk/src/store/lignin/tests/views.rs` (GROVE-01) runs on a database of the engine's facade (`lignindb::Db`), and has the physical checker read it.

| Test | What it holds |
| --- | --- |
| `a_transaction_with_no_writer_of_its_own_writes_through_a_scope_for_each_write` | A handle on a write transaction: a batch and the commits of an immediate handle at the costs of RocksDB, no scope open between two writes, a refused write that leaves nothing, and a savepoint that takes the writes back. |
| `a_snapshot_reads_the_merk_of_its_version_and_refuses_every_write` | A handle on a snapshot reads the nodes, the records and the cursor of its version beside a later one, at the costs of the same reads through a transaction, and refuses the write of an immediate handle and the application of a batch. |
| `a_merk_that_two_operations_of_one_batch_write_is_charged_for_what_stands` | A Merk written by two change sets of one batch has the outcome and the cost of the storage batch. |
| `a_flat_keyspace_costs_what_it_costs_in_a_storage_context` | The data, aux and meta keyspaces of a deferred handle, of an immediate handle and of a writer's handle: every call, its answer and its cost against a storage context of RocksDB. |
| `a_clear_removes_flat_records_and_refuses_the_nodes_of_a_merk` | `FlatStore::clear` on a handle: refused for a Merk with a node, which keeps its nodes; the walk and its cost for a Merk with none, for flat records and for a subtree that is not entered; and the error of a directory that another scope holds. |
| `settled_batches_are_applied_together_or_not_at_all` | What a settled batch says of a key and of the subtrees it writes into, two settled batches of which the second is refused and nothing is kept, both applied with a subtree entered beside them, a subtree entered for a batch after the one that the batch enters itself, and a subtree whose writes a later operation took back. |
| `a_writers_flat_cursor_starts_each_move_from_the_key_it_stands_on` | The cursor of a writer's flat keyspace beside writes of its scope: a step from a key that went, a clear through the scope, a clear that another scope's claim refuses, by its class, and a writer that refuses a group of two settled batches as not supported. |

`lignindb/src/txn/tests/parked.rs` holds the two additions to the transaction: a parked cursor reads its version across later commits and adds no page pin; it is resumed by the scope that opened it and by no other, nor by any once that scope has ended, and a read through it is refused where another scope holds the Merk for writing; it asks for its subtree again after a drop and a clear; and the keys of a node's children are read at the cursor's version, also for a node that is found by its key after a commit outside gave the Merk another shape.

`lignin-testkit/tests/merk_store.rs` replays the golden corpus through the store. Where a step of a trace proves (a path query, several at once, a trunk or a branch chunk), `merk_store/proofs.rs` (MERK-06) has Merk make the proofs GroveDB asks of it on both engines, over the nodes the replay has handed over so far: on each Merk above the query's path the proof of the key that leads on, and on the Merk at the path the proof of the query's items, an aggregate proof for an item that asks for one, or the trunk or the branch. The bytes, the errors and the cost of every call are compared, the open of each Merk included. A call is not made where no Merk is at the path it names or what is there opens none; the replay counts those, and the whole corpus must come to more calls made than not, over ten thousand steps that prove and fifty thousand proofs. GroveDB does not run on LigninDB yet, so each trace runs on RocksDB, and what each operation did to the nodes of every Merk is handed to a LigninDB transaction as one batch: a drop for each tree the operation removed, an enter for each Merk it made, and the change sets, each Merk named in the element that hosts it. Every written node is read back and compared with RocksDB's record. At intervals and at the end of the trace the transaction is committed: the check of the committed tree pages walks every Merk from the root that its entry in the subtree directory names and must reach the keys RocksDB holds for it and no others; the cell of the element that hosts a Merk must point, at the Merk's position, at that root, and no cell at another; and every node of every Merk is read from the committed pages and compared. Half of the traces are also handed over with every node as `ValueWritten`, and the store then writes exactly the values whose bytes changed and the values of each Merk its operation made. Since MERK-03 the same change sets also go to a second RocksDB storage, each node with a cost of the kinds a writer gives or with none, and every batch costs on the store what it costs there, and every read of a node, each by itself, what it costs on RocksDB (`cost-model.md`, section 15.5).

## What later tasks get

| Task | What it builds on |
| --- | --- |
| SEAM-02 | The adapter exists and Merk runs on it. SEAM-02 showed that it makes the storage calls the code made before the seam and added the tests of [How the adapter is checked](#how-the-adapter-is-checked). |
| SEAM-04 | A subtree handle of an engine is a `NodeStore<'db>`; `Merk<E::..>` needs no other bound. The side records move to `FlatStore`. A RocksDB context is a `NodeStore` and a `FlatStore`, and both have a `cursor()`, which give the same raw iterator: code that has both traits in scope for a context, or is generic over a type bounded by both, names the trait (`NodeStore::cursor(&context)`), as it already does for `get`, `put` and `delete` of `StorageContext` and `FlatStore`. Generic code bounded by one of them is not affected. A `NodeStore` that is not a storage context is a type of Merk (see [The RocksDB adapter](#the-rocksdb-adapter)). |
| SEAM-05 | Every storage charge Merk sees passes through this trait's return values, and every size Merk supplies is in `NodeWriteCost` or a `KeyValueStorageCost`. The recording context of `merk/src/store/recording.rs` shows each storage call with the sizes and cost information it was given and adds up the costs the calls returned, so a test can hold those while the place of the charging moves. |
| SEAM-06 | `merk/src/store/mem.rs` is a second `NodeStore`, in memory, used by the tests here. It keeps of each written node only what its kind names and fails a commit whose result is not the node Merk wrote. It charges nothing, so it checks trees and proofs, not costs. |
| MERK-01 | `fetch_node` takes the link and is defined by its key (RD5). `prepare_node` gets the node and the kind, so a value is copied only for `ValueWritten`. Every `put_node` and `delete_node` is one write of its key (ST6). |
| MERK-02 | The kinds, and what they come from (`known_storage_cost` and the baseline for the value, `NodeMarks` for the children), are the dirty state of a node's parts. The dirty state of its hashes is the state of its links ([Hashes](#hashes)). |
| MERK-03 | The inputs of every charge are logical: `NodeWriteCost`, and the node itself for the record length (`cost_model::node_record_len`, its parts for a node with a modified link, and `node_record_len_from_sizes` for a node the store holds as a cell). Done: `cost-model.md`, section 15. |
| MERK-04 | `root_key` and the root key operations of a change set are the whole of root handling. Nothing in Merk names the `roots` column. Done: see [Opening a Merk](#opening-a-merk). |
| GROVE-01 | Done: the store is the handle of `LigninEngine` (`engine.md`, "The LigninDB engine"). A handle by prefix names its host from `LigninNodeStore::entry`, for a subtree that is entered and is the first its element hosts. The root Merk of a grove and the namespace of the pending-drop records are entered when the engine opens a database that does not have them. A Merk more than 65,535 Merks deep is `Error::NotSupported`. |
| MERK-05 | `cursor`, `Merk::clear` (cursor plus `delete_node`), `is_empty_tree`, the aux and meta operations. |
| MERK-06 | Proofs walk through `fetch_node_without_value` and read a value with `load_node_value` where they emit it; a count-offset proof also reads the value of the rows of its range that it gives as a digest. Done: see [Proofs](#proofs). |
| MERK-07 | Chunk restore writes through `prepare_node`, `put_node` and `write_node` with honest kinds: new nodes are `ValueWritten`, a corrected parent link is `StructureChanged`, corrected heights are `HashOnly`, corrected aggregates are `ValueWritten` (the feature type changes). Chunk production walks whole nodes; `RefWalker::values_on_demand` before the walk spares the values of the nodes on the way to a chunk. |
| GROVE-06 | A Merk proof reads from the value log the values it emits, and a count-offset proof also those of the rows of its range that it gives as a digest. GroveDB's own reads of a proven element (a reference it follows, a tree it descends into) are reads of nodes the proof emitted. The open of each Merk on the path reads its root whole. |
| PERF-01 | `get_node_for_read` is the read that already asks for less than a `TreeNode`. Node views replace its return type. With a view in the tree, the root need not be read whole, a node without its value can stay in the tree, and `PaidNodes` goes. |

## Call sites outside Merk

These are in GroveDB and `grovedb-query`. SEAM-04 and SEAM-07 change them. For each, this is the operation that serves it, or why it stays with the engine.

| Site | What it does today | Served by |
| --- | --- | --- |
| `has_raw` (`grovedb/src/operations/get/mod.rs`) | Opens a context by path and calls `get(key)`, to test whether a node record exists. Charges one seek and the record length. | `NodeStore::has_node(key)`, which SEAM-04 added: the adapter is `get(key)` and `is_some`. Same seek and same loaded bytes, and a record that does not decode is still `true`. `get_node_for_read(key)` would have made that record an error. Merk itself does not need the operation: `Merk::exists` decodes the node today. |
| The deletion visitor's raw delete (`delete_internal_on_transaction/v2.rs`) | `storage.delete(key, None)` on a subtree that is being deleted whole, without opening a Merk. | `NodeChangeSet::delete_node(key, None)` in a change set of the subtree, committed with `NodeStore::commit`. On a transactional context a direct delete and a committed batch part both stage the same delete in the shared batch. |
| The repair's removal of a row no tree links (`remove_secondary_rows_from_storage` in `grovedb/src/operations/indexed_tree.rs`, GroveDB #1063) | Upstream: `storage.new_batch()`, `delete(key, Some(cost))` for each row and `commit_batch`, on the secondary of an indexed tree. The rows are records under the secondary's prefix that no node of its Merk links: an earlier repair of a released version left them. The caller states the cost, from the length at which its scan read the record. | `NodeChangeSet::delete_node(key, Some(cost))` in a change set of the secondary, committed with `NodeStore::commit`. It is the one caller outside Merk that removes a node with a stated cost, and the one caller that names a key no link of the tree leads to. The scan that finds such a row is a cursor of the secondary (`KVIterator` on `NodeStore::cursor`), and what tells it from a linked one is `Merk::linked_keys`, a walk of the tree through `fetch_node`. The LigninDB store charges the removal as RocksDB does and has no such record to remove (`cost-model.md`, section 15.4, item 6; MERK-09). |
| `move_subtree_storage`, `move_namespace` (`grovedb/src/batch/move_element.rs`) | Copies every raw record of a subtree from one prefix to another through an iterator, and deletes the source. | **Engine.** It moves a whole namespace between two subtrees, and for LigninDB it is a change in the subtree directory, not a copy. A `NodeStore` handle is one subtree. It is an `Engine` operation ("move the subtree at prefix A to prefix B"), with the copy as RocksDB's implementation of it. The costs it charges per record today are part of BASE-06. |
| `snapshot_fetch_node` (`grovedb/src/operations/indexed_tree.rs`) | Seeks a pinned iterator to a key and decodes the record there. | The cursor: `NodeCursor::seek`, `key`, `value`, then `TreeNode::decode`, as now. It is the same read as Merk's own `PinnedViewSource`. What it needs from the engine is the pinned view itself, see the next row. |
| Retargeted pinned iterators (`PrefixedRocksDbRawIterator::retarget`, `grovedb/src/element/local_offset_read.rs`, `indexed_tree.rs`) | Takes one iterator, which pins one snapshot, and points it at another subtree's prefix, so that reads across several subtrees see one state. | **Engine.** One view across subtrees is not something a per-subtree handle can give. `Engine` hands out a pinned read view from which a cursor for any subtree can be taken; on LigninDB that is a snapshot. The cursor it yields is a `NodeCursor`, so the readers stay as they are. |
| `QueryItem::seek_for_iter`, `iter_is_valid_for_type` (`grovedb-query`) | Positions a cursor for a query range. | `NodeStore::cursor()`. The functions are generic over `RawIterator`, which is `NodeCursor`. No change is needed. |
| `Element::iterator`, `KVIterator::new(merk.storage.raw_iter(), ..)`, `ElementsIterator` (GroveDB) | Ordered walk of a subtree's raw records. | `NodeStore::cursor()` in place of `StorageContext::raw_iter()`. `KVIterator` takes any `NodeCursor`. |
| Replication `visit_nodes` (GroveDB) | Walks every record of a subtree. | `NodeStore::cursor()`. |

## Choices this task made

The plan left these open. Later tasks build on them.

1. **The adapter is a blanket `impl` for every `StorageContext`**, not a `KvNodeStore` type. Reason above. One consequence: a type that implements `StorageContext` cannot have another `NodeStore` implementation, and every other `NodeStore` implementation in Merk has to be for a type Merk defines (see [The RocksDB adapter](#the-rocksdb-adapter)). LigninDB's handle is not a storage context, and `LigninNodeStore` is a type of Merk.
2. **`NodeStore<'db>` has a lifetime parameter.** It is what lets one `impl` cover every `StorageContext<'db>`, and it names what cursors and change sets may borrow.
3. **The cursor trait is `grovedb_storage::RawIterator`**, re-exported as `NodeCursor`. Merk's own code names it only under that name.
4. **Heights and aggregate data count as hash, not structure.**
5. **A node write has two steps**, `prepare_node` and `put_node`, so that Merk can keep issuing writes in key order while each store decides what to keep of a node.
6. **Merk classifies soundly and not minimally.** The same-bytes put is left to MERK-01 and MERK-02.
7. **`Send` is a property of `Merk<S>` for a `Send` store, not a bound on the trait.** The RocksDB handle is not `Send`, so on RocksDB a Merk still stays on the thread that opened its context. This is narrower than the register's sentence and is reported to the owner as such.
8. **Aux and meta records are operations of `NodeStore` until SEAM-04.**
9. **`fetch_node` is defined by the link's key.** A location is a shortcut to the same node. The trait gives a store no way to pin a view for a handle.
10. **A written node's location comes back from the application of the batch, not through this trait.** A node is placed when its write is applied ([P5 of the tree layout](tree-layout.md), owner ruling D-034), so `prepare_node`, `put_node` and `commit` return what they returned, and `Engine::apply_batch` returns the addresses (`Applied`, SEAM-04). A link to a node the Merk wrote is followed by key until a Merk that stays open takes them (PERF-03). See [Following a location](#following-a-location).

SEAM-02 added two:

11. **The adapter stays a blanket `impl` with no name.** See [The RocksDB adapter](#the-rocksdb-adapter) for what that costs and when a `KvNodeStore` type would be needed.
12. **`merk/src/store/recording.rs` is a test tool of `merk::store`**, next to `mem.rs`: a storage context that records the calls made on the context it wraps and can fail one of them.
