# Engine: the seam between GroveDB and its storage engine

LigninDB task SEAM-04 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

`Engine` is everything `GroveDb` asks of a storage engine. It lives in `grovedb/src/engine/mod.rs`. `GroveDb<E: Engine = RocksDbEngine>` is generic over it, and the one implementation today is `RocksDbEngine` (`grovedb/src/engine/rocksdb.rs`), which is `grovedb-storage` behind the trait. GROVE-01 adds `LigninEngine` in `grovedb::engine::lignin`.

This document states the contract of the trait, how the RocksDB engine meets it, what `GroveDb<E>` looks like to its callers, and the choices the plan left to this task. It builds on the two seams below it, [`NodeStore`](node-store.md) and [`FlatStore`](flat-store.md), and on the transaction rules of [`transactions.md`](transactions.md).

## The shape of the seam

```text
GroveDb<E: Engine>           operations, batches, proofs, references, state sync
---------------------------- Engine --------------------------------------------
  E::Transaction<'db>        a write transaction, or a snapshot read transaction
  E::Batch                   the writes of one operation, staged across subtrees
  E::Subtree<'a>             a deferred handle of one subtree
  E::ImmediateSubtree<'a>    an immediate handle of one subtree
      NodeStore              the Merk of the subtree runs on it (SEAM-01)
      FlatStore              its data keyspace, and aux() and meta() (SEAM-03)
---------------------------- engines -------------------------------------------
RocksDbEngine                RocksDbStorage, Tx, StorageBatch, the storage contexts
LigninEngine                 GROVE-01
```

An engine is `Send + Sync + 'static`: it is a database handle that every thread of a node shares. Everything else is an associated type, so no call through the seam is dispatched dynamically.

## Operations

