# Storage surface inventory

LigninDB task BASE-04 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

This document lists every call from Merk, GroveDB and the append-only tree crates into `grovedb-storage`. Each site has a class, the seam that will own it, and the cost the call charges. BASE-06 (cost semantics), SEAM-01 (`NodeStore`), SEAM-03 (`FlatStore`), SEAM-04 (`Engine`) and SEAM-08 (engine-free estimates) build on it.

**Pinned commit:** `f69acc954a111a0f58a425b74e9f495715301cfd` (`feat/LigninDB`, "chore: scaffold the LigninDB workspace crates"). Every `path:line` below is at that commit. Each site also names its enclosing function, so it can be found again after lines move.

**Result:** {{COUNTS}}.

## How the list was produced

The list comes from the compiler, not from a text search. A text search cannot tell `storage.get(key)` from `HashMap::get` or `merk.get(...)`, and it misses calls made through a generic `S: StorageContext<'db>`.

[`storage-surface-search.py`](storage-surface-search.py) does the following:

1. It exports the pinned commit with `git archive` into a scratch directory.
2. It marks `#[deprecated]` every public function, inherent method, trait, trait method, associated type, struct, enum and type alias in `storage/src` (103 items).
3. It runs `cargo check --all-features --message-format=json` with `RUSTFLAGS=--force-warn deprecated` on every workspace package whose manifest depends on `grovedb-storage` (found with `cargo metadata`). There are two passes: `--lib --bins`, then `--all-targets`.
4. It records one use for every deprecation warning whose primary span lies outside `storage/`.
5. A trait impl item cannot carry `#[deprecated]`, so a `StorageBatch` or `TempStorage` built through `Default::default()` is invisible to steps 2 to 4. A third pair of passes (the `Default` probe) removes every `impl Default for <Type>` from `storage/src` and runs `cargo check --keep-going`. It records each resulting error as a call of `<Type>::default`. It found four library sites, all `StorageBatch::default` in GroveDB.

What this catches:

