# NodeStore: the seam between Merk and its engine

LigninDB task SEAM-01 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

`NodeStore` is the handle through which a Merk reads and writes the nodes of one subtree. It lives in `merk/src/store/mod.rs`. Since SEAM-01, Merk reaches storage only through it. The one implementation today is the adapter over the storage contexts of `grovedb-storage`, in `merk/src/store/kv.rs`. LigninDB adds its own in `merk::store::lignin` (MERK-01).

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
| `put_root_key(key, cost)`, `delete_root_key(cost)` | Records or removes the root key. | `Batch::put_root`, `Batch::delete_root` on `ROOT_KEY_KEY` |
| `put_aux_record(key, value, cost)`, `delete_aux_record(key, cost)` | Writes or removes an aux record. | `Batch::put_aux`, `Batch::delete_aux` |

## Contract

### Reads

- `get_node` and `get_node_for_read` return `None` for a key the subtree does not hold, and `has_node` returns `false`. They look a key up; they do not need a tree in memory.
- `fetch_node` is the read of a tree walk: Merk has a parent's `Link` and wants the child. Merk's `Fetch` implementation for a Merk, `MerkSource`, calls only this. Its answer is defined by the link's key: it is the node `get_node(link.key())` returns at that moment. A link whose key has no node is an error (`KeyNotFoundError("Key not found for fetch")`), not `None`, and the read is charged as a lookup that found nothing.
- The link is the whole argument of `fetch_node` so that a store can keep a location in it and follow that instead of searching (MERK-01, PERF-03). The location is a shortcut to the same answer, never another answer. See [Following a location](#following-a-location) for what that still needs.
- A node from `get_node` or `fetch_node` may be modified and committed. A node from `get_node_for_read` has no mutation baseline and must never reach a commit: a commit would take it for a new node.
- A read returns what the engine shows the handle under that key at the moment of the call. On RocksDB that is the transaction's view: its own writes, and otherwise the latest committed record. Writes staged in a storage batch that has not been applied to the transaction are not visible to reads, as today. A store answers no read from a node it kept from an earlier read. Merk keeps the nodes it has loaded in its own tree, above the seam, on every engine alike.

### The root key

A Merk that is not layered under a parent element (base and standalone Merks) keeps the key of its root node in its store. `root_key` reads it on open. `put_root_key` and `delete_root_key` change it as part of a commit. `set_root_key` changes it at once (`Merk::set_base_root_key`).

A layered Merk takes its root key from its parent element and does not use the record. On LigninDB the subtree directory names the root as well (MERK-04). Loading the root node is `get_node` with that key: no separate operation exists, because a store that knows where the root is answers that lookup without a search.

### Cursor

`cursor()` returns an unpositioned ordered cursor over the nodes of the subtree. Its trait is `NodeCursor`, a re-export of `grovedb_storage::RawIterator`: seek to first, to last, to a key, to the last key not after a key, step forward and back, and read the key, the value and validity. The same trait is kept for three reasons:

- `QueryItem::seek_for_iter` and `iter_is_valid_for_type` in `grovedb-query` position cursors through it, and `grovedb-query` sits below Merk.
- GroveDB iterates raw records through it in 41 places (`Element::iterator`, `ElementsIterator`, replication).
- `FlatStore` (SEAM-03) returns the same trait for its cursor.

A second cursor trait in Merk would have to be bridged to all three. The trait names no engine.

A cursor's value is the node record in Merk's node encoding, the bytes `TreeNode::encode` produces. This is the one place where the seam shows the record form. The reason is `KVIterator`, which hands records to its callers, and those callers (in GroveDB) decode them. A store that keeps no records builds the record for the node under the cursor (MERK-05). Moving those callers to node views is PERF-05.

A cursor shows the state the handle's reads show. How long it stays stable under later writes is the engine's rule (`transactions.md`, RD6): a RocksDB iterator pins the committed state at its creation.

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
| `ValueWritten` | The node has no mutation baseline (it was created, not loaded), or a value was put on it since it was loaded or last committed. |
| `StructureChanged` | Otherwise, when a child slot was emptied or filled since the node was loaded or last committed, and the tree walk could not show that the same child came back. |
| `HashOnly` | Otherwise. |

The node is written at all only because it, or a node below it, changed. So `HashOnly` is the common case: the ancestors of a changed node.

What makes this sound:

- **Value.** In a tree that is committed, a value, a feature type, a value hash or a key-value hash changes only in the `put_value` functions and the put of a moved node. On a loaded node each of them records a storage cost for the commit (`known_storage_cost`), and that is the mark the rule reads. Chunk restore changes nodes outside a commit and states its kinds itself. `take_node_change` asserts in debug builds that a node it does not report as `ValueWritten` still holds its baseline value.
- **Structure.** A node in memory carries two marks, `children_changed` and `on_loan` (`NodeMarks`). They sit in the boxed part of the node, encode to nothing and do not take part in equality, so a stored node and the size of a `TreeNode` are what they were.
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
| Golden corpus, standard scale (290 traces, 216,069 steps, one trace at a time) | 14,310,752 storage calls | one digest, equal |
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
| `a_merk_makes_its_storage_calls_in_the_order_it_always_made_them` | The calls of a Merk on a storage context, by name, key and order. For a base Merk: open, commit (the root key, then nodes in key order, then aux records), reads, the side records, the root key, `is_empty_tree`, `clear`, and a commit of an empty tree. For a layered Merk: open and commit, which read and write no root key, and `is_empty_tree_except`. The lists were read off the Merk before the seam. |
| `a_chunk_restore_writes_uncharged_and_corrects_a_parent_with_a_direct_put` | Chunk restore of a plain tree: one batch of uncharged puts per chunk, the root key put directly after the first chunk, a parent read and put directly after each later one, and one batch for the corrected heights. The corrected aggregates of `finalize_with_grovedb_elements` are held by the comparison above and by the restore tests, not by this one. |
| `what_a_storage_batch_holds_is_read_once_the_batch_is_applied` | On a transactional context nothing written through the adapter, in a change set or at once, is seen by a read or a cursor before the storage batch is applied to the transaction. This is the storage context's rule; the test holds that the adapter keeps nothing that would answer earlier. |
| `a_read_is_answered_by_the_transaction_when_it_is_made` | RD3, RD5 and RD6 at the seam: a fetch returns the transaction's own node, a node committed by another transaction as soon as it is committed, and nothing of an uncommitted one. A cursor opened before that commit does not show it and the next cursor does. These are RocksDB's rules; the test holds that the adapter pins no view and keeps no node. |
| `the_cursor_and_the_batch_are_the_storage_contexts_own` | The cursor of the adapter is the context's raw iterator and its change set holds the context's batch. This is checked by the compiler. |

## What later tasks get

| Task | What it builds on |
| --- | --- |
| SEAM-02 | The adapter exists and Merk runs on it. SEAM-02 showed that it makes the storage calls the code made before the seam and added the tests of [How the adapter is checked](#how-the-adapter-is-checked). |
| SEAM-04 | A subtree handle of an engine is a `NodeStore<'db>`; `Merk<E::..>` needs no other bound. The side records move to `FlatStore`. A RocksDB context is a `NodeStore` and a `FlatStore`, and both have a `cursor()`, which give the same raw iterator: code that has both traits in scope for a context, or is generic over a type bounded by both, names the trait (`NodeStore::cursor(&context)`), as it already does for `get`, `put` and `delete` of `StorageContext` and `FlatStore`. Generic code bounded by one of them is not affected. A `NodeStore` that is not a storage context is a type of Merk (see [The RocksDB adapter](#the-rocksdb-adapter)). |
| SEAM-05 | Every storage charge Merk sees passes through this trait's return values, and every size Merk supplies is in `NodeWriteCost` or a `KeyValueStorageCost`. The recording context of `merk/src/store/recording.rs` shows each storage call with the sizes and cost information it was given and adds up the costs the calls returned, so a test can hold those while the place of the charging moves. |
| SEAM-06 | `merk/src/store/mem.rs` is a second `NodeStore`, in memory, used by the tests here. It keeps of each written node only what its kind names and fails a commit whose result is not the node Merk wrote. It charges nothing, so it checks trees and proofs, not costs. |
| MERK-01 | `fetch_node` takes the link and is defined by its key (RD5). `prepare_node` gets the node and the kind, so a value is copied only for `ValueWritten`. Every `put_node` and `delete_node` is one write of its key (ST6). |
| MERK-02 | The kinds, and what they come from (`known_storage_cost` and the baseline for the value, `NodeMarks` for the children), are the dirty state to map. |
| MERK-03 | The inputs of every charge are logical: `NodeWriteCost`, the node itself for the record length (`TreeNode::encoding_length`). |
| MERK-04 | `root_key` and the root key operations of a change set are the whole of root handling. Nothing in Merk names the `roots` column. |
| MERK-05 | `cursor`, `Merk::clear` (cursor plus `delete_node`), `is_empty_tree`, the aux and meta operations. |
| MERK-06 | Proofs walk through `fetch_node`. A proof that must not load a value needs a node view, which is PERF-01. |
| MERK-07 | Chunk restore writes through `prepare_node`, `put_node` and `write_node` with honest kinds: new nodes are `ValueWritten`, a corrected parent link is `StructureChanged`, corrected heights are `HashOnly`, corrected aggregates are `ValueWritten` (the feature type changes). |
| PERF-01 | `get_node_for_read` is the read that already asks for less than a `TreeNode`. Node views replace its return type. |

## Call sites outside Merk

These are in GroveDB and `grovedb-query`. SEAM-04 and SEAM-07 change them. For each, this is the operation that serves it, or why it stays with the engine.

| Site | What it does today | Served by |
| --- | --- | --- |
| `has_raw` (`grovedb/src/operations/get/mod.rs`) | Opens a context by path and calls `get(key)`, to test whether a node record exists. Charges one seek and the record length. | `NodeStore::has_node(key)`, which SEAM-04 added: the adapter is `get(key)` and `is_some`. Same seek and same loaded bytes, and a record that does not decode is still `true`. `get_node_for_read(key)` would have made that record an error. Merk itself does not need the operation: `Merk::exists` decodes the node today. |
| The deletion visitor's raw delete (`delete_internal_on_transaction/v2.rs`) | `storage.delete(key, None)` on a subtree that is being deleted whole, without opening a Merk. | `NodeChangeSet::delete_node(key, None)` in a change set of the subtree, committed with `NodeStore::commit`. On a transactional context a direct delete and a committed batch part both stage the same delete in the shared batch. |
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