| Operation | What it does | RocksDB engine | Cost ([codes](storage-surface.md#the-api-and-what-each-call-charges)) |
| --- | --- | --- | --- |
| `open(path)` | Opens the database, creating it if there is none. | `RocksDbStorage::default_rocksdb_with_path` | none |
| `open_checkpoint(path)` | Opens a checkpoint made by `create_checkpoint`. | `RocksDbStorage::checkpoint_rocksdb_with_path` | none |
| `create_checkpoint(path)` | Captures the committed state in a new directory. An existing target is refused and left as it is. | `Storage::create_checkpoint` | none |
| `flush()` | Makes every commit made so far durable. | `Storage::flush` | none |
| `wipe()` | Removes every record of every keyspace of every subtree. | `RocksDbStorage::wipe` | `DB` |
| `start_transaction()` | Starts a write transaction. | `Storage::start_transaction` | `zero` |
| `start_snapshot_read_transaction()` | Starts a transaction pinned to the state committed at that moment. | `RocksDbStorage::start_snapshot_read_transaction` | `zero` |
| `start_read_transaction()` | Starts the transaction of one read that was given none. The engine may pin it. | `Storage::start_transaction` | `zero` |
| `commit_transaction(tx)` | Commits and ends the transaction, whether the commit succeeds or not. | `Storage::commit_transaction` | `zero` |
| `rollback_transaction(&tx)` | Discards the transaction's writes. It stays usable. | `Storage::rollback_transaction` | none |
| `set_savepoint(&tx)`, `rollback_to_savepoint(&tx)` | A stack of savepoints on the transaction. | `Tx::set_savepoint`, `Tx::rollback_to_savepoint` | none |
| `open_write_scope(&tx)`, `scope_transaction(&scope)`, `merge_write_scope(scope)`, `rollback_write_scope(scope)` | One writer's part of a transaction. See [Write scopes](#write-scopes). | one scope at a time, remembered by its transaction | none |
| `subtree(path, batch, &tx)` | A deferred handle of the subtree at a path. | `Storage::get_transactional_storage_context` | `P-path` |
| `subtree_by_prefix(prefix, batch, &tx)` | A deferred handle of the subtree with a prefix. | `Storage::get_transactional_storage_context_by_subtree_prefix` | `zero` |
| `immediate_subtree(path, &tx)` | An immediate handle of the subtree at a path. | `Storage::get_immediate_storage_context` | `P-path` |
| `immediate_subtree_by_prefix(prefix, &tx)` | An immediate handle of the subtree with a prefix. | `Storage::get_immediate_storage_context_by_subtree_prefix` | `zero` |
| `settle_batch(&mut group, batch)` | Works out what a batch costs and adds its writes to a write group. Nothing is applied. | `RocksDbStorage::continue_write_batch` | the seeks and loaded bytes of `B-commit` |
| `apply_write_group(group, &tx)` | Applies a write group to the transaction, all of it or nothing. | `RocksDbStorage::commit_db_write_batch` | the pending storage cost of `B-commit`, if the application succeeds |
| `apply_batch(batch, &tx)` | `settle_batch` and `apply_write_group` in one step. Provided by the trait. | the two calls above, which are what `Storage::commit_multi_context_batch` makes | `B-commit` |
| `retarget_cursor(cursor, prefix)` | Points a cursor at another subtree. It keeps the state it reads from. | `PrefixedRocksDbRawIterator::retarget` | `zero` |
| `move_subtree(from, to, batch, &tx)` | Starts to move the records of one subtree's data keyspace to another prefix. See [Moving a subtree](#moving-a-subtree). | two contexts by prefix, `raw_iter`, `seek_to_first` | `I-move` |
| `pending_prefix_drop_records()`, `delete_prefix_ranges(prefixes)`, `remove_pending_prefix_drop_record(prefix)` | The supertrait [`PendingPrefixDrops`](flat-store.md#pending-prefix-drops). | the `RocksDbStorage` methods of the same names | `DB` |

Prefix derivation is not an operation of the engine. A subtree's prefix is the Blake3 hash of its path on every engine, because state sync chunk ids carry it, and what the derivation charges is a fee rule (`P-path`, `P-sec`). GroveDB still calls `RocksDbStorage::build_prefix` and `secondary_prefix_for` for it, by name. SEAM-07 and SEAM-05 give the two functions a home that names no engine.

## Contract

### Transactions

The read rules are those of [`transactions.md`, section 4](transactions.md#4-what-each-handle-reads):

- A write transaction reads its own writes, and for everything else the latest committed state at the moment of the read (RD3). It has no snapshot of its own.
- A snapshot read transaction reads the state that was committed when it started (RD1). A write through it, the application of a batch, a write scope and its commit are refused with `Error::SnapshotReadOnlyTransaction`. Rolling it back is allowed and does nothing.
- Both are the same associated type, `Transaction<'db>`, as they are one type on RocksDB today. `GroveDb` takes either wherever it takes a transaction.
- A read with no transaction and a write with no transaction are made by `GroveDb`, not by the engine: an operation that is given no transaction starts one (`TxRef` in `grovedb/src/util.rs`). An engine sees a transaction in every call.
  - An operation that writes starts it with `start_transaction` and commits it when it is done (RD7). These are the operations that call `TxRef::new`.
  - An operation that only reads starts it with `start_read_transaction` and never commits it. These are the operations that call `TxRef::for_read`. An engine may pin that transaction to the state committed when it starts, which is RD2 on LigninDB. The RocksDB engine starts a write transaction for it, as before the seam, so each of its reads sees the latest committed state.
  - A read hands its one transaction to everything it calls, so the whole operation reads through it: a query in every frame it walks, a `get` in every hop of the reference it follows, a read of an append-only tree in the read of the element that owns the tree. Before the seam such a read started a transaction for each of these. RocksDB cannot tell the difference, because none of them is pinned. An engine that pins would have read a different state in each.
  - What still starts more than one transaction is more than one operation, as it was before the seam. `bulk_append`, `commitment_tree_insert_raw`, `dense_tree_insert`, `mmr_tree_append` and `private_document_store_insert` read the element of their tree as a read of its own and then start the transaction they write through. `insert_if_not_exists`, `insert_if_not_exists_return_existing_element`, `insert_if_changed_value` and `delete_up_tree_while_empty` are a read followed by a write. `apply_operations_without_batching` is one write for each operation it is given.
  - A proof does not start its transaction through `TxRef`. The prover starts a snapshot read transaction itself (`start_snapshot_read_transaction`) and reads every layer through it, the second proof of a layer it cuts under a per-instance cap included. The references it follows are read in three ways, as upstream has them:
    - A reference in an ordinary row is followed with no transaction: `prove_subqueries_v1` calls `rewrite_reference_row_v1` without one. Each such row starts a read transaction of its own, outside the snapshot.
    - A page that is skipped to by count over the whole query (`prove_count_offset_layer_v1`) rewrites its reference rows with no transaction either. It starts one read transaction for all of them before it looks at a row, whether or not the page holds a reference.
    - A page that is skipped to by count within a branch (`prove_local_count_offset_layer_v1`) hands the snapshot on, and its references are read in it.

    On RocksDB a read transaction reads the latest committed state, so the first two can read a target that was committed after the snapshot was taken.
- `commit_transaction` takes the transaction by value: a commit ends it whether it succeeds or not (CM6). The two commits CM1 refuses with the transaction left open do not reach the trait as it is. A scope borrows its transaction, so a transaction with an open scope cannot be handed to a commit. Pending marks and `NotFinalized` (SC9) belong to iteration two, and GROVE-11 adds them with whatever lets the caller keep the transaction.
- Savepoints form a stack. `rollback_to_savepoint` undoes every write since the newest savepoint and removes it. There is no operation that removes a savepoint without undoing.

### Commit errors

Every operation of `Engine` returns `grovedb_storage::Error`, the error type of all three seams (`FlatStore` returns it, and `NodeStore` wraps it). `GroveDb` hands it on as `grovedb::Error::StorageError`, as before the seam. The one exception is `SubtreeMove::move_next`, which returns `grovedb::Error` because it hands back the error of GroveDB's own inspection of a record.

A commit can end in more ways than success and failure ([CM5](transactions.md#81-sequence)). The engine-neutral outcomes are `grovedb_storage::error::CommitError`:

| Outcome | The transaction | The database |
| --- | --- | --- |
| `Conflict` | over, its writes discarded | unchanged |
| `NotDurable` | over | the commit happened and is visible; it is not known to survive a crash |
| `Unsettled` | over | unchanged in this session, which accepts no further commit; the next open settles it |

Any other error of a commit means the transaction is over and the database unchanged.

`Error::commit_error()` classes an error a commit returned, and `Error::is_commit_conflict()` asks for the one case Platform acts on. `grovedb::Error` has the same two methods.

- An engine with outcomes of its own returns them as `Error::Commit(CommitError)`. That is what GROVE-01 maps LigninDB's `Conflict`, `NotDurable` and unsettled commit to.
- The RocksDB engine returns what it returned before the seam: `Error::RocksDBError` with the kind `Busy`. `commit_error()` classes exactly that value as `CommitError::Conflict`. So a caller that matches `rocksdb::ErrorKind::Busy` keeps working, and a caller that asks `is_commit_conflict()` gets the same answer on both engines.
- `Conflict` is reported for a conflict and for nothing else (CF6). A RocksDB `TryAgain` is therefore not classed: RocksDB returns it when its memtable history no longer reaches back far enough to tell whether there was a conflict. LigninDB always decides (CF9), so it has no such error.

The classification is of an error that a commit returned. RocksDB does not return `Busy` from the other calls GroveDB makes.

State sync commits inside GroveDB, and wraps a failed commit in `Error::InternalError` with the error's text (`replication/state_sync_session.rs`). That was so before the seam, and an outcome cannot be classified there until it stops.

### Handles

A subtree handle lives inside one transaction, in one of two write modes ([flat-store.md](flat-store.md#transactions-and-visibility)):

- A **deferred** handle (`Subtree<'a>`) stages its writes in the batch it was opened with. Neither it nor any other handle reads them until the batch is applied. Opened with no batch, it is read-only.
- An **immediate** handle (`ImmediateSubtree<'a>`) writes to the transaction at once and reads its own writes. GroveDB uses it in state-sync restore only.

Both are a `SubtreeHandle<'a>`:

```rust
pub trait SubtreeHandle<'db>: NodeStore<'db> + FlatStore {
    type Aux<'a>: FlatStore where Self: 'a;
    type Meta<'a>: FlatStore where Self: 'a;
    fn aux(&self) -> Self::Aux<'_>;
    fn meta(&self) -> Self::Meta<'_>;
}
```

- As a `NodeStore` it is the handle a Merk of the subtree runs on: `Merk<E::Subtree<'a>>`.
- As a `FlatStore` it is the subtree's data keyspace, for the append-only trees.
- `aux()` and `meta()` are the handles of the subtree's two other keyspaces, in the same transaction and write mode.

The data keyspace of a subtree holds Merk nodes or flat records, never both, and the element that owns the subtree decides which. A handle does not know. The code that holds it does, and says so by the trait it calls: both traits have a `cursor`, so GroveDB writes `NodeStore::cursor(&handle)` for a walk over Merk nodes and `FlatStore::cursor(&handle)` for a walk over flat records. On RocksDB the two are the same raw iterator. On LigninDB they read different structures.

Outside `grovedb::engine` and the tests, GroveDB's code names `StorageContext` nowhere any more. The places where it called a storage context directly now go through one of the two traits:

| Site | Before | Now |
| --- | --- | --- |
| `has_raw` | `StorageContext::get`, then `is_some` | `NodeStore::has_node`, added to the trait for this. A record that does not decode counts as held, as before. |
| The deletion visitor's raw delete (`delete_internal_on_transaction/v2.rs`) | `StorageContext::delete(key, None)` | one `NodeChangeSet::delete_node(key, None)`, committed with `NodeStore::commit`, as [node-store.md](node-store.md#call-sites-outside-merk) names it. On a transactional context both stage the same delete in the shared batch. |
| Walks of Merk records (`Element::iterator`, `find_subtrees`, replication, verification, the visitor) | `StorageContext::raw_iter` | `NodeStore::cursor` |
| The append-only trees, the commitment frontier, the dense value read, `clear` of a subtree | `FlatStore` methods, since SEAM-03 | unchanged |
| Aux records, the pending-drop record, the restore marker | `aux()` and `meta()` of the RocksDB contexts | `SubtreeHandle::aux` and `meta` |

`grovedb::engine::node_store_error` keeps the error of the first two as it was: a storage error stays `Error::StorageError`.

Nothing in the seam takes an unchanged hash to mean an unchanged record. Since GroveDB #1059 the referrer list of an element that takes backward references is in no hash, so a stored record can change while every hash stays what it was. A batch stages every write it is given, a move carries every record, and no operation of the trait skips a write, or answers a read from what it read before, because a hash is the same.

### The batch across subtrees

`Batch` is the batch that stages the writes of one GroveDB operation across subtrees, `StorageBatch` on RocksDB. Its trait, `EngineBatch`, has what GroveDB calls on it besides creating one: `next_operation`, `take_pending` and `merge_overwriting`. The rules a batch keeps (one outcome per key, a put wins inside one operation, the later operation wins across a boundary) are those of [flat-store.md](flat-store.md#transactions-and-visibility) and TXN-08.

Applying a batch has two steps, because the partial batch needs them apart ([finding 8 of the storage surface](storage-surface.md#findings-for-the-seams)):

1. `settle_batch(&mut group, batch)` works out what the batch's writes cost and adds them to a `WriteGroup`, after what the group already holds. Nothing is visible yet. The cost it returns, the seeks and the bytes loaded for deletes that carry no cost, is charged whether or not the group is ever applied. The storage cost of the writes stays in the group as its pending cost (`WriteGroup::pending_cost`).
2. `apply_write_group(group, &tx)` applies everything in the group to the transaction, in the order it was settled, all of it or nothing, and charges the pending cost if it succeeds.

`apply_batch` does both. The partial batch settles what is staged so far (`batch.take_pending()`), reports the cost to its caller, lets the caller add operations that are staged into the same batch, settles those, and applies the group once.

`apply_write_group` and `apply_batch` return `Applied`. An engine that places nodes when their batch is applied (LigninDB, [P5 of the tree layout](tree-layout.md)) reports the locations of the written nodes there, for Merks that stay open. On RocksDB it is `()`. GroveDB drops it today: no Merk outlives the batch it wrote into. PERF-03 is the first user.

### Write scopes

DEC-03: inside one write transaction, several threads may write different Merks at once. The rules are [section 9 of `transactions.md`](transactions.md#9-write-scopes-several-threads-in-one-transaction). The trait has:

- `open_write_scope(&tx)` opens a scope, one writer's part of the transaction. A scope that needs what another open scope holds fails with `WriteScopeError::Overlap`. It never waits.
- `scope_transaction(&scope)` is the transaction as the scope sees it: its own writes, then what is merged into the transaction, then the committed state. It has the type of a transaction, so every operation of `GroveDb` that takes a transaction runs in a scope when it is given this view. Nothing in GroveDB has to be written a second time for scopes.
- `merge_write_scope(scope)` ends the scope and makes its writes the transaction's.
- `rollback_write_scope(scope)` ends the scope and removes its writes.
- A scope that is dropped is rolled back, as a transaction that is dropped is. The one exception is an engine that cannot roll a scope back at all and says so with `WriteScopeError::RollbackUnsupported`: there the writes of a dropped scope stay in the transaction, as they do after the refused rollback.
- Setting a savepoint on a transaction and rolling back to one need every scope of that transaction closed, and fail with `WriteScopeError::ScopesOpen` otherwise (SC8). The check is in `Engine::set_savepoint` and `rollback_to_savepoint`, which `GroveDb::set_savepoint` and `rollback_to_savepoint` call. Platform calls the methods of `Tx` itself, which know no scope. Nothing opens a scope on RocksDB, so nothing is missed there, and OPS-03 moves Platform to the methods of `GroveDb`.
- A commit needs every scope closed as well (SC8). The types see to it: a scope borrows its transaction, and a commit takes the transaction by value.

On existing GroveVersions GroveDB opens no scope: one thread writes the transaction, as the plan says of iteration one. How LigninDB maps those writes to its scopes is GROVE-01's choice (SC1).

The RocksDB engine allows one scope at a time, for the whole engine:

- A second scope, on any transaction, is refused with `Overlap`. The engine remembers which transaction the open scope belongs to, so the savepoints of every other transaction stay usable.
- The scope's view is its transaction, and its writes go there directly, so merging is closing.
- A scope on a snapshot read transaction is refused with `SnapshotReadOnlyTransaction`.
- `rollback_write_scope` fails with `WriteScopeError::RollbackUnsupported`, closes the scope, and leaves its writes in the transaction. A scope that is dropped leaves them there too. A caller that has to undo a scope on this engine sets a savepoint before it opens the scope and rolls back to it after the scope is closed, which is what Platform does for a state transition today. RocksDB can undo back to a savepoint, but it cannot remove a savepoint without undoing, in its Rust binding or its C API. A scope that set a savepoint would leave it on the transaction's stack when it merged, and the caller's next `rollback_to_savepoint` would stop there. Platform sets a savepoint for every state transition (P-12), so the engine does not touch the stack. Nothing needs the rollback on RocksDB: scopes are used from iteration two, when RocksDB is gone.

### One state across subtrees

A cursor reads the committed state as it was when the cursor was opened, under the transaction's own writes (RD6). `retarget_cursor(cursor, prefix)` points a cursor at another subtree and keeps that state, so one cursor can read an element in one subtree and then the tree it names in another without a commit landing in between. GroveDB uses it for local count offsets (`element/local_offset_read.rs`) and for the paginated reads of an indexed tree's secondary (`operations/indexed_tree.rs`).

It is a function of the engine and not of the cursor because the cursor trait, `RawIterator`, is shared with `grovedb-query` and names no subtree. On RocksDB it is the iterator's own `retarget`. On LigninDB a cursor holds a version pin, and retargeting keeps the pin.

### Moving a subtree

A batch `Move` renames a key and keeps what is under it. `move_subtree(from, to, batch, &tx)` starts the move of one subtree's data keyspace to another prefix and returns a `SubtreeMove`. The move is made by calling `move_next(inspect, bill)` until it returns `None`:

- Each call moves one record. It is shown to `inspect` first, as its key and its bytes. GroveDB classifies it there: it refuses a record the move must not carry, and it finds the trees nested under the moved key, which it moves next. If `inspect` fails, the record stays and the error is returned.
- `bill` gives what the record is charged where it lands. Its removal from the old prefix is charged as a delete that frees nothing.
- The steps are GroveDB's to order. It moves a nested tree as soon as it finds it, before the rest of the tree above, as before the seam.

On RocksDB a move is a copy, and `RocksDbSubtreeMove` makes the storage calls `move_subtree_storage` and `move_namespace` made: two contexts by prefix, one raw iterator on the source, and for each record a `put` under the new prefix and a `delete` under the old one. LigninDB re-hosts the subtree instead and moves no cell ([D5 of the tree layout](tree-layout.md)). What a move charges on existing GroveVersions is the same on both, and is BASE-06's and SEAM-05's.

### Outside the transaction

`wipe`, `flush`, the checkpoints and the three operations of `PendingPrefixDrops` take no transaction. Their contracts are in [flat-store.md](flat-store.md#other-paths-that-bypass-the-transaction) and [platform-requirements.md](platform-requirements.md) (P-05, P-14, P-23, P-24). `PendingPrefixDrops` is a supertrait of `Engine`, so the drain of `flush_pending_prefix_drops` is written for any engine.

Three things stay with the RocksDB engine and exist on `GroveDb<RocksDbEngine>` only: the hatches `GroveDb::raw_storage` and `GroveDb::ingest_subtree_sst` (feature `unsafe-dump-load`), which GROVE-09 replaces, and `GroveDb::start_visualizer` (feature `grovedbg`), because the debugger holds a `GroveDb` with no engine named.

### Threads

- An `Engine` is `Send + Sync`. So is `GroveDb<E>`.
- `Transaction`, `Batch`, the handles and `WriteScope` have no `Send` bound, and neither have `NodeStore` and `FlatStore`. On RocksDB a transaction is `Send`, and a storage context and a scope are not (a context holds `&StorageBatch`, whose operations sit in a `RefCell`). Nothing on RocksDB needs them to be: one thread at a time writes a transaction.
- `Send` is asked for where something crosses threads. That is GROVE-12, which sends a scope to a worker: it asks `E::WriteScope<'a>: Send`, and for the handles opened in it. LigninDB's types meet that (MERK-01: `LigninNodeStore` and a Merk on it are `Send`). A Merk is `Send` whenever its store is (SEAM-01).

## GroveDb<E>

```rust
pub struct GroveDb<E: Engine = RocksDbEngine> { db: E }
pub type Transaction<'db, E = RocksDbEngine> = <E as Engine>::Transaction<'db>;
pub type TransactionArg<'db, 'a, E = RocksDbEngine> = Option<&'a Transaction<'db, E>>;
```

`GroveDb`, `Transaction<'db>` and `TransactionArg<'db, 'a>` with no engine named are what they were: `GroveDb<RocksDbEngine>`, `Tx<'db>` and `Option<&'a Tx<'db>>`. Platform builds against this branch with no change to its source.

Three rules decide where a function of `GroveDb` lives:

1. **A method that takes `self` is generic**: `impl<E: Engine> GroveDb<E>`. That is every operation: reads, writes, batches, queries, proofs, state sync, checkpoints, aux, the append-only trees.
2. **An associated function with no `self` and no engine type in its signature is on the default engine**: `impl GroveDb`. `GroveDb::verify_query(..)`, `GroveDb::open(path)` or `GroveDb::estimated_case_operations_for_batch(..)` name no engine, and Rust does not use the default of a type parameter to infer one, so such a call resolves only if exactly one `impl` has the function. This is how `HashMap::new()` works. It covers the proof verifiers, the estimated-cost helpers, and the constructors `open`, `open_checkpoint` and `delete_checkpoint`.
3. **The constructors have a generic twin**: `GroveDb::<E>::open_with_engine(path)`, `open_checkpoint_with_engine(path)`, `delete_checkpoint_with_engine(path)`, and `GroveDb::from_engine(engine)` for an engine opened with options of its own. `GroveDb::engine()` gives the engine back. `open_with_cidx_integrity_check` has none: it is `open`, then `verify_grovedb`, and an error if that reports an issue, which a caller with another engine writes out.

Without the `minimal` feature there is no engine, and `GroveDb` is a plain struct that names the proof verifiers, as before.

`MultiStateSyncSession<'db, E = RocksDbEngine>` is generic in the same way.

### Lifetimes

`E::Transaction<'db>` is an associated type with a lifetime parameter. The compiler cannot shorten that lifetime in generic code, as it does for the concrete `Tx<'db>`. Three conventions in GroveDB's generic code follow from it. A caller that names a concrete engine sees none of them: for it `Tx<'db>` still shortens.

- A method that takes a transaction ties it to `self`: `fn get<'db>(&'db self, .., transaction: TransactionArg<'db, '_, E>, ..)`. A transaction the method starts itself borrows `self`, so both must be one type.
- A function that returns a handle or a Merk has a lifetime of its own for the borrow: `fn open<'a, 'db: 'a>(&'db self, tx: &'a Transaction<'db, E>, batch: Option<&'a E::Batch>) -> Merk<E::Subtree<'a>>`. The methods of `Engine` that open handles have this shape.
- A handle of an engine may have a destructor, so GroveDB drops a handle, or a Merk on one, before it moves the batch or the transaction the handle borrows.

One change outside GroveDB follows from the same fact: `ChunkProducer::new` in Merk took `&'db Merk<S>` with `S: NodeStore<'db>`, one lifetime for the borrow of the Merk and for what its store borrows. A generic handle cannot meet that. The two are separate lifetimes now.

### Engine-independent helpers

Ten associated functions of `GroveDb` that name no engine are free functions of the crate now. They are called from types that know no engine, the batch's Merk cache among them, or with a store of a test's own:

- on a `Merk<S>`, generic over `S: NodeStore`: `update_tree_item_preserve_flag` and its two batch variants, `get_element_from_subtree`, `capture_indexed_entry_state`, and from the verification `verify_indexed_axis_content` and `element_for_reciprocity_audit`
- on batch operations or plain values: `insert_indexed_tree_item_into_batch_operations`, `indexed_tree_item_with_root_state_into_batch_operations`, and `axis_label`

Functions that take a `Merk<S>` are bounded by `S: NodeStore<'db>` and not by an engine, so a test can run them on a store of its own. `FaultableReads` (`grovedb/src/tests/faultable_reads.rs`) is such a store: it wraps any `NodeStore` and fails the node reads of an armed key with a storage error. `open_merk` reads a subtree's parent element through it in test builds, on every engine. The ten tests of `parent_read_fault_tests.rs` (GroveDB #1056) open their handles with `Engine::subtree` and read them as a `NodeStore` or a `FlatStore`, so they name no storage context. What ties them to RocksDB is the fixture that opens their database, which is SEAM-06's to make generic. One of them already runs on a second engine: `a_failed_parent_read_is_a_storage_error_on_a_second_engine`.

## How the RocksDB engine is checked

`RocksDbEngine` keeps one piece of state, the flag for the open write scope. Everything else forwards. What can go wrong is a call that is missing, made twice or made with other arguments, and a cost that is dropped or added. `grovedb/src/engine/tests.rs` runs the same history on two databases, one through the engine and one with the calls on `RocksDbStorage` that GroveDB made before the seam, and compares:

| Test | What it holds |
| --- | --- |
| `a_handle_is_the_storage_context_of_its_subtree` | A handle by path charges the prefix hashing, a handle by prefix nothing. An immediate handle writes the three keyspaces at once, where the storage contexts read them. A deferred handle stages, and one with no batch refuses a write. |
| `a_batch_is_applied_with_the_calls_and_costs_of_a_multi_context_commit` | `apply_batch` returns the cost `commit_multi_context_batch` returns for the same batch, with a billed put, a delete that reads the committed record, and aux and meta records, and leaves the same records. |
| `a_batch_that_cannot_be_settled_is_not_applied` | A batch with a bill that does not match its record fails in both with the same error and the same cost, the seek of the put, and nothing reaches the transaction. |
| `a_write_group_settles_what_is_staged_and_keeps_staging` | Two settles and one application cost, step by step, what `build_write_batch`, `continue_write_batch` and `commit_db_write_batch` cost, and the parts are applied in order. |
| `a_subtree_moves_record_by_record_with_the_calls_of_a_copy` | A move costs what the copy loop of `move_namespace` cost and leaves the same records. |
| `a_move_fails_when_its_cursor_stopped_on_an_error_or_withholds_a_record`, `a_record_its_inspection_refuses_stays_where_it_is` | A move fails rather than end early, and a refused record is neither landed nor cleared. |
| `a_write_transaction_reads_its_own_writes_and_the_latest_committed_state`, `a_read_transaction_of_the_rocksdb_engine_reads_the_latest_committed_state`, `a_snapshot_read_transaction_is_pinned_and_refuses_writes`, `savepoints_form_a_stack` | The transaction rules above, on RocksDB. |
| `a_commit_that_loses_is_the_error_rocksdb_returned_and_is_classed_as_a_conflict` | The commit of a transaction whose key another commit wrote returns `RocksDBError` of kind `Busy`, unchanged through `GroveDb`'s error, and both classify it as a conflict. Two transactions whose keys do not meet both commit. |
| `the_rocksdb_engine_allows_one_write_scope_at_a_time` | `Overlap`, `ScopesOpen` for the scope's own transaction and not for another, the scope's view, merge, and what this engine cannot do: a refused rollback and a dropped scope both leave the scope's writes in the transaction, and the caller's savepoint from before the scope undoes them. |
| `a_retargeted_cursor_keeps_the_state_it_was_opened_on` | A cursor retargeted after a commit does not show it, and the next cursor does. |
| `a_checkpoint_opens_with_the_state_it_captured`, `wipe_empties_every_keyspace`, `pending_prefix_drops_are_the_storages` | The operations outside the transaction. |
| `a_merk_runs_on_a_subtree_handle_of_any_engine` | A function written for any `E: Engine` opens a Merk on a handle, commits through the batch, and reads the node back. |
| `grovedb/src/tests/one_transaction_per_operation_tests.rs` | `GroveDb` on a second engine, `PinnedReads`: the RocksDB engine, except that the transaction of a read is a read-only snapshot and the engine counts the transactions it starts. Read operations given no transaction each start exactly one read transaction and no write transaction, among them a `get` that follows a reference, a query with a subquery, the getters of an MMR tree and the five reads of the big sum axis. None of these starts a snapshot. A proof starts the snapshot it pins and one read transaction for the row that is a reference. A proof of a layer under a per-instance cap, which proves that layer twice, starts one snapshot and nothing else, and its bytes are those of the default engine. `insert`, `delete`, `apply_batch`, `put_aux` and the four entry points of the indexed trees with a 128-bit sum each start one write transaction and no read, and an MMR append starts one of each. With a transaction of the caller's, none is started. A parent read that fails in a handle of this engine comes back as the storage error, and a parent that is not there as absence. |
| `a_grovedb_is_opened_on_an_engine_and_sets_savepoints_through_it` | `open_with_engine`, `from_engine` and `engine()`, and `GroveDb`'s savepoints: a rollback undoes what came after the savepoint, and both are refused while a scope of the transaction is open. |

Above the engine, the golden corpus (BASE-02) replays unchanged on the branch, and so do the GroveDB and Merk suites.

## What later tasks get

| Task | What it builds on |
| --- | --- |
| SEAM-05 | Every storage charge GroveDB sees comes back from a call on `Engine`, on a handle or on a cursor. The fee rules that sit in the RocksDB engine today are listed under [Choices](#choices-this-task-made), item 9. |
| SEAM-06 | Every operation of `GroveDb` is on `GroveDb<E>`. A suite written for `E: Engine` opens its database with `GroveDb::<E>::open_with_engine(path)`. The tests still name `GroveDb`, which is the RocksDB engine. A test that reaches under GroveDB to read or damage stored records says so: it calls `RocksDbEngine::storage()` and works on the storage contexts. `merk/src/test_utils` is unchanged and still opens RocksDB contexts. |
| SEAM-07 | What still names RocksDB in `grovedb`, outside `grovedb::engine`: `RocksDbStorage::build_prefix` and `secondary_prefix_for`, the estimated-cost helpers with their `S: Storage` parameter, the debugger, `raw_storage` and `ingest_subtree_sst`, and the `impl GroveDb` blocks of rule 2, which exist for the default engine. With the RocksDB feature off there is no default engine, so those blocks need another home. |
| GROVE-01 | The trait to implement. `Transaction<'db>` has to stand for a write transaction, a snapshot, the transaction of one read and a scope's view. `start_read_transaction` is where RD2 goes: a snapshot held for the operation. `Subtree<'a>` has to be a type of Merk that is a `NodeStore` (see [the RocksDB adapter](node-store.md#the-rocksdb-adapter)) and a `FlatStore`. The conflict maps to `Error::Commit(CommitError::Conflict)`. |
| GROVE-03 | `SubtreeMove`: the re-hosting move behind the stepper, with the charges of the copy. |
| GROVE-05 | `PendingPrefixDrops` on the engine. `FlatStore::clear` on a handle is today's clear of a subtree's data keyspace, Merk nodes included. |
| GROVE-08 | `create_checkpoint`, `open_checkpoint`, and the generic checkpoint constructors of `GroveDb`. |
| GROVE-09 | `raw_storage` and `ingest_subtree_sst` are on the RocksDB engine only. |
| GROVE-12 | `open_write_scope` and `scope_transaction`: an operation runs in a scope when it is given the scope's view. The `Send` bounds are asked for where a scope is sent. |
| PERF-03 | `Applied`, returned by the application of a batch. |
| OPS-03 | `grovedb::Error::is_commit_conflict()` in place of the match on `rocksdb::ErrorKind::Busy`. `GroveDb::set_savepoint` and `rollback_to_savepoint` in place of the methods of `Tx`. |

## Choices this task made

The plan left these open. Later tasks build on them.

1. **The blanket `impl<S: StorageContext> NodeStore for S` stays.** The RocksDB engine's handle is the storage context itself, so `Merk<PrefixedRocksDbTransactionContext>` is still the Merk type on RocksDB and no `KvNodeStore<S>` wrapper is needed. The price is the one SEAM-02 measured: a `NodeStore` that is not a storage context has to be a type Merk defines, so `LigninEngine::Subtree` is `LigninNodeStore`, in `merk::store::lignin`.
2. **One handle is a `NodeStore` and a `FlatStore`**, and the caller names the trait. The alternative, two handle types and two ways to open them, would have asked every call site which one it wants at the open, where the element is often not read yet.
3. **Errors are `grovedb_storage::Error`**, with the engine-neutral outcomes as variants of it (`Commit`, `WriteScope`), next to `SnapshotReadOnlyTransaction`, which is the engine-neutral `ReadOnly` already. It is the error that crosses all three seams, so a scope's `Overlap` can come out of a `FlatStore::put`. This put two variants and two types into `storage/src/error.rs`, outside the location the register names.
4. **A conflict is a class of error, not a new error on RocksDB.** `commit_error()` reads the class off the value RocksDB returns.
5. **A write scope lends a view that has the type of a transaction.** GROVE-12 may want more than that, such as a scope that owns a Merk cache. RocksDB is removed before scopes are used, so its part of the choice is small.
6. **The RocksDB engine cannot roll back a scope**, and says so with a typed error.
7. **No `Send` bound** on `NodeStore`, `FlatStore`, the handles, the batch or the transaction.
8. **The pinned view is a cursor that can be retargeted**, not a view type of its own. [node-store.md](node-store.md#call-sites-outside-merk) sketched "a pinned read view from which a cursor for any subtree can be taken". On RocksDB the view is one iterator, which serves one subtree at a time, and that is what the function says.
9. **Two fee rules are stated by the RocksDB engine's code** until SEAM-05 moves them: the removal of a moved record from its old prefix is staged as a delete with an empty cost (one seek, nothing freed), and what `settle_batch` charges is `continue_write_batch`'s rule.
10. **Static functions that name no engine stay on the default engine** (rule 2 above). They would otherwise need an engine written out at every call, in Platform too.
11. **Savepoints are operations of the engine and of `GroveDb`**, so that an engine can refuse one while a scope is open. `Tx::set_savepoint` and `Tx::rollback_to_savepoint` stay, and Platform calls them.
12. **`GroveDb`'s lifetimes in generic code** are as described under [Lifetimes](#lifetimes).
13. **`merk/src/test_utils` is unchanged.** SEAM-06 decides what it becomes.
14. **A read that is given no transaction starts one of its own kind**, `start_read_transaction`, so that an engine can tell it from a write and pin it (RD2). Which operations read was decided by one rule: an operation that never commits the transaction it started. A read starts exactly one, and hands it on. This changed how many transactions a read starts on RocksDB, from one for each part of the operation to one, and nothing a caller can see.
15. **A test that reaches under GroveDB names the RocksDB storage**, through `RocksDbEngine::storage()`. It reads or damages records as RocksDB stores them, and no other engine has those.
