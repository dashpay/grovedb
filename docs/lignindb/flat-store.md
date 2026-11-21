# FlatStore seam

LigninDB task SEAM-03 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

This document is the contract of the flat key-value seam: what `FlatStore` and `PendingPrefixDrops` promise, what is atomic with what, and what holds after a crash. The RocksDB engine meets it today. LigninDB has to meet the same contract (TREE-07, TXN-08, GROVE-04, GROVE-05). The code is in `storage/src/flat.rs` (`grovedb_storage::flat`), and the RocksDB implementation in `storage/src/rocksdb_storage/storage_context/flat.rs`.

It builds on the [storage surface inventory](storage-surface.md) (BASE-04). Cost codes such as `R-tx` and `B-commit` are the ones defined there.

## What the seam covers

GroveDB keeps two kinds of data: Merk nodes, and ordered key-value records that are not Merk nodes. `NodeStore` (SEAM-01) covers the first. `FlatStore` covers the second:

| Records | Keyspace | Written by |
| --- | --- | --- |
| MMR nodes, the bulk-append tree's cached MMR root, dense-tree values and hash records, the commitment frontier. A bulk-append tree is a dense tree and an MMR in one keyspace, and a commitment tree or a private-document store is a bulk-append tree with one more record or none. | the data keyspace of the tree's subtree | the append-only tree crates, and GroveDB for the frontier and one dense read |
| Auxiliary records (Platform state, protocol version, the state-sync restore marker) | the aux keyspace of the root subtree | `GroveDb::put_aux`, `delete_aux`, `get_aux`, `get_aux_by_key_prefix` |
| Auxiliary records of a Merk | the aux keyspace of the Merk's subtree | `Merk::commit` with aux operations, `Merk::get_aux` |
| Merk metadata | the meta keyspace of a Merk's subtree | `Merk::put_meta`, `get_meta`, `delete_meta` |
| Pending-prefix-drop redo records | the meta keyspace of the reserved subtree prefix `pending_prefix_drops_namespace()` | `GroveDb::drop_flat_subtree`, batch `DeleteTree` with a flat drop |

A subtree is identified by its 32-byte prefix. It has four keyspaces: data, aux, meta and roots. The roots keyspace holds the Merk root-key pointer and belongs to `NodeStore`. The data keyspace holds either Merk nodes or flat records, never both, and which one is decided by the element that owns the subtree.

The four keyspaces of one subtree are disjoint, and so are the keyspaces of different subtrees. The same key may hold a different value in each.

On RocksDB a keyspace is one column family restricted to the subtree prefix:

| Keyspace | Column family | Stored key |
| --- | --- | --- |
| data | default | `prefix ‖ key` |
| aux | `aux` | `prefix ‖ key` |
| meta | `meta` | `prefix ‖ key` |
| roots | `roots` | `prefix ‖ key` |

## The trait

```rust
pub trait FlatStore {
    type Cursor<'a>: RawIterator where Self: 'a;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error>;
    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8],
        cost_info: Option<KeyValueStorageCost>) -> CostResult<(), Error>;
    fn delete<K: AsRef<[u8]>>(&self, key: K,
        cost_info: Option<KeyValueStorageCost>) -> CostResult<(), Error>;
    fn cursor(&self) -> Self::Cursor<'_>;
    fn clear(&self) -> CostResult<(), Error>; // provided
}
```

A value of a type that implements `FlatStore` is a handle to one keyspace inside one transaction. The trait has no lifetime parameter and names no engine type. It is used through generics only, so every call is dispatched statically.

Rules that hold for every implementation:

