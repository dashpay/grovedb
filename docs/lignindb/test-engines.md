# Test engines: the suites run once on every registered engine

LigninDB task SEAM-06 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

GroveDB's and Merk's test suites name no storage engine. Each is built for one engine at a time, and run once for every engine registered for it. This document says how an engine is selected, what a test may name, which tests are about one engine, and what registering an engine takes. It builds on [`engine.md`](engine.md) and [`node-store.md`](node-store.md).

## Running a suite

| Command | What runs |
| --- | --- |
| `cargo test -p grovedb` | GroveDB's suite on the first engine of `grovedb/test-engines.txt` |
| `GROVEDB_TEST_ENGINE=pinned-reads cargo test -p grovedb` | the same suite on the engine of that name |
| `cargo test -p grovedb-merk` | Merk's suite on the first engine of `merk/test-engines.txt` |
| `MERK_TEST_ENGINE=node-store-only cargo test -p grovedb-merk` | the same suite on the engine of that name |
| `.github/scripts/engine-suites.sh run` | both suites on every registered engine, one run after the other: every target on the first engine of a suite, the unit tests (`--lib`) on the others. Arguments follow `cargo test -p PACKAGE`. |

A name that is not registered fails the build, and the message lists the names that are. So does a name whose engine is not compiled in: `GROVEDB_TEST_ENGINE=rocksdb` with the feature `rocksdb_storage` off is an error, not a run with no engine.

The engine is a property of the build, not of the run: one test binary holds the suite for one engine. A second engine is a second build of the crate's tests, and of nothing below the crate.

`cargo nextest run` takes the same variables. CI uses it.

## The registry

A suite has a registry: a text file with one engine a line, the default first. A line is the engine's name and the Cargo feature of the crate that compiles the engine.

| Suite | Registry | Variable | cfg | The type of a name |
| --- | --- | --- | --- | --- |
| GroveDB | `grovedb/test-engines.txt` | `GROVEDB_TEST_ENGINE` | `grovedb_test_engine` | `SuiteEngine` in `grovedb/src/tests/mod.rs` |
| Merk | `merk/test-engines.txt` | `MERK_TEST_ENGINE` | `merk_test_engine` | `SuiteEngine` in `merk/src/test_utils/engine.rs` |

The crate's build script reads the registry and the variable. If the feature of the selected engine is on, it sets the cfg twice: with no value, which says that the suite has an engine, and with the selected name. It declares every registered name as a value of the cfg, so a `#[cfg(grovedb_test_engine = "..")]` with a name that is not registered is a warning, and an error under `-D warnings`.

