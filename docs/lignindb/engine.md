# Engine: the seam between GroveDB and its storage engine

LigninDB task SEAM-04 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

`Engine` is everything `GroveDb` asks of a storage engine. It lives in `grovedb/src/engine/mod.rs`. `GroveDb<E: Engine = RocksDbEngine>` is generic over it. It has two implementations: `RocksDbEngine` (`grovedb/src/engine/rocksdb.rs`), which is `grovedb-storage` behind the trait, and `LigninEngine` (`grovedb/src/engine/lignin`, GROVE-01), which is `lignindb` behind it: [The LigninDB engine](#the-lignindb-engine). GroveDB builds with no engine at all: [Features and engines](#features-and-engines) (SEAM-07).

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
LigninEngine                 lignindb::Db, LigninTransaction, LigninBatch, LigninNodeStore
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
| `subtree_beside(path, own, prefix, batch, &tx)` | A deferred handle of the subtree `prefix`, which the element at `path` hosts beside the subtree of its own path, `own`: a secondary Merk of an indexed tree. Provided by the trait as `subtree_by_prefix(prefix, batch, &tx)`. | the provided call | `zero` |
| `immediate_subtree(path, &tx)` | An immediate handle of the subtree at a path. | `Storage::get_immediate_storage_context` | `P-path` |
| `immediate_subtree_by_prefix(prefix, &tx)` | An immediate handle of the subtree with a prefix. | `Storage::get_immediate_storage_context_by_subtree_prefix` | `zero` |
| `settle_batch(&mut group, batch)` | Works out what a batch costs and adds its writes to a write group. Nothing is applied. | `RocksDbStorage::continue_write_batch` | the seeks and loaded bytes of `B-commit` |
| `apply_write_group(group, &tx)` | Applies a write group to the transaction, all of it or nothing. | `RocksDbStorage::commit_db_write_batch` | the pending storage cost of `B-commit`, if the application succeeds |
| `apply_batch(batch, &tx)` | `settle_batch` and `apply_write_group` in one step. Provided by the trait. | the two calls above, which are what `Storage::commit_multi_context_batch` makes | `B-commit` |
| `retarget_cursor(cursor, prefix)` | Points a cursor at another subtree. It keeps the state it reads from. | `PrefixedRocksDbRawIterator::retarget` | `zero` |
| `move_subtree(from, to, batch, &tx)` | Starts to move the records of one subtree's data keyspace to another prefix. See [Moving a subtree](#moving-a-subtree). | two contexts by prefix, `raw_iter`, `seek_to_first` | `I-move` |
| `pending_prefix_drop_records()`, `delete_prefix_ranges(prefixes)`, `remove_pending_prefix_drop_record(prefix)` | The supertrait [`PendingPrefixDrops`](flat-store.md#pending-prefix-drops). | the `RocksDbStorage` methods of the same names | `DB` |

Prefix derivation is not an operation of the engine. A subtree's prefix is the Blake3 hash of its path on every engine, because state sync chunk ids carry it, and what the derivation charges is a fee rule (`P-path`, `P-sec`). The two functions are `grovedb_storage::prefix::build_prefix` and `secondary_prefix_for`, which name no engine and are compiled with none (SEAM-07). GroveDB calls them there. `RocksDbStorage::build_prefix` and `RocksDbStorage::secondary_prefix_for` are the same functions under their old names, for the RocksDB contexts and for callers outside GroveDB.

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

Three things stay with the RocksDB engine: the hatches `GroveDb::raw_storage` and `GroveDb::ingest_subtree_sst` (feature `unsafe-dump-load`), which are on `GroveDb<RocksDbEngine>` by name and which GROVE-09 replaces, and `GroveDb::start_visualizer` (feature `grovedbg`), because the debugger holds a `GroveDb` with no engine named. All three are compiled with the `rocksdb_storage` feature only.

### Threads

- An `Engine` is `Send + Sync`. So is `GroveDb<E>`.
- `Transaction`, `Batch`, the handles and `WriteScope` have no `Send` bound, and neither have `NodeStore` and `FlatStore`. On RocksDB a transaction is `Send`, and a storage context and a scope are not (a context holds `&StorageBatch`, whose operations sit in a `RefCell`). Nothing on RocksDB needs them to be: one thread at a time writes a transaction.
- `Send` is asked for where something crosses threads. That is GROVE-12, which sends a scope to a worker: it asks `E::WriteScope<'a>: Send`, and for the handles opened in it. LigninDB's types meet that (MERK-01: `LigninNodeStore` and a Merk on it are `Send`). A Merk is `Send` whenever its store is (SEAM-01).

## GroveDb<E>

```rust
pub type DefaultEngine = RocksDbEngine;
pub struct GroveDb<E: Engine = DefaultEngine> { db: E }
pub type Transaction<'db, E = DefaultEngine> = <E as Engine>::Transaction<'db>;
pub type TransactionArg<'db, 'a, E = DefaultEngine> = Option<&'a Transaction<'db, E>>;
```

`GroveDb`, `Transaction<'db>` and `TransactionArg<'db, 'a>` with no engine named are what they were: `GroveDb<RocksDbEngine>`, `Tx<'db>` and `Option<&'a Tx<'db>>`. Platform builds against this branch with no change to its source.

`grovedb::engine::DefaultEngine` is the one place that says which engine that is. In GroveDB's own unit tests it is the engine the suite was built for instead, which is how the tests, naming no engine, run on every registered engine ([`test-engines.md`](test-engines.md), SEAM-06).

Three rules decide where a function of `GroveDb` lives:

1. **A method that takes `self` is generic**: `impl<E: Engine> GroveDb<E>`. That is every operation: reads, writes, batches, queries, proofs, state sync, checkpoints, aux, the append-only trees.
2. **An associated function with no `self` and no engine type in its signature is on the default engine**: `impl GroveDb`. `GroveDb::verify_query(..)`, `GroveDb::open(path)` or `GroveDb::estimated_case_operations_for_batch(..)` name no engine, and Rust does not use the default of a type parameter to infer one, so such a call resolves only if exactly one `impl` has the function. This is how `HashMap::new()` works. It covers the proof verifiers, the estimated-cost helpers, and the constructors `open`, `open_checkpoint` and `delete_checkpoint`.
3. **The constructors have a generic twin**: `GroveDb::<E>::open_with_engine(path)`, `open_checkpoint_with_engine(path)`, `delete_checkpoint_with_engine(path)`, and `GroveDb::from_engine(engine)` for an engine opened with options of its own. `GroveDb::engine()` gives the engine back. `open_with_cidx_integrity_check` has none: it is `open`, then `verify_grovedb`, and an error if that reports an issue, which a caller with another engine writes out.

Without the `engine_generic` feature there is no `GroveDb<E>`, and `GroveDb` is a plain struct that names the proof verifiers, as before.

### Features and engines

GroveDB's Cargo features separate what is written for any engine from the engine that is compiled in (SEAM-07):

| Feature | What it compiles | RocksDB |
| --- | --- | --- |
| `engine_generic` | `Engine`, `GroveDb<E>` and every operation on it, batches, proofs, references, state sync, the proof verifiers | no |
| `estimated_costs` | `engine_generic` and the estimated costs. It was `full` before, so a build with this feature alone had RocksDB and `GroveDb::open`, and now has neither | no |
| `rocksdb_storage` | `engine_generic` and `RocksDbEngine`: the default engine, `GroveDb::open`, `open_checkpoint`, `delete_checkpoint` and `open_with_cidx_integrity_check` | yes |
| `unsafe-dump-load` | `rocksdb_storage` and the hatches `raw_storage` and `ingest_subtree_sst`. Before, it turned on the storage crate's feature only, and the hatches came with `minimal` | yes |
| `grovedbg` | `full` and the debugger, with `start_visualizer` | yes |
| `lignin_storage` | `engine_generic` and `LigninEngine`, with the LigninDB crates. It is in no other feature and not among the defaults. Without `rocksdb_storage` it is the default engine, and `GroveDb::open`, `open_checkpoint`, `delete_checkpoint` and `open_with_cidx_integrity_check` are on it | no |
| `minimal` | `engine_generic` and `rocksdb_storage`, which is what it compiled before | yes |
| `full` | `minimal` and Merk's `full`: its test helpers and coloured `Debug` output | yes |

The default is `full` and `estimated_costs`.

- In the sources of `grovedb`, code is under `#[cfg(feature = "engine_generic")]` where it was under `minimal`, and what needs RocksDB is under `rocksdb_storage`. Nothing in `grovedb` is gated on `minimal` any more. Upstream writes `minimal`: after a merge of `develop`, a gate it added has to be renamed, or the item is missing from the build with no engine. `sed -i 's/feature = "minimal"/feature = "engine_generic"/g'` over `grovedb/src` is the whole change this task made to those gates. The benches and the integration tests under `grovedb/tests` open a database, so there the same `sed` wrote `rocksdb_storage`, and each bench has `required-features = ["rocksdb_storage"]`: a feature set that has the engine and not `minimal` by name, such as `unsafe-dump-load` alone, builds and runs them, and one with no engine leaves the benches out.
- Merk's `minimal` compiles no engine either. Its code under that feature never named one. The feature only switched `grovedb-storage/rocksdb_storage` on, which Merk's own `rocksdb_storage` does now, and with it `test_utils` (for `TempMerk`, which opens a RocksDB storage) and `full`. Merk's own tests that open a store are under the cfg of Merk's suite, `merk_test_engine` ([`test-engines.md`](test-engines.md), SEAM-06), which is set where the engine of that suite is compiled in. `cargo test -p grovedb-merk --no-default-features --features minimal --lib` builds and runs the others.
- A build with `minimal` and `estimated_costs` and not `full`, which is Platform's, got Merk's `full` through `estimated_costs` before. It keeps `colored_debug`, which `estimated_costs` still turns on so that the `Debug` output of a Merk node is what it was, and loses Merk's `test_utils` module, which Platform does not name.
- `grovedb-storage` without `rocksdb_storage` is the seams: `FlatStore`, the storage traits, the multi-context batch, the errors, `prefix` and `worst_case_costs`.

With `engine_generic` and no engine compiled in (neither `rocksdb_storage` nor `lignin_storage`), `GroveDb` with no engine named is `GroveDb<NoEngine>`. (With `verify` alone there is no `GroveDb<E>` at all, as above.) `NoEngine` (`grovedb::engine::NoEngine`) is not an `Engine` and has no value, so there is no database of it. It is where the associated functions of rule 2 live in such a build: the proof verifiers and the estimated costs resolve as `GroveDb::verify_query(..)` and `GroveDb::average_case_for_get(..)` there too. `Transaction<'db, E>`, `TransactionArg<'db, 'a, E>` and `MultiStateSyncSession<'db, E>` have no default for `E` in that build. With `rocksdb_storage`, all four are what they were, and `NoEngine` does not exist. With `lignin_storage` alone their default is `LigninEngine`. So code names `GroveDb`, never `GroveDb<NoEngine>`: a feature another crate of the build turns on would take the name away.

The fifteen estimated-cost helpers that take a storage type parameter (`GroveDb::add_worst_case_get_merk_at_path::<S>` and the others, `S: Storage`) never used it. Each is now a call of a function of the same name with `_engine_free` appended and no type parameter, which holds the estimate. GroveDB's own code calls the engine-free one. A caller that writes `::<RocksDbStorage>`, as Platform does for two of them (`average_case_delete_operation_for_delete` and `average_case_delete_operations_for_delete_up_tree_while_empty`, six call sites outside its tests), compiles as before, and OPS-03 moves it to the engine-free name.

The tests of `grovedb` that open no database run with no engine compiled in: `cargo test -p grovedb --no-default-features --features estimated_costs --lib --tests`. CI runs that command, after it has checked that no RocksDB crate is among the crates of the build and that no gate in `grovedb` says `minimal`. It also builds `engine_generic` alone. What decides whether a test is in it:

- `grovedb/src/tests` is compiled where the suite has an engine, under the cfg `grovedb_test_engine` ([`test-engines.md`](test-engines.md), SEAM-06): its fixtures open a database on the engine of the suite. The cfg is set where the Cargo feature of that engine is on, which for both engines registered today is `rocksdb_storage`. Without it, `tests` is `grovedb/src/tests_without_engine.rs`, which takes five files of that directory by path: four files of tests of the estimated costs, which call functions of sizes only, and `faultable_reads`, the store the library reads a parent element through in every test build. No other file of the directory is in the build, whether or not its tests open a database.
- A test module inside a source file that opens a database, or takes a fixture of `tests`, is under `#[cfg(all(test, grovedb_test_engine))]`. One that does neither stays under `#[cfg(test)]` and runs in both builds. In three modules the gate is on each test that opens a database or takes a fixture, so the others run with no engine: the two of the batch estimator (`batch/estimated_costs`) and the one of `query`. A module gated as a whole may hold tests that open none: they are left out of the build with no engine, not lost, and run with the default features.
- The two test modules of `grovedb/src/estimated_costs` compare an estimate with an operation on a Merk of Merk's `test_utils`, so they are under `feature = "full"`, which is what turns that module on. Before, `estimated_costs` was `full`.
- An integration test under `grovedb/tests` that opens a database is under `rocksdb_storage`. Three files hold tests that only decode proofs, and those run in both builds.
- The doc tests open a database, so the command has `--lib --tests` and leaves them out.

A test that needs a database needs an engine for the suite, not RocksDB by name: the gates above are on the cfg of the suite (SEAM-06). The type default of `GroveDb` is there where an engine is compiled in: with `rocksdb_storage` it is the RocksDB engine, and with `lignin_storage` and without `rocksdb_storage` it is the LigninDB engine. In the test build with no engine, what only the left-out tests call is unused, and `dead_code` is allowed for the crate in that build alone.

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

Functions that take a `Merk<S>` are bounded by `S: NodeStore<'db>` and not by an engine, so a test can run them on a store of its own. `FaultableReads` (`grovedb/src/tests/faultable_reads.rs`) is such a store: it wraps any `NodeStore` and fails the node reads of an armed key with a storage error. `open_merk` reads a subtree's parent element through it in test builds, on every engine. The ten tests of `parent_read_fault_tests.rs` (GroveDB #1056) open their handles with `Engine::subtree` and read them as a `NodeStore` or a `FlatStore`, so they name no storage context. The fixture that opens their database opens the engine of the suite ([`test-engines.md`](test-engines.md)), so they run on every registered engine. `a_failed_parent_read_is_a_storage_error_on_a_second_engine` names its engine, `PinnedReads`, and runs on it in every run.

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
| `grovedb/src/tests/one_transaction_per_operation_tests.rs` | `GroveDb` on a second engine, `PinnedReads` (`grovedb/src/tests/pinned_reads.rs`): the RocksDB engine, except that the transaction of a read is a read-only snapshot and the engine counts the transactions it starts. It is also registered as an engine of the whole suite ([`test-engines.md`](test-engines.md)). Read operations given no transaction each start exactly one read transaction and no write transaction, among them a `get` that follows a reference, a query with a subquery, the getters of an MMR tree and the five reads of the big sum axis. None of these starts a snapshot. A proof starts the snapshot it pins and one read transaction for the row that is a reference. A proof of a layer under a per-instance cap, which proves that layer twice, starts one snapshot and nothing else, and its bytes are those of the default engine. `insert`, `delete`, `apply_batch`, `put_aux` and the four entry points of the indexed trees with a 128-bit sum each start one write transaction and no read, and an MMR append starts one of each. So do the conversion of a tree to a 128-bit sum (GroveDB #1063), of a plain tree and of an indexed one, and a repair of an indexed tree that has a row to remove from a ranking. With a transaction of the caller's, none is started. A parent read that fails in a handle of this engine comes back as the storage error, and a parent that is not there as absence. |
| `a_grovedb_is_opened_on_an_engine_and_sets_savepoints_through_it` | `open_with_engine`, `from_engine` and `engine()`, and `GroveDb`'s savepoints: a rollback undoes what came after the savepoint, and both are refused while a scope of the transaction is open. |

Above the engine, the golden corpus (BASE-02) replays unchanged on the branch, and so do the GroveDB and Merk suites.

## The LigninDB engine

`LigninEngine` (`grovedb/src/engine/lignin`, GROVE-01, Cargo feature `lignin_storage`) is one open LigninDB database, `lignindb::Db`. `LigninEngine::open_with(path, &Options)` opens one with options of the caller's, `LigninEngine::new(db)` takes one that is open, and `database()` gives it back. Its handles are the node store of Merk on LigninDB ([node-store.md](node-store.md#the-lignindb-store)), and its batch is that store's batch.

| Associated type | On LigninDB |
| --- | --- |
| `Transaction<'db>` | `LigninTransaction<'db>`: a write transaction (`lignindb::txn::WriteTxn`), a snapshot (`lignindb::snapshot::Snapshot`), or the view of a write scope |
| `Batch` | `LigninBatch` of `merk::store::lignin`, which is `lignindb::batch::StagedBatch` with what each write is charged |
| `WriteGroup` | `LigninWriteGroup`: the settled batches (`LigninSettled`) in the order they were settled |
| `Applied` | `LigninApplied`: where the written nodes are (`tree-layout.md` P5), for a handle that stays open (`LigninNodeStore::take_locations`) |
| `Subtree<'a>`, `ImmediateSubtree<'a>` | `LigninNodeStore<'a>`, deferred or immediate. `Aux<'_>` and `Meta<'_>` are `LigninKeyspace` |
| `SubtreeMove<'a>` | `LigninSubtreeMove`, whose first step is refused (GROVE-03) |
| `WriteScope<'a>` | `LigninWriteScope<'a>`: a scope of the transaction (`lignindb::scope`) |

### Each operation

| Operation | On LigninDB |
| --- | --- |
| `open(path)` | `Db::open` with the default `Options`, which makes the database if the directory has none. A directory of another engine is refused and left as it is (the module documentation of `lignindb::db`). Then the root Merk and the namespace of the pending-drop records are entered, if they are not: see [Subtrees](#subtrees-are-entered-by-the-first-batch-that-writes-into-them). |
| `open_checkpoint(path)` | refused, `Error::NotSupported` (GROVE-08). The engine makes no checkpoint yet and has no way to tell one from the directory of a database, and `GroveDb::delete_checkpoint` removes whatever this opens: it removes nothing while this is refused. |
| `create_checkpoint(path)` | refused, `Error::NotSupported`: the facade has no checkpoint of an open database yet (GROVE-08). |
| `flush()` | `Db::flush` (DU1a). |
| `wipe()` | `Db::wipe`, one commit (CV3), and then the root Merk and the namespace of the pending-drop records are entered again, each as a commit of its own. A write that another thread commits in between and that writes into one of them enters it itself, as the first write into any subtree does, and the wipe takes that one as entered. Every other way one of the two commits ends without being published and durable is the error of the wipe, which has wiped by then. Either subtree can then be entered or not: an enter that failed has entered nothing, an enter that was published and is not known to be durable has entered its subtree and is still the error, and a write of another thread may have entered either. A missing one is entered by the next write into it while the session takes writes, and by the next open of the database otherwise: after a commit that is not known to be durable the session takes no write until the database is opened again. One commit for all three needs an enter in `Db::wipe`. |
| `start_transaction()` | `Db::begin`. Any number are open, and one commits at a time. |
| `start_snapshot_read_transaction()`, `start_read_transaction()` | `Db::snapshot`: both are pinned (RD1, RD2). |
| `commit_transaction(tx)` | `Db::commit` with `NoStaleHashes`, the rules of every existing GroveVersion: a Merk hashes its tree before it commits, so no hash is left to the engine. The transaction is dropped however the commit ends. A snapshot refuses with `SnapshotReadOnlyTransaction`. Charged nothing. |
| `rollback_transaction(&tx)` | `Db::rollback`. On a snapshot it does nothing. |
| `set_savepoint`, `rollback_to_savepoint` | `WriteTxn::set_savepoint` and `rollback_to_savepoint`, refused with `ScopesOpen` while a scope is open. A snapshot counts its savepoints and has nothing to undo. |
| `open_write_scope`, `scope_transaction`, `merge_write_scope`, `rollback_write_scope` | `WriteTxn::scope`, kept behind the store's `LigninWriter`; the view is a `LigninTransaction` of the scope; `Scope::merge` and `Scope::rollback`. A scope that is dropped is rolled back. |
| `subtree(path, batch, &tx)`, `immediate_subtree(path, &tx)` | the prefix of the path (`build_prefix`, charged `P-path`), and a handle on the transaction's view. Where the handle can write, it also names the element that hosts its subtree: the key of the path's last segment in the subtree of the segments before. That second prefix is not charged. |
| `subtree_by_prefix`, `immediate_subtree_by_prefix` | a handle of the prefix, charged nothing. Where it can write, its host is read from the subtree directory as the transaction has it: the element of a subtree that is entered and is the first its element hosts. A subtree that is not entered, one at a later position of its element and one whose entry the transaction does not let the handle read name none. |
| `subtree_beside(path, own, prefix, batch, &tx)` | a handle of the prefix, charged nothing. Where it can write, it names the element at `path` as its host, and `own` as the first subtree of that element: [An indexed tree hosts several Merks](#an-indexed-tree-hosts-several-merks). The prefix of the Merk that holds the element is derived for it and not charged. |
| `settle_batch(&mut group, batch)` | `LigninBatch::settle`: the batch is resolved to one outcome for each key, and charged as a storage batch is at once (`cost-model.md` B1). A delete that came with no cost is charged from the record that the committed state holds (RD10), read where the delete is come to, through a write transaction of the engine's own that nothing is written into. The storage cost joins the group's pending cost. |
| `apply_write_group(group, &tx)` | the subtrees the group has to enter are worked out ([Subtrees](#subtrees-are-entered-by-the-first-batch-that-writes-into-them)), and every settled batch is applied to the transaction, one operation each, in one scope: all of them or none. The pending cost is charged if the transaction took them. A snapshot is asked for nothing and refuses the group. |
| `retarget_cursor(cursor, prefix)` | `LigninCursor::retarget`. A cursor of a snapshot goes on in its snapshot. A cursor of a write transaction is refused: it stands on no node and its `status` is `Error::NotSupported`. |
| `move_subtree` | a move whose first step is refused, `Error::NotSupported` (GROVE-03). |
| `pending_prefix_drop_records()` | the meta records of the pending-drops namespace, read through a snapshot. |
| `remove_pending_prefix_drop_record(prefix)` | one write with no transaction (`Db::write`), which deletes the record. |
| `delete_prefix_ranges(prefixes)` | refused, `Error::NotSupported` (GROVE-05). |

### What a transaction is

- **A write transaction has no writer of its own.** Its handles read it from outside every scope (`transactions.md` RD3), and each write, the application of a batch or one write of an immediate handle, goes through a scope that is opened for it and merged when it is made, or rolled back if the transaction refuses it (SC1). So no scope is open between two calls, and a savepoint, a rollback and the commit find every scope closed (SC8). A scope that is opened while no other is open has its writes laid out as it makes them, so this is the path of one writer.
- **A read with no transaction is a snapshot** held for the operation (RD2), and so is a snapshot read transaction (RD1). The subtree directory is asked for a subtree by the first read of it through the snapshot, and the entry is kept for the later ones.
- **A write with no transaction** is made by `GroveDb` as a write transaction of its own, committed at once (RD7). It waits for no open write transaction. An open one reads it wherever it has not written the key itself, and its commit then fails with `Error::Commit(CommitError::Conflict)` exactly where the two wrote a key (CF2): the rule of RocksDB's optimistic transactions. On existing GroveVersions any two tree changes share the root node of the root Merk (GM1), so a transaction that has changed the tree conflicts with every tree change committed beside it, and not with an aux record.
- **A write scope's view** reads as the scope does and writes through it. A group of more than one settled batch is refused through a view: a scope that lives on cannot take a part of them back.

### Errors

| LigninDB | At the seam |
| --- | --- |
| a commit that met another (`CommitError::Conflict`) | `Error::Commit(CommitError::Conflict)` |
| a commit that is published and not known to be durable (`CommitError::NotDurable`) | `Error::Commit(CommitError::NotDurable)` |
| a commit the log cannot say it holds (`CommitError::Unsettled`), and one that is in the log and could not be installed (`CommitError::Halted`): the session takes no further commit, and the next open settles whether the commit happened | `Error::Commit(CommitError::Unsettled)` |
| a scope is open (`TxnError::ScopeOpen`, `SavepointError::ScopesOpen`) | `Error::WriteScope(WriteScopeError::ScopesOpen)` |
| another open scope holds the unit (`TxnError::ScopeOverlap`), from an operation and from a read or a write of a handle | `Error::WriteScope(WriteScopeError::Overlap)` |
| the write of an immediate handle on a snapshot, the application of a batch to one, its commit, a write scope on it | `Error::SnapshotReadOnlyTransaction`. A deferred handle on a snapshot stages into its batch, as on RocksDB, and the batch is refused where it is applied to the snapshot |
| a group of more than one settled batch through the view of a write scope; a cursor of a write transaction that is pointed at another subtree | `Error::NotSupported` |
| a commit that found the log ahead of the session (`Failure::VersionGap`), and a write to a session that takes none (`WriteError::Reopen`) | `Error::StorageError`, whose words say that the database has to be opened again |
| a cursor that was refused or whose move failed | its `status`: `WriteScope` and `NotSupported` as they are, anything else as `Error::StorageError` with the words |
| a Merk more than 65,535 Merks deep (`DirectoryError::TooDeep`), from an operation or from a commit; an operation the engine has not; a first write into a subtree under a key that holds no element | `Error::NotSupported` |
| anything else | `Error::StorageError` with the engine's words |

`Error::NotSupported` is a variant of `grovedb_storage::Error` that this engine added. RocksDB has no limit on the depth of a grove, and never returns it. `From<grovedb_storage::Error> for grovedb::Error` makes it `grovedb::Error::NotSupported` and not a storage error, so it is that wherever GroveDB converts the error with `?` or `into`, which is the application of a batch, the commit and the open. Where a call site puts a storage error into words of its own, as the delete of a tree does for a failed clear (`CorruptedData`), the caller gets that error with the engine's words in it.

### Subtrees are entered by the first batch that writes into them

LigninDB reads and writes no record of a subtree that its subtree directory has no entry for (`transactions.md` CV9). On RocksDB a subtree is its prefix, and GroveDB says of no operation which subtrees it makes. So the engine says it, where a group is applied (`grovedb/src/engine/lignin/enter.rs`):

- A subtree that a batch holds a write for, in any column, and that neither the transaction nor a batch of the group has entered, is entered with the group: by the batch that enters the subtree its host is in, whether that batch enters it itself or the engine enters it with that batch by this rule, and by the first batch where the subtree its host is in is entered already or the subtree has no host. A batch knows the subtrees it writes into from the handles that staged the writes (`LigninSettled::written`); a write that a later operation of the batch took back is none. A batch that enters a subtree itself (`LigninBatch::enter`) has said its host and its tree type, and the engine looks at neither.
- Its host is the element its handle named: the last segment of the path it was opened by, or what the directory holds for a handle by prefix, which is none for a subtree that is not entered. A subtree is entered after the one its host is in. Under a key that holds no element once the group is applied, because none was ever written or the group deletes it, the first write is refused with `Error::NotSupported` and nothing of the group is applied: the directory would name a host that is no node, or none for a subtree that an element inserted later points at. RocksDB takes such a write and keeps records that no element leads to.
- Its tree type is the one of that element: from the value that the last batch of the group to state one states for it, and from the transaction where no batch of the group states a value, because none writes the element or those that do keep the value it has. With an element that is no tree, and for a subtree with no host, the directory keeps no tree type, and `Merk::open_from_directory` refuses the subtree as it refuses any other bytes. Nothing in GroveDB opens a Merk from the directory yet.
- The root Merk has no element, and is entered as a tree of the normal type. The namespace of the pending-drop records has none either, and keeps no tree. The engine enters both when it opens a database that does not have them, a new one, and after a wipe; a batch that finds one gone enters it as any other subtree.

So a tree that was inserted and never written into is in no directory, and is read as the empty tree it is: an open of it costs what it costs on RocksDB and finds no node. The enter is charged nothing and is a write of its transaction: it is rolled back with it. It is no key of the conflict rule, so two transactions that enter one subtree meet as described below.

What this leaves to later tasks:

- **A subtree is not dropped when its element goes** (GROVE-05). After a delete of a tree that was written into, the directory still has the entry, whose host is no node any more: the physical checker reports it (`EntryHost`), and a tree inserted under the same key again finds the old entry.
- **The nodes of a Merk are not removed through its flat keyspace** (GROVE-05). `FlatStore::clear` on a handle is the walk of the subtree's flat records, which is what it is on RocksDB for a tree that keeps its data flat and for a subtree with no record. The nodes of a Merk are no flat records on LigninDB, so where the subtree's Merk has a node the clear is refused with `Error::NotSupported` and removes nothing. The delete of a tree whose Merk holds elements is therefore refused, and changes nothing; a tree with no element is deleted, and so is a tree that keeps its data flat, whose records the walk removes. `Merk::clear`, which `clear_subtree` uses for a Merk, removes the nodes through the node store and is not affected.
- **An element that hosts more than one Merk is refused** (GROVE-04): the Merks of an indexed tree are named together, by the element, and a handle knows one prefix. A group fails with `Error::NotSupported`, and nothing of it is applied, where it writes into a subtree whose element it states as an indexed tree, and where the engine would enter a subtree under one. A batch that enters a subtree itself is not looked at: that is the statement by which the Merks of such an element are named.
- **An entry keeps what it was entered with, and outlives its element** (GROVE-05, MERK-09). A tree whose element is written again as a tree of another type keeps the tree type and the host of its entry. A tree that was written into, emptied and deleted keeps its entry, whose host is no node any more: the physical checker reports it (`EntryHost`), `Merk::open_from_directory` still opens the subtree with the tree type it had, and a tree inserted under the same key again is written under the old entry. GroveDB's own opens take the root key and the tree type from the parent element and are not affected. A refusal of the delete needs the subtrees a node hosts, which the transaction does not give the engine.
- **Two transactions that both write first into one subtree both enter it**, unless one commits before the other's batch looks, which then finds the subtree entered and enters nothing. The directory takes the enter of the first to commit, and the second is refused. If the first commits between the look of the second's batch and its application, that batch is refused where it is applied, by the directory, with `Error::StorageError` and the engine's words (`BatchError::Entered`), whatever keys the two wrote. If both batches were applied, the second is refused at its commit: as a conflict (CF2) where the two wrote a key, as on RocksDB, which on existing GroveVersions is every pair of writes that change a tree, since both changed the root Merk (GM1); and by the directory, with the same storage error, where they wrote no key in common. That is two writers of different aux or meta records of one subtree nothing has entered, whether a tree that was never written into or a keyspace that no element names. RocksDB commits both. The caller makes the write again and finds the subtree entered. The namespace of the pending-drop records is the one keyspace with no element that GroveDB writes, which is why the engine enters it with the database. See the choices below, item 8.
- **A write scope's flat cursor does not keep the committed state it was opened on** (RD6): it reads the committed state of each move (`node-store.md`, "Flat records"). GroveDB opens no write scope before GROVE-12.
- **An immediate handle enters nothing.** A write through one into a subtree that is not entered is refused by the transaction. State sync restores through immediate handles (GROVE-07), and enters through `LigninNodeStore::enter`.

### Root-hash propagation

GROVE-02. An insert or a delete that has written its Merk carries that Merk's root hash up the grove before its batch is applied: GroveDB writes the element of the Merk's tree again in the Merk above, with the root hash, the root key and the aggregate the Merk has by then, and so on up to the root Merk. It makes these steps whether or not the write changed the root hash, inside the batch of the operation, one Merk at a time, with code that names no engine: `propagate_changes_inner` (`grovedb/src/lib.rs`) for an operation that wrote one Merk, such as an insert or a delete, `propagate_changes_with_batch_transaction` beside it for the delete of a tree that holds elements on grove versions 1 to 3, and `MerkCache::propagate_subtrees` (`grovedb/src/merk_cache.rs`) for an operation that maintains backward references. For a tree whose element hosts one Merk, each step takes the root hash, the root key and the aggregate of the Merk below (`Merk::root_hash_key_and_aggregate_data`), takes the Merk above, which it opens by its path or which the Merk cache holds, and writes the tree's element there again (`update_tree_item_preserve_flag`).

On existing GroveVersions Merk hashes every node before it hands it to the store, so each hash on the way up is the one Merk stated, and each charge is what Merk and the cost rules give for the write ([cost-model.md](cost-model.md), section 15). What the LigninDB engine does with such a step:

- **The handle of each Merk on the way, but the root Merk, names the element that hosts it.** A Merk is opened by its path, so its host is the key of the path's last segment in the subtree of the segments before (`LigninSubtree::host`). The root Merk has the empty path and no host. The batch of the operation holds the nodes of the Merk and, from the step above, the node of that element. Where the batch is applied, the element's cell is made to point at the root cell of the Merk it hosts, and at none for a Merk that has no node ([node-store.md](node-store.md#the-batch-of-an-operation), `tree-layout.md` C3).
- **The value hash that a step states for the element is the one the engine's rules make from that pointer.** For a tree whose data is one Merk a step states `combine_hash(value_hash(value), root)`, where `root` is the root hash of the Merk below, and `NULL_HASH` for a Merk with no node. The engine's rules make the same from the element's cell: `root` is the node hash of the root cell it points at, and `NULL_HASH` where it points at none ([node-store.md](node-store.md#hashes)). Given Merk's rules, the physical checker makes the hash again for the cell of every tree that has a root and compares it with the stored one. The hash that the insert of a tree states, before any step, is the insert's own: the same section has the three kinds of tree whose insert states the plain value hash on grove versions 1 and 2. On existing GroveVersions no hash is left to the engine: the commit runs with `NoStaleHashes`.
- **The engine adds no charge.** A handle by path is charged the prefix of its path, as on RocksDB. The prefix of the host's subtree and the enter of a subtree are charged nothing ([Each operation](#each-operation)).

#### The Merk cache

`MerkCache` keeps the Merks of one operation open, by path, with one batch for all of them. It is used where a write has to maintain backward references: the insert of grove version 4 and later for an element that takes them or replaces one (`insert_on_transaction` v1), the delete of such an element, or of a tree that holds one, where the delete checks for backward references, which is the default (`delete_internal_on_transaction` v2), and the code that follows a reference through the cache (`reference_path.rs`, `bidirectional_references`).

- **The cache holds one Merk for a path.** A Merk it was seeded with (`with_prepared_merk`) is the one its caller opened, and any other is opened by the first `get_merk` of its path that returns a handle. While the cache holds a Merk, a `get_merk` of its path that returns a handle returns one of that Merk. The cache lets go of a Merk when its path is marked as deleted, when a closure that may write it returns an error (the next point), and when the cache ends. A path that was marked as deleted is opened again only when a tree's element is there again in the Merk above, which the cache has to hold.
- **A closure that may write gets the Merk through `MerkHandle::for_merk`, and an error of it drops the Merk.** A Merk whose write failed may not hold the tree that its store and the batch hold: `Merk::apply` takes the tree for its walk and sets one back only when the walk succeeded, so a write that failed in its walk leaves a Merk that reads as empty, and one that failed in its commit leaves a tree that was not written. The cache marks the path as failed and lets go of the Merk. From then on a closure of a handle of that path, `get_merk` of the path and the open of a Merk through it are refused with `Error::CorruptedCodeExecution`, marking the path as deleted leaves it failed, and an end of the cache (`finish_prepared`) that is begun then fails before it opens or writes a Merk. So an operation that went on after such an error carries no root hash up, from that Merk or from another Merk of the cache. The end of the cache writes the elements on the way up through the same closures. A write that fails there ends it with that write's error, and the steps made before it are in the batch of the operation, which `insert` and `delete` do not apply after an error.
- **A closure that only reads gets the Merk through `MerkHandle::read_merk`**, as a shared borrow, and an error of it leaves the Merk in the cache. Following a reference and planning the maintenance of backward references read elements that may not be there, and go on with the same cache.

Both borrows are exclusive and mark an operation boundary in the batch (`EngineBatch::next_operation`), as `for_merk` did before the two were told apart. On RocksDB an operation that returned no error from a `for_merk` closure stages what it staged before.

A subtree of the cache is on the heap for as long as the cache lives, and from the moment it is put there the cache and every handle reach it through one pointer and through shared references made from it. The flag that says whether a closure has the subtree is beside it. A unique reference to the subtree, or a write of it, is made by a closure that holds the flag; by the cache after it has checked that the flag is not held, in a borrow that ends before anything else reaches the entry; and by the end of the cache, which has the cache to itself. So a second handle of a subtree, which a delete that maintains backward references can take while it holds the first, is made with no unique reference to the entry that the first handle points into.

**The cache knows the Merks it holds and no others.** A Merk that something else opens for one of its subtrees in the same transaction is a second Merk of that subtree. GroveDB opens one itself, to read: an insert through the cache that writes a plain reference follows the reference through Merks of its own (`follow_reference_as_stored_for_write`), which read what the transaction holds and not what the batch has staged. A caller can open one that writes: the removal callback of `delete_with_sectional_storage_function` is the caller's code and is called before the node is removed, and nothing refuses a write it makes through the same transaction into the subtree of the delete.

What this leaves to later tasks:

- **Batches** (GROVE-03). `apply_batch` propagates through the cache of the batch, which is not `MerkCache`, and no test here applies a batch through `GroveDb`.
- **Indexed trees** (GROVE-04). The engine refuses a write into an indexed tree, so the steps of `propagate_changes_inner` for an indexed primary and its secondaries are compared with RocksDB by no test here.
- **The delete of a tree that holds elements** (GROVE-05) is refused on LigninDB, so what it propagates is compared for a tree that holds none.

### Choices made here that the plan left open

1. **Scopes map to operations one to one in iteration one** (SC1): a scope for each application, opened and merged inside the call. The alternative, one scope that a transaction keeps for its life, would have to be merged and opened again around every savepoint and before the commit, and a transaction would be a value that borrows itself.
2. **Subtrees are entered by the engine, lazily**, as above. The alternative is a call in the seam with which GroveDB names each subtree where it writes the element; GROVE-04 and GROVE-05 need such a statement for the Merks of an indexed tree and for drops, and this rule can be replaced by it then.
3. **`settle_batch` reads the committed state through a write transaction of its own**, because the seam gives it none, and writes nothing into it: such a transaction reads the latest committed state at each read (RD3), so each delete that came with no cost is charged from the record as it is committed when the delete is come to, as RocksDB does between `continue_write_batch` and the write.
4. **A read with no transaction is pinned** (RD2, `transactions.md` 14.5 D1), which the seam allows and the `pinned-reads` engine of the suite has held GroveDB to.
5. **`Error::NotSupported` in `grovedb_storage::Error`**, mapped by `From` to `grovedb::Error::NotSupported`: a limit of the engine is told apart from a fault of the storage, as the node store does for a write (`node-store.md`, "A Merk too deep for the directory").
6. **A cursor is retargeted in a snapshot only.** GroveDB retargets for local count offsets and for the paginated reads of an indexed tree's secondary; with a write transaction of the caller's those are refused until the transaction can point the committed side of a cursor at another Merk (GROVE-06).
7. **`lignindb` gained five things for the engine**: a node of a Merk read by key through a reader's opened subtree (`Opened::node`); cursors over nodes and over flat records that a reader keeps from one operation to the next (`NodeWalk`, which refuses an operation of another reader, by its version and its lease, and `RecordWalk`, which refuses one of another database or version and takes any reader of the same); three variants of `ReadError` for them; `Db::check`, which runs the physical checker on an open database; and what a resolved batch enters and holds for a key (`Resolved::enters`, `Resolved::held`).
8. **Whether a subtree is owed an enter is read before the group is applied**, in the transaction's view, and an enter is no key of the conflict rule. So of two transactions that enter one subtree the second is refused by the subtree directory, where it is applied or where it commits, and that is a conflict only where both were applied and wrote a key in common (above). The alternative is an enter that the transaction makes only where the subtree is not entered, and that a commit beside it does not refuse; `lignindb` has none. It is the owner's to decide whether the engine gets one, and until then which error the seam gives the second writer.

### How the LigninDB engine is checked

`grovedb/src/engine/lignin/tests` runs with `cargo test -p grovedb --features lignin_storage --lib engine::lignin`, on LigninDB, and where the RocksDB engine is compiled in also on RocksDB, by the same code. Six tests close the database they leave, open it again and have the physical checker read it with Merk's rules for a check (`MerkHashRules::for_check`): the first below, and the tests of the lazy enter, the wipe, the delete of a tree, the two first writers and the restore of a root Merk. So does every history of `tests/nested.rs` that is compared with RocksDB, and the history that the refused fetches are held against, each by the rules of the grove version it ran on.

| Test | What it holds |
| --- | --- |
| `grovedb_opens_inserts_reads_and_reopens_on_lignindb` | A grove is made, written and read: trees of three kinds four deep, 40 items, sum items, a reference, replaced and deleted items, aux records, conditional inserts, reads of what is and is not there, and a transaction that is rolled back to a savepoint and committed. The database is closed and opened again, reads the same root hash and the same elements, and is written again. Every answer, every `OperationCost` and the root hash after every write are those of the same calls on RocksDB. The physical checker finds the closed database sound, with every hash of every node made again by Merk's rules. |
| `a_write_with_no_transaction_beside_an_open_one_behaves_as_on_rocksdb` | The seven steps of [`transactions.md` 14.3](transactions.md#143-the-mainnet-case-step-by-step): each observation, the conflict by its engine-neutral class, and on both engines the same `OperationCost` for each of the twelve calls (T-RD4). |
| `the_recorded_raw_cases_give_their_results_on_lignindb`, `..._on_rocksdb` | The 24 cases of `transactions.md` 16.3 that are cases of the seam (T-RD2, T-RD3, T-CF1, T-SP1), each written once for any `Engine`. The seven others need a range delete outside a transaction, RocksDB's memtables or the ingestion of a file. |
| `the_recorded_grovedb_cases_give_their_results_on_lignindb`, `..._on_rocksdb` | The 21 cases of 16.4 other than B8, each written once for any `GroveDb<E>`. |
| `a_tree_that_was_never_written_into_is_read_as_empty_and_entered_by_its_first_write` | The rule of the section above: no entry for a tree nothing was written into, and after the first write an entry with the element as its host, the element's tree type and the root key. |
| `a_wipe_leaves_a_grove_that_is_written_again` | After a wipe nothing is read, a snapshot from before reads its version, and the grove is written again. |
| `a_handle_by_path_is_charged_the_prefix_of_its_path_and_one_by_prefix_nothing`, `a_deferred_handle_stages_and_an_immediate_one_writes_at_once` | The handles: their costs and write modes, a group of two settled batches, and the data keyspace of a subtree as flat records with its cursor. |
| `a_snapshot_reads_one_version_and_refuses_every_write`, `a_write_scope_is_one_writers_part_of_its_transaction`, `a_cursor_of_a_snapshot_is_pointed_at_another_subtree_in_the_same_state` | The transaction rules of the contract on LigninDB, each refusal by its typed error. |
| `a_subtree_is_entered_under_what_its_handle_and_its_batch_say` | A subtree under an element that is no tree; a first write under a key that holds no element, refused with its group; a tree, a tree in it and an item in that made by one batch, each subtree entered under its element and the outer one first; the host a handle by prefix names; and the root Merk entered by a batch that finds it gone. The physical checker reads the open database after each. |
| `the_element_of_a_subtree_is_the_one_its_group_states_last` | A group of two batches: the first states an element as a tree of another type, the second writes its node with the value it has and writes into its subtree. The subtree is entered with the type the first states, and the group is refused where that is an indexed tree. |
| `a_tree_that_holds_elements_is_not_deleted_yet_and_stays_whole` | The delete of a tree whose Merk holds an item is refused and leaves the root hash and the item. A tree nothing was written into is deleted: it is gone, and the root hash is the one RocksDB has after the same calls. |
| `two_transactions_that_both_write_first_into_one_subtree` | Two inserts into a tree nothing was written into: the second commit is a conflict. Two records with different keys in a keyspace nothing has entered: the second commit is refused, is no conflict and leaves nothing, and made again it is committed. Two records in the namespace of the pending-drop records: both commit. |
| `a_root_merk_is_restored_from_chunks_through_the_engine` | MERK-07. The root Merk of a grove of 300 items is restored from chunks, which a deferred handle on a first grove gives, through an immediate handle of a second in a transaction that is committed: the second grove gives the root hash and every item of the first, and the checker finds it sound. Where the RocksDB engine is compiled in, each chunk and the end of the restore answer what they answer there. |
| `what_the_engine_does_not_do_yet_is_not_supported_and_changes_nothing`, `pending_drop_records_are_read_and_removed_outside_any_transaction`, `the_errors_of_lignindb_are_named_by_what_the_seam_knows` | What is refused and how it is named: a checkpoint made, opened or deleted, a range delete, a move, and an insert into an indexed tree, which leaves the root hash and enters nothing. |
| `nested_inserts_five_levels_deep_on_grove_v1` to `_v5` | Chains of five trees, one inside the other under the root, for every ordered pair of the kinds of tree whose data is one Merk, as a tree and the tree in it: nine kinds on grove versions 1 to 4 and twelve on 5. Each chain is written at its deepest tree (items, sum items where the tree takes them, a reference, a sixth tree that nothing is written into, an item replaced and written again unchanged, deletes, calls that are refused, the tree emptied and written again), and beside the tree at every level above it, the deepest first, with deletes at two of them. Every answer, every `OperationCost` and the root hash after every write are those of the same calls on RocksDB. |
| `chains_in_transactions_and_in_a_second_session_on_every_grove_version` | Chains made inside a write transaction, with a savepoint that is rolled back to and the undone chain made again of other kinds, and inside a transaction that is rolled back whole and written again; the root hash through the transaction and with none; then the database closed, opened again and written at the bottom of every chain. Compared with RocksDB call for call. |
| `backward_references_five_levels_deep_on_grove_v4`, `_v5` | An item that takes backward references at the bottom of one chain, a reference to it at the bottom of a second and a reference to that one at the bottom of a third, for each kind of tree at the bottom: the inserts, the item written again, the references read, a reference deleted, and the item deleted with its referrer. The inserts of these elements and their deletes go through the Merk cache, and the reads follow the references as any read does. Compared with RocksDB call for call. |
| `a_read_refused_inside_a_write_ends_the_operation_and_leaves_the_stored_state`, `a_read_refused_inside_a_write_through_the_merk_cache_leaves_the_stored_state` | On both engines behind handles that refuse one fetch (`RefusedReads`): an insert five trees deep with the fetch of one node refused, at the bottom and in each of the five Merks above. The insert fails with the store's refusal, at the same cost on both engines, and the root hash and every node record of the six Merks of the chain are what they were before it. The insert made again costs and leaves what it does in a database that met no refusal, and after each of these inserts that succeeded both engines hold the same node records. An insert that is made changes a record in each of the six Merks, in the upper four nothing but hashes, so the records show a write at any level. The second test inserts an element that takes backward references, on grove versions 4 and 5. |
| `the_merk_cache_drops_a_merk_whose_write_failed_on_lignindb`, and `merk_cache::tests::a_merk_whose_write_failed_is_dropped` on the engine of the suite | A failed read leaves a Merk in the cache. A write whose walk is refused a fetch fails, and afterwards the handle that wrote, a second handle of the subtree that was taken before the write, a new handle and the open of a Merk under it return `CorruptedCodeExecution`, and so does a new handle after the path was marked as deleted. The end of the cache returns it too, at no cost, with another Merk of the cache written and its root hash owed to the Merk above. |
| `a_kind_of_tree_that_a_grove_version_does_not_make_is_refused_by_both_engines`, `the_grove_versions_make_nine_kinds_of_tree_and_the_latest_twelve` | The three kinds with a 128-bit sum beside a count or in the hashes, which grove versions 1 to 4 do not make, are refused there with the same answer on both engines, and the kinds of the histories are twelve tree types that keep their data in one Merk. |

## What later tasks get

| Task | What it builds on |
| --- | --- |
| SEAM-05 | Every storage charge GroveDB sees comes back from a call on `Engine`, on a handle or on a cursor. The fee rules that sit in the RocksDB engine today are listed under [Choices](#choices-this-task-made), item 9. |
| SEAM-06 | Every operation of `GroveDb` is on `GroveDb<E>`. The tests name `GroveDb`, which in a test build is the engine the suite was built for, and the suite is built and run once for every registered engine. A test that reaches under GroveDB to read or damage stored records says so: it calls `RocksDbEngine::storage()`, works on the storage contexts, and is compiled for the RocksDB engine alone. `merk/src/test_utils` is a harness over a `TestEngine` of Merk's own. All of it is in [`test-engines.md`](test-engines.md). |
| SEAM-07 | Done: see [Features and engines](#features-and-engines). What named RocksDB in `grovedb` outside `grovedb::engine` was `RocksDbStorage::build_prefix` and `secondary_prefix_for`, the estimated-cost helpers with their `S: Storage` parameter, the debugger, `raw_storage` and `ingest_subtree_sst`, and the `impl GroveDb` blocks of rule 2, which exist for the default engine. |
| GROVE-01 | Done: see [The LigninDB engine](#the-lignindb-engine). |
| GROVE-03 | `SubtreeMove`: the re-hosting move behind the stepper, with the charges of the copy. |
| GROVE-05 | `PendingPrefixDrops` on the engine. `FlatStore::clear` on a handle is today's clear of a subtree's data keyspace, Merk nodes included, on RocksDB. On LigninDB it refuses a Merk that has a node, until the drop of a subtree takes its place there. |
| GROVE-08 | `create_checkpoint`, `open_checkpoint`, and the generic checkpoint constructors of `GroveDb`. The LigninDB engine refuses both until a checkpoint is told from a database directory. |
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
13. **`merk/src/test_utils` is unchanged.** SEAM-06 decided what it becomes: the harness of [`test-engines.md`](test-engines.md#merk).
14. **A read that is given no transaction starts one of its own kind**, `start_read_transaction`, so that an engine can tell it from a write and pin it (RD2). Which operations read was decided by one rule: an operation that never commits the transaction it started. A read starts exactly one, and hands it on. This changed how many transactions a read starts on RocksDB, from one for each part of the operation to one, and nothing a caller can see.
15. **A test that reaches under GroveDB names the RocksDB storage**, through `RocksDbEngine::storage()`. It reads or damages records as RocksDB stores them, and no other engine has those.