1. **Keys.** A key is the caller's bytes. The handle never shows the subtree prefix, in `get`, in `put` or through the cursor. Keys order as byte strings (`memcmp` order, a shorter key before any key it is a prefix of). The empty key is a valid key.
2. **Sizes.** The seam sets no limit on key or value length. Keys are short today: a few bytes for a position in an append-only tree, 32 bytes for a pending-drop record, and whatever the caller chooses in aux. Aux values can be large: Platform stores its serialized state in one aux record ([P-26](platform-requirements.md)).
3. **`get`** returns the value under the key, or `None`. A read failure is an `Err`, never `None`.
4. **`put`** stores the value, replacing any value under the key.
5. **`delete`** removes the record. Deleting a key with no record succeeds.
6. **`cursor`** returns a cursor that is not positioned: `valid()` is false until the first seek. It walks the records that were visible to the handle when the cursor was opened, in key order, and never shows a record outside the keyspace. `seek(k)` goes to the first key `>= k`, `seek_for_prev(k)` to the last key `<= k`. Stepping past either end makes the cursor invalid. `status()` tells an error from the end of the keyspace. **A step from an invalid cursor is defined**, and it is part of the answer of a query: [A step from no key](#a-step-from-no-key) says where it leads.
7. **Cursor and writes.** One pattern of writing under an open cursor is defined: deleting the record the cursor stands on and then calling `next()`. `clear` uses it. Any other write to the keyspace while a cursor is open leaves that cursor's later results unspecified.
8. **`clear`** is the range delete: it removes every record of the keyspace, and only of that keyspace. The aux, meta and roots records of the subtree stay when its data keyspace is cleared (inventory finding 6).
9. **Refused writes.** A deferred handle opened with no batch is read-only: `put` and `delete` return `Err` and change nothing, and so does `clear` unless the keyspace is already empty. A snapshot read transaction is read-only as well. An immediate handle on it refuses each write, and a batch staged through deferred handles on it is refused when it is applied (`Error::SnapshotReadOnlyTransaction`).

The cursor is the existing `RawIterator` trait. `FlatStore` does not define a second cursor trait, so code that walks a keyspace is the same for a flat keyspace and for Merk records.

## A step from no key

Rule 6 once said that from an invalid cursor only a seek gives a defined position. GroveDB relies on more (D-189): a descending read of an exclusive range seeks to the end of the range and steps back (`QueryItem::Range`, `RangeTo` and `RangeAfterTo` in `grovedb-query/src/query_item/mod.rs`: `iter.seek(end)`, then `iter.prev()`). Where the end is past the last key of the subtree, that step is made from an invalid cursor, and the rows of the read depend on where it leads. So what RocksDB does is the contract on every existing GroveVersion, for the cursor of a flat keyspace and for the cursor of a Merk's nodes ([node-store.md](node-store.md#cursor)) alike.

**The line of a column.** RocksDB keeps the records of one keyspace of every subtree in one column family, under `prefix ‖ key`, and its iterator walks them all in that order: the *line* of the column. A cursor of a subtree is such an iterator, which shows a record of another subtree as no record (`valid()` is false, `key()` and `value()` are `None`). The iterator still stands on that record.

| Cursor | Column family | What its line holds |
| --- | --- | --- |
| the nodes of a Merk (`NodeStore::cursor`), the flat data of a tree (`FlatStore::cursor` of a handle) | default | the nodes of every Merk and the flat records of every tree that keeps its data flat |
| aux (`aux().cursor()`) | `aux` | the aux records of every subtree |
| meta (`meta().cursor()`) | `meta` | the meta records of every subtree, and the pending-prefix-drop records under their reserved prefix |

The `roots` column family has no cursor.

**The moves.** A cursor stands on a record of its line, or nowhere.

- `seek(k)` goes to the first record of the line at or after `prefix ‖ k`, and `seek_to_first()` to the first at or after `prefix`. `seek_for_prev(k)` goes to the last record at or before `prefix ‖ k`, and `seek_to_last()` to the last record before the next prefix. Where the line has no such record the cursor stands nowhere.
- `next()` and `prev()` go one record along the line from the record the cursor stands on, whichever subtree that record is of, and off the end of the line to nowhere. **From nowhere a step does nothing.** A cursor that was never sought stands nowhere.

So a cursor that has left the keys of its subtree is on the nearest record of another subtree that way, if the line has one, and nowhere if it has none:

| The cursor left its subtree's keys by | The line has a record of another subtree on that side | It has none |
| --- | --- | --- |
| `seek(k)` with every key of the subtree before `k`; `next()` from the last key; `seek_to_first()` in a subtree with no record | It stands on the first record after the subtree. `prev()` goes to the subtree's last key. `next()` goes one record further from the subtree, and `prev()` then comes back by one. | It stands nowhere: `prev()` and `next()` do nothing. |
| `seek_for_prev(k)` with every key of the subtree after `k`; `prev()` from the first key; `seek_to_last()` in a subtree with no record | It stands on the last record before the subtree. `next()` goes to the subtree's first key. `prev()` goes one record further from the subtree, and `next()` then comes back by one. | It stands nowhere: `next()` and `prev()` do nothing. |

A step into a subtree that has no record goes over it, to the nearest record on its other side or to nowhere.

Which subtree is before or after another is the order of their prefixes, which are hashes of their paths. So the same read of the same tree answers its rows in one grove and nothing in another: a descending read of `..end` with `end` past the last key, of a tree that has rows, answers nothing exactly when the tree's subtree is the last of its column family that holds a record. LigninDB reproduces this on the existing GroveVersions.

**Costs.** Every move is charged as a move (`cost-model.md` C1), also a step that does nothing. `valid()`, `key()` and `value()` of a cursor that is on no record of its subtree are charged the fixed charge of C2, whether it stands on a record of another subtree or nowhere.

**What counts as a record on the line.** A record that a cursor of its own subtree shows: the committed records of the state the cursor reads, with the transaction's applied writes. A staged write is not on it. A record whose delete the transaction has applied is not on it, and a record it has put is.

**On LigninDB** the records of a column are not one keyspace: the nodes of a Merk are in the Merk's own tree. The store's cursors keep the place a cursor of RocksDB has, and ask the engine what lies beside the subtree when a step is made from no key ([node-store.md](node-store.md#the-cursor)). That question is a read of the subtrees beside, so inside a transaction with open write scopes it can be refused as any read can (`transactions.md` SC3), which RocksDB has no word for; the cursor's `status()` gives the refusal.

Of the line itself, these are not reproduced:

- `seek_to_last()` on RocksDB seeks back from the 32 bytes that follow the prefix in order. If another subtree has exactly those bytes as its prefix and a record under the empty key, the iterator stands on that record, and the cursor is invalid although its subtree has records. On LigninDB it stands on the subtree's last key. Prefixes are Blake3 hashes, so two of them are consecutive with probability 2^-256 for each pair, as for the prefix of all `0xFF` bytes that `seek_to_last` of the RocksDB engine already leaves out.
- A cursor that is pointed at another subtree (`Engine::retarget_cursor`, [engine.md](engine.md)) keeps its place on the line on RocksDB. On LigninDB it stands nowhere. Every caller seeks such a cursor before it reads or steps it.
- A flat drop leaves the records of the dropped subtree in their column families until its ranges are deleted ([Pending prefix drops](#pending-prefix-drops)), and until then they are records of the line on RocksDB. GROVE-05 decides when LigninDB takes them off it; no drop of a subtree that holds records is made through the LigninDB engine before that task.

## Transactions and visibility

A handle lives inside one transaction. The engine hands it out in one of two write modes. Which mode is used at each call site is part of what must stay the same, because trees are written against it and costs depend on it.

**Deferred.** `put`, `delete` and `clear` stage their writes in the multi-context batch (`StorageBatch`) the handle was opened with.

- A staged write is not visible to `get` or to the cursor, through this handle or any other. The append-only trees are written for this: they keep in memory what they have to read again before the batch is applied.
- The batch is applied to the transaction as one unit (`commit_multi_context_batch`). Every write staged through any handle of that batch becomes visible at that moment, to every handle of the transaction.
- The batch keeps one outcome per key, not a log. Within one operation a put wins over a delete of the same key. Across operation boundaries (`StorageBatch::next_operation`) the later operation wins. This rule is the batch's (TXN-08), not the handle's.
- If the batch is dropped without being applied, none of its writes happen.

**Immediate.** `put` and `delete` write to the transaction at once and the handle's own reads see them. GroveDB uses immediate handles only in state-sync restore.

In both modes a read sees the transaction's applied writes and, under them, the state the transaction reads from. For a plain write transaction that is the latest committed state at the moment of the read, not a snapshot taken when the transaction started ([P-08](platform-requirements.md)). For a snapshot read transaction it is the state pinned when it started.

What is atomic with what:

| Unit | Atomic |
| --- | --- |
| The writes staged in one batch | They are applied to the transaction together or not at all. |
| The writes applied to one transaction | They commit together or not at all, with every Merk write of the same transaction. A flat record and the Merk element that commits to it can therefore never disagree after a commit or a crash. |
| A rollback to a savepoint | It undoes the applied flat writes made since the savepoint, together with the Merk writes. |

After a crash the database is at some committed transaction (DEC-04). No flat keyspace is ever ahead of or behind the trees.

## Costs

Every method returns its `OperationCost` with its result. The trait does not say what a call costs. The cost model of the GroveVersion in force does (BASE-06 specifies it; since SEAM-05 it is the functions of `grovedb_costs::cost_model`, which both engines call). The seam only fixes where a cost is reported, because callers drop some costs and keep others (inventory finding 10):

| Call | Deferred handle | Immediate handle |
| --- | --- | --- |
| `get` | reported by the call (`R-tx`: one seek, the value's length as loaded bytes) | zero (`R-imm`) |
| `put`, `delete` | zero at the call (`W-tx`); reported when the batch is applied (`B-commit`) | zero (`W-imm`) |
| cursor moves and reads | reported by each call (`I-move`, `I-key`, `I-value`) | the same |
| `clear` | the cursor walk at the call, the deletes when the batch is applied (`CLR`) | the cursor walk only |

`cost_info` is the caller's own account of a write. When it is given, the model charges from it and not from the record. These rules of today's model are visible through the seam and have to survive SEAM-05 unchanged:

- A `put` of a new record (`cost_info.new_node`) into a **data** keyspace gives the key's cost without the subtree prefix. The engine adds the cost of the full stored key, `32 + key length` plus the length of that number as a varint. The aux and meta keyspaces take `cost_info` as it is given.
- A `put` with `KeyValueStorageCost::prepaid()` is charged nothing when the batch is applied, not even a seek.
- A `delete` with no `cost_info` is charged from the **committed** state, not from the transaction: two seeks, the committed value's length as loaded bytes, and the stored key and value with their length varints as removed bytes. For a record that is not in the committed state, such as one the same transaction wrote, the value counts as empty: two seeks, no loaded bytes, and the stored key with its length varint plus one byte as removed bytes.
- An immediate handle ignores `cost_info`.

`clear` is defined in terms of the other operations so that its cost is too: one seek to the first record, then for each record `valid` and `key` on the cursor, one `delete` with no `cost_info`, and one `next`. An engine may remove the range in one step. It must still report this cost on every existing GroveVersion.

Nothing in the signature is RocksDB's. `CostResult` is how a call reports what the model computed, and `cost_info` is an input to the model. `ChildrenSizesWithIsSumTree`, which `StorageContext::put` takes, is a Merk node property and is not part of `FlatStore`: a flat record has no children.

## Pending prefix drops

A flat drop removes a subtree in O(1): it deletes the subtree's element from the parent Merk and leaves the subtree's records where they are. They are reclaimed later, outside consensus. The link between the two steps is a redo record.

```rust
pub struct PendingPrefixDropRecord {
    pub primary_prefix: SubtreePrefix,     // key of the record
    pub path: Vec<Vec<u8>>,                // for the liveness guard
    pub doomed_prefixes: Vec<SubtreePrefix>,
}

pub fn stage_pending_prefix_drop<S: FlatStore>(
    meta: &S, record: &PendingPrefixDropRecord) -> CostResult<(), Error>;

pub trait PendingPrefixDrops {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error>;
    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error>;
    fn remove_pending_prefix_drop_record(&self, primary_prefix: &SubtreePrefix)
        -> Result<(), Error>;
}
```

**The record.** It lives in the meta keyspace of the reserved subtree prefix `pending_prefix_drops_namespace()`, which is `Blake3("grovedb_pending_prefix_drops_v0")`. Its key is the primary prefix of the dropped subtree. Its value is `[version = 0] [doomed count] [doomed prefixes, 32 bytes each] [segment count] ([segment length] [segment])*`, with every count and length one byte. Both engines store these bytes: the record is charged as an ordinary meta `put` with no `cost_info`, so its length is part of the drop's `OperationCost`.

**Staging.** `stage_pending_prefix_drop` writes the record through a deferred meta handle of the batch that also carries the parent-Merk delete. So the record and the delete are applied together and commit together. There is no committed state in which the element is gone and the record is missing, or the reverse.

**The drain.** `GroveDb::flush_pending_prefix_drops` runs after a commit that contained drops and at startup. For each record it checks that the dropped path does not resolve to a live element (that guard is GroveDB's, above the seam), then calls `delete_prefix_ranges` and `remove_pending_prefix_drop_record`. The three operations of `PendingPrefixDrops` follow these rules:

1. **Outside the transaction.** None of them takes a transaction. Each takes effect on its own as soon as it returns, as a commit of its own. None returns or adds to an `OperationCost`.
2. **`pending_prefix_drop_records`** lists the records of the latest committed state, in ascending order of primary prefix. A record that is staged, or applied to a transaction that has not committed, is not listed. So a drop that can still roll back is never reclaimed. The order is part of the contract because the drain reports the first record it could not resolve.
3. **`delete_prefix_ranges`** removes every record under each given prefix in all four keyspaces of that subtree, Merk nodes and root pointer included. It refuses the all-`0xFF` prefix. Calling it on a prefix with no records changes nothing.
4. **`remove_pending_prefix_drop_record`** removes one record. Removing a record that is not there succeeds.
5. **Readers.** A reader that pinned a committed state before a range was deleted still sees the records: snapshot read transactions, state-sync sessions and checkpoints taken earlier. A checkpoint taken between the drop and the drain carries both the record and the data, and can be drained itself.
6. **Caller's side.** The caller passes only prefixes that no live element refers to, and nothing writes under them. What an open write transaction that wrote under such a prefix observes is not specified.
7. **Order after a crash.** The steps are durable in the order they were made. If a later step survives a crash, every earlier step did. A record can therefore never be gone while its ranges are still there.

Because every step is idempotent and they are ordered, a drain interrupted anywhere is finished by running it again:

| Crash after | On restart | The next drain |
| --- | --- | --- |
| the drop committed, no drain step yet | element gone, record listed, records of the subtree present | deletes the ranges, removes the record |
| some of the record's ranges deleted | record listed, those ranges empty | deletes all its ranges again (no change for the empty ones), removes the record |
| all ranges deleted, record still there | record listed, ranges empty | deletes nothing, removes the record |
| record removed | nothing left | nothing to do |

If a step fails, the drain returns the error and the record stays for the next run. Records reclaimed earlier in the same run stay reclaimed.

On RocksDB the range delete is one range tombstone per prefix and column family, written directly to the database, because an optimistic transaction cannot carry a range delete. On LigninDB it is the engine's subtree drop (TREE-09, GROVE-05): unlink in O(1), reclaim pages later. A flat drop through the LigninDB engine drops the subtree with its element, in the drop's own commit, so the range delete of its record finds nothing entered and writes nothing, and the removal of the record is the drain's one commit; the reads that differ until then are row D3 of `transactions.md` 14.5. The contract above holds on both engines, with the report counts and the record left in place after a step that failed (`engine.md`, "Flat drops and their drain").

## Other paths that bypass the transaction

Inventory finding 7 lists three paths that write to the database directly. The drain is described above. The other two are engine operations (`Engine`, SEAM-04), not `FlatStore` methods, but they act on flat keyspaces, so their contract is recorded here.

**`wipe`.** Removes every record of every keyspace of every subtree: tree data, roots, aux and meta, the pending-drop records with them. It takes no transaction and is not charged. On RocksDB it clears one column family after another (default, roots, aux, meta), each with one range tombstone, so it is not atomic: a crash part-way leaves some keyspaces empty and others full. The database must open after such a crash, and a second `wipe` completes the first. Platform relies on exactly that: it guards a wipe with a marker file and wipes again at the next start ([P-23](platform-requirements.md)). LigninDB may make `wipe` atomic. It may not leave a state that fails to open. What a transaction that is open during a wipe observes is not specified.

**`ingest_subtree_sst`** (feature `unsafe-dump-load`). Loads records prepared outside the database into one keyspace, bypassing any open transaction. The caller guarantees that the target subtree is empty and recovers from a failure by wiping, as the documentation of `GroveDb::ingest_subtree_sst` requires. Platform's use of it is [P-30](platform-requirements.md). It is a dump-and-load hatch and belongs to GROVE-09.

## What changed in the code

- `grovedb_storage::flat` holds `FlatStore`, `PendingPrefixDrops`, `stage_pending_prefix_drop`, `PendingPrefixDropRecord`, `pending_prefix_drops_namespace` and `SubtreePrefix`. It compiles without the `rocksdb_storage` feature. The record type and the namespace moved there from `rocksdb_storage`, which still re-exports them. The namespace is now a constant, checked against the Blake3 hash by a test.
- Both RocksDB contexts implement `FlatStore` for their data keyspace. `aux()` and `meta()` on a context return the handles of the other two keyspaces (`AuxKeyspace`, `MetaKeyspace`). Each method forwards to the `StorageContext` method of the matching column family and is `#[inline]`, so no call allocates, copies or dispatches more than before.
- `RocksDbStorage` implements `PendingPrefixDrops` by forwarding to its inherent methods.
- `PrefixedRocksDbTransactionContext::clear` is now `FlatStore::clear`, the same loop as before.
- `raw_iter_meta` was added to both contexts, for the meta cursor.
- The append-only tree crates (`grovedb-merkle-mountain-range`, `grovedb-bulk-append-tree`, `grovedb-dense-fixed-sized-merkle-tree`, `grovedb-commitment-tree`, `grovedb-private-document-store`) are generic over `S: FlatStore` where they were generic over `S: StorageContext<'db>`. Their eleven storage calls (finding 4) are `FlatStore::get` and `FlatStore::put`. Their mock contexts (finding 13) implement `FlatStore`.
- GroveDB's own flat call sites go through the seam: the aux operations and the state-sync restore marker through `aux()`, the pending-drop record through `stage_pending_prefix_drop`, and its direct reads and writes of flat records (the dense value read, the commitment frontier) through `FlatStore`. The drain still calls the `RocksDbStorage` methods by name; they are the ones `PendingPrefixDrops` forwards to. `GroveDb` holds a concrete `RocksDbStorage` until SEAM-04, so nothing in GroveDB is generic over these traits yet.

`StorageContext` is unchanged. Merk still reaches its aux and meta records through it; SEAM-01 owns that code and SEAM-04 joins the two seams.

## Left to later tasks

- **SEAM-04.** The RocksDB contexts expose the aux and meta handles as inherent methods. A handle type that is generic over the engine needs a trait for "the flat keyspaces of a subtree handle"; that belongs with the `Engine` trait, which decides what a subtree handle is. `PendingPrefixDrops` is meant to become a supertrait or a part of `Engine`, next to `wipe`.
- **SEAM-04, GROVE-12.** `FlatStore` does not require `Send`. The RocksDB deferred handle holds a reference to a `StorageBatch`, which is not `Sync`. Parallel writers need a decision on that for flat handles as for Merk handles.
- **SEAM-05.** Done: the formulas in "Costs" are functions of the versioned cost model (`grovedb_costs::cost_model`; [cost-model.md](cost-model.md), section 14). The table of where each cost is reported stays.
- **TREE-07, GROVE-01, GROVE-04.** `FlatStore` reports costs, and `lignin-tree` must not know cost rules. So LigninDB's implementation of `FlatStore` is an adapter above the engine: it wraps the engine's flat keyspace handle and cursor, and gets every cost from the model. The engine's own flat keyspace (TREE-07) needs the same operations and the same ordering, without costs.
- **MERK-05.** Merk's aux and meta records can use `aux()` and `meta()` once Merk holds a handle that offers them.
- **A cursor error inside `clear`.** `clear` stops when the cursor becomes invalid and does not look at `status()`, as before this task. A cursor that fails part-way therefore ends the walk without an error. Changing that changes behaviour and was left alone.
