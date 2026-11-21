# Platform requirements for LigninDB

Task BASE-05 of the LigninDB plan ([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).
This document lists what Dash Platform (Drive and drive-abci) needs from GroveDB's
storage engine. OPS-03 (Platform integration) is built from it. The transaction,
checkpoint and state-sync tasks are checked against it.

Each requirement cites the Platform call site that creates it, as `path:line` plus the
function, and names the plan tasks that satisfy it. Requirements that no task covers, or
that a task as written does not meet, are collected at the end for the owner.

## Sources

| Source | Commit | Why |
| --- | --- | --- |
| dashpay/platform `v5.1-dev` | `979510a95ae0f987d58fc98bf2e95565187d77b8` | Pins grovedb `22f163c2`, the `develop` commit that `feat/LigninDB` starts from. It is the only development branch that builds against this grovedb version. `v5.2-dev` (`5b5241b0`) pins `89dc7351`, two grovedb commits older. `v6.0-dev` (`654eeea2`) pins `dce8252f`, 49 commits older. |
| dashpay/platform PR [#4648](https://github.com/dashpay/platform/pull/4648), branch `feat/state-sync-v15` | `c7f87b9000c7ade3bed0d9d5ed685cbd12755168` | State sync serving and restoring (ABCI snapshots). No development branch has it yet. The PR is open against `v5.1-dev` and pins grovedb `dce8252f`. Its rows are marked **PR #4648**. |

The storage-relevant files are identical between `v5.1-dev` and `v5.2-dev` (only the
`Cargo.toml` pins differ), so these line numbers also hold at `v5.2-dev` `5b5241b0`.

Paths are abbreviated:

- `abci/` is `packages/rs-drive-abci/src/`
- `drive/` is `packages/rs-drive/src/`
- `version/` is `packages/rs-platform-version/src/version/`
- `grovedb:` and `storage:` are files in this repository, at this branch's base.

Unless a row is marked **PR #4648**, its line numbers are at `979510a9`.

## How Platform uses GroveDB's storage today

- **One database per node.** `Drive::open` opens `GroveDb::open(db_path)` with no options and keeps it as `Arc<GroveDb>`. Queries, check_tx and block execution all share it.
- **One write transaction per block.** It starts in prepare or process proposal and is held across ABCI calls in an `RwLock<Option<Transaction<'a>>>`. It is committed in finalize block, or dropped when a round ends.
- **Savepoints:**
  - At genesis, init chain sets a savepoint and every genesis round rolls back to it.
  - While building a proposal, each state transition gets a savepoint. The node rolls back to it when the transition is removed from the block.
- **Readers outside the transaction.** gRPC queries run on blocking threads, with no transaction, against committed state while the block transaction is open. A few multi-part reads use snapshot read transactions.
- **Checkpoints:**
  - Every 600 s of block time, after the commit, Platform creates a RocksDB checkpoint in `<db_path>/checkpoints/<height>`, inside the live database directory, and keeps the last 3.
  - Each checkpoint is opened as a full `GroveDb` and stays open. Address-sync proofs, and with PR #4648 state-sync chunks, are served from it.
  - Platform deletes old checkpoints itself with `remove_dir_all`.
- **Files Platform owns inside the database directory:**
  - `checkpoints/` and `last_checkpoint_attempt`.
  - `.fsck`.
  - With PR #4648, `state_sync_restore_in_progress`.
  - Inside each checkpoint, `platform_state.bin`.
- **RocksDB leaks:**
  - A commit conflict is recognised by matching `rocksdb::ErrorKind::Busy`.
  - The estimated-cost helpers are named with `RocksDbStorage` as a type parameter.
  - Some tests and a devnet seeding tool open RocksDB directly or build RocksDB SST files.

## Requirements

### Open, close and the database directory

| ID | Requirement | Platform call site | Tasks |
| --- | --- | --- | --- |
| P-01 | Open a database by path with no options: `GroveDb::open(path)`, which creates it if missing. The type is named `GroveDb` with no type parameter and held as `Arc<GroveDb>`. | `drive/open/mod.rs:37` `Drive::open`; `drive/drive/mod.rs:167` `Drive::grove`; `abci/platform_types/platform/mod.rs:162` `Platform::open_with_client`; `abci/replay/mod.rs:35` `run` (creates the directory, then opens) | SEAM-04 (default type parameter, Platform builds unchanged), TXN-07 (create, open), GROVE-01, OPS-03 (choose the engine) |
| P-02 | Engine selected by configuration. Today the engine is implied by `GroveDb::open`. Drive stores a concrete `Arc<GroveDb>`, so choosing LigninDB at run time needs either a generic `Drive` or a run-time engine choice behind one type. Opening a directory written by the other engine must fail with a clear error, never create a second database beside the first. | `drive/open/mod.rs:37` `Drive::open`; `drive/drive/mod.rs:167` | SEAM-04, TXN-07 (format version on open), FMT-06 (format version in the manifest), OPS-03 ("engine chosen by configuration"), OPS-02 (migration path). No "Done when" tests the refusal; see below. |
| P-03 | The engine must ignore, and never delete or rename, entries in its directory that it did not create. That rule covers open, recovery, the cleaner, wipe and checkpoint. Platform writes `checkpoints/`, `last_checkpoint_attempt` and its `.tmp` and, with PR #4648, `state_sync_restore_in_progress`. The operator may create `.fsck`, which Platform leaves in place until a verification run removes it. That marker must survive `wipe` by design. The engine's own file names must not collide with these. | `abci/execution/platform_events/block_end/create_grovedb_checkpoint/v0/mod.rs:57` `create_grovedb_checkpoint_v0`; `abci/platform_types/platform/checkpoint_attempt.rs:18,54-61` `store_last_checkpoint_attempt`; `abci/main.rs:55-60` (`.fsck` documented for operators); `abci/verify/mod.rs:41,72-73` `verify_grovedb`; **PR #4648** `abci/platform_types/snapshot/mod.rs:18-34,47-55` `write_restore_sentinel` | FMT-06 (manifest names the engine's files), STORE-02, STORE-08 (deletes only dead segments), TXN-06, TXN-07 (open, wipe) |
| P-04 | Exclusive lock per directory, released when the handle is dropped. Platform opens a bare `GroveDb`, drops it and reopens the same path through `Drive::open` in the same process. | `abci/verify/mod.rs:15-18,53` `run`, `verify_grovedb`; **PR #4648** `abci/platform_types/snapshot/mod.rs:148-155` `wipe_marked_database_before_open`, called at `abci/platform_types/platform/mod.rs:188` | IO-01 (directory lock), TXN-07 (directory lock, close) |
| P-05 | `flush()` makes all committed state durable. It is called when Platform shuts down and, with PR #4648, after a wipe and after a restore. Without a flush, a crash may lose recent commits but must land on a committed block (DEC-04). | `abci/platform_types/platform/mod.rs:334-342` `Drop for Platform`; **PR #4648** `abci/platform_types/snapshot/mod.rs:154`, `abci/abci/handler/apply_snapshot_chunk.rs:367` | DEC-04, STORE-01, STORE-07, TXN-07 (flush), VER-04 |
| P-06 | Configuration. Platform has no engine settings today: `db_path` only, plus the GroveDB visualizer address. LigninDB needs cache size, sync policy and paths settings. Several databases are open at once, each from `GroveDb::open`: the live one, up to `num_checkpoints` = 3 checkpoints and, with PR #4648, checkpoints pinned for serving. A fixed cache per open database must therefore be sized per role, or shared, so memory stays bounded. | `abci/config.rs:181-182` `PlatformConfig::db_path`; `drive/config.rs:110,118` visualizer settings; `version/drive_abci_versions/drive_abci_checkpoint_parameters/v1.rs:5-6`; **PR #4648** `abci/platform_types/snapshot/mod.rs:466` `pin_for_serving` | TXN-07 (`Options`), OPS-03 (cache size, sync policy, paths), OPS-05 (tuning) |
| P-35 | Test databases live in a temporary directory that is removed once the database is dropped. Dropping the database must release every file so the removal succeeds. A flush after setup, or in `Drop for Platform`, must work while the directory exists, and a test can reopen the same directory as a restart. <ul><li>Drive's helper: `Drive` owns the directory and drops `grove` first.</li><li>drive-abci's `TempPlatform`: owns `Platform` and the directory, declared in that order.</li></ul> Every Drive and drive-abci test relies on this. | `drive/util/test_helpers/setup.rs:37-48` `setup_drive`, tests at `:141-170`; `drive/drive/mod.rs:180-187` `Drive::temp_dir`; `abci/test/helpers/setup.rs:32-37` `TempPlatform`, `:67-90` `build_with_mock_rpc`, `:264-267` `open_with_tempdir` | TXN-07 (close, flush), OPS-03 (Platform test suite on LigninDB) |

### Transactions and readers

| ID | Requirement | Platform call site | Tasks |
| --- | --- | --- | --- |
| P-07 | One block write transaction, started by `GroveDb::start_transaction()` and held across ABCI requests. It sits in `RwLock<Option<Transaction<'a>>>` with a lifetime tied to the `GroveDb`, so it must be `Send + Sync`. It is committed with `commit_transaction`, or rolled back by drop when a round ends. Only one is open at a time. | `abci/abci/app/full.rs:24,61-85` `FullAbciApplication::{start_transaction, commit_transaction}`; `abci/abci/app/consensus.rs:24,59-72`; `abci/abci/handler/prepare_proposal.rs:148`; `abci/abci/handler/process_proposal.rs:311`; `abci/abci/handler/finalize_block.rs:197` `finalize_block`; `drive/util/operations/commit_transaction/v0/mod.rs:8-12` `commit_transaction_v0` | SEAM-04, TXN-03, TXN-05, GROVE-01 |
| P-08 | A read through a plain write transaction sees the transaction's own writes, and for everything else **the latest committed state at the moment of the read**, not a snapshot taken at `start_transaction`. Platform states this rule and builds on it. **TXN-03 as written does not meet it**; see "Requirements whose task does not meet them". | `drive/query/drive_document_ranked_query/branches.rs:268-276` (rule stated); `drive/query/drive_document_count_and_sum_query/executors/range_no_proof.rs:215-220`; `abci/shielded_snapshot/mod.rs:623-637` `shielded_subtree_has_committed_keys`; grovedb side: `storage:src/rocksdb_storage/storage.rs:1136-1138` `start_transaction`, doc at `:834-840` | TXN-01 (visibility rule), TXN-03, GROVE-01, VER-08 |
| P-09 | Writes with no transaction (`TransactionArg = None`) commit at once, even while the block write transaction is open. Afterwards that open transaction's commit fails with a conflict error that Platform can classify. Mainnet `evo1` heights 32326 to 32328 depend on this: the vote cleanup applies its batch outside the block transaction, the block commit fails with RocksDB `Busy`, and Platform matches exactly that error and carries on. Replaying mainnet from genesis must reproduce it. | `drive/drive/votes/cleanup/remove_all_votes_given_by_identities/v0/mod.rs:104-128` `remove_all_votes_given_by_identities_v0`; `abci/abci/handler/finalize_block.rs:197-240` `finalize_block` (matches `grovedb_storage::error::Error::RocksDBError` with `rocksdb::ErrorKind::Busy`); `abci/abci/handler/prepare_proposal.rs:62-77` (root-hash check skipped below 33000) | TXN-05 (commit conflict), SEAM-04 (an engine-neutral conflict error), GROVE-01, OPS-03 (classifier), VER-08 (mainnet replay) |
| P-10 | Concurrent readers with no transaction see the last committed state, never the open block transaction's writes. They never block the writer. gRPC queries run on blocking threads during block execution. | `abci/query/service.rs:120-192` `QueryService::handle_blocking_query`; `abci/abci/handler/prepare_proposal.rs:47-52` (`root_hash(None)` while the previous block is committed) | TXN-02, PERF-07, GROVE-01 |
| P-11 | Snapshot read transactions: `start_snapshot_read_transaction()` pins one committed state for a multi-part read and is dropped without commit. Writes and commit through it are refused with a typed error (`SnapshotReadOnlyTransaction`). | `drive/query/drive_document_ranked_query/branches.rs:280,290-297`; `drive/query/drive_document_count_and_sum_query/executors/range_no_proof.rs:225`; `drive/query/drive_document_count_and_sum_query/executors/per_in_value.rs:94`; grovedb side: `grovedb:src/lib.rs:1922`, `storage:src/rocksdb_storage/storage.rs:140-157,859-866` | SEAM-04 (snapshot read transactions in `Engine`), TXN-02, GROVE-01 |
| P-12 | Savepoints on the write transaction: `set_savepoint()` (no result) and `rollback_to_savepoint() -> Result<(), grovedb_storage::Error>`, mapped by Platform into `grovedb::Error::StorageError`. The rules Platform relies on: <ul><li>Savepoints form a stack.</li><li>A rollback undoes every write since the newest savepoint and pops that savepoint; Platform sets it again straight after.</li><li>There is no pop without a rollback, and Platform leaves kept savepoints on the stack, inert at commit and drop.</li><li>A rollback restores the exact root hash and every read through the transaction, including GroveDB's in-memory Merk state.</li></ul> | `abci/abci/handler/init_chain.rs:31` `handle_init_chain`; `abci/abci/handler/prepare_proposal.rs:141-145`; `abci/abci/handler/process_proposal.rs:304-308`; `abci/execution/platform_events/state_transition_processing/process_raw_state_transitions/v0/mod.rs:140-149,198-200,250-266` `process_raw_state_transitions_v0`; test of the rule: `abci/execution/validation/state_transition/state_transitions/shield/tests.rs:2816-2940` `savepoint_rollback_must_undo_an_applied_shield` | SEAM-04 (savepoints in `Engine`), TXN-04 (exact root hash, nested), GROVE-01 |
| P-13 | `rollback_transaction(&tx)` discards a transaction's writes. Drive exposes it, but at this commit only tests call it. | `drive/util/operations/rollback_transaction/v0/mod.rs:8-10` `rollback_transaction_v0` | TXN-05 (rollback) |

### Checkpoints

| ID | Requirement | Platform call site | Tasks |
| --- | --- | --- | --- |
| P-14 | `create_checkpoint(path)` captures the committed state after the block commit. <ul><li>The target is `<db_path>/checkpoints/<height>`, a new directory inside the live database's own directory, so the engine must neither include `checkpoints/` in a checkpoint nor treat it as its own.</li><li>Platform creates the parent and the engine creates the target.</li><li>An existing target is refused and left untouched; Platform's retry logic depends on this.</li><li>The call runs on the finalize-block path, so its duration adds to block time.</li><li>Every 600 s of block time, the last 3 kept.</li></ul> | `abci/execution/platform_events/block_end/create_grovedb_checkpoint/v0/mod.rs:41-72` `create_grovedb_checkpoint_v0`; refusal test `:323-348` `checkpoint_creation_failure_leaves_existing_directory_alone`; `abci/abci/handler/finalize_block.rs:314-341` (retried once); `abci/execution/platform_events/block_end/should_checkpoint/v0/mod.rs:38-111` `should_checkpoint_v0`; `version/drive_abci_versions/drive_abci_checkpoint_parameters/v1.rs:5-6`; grovedb side: `grovedb:src/checkpoints.rs:9`. (`update_checkpoints_v0`, `abci/execution/platform_events/block_end/update_checkpoints/v0/mod.rs:54-57`, does the same, but nothing calls it.) | TXN-06, GROVE-08, PERF-09 (checkpoint row) |
| P-15 | A checkpoint opens as an ordinary read-write database through `GroveDb::open`. It stays open for its whole life, next to the live database, while the live database keeps writing and cleaning. Opening it never modifies a file it shares with the live database. | `abci/execution/platform_events/block_end/create_grovedb_checkpoint/v0/mod.rs:162-180` `complete_checkpoint`; `drive/open/load_current_checkpoints.rs:58-62` `load_current_checkpoints` | TXN-06 (opening a checkpoint is the normal open), STORE-07 (never appends to an earlier session's file), GROVE-08 |
| P-16 | Proofs and costs served from a checkpoint equal those of the live database at that height. Trunk and branch chunk proofs for address sync are served from the latest or a named checkpoint, and their `OperationCost` is pushed as a drive operation. | `drive/util/grove_operations/grove_get_proved_trunk_chunk_query/v0/mod.rs:35-57` `grove_get_proved_trunk_chunk_query_v0`; `drive/util/grove_operations/grove_get_proved_branch_chunk_query/v0/mod.rs:35-57`; `drive/drive/address_funds/prove/prove_address_funds_trunk_query/v0/mod.rs:39-41`; `drive/drive/address_funds/prove/prove_address_funds_branch_query/v0/mod.rs:56-58`; `abci/query/response_metadata/v0/mod.rs:59-106` `response_proof_v0` | GROVE-06, GROVE-08, MERK-06, TXN-06 |
| P-17 | Platform writes `platform_state.bin` into the checkpoint directory after `create_checkpoint` and before opening the checkpoint. It reads the file back at startup. Opening must accept it, and the engine must never remove it. | `abci/execution/platform_events/block_end/create_grovedb_checkpoint/v0/mod.rs:167-174` `complete_checkpoint`; `abci/platform_types/platform/mod.rs:203-242` `open_with_client` | TXN-06 |
| P-18 | Platform deletes a checkpoint with `std::fs::remove_dir_all` **while its `GroveDb` is still open**: `Drop for Checkpoint` runs before its `grove_db` field is dropped. Afterwards the engine must not panic, recreate the directory or write anything when the handle closes. Deleting the checkpoint must not affect the live database it shares files with. Platform also removes a half-made checkpoint directory after a failed step. | `drive/drive/mod.rs:96-136` `Checkpoint`, `Checkpoint::mark_for_deletion`, `Drop for Checkpoint`; `abci/execution/platform_events/block_end/create_grovedb_checkpoint/v0/mod.rs:75-89,100-104` | TXN-06, TXN-07 (close), STORE-08 |
| P-19 | After a restart Platform scans `<db_path>/checkpoints/`, opens every directory whose name is a number and silently skips any that fails to open. It takes each checkpoint's time from the **directory's modification time**, and `should_checkpoint` uses that time to schedule the next checkpoint. With RocksDB, opening a checkpoint creates files in the directory, which in practice moves its modification time to the restart. An engine that opens without touching the directory leaves the creation time. Either way the result is node-local, and the persisted `last_checkpoint_attempt` bounds it, but the schedule after a restart can differ between engines. | `drive/open/load_current_checkpoints.rs:23-81` `load_current_checkpoints`; `abci/execution/platform_events/block_end/should_checkpoint/v0/mod.rs:79-98`; `abci/platform_types/platform/checkpoint_attempt.rs:1-8,26-50` | TXN-06, OPS-03 (store the timestamp in `platform_state.bin` rather than read the directory's modification time), OPS-04 (shadow comparison) |
| P-20 | **PR #4648:** state-sync configuration. When `snapshots_enabled` is true, `snapshots_frequency_seconds` and `max_num_snapshots` replace the platform-version checkpoint interval and count, both for scheduling and for pruning. `checkpoints_path` moves the checkpoint directory, possibly to another filesystem than the database. Creation, startup loading, serving and pruning all use that same directory. Checkpoints still referenced by a transfer in progress stay open after they leave the registry, so more than `max_num_snapshots` can be open. | **PR #4648** `abci/abci/config.rs:55-101` `StateSyncAbciConfig`, `resolved_checkpoints_path`; `abci/execution/platform_events/block_end/should_checkpoint/v0/mod.rs:49-62` `should_checkpoint_v0` (interval and count); `abci/execution/platform_events/block_end/create_grovedb_checkpoint/v0/mod.rs:57-58,65,104` `create_grovedb_checkpoint_v0` (count, directory, pruning); `abci/platform_types/platform/mod.rs:175-198` `open_with_client` (startup through `Drive::open_with_checkpoints_path`); `drive/open/mod.rs:50-77` `Drive::open_with_checkpoints_path`; `abci/platform_types/snapshot/mod.rs:466,521` `pin_for_serving`, `pinned_checkpoint` | TXN-06 (copy fallback across filesystems), STORE-08 (a pinned checkpoint holds segments), OPS-03 (configuration, pinning and pruning wired on LigninDB) |

### State sync

All rows in this section are **PR #4648** (`c7f87b90`). Platform's development branches do not serve or restore state sync yet.

| ID | Requirement | Platform call site | Tasks |
| --- | --- | --- | --- |
| P-21 | Serve from a checkpoint: <ul><li>`root_hash(None)` on the checkpoint for `list_snapshots`.</li><li>`fetch_chunk(chunk_id, None, wire_version, grove_version)` on the checkpoint's `GroveDb`, under the GroveVersion the checkpoint was written at, read from the checkpoint's own aux storage.</li></ul> Chunk bytes and chunk ids (the 32-byte path prefix) must equal RocksDB's, so a LigninDB node and a RocksDB node can sync from each other. | `abci/abci/handler/list_snapshots.rs:57-71` `list_snapshots`; `abci/abci/handler/load_snapshot_chunk.rs:131-167` `load_snapshot_chunk`; `drive/drive/mod.rs:157` `Checkpoint::platform_version` | MERK-07, GROVE-07, TXN-06 |
| P-22 | Restore into the live database, in this order: <ol><li>`wipe()` on the open, shared `GroveDb`</li><li>`start_snapshot_syncing(app_hash, batch_size, wire_version, grove_version)`</li><li>`apply_chunk` for each chunk</li><li>`commit_session`</li><li>`verify_grovedb`</li><li>`root_hash(None)`</li><li>`flush()`</li></ol> | `abci/abci/handler/offer_snapshot.rs:110-144` `offer_snapshot`; `abci/abci/handler/apply_snapshot_chunk.rs:190,254-255,273-274,303-304,367` `apply_snapshot_chunk` | GROVE-07, MERK-07, TREE-10 (restore through the bulk loader), GROVE-08 (`verify_grovedb`) |
| P-23 | `wipe()` empties every engine keyspace: tree data, roots, aux and meta. It runs on the open database while Drive holds it. The RocksDB wipe clears one column family after another, so a crash part-way leaves aux without data; Platform guards that with a marker file and wipes again at the next start. Wipe must leave Platform's own files alone (P-03). After a crash at any point, the database must still open. | `abci/platform_types/snapshot/mod.rs:20-34,139-166` `wipe_marked_database_before_open`, `wipe_drive_for_restore`; grovedb side: `grovedb:src/lib.rs:432`, `storage:src/rocksdb_storage/storage.rs:871-881` | SEAM-04 (`wipe` in `Engine`), TXN-07 (wipe), GROVE-01, VER-04, OPS-03. No "Done when" tests these wipe rules; see below. |

### Other GroveDB operations that touch storage directly

| ID | Requirement | Platform call site | Tasks |
| --- | --- | --- | --- |
| P-24 | Pending prefix drops. `drop_flat_subtree` records a drop inside the block transaction. After every commit and at startup, `flush_pending_prefix_drops` reclaims the dropped prefixes and reports `reclaimed_records`, `skipped_live` and `unresolved_records`. This happens outside consensus, and a failure leaves the records to retry. | `drive/drive/document/time_range_ttl.rs:626` `drop_flat_subtree` call; `abci/abci/handler/finalize_block.rs:263-301` `finalize_block`; `abci/platform_types/platform/mod.rs:164-192` `open_with_client` | GROVE-05 (pending-drop records to subtree drop), TREE-09, OPS-03 (startup and per-block flush wired on LigninDB). No "Done when" tests the report or the retry; see below. |
| P-25 | `clear_subtree` empties a subtree in place. | `drive/util/grove_operations/grove_clear/v0/mod.rs:42-46` `grove_clear_v0` | GROVE-05 |
| P-26 | Aux storage: `put_aux`, `get_aux`, `delete_aux` and `get_aux_by_key_prefix` hold the platform state and the protocol version. They are written inside the block transaction, so they commit atomically with the tree. `Drive::open` reads the protocol version from aux before anything else. Aux is part of checkpoints and is cleared by wipe. | `drive/drive/platform_state/store_platform_state_bytes/v0/mod.rs:13`; `drive/drive/platform_state/store_platform_state_entry_bytes/v0/mod.rs:15`; `drive/drive/platform_state/delete_platform_state_entry/v0/mod.rs:14`; `drive/drive/platform_state/fetch_platform_state_entries_bytes/v0/mod.rs:15`; `drive/drive/system/protocol_version/fetch_current_protocol_version.rs:42-43`; `drive/open/mod.rs:47-48` | TREE-07 (aux in the flat keyspace), MERK-05, GROVE-01, TXN-06 |
| P-27 | `verify_grovedb` and `visualize_verify_grovedb` check the whole database. They run on startup when `.fsck` exists or `grovedb_verify_on_startup` is set, in the `verify` command, and (PR #4648) after a restore. | `abci/main.rs:141-142`; `abci/verify/mod.rs:40-57` `verify_grovedb`; `drive/config.rs:45`; **PR #4648** `abci/abci/handler/apply_snapshot_chunk.rs:270-274` | GROVE-08 (verification on the engine), OPS-03 (startup, `verify` command and post-restore paths on LigninDB) |
| P-28 | The GroveDB visualizer starts behind Drive's `grovedbg` feature. | `drive/open/mod.rs:39-42` `Drive::open` | GROVE-08 |
| P-29 | Platform's direct root-hash checks: `root_hash(None)` must equal the app hash in the platform state at every prepare and process proposal, and in `verify`. Root hashes, proofs and `OperationCost` are bit-identical on every existing GroveVersion (DEC-01). Costs feed fees through `CalculatedCostOperation`. | `abci/abci/handler/prepare_proposal.rs:47-77`; `abci/verify/mod.rs:108`; `drive/util/grove_operations/grove_get_proved_trunk_chunk_query/v0/mod.rs:32` | DEC-01, GROVE-02, GROVE-06, MERK-03, VER-02, VER-08 |
| P-30 | Devnet shielded-pool seeding uses three hatches: `raw_storage()`, `ingest_subtree_sst(cf, path)` and `replace_subtree_root`. Platform builds the RocksDB SST itself with the `rocksdb` crate, from keys `RocksDbStorage::build_prefix(path) ++ key` in the default column family. It relies on the ingest bypassing the open transaction. The module compiles only with the `shielded_test_data` feature (or in tests). Genesis seeding calls it only in builds with the `create_sdk_test_data` cfg. Mainnet and testnet builds have neither. | `abci/shielded_snapshot/mod.rs:259-450` `dump_shielded_subtree`, `:463-610` `apply_shielded_snapshot`, `:633-645`; `abci/lib.rs:103-104`; `abci/execution/platform_events/initialization/create_genesis_state/mod.rs:10-11,53-57`; `abci/execution/platform_events/initialization/create_genesis_state/test/mod.rs:12`; `drive/Cargo.toml` feature `shielded_test_data` | GROVE-09, OPS-03 (port Platform's snapshot file off RocksDB SST) |

### Types, crates and features Platform names

| ID | Requirement | Platform call site | Tasks |
| --- | --- | --- | --- |
| P-31 | These names must keep compiling until Platform changes them: <ul><li>`drive::grovedb::{GroveDb, Transaction, TransactionArg}`, with `Transaction<'db>` borrowing the database.</li><li>`grovedb::Error::StorageError(grovedb_storage::error::Error)`.</li><li>Drive re-exports `grovedb_storage` whole.</li></ul> | `drive/lib.rs:44` (`pub use grovedb_storage`); `abci/abci/app/full.rs:24`; `abci/abci/handler/prepare_proposal.rs:142-143`; grovedb side: `grovedb:src/lib.rs:336,339` | SEAM-04 (Platform builds with no source change), SEAM-07 |
| P-32 | Estimated-cost helpers named with RocksDB as the storage type parameter: `GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>` and the `…delete_up_tree_while_empty::<RocksDbStorage>` variant, in Drive's production delete and move paths. `grovedb_storage::worst_case_costs::WorstKeyLength` is imported by path. SEAM-08 moves these helpers; Platform must keep compiling, or change in step. | `drive/util/grove_operations/batch_delete/v0/mod.rs:13,63`; `drive/util/grove_operations/batch_delete_items_in_path_query/v0/mod.rs:14,96`; `drive/util/grove_operations/batch_delete_up_tree_while_empty/v0/mod.rs:11,31-32`; `drive/util/grove_operations/batch_move/v0/mod.rs:15,79`; `drive/util/grove_operations/batch_move_items_in_path_query/v0/mod.rs:15,103`; `drive/util/grove_operations/batch_remove_raw/v0/mod.rs:15,97`; `drive/util/object_size_info/{drive_key_info.rs:10,path_info.rs:4,path_key_info.rs:11}`; `drive/drive/document/summable_off_count_counter.rs:51` | SEAM-08, SEAM-04, SEAM-07, OPS-03 |
| P-33 | drive-abci depends on the `rocksdb` crate directly (0.25.0, the version `grovedb-storage` uses). In production, `finalize_block` matches `rocksdb::ErrorKind::Busy` (P-09), and the devnet snapshot builds SST files (P-30). Tests open RocksDB directly or construct RocksDB errors. | `abci/Cargo.toml` (dependency and dev-dependency `rocksdb = "0.25.0"`); `abci/abci/handler/finalize_block.rs:215,733-749,832-845`; `abci/main.rs:660,700-737` `corrupt_rocksdb_item` (test); `abci/execution/platform_events/initialization/create_genesis_state/test/shielded.rs:899-967` (ignored test) | OPS-03 (Platform test suite on LigninDB), OPS-10 |
| P-34 | Feature flags Platform turns on: grovedb with `default-features = false`, then `minimal` and `estimated_costs` (Drive `server`), `verify`, `grovedbg` and `unsafe-dump-load` (`shielded_test_data`). grovedb-merk gets `minimal` or `verify`, and grovedb-storage is a direct dependency. Today `minimal` turns on `grovedb-storage/rocksdb_storage`. A LigninDB engine feature must be reachable from these flags, or OPS-03 adds it. | `Cargo.toml:64-72` (workspace pins); `drive/Cargo.toml` features `server`, `verify`, `grovedbg`, `shielded_test_data`; grovedb side: `grovedb/Cargo.toml` feature `minimal` | SEAM-07, SEAM-08, OPS-06, OPS-03, OPS-10 |

## Requirements whose task does not meet them as written

These map to tasks, but the task text contradicts or leaves out what Platform needs. The
owner should decide.

1. **P-08: a write transaction reads the latest committed state.**
   - **Today:** GroveDB's plain transaction is a RocksDB optimistic transaction without a snapshot. Every read sees the latest committed state, plus the transaction's own writes.
   - **The task:** TXN-03 specifies "private copy-on-write frames over a base snapshot", which pins the state at `start_transaction`.
   - **When they differ:** only when something commits while a write transaction is open. Platform does exactly that at mainnet heights 32326 to 32328 (P-09), where the block transaction keeps reading after the vote cleanup committed outside it.
   - **The risk:** under snapshot isolation the block transaction would not see those deletions, so the app hash LigninDB computes for those blocks could differ. A mainnet replay from genesis (VER-08), the only way a new node syncs while state sync is unmerged, would then diverge.
   - **Options:**
     - TXN-01 and TXN-03 make reads of keys the transaction has not written see the latest committed version, matching RocksDB.
     - Alternatively VER-08 proves the snapshot rule gives the same app hashes at those heights.
     - Or VER-08's replay, and LigninDB nodes, start from a state after height 33000.
2. **P-09: a write outside the transaction makes the open transaction's commit fail.**
   - **The task:** TXN-05 says "a second write transaction fails at commit with a conflict".
   - **Platform's case:** a write with no transaction commits while the block transaction is open, and the *block* transaction's commit must fail.
   - **Error type:** Platform classifies the failure by RocksDB's error type. SEAM-04 or TXN-05 should give `Engine` an engine-neutral conflict error, and OPS-03 should match on it.
3. **P-03 and P-18: foreign files, and deletion while open.** TXN-06 has the engine delete a checkpoint ("Deleting one removes its directory"). Platform instead deletes the directory itself with `remove_dir_all` while the handle is open, and keeps its own files in both directories. No task's "Done when" tests either. Suggested additions:
   - TXN-07: open, recovery and wipe leave unknown directory entries untouched.
   - TXN-06: a checkpoint whose directory is removed while it is open closes cleanly, and the live database is unaffected.
4. **P-02: opening the other engine's directory.** TXN-07 and FMT-06 give LigninDB a format version, but no "Done when" checks that LigninDB refuses a RocksDB directory, or the reverse, without changing it. Suggested addition to TXN-07: opening a directory written by another engine, or by an unknown format version, fails with a typed error and leaves the directory byte-for-byte unchanged.
5. **P-23: wipe.** TXN-07 lists wipe, but its "Done when" covers documentation and reopen time only, and VER-04's crash campaign does not name wipe. Suggested additions:
   - TXN-07: wipe empties tree data, roots, aux and meta and leaves files it does not own.
   - VER-04: a crash at any point during a wipe leaves a database that opens, either unchanged or empty.
6. **P-24: pending prefix drops.** GROVE-05 and TREE-09 test that dropped space is reclaimed. They do not test the report Platform logs (`reclaimed_records`, `skipped_live`, `unresolved_records`) or that a failed flush leaves its records to be retried. Suggested addition to GROVE-05: the report counts match RocksDB's on the same history, and an injected failure leaves the records for the next flush.
7. **P-06: several databases in one process.** TXN-07 gives each database an `Options` with a cache size, but nothing bounds memory when the live database and three or more checkpoints are open together. OPS-03 should choose the checkpoint cache size, or TXN-07 should allow a shared cache.

## Requirements with no task

1. **Platform after RocksDB is removed (OPS-10).**
   - **What breaks:** OPS-10 deletes the RocksDB engine and the `rocksdb_storage` feature, and its acceptance test is GroveDB-only. Platform at the pinned commit names, in production code:
     - `grovedb_storage::error::Error::RocksDBError`
     - `rocksdb::ErrorKind::Busy`
     - `RocksDbStorage` as the type parameter of the estimated-cost helpers (P-32)
   - **The gap:** no task makes Platform build and replay against the release OPS-10 and OPS-11 produce. In particular, nothing re-expresses the historical commit-conflict rule (P-09) without RocksDB types.
   - **Options:**
     - OPS-03 makes Platform's storage use engine-neutral as part of the integration.
     - Or OPS-10's "Done when" adds "Platform builds and replays against the release".

Everything else in this document maps to at least one task.

## What OPS-03 has to change in Platform

These follow from the rows above. They are listed here so OPS-03 can be scoped from this file alone.

- **Engine choice:** choose the engine and its options from configuration (P-01, P-02, P-06), and size caches for checkpoints.
- **Error types:** classify a commit conflict through an engine-neutral error instead of `rocksdb::ErrorKind::Busy` (P-09, P-33).
- **Estimated costs:** replace `RocksDbStorage` type parameters with whatever SEAM-08 leaves (P-32).
- **Checkpoint time:** take a checkpoint's time from `platform_state.bin` rather than the directory's modification time, so the schedule after a restart does not depend on the engine (P-19).
- **Devnet snapshot:** port the shielded snapshot off RocksDB SST files onto the GROVE-09 replacement (P-30).
- **Tests:** gate or port the tests that open RocksDB directly (P-33), and keep both temporary-directory test helpers working (P-35).
- **Features and dependencies:** forward the LigninDB engine feature through Drive's and drive-abci's feature flags, or confirm SEAM-07 and OPS-06 already make it reachable. Decide whether drive-abci still needs its direct `rocksdb` dependency (P-33, P-34).
- **State sync:** if PR #4648 has merged by then, carry state sync onto LigninDB (P-20 to P-23), including its configuration and checkpoint pinning.
- **Wiring:** keep the pending-drop flush (P-24) and the verification triggers (P-27) working on LigninDB.

## Not engine requirements

These came up in the survey and need nothing from the engine:

- **Drive's caches:** the data-contract cache and the protocol-version counters cache.
- **Withdrawal rejections:** written to `rejections_path`, outside the database directory (`abci/config.rs:184-188`).
- **Most GroveDB API traffic:** `insert`, `get`, `query`, `apply_batch` and proofs. It goes through GroveDB's public API and is covered by DEC-01 and the GROVE tasks as a whole, not by a specific storage call.

## How this was checked

- **Pinned sources.** Both Platform commits were exported with `git archive` and searched read-only. Nothing was built or edited in the Platform checkout.
- **Method calls.** Every method called on a `GroveDb` handle in `packages/rs-drive/src` and `packages/rs-drive-abci/src`, test modules excluded, was listed with a multi-line search for `(grove|grove_db|grovedb|db).<method>(`. That gave 72 distinct methods. Each was sorted into one of two groups:
  - Storage-relevant, and therefore in a row above: transactions, snapshot reads, savepoints, checkpoints, open, flush, wipe, aux, drops and clears, verify, visualizer, chunks, root hash, and the dump hatches.
  - Ordinary GroveDB API, covered by DEC-01.
- **Other storage access.** The same sources were searched for every other way Platform reaches storage:
  - the `grovedb_storage` and `rocksdb` crates, and `RocksDbStorage`
  - `start_transaction`, `start_snapshot_read_transaction` and savepoints
  - `db_path`, and `std::fs` calls that create, write, remove or list files
  - grovedb features in each `Cargo.toml`
- **Test code.** Uses inside `#[cfg(test)]` modules are cited only where a test pins down a behaviour the engine must keep (P-12, P-14) or uses RocksDB directly (P-33).
- **Line numbers.** Each cited line was re-read at the pinned commit.
- **Task names.** Every task ID named above exists in the registers of #1052 (draft 7).
