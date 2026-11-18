# The cost model as a storage crate

This directory is how the cost model of [`docs/lignindb/cost-model.md`](../../docs/lignindb/cost-model.md) (LigninDB task BASE-06) is checked against the golden corpus. It is not a member of the workspace.

`src/` is a crate with the name and the interface of `grovedb-storage`. Its traits, its multi-context batch and its flat keyspace seam are the real crate's source files, taken by path. Its RocksDB implementation is replaced: the types keep their names (`RocksDbStorage`, `PrefixedRocksDbTransactionContext` and so on) and behind them is `lignin_testkit::cost_model`, a logical state that keeps no node record and charges only through the functions of `cost_model/rules.rs`.

GroveDB names the RocksDB storage types until the `Engine` seam (SEAM-04) is cut, so it cannot be given another store from inside the workspace. `check.sh` therefore lays the workspace out a second time, under `target/cost-model`, with every crate linked from this checkout and `storage` replaced by this crate, and runs Cargo there. No engine source is changed, and RocksDB is not built.

```text
check.sh                      replay the golden corpus on the model
check.sh replay --reference   the million-document reference scale
check.sh replay --only NAME --grove-version N
check.sh dump --out DIR ...   full traces, to compare with a dump from RocksDB
check.sh test -p grovedb-storage --features rocksdb_storage
                              the storage crate's own tests, on the model
check.sh test -p grovedb-merk the test suite of any crate, on the model
check.sh profile [VERSION]    the cost of a few common operations and the
                              rules applied (section 11.1 of the document)
```

A replay is BASE-02's: it compares the cost, result, root hash and proof of every step with the committed corpus and names the first step that differs. After it, the script prints how often each rule was applied (`target/cost-model/rule-hits.tsv`) and fails if the model met a node whose link sizes are not the ones Merk passed with it. A test run leaves its counts in the same file, added up over its test processes.

`check.sh test -p grovedb-storage ...` also runs the model's own unit tests, which the stand-in crate includes; `-- --skip cost_model::` leaves them out. GroveDB's suite passes on the model but for `test_checkpoint_wal_under_threshold_is_preserved`, which looks for RocksDB's WAL files.

When the corpus grows, run `check.sh` again: it replays whatever `lignin-testkit/corpus` holds.

## What the model does not do

- It commits every transaction: there are no commit conflicts.
- It keeps its state in memory. A directory gets a small file that names its database, so for the life of the process a reopen finds what was committed there, and a checkpoint is a database of its own that can be opened either way. Another process finds an empty database.
- It reads no SST files (`ingest_subtree_sst`).

## When this goes away

Once `GroveDb` is generic over its engine (SEAM-04), the model can be an `Engine` inside `lignin-testkit` and run in the workspace's own tests. `src/` then reduces to that implementation, and `check.sh` and `manifest.toml` are deleted.