If the feature is off and the variable is not set, the cfg is not set and the suite has no engine. A registry line whose feature the crate does not have fails every build of the crate, whichever engine is selected. That is the build with no engine compiled in ([`engine.md`](engine.md#features-and-engines)): `cargo test -p grovedb --no-default-features --features estimated_costs --lib --tests` and `cargo test -p grovedb-merk --no-default-features --features minimal --lib` run the tests that open no database. A test that opens one is under the cfg, not under the feature of an engine:

| Gate | What is under it |
| --- | --- |
| `#[cfg(all(test, grovedb_test_engine))]`, `#[cfg(grovedb_test_engine)]` on one test | a test of GroveDB that opens a database or takes a fixture. `grovedb/src/tests` as a whole is under it. |
| `#[cfg(all(test, merk_test_engine))]`, `#[cfg(merk_test_engine)]` | a test of Merk that opens a store, and the part of `merk::test_utils` that is the suite's engine |
| `#[cfg(grovedb_test_engine = "rocksdb")]` | a test of GroveDB that is about the records of RocksDB |
| `feature = "rocksdb_storage"` | code that names RocksDB: `RocksDbEngine`, Merk's two test engines, the tests of Merk's RocksDB adapter |

The two suites have registries of their own because an engine is a different thing at each level. To GroveDB it is an [`Engine`](engine.md). To Merk it is whatever hands out [`NodeStore`](node-store.md) handles, and Merk does not know `Engine`.

The engines registered today:

| Suite | Name | Engine |
| --- | --- | --- |
| GroveDB | `rocksdb` | `RocksDbEngine` |
| GroveDB | `pinned-reads` | `PinnedReads` (`grovedb/src/tests/pinned_reads.rs`): the RocksDB engine, except that the transaction of a read that was given none is a read-only snapshot, as on LigninDB (RD2 of [`transactions.md`](transactions.md)). A read that wrote would be refused, and a read that started a second transaction could read another state. The engine is a type of its own and gives no way to the storage underneath. |
| Merk | `rocksdb` | `RocksDbTestEngine`: a handle is the RocksDB storage context of its subtree |
| Merk | `node-store-only` | `NodeStoreOnlyEngine`: RocksDB behind handles that are a `NodeStore` and nothing else, as a LigninDB handle will be |

The second engine of each suite is there so that the suite is run on an engine that is not `RocksDbEngine` before LigninDB exists. A test that reaches for RocksDB without saying so does not compile on it.

## GroveDB

In a test build of `grovedb`, the engine of the suite is the default engine of `GroveDb`:

```rust
// grovedb/src/engine/mod.rs
#[cfg(not(test))]
pub type DefaultEngine = RocksDbEngine;
#[cfg(test)]
pub type DefaultEngine = crate::tests::SuiteEngine;

// grovedb/src/lib.rs
pub struct GroveDb<E: Engine = DefaultEngine> { db: E }
```

`DefaultEngine` exists where an engine is compiled in. So the tests did not change. `GroveDb`, `Transaction`, `TransactionArg` and `MultiStateSyncSession` with no engine named, `GroveDb::open`, `GroveDb::open_checkpoint`, and the fixtures of `grovedb/src/tests/mod.rs` (`make_test_grovedb`, `make_empty_grovedb`, `make_deep_tree` and the rest, `TempGroveDb`) all mean the suite's engine. A test written upstream, which names no engine, runs on every engine the day it is merged.

Outside a test build nothing changed: `DefaultEngine` is `RocksDbEngine`, and a dependent of the crate sees `GroveDb<RocksDbEngine>`.

The functions of `impl GroveDb` that take no `self` (rule 2 of [`engine.md`](engine.md#grovedbe): the proof verifiers, the estimated-cost helpers, the constructors) are on the default engine, so in a test build they are on the suite's engine too, and `GroveDb::verify_query(..)` resolves on every engine.

### What a test may name

- **No engine.** That is every test of a GroveDB operation. It runs on every registered engine.
- **`Engine`, through `db.engine()` or `db.db`.** A test that opens a handle, a transaction or a batch itself calls the trait (`Engine::subtree`, `Engine::apply_batch`), as the ten tests of `parent_read_fault_tests.rs` do. It runs on every registered engine.
- **One engine, by name.** A test of that engine: `grovedb/src/engine/tests.rs` names `RocksDbEngine`, and `one_transaction_per_operation_tests.rs` names `PinnedReads`. It is the same test in every run, and it is under the feature that compiles its engine (`rocksdb_storage` for both), not under the cfg of the suite.
- **The records of one engine.** A test that reads or damages what is stored, or looks at an error or a file of one engine, is compiled for that engine alone: `#[cfg(grovedb_test_engine = "rocksdb")]`. On RocksDB it reaches the storage through `RocksDbEngine::storage()`.

The tests of the last kind are the ones a new engine has to answer for: each is a behaviour that was checked by looking under RocksDB. They are listed in [RocksDB-only tests](#rocksdb-only-tests-of-grovedb).

A test that only counts the records of a subtree, or asks whether a pending drop is recorded, goes through the seam and runs on every engine. Two helpers of the fixtures do it: `data_keyspace_len(db, prefix, transaction)` walks the node cursor and the flat cursor of `Engine::subtree_by_prefix` and returns the larger count, and `pending_prefix_drop_recorded(db, prefix)` asks `PendingPrefixDrops`. The tests of the flat drop, of moves and of the cleanup of nested indexed trees use them (about 100 call sites that read RocksDB contexts before).

In a test build for an engine other than RocksDB, what only the RocksDB-only tests import is unused, so `unused_imports` is allowed for the crate in that build, and in no other.

### Integration tests, doc tests and benches

The eight files of `grovedb/tests/` (80 tests) use the crate as a dependent does. They are not built with `cfg(test)` set on the library, so `GroveDb` is `GroveDb<RocksDbEngine>` there whatever engine is selected. So it is for the doc tests and the benches. They are not part of what is run once for every engine: the engine of a suite is the engine of the crate's unit tests, and a run on an engine other than the first is of those (`--lib`). The run on the first engine, and CI's test shards, run the integration tests on the engine the crate exports by default. GROVE-10 gives them the suite's engine once an engine other than RocksDB is exported: the cfg `grovedb_test_engine` is set for them as well.

### RocksDB-only tests of GroveDB

78 tests are compiled for the RocksDB engine alone, with the 47 helpers only they call. Each carries `#[cfg(grovedb_test_engine = "rocksdb")]`, so `grep -rn 'grovedb_test_engine = "rocksdb"' grovedb/src` lists them. All of them write or remove a record under GroveDB, in RocksDB's layout, to see what GroveDB makes of a store it did not write:

| File | Tests | What they do under GroveDB |
| --- | --- | --- |
| `tests/verify_grovedb_aggregate_recompute_tests.rs` | 16 | plant an aggregate in a stored node or a secondary row, rebind a root key, and expect `verify_grovedb` to report it |
| `tests/coverage_lib_paths_tests.rs` | 16 | put raw and undecodable records into a subtree or an axis secondary |
| `tests/indexed_tree_secondary_drift_tests.rs` | 15 | make the secondary of a count-indexed tree drift from its primary, write a key over the ceiling, put a record into a secondary's storage that no tree links, and pin the records and the cost of the repair of grove version 4 (the last four came with GroveDB #1063) |
| `tests/verify_grovedb_indexed_tests.rs` | 12 | delete a secondary row, or insert an orphan one, behind the primary |
| `tests/indexed_reference_row_tests.rs` | 8 | overwrite a stored row |
| `tests/provable_sum_tree_tests.rs` | 4 | change a stored value without its hash, or a parent element with consistent hashes |
| `tests/flat_drop_tests.rs` | 2 | plant a root node record, and remove a tombstone's record between two steps of a drain |
| `tests/delete_indexed_tree_tests.rs` | 2 | seed a stale row in a secondary |
| `tests/coverage_round7_tests.rs` | 2 | delete rows from a secondary |
| `batch/move_element.rs` | 1 | bill a move against the stored size of each record, key prefix included |

Besides these, the tests of `grovedb/src/engine/tests.rs` name `RocksDbEngine` and run on it whatever engine the suite is on, as said above, and the tests of the hatches `raw_storage` and `ingest_subtree_sst` (feature `unsafe-dump-load`) are compiled for the RocksDB engine alone, as the hatches are.

Thirteen tests that only read through RocksDB contexts, or committed a batch through the storage, go through `Engine` now (`Engine::subtree`, `Engine::apply_batch`, `NodeStore::cursor`, `NodeStore::has_node`) and run on every engine. So do three of the tests of the repair that came with GroveDB #1063: `drift_axis_secondary`, the helper that makes one axis's secondary drift, writes through a Merk of the secondary and the engine's batch, and the tests that use it alone check on every engine that each row a repair removes leaves storage, where a batch that kept a put over a later delete would keep the row.

One thing the second engine does not catch: `PinnedReads` is a type of its own, and its batch, transactions and handles are RocksDB's. A test that names `StorageBatch` (many write `StorageBatch::new()`), or calls a method of a storage context on a handle it opened through `Engine`, compiles on both engines of today. On an engine with types of its own it does not compile, so nothing can run on the wrong engine unseen, and `SuiteBatch` of the fixtures is the name to use instead.

What the suite still names of RocksDB, for an engine that is built without it (GROVE-10): the storage type parameter of the estimated-cost helpers, `::<RocksDbStorage>`, in the tests that call those helpers by their public names (`misc_coverage_tests.rs`, `automatic_backward_references_tests.rs` and the two modules of `estimated_costs`; the parameter is public API until OPS-10 folds it away); the context type in the imports of `coverage_lib_paths_tests.rs`, whose RocksDB-only helpers take it; and `PinnedReads` itself, which is under `rocksdb_storage`. The prefix of a path is not among them: the tests derive it with `grovedb_storage::prefix::build_prefix` and `secondary_prefix_for`, which name no engine.

What the list means for LigninDB: the behaviour each of these checks, most of it `verify_grovedb` finding a damaged store, has no test on an engine that is not RocksDB until that engine has a way to damage its own records. GROVE-10 decides which of them it needs.

## Merk

Merk's tests are written against `merk::test_utils`:

```rust
pub trait TestEngine: Sized {
    type Transaction<'db>;
    type Batch: Default;
    type Subtree<'a>: NodeStore<'a>;           // deferred: writes go to a batch
    type ImmediateSubtree<'a>: NodeStore<'a>;  // writes at once

    fn open(path: &Path) -> Self;
    fn start_transaction(&self) -> Self::Transaction<'_>;
    fn commit_transaction(&self, transaction: Self::Transaction<'_>) -> CostResult<(), Error>;
    fn subtree(&self, path, batch: Option<&Self::Batch>, transaction) -> CostContext<Self::Subtree<'_>>;
    fn immediate_subtree(&self, path, transaction) -> CostContext<Self::ImmediateSubtree<'_>>;
    fn apply_batch(&self, batch: Self::Batch, transaction) -> CostResult<(), Error>;
}
```

It is the part of [`Engine`](engine.md) that a test of Merk uses, with the same names and the same two write modes. Merk cannot name `Engine`, which is GroveDB's.

| Name | What it is |
| --- | --- |
| `SuiteEngine` | the engine of this build |
| `SuiteTransaction<'db>`, `SuiteBatch`, `SuiteSubtree<'a>`, `SuiteImmediateSubtree<'a>` | its associated types |
| `TempEngine` | a database of `SuiteEngine` in a temporary directory |
| `TempMerk` | a Merk of the root subtree on a `TempEngine`, with `commit`. It is `Merk<SuiteSubtree<'static>>`. |
| `root_merk` | a Merk of the root subtree on any `TestEngine`, writing to a batch or read-only |
| `empty_path_merk`, `empty_path_merk_read_only` | as before: a Merk of the root subtree on a `Storage`, for callers outside the suite |

A test of Merk names these and no storage type. Before this task 13 calls in `test_utils` and the test modules of seven files opened RocksDB contexts themselves (`TempStorage`, `Storage::get_transactional_storage_context`, `commit_multi_context_batch`). They call the trait now.

`test_utils` is compiled in every test build of Merk that has `minimal`, and for other crates with the feature `test_utils`. The trait, `NodeStoreOnly` and the helpers that build trees in memory need no engine. `SuiteEngine`, its aliases, `TempEngine` and `TempMerk` are under `merk_test_engine`. With no engine compiled in, the test modules that open a store are left out and the others run.

The tests that still name RocksDB are tests of RocksDB:

- `merk/src/store/kv.rs`: the tests of the RocksDB adapter of `NodeStore`.
- `merk/src/store/tests.rs`: Merk on the in-memory store against Merk on RocksDB.
- A test that reads a handle as a storage context is compiled for the engine whose handle is one: `#[cfg(merk_test_engine = "rocksdb")]`.

## The append-only trees

The crates of the append-only trees (`grovedb-merkle-mountain-range`, `grovedb-bulk-append-tree`, `grovedb-dense-fixed-sized-merkle-tree`, `grovedb-commitment-tree`, `grovedb-private-document-store`) depend on `FlatStore` and on no engine ([`flat-store.md`](flat-store.md)). Their own tests run on in-memory stores of their own and reach no engine, so there is nothing in them to select.

Their suites on an engine are GroveDB's: `mmr_tree_tests`, `bulk_append_tree_tests`, `dense_tree_tests`, `commitment_tree_tests`, `private_document_store_tests`, `clear_append_tree_tests` and the other modules of `grovedb/src/tests` that insert into these trees, prove them and replicate them. They open their database with the fixtures, so they run on every engine registered for GroveDB.

## CI

`.github/workflows/grovedb.yml`:

- The three test shards run the whole workspace, each suite on its first engine, with coverage. This is unchanged.
- The job `test-engines` runs each suite on each of its other engines: `cargo nextest run -p PACKAGE --all-features --lib` with the suite's variable set. Its matrix is what `.github/scripts/engine-suites.sh matrix` prints, which the job `detect-changes` puts in an output. No engine is named in the workflow, so registering an engine adds its run.
- A change to a registry or to the script runs the tests.

## Registering an engine

1. Add its name to the registry of the suite. The first line stays the default.
2. Give the name its type: a `#[cfg(grovedb_test_engine = "name")] pub type SuiteEngine = ..;` in `grovedb/src/tests/mod.rs`, or the same with `merk_test_engine` in `merk/src/test_utils/engine.rs`, where the type implements `TestEngine`.
3. Run the suite on it. A test that does not compile, or fails, either found a difference the engine has to remove, or is about the records of another engine and gets that engine's `#[cfg]`.

Nothing else names the engine: the build scripts, the script and the workflow read the registry.

To add a suite of another crate, give the crate a registry and a build script like Merk's, and add its line to `suites` in `.github/scripts/engine-suites.sh`.