- `--force-warn` overrides the `#[allow(deprecated)]` attributes present in six test files.
- Calls through generic bounds, on concrete types, through `Type::method` paths and inside macros are all reported. No primary span fell inside a macro definition (the `## macro-expanded` section of the output is empty).
- A use is keyed on file, line, column and item. A file compiled into several targets therefore counts once.
- Sites seen in the `--lib --bins` pass are library code. The rest exist only in test, bench or example targets, or under `#[cfg(test)]`.
- Methods, associated functions and free functions are call sites. A free function named at item level is a `use` import and goes with the references.
- Traits, types, associated types, enum variants and struct fields are references, aggregated in [Appendix A](#appendix-a-type-trait-and-import-references).
- The script reads the dep-info files of the build and lists every `.rs` file of the scanned packages that no target compiled. At the pinned commit the only such file is `merk/src/merk/tree_type.rs`. It is not in the module tree and contains no storage use.

Scanned packages: `grovedb-merk`, `grovedb`, `grovedb-query`, `grovedb-merkle-mountain-range`, `grovedb-bulk-append-tree`, `grovedb-dense-fixed-sized-merkle-tree`, `grovedb-commitment-tree`, `grovedb-private-document-store`. `grovedb-query` is included because Merk's query code lives there and positions storage iterators. No other workspace member depends on `grovedb-storage`. `tutorials/` is not a workspace member and is out of scope.

To reproduce, and to check that this document lists exactly the sites the search finds:

```sh
python3 -I docs/lignindb/storage-surface-search.py run \
    --rev f69acc954a111a0f58a425b74e9f495715301cfd \
    --workdir /tmp/storage-surface > sites.tsv
python3 -I docs/lignindb/storage-surface-search.py check \
    --rev f69acc954a111a0f58a425b74e9f495715301cfd --sites sites.tsv \
    --template docs/lignindb/storage-surface.template.md \
    --doc docs/lignindb/storage-surface.md
```

`check` does two things:

1. It reads every inventory row below (site, API item and call count). It fails if a site is missing on either side or the counts differ, and prints `OK: the document lists exactly the call sites the search found`.
2. It runs [`storage-surface-gen.py`](storage-surface-gen.py) on the same search output and the [template](storage-surface.template.md), and fails unless this document is byte-for-byte equal to the result. It prints `OK: the document is the generator's output`. So every class, seam, cost and note, every summary count and Appendix A come from the committed rules, not from hand edits.

The class, seam and cost of a site are a function of its API item, its crate and file, and, for a few files, its line. The rules are in `classify` in the generator, with the line lists for flat namespaces, immediate contexts, escape hatches and aux cursors next to it. Test-only sites go through the same rules.

What the search does not cover:

- Code behind a `cfg` that `--all-features` on a debug build leaves off. In the scanned crates those are `cfg(not(feature = "minimal"))` stubs in two benches, `cfg(not(feature = "server"))` in one bench, `cfg(not(feature = "grovedbg"))` in `build.rs`, and two `target_os` branches in one test. None of them touch storage.
- Doc-test code. It is never compiled by `cargo check`.
- Targets the `Default` probe cannot compile, because the GroveDB library they depend on fails in it. These are GroveDB's 8 integration tests and 5 benches; the output lists them under `## default-probe-unchecked`. The main passes do cover them. Only `grovedb/tests/state_sync_serving_tree_type.rs` uses storage there, and it names neither `StorageBatch` nor `TempStorage`. `grep -nE 'default\(\)' grovedb/tests/*.rs grovedb/benches/*.rs` finds only struct-update `..Default::default()` on option structs and `StorageRemovalPerEpochByIdentifier::default()`. So no unlisted `Default` call exists there.
- Other trait impls on storage types: `Debug` formatting, `Deref` from `TempStorage` to `RocksDbStorage`, and the `Display`, `Error` and `From` impls of `grovedb_storage::Error`. These are not storage operations and are not counted. A method reached through the `Deref` is a marked `RocksDbStorage` method and is counted.
- Direct use of the `rocksdb` crate, which is not `grovedb-storage`. It is listed separately [below](#outside-the-grovedb-storage-api).

## The API and what each call charges

`grovedb-storage` has four storage-operation traits (`storage/src/storage.rs`): `Storage` (line 60), `StorageContext` (148), `Batch` (245) and `RawIterator` (284). Its fifth public trait, `WorstKeyLength` (`storage/src/worst_case_costs.rs:32`), feeds estimates only (finding 11). It also has the `StorageBatch` struct, and the RocksDB implementation in `storage/src/rocksdb_storage/`.

A storage context is a view of one 32-byte prefix in four column families: default (Merk nodes, or the records of a flat store), `aux`, `roots` and `meta`. There are two kinds of context, and they charge differently:

- **Transactional context** (`PrefixedRocksDbTransactionContext`, `storage/src/rocksdb_storage/storage_context/context_tx.rs`). Reads go through the transaction. Writes are staged into a shared `StorageBatch` and charged only when that batch is committed.
- **Immediate context** (`PrefixedRocksDbImmediateStorageContext`, `context_immediate.rs`). Reads and writes go straight to the transaction and charge nothing. Its `Batch` charges as it is filled. GroveDB uses it only in state-sync restore.

The cost column of the inventory uses these codes. All of them are `OperationCost` fields.

| Code | Charged by | What it charges |
| --- | --- | --- |
| `P-path` | `get_transactional_storage_context`, `get_immediate_storage_context`, `RocksDbStorage::build_prefix` (`rocksdb_storage/storage.rs:436`) | `hash_node_calls` += Blake3 blocks of the prefix body: `1 + (len - 1) / 64` (`blake_block_count`, line 66). The body is the path segments, then the segment count as a native-endian `usize`, then one length byte per segment (line 406). The empty path is free. |
| `P-sec` | `RocksDbStorage::secondary_prefix_for` (line 474) | `hash_node_calls` += 1 |
| `R-tx` | `get`, `get_aux`, `get_root`, `get_meta` on a transactional context (`context_tx.rs:310`, 329, 345, 361) | `seek_count` += 1; `storage_loaded_bytes` += length of the value found, 0 if absent |
| `R-imm` | the same reads on an immediate context (`context_immediate.rs:186` to 210) | nothing |
| `W-tx` | `put`, `put_aux`, `put_root`, `put_meta` and the four deletes on a transactional context (`context_tx.rs:126` to 291) | Nothing at the call; the operation is staged in the `StorageBatch` and charged by `B-commit`. `put` with `cost_info.new_node` first adds the prefixed key length plus its varint length to `key_storage_cost.added_bytes` (line 142). Without a batch the call fails. |
| `W-imm` | the same writes on an immediate context (`context_immediate.rs:94` to 176) | nothing; written to the transaction at once, `cost_info` ignored |
| `B-part` | `new_batch`, `Batch::*` and `commit_batch` on a transactional context (`batch.rs:158`, `context_tx.rs:377`, 384) | Nothing. The operations go into a private `StorageBatch`, which `commit_batch` merges into the shared one. They are charged by `B-commit`. `Batch::put` applies the same `new_node` adjustment as `W-tx`. |
| `B-imm` | `Batch::*` on an immediate context's batch, returned by its `commit_batch` (`batch.rs:36`, `context_immediate.rs:227`) | `put`, `put_aux`: `seek_count` += 1 unless the cost info is prepaid, plus the key-value storage rule on the prefixed key. `put_root`: `seek_count` += 1, plus the storage rule only when cost info is given. Deletes: `seek_count` += 1, plus `removed_bytes` from the cost info when given, nothing otherwise. |
| `B-commit` | `Storage::commit_multi_context_batch` (`rocksdb_storage/storage.rs:1215`), and `continue_write_batch` (541) with `commit_db_write_batch` (768) when called directly | Per staged operation. Put to data, aux or meta: `seek_count` += 1 unless the cost info is prepaid, plus the key-value storage rule (`OperationCost::add_key_value_storage_costs`, `costs/src/lib.rs:191`) with child sizes for data puts. Put to roots: `seek_count` += 1, plus the rule only with cost info. Delete with cost info: `seek_count` += 1, `removed_bytes` += the info's combined removed bytes. Delete without cost info: `seek_count` += 2; reads the committed value (`storage_loaded_bytes` += its length); `removed_bytes` += key + value + both varint lengths. Seeks are charged at once. The storage cost is pending and is added only if the write succeeds (line 768). |
| `I-move` | `RawIterator::seek_to_first`, `seek_to_last`, `seek`, `seek_for_prev`, `next`, `prev` (`raw_iterator.rs:66` onward, and 195 onward for the transactional iterator) | `seek_count` += 1 |
| `I-key` | `RawIterator::key`, `RawIterator::valid` (`raw_iterator.rs:124`, 158) | `storage_loaded_bytes` += the full prefixed key length (32 + key) when the key is inside the prefix, else 288 (`MAX_PREFIXED_KEY_LENGTH`, line 41) |
| `I-value` | `RawIterator::value` (`raw_iterator.rs:110`) | `I-key` (it calls `valid`), plus the value length when valid |
| `CLR` | `PrefixedRocksDbTransactionContext::clear` (`context_tx.rs:83`) | One `I-move` for the first seek and one per step; `I-key` twice per record (`valid` and `key`); one staged `delete` without cost info per record, which `B-commit` charges as above. It clears only the default column family of the prefix. |
| `EST` | `Storage::get_storage_context_cost` (`rocksdb_storage/storage.rs:1228`), `WorstKeyLength::max_length` | Nothing. They return a worst-case figure that the caller adds to an estimate. `get_storage_context_cost` is `P-path` computed from each segment's maximum length. |
| `DB` | `wipe`, `delete_prefix_ranges`, `pending_prefix_drop_records`, `remove_pending_prefix_drop_record`, `ingest_subtree_sst` | Nothing. These act on the database directly, outside the transaction and outside the cost model. |
| `zero` | opens by prefix, `raw_iter`, `raw_iter_aux`, `status`, `retarget`, transactions, lifecycle, `StorageBatch` bookkeeping | nothing |

When a site's context is generic (Merk and the append-only crates take any `StorageContext`), the cost reads `R-tx / R-imm` (or `W-tx / W-imm`, `B-part / B-imm`): the first applies on the transactional contexts GroveDB normally passes, the second on the immediate contexts of state-sync restore.

Some library sites throw the returned cost away (`.unwrap()` or `.value` on the `CostContext`). The note column says "cost dropped by the caller" at those sites. The generator sets that flag by a heuristic: a `.unwrap()` or `.value` applied directly to the call. A cost bound to a variable and dropped later is not flagged, so the flag marks a lower bound. Mostly these are verification, replication and test-helper paths.

## Classes and seams

The register names ten classes. The inventory uses them as written and adds six, because some calls fit none of the ten.

| Class | Meaning |
| --- | --- |
| tree-node access | Merk reads or writes a node record in the default column family: point fetch, commit change set, restore writes |
| flat key-value | An append-only tree (MMR, bulk-append, dense, commitment, private-document) or GroveDB on its behalf reads or writes a record in the default column family of a flat namespace |
| aux | `aux` column family |
| meta | `meta` column family, including the pending-prefix-drop redo records |
| roots | `roots` column family (the Merk root-key pointer) |
| iterator | an ordered cursor over a namespace: `raw_iter` and every `RawIterator` method (a cursor from `raw_iter_aux` is an iterator owned by FlatStore; the `raw_iter_aux` open itself is aux) |
| by-prefix open | a context opened from a 32-byte prefix the caller already holds |
| immediate context | an immediate context opened, by path or by prefix |
| range delete | every record under a prefix removed: `clear`, `wipe`, `delete_prefix_ranges` |
| escape hatch | code that steps around the abstraction that owns the data: GroveDB reading, writing or decoding raw Merk node records, copying raw records between namespaces, retargeting one iterator across subtrees, building RocksDB write batches itself, or touching the database outside the transaction |
| *path open* (added) | a transactional context opened from a subtree path |
| *batch* (added) | `StorageBatch` construction and bookkeeping, `new_batch`/`commit_batch`, and the multi-context commit |
| *transaction* (added) | start, commit, roll back and snapshot reads |
| *lifecycle* (added) | opening the database, checkpoints, flush |
| *prefix* (added) | prefix derivation: `build_prefix`, `secondary_prefix_for`, the pending-drop namespace |
| *estimate* (added) | inputs to worst-case and average-case estimates |

The seam column names the seam that will own the call:

- **NodeStore** (SEAM-01): Merk node access, root pointer, change-set commit, and ordered cursors over Merk records.
- **FlatStore** (SEAM-03): flat records, aux, meta and pending-drop records, the cursor over aux (`get_aux_by_key_prefix`), and range deletes over flat namespaces.
- **Engine** (SEAM-04): opening subtree handles by path and by prefix, transactions, the multi-context batch, checkpoints, wipe, and the database-level drop reclamation.
- **Estimates** (SEAM-08): the `estimated_costs` helpers.

A `clear` that empties a flat tree's data namespace is assigned to FlatStore. One that empties a Merk or an indexed secondary is assigned to Engine.

## Summary

Library code, calls per class and crate (`grovedb-private-document-store` makes no call of its own: it reaches storage only through `grovedb-bulk-append-tree`):

{{SUMMARY_LIB}}

Library code, per seam:

{{SEAMS_LIB}}

Test-only code (test, bench and example targets, and `#[cfg(test)]` items):

{{SUMMARY_TEST}}

## Findings for the seams

These follow from the inventory. They are what SEAM-01, SEAM-03, SEAM-04, SEAM-08 and BASE-06 need to account for.

1. **Merk's own surface is small.** Merk makes 71 library calls. Thirteen of them are in `merk/src/test_utils/`, which the `test_utils` feature (part of `full`) compiles into the library. That is also the only library code in Merk that names a RocksDB type (`RocksDbStorage`, `PrefixedRocksDbTransactionContext`, `TempStorage`). Node fetch is `TreeNode::get` (`merk/src/tree/encoding.rs:73`) and `fetch_node` (`merk/src/merk/mod.rs:1091`). Commit is one `new_batch`, the `Batch` puts and deletes and one `commit_batch` in `Merk::commit` (`merk/src/merk/mod.rs:424` to 535). The root pointer is handled by `load_base_root` and `set_base_root_key`. Restore writes through `Restorer` (`merk/src/merk/restore.rs`), and ordered walks through `KVIterator`, `is_empty_tree` and `clear`. The two traits the plan names sit directly above these calls. `Fetch` for `MerkSource` (`merk/src/merk/source.rs:43`) calls `TreeNode::get`. `Commit` for `MerkCommitter` (`merk/src/merk/committer.rs:27`) collects the change set that `Merk::commit` writes through `Batch`.
2. **Merk's cursor is used outside Merk.** `grovedb-query` (`QueryItem::seek_for_iter`, 30 calls) positions node iterators. GroveDB iterates raw Merk records in {{GROVEDB_NODE_ITER}} library calls (`Element::iterator`, `KVIterator::new(merk.storage.raw_iter(), ..)`, `ElementsIterator`, replication `visit_nodes`). It also decodes node records it reads through iterators itself (`snapshot_fetch_node`, `merk/src/proofs/query/count_offset/pinned_view.rs`). The `NodeStore` cursor must serve all of these, not only Merk.
3. **GroveDB reads and writes Merk records directly.** The node-level escape hatches are:
   - `has_raw` (`grovedb/src/operations/get/mod.rs:607`)
   - the deletion visitor's raw delete (`grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs:394`)
   - `move_subtree_storage` and `move_namespace`, which copy raw records between namespaces (`grovedb/src/batch/move_element.rs`)
   - `snapshot_fetch_node` (`grovedb/src/operations/indexed_tree.rs:3526`)
   - pinned iterator views retargeted across subtrees: `PrefixedRocksDbRawIterator::retarget` in `grovedb/src/element/local_offset_read.rs` and `grovedb/src/operations/indexed_tree.rs:1514`

   SEAM-01 needs an answer for each one: a `NodeStore` operation, or a reason it stays engine-level.
4. **The append-only crates touch storage in very few places.** There are eleven library calls in total, all `get`/`put` on the default column family of a generic `StorageContext`:
   - MMR: 2
   - bulk-append: 3
   - dense: 4
   - commitment: 2
   - private-document: 0, it goes through bulk-append

   GroveDB opens their contexts, by path (`grovedb/src/operations/*_tree.rs`, the verify and state-root helpers in `grovedb/src/lib.rs`, the proof generators) or by prefix (state sync, `grovedb/src/replication/non_merk_sync.rs`). GroveDB also reads and writes a few flat records itself: the dense value read, and the commitment frontier in proofs and state sync. `FlatStore` therefore needs get, put and a cursor. Range delete is used only by GroveDB (`clear` on flat namespaces). No delete call by key is made on flat namespaces in library code.
5. **Cursor over flat namespaces.** No library call iterates a flat namespace. Only tests do (`flat_drop_tests.rs`, `private_document_store_tests.rs`, `non_merk_subtree_discovery_tests.rs`, `commitment_tree_tests.rs`). The register's `FlatStore` cursor is needed for aux (`get_aux_by_key_prefix`) and for those tests.
6. **`clear` leaves three column families behind.** `PrefixedRocksDbTransactionContext::clear` deletes key by key through a transactional iterator, in the default column family only. The `aux`, `roots` and `meta` records under the prefix stay. Each record costs a committed-state read at batch commit (`CLR`). `delete_prefix_ranges` and `wipe` cover all four families, outside the transaction and outside the cost model (`DB`).
7. **Two paths bypass the transaction.** The flat-drop drain (`GroveDb::flush_pending_prefix_drops`, `grovedb/src/operations/delete/flat_drop.rs:410` to 419) reads redo records and writes range tombstones and deletes directly on the database. `GroveDb::wipe` and `GroveDb::ingest_subtree_sst` (`grovedb/src/lib.rs:433`, 481) do the same. LigninDB has to provide these as engine operations with the same crash-safety contract. The redo record is staged in the meta family, in the same commit as the parent delete (`flat_drop.rs:321`, `grovedb/src/batch/mod.rs:6989`).
8. **The partial batch drives RocksDB write batches itself.** `apply_partial_batch_with_batch_flags_update` calls `build_write_batch`, `take_pending`, `continue_write_batch` and `commit_db_write_batch` (`grovedb/src/batch/mod.rs:8265` to 8782). The multi-context batch of SEAM-04 and TXN-08 has to offer "commit what is staged so far, keep staging" in engine-neutral terms.
9. **Costs that depend on the context type.** Reads on an immediate context are free, and its batch charges at fill time. `Merk::commit` drops the batch's returned cost when `merk_versions.batch.commit == 0` (`merk/src/merk/mod.rs:535`). On a transactional context that cost is zero anyway: it is charged later by `B-commit`. On an immediate context it is the whole write cost. BASE-06 has to specify both. SEAM-05 has to keep both while the RocksDB path charges through the model.
10. **Uncharged reads.** `pinned_view.rs:96` and 99 read the key and value without billing them; the caller charges one seek and the bytes itself. The library sites marked "cost dropped by the caller" are the other places where a charged cost is discarded. A cost model that charges at a different layer must reproduce these omissions, not fill them in.
11. **Estimates use a storage-crate trait outside costing.** `WorstKeyLength::max_length` orders batch keys (`Ord for KeyInfo`, `grovedb/src/batch/key_info.rs:85`) and builds synthetic keys (`grovedb/src/batch/batch_structure.rs:72`). SEAM-08 has to move the trait so this ordering does not need an engine.
12. **RocksDB-specific inherent methods are called directly.** `RocksDbStorage::build_prefix`, `secondary_prefix_for`, `start_snapshot_read_transaction`, `wipe`, `delete_prefix_ranges`, `ingest_subtree_sst`, the write-batch methods, `PrefixedRocksDbTransactionContext::clear` and `PrefixedRocksDbRawIterator::retarget` are not on any trait. The scanned crates name `RocksDbStorage` 235 times in 48 files (213 of them in GroveDB), and `PrefixedRocksDbTransactionContext` 96 times in 28 files (78 in GroveDB) ([Appendix A](#appendix-a-type-trait-and-import-references)). `grovedb/src/lib.rs:336` defines `Transaction<'db>` as `<RocksDbStorage as Storage<'db>>::Transaction`. These are SEAM-04's and SEAM-07's work list.
13. **Mock storage contexts exist outside the storage crate.** They will need a `FlatStore` or `NodeStore` counterpart, or a rewrite:
    - `grovedb-bulk-append-tree/src/test_utils.rs:58` (`MemStorageContext`, exported by the `test-utils` feature), with `MemBatch` and `MemRawIterator`
    - `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs:39` and `:203`
    - `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs:139` and `:270`
    - `grovedb-commitment-tree/src/commitment_tree/tests.rs:176` and `:330`
    - `RawIterator` implementations in `grovedb/src/batch/move_element.rs:1009`, `grovedb/src/tests/snapshot_fetch_node_tests.rs:15` and `merk/src/proofs/query/count_offset/tests.rs:2451`

    `WorstKeyLength` is implemented by `KeyInfo` (`grovedb/src/batch/key_info.rs:110`).

## Outside the `grovedb-storage` API

- `grovedb/src/tests/commitment_tree_tests.rs:30` uses the `rocksdb` crate directly (`Options`, `SstFileWriter`) to build an SST for `ingest_subtree_sst`. `grovedb/Cargo.toml` depends on `rocksdb` for that test and for `unsafe-dump-load`. No library code in the scanned crates names the `rocksdb` crate. No crate uses the `rocksdb` items re-exported by `grovedb_storage::rocksdb_storage` (`Error`, `WriteBatchWithTransaction`).
- `grovedb-epoch-based-storage-flags`, `grovedb-element`, `grovedb-version`, `costs`, `path` and `visualize` do not depend on `grovedb-storage`. The `lignin-*` crates do not either.

## Inventory: library code

Columns:

- **Site**: `path:line` at the pinned commit.
- **Function**: the enclosing function, qualified by its `impl` or trait.
- **API item**: the `grovedb-storage` item called.
- **Calls**: how many calls of that item are on that line.
- **Class**, **Seam**, **Cost**: as defined above.
- **Note**: what the call is for, where that is not obvious.

### grovedb-merk

{{LIB_merk}}

### grovedb-query

{{LIB_grovedb-query}}

### grovedb-merkle-mountain-range

{{LIB_grovedb-merkle-mountain-range}}

### grovedb-bulk-append-tree

{{LIB_grovedb-bulk-append-tree}}

### grovedb-dense-fixed-sized-merkle-tree

{{LIB_grovedb-dense-fixed-sized-merkle-tree}}

### grovedb-commitment-tree

{{LIB_grovedb-commitment-tree}}

### grovedb

{{LIB_grovedb}}

## Inventory: test-only code

These sites compile only into test, bench or example targets, or under `#[cfg(test)]`. They are classified the same way. A raw get or put on a Merk namespace in a GroveDB test is an escape hatch: it tampers with or inspects stored records. In the flat-tree test files it is flat key-value. SEAM-06 (engine-generic tests) is the main reader of this part.

### grovedb-merk

{{TEST_merk}}

### grovedb-commitment-tree

{{TEST_grovedb-commitment-tree}}

### grovedb

{{TEST_grovedb}}

## Appendix A: type, trait and import references

Each line counts the places that name a `grovedb-storage` type, trait, associated type, variant or field, or import a function, outside `storage/`. These places call nothing. They are the type-level coupling SEAM-04 and SEAM-07 remove. The `## types` section of the search output lists each one by line.

{{TYPES}}
