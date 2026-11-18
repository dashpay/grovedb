# Storage surface inventory

LigninDB task BASE-04 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 1](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960582299)).

This document lists every call from Merk, GroveDB and the append-only tree crates into `grovedb-storage`. Each site has a class, the seam that will own it, and the cost the call charges. BASE-06 (cost semantics), SEAM-01 (`NodeStore`), SEAM-03 (`FlatStore`), SEAM-04 (`Engine`) and SEAM-08 (engine-free estimates) build on it.

**Pinned commit:** `f69acc954a111a0f58a425b74e9f495715301cfd` (`feat/LigninDB`, "chore: scaffold the LigninDB workspace crates"). Every `path:line` below is at that commit. Each site also names its enclosing function, so it can be found again after lines move.

**Result:** 942 call sites, 944 calls (469 sites / 469 calls in library code, 473 sites / 475 calls only in test targets); 1249 type, trait and import references.

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
    --sites sites.tsv --doc docs/lignindb/storage-surface.md
```

`check` reads every inventory row below (site, API item and call count). It fails if a site is missing on either side or the counts differ. It ends with `OK: the document lists exactly the call sites the search found`.

What the search does not cover:

- Code behind a `cfg` that `--all-features` on a debug build leaves off. In the scanned crates those are `cfg(not(feature = "minimal"))` stubs in two benches, `cfg(not(feature = "server"))` in one bench, `cfg(not(feature = "grovedbg"))` in `build.rs`, and two `target_os` branches in one test. None of them touch storage.
- Doc-test code. It is never compiled by `cargo check`.
- Targets the `Default` probe cannot compile, because the GroveDB library they depend on fails in it. These are GroveDB's 8 integration tests and 5 benches; the output lists them under `## default-probe-unchecked`. The main passes do cover them. Only `grovedb/tests/state_sync_serving_tree_type.rs` uses storage there, and it names neither `StorageBatch` nor `TempStorage`. `grep -nE 'default\(\)' grovedb/tests/*.rs grovedb/benches/*.rs` finds only struct-update `..Default::default()` on option structs and `StorageRemovalPerEpochByIdentifier::default()`. So no unlisted `Default` call exists there.
- Other trait impls on storage types: `Debug` formatting, `Deref` from `TempStorage` to `RocksDbStorage`, and the `Display`, `Error` and `From` impls of `grovedb_storage::Error`. These are not storage operations and are not counted. A method reached through the `Deref` is a marked `RocksDbStorage` method and is counted.
- Direct use of the `rocksdb` crate, which is not `grovedb-storage`. It is listed separately [below](#outside-the-grovedb-storage-api).

## The API and what each call charges

`grovedb-storage` exposes four traits (`storage/src/storage.rs`): `Storage` (line 60), `StorageContext` (148), `Batch` (245) and `RawIterator` (284). It also has the `StorageBatch` struct, and the RocksDB implementation in `storage/src/rocksdb_storage/`.

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

Some library sites throw the returned cost away (`.unwrap()` or `.value` on the `CostContext`). The note column says "cost dropped by the caller" at those sites. That flag is found mechanically and covers only a `.unwrap()` or `.value` applied directly to the call. Mostly these are verification, replication and test-helper paths.

## Classes and seams

The register names ten classes. The inventory uses them as written and adds six, because some calls fit none of the ten.

| Class | Meaning |
| --- | --- |
| tree-node access | Merk reads or writes a node record in the default column family: point fetch, commit change set, restore writes |
| flat key-value | An append-only tree (MMR, bulk-append, dense, commitment, private-document) or GroveDB on its behalf reads or writes a record in the default column family of a flat namespace |
| aux | `aux` column family |
| meta | `meta` column family, including the pending-prefix-drop redo records |
| roots | `roots` column family (the Merk root-key pointer) |
| iterator | an ordered cursor over a namespace: `raw_iter` and every `RawIterator` method |
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
- **FlatStore** (SEAM-03): flat records, aux, meta and pending-drop records, and range deletes over flat namespaces.
- **Engine** (SEAM-04): opening subtree handles by path and by prefix, transactions, the multi-context batch, checkpoints, wipe, and the database-level drop reclamation.
- **Estimates** (SEAM-08): the `estimated_costs` helpers.

A `clear` that empties a flat tree's data namespace is assigned to FlatStore. One that empties a Merk or an indexed secondary is assigned to Engine.

## Summary

Library code, calls per class and crate (`grovedb-private-document-store` makes no call of its own: it reaches storage only through `grovedb-bulk-append-tree`):

| Class | merk | query | MMR | bulk | dense | commitment | grovedb | Total |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| tree-node access | 17 |  |  |  |  |  |  | 17 |
| flat key-value |  |  | 2 | 3 | 4 | 2 | 5 | 16 |
| aux | 3 |  |  |  |  |  | 6 | 9 |
| meta | 3 |  |  |  |  |  | 4 | 7 |
| roots | 5 |  |  |  |  |  |  | 5 |
| iterator | 20 | 30 |  |  |  |  | 45 | 95 |
| path open | 4 |  |  |  |  |  | 78 | 82 |
| by-prefix open |  |  |  |  |  |  | 18 | 18 |
| immediate context |  |  |  |  |  |  | 10 | 10 |
| range delete |  |  |  |  |  |  | 14 | 14 |
| escape hatch |  |  |  |  |  |  | 28 | 28 |
| batch | 14 |  |  |  |  |  | 58 | 72 |
| transaction | 4 |  |  |  |  |  | 6 | 10 |
| lifecycle | 1 |  |  |  |  |  | 4 | 5 |
| prefix |  |  |  |  |  |  | 30 | 30 |
| estimate |  |  |  |  |  |  | 51 | 51 |
| **total** | 71 | 30 | 2 | 3 | 4 | 2 | 357 | **469** |

Library code, per seam:

| Seam | Calls | Classes |
| --- | --- | --- |
| NodeStore | 146 | tree-node access 17, roots 5, iterator 95, escape hatch 19, batch 10 |
| FlatStore | 42 | flat key-value 16, aux 9, meta 7, range delete 6, escape hatch 2, prefix 2 |
| Engine | 230 | path open 82, by-prefix open 18, immediate context 10, range delete 8, escape hatch 7, batch 62, transaction 10, lifecycle 5, prefix 28 |
| Estimates | 51 | estimate 51 |

Test-only code (test, bench and example targets, and `#[cfg(test)]` items):

| Class | merk | commitment | grovedb | Total |
| --- | --- | --- | --- | --- |
| tree-node access | 4 |  |  | 4 |
| flat key-value |  |  | 3 | 3 |
| meta |  |  | 1 | 1 |
| iterator | 25 |  | 34 | 59 |
| path open | 17 | 1 | 15 | 33 |
| by-prefix open |  |  | 8 | 8 |
| immediate context | 20 |  | 6 | 26 |
| range delete |  |  | 1 | 1 |
| escape hatch |  |  | 11 | 11 |
| batch | 22 | 2 | 138 | 162 |
| transaction | 38 | 2 | 53 | 93 |
| lifecycle | 35 | 1 | 2 | 38 |
| prefix |  |  | 34 | 34 |
| estimate |  |  | 2 | 2 |
| **total** | 161 | 6 | 308 | **475** |

## Findings for the seams

These follow from the inventory. They are what SEAM-01, SEAM-03, SEAM-04, SEAM-08 and BASE-06 need to account for.

1. **Merk's own surface is small.** Merk makes 71 library calls. Thirteen of them are in `merk/src/test_utils/`, which the `test_utils` feature (part of `full`) compiles into the library. That is also the only library code in Merk that names a RocksDB type (`RocksDbStorage`, `PrefixedRocksDbTransactionContext`, `TempStorage`). Node fetch is `TreeNode::get` (`merk/src/tree/encoding.rs:73`) and `fetch_node` (`merk/src/merk/mod.rs:1091`). Commit is one `new_batch`, the `Batch` puts and deletes and one `commit_batch` in `Merk::commit` (`merk/src/merk/mod.rs:424` to 535). The root pointer is handled by `load_base_root` and `set_base_root_key`. Restore writes through `Restorer` (`merk/src/merk/restore.rs`), and ordered walks through `KVIterator`, `is_empty_tree` and `clear`. The two traits the plan names sit directly above these calls. `Fetch` for `MerkSource` (`merk/src/merk/source.rs:43`) calls `TreeNode::get`. `Commit` for `MerkCommitter` (`merk/src/merk/committer.rs:27`) collects the change set that `Merk::commit` writes through `Batch`.
2. **Merk's cursor is used outside Merk.** `grovedb-query` (`QueryItem::seek_for_iter`, 30 calls) positions node iterators. GroveDB iterates raw Merk records in 45 library calls (`Element::iterator`, `KVIterator::new(merk.storage.raw_iter(), ..)`, `ElementsIterator`, replication `visit_nodes`). It also decodes node records it reads through iterators itself (`snapshot_fetch_node`, `merk/src/proofs/query/count_offset/pinned_view.rs`). The `NodeStore` cursor must serve all of these, not only Merk.
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

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `merk/src/element/get.rs:386` | `Element::get_optional_from_storage_v0` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm | `Element::get_optional_from_storage_v0`: raw node record read, decoded as an element |
| `merk/src/element/get.rs:516` | `Element::get_optional_from_storage_v1` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm | `Element::get_optional_from_storage_v1` |
| `merk/src/merk/clear.rs:19` | `Merk::clear` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero | `Merk::clear`: walks every node record of the Merk |
| `merk/src/merk/clear.rs:20` | `Merk::clear` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/clear.rs:22` | `Merk::clear` | `StorageContext::new_batch` | 1 | batch | NodeStore | zero |  |
| `merk/src/merk/clear.rs:23` | `Merk::clear` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |  |
| `merk/src/merk/clear.rs:24` | `Merk::clear` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `merk/src/merk/clear.rs:25` | `Merk::clear` | `Batch::delete` | 1 | tree-node access | NodeStore | B-part / B-imm | one delete per node, no cost info |
| `merk/src/merk/clear.rs:27` | `Merk::clear` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/clear.rs:31` | `Merk::clear` | `StorageContext::commit_batch` | 1 | batch | NodeStore | B-part / B-imm | commit of the clear batch |
| `merk/src/merk/get.rs:134` | `Merk::get_aux` | `StorageContext::get_aux` | 1 | aux | FlatStore | R-tx / R-imm | `Merk::get_aux` |
| `merk/src/merk/meta.rs:18` | `Merk::get_meta` | `StorageContext::get_meta` | 1 | meta | FlatStore | R-tx / R-imm | Merk meta cache, read-through |
| `merk/src/merk/meta.rs:27` | `Merk::put_meta` | `StorageContext::put_meta` | 1 | meta | FlatStore | W-tx / W-imm |  |
| `merk/src/merk/meta.rs:42` | `Merk::delete_meta` | `StorageContext::delete_meta` | 1 | meta | FlatStore | W-tx / W-imm |  |
| `merk/src/merk/mod.rs:176` | `KVIterator::get_kv` | `RawIterator::key` | 1 | iterator | NodeStore | I-key | `KVIterator`: ordered walk of node records |
| `merk/src/merk/mod.rs:181` | `KVIterator::get_kv` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |  |
| `merk/src/merk/mod.rs:187` | `KVIterator::get_kv` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/mod.rs:189` | `KVIterator::get_kv` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/mod.rs:424` | `Merk::commit` | `StorageContext::new_batch` | 1 | batch | NodeStore | zero | `Merk::commit`: change set of one commit, written as one batch |
| `merk/src/merk/mod.rs:465` | `Merk::commit` | `Batch::put_root` | 1 | roots | NodeStore | B-part / B-imm | root-key pointer when the root moved (cost info only for standalone Merks) |
| `merk/src/merk/mod.rs:481` | `Merk::commit` | `Batch::delete_root` | 1 | roots | NodeStore | B-part / B-imm | root-key pointer removed when the tree became empty |
| `merk/src/merk/mod.rs:501` | `Merk::commit` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm | every node the committer wrote, with child sizes and storage cost |
| `merk/src/merk/mod.rs:510` | `Merk::commit` | `Batch::delete` | 1 | tree-node access | NodeStore | B-part / B-imm | deleted node keys, with removed-bytes cost info |
| `merk/src/merk/mod.rs:519` | `Merk::commit` | `Batch::put_aux` | 1 | aux | FlatStore | B-part / B-imm | aux puts carried in the same commit |
| `merk/src/merk/mod.rs:522` | `Merk::commit` | `Batch::delete_aux` | 1 | aux | FlatStore | B-part / B-imm | aux deletes carried in the same commit |
| `merk/src/merk/mod.rs:535` | `Merk::commit` | `StorageContext::commit_batch` | 1 | batch | NodeStore | B-part / B-imm | commit of the batch; GROVE versions with `merk_versions.batch.commit == 0` drop the cost it returns |
| `merk/src/merk/mod.rs:561` | `Merk::is_empty_tree` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero | `is_empty_tree`: one seek |
| `merk/src/merk/mod.rs:562` | `Merk::is_empty_tree` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/mod.rs:562` | `Merk::is_empty_tree` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |  |
| `merk/src/merk/mod.rs:569` | `Merk::is_empty_tree_except` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero | `is_empty_tree_except` |
| `merk/src/merk/mod.rs:570` | `Merk::is_empty_tree_except` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/mod.rs:571` | `Merk::is_empty_tree_except` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `merk/src/merk/mod.rs:575` | `Merk::is_empty_tree_except` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `merk/src/merk/mod.rs:608` | `Merk::set_base_root_key` | `StorageContext::put_root` | 1 | roots | NodeStore | W-tx / W-imm | `set_base_root_key`: writes the root-key pointer outside a commit, no cost info |
| `merk/src/merk/mod.rs:612` | `Merk::set_base_root_key` | `StorageContext::delete_root` | 1 | roots | NodeStore | W-tx / W-imm | `set_base_root_key(None)`: no cost info, so the batch commit estimates freed bytes from committed state |
| `merk/src/merk/mod.rs:628` | `Merk::load_base_root` | `StorageContext::get_root` | 1 | roots | NodeStore | R-tx / R-imm | `load_base_root`: reads the root-key pointer on open |
| `merk/src/merk/mod.rs:1091` | `fetch_node` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm | `fetch_node`: parent node read during chunk restore (`Restorer`, `restore.rs:661`); cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/merk/restore.rs:384` | `Restorer::write_chunk` | `StorageContext::new_batch` | 1 | batch | NodeStore | zero | chunk restore writes restored nodes in one batch (immediate context in state sync) |
| `merk/src/merk/restore.rs:465` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:484` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:523` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:542` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:562` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:586` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:605` | `Restorer::write_chunk` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:635` | `Restorer::write_chunk` | `StorageContext::commit_batch` | 1 | batch | NodeStore | B-part / B-imm | batch commit; caller reads `.value` and drops the cost; cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/merk/restore.rs:688` | `Restorer::rewrite_parent_link` | `StorageContext::put` | 1 | tree-node access | NodeStore | W-tx / W-imm | rewrites a parent node's link record in place; cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/merk/restore.rs:767` | `Restorer::rewrite_child_heights` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:773` | `Restorer::rewrite_heights` | `StorageContext::new_batch` | 1 | batch | NodeStore | zero | height rewrite pass after restore |
| `merk/src/merk/restore.rs:790` | `Restorer::rewrite_heights` | `StorageContext::commit_batch` | 1 | batch | NodeStore | B-part / B-imm | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/merk/restore.rs:943` | `Restorer::rewrite_child_aggregates` | `Batch::put` | 1 | tree-node access | NodeStore | B-part / B-imm |  |
| `merk/src/merk/restore.rs:951` | `Restorer::rewrite_aggregates` | `StorageContext::new_batch` | 1 | batch | NodeStore | zero | aggregate rewrite pass after restore |
| `merk/src/merk/restore.rs:973` | `Restorer::rewrite_aggregates` | `StorageContext::commit_batch` | 1 | batch | NodeStore | B-part / B-imm | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/proofs/query/count_offset/pinned_view.rs:93` | `stored_node_bytes` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move | point read of a stored node through a pinned iterator view |
| `merk/src/proofs/query/count_offset/pinned_view.rs:95` | `stored_node_bytes` | `RawIterator::status` | 1 | iterator | NodeStore | zero |  |
| `merk/src/proofs/query/count_offset/pinned_view.rs:96` | `stored_node_bytes` | `RawIterator::key` | 1 | iterator | NodeStore | I-key | cost of the key read is not billed (`.unwrap()`) |
| `merk/src/proofs/query/count_offset/pinned_view.rs:99` | `stored_node_bytes` | `RawIterator::value` | 1 | iterator | NodeStore | I-value | cost of the value read is not billed; the caller charges one seek and the bytes itself |
| `merk/src/test_utils/mod.rs:324` | `empty_path_merk` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/test_utils/mod.rs:345` | `empty_path_merk_read_only` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/test_utils/temp_merk.rs:68` | `TempMerk::new_with_tree_type` | `TempStorage::new` | 1 | lifecycle | Engine | zero | test helper compiled into the lib by the `test_utils` feature |
| `merk/src/test_utils/temp_merk.rs:69` | `TempMerk::new_with_tree_type` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `merk/src/test_utils/temp_merk.rs:70` | `TempMerk::new_with_tree_type` | `Storage::start_transaction` | 1 | transaction | Engine | zero |  |
| `merk/src/test_utils/temp_merk.rs:73` | `TempMerk::new_with_tree_type` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/test_utils/temp_merk.rs:103` | `TempMerk::commit` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/test_utils/temp_merk.rs:107` | `TempMerk::commit` | `Storage::commit_transaction` | 1 | transaction | Engine | zero | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/test_utils/temp_merk.rs:110` | `TempMerk::commit` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `merk/src/test_utils/temp_merk.rs:111` | `TempMerk::commit` | `Storage::start_transaction` | 1 | transaction | Engine | zero |  |
| `merk/src/test_utils/temp_merk.rs:114` | `TempMerk::commit` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/test_utils/temp_merk.rs:137` | `TempMerk::drop` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `merk/src/test_utils/temp_merk.rs:138` | `TempMerk::drop` | `Storage::commit_transaction` | 1 | transaction | Engine | zero | cost dropped by the caller (`.unwrap()` / `.value`) |
| `merk/src/tree/encoding.rs:73` | `TreeNode::get` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm | `TreeNode::get`: the node fetch behind `Fetch` (one record per node) |

### grovedb-query

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `grovedb-query/src/query_item/mod.rs:1109` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move | `QueryItem::seek_for_iter`: positions a node iterator for a query range |
| `grovedb-query/src/query_item/mod.rs:1112` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1114` | `QueryItem::seek_for_iter` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1114` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1119` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1121` | `QueryItem::seek_for_iter` | `RawIterator::seek_for_prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1126` | `QueryItem::seek_for_iter` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1128` | `QueryItem::seek_for_iter` | `RawIterator::seek_to_last` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1133` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1135` | `QueryItem::seek_for_iter` | `RawIterator::seek_to_last` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1140` | `QueryItem::seek_for_iter` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1142` | `QueryItem::seek_for_iter` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1142` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1147` | `QueryItem::seek_for_iter` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1149` | `QueryItem::seek_for_iter` | `RawIterator::seek_for_prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1155` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1156` | `QueryItem::seek_for_iter` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb-query/src/query_item/mod.rs:1159` | `QueryItem::seek_for_iter` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1164` | `QueryItem::seek_for_iter` | `RawIterator::seek_to_last` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1170` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1171` | `QueryItem::seek_for_iter` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb-query/src/query_item/mod.rs:1174` | `QueryItem::seek_for_iter` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1179` | `QueryItem::seek_for_iter` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1179` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1186` | `QueryItem::seek_for_iter` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1187` | `QueryItem::seek_for_iter` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb-query/src/query_item/mod.rs:1190` | `QueryItem::seek_for_iter` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1196` | `QueryItem::seek_for_iter` | `RawIterator::seek_for_prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb-query/src/query_item/mod.rs:1236` | `QueryItem::iter_is_valid_for_type` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key | `iter_is_valid_for_type` |
| `grovedb-query/src/query_item/mod.rs:1257` | `QueryItem::iter_is_valid_for_type` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |

### grovedb-merkle-mountain-range

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `grovedb-merkle-mountain-range/src/storage_adapter.rs:159` | `MmrStore::element_at_position` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx / R-imm | `MmrStore`: MMR node by position |
| `grovedb-merkle-mountain-range/src/storage_adapter.rs:207` | `MmrStore::append` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx / W-imm | `MmrStore::append` |

### grovedb-bulk-append-tree

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `grovedb-bulk-append-tree/src/tree/append.rs:743` | `BulkAppendTree::get_mmr_root_with_cost` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx / R-imm | cached MMR root record |
| `grovedb-bulk-append-tree/src/tree/append.rs:783` | `BulkAppendTree::persist_mmr_root` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx / W-imm | persists the MMR root record; cost dropped (zero on a transactional context) |
| `grovedb-bulk-append-tree/src/tree/append.rs:809` | `BulkAppendTree::ensure_mmr_root_resolved` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx / R-imm | MMR root record; read cost dropped by the caller |

### grovedb-dense-fixed-sized-merkle-tree

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:484` | `DenseFixedSizedMerkleTree::get_value` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx / R-imm | value at a position |
| `grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:535` | `DenseFixedSizedMerkleTree::put_value` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx / W-imm | value at a position |
| `grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:599` | `DenseFixedSizedMerkleTree::read_record_from_storage` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx / R-imm | hash record |
| `grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs:687` | `DenseFixedSizedMerkleTree::put_record` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx / W-imm | hash record |

### grovedb-commitment-tree

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `grovedb-commitment-tree/src/commitment_tree/mod.rs:228` | `CommitmentTree::open` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx / R-imm | frontier record on open |
| `grovedb-commitment-tree/src/commitment_tree/mod.rs:644` | `CommitmentTree::save` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx / W-imm | frontier record on save |

### grovedb

| Site | Function | API item | Calls | Class | Seam | Cost | Note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `grovedb/src/batch/batch_structure.rs:72` | `keyless_op_synthetic_key` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST | synthetic key for keyless ops, not a cost |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs:104` | `GroveOp::average_case_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs:106` | `GroveOp::average_case_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/estimated_costs/average_case_costs.rs:257` | `GroveOp::average_case_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs:91` | `GroveOp::worst_case_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs:93` | `GroveOp::worst_case_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/estimated_costs/worst_case_costs.rs:233` | `GroveOp::worst_case_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/key_info.rs:85` | `KeyInfo::cmp` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST | `Ord for KeyInfo`: orders batch keys by `max_length`, not a cost |
| `grovedb/src/batch/key_info.rs:86` | `KeyInfo::cmp` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/batch/mod.rs:6836` | `GroveDb::open_batch_transactional_merk_at_path` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:6851` | `GroveDb::open_batch_transactional_merk_at_path` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:6932` | `GroveDb::open_batch_transactional_merk_with_parent_element` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:6979` | `GroveDb::stage_flat_drop_records` | `PendingPrefixDropRecord::encode` | 1 | meta | FlatStore | zero | pending-prefix-drop redo record |
| `grovedb/src/batch/mod.rs:6982` | `GroveDb::stage_flat_drop_records` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | meta namespace of pending-prefix-drop records |
| `grovedb/src/batch/mod.rs:6983` | `GroveDb::stage_flat_drop_records` | `pending_prefix_drops_namespace` | 1 | prefix | FlatStore | zero |  |
| `grovedb/src/batch/mod.rs:6989` | `GroveDb::stage_flat_drop_records` | `StorageContext::put_meta` | 1 | meta | FlatStore | W-tx | redo record staged in the batch |
| `grovedb/src/batch/mod.rs:7076` | `GroveDb::scan_delete_tree_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:7217` | `GroveDb::scan_delete_tree_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:7559` | `GroveDb::apply_batch_with_batch_flags_update` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/batch/mod.rs:7795` | `GroveDb::apply_batch_with_batch_flags_update` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:7803` | `GroveDb::apply_batch_with_batch_flags_update` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | FlatStore | CLR | clears a deleted flat tree's data namespace |
| `grovedb/src/batch/mod.rs:7861` | `GroveDb::apply_batch_with_batch_flags_update` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:7869` | `GroveDb::apply_batch_with_batch_flags_update` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/batch/mod.rs:7876` | `GroveDb::apply_batch_with_batch_flags_update` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/batch/mod.rs:7884` | `GroveDb::apply_batch_with_batch_flags_update` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | Engine | CLR | clears an indexed tree's secondary namespace |
| `grovedb/src/batch/mod.rs:7930` | `GroveDb::apply_batch_with_batch_flags_update` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/batch/mod.rs:8115` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/batch/mod.rs:8265` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `RocksDbStorage::build_write_batch` | 1 | escape hatch | Engine | B-commit | partial batch: builds a RocksDB `WriteBatch` directly |
| `grovedb/src/batch/mod.rs:8265` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `StorageBatch::take_pending` | 1 | batch | Engine | zero | partial batch: builds a RocksDB `WriteBatch` directly |
| `grovedb/src/batch/mod.rs:8653` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:8661` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | FlatStore | CLR | clears a deleted flat tree's data namespace |
| `grovedb/src/batch/mod.rs:8706` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/batch/mod.rs:8714` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/batch/mod.rs:8721` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/batch/mod.rs:8729` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | Engine | CLR | clears an indexed tree's secondary namespace |
| `grovedb/src/batch/mod.rs:8772` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `RocksDbStorage::continue_write_batch` | 1 | escape hatch | Engine | B-commit | partial batch: appends to the RocksDB `WriteBatch` |
| `grovedb/src/batch/mod.rs:8782` | `GroveDb::apply_partial_batch_with_batch_flags_update` | `RocksDbStorage::commit_db_write_batch` | 1 | escape hatch | Engine | B-commit | partial batch: commits the RocksDB `WriteBatch` into the transaction |
| `grovedb/src/batch/move_element.rs:637` | `copy_next_record` | `RawIterator::valid` | 1 | escape hatch | NodeStore | I-key | raw record copy between namespaces |
| `grovedb/src/batch/move_element.rs:638` | `copy_next_record` | `RawIterator::status` | 1 | escape hatch | NodeStore | zero |  |
| `grovedb/src/batch/move_element.rs:642` | `copy_next_record` | `RawIterator::key` | 1 | escape hatch | NodeStore | I-key |  |
| `grovedb/src/batch/move_element.rs:643` | `copy_next_record` | `RawIterator::value` | 1 | escape hatch | NodeStore | I-value |  |
| `grovedb/src/batch/move_element.rs:661` | `copy_next_record` | `RawIterator::next` | 1 | escape hatch | NodeStore | I-move |  |
| `grovedb/src/batch/move_element.rs:674` | `copy_records` | `RawIterator::seek_to_first` | 1 | escape hatch | NodeStore | I-move |  |
| `grovedb/src/batch/move_element.rs:808` | `GroveDb::move_subtree_storage` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx | raw record moved to the destination namespace |
| `grovedb/src/batch/move_element.rs:815` | `GroveDb::move_subtree_storage` | `StorageContext::delete` | 1 | escape hatch | NodeStore | W-tx | raw record removed from the source namespace |
| `grovedb/src/batch/move_element.rs:881` | `GroveDb::start_tree_copy` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/batch/move_element.rs:883` | `GroveDb::start_tree_copy` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/batch/move_element.rs:885` | `GroveDb::start_tree_copy` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/batch/move_element.rs:887` | `GroveDb::start_tree_copy` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/batch/move_element.rs:900` | `GroveDb::start_tree_copy` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | source namespace by prefix |
| `grovedb/src/batch/move_element.rs:908` | `GroveDb::start_tree_copy` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | destination namespace by prefix |
| `grovedb/src/batch/move_element.rs:914` | `GroveDb::start_tree_copy` | `StorageContext::raw_iter` | 1 | escape hatch | NodeStore | zero | raw scan of the source namespace |
| `grovedb/src/batch/move_element.rs:915` | `GroveDb::start_tree_copy` | `RawIterator::seek_to_first` | 1 | escape hatch | NodeStore | I-move |  |
| `grovedb/src/batch/move_element.rs:939` | `GroveDb::move_namespace` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/batch/move_element.rs:943` | `GroveDb::move_namespace` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/batch/move_element.rs:946` | `GroveDb::move_namespace` | `StorageContext::raw_iter` | 1 | escape hatch | NodeStore | zero | raw scan of the source namespace |
| `grovedb/src/batch/move_element.rs:953` | `GroveDb::move_namespace` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx | raw record moved to the destination namespace |
| `grovedb/src/batch/move_element.rs:960` | `GroveDb::move_namespace` | `StorageContext::delete` | 1 | escape hatch | NodeStore | W-tx | raw record removed from the source namespace |
| `grovedb/src/bidirectional_references/mod.rs:98` | `crate::backward_reference_participants` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/bidirectional_references/mod.rs:104` | `crate::backward_reference_participants` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/checkpoints.rs:10` | `GroveDb::create_checkpoint` | `Storage::create_checkpoint` | 1 | lifecycle | Engine | zero |  |
| `grovedb/src/checkpoints.rs:15` | `GroveDb::open_checkpoint` | `RocksDbStorage::checkpoint_rocksdb_with_path` | 1 | lifecycle | Engine | zero |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:715` | `Element::aggregate_sum_query_item` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:718` | `Element::aggregate_sum_query_item` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:729` | `Element::aggregate_sum_query_item` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:750` | `Element::aggregate_sum_query_item` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:752` | `Element::aggregate_sum_query_item` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:759` | `Element::aggregate_sum_query_item` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:797` | `Element::aggregate_sum_query_item` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/aggregate_sum_query/mod.rs:799` | `Element::aggregate_sum_query_item` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/elements_iterator.rs:21` | `Element::iterator` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/elements_iterator.rs:45` | `ElementsIterator::next_element` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |  |
| `grovedb/src/element/elements_iterator.rs:48` | `ElementsIterator::next_element` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb/src/element/elements_iterator.rs:50` | `ElementsIterator::next_element` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |  |
| `grovedb/src/element/elements_iterator.rs:57` | `ElementsIterator::next_element` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/local_offset_read.rs:56` | `refuse_non_merk_on_offset_path` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/element/local_offset_read.rs:58` | `refuse_non_merk_on_offset_path` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/element/local_offset_read.rs:70` | `walk_offset_path` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path | prefix of each path level for a retargeted view |
| `grovedb/src/element/local_offset_read.rs:71` | `walk_offset_path` | `PrefixedRocksDbRawIterator::retarget` | 1 | escape hatch | Engine | zero | one pinned iterator walks several subtrees |
| `grovedb/src/element/local_offset_read.rs:133` | `read_local_count_page` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/element/local_offset_read.rs:135` | `read_local_count_page` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/element/local_offset_read.rs:186` | `read_local_count_page` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path | prefix of the host subtree for a retargeted view |
| `grovedb/src/element/local_offset_read.rs:190` | `read_local_count_page` | `PrefixedRocksDbRawIterator::retarget` | 1 | escape hatch | Engine | zero | one pinned iterator walks several subtrees |
| `grovedb/src/element/query.rs:659` | `query_item_internal` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/element/query.rs:662` | `query_item_internal` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/element/query.rs:677` | `query_item_internal` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |  |
| `grovedb/src/element/query.rs:690` | `query_item_internal` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb/src/element/query.rs:731` | `query_item_internal` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/element/query.rs:733` | `query_item_internal` | `RawIterator::prev` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:62` | `GroveDb::add_average_case_get_merk_at_path` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:68` | `GroveDb::add_average_case_get_merk_at_path` | `Storage::get_storage_context_cost` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:123` | `GroveDb::average_case_merk_replace_tree_v0` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:159` | `GroveDb::average_case_merk_replace_tree_v1` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:206` | `GroveDb::average_case_merk_insert_tree` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:245` | `GroveDb::average_case_merk_delete_tree` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:288` | `GroveDb::average_case_merk_patch_element` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:516` | `GroveDb::average_case_merk_delete_element` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:553` | `GroveDb::add_average_case_has_raw_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:559` | `GroveDb::add_average_case_has_raw_cost` | `Storage::get_storage_context_cost` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:614` | `GroveDb::add_average_case_get_raw_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:644` | `GroveDb::add_average_case_get_raw_tree_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:673` | `GroveDb::add_average_case_get_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs.rs:683` | `GroveDb::add_average_case_get_cost` | `Storage::get_storage_context_cost` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs/merk_insert_element/v0.rs:35` | `GroveDb::average_case_merk_insert_element_v0` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs/merk_insert_element/v1.rs:35` | `GroveDb::average_case_merk_insert_element_v1` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs/merk_insert_element/v2.rs:52` | `GroveDb::average_case_merk_insert_element_v2` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs/merk_replace_element/v0.rs:34` | `GroveDb::average_case_merk_replace_element_v0` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/average_case_costs/merk_replace_element/v1.rs:35` | `GroveDb::average_case_merk_replace_element_v1` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:55` | `GroveDb::add_worst_case_get_merk_at_path` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:61` | `GroveDb::add_worst_case_get_merk_at_path` | `Storage::get_storage_context_cost` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:84` | `GroveDb::worst_case_merk_replace_tree` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:122` | `GroveDb::worst_case_merk_insert_tree` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:158` | `GroveDb::worst_case_merk_delete_tree` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:192` | `GroveDb::worst_case_merk_patch_element` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:242` | `GroveDb::worst_case_merk_delete_element` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:272` | `GroveDb::add_worst_case_has_raw_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:278` | `GroveDb::add_worst_case_has_raw_cost` | `Storage::get_storage_context_cost` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:304` | `GroveDb::add_worst_case_get_raw_tree_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:332` | `GroveDb::add_worst_case_get_raw_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:360` | `GroveDb::add_worst_case_get_cost` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs.rs:367` | `GroveDb::add_worst_case_get_cost` | `Storage::get_storage_context_cost` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs/merk_insert_element/v0.rs:35` | `GroveDb::worst_case_merk_insert_element_v0` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs/merk_insert_element/v1.rs:35` | `GroveDb::worst_case_merk_insert_element_v1` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs/merk_insert_element/v2.rs:46` | `GroveDb::worst_case_merk_insert_element_v2` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs/merk_replace_element/v0.rs:39` | `GroveDb::worst_case_merk_replace_element_v0` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/estimated_costs/worst_case_costs/merk_replace_element/v1.rs:40` | `GroveDb::worst_case_merk_replace_element_v1` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/lib.rs:374` | `GroveDb::open` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero | `GroveDb::open` |
| `grovedb/src/lib.rs:433` | `GroveDb::wipe` | `RocksDbStorage::wipe` | 1 | range delete | Engine | DB | `GroveDb::wipe`: one range tombstone per column family, outside any transaction |
| `grovedb/src/lib.rs:481` | `GroveDb::ingest_subtree_sst` | `RocksDbStorage::ingest_subtree_sst` | 1 | escape hatch | Engine | DB | SST ingest into a column family (`unsafe-dump-load`), outside any transaction |
| `grovedb/src/lib.rs:536` | `GroveDb::open_transactional_merk_by_prefix` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | Merk opened by a known prefix (indexed secondaries) |
| `grovedb/src/lib.rs:579` | `GroveDb::open_merk_for_replication` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | state-sync restore: Merk on an immediate context |
| `grovedb/src/lib.rs:584` | `GroveDb::open_merk_for_replication` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | state-sync restore: parent Merk on an immediate context |
| `grovedb/src/lib.rs:674` | `GroveDb::open_root_merk` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | root Merk |
| `grovedb/src/lib.rs:1805` | `GroveDb::flush` | `Storage::flush` | 1 | lifecycle | Engine | zero |  |
| `grovedb/src/lib.rs:1888` | `GroveDb::start_transaction` | `Storage::start_transaction` | 1 | transaction | Engine | zero |  |
| `grovedb/src/lib.rs:1923` | `GroveDb::start_snapshot_read_transaction` | `RocksDbStorage::start_snapshot_read_transaction` | 1 | transaction | Engine | zero | snapshot read transaction (RocksDB inherent method) |
| `grovedb/src/lib.rs:1936` | `GroveDb::commit_transaction` | `Storage::commit_transaction` | 1 | transaction | Engine | zero |  |
| `grovedb/src/lib.rs:1948` | `GroveDb::rollback_transaction` | `Storage::rollback_transaction` | 1 | transaction | Engine | zero |  |
| `grovedb/src/lib.rs:2103` | `GroveDb::verify_indexed_axis_content` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/lib.rs:2170` | `GroveDb::verify_indexed_axis_content` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/lib.rs:2468` | `GroveDb::verify_merk_and_submerks_in_transaction` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/lib.rs:3364` | `GroveDb::private_document_store_entry_size_issue` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | verify: flat store context; cost dropped |
| `grovedb/src/lib.rs:3400` | `GroveDb::dense_buffer_record_issue` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3420` | `GroveDb::dense_buffer_record_issue` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3438` | `GroveDb::dense_buffer_record_issue` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3458` | `GroveDb::dense_buffer_record_issue` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3500` | `GroveDb::compute_non_merk_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | verify: flat store context; cost dropped |
| `grovedb/src/lib.rs:3522` | `GroveDb::compute_non_merk_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3541` | `GroveDb::compute_non_merk_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3556` | `GroveDb::compute_non_merk_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3579` | `GroveDb::compute_non_merk_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3647` | `GroveDb::compute_non_merk_state_root` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | flat store state root; cost dropped |
| `grovedb/src/lib.rs:3673` | `GroveDb::compute_non_merk_state_root` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3697` | `GroveDb::compute_non_merk_state_root` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3716` | `GroveDb::compute_non_merk_state_root` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/lib.rs:3747` | `GroveDb::compute_non_merk_state_root` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/merk_cache.rs:66` | `MerkCache::new` | `StorageBatch::default` | 1 | batch | Engine | zero | `batch: Default::default()`, found by the `Default` probe |
| `grovedb/src/merk_cache.rs:157` | `MerkCache::try_open_merk_using_cached_parent` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | `MerkCache` opens each Merk once per batch |
| `grovedb/src/merk_cache.rs:453` | `MerkHandle::for_merk` | `StorageBatch::next_operation` | 1 | batch | Engine | zero | marks an operation boundary in the shared `StorageBatch` |
| `grovedb/src/operations/auxiliary.rs:59` | `GroveDb::put_aux` | `StorageBatch::default` | 1 | batch | Engine | zero | `Default::default()`, found by the `Default` probe |
| `grovedb/src/operations/auxiliary.rs:63` | `GroveDb::put_aux` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | root-path context used for aux |
| `grovedb/src/operations/auxiliary.rs:69` | `GroveDb::put_aux` | `StorageContext::put_aux` | 1 | aux | FlatStore | W-tx |  |
| `grovedb/src/operations/auxiliary.rs:76` | `GroveDb::put_aux` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/auxiliary.rs:92` | `GroveDb::delete_aux` | `StorageBatch::default` | 1 | batch | Engine | zero | `Default::default()`, found by the `Default` probe |
| `grovedb/src/operations/auxiliary.rs:96` | `GroveDb::delete_aux` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | root-path context used for aux |
| `grovedb/src/operations/auxiliary.rs:102` | `GroveDb::delete_aux` | `StorageContext::delete_aux` | 1 | aux | FlatStore | W-tx |  |
| `grovedb/src/operations/auxiliary.rs:109` | `GroveDb::delete_aux` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/auxiliary.rs:127` | `GroveDb::get_aux` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | root-path context used for aux |
| `grovedb/src/operations/auxiliary.rs:131` | `GroveDb::get_aux` | `StorageContext::get_aux` | 1 | aux | FlatStore | R-tx |  |
| `grovedb/src/operations/auxiliary.rs:155` | `GroveDb::get_aux_by_key_prefix` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | root-path context used for aux |
| `grovedb/src/operations/auxiliary.rs:158` | `GroveDb::get_aux_by_key_prefix` | `StorageContext::raw_iter_aux` | 1 | aux | FlatStore | zero | prefix scan of aux keys |
| `grovedb/src/operations/auxiliary.rs:159` | `GroveDb::get_aux_by_key_prefix` | `RawIterator::seek` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/operations/auxiliary.rs:162` | `GroveDb::get_aux_by_key_prefix` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |  |
| `grovedb/src/operations/auxiliary.rs:168` | `GroveDb::get_aux_by_key_prefix` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |  |
| `grovedb/src/operations/auxiliary.rs:172` | `GroveDb::get_aux_by_key_prefix` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |  |
| `grovedb/src/operations/auxiliary.rs:254` | `GroveDb::clear_subtree_storage_recursively` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/auxiliary.rs:258` | `GroveDb::clear_subtree_storage_recursively` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | Engine | CLR | clears every Merk subtree found under a deleted path |
| `grovedb/src/operations/auxiliary.rs:268` | `GroveDb::clear_subtree_storage_recursively` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/auxiliary.rs:271` | `GroveDb::clear_subtree_storage_recursively` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/operations/auxiliary.rs:275` | `GroveDb::clear_subtree_storage_recursively` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/operations/auxiliary.rs:283` | `GroveDb::clear_subtree_storage_recursively` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | Engine | CLR | clears an indexed tree's secondary namespace |
| `grovedb/src/operations/auxiliary/find_subtrees/v0.rs:46` | `GroveDb::find_subtrees_v0` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/auxiliary/find_subtrees/v0.rs:48` | `GroveDb::find_subtrees_v0` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/auxiliary/find_subtrees/v1.rs:94` | `GroveDb::walk_subtrees_v1` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/auxiliary/find_subtrees/v1.rs:96` | `GroveDb::walk_subtrees_v1` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/bulk_append_tree.rs:78` | `GroveDb::bulk_append` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/bulk_append_tree.rs:81` | `GroveDb::bulk_append` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/bulk_append_tree.rs:121` | `GroveDb::bulk_append` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/bulk_append_tree.rs:126` | `GroveDb::bulk_append` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/bulk_append_tree.rs:192` | `GroveDb::bulk_append` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/bulk_append_tree.rs:245` | `GroveDb::bulk_get_value` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/bulk_append_tree.rs:336` | `GroveDb::bulk_get_range` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/bulk_append_tree.rs:393` | `GroveDb::bulk_get_chunk` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/bulk_append_tree.rs:447` | `GroveDb::bulk_get_buffer` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/bulk_append_tree.rs:615` | `GroveDb::preprocess_bulk_append_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/commitment_tree.rs:146` | `GroveDb::commitment_tree_insert_raw` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/commitment_tree.rs:149` | `GroveDb::commitment_tree_insert_raw` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/commitment_tree.rs:206` | `GroveDb::commitment_tree_insert_raw` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/commitment_tree.rs:211` | `GroveDb::commitment_tree_insert_raw` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/commitment_tree.rs:277` | `GroveDb::commitment_tree_insert_raw` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/commitment_tree.rs:326` | `GroveDb::commitment_tree_anchor` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/commitment_tree.rs:387` | `GroveDb::commitment_tree_get_value` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/commitment_tree.rs:484` | `GroveDb::commitment_tree_get_range` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/commitment_tree.rs:638` | `GroveDb::preprocess_commitment_tree_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/delete/average_case.rs:87` | `GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/operations/delete/average_case.rs:112` | `GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/operations/delete/clear_subtree/v0.rs:42` | `GroveDb::clear_subtree_with_costs_v0` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/delete/clear_subtree/v0.rs:73` | `GroveDb::clear_subtree_with_costs_v0` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/delete/clear_subtree/v0.rs:77` | `GroveDb::clear_subtree_with_costs_v0` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | FlatStore | CLR |  |
| `grovedb/src/operations/delete/clear_subtree/v0.rs:87` | `GroveDb::clear_subtree_with_costs_v0` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/clear_subtree/v0.rs:99` | `GroveDb::clear_subtree_with_costs_v0` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/delete/clear_subtree/v0.rs:163` | `GroveDb::clear_subtree_with_costs_v0` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/clear_subtree/v1.rs:50` | `GroveDb::clear_subtree_with_costs_v1` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/delete/clear_subtree/v1.rs:146` | `GroveDb::clear_subtree_with_costs_v1` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/delete/clear_subtree/v1.rs:150` | `GroveDb::clear_subtree_with_costs_v1` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | FlatStore | CLR |  |
| `grovedb/src/operations/delete/clear_subtree/v1.rs:215` | `GroveDb::clear_subtree_with_costs_v1` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/clear_subtree/v1.rs:245` | `GroveDb::clear_subtree_with_costs_v1` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/delete/clear_subtree/v1.rs:310` | `GroveDb::clear_subtree_with_costs_v1` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:169` | `GroveDb::delete_internal_on_transaction_v0` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:177` | `GroveDb::delete_internal_on_transaction_v0` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:181` | `GroveDb::delete_internal_on_transaction_v0` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:189` | `GroveDb::delete_internal_on_transaction_v0` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | Engine | CLR |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:207` | `GroveDb::delete_internal_on_transaction_v0` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:215` | `GroveDb::delete_internal_on_transaction_v0` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | FlatStore | CLR |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs:247` | `GroveDb::delete_internal_on_transaction_v0` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs:201` | `GroveDb::delete_prepared_ordinary_v1` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs:209` | `GroveDb::delete_prepared_ordinary_v1` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs:213` | `GroveDb::delete_prepared_ordinary_v1` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs:221` | `GroveDb::delete_prepared_ordinary_v1` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | Engine | CLR |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs:239` | `GroveDb::delete_prepared_ordinary_v1` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs:247` | `GroveDb::delete_prepared_ordinary_v1` | `PrefixedRocksDbTransactionContext::clear` | 1 | range delete | FlatStore | CLR |  |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs:271` | `GroveDb::delete_with_backward_references_v2` | `StorageBatch::merge_overwriting` | 1 | batch | Engine | zero | merges the traversal's batch over the main one |
| `grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs:394` | `DeletionVisitor::visit_element` | `StorageContext::delete` | 1 | escape hatch | NodeStore | W-tx | raw node-record delete during a recursive subtree teardown, bypassing Merk |
| `grovedb/src/operations/delete/flat_drop.rs:126` | `doomed_prefixes_for_drop` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/delete/flat_drop.rs:132` | `doomed_prefixes_for_drop` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/operations/delete/flat_drop.rs:163` | `add_flat_drop_record_put_estimate` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/operations/delete/flat_drop.rs:164` | `add_flat_drop_record_put_estimate` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/operations/delete/flat_drop.rs:260` | `GroveDb::drop_flat_subtree` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/delete/flat_drop.rs:309` | `GroveDb::drop_flat_subtree` | `PendingPrefixDropRecord::encode` | 1 | meta | FlatStore | zero | pending-prefix-drop redo record |
| `grovedb/src/operations/delete/flat_drop.rs:312` | `GroveDb::drop_flat_subtree` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | meta namespace of pending-prefix-drop records |
| `grovedb/src/operations/delete/flat_drop.rs:313` | `GroveDb::drop_flat_subtree` | `pending_prefix_drops_namespace` | 1 | prefix | FlatStore | zero |  |
| `grovedb/src/operations/delete/flat_drop.rs:321` | `GroveDb::drop_flat_subtree` | `StorageContext::put_meta` | 1 | meta | FlatStore | W-tx | redo record staged in the same commit as the parent delete |
| `grovedb/src/operations/delete/flat_drop.rs:364` | `GroveDb::drop_flat_subtree` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/flat_drop.rs:410` | `GroveDb::flush_pending_prefix_drops` | `RocksDbStorage::pending_prefix_drop_records` | 1 | escape hatch | FlatStore | DB | DB-level read of committed redo records, outside any transaction |
| `grovedb/src/operations/delete/flat_drop.rs:417` | `GroveDb::flush_pending_prefix_drops` | `RocksDbStorage::delete_prefix_ranges` | 1 | range delete | Engine | DB | range tombstones over each doomed prefix in all four column families, outside any transaction |
| `grovedb/src/operations/delete/flat_drop.rs:419` | `GroveDb::flush_pending_prefix_drops` | `RocksDbStorage::remove_pending_prefix_drop_record` | 1 | escape hatch | FlatStore | DB | DB-level delete of the redo record |
| `grovedb/src/operations/delete/mod.rs:199` | `GroveDb::delete` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/delete/mod.rs:225` | `GroveDb::delete` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/mod.rs:280` | `GroveDb::delete_with_sectional_storage_function` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/delete/mod.rs:316` | `GroveDb::delete_with_sectional_storage_function` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/mod.rs:358` | `GroveDb::delete_if_empty_tree` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/delete/mod.rs:381` | `GroveDb::delete_if_empty_tree` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/delete/worst_case.rs:86` | `GroveDb::worst_case_delete_operations_for_delete_up_tree_while_empty` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |  |
| `grovedb/src/operations/dense_tree.rs:77` | `GroveDb::dense_tree_insert` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/dense_tree.rs:80` | `GroveDb::dense_tree_insert` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/dense_tree.rs:110` | `GroveDb::dense_tree_insert` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/dense_tree.rs:115` | `GroveDb::dense_tree_insert` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/dense_tree.rs:181` | `GroveDb::dense_tree_insert` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/dense_tree.rs:231` | `GroveDb::dense_tree_get` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/dense_tree.rs:236` | `GroveDb::dense_tree_get` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx | dense tree value read directly by GroveDB |
| `grovedb/src/operations/dense_tree.rs:287` | `GroveDb::dense_tree_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/dense_tree.rs:448` | `GroveDb::preprocess_dense_tree_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/get/mod.rs:606` | `GroveDb::has_raw` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/get/mod.rs:607` | `GroveDb::has_raw` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx | `has_raw`: existence of a node record, read directly |
| `grovedb/src/operations/indexed_tree.rs:388` | `GroveDb::open_indexed_secondary_at_path` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/indexed_tree.rs:389` | `GroveDb::open_indexed_secondary_at_path` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/operations/indexed_tree.rs:393` | `GroveDb::open_indexed_secondary_at_path` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | indexed secondary Merk by derived prefix |
| `grovedb/src/operations/indexed_tree.rs:605` | `GroveDb::insert_into_indexed_tree_via_batch` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/indexed_tree.rs:641` | `GroveDb::insert_into_indexed_tree_via_batch` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/indexed_tree.rs:656` | `GroveDb::insert_into_indexed_tree_via_batch` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/indexed_tree.rs:706` | `GroveDb::delete_from_indexed_tree_via_batch` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/indexed_tree.rs:872` | `GroveDb::reconcile_indexed_tree_secondaries` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/indexed_tree.rs:889` | `GroveDb::reconcile_indexed_tree_secondaries` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/indexed_tree.rs:972` | `GroveDb::reconcile_indexed_tree_secondaries_on_transaction` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/indexed_tree.rs:1085` | `GroveDb::reconcile_indexed_tree_secondaries_on_transaction` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/indexed_tree.rs:1477` | `GroveDb::indexed_axis_top_k_paginated_rows_generic` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/indexed_tree.rs:1478` | `GroveDb::indexed_axis_top_k_paginated_rows_generic` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |  |
| `grovedb/src/operations/indexed_tree.rs:1479` | `GroveDb::indexed_axis_top_k_paginated_rows_generic` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |  |
| `grovedb/src/operations/indexed_tree.rs:1483` | `GroveDb::indexed_axis_top_k_paginated_rows_generic` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | parent context whose iterator is retargeted to the secondary |
| `grovedb/src/operations/indexed_tree.rs:1489` | `GroveDb::indexed_axis_top_k_paginated_rows_generic` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/indexed_tree.rs:1514` | `GroveDb::indexed_axis_top_k_paginated_rows_generic` | `PrefixedRocksDbRawIterator::retarget` | 1 | escape hatch | Engine | zero | one pinned iterator reused across prefixes |
| `grovedb/src/operations/indexed_tree.rs:1654` | `GroveDb::indexed_axis_range_rows_generic` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/indexed_tree.rs:3211` | `collect_top_k_via_iterator` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/operations/indexed_tree.rs:3526` | `snapshot_fetch_node` | `RawIterator::seek` | 1 | escape hatch | NodeStore | I-move | `snapshot_fetch_node`: node fetched and decoded by GroveDB through a pinned iterator |
| `grovedb/src/operations/indexed_tree.rs:3527` | `snapshot_fetch_node` | `RawIterator::status` | 1 | escape hatch | NodeStore | zero |  |
| `grovedb/src/operations/indexed_tree.rs:3528` | `snapshot_fetch_node` | `RawIterator::key` | 1 | escape hatch | NodeStore | I-key |  |
| `grovedb/src/operations/indexed_tree.rs:3532` | `snapshot_fetch_node` | `RawIterator::value` | 1 | escape hatch | NodeStore | I-value |  |
| `grovedb/src/operations/insert/mod.rs:129` | `GroveDb::insert` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/insert/mod.rs:149` | `GroveDb::insert` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/mmr_tree.rs:82` | `GroveDb::mmr_tree_append` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/mmr_tree.rs:85` | `GroveDb::mmr_tree_append` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/mmr_tree.rs:139` | `GroveDb::mmr_tree_append` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/mmr_tree.rs:144` | `GroveDb::mmr_tree_append` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/mmr_tree.rs:205` | `GroveDb::mmr_tree_append` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/mmr_tree.rs:257` | `GroveDb::mmr_tree_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/mmr_tree.rs:317` | `GroveDb::mmr_tree_get_value` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/mmr_tree.rs:474` | `GroveDb::preprocess_mmr_tree_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/private_document_store.rs:169` | `GroveDb::private_document_store_insert` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/private_document_store.rs:172` | `GroveDb::private_document_store_insert` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/private_document_store.rs:210` | `GroveDb::private_document_store_insert` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/private_document_store.rs:215` | `GroveDb::private_document_store_insert` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/private_document_store.rs:278` | `GroveDb::private_document_store_insert` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/operations/private_document_store.rs:347` | `GroveDb::private_document_store_get_value` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/private_document_store.rs:431` | `GroveDb::private_document_store_get_range` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/private_document_store.rs:607` | `GroveDb::preprocess_private_document_store_ops` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:156` | `GroveDb::non_merk_tree_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:173` | `GroveDb::non_merk_tree_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:200` | `GroveDb::non_merk_tree_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:225` | `GroveDb::non_merk_tree_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:229` | `GroveDb::non_merk_tree_child_hash` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx | commitment frontier read; caller reads `.value` and drops the cost; cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs:286` | `GroveDb::non_merk_tree_child_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/generate.rs:2780` | `GroveDb::prove_subqueries_v1` | `StorageBatch::new` | 1 | batch | Engine | zero | scratch batch for payload reads |
| `grovedb/src/operations/proof/generate.rs:4319` | `GroveDb::generate_mmr_layer_proof` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/generate.rs:4408` | `GroveDb::generate_bulk_append_layer_proof` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/generate.rs:4502` | `GroveDb::generate_commitment_tree_layer_proof` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/generate.rs:4505` | `GroveDb::generate_commitment_tree_layer_proof` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx | commitment frontier read; caller reads `.value` and drops the cost; cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/operations/proof/generate.rs:4595` | `GroveDb::generate_dense_tree_layer_proof` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/operations/proof/indexed_axis/generate.rs:661` | `GroveDb::compute_indexed_axis_rank_of_key` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/replace_subtree_root.rs:75` | `GroveDb::replace_subtree_root` | `StorageBatch::new` | 1 | batch | Engine | zero |  |
| `grovedb/src/operations/replace_subtree_root.rs:133` | `GroveDb::replace_subtree_root` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |  |
| `grovedb/src/replication/indexed_sync.rs:305` | `GroveDb::serve_indexed_header_page` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/non_merk_sync.rs:302` | `GroveDb::fetch_non_merk_page` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero | flat store page served by chunk prefix |
| `grovedb/src/replication/non_merk_sync.rs:308` | `GroveDb::fetch_non_merk_page` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx | commitment frontier; cost dropped |
| `grovedb/src/replication/non_merk_sync.rs:322` | `GroveDb::fetch_non_merk_page` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/replication/non_merk_sync.rs:398` | `GroveDb::fetch_non_merk_page` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/replication/non_merk_sync.rs:441` | `GroveDb::fetch_non_merk_page` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |  |
| `grovedb/src/replication/non_merk_sync.rs:674` | `NonMerkRestorer::apply_page` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | flat store restore on an immediate context; cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/non_merk_sync.rs:676` | `NonMerkRestorer::apply_page` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-imm | frontier written directly; cost dropped |
| `grovedb/src/replication/non_merk_sync.rs:687` | `NonMerkRestorer::apply_page` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/non_merk_sync.rs:708` | `NonMerkRestorer::apply_page` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/non_merk_sync.rs:727` | `NonMerkRestorer::apply_page` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/non_merk_sync.rs:751` | `NonMerkRestorer::apply_page` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/state_sync_session.rs:888` | `MultiStateSyncSession::set_incomplete_restore_marker` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path | incomplete-restore marker on the root path; cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/state_sync_session.rs:891` | `MultiStateSyncSession::set_incomplete_restore_marker` | `StorageContext::put_aux` | 1 | aux | FlatStore | W-imm | incomplete-restore marker set |
| `grovedb/src/replication/state_sync_session.rs:893` | `MultiStateSyncSession::set_incomplete_restore_marker` | `StorageContext::delete_aux` | 1 | aux | FlatStore | W-imm | incomplete-restore marker cleared |
| `grovedb/src/replication/state_sync_session.rs:983` | `MultiStateSyncSession::add_indexed_primary_sync_info` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/state_sync_session.rs:1066` | `MultiStateSyncSession::add_indexed_secondary_sync_info` | `Storage::get_immediate_storage_context_by_subtree_prefix` | 1 | immediate context | Engine | zero | indexed secondary restored on an immediate context; cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/state_sync_session.rs:1670` | `MultiStateSyncSession::discover_new_subtrees_metadata` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/replication/state_sync_session.rs:1714` | `MultiStateSyncSession::discover_new_subtrees_metadata` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:32` | `visit_nodes` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero | walks every node record of a restored Merk |
| `grovedb/src/replication/verify.rs:33` | `visit_nodes` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:34` | `visit_nodes` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:35` | `visit_nodes` | `RawIterator::key` | 1 | iterator | NodeStore | I-key | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:38` | `visit_nodes` | `RawIterator::value` | 1 | iterator | NodeStore | I-value | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:49` | `visit_nodes` | `RawIterator::next` | 1 | iterator | NodeStore | I-move | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:230` | `GroveDb::verify_restored_value_hashes` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:233` | `GroveDb::verify_restored_value_hashes` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/replication/verify.rs:236` | `GroveDb::verify_restored_value_hashes` | `Storage::get_immediate_storage_context_by_subtree_prefix` | 1 | immediate context | Engine | zero | indexed secondary checked after restore; cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/util.rs:18` | `TxRef::new` | `Storage::start_transaction` | 1 | transaction | Engine | zero |  |
| `grovedb/src/util.rs:31` | `TxRef::commit_local` | `Tx::commit` | 1 | transaction | Engine | zero | commits a transaction the call opened itself |
| `grovedb/src/util/compat.rs:45` | `open_merk` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | generic Merk open |
| `grovedb/src/util/compat.rs:49` | `open_merk` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/util/visitor.rs:58` | `GroveVisitor::new` | `StorageBatch::default` | 1 | batch | Engine | zero | `batch: Default::default()`, found by the `Default` probe |
| `grovedb/src/util/visitor.rs:78` | `GroveVisitor::walk_from` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |  |
| `grovedb/src/util/visitor.rs:93` | `GroveVisitor::walk_from` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |
| `grovedb/src/visualize.rs:35` | `GroveDb::draw_subtree` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path | cost dropped by the caller (`.unwrap()` / `.value`) |
| `grovedb/src/visualize.rs:38` | `GroveDb::draw_subtree` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |  |

## Inventory: test-only code

These sites compile only into test, bench or example targets, or under `#[cfg(test)]`. They are classified the same way. A raw get or put on a Merk namespace in a GroveDB test is an escape hatch: it tampers with or inspects stored records. In the flat-tree test files it is flat key-value. SEAM-06 (engine-generic tests) is the main reader of this part.

### grovedb-merk

| Site | Function | API item | Calls | Class | Seam | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| `merk/benches/merk.rs:402` | `restore_500_1` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/benches/merk.rs:404` | `restore_500_1` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/benches/merk.rs:406` | `restore_500_1` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/benches/merk.rs:429` | `restore_500_1` | `Storage::commit_transaction` | 1 | transaction | Engine | zero |
| `merk/src/element/get.rs:639` | `test_cache_changes_cost` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/element/get.rs:640` | `test_cache_changes_cost` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/element/get.rs:641` | `test_cache_changes_cost` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/element/get.rs:644` | `test_cache_changes_cost` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/element/get.rs:664` | `test_cache_changes_cost` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/element/get.rs:668` | `test_cache_changes_cost` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/element/get.rs:670` | `test_cache_changes_cost` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/element/insert.rs:1413` | `test_insert_if_changed_value_inserts_when_value_changed` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/element/insert.rs:1414` | `test_insert_if_changed_value_inserts_when_value_changed` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/element/insert.rs:1415` | `test_insert_if_changed_value_inserts_when_value_changed` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/element/insert.rs:1428` | `test_insert_if_changed_value_inserts_when_value_changed` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/element/insert.rs:1432` | `test_insert_if_changed_value_inserts_when_value_changed` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/element/insert.rs:1444` | `test_insert_if_changed_value_inserts_when_value_changed` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/merk/mod.rs:1518` | `actual_deletes` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm |
| `merk/src/merk/mod.rs:1607` | `reopen_check_root_hash` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1609` | `reopen_check_root_hash` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1610` | `reopen_check_root_hash` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/mod.rs:1614` | `reopen_check_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1640` | `test_get_node_cost` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1642` | `test_get_node_cost` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1643` | `test_get_node_cost` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/mod.rs:1647` | `test_get_node_cost` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1702` | `reopen` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1704` | `reopen` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/mod.rs:1705` | `reopen` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1709` | `reopen` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1727` | `reopen` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/merk/mod.rs:1733` | `reopen` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1748` | `reopen` | `Storage::commit_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1753` | `reopen` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1755` | `reopen` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1759` | `reopen` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1783` | `collect` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `merk/src/merk/mod.rs:1785` | `collect` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |
| `merk/src/merk/mod.rs:1786` | `collect` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |
| `merk/src/merk/mod.rs:1788` | `collect` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |
| `merk/src/merk/mod.rs:1794` | `reopen_iter` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1796` | `reopen_iter` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/mod.rs:1797` | `reopen_iter` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1801` | `reopen_iter` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1819` | `reopen_iter` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/merk/mod.rs:1826` | `reopen_iter` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1834` | `reopen_iter` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/merk/mod.rs:1836` | `reopen_iter` | `Storage::commit_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1841` | `reopen_iter` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1843` | `reopen_iter` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1846` | `reopen_iter` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1856` | `reopen_iter` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/merk/mod.rs:1865` | `update_node` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/mod.rs:1867` | `update_node` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/mod.rs:1868` | `update_node` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/mod.rs:1872` | `update_node` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/mod.rs:1930` | `update_node` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/merk/mod.rs:1934` | `update_node` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/mod.rs:1937` | `update_node` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/open.rs:118` | `test_reopen_root_hash` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/open.rs:120` | `test_reopen_root_hash` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/open.rs:121` | `test_reopen_root_hash` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/open.rs:127` | `test_reopen_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/open.rs:152` | `test_reopen_root_hash` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/merk/open.rs:158` | `test_reopen_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/open.rs:176` | `test_open_fee` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/open.rs:177` | `test_open_fee` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/merk/open.rs:178` | `test_open_fee` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/open.rs:182` | `test_open_fee` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/open.rs:206` | `test_open_fee` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/merk/open.rs:212` | `test_open_fee` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/merk/restore.rs:1482` | `test_hash_node_at_chunk_root_returns_error_not_panic` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:1483` | `test_hash_node_at_chunk_root_returns_error_not_panic` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:1486` | `test_hash_node_at_chunk_root_returns_error_not_panic` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:1546` | `test_forged_kv_under_hash_boundary_node_is_rejected` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:1547` | `test_forged_kv_under_hash_boundary_node_is_rejected` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:1550` | `test_forged_kv_under_hash_boundary_node_is_rejected` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:1605` | `test_process_chunk_correct_chunk_id_map` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:1606` | `test_process_chunk_correct_chunk_id_map` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:1609` | `test_process_chunk_correct_chunk_id_map` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:1822` | `assert_raw_db_entries_eq` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/merk/restore.rs:1823` | `assert_raw_db_entries_eq` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/merk/restore.rs:1824` | `assert_raw_db_entries_eq` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |
| `merk/src/merk/restore.rs:1825` | `assert_raw_db_entries_eq` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |
| `merk/src/merk/restore.rs:1830` | `assert_raw_db_entries_eq` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `merk/src/merk/restore.rs:1831` | `assert_raw_db_entries_eq` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `merk/src/merk/restore.rs:1833` | `assert_raw_db_entries_eq` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `merk/src/merk/restore.rs:1837` | `assert_raw_db_entries_eq` | `RawIterator::key` | 2 | iterator | NodeStore | I-key |
| `merk/src/merk/restore.rs:1838` | `assert_raw_db_entries_eq` | `RawIterator::value` | 2 | iterator | NodeStore | I-value |
| `merk/src/merk/restore.rs:1840` | `assert_raw_db_entries_eq` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |
| `merk/src/merk/restore.rs:1841` | `assert_raw_db_entries_eq` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |
| `merk/src/merk/restore.rs:1855` | `test_restoration_single_chunk_strategy` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:1856` | `test_restoration_single_chunk_strategy` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:1859` | `test_restoration_single_chunk_strategy` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:1874` | `test_restoration_single_chunk_strategy` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:1875` | `test_restoration_single_chunk_strategy` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:1878` | `test_restoration_single_chunk_strategy` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:1947` | `test_process_multi_chunk_no_limit` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:1948` | `test_process_multi_chunk_no_limit` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:1951` | `test_process_multi_chunk_no_limit` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2016` | `test_process_multi_chunk_no_limit_but_non_root` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2017` | `test_process_multi_chunk_no_limit_but_non_root` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2020` | `test_process_multi_chunk_no_limit_but_non_root` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2098` | `test_process_multi_chunk_with_limit` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2099` | `test_process_multi_chunk_with_limit` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2102` | `test_process_multi_chunk_with_limit` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2189` | `test_process_multi_chunk_rejects_dangling_chunk_id` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2190` | `test_process_multi_chunk_rejects_dangling_chunk_id` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2193` | `test_process_multi_chunk_rejects_dangling_chunk_id` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2223` | `test_process_multi_chunk_rejects_trailing_chunk_id_after_valid_chunk` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2224` | `test_process_multi_chunk_rejects_trailing_chunk_id_after_valid_chunk` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2227` | `test_process_multi_chunk_rejects_trailing_chunk_id_after_valid_chunk` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2273` | `test_restoration_multi_chunk_strategy` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2274` | `test_restoration_multi_chunk_strategy` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2277` | `test_restoration_multi_chunk_strategy` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2355` | `test_restoration_interruption` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2356` | `test_restoration_interruption` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2359` | `test_restoration_interruption` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2409` | `test_restoration_interruption` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2450` | `single_leaf_chunk_round_trip` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2451` | `single_leaf_chunk_round_trip` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2454` | `single_leaf_chunk_round_trip` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2472` | `single_leaf_chunk_round_trip` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2473` | `single_leaf_chunk_round_trip` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2476` | `single_leaf_chunk_round_trip` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2566` | `drive_multi_node_chunk_round_trip` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2567` | `drive_multi_node_chunk_round_trip` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2570` | `drive_multi_node_chunk_round_trip` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2584` | `drive_multi_node_chunk_round_trip` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2585` | `drive_multi_node_chunk_round_trip` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2588` | `drive_multi_node_chunk_round_trip` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2897` | `restore_rechecks_root_after_element_feature_rewrite` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:2898` | `restore_rechecks_root_after_element_feature_rewrite` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:2901` | `restore_rechecks_root_after_element_feature_rewrite` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:2933` | `open_restoration_merk` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `merk/src/merk/restore.rs:3005` | `restore_with_grovedb_elements_rejects_forged_value_bytes_under_honest_value_hash` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:3006` | `restore_with_grovedb_elements_rejects_forged_value_bytes_under_honest_value_hash` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:3027` | `restore_with_grovedb_elements_rejects_forged_value_bytes_under_honest_value_hash` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:3028` | `restore_with_grovedb_elements_rejects_forged_value_bytes_under_honest_value_hash` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:3091` | `restore_rejects_feature_type_family_foreign_to_the_tree_type` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:3092` | `restore_rejects_feature_type_family_foreign_to_the_tree_type` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/merk/restore.rs:3119` | `test_hash_only_non_root_chunk_returns_error_not_panic` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/merk/restore.rs:3120` | `test_hash_only_non_root_chunk_returns_error_not_panic` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/proofs/query/count_offset/tests.rs:108` | `trusted_rows_are_the_rows_a_proof_returns_at_every_offset_and_limit` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/proofs/query/count_offset/tests.rs:138` | `should_fetch_pinned_view_nodes_without_a_mutation_baseline` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm |
| `merk/src/proofs/query/count_offset/tests.rs:150` | `should_fetch_pinned_view_nodes_without_a_mutation_baseline` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/proofs/query/count_offset/tests.rs:168` | `should_read_only_the_value_stored_at_a_key_through_a_view` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm |
| `merk/src/proofs/query/count_offset/tests.rs:172` | `should_read_only_the_value_stored_at_a_key_through_a_view` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/proofs/query/count_offset/tests.rs:221` | `local_count_unit_pages_refuse_exposed_populated_trees` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/proofs/query/count_offset/tests.rs:410` | `should_charge_exactly_the_nodes_a_local_page_loads` | `StorageContext::get` | 1 | tree-node access | NodeStore | R-tx / R-imm |
| `merk/src/proofs/query/count_offset/tests.rs:428` | `should_charge_exactly_the_nodes_a_local_page_loads` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/proofs/query/count_offset/tests.rs:451` | `should_charge_exactly_the_nodes_a_local_page_loads` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `merk/src/tree/put_value/mod.rs:257` | `step` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `merk/src/tree/put_value/mod.rs:260` | `step` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `merk/src/tree/put_value/mod.rs:271` | `step` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `merk/src/tree/put_value/mod.rs:286` | `replace` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/tree/put_value/mod.rs:287` | `replace` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `merk/src/tree/put_value/mod.rs:470` | `unknown_version_is_rejected` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `merk/src/tree/put_value/mod.rs:471` | `unknown_version_is_rejected` | `Storage::start_transaction` | 1 | transaction | Engine | zero |

### grovedb-commitment-tree

| Site | Function | API item | Calls | Class | Seam | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| `grovedb-commitment-tree/benches/seeding.rs:44` | `main` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `grovedb-commitment-tree/benches/seeding.rs:45` | `main` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `grovedb-commitment-tree/benches/seeding.rs:46` | `main` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb-commitment-tree/benches/seeding.rs:49` | `main` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb-commitment-tree/benches/seeding.rs:108` | `main` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb-commitment-tree/benches/seeding.rs:112` | `main` | `Storage::commit_transaction` | 1 | transaction | Engine | zero |

### grovedb

| Site | Function | API item | Calls | Class | Seam | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| `grovedb/src/batch/just_in_time_cost_tests.rs:203` | `test_partial_costs_with_no_new_operations_are_same_as_apply_batch` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/just_in_time_cost_tests.rs:337` | `test_partial_costs_with_add_balance_operations` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/move_element.rs:1195` | `a_moved_record_is_billed_exactly` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/batch/move_element.rs:1197` | `a_moved_record_is_billed_exactly` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `grovedb/src/batch/move_element.rs:1198` | `a_moved_record_is_billed_exactly` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |
| `grovedb/src/batch/move_element.rs:1200` | `a_moved_record_is_billed_exactly` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `grovedb/src/batch/move_element.rs:1201` | `a_moved_record_is_billed_exactly` | `RawIterator::key` | 1 | iterator | NodeStore | I-key |
| `grovedb/src/batch/move_element.rs:1202` | `a_moved_record_is_billed_exactly` | `RawIterator::value` | 1 | iterator | NodeStore | I-value |
| `grovedb/src/batch/move_element.rs:1204` | `a_moved_record_is_billed_exactly` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |
| `grovedb/src/batch/move_element.rs:1219` | `a_moved_record_is_billed_exactly` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/batch/move_element.rs:1231` | `a_moved_record_is_billed_exactly` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/batch/move_element.rs:1234` | `a_moved_record_is_billed_exactly` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/batch/move_element.rs:1257` | `a_moved_record_is_billed_exactly` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/batch/multi_insert_cost_tests.rs:52` | `test_batch_two_insert_empty_tree_same_level_added_bytes_match_non_batch` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/multi_insert_cost_tests.rs:111` | `test_batch_three_inserts_elements_same_level_added_bytes_match_non_batch` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/multi_insert_cost_tests.rs:191` | `test_batch_four_inserts_elements_multi_level_added_bytes_match_non_batch` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_deletion_cost_tests.rs:216` | `insert_two_epoch_flagged_items_then_delete_in_batch` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_deletion_cost_tests.rs:346` | `test_batch_one_deletion_tree_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_deletion_cost_tests.rs:416` | `test_batch_one_deletion_item_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_deletion_cost_tests.rs:641` | `test_batch_one_deletion_tree_with_flags_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_deletion_cost_tests.rs:741` | `test_batch_one_deletion_tree_with_identity_cost_flags_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_deletion_cost_tests.rs:837` | `test_batch_one_deletion_item_with_flags_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_insert_cost_tests.rs:48` | `test_batch_one_insert_costs_match_non_batch` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_sum_item_deletion_cost_tests.rs:46` | `test_batch_one_deletion_sum_tree_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_sum_item_deletion_cost_tests.rs:109` | `test_batch_one_deletion_sum_item_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_sum_item_deletion_cost_tests.rs:154` | `test_batch_one_deletion_sum_tree_with_flags_costs_match_non_batch_on_transaction` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/batch/single_sum_item_insert_cost_tests.rs:44` | `test_batch_one_sum_item_insert_costs_match_non_batch` | `Tx::rollback` | 1 | transaction | Engine | zero |
| `grovedb/src/element/aggregate_sum_query/tests.rs:3145` | `test_cyclic_reference_detected_in_aggregate_sum_query` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/element/aggregate_sum_query/tests.rs:3209` | `test_self_referencing_element_detected_in_aggregate_sum_query` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/element/query.rs:1315` | `test_get_range_query` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/element/query.rs:1347` | `test_get_range_query` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/element/query.rs:1351` | `test_get_range_query` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/element/query.rs:1431` | `test_get_range_inclusive_query` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/element/query.rs:1464` | `test_get_range_inclusive_query` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/element/query.rs:1468` | `test_get_range_inclusive_query` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/estimated_costs/average_case_costs.rs:714` | `test_get_merk_node_average_case` | `RocksDbStorage::default_rocksdb_with_path` | 1 | lifecycle | Engine | zero |
| `grovedb/src/estimated_costs/average_case_costs.rs:716` | `test_get_merk_node_average_case` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/estimated_costs/average_case_costs.rs:717` | `test_get_merk_node_average_case` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `grovedb/src/estimated_costs/average_case_costs.rs:721` | `test_get_merk_node_average_case` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/estimated_costs/average_case_costs.rs:736` | `test_get_merk_node_average_case` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/estimated_costs/average_case_costs.rs:743` | `test_get_merk_node_average_case` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/estimated_costs/average_case_costs.rs:771` | `test_get_merk_node_average_case` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |
| `grovedb/src/estimated_costs/worst_case_costs.rs:402` | `test_get_merk_node_worst_case` | `TempStorage::new` | 1 | lifecycle | Engine | zero |
| `grovedb/src/estimated_costs/worst_case_costs.rs:403` | `test_get_merk_node_worst_case` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/estimated_costs/worst_case_costs.rs:404` | `test_get_merk_node_worst_case` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `grovedb/src/estimated_costs/worst_case_costs.rs:415` | `test_get_merk_node_worst_case` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/estimated_costs/worst_case_costs.rs:439` | `test_get_merk_node_worst_case` | `WorstKeyLength::max_length` | 1 | estimate | Estimates | EST |
| `grovedb/src/lib.rs:4683` | `GroveDb::raw_subtree_get` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/lib.rs:4686` | `GroveDb::raw_subtree_get` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx |
| `grovedb/src/merk_cache.rs:600` | `subtrees_are_propagated` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/merk_cache.rs:611` | `subtrees_are_propagated` | `StorageBatch::len` | 1 | batch | Engine | zero |
| `grovedb/src/merk_cache.rs:627` | `subtrees_are_propagated` | `StorageBatch::len` | 1 | batch | Engine | zero |
| `grovedb/src/operations/delete/mod.rs:1058` | `test_empty_subtree_deletion_with_transaction` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/operations/delete/mod.rs:1395` | `test_recurring_deletion_through_subtrees_with_transaction` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/operations/indexed_tree.rs:3774` | `GroveDb::legacy_linear_indexed_count_top_k_paginated` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `grovedb/src/operations/indexed_tree.rs:4214` | `open_axis_secondary` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/operations/proof/indexed_axis/generate.rs:531` | `GroveDb::prove_indexed_axis_query` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/operations/proof/indexed_axis/generate.rs:605` | `GroveDb::prove_indexed_axis_top_k_paginated` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/operations/proof/indexed_axis/generate.rs:805` | `GroveDb::prove_indexed_axis_rank_of_key` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/operations/proof/indexed_axis/generate.rs:880` | `GroveDb::prove_indexed_axis_aggregate_over_value_range` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/automatic_backward_references_tests.rs:1384` | `skipped_callback_delete_keeps_the_executed_deletions_cleanup` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/automatic_backward_references_tests.rs:1387` | `skipped_callback_delete_keeps_the_executed_deletions_cleanup` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx |
| `grovedb/src/tests/automatic_backward_references_tests.rs:1683` | `callback_delete_tree_cannot_erase_the_initial_segments_cleanup` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/automatic_backward_references_tests.rs:1686` | `callback_delete_tree_cannot_erase_the_initial_segments_cleanup` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx |
| `grovedb/src/tests/bidirectional_references_tests.rs:2333` | `verify_grovedb_reports_corrupt_provided_value_hashes` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/bidirectional_references_tests.rs:2356` | `verify_grovedb_reports_corrupt_provided_value_hashes` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/bidirectional_references_tests.rs:3393` | `verify_grovedb_reports_forged_referrer_entries` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/bidirectional_references_tests.rs:3416` | `verify_grovedb_reports_forged_referrer_entries` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/bidirectional_references_tests.rs:3842` | `verify_grovedb_reports_missing_forward_registration_and_empty_inverse` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/bidirectional_references_tests.rs:3865` | `verify_grovedb_reports_missing_forward_registration_and_empty_inverse` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/bidirectional_references_tests.rs:3898` | `verify_grovedb_reports_missing_forward_registration_and_empty_inverse` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/bidirectional_references_tests.rs:3921` | `verify_grovedb_reports_missing_forward_registration_and_empty_inverse` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/bidirectional_references_tests.rs:4079` | `cascade_removes_the_physical_referrer_record` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `grovedb/src/tests/bidirectional_references_tests.rs:4081` | `cascade_removes_the_physical_referrer_record` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx |
| `grovedb/src/tests/commitment_tree_tests.rs:2633` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/commitment_tree_tests.rs:2638` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/commitment_tree_tests.rs:2658` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/commitment_tree_tests.rs:2667` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `StorageContext::raw_iter` | 1 | iterator | FlatStore | zero |
| `grovedb/src/tests/commitment_tree_tests.rs:2668` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `RawIterator::seek_to_first` | 1 | iterator | FlatStore | I-move |
| `grovedb/src/tests/commitment_tree_tests.rs:2670` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `RawIterator::valid` | 1 | iterator | FlatStore | I-key |
| `grovedb/src/tests/commitment_tree_tests.rs:2672` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `RawIterator::key` | 1 | iterator | FlatStore | I-key |
| `grovedb/src/tests/commitment_tree_tests.rs:2677` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `RawIterator::value` | 1 | iterator | FlatStore | I-value |
| `grovedb/src/tests/commitment_tree_tests.rs:2686` | `unsafe_dump_load_subtree_roundtrip_preserves_root_hash` | `RawIterator::next` | 1 | iterator | FlatStore | I-move |
| `grovedb/src/tests/count_sum_tree_tests.rs:112` | `test_count_sum_tree_behaves_like_regular_tree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:174` | `test_count_sum_tree_item_behaves_like_regular_item` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:258` | `test_homogenous_node_type_in_count_sum_trees_and_regular_trees` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:356` | `test_count_sum_tree_feature` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:420` | `test_count_sum_tree_feature` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:467` | `test_count_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:539` | `test_count_sum_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_sum_tree_tests.rs:560` | `test_count_sum_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_tree_tests.rs:379` | `test_homogenous_node_type_in_count_trees_and_regular_trees` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_tree_tests.rs:525` | `test_count_tree_feature` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_tree_tests.rs:799` | `test_count_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_tree_tests.rs:933` | `test_count_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_tree_tests.rs:981` | `test_count_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/count_tree_tests.rs:1071` | `test_count_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:47` | `axis_secondary_prefix` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:48` | `axis_secondary_prefix` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:57` | `raw_put_in_subtree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:61` | `raw_put_in_subtree` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:63` | `raw_put_in_subtree` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:68` | `raw_put_in_subtree` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:71` | `raw_put_in_subtree` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:91` | `insert_into_axis_secondary` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:128` | `insert_into_axis_secondary` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:131` | `insert_into_axis_secondary` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:143` | `raw_put_in_axis_secondary` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:147` | `raw_put_in_axis_secondary` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:149` | `raw_put_in_axis_secondary` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:154` | `raw_put_in_axis_secondary` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:157` | `raw_put_in_axis_secondary` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:171` | `merk_insert_element` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:188` | `merk_insert_element` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:191` | `merk_insert_element` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:210` | `corrupt_pcpsit_axes` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:249` | `corrupt_pcpsit_axes` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:252` | `corrupt_pcpsit_axes` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:270` | `propagate_from` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:294` | `propagate_from` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:297` | `propagate_from` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:521` | `propagate_at_root_with_unconsumed_deferred_secondary_is_rejected` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:575` | `propagate_at_root_with_unconsumed_deferred_axes_is_rejected` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_lib_paths_tests.rs:816` | `update_count_indexed_item_into_batch_operations_rejects_non_indexed_element` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_round7_tests.rs:1450` | `verify_grovedb_detects_pcit_secondary_missing_entry` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_round7_tests.rs:1496` | `verify_grovedb_detects_pcit_secondary_missing_entry` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_round7_tests.rs:1499` | `verify_grovedb_detects_pcit_secondary_missing_entry` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/coverage_round7_tests.rs:1588` | `visualize_verify_grovedb_returns_paths_on_corruption` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/coverage_round7_tests.rs:1634` | `visualize_verify_grovedb_returns_paths_on_corruption` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/coverage_round7_tests.rs:1637` | `visualize_verify_grovedb_returns_paths_on_corruption` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:43` | `seed_stale_secondary_row` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:45` | `seed_stale_secondary_row` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:48` | `seed_stale_secondary_row` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:52` | `seed_stale_secondary_row` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:58` | `seed_stale_secondary_row` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:63` | `seed_stale_secondary_row` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:66` | `seed_stale_secondary_row` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:76` | `secondary_namespace_non_empty` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:78` | `secondary_namespace_non_empty` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:79` | `secondary_namespace_non_empty` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |
| `grovedb/src/tests/delete_indexed_tree_tests.rs:80` | `secondary_namespace_non_empty` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `grovedb/src/tests/flat_drop_tests.rs:47` | `data_namespace_key_count` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/flat_drop_tests.rs:50` | `data_namespace_key_count` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/flat_drop_tests.rs:52` | `data_namespace_key_count` | `StorageContext::raw_iter` | 1 | iterator | FlatStore | zero |
| `grovedb/src/tests/flat_drop_tests.rs:53` | `data_namespace_key_count` | `RawIterator::seek_to_first` | 1 | iterator | FlatStore | I-move |
| `grovedb/src/tests/flat_drop_tests.rs:55` | `data_namespace_key_count` | `RawIterator::valid` | 1 | iterator | FlatStore | I-key |
| `grovedb/src/tests/flat_drop_tests.rs:57` | `data_namespace_key_count` | `RawIterator::next` | 1 | iterator | FlatStore | I-move |
| `grovedb/src/tests/flat_drop_tests.rs:65` | `record_exists` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/flat_drop_tests.rs:68` | `record_exists` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/flat_drop_tests.rs:69` | `record_exists` | `pending_prefix_drops_namespace` | 1 | prefix | FlatStore | zero |
| `grovedb/src/tests/flat_drop_tests.rs:74` | `record_exists` | `StorageContext::get_meta` | 1 | meta | FlatStore | R-tx |
| `grovedb/src/tests/flat_drop_tests.rs:81` | `prefix_of` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/flat_drop_tests.rs:161` | `put_raw` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `grovedb/src/tests/flat_drop_tests.rs:164` | `put_raw` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx |
| `grovedb/src/tests/flat_drop_tests.rs:177` | `get_raw_bytes` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `grovedb/src/tests/flat_drop_tests.rs:180` | `get_raw_bytes` | `StorageContext::get` | 1 | flat key-value | FlatStore | R-tx |
| `grovedb/src/tests/flat_drop_tests.rs:436` | `flush_resumes_after_simulated_crash_between_tombstones_and_record_removal` | `RocksDbStorage::delete_prefix_ranges` | 1 | range delete | Engine | DB |
| `grovedb/src/tests/flat_drop_tests.rs:1030` | `drop_flat_indexed_tree_reclaims_all_axis_secondary_namespaces` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/flat_drop_tests.rs:1246` | `batch_drop_flat_of_indexed_tree_reclaims_secondaries` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/indexed_reference_row_tests.rs:70` | `overwrite_row` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_reference_row_tests.rs:134` | `overwrite_row` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/indexed_reference_row_tests.rs:137` | `overwrite_row` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/indexed_reference_row_tests.rs:201` | `a_healthy_indexed_tree_stores_canonical_reference_rows` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_reference_row_tests.rs:1160` | `replace_subtree_root_moves_the_row_when_aggregates_change` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:92` | `read_secondary_root_key` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:130` | `drift_secondary` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:221` | `drift_secondary` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:224` | `drift_secondary` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:713` | `reconcile_refuses_a_primary_key_over_the_cidx_ceiling` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:727` | `reconcile_refuses_a_primary_key_over_the_cidx_ceiling` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:730` | `reconcile_refuses_a_primary_key_over_the_cidx_ceiling` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:1082` | `drift_axis_secondary` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:1213` | `drift_axis_secondary` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:1216` | `drift_axis_secondary` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:1233` | `rebind_count_secondary_root_key_to` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:1300` | `rebind_count_secondary_root_key_to` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs:1303` | `rebind_count_secondary_root_key_to` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/mod.rs:3919` | `test_subtree_pairs_iterator` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/mod.rs:3925` | `test_subtree_pairs_iterator` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `grovedb/src/tests/mod.rs:4016` | `test_root_subtree_has_root_key` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/mod.rs:4122` | `test_get_subtree` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/mod.rs:4176` | `test_get_subtree` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/mod.rs:4200` | `test_get_subtree` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/move_element_tests.rs:104` | `records_in` | `Storage::start_transaction` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/move_element_tests.rs:107` | `records_in` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/move_element_tests.rs:109` | `records_in` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `grovedb/src/tests/move_element_tests.rs:110` | `records_in` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |
| `grovedb/src/tests/move_element_tests.rs:112` | `records_in` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `grovedb/src/tests/move_element_tests.rs:114` | `records_in` | `RawIterator::next` | 1 | iterator | NodeStore | I-move |
| `grovedb/src/tests/move_element_tests.rs:120` | `prefix_of` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/move_element_tests.rs:125` | `axis_prefix_of` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs:160` | `secondary_prefix_for_path` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs:161` | `secondary_prefix_for_path` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs:169` | `namespace_non_empty` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs:171` | `namespace_non_empty` | `StorageContext::raw_iter` | 1 | iterator | NodeStore | zero |
| `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs:172` | `namespace_non_empty` | `RawIterator::seek_to_first` | 1 | iterator | NodeStore | I-move |
| `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs:173` | `namespace_non_empty` | `RawIterator::valid` | 1 | iterator | NodeStore | I-key |
| `grovedb/src/tests/non_counted_tests.rs:192` | `batch_propagation_preserves_non_counted_wrapper_on_subtree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/non_merk_integrity_audit_tests.rs:25` | `tamper` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `grovedb/src/tests/non_merk_integrity_audit_tests.rs:27` | `tamper` | `StorageContext::put` | 1 | flat key-value | FlatStore | W-tx |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:91` | `namespace_non_empty` | `Storage::get_transactional_storage_context_by_subtree_prefix` | 1 | by-prefix open | Engine | zero |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:93` | `namespace_non_empty` | `StorageContext::raw_iter` | 1 | iterator | FlatStore | zero |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:94` | `namespace_non_empty` | `RawIterator::seek_to_first` | 1 | iterator | FlatStore | I-move |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:95` | `namespace_non_empty` | `RawIterator::valid` | 1 | iterator | FlatStore | I-key |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:99` | `prefix` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:227` | `assert_discovery_and_cleanup` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/non_merk_subtree_discovery_tests.rs:236` | `assert_discovery_and_cleanup` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/not_counted_or_summed_tests.rs:204` | `direct_insert_not_counted_or_summed_in_count_sum_tree_excludes_both_axes` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/not_counted_or_summed_tests.rs:330` | `direct_insert_not_counted_or_summed_provable_sum_tree_in_count_sum_tree_excludes_both_axes` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/not_counted_or_summed_tests.rs:454` | `batch_propagation_preserves_not_counted_or_summed_wrapper_on_subtree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/not_summed_tests.rs:144` | `direct_insert_not_summed_in_sum_tree_excludes_subtree_sum` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/not_summed_tests.rs:232` | `batch_propagation_preserves_not_summed_wrapper_on_subtree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/private_document_store_tests.rs:567` | `test_private_document_store_delete` | `Storage::get_transactional_storage_context` | 1 | path open | Engine | P-path |
| `grovedb/src/tests/private_document_store_tests.rs:573` | `test_private_document_store_delete` | `StorageContext::raw_iter` | 1 | iterator | FlatStore | zero |
| `grovedb/src/tests/private_document_store_tests.rs:574` | `test_private_document_store_delete` | `RawIterator::seek_to_first` | 1 | iterator | FlatStore | I-move |
| `grovedb/src/tests/private_document_store_tests.rs:576` | `test_private_document_store_delete` | `RawIterator::valid` | 1 | iterator | FlatStore | I-key |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:355` | `test_provable_count_sum_tree_behaves_like_regular_tree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:417` | `test_provable_count_sum_tree_feature_types` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:554` | `test_provable_count_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:627` | `test_provable_count_sum_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:648` | `test_provable_count_sum_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:791` | `test_provable_count_sum_tree_vs_count_sum_tree_aggregate_difference` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:811` | `test_provable_count_sum_tree_vs_count_sum_tree_aggregate_difference` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:876` | `test_provable_count_sum_tree_empty_tree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:984` | `test_provable_count_sum_tree_with_negative_sums` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1033` | `test_provable_count_sum_tree_single_item` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1145` | `test_provable_count_sum_tree_deeply_nested` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1249` | `test_provable_count_sum_tree_mixed_tree_types_nested` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1346` | `test_provable_count_sum_tree_sibling_trees` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1435` | `test_provable_count_sum_tree_with_subtree_inside` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1455` | `test_provable_count_sum_tree_with_subtree_inside` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1514` | `test_provable_count_sum_tree_delete_and_reinsert` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1544` | `test_provable_count_sum_tree_delete_and_reinsert` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1575` | `test_provable_count_sum_tree_delete_and_reinsert` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1642` | `test_provable_count_sum_tree_avl_rotations_for_version_2` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:1806` | `test_provable_count_sum_tree_many_items_rotation_stress_for_version_2` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:2485` | `test_provable_count_sum_tree_avl_rotations` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_count_sum_tree_tests.rs:2635` | `test_provable_count_sum_tree_many_items_rotation_stress` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/provable_sum_tree_tests.rs:818` | `tamper_value_no_hash_update` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `grovedb/src/tests/provable_sum_tree_tests.rs:822` | `tamper_value_no_hash_update` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx |
| `grovedb/src/tests/provable_sum_tree_tests.rs:849` | `tamper_value_no_hash_update` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx |
| `grovedb/src/tests/provable_sum_tree_tests.rs:903` | `tamper_parent_element_with_consistent_hashes` | `Storage::get_immediate_storage_context` | 1 | immediate context | Engine | P-path |
| `grovedb/src/tests/provable_sum_tree_tests.rs:907` | `tamper_parent_element_with_consistent_hashes` | `StorageContext::get` | 1 | escape hatch | NodeStore | R-tx |
| `grovedb/src/tests/provable_sum_tree_tests.rs:944` | `tamper_parent_element_with_consistent_hashes` | `StorageContext::put` | 1 | escape hatch | NodeStore | W-tx |
| `grovedb/src/tests/replication_incremental_commit_tests.rs:442` | `secondary_starving_indexed_source` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_incremental_commit_tests.rs:444` | `secondary_starving_indexed_source` | `RocksDbStorage::secondary_prefix_for` | 1 | prefix | Engine | P-sec |
| `grovedb/src/tests/replication_incremental_commit_tests.rs:456` | `secondary_starving_indexed_source` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_served_node_type_tests.rs:153` | `request_for` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_serving_tree_type_tests.rs:310` | `served_targets` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_serving_tree_type_tests.rs:449` | `fetch_chunk_refuses_provable_and_indexed_types_for_a_plain_subtree` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:1551` | `state_sync_empty_payload_for_a_populated_subtree_is_rejected` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:1900` | `fetch_chunk_rejects_append_only_request_without_page_cursor` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:1963` | `fetch_chunk_rejects_non_canonical_mmr_cursor` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:4021` | `state_sync_normal_tree_value_hash_forgery_rejected` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:4308` | `assert_state_sync_forged_element_rejected` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:4473` | `fetch_chunk_bounds_the_number_of_chunk_ids_per_request` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:4502` | `fetch_chunk_bounds_the_number_of_chunk_ids_per_request` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:4571` | `fetch_chunk_rejects_chunk_id_longer_than_subtree_depth` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/replication_session_tests.rs:4715` | `fetch_chunk_rejects_malformed_indexed_header_requests` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |
| `grovedb/src/tests/snapshot_read_transaction_tests.rs:112` | `snapshot_lifetime_is_observable` | `Tx::is_snapshot_read` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/snapshot_read_transaction_tests.rs:113` | `snapshot_lifetime_is_observable` | `Tx::snapshot_age` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/snapshot_read_transaction_tests.rs:116` | `snapshot_lifetime_is_observable` | `Tx::is_snapshot_read` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/snapshot_read_transaction_tests.rs:118` | `snapshot_lifetime_is_observable` | `Tx::snapshot_age` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/snapshot_read_transaction_tests.rs:121` | `snapshot_lifetime_is_observable` | `Tx::snapshot_age` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:372` | `test_homogenous_node_type_in_sum_trees_and_regular_trees` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:523` | `test_sum_tree_feature` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:734` | `test_sum_tree_overflow` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1064` | `test_sum_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1305` | `test_big_sum_tree_propagation` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1479` | `test_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1526` | `test_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1618` | `test_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1660` | `test_big_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1707` | `test_big_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1799` | `test_big_sum_tree_with_batches` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:1967` | `test_count_sum_tree_sum_propagates_to_sum_tree_parent` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/sum_tree_tests.rs:2103` | `test_count_sum_tree_sum_propagates_to_big_sum_tree_parent` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/tree_hashes_tests.rs:58` | `test_node_hashes_when_inserting_item` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/tree_hashes_tests.rs:133` | `test_tree_hashes_when_inserting_empty_tree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/tree_hashes_tests.rs:232` | `test_tree_hashes_when_inserting_empty_trees_twice_under_each_other` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:82` | `plant_node_aggregate` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:103` | `plant_node_aggregate` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:106` | `plant_node_aggregate` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:148` | `plant_node_aggregate_without_propagating` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:164` | `plant_node_aggregate_without_propagating` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:167` | `plant_node_aggregate_without_propagating` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:206` | `plant_secondary_row_aggregate` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:253` | `plant_secondary_row_aggregate` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:256` | `plant_secondary_row_aggregate` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:270` | `rebind_count_indexed_tree` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:328` | `rebind_count_indexed_tree` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs:331` | `rebind_count_indexed_tree` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:60` | `corrupt_pcit_secondary_delete` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:104` | `corrupt_pcit_secondary_delete` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:107` | `corrupt_pcit_secondary_delete` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:118` | `corrupt_pcit_secondary_insert_orphan` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:157` | `corrupt_pcit_secondary_insert_orphan` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:160` | `corrupt_pcit_secondary_insert_orphan` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1208` | `corrupt_psit_secondary_delete` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1249` | `corrupt_psit_secondary_delete` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1252` | `corrupt_psit_secondary_delete` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1273` | `delete_psit_secondary_row_and_get_root_key` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1305` | `delete_psit_secondary_row_and_get_root_key` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1308` | `delete_psit_secondary_row_and_get_root_key` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1466` | `corrupt_pcpsit_axis_secondary_delete` | `StorageBatch::new` | 1 | batch | Engine | zero |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1513` | `corrupt_pcpsit_axis_secondary_delete` | `Storage::commit_multi_context_batch` | 1 | batch | Engine | B-commit |
| `grovedb/src/tests/verify_grovedb_indexed_tests.rs:1516` | `corrupt_pcpsit_axis_secondary_delete` | `Tx::commit` | 1 | transaction | Engine | zero |
| `grovedb/tests/state_sync_serving_tree_type.rs:86` | `plain_subtree_source` | `RocksDbStorage::build_prefix` | 1 | prefix | Engine | P-path |

## Appendix A: type, trait and import references

Each line counts the places that name a `grovedb-storage` type, trait, associated type, variant or field, or import a function, outside `storage/`. These places call nothing. They are the type-level coupling SEAM-04 and SEAM-07 remove. The `## types` section of the search output lists each one by line.

| Item | Uses | Files | Where (file: uses) |
| --- | --- | --- | --- |
| `StorageBatch` | 290 | 61 | `grovedb-commitment-tree/benches/seeding.rs`: 2, `grovedb/src/batch/backward_references.rs`: 4, `grovedb/src/batch/mod.rs`: 7, `grovedb/src/batch/move_element.rs`: 6, `grovedb/src/element/query.rs`: 3, `grovedb/src/estimated_costs/average_case_costs.rs`: 2, `grovedb/src/estimated_costs/worst_case_costs.rs`: 2, `grovedb/src/lib.rs`: 10, `grovedb/src/merk_cache.rs`: 10, `grovedb/src/operations/auxiliary.rs`: 2, `grovedb/src/operations/bulk_append_tree.rs`: 4, `grovedb/src/operations/commitment_tree.rs`: 4, `grovedb/src/operations/delete/clear_subtree/v0.rs`: 2, `grovedb/src/operations/delete/clear_subtree/v1.rs`: 2, `grovedb/src/operations/delete/delete_internal_on_transaction/mod.rs`: 2, `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs`: 2, `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs`: 3, `grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs`: 3, `grovedb/src/operations/delete/flat_drop.rs`: 2, `grovedb/src/operations/delete/mod.rs`: 5, `grovedb/src/operations/dense_tree.rs`: 4, `grovedb/src/operations/indexed_tree.rs`: 13, `grovedb/src/operations/insert/add_element_on_transaction/mod.rs`: 3, `grovedb/src/operations/insert/add_element_on_transaction/v0.rs`: 2, `grovedb/src/operations/insert/add_element_on_transaction/v1.rs`: 2, `grovedb/src/operations/insert/add_element_on_transaction/v2.rs`: 3, `grovedb/src/operations/insert/insert_on_transaction/mod.rs`: 2, `grovedb/src/operations/insert/insert_on_transaction/v0.rs`: 3, `grovedb/src/operations/insert/insert_on_transaction/v1.rs`: 2, `grovedb/src/operations/insert/mod.rs`: 2, `grovedb/src/operations/mmr_tree.rs`: 4, `grovedb/src/operations/private_document_store.rs`: 4, `grovedb/src/operations/proof/generate.rs`: 1, `grovedb/src/operations/proof/indexed_axis/generate.rs`: 17, `grovedb/src/operations/proof/indexed_axis/target_chain.rs`: 4, `grovedb/src/operations/replace_subtree_root.rs`: 2, `grovedb/src/tests/bidirectional_references_tests.rs`: 7, `grovedb/src/tests/count_sum_tree_tests.rs`: 9, `grovedb/src/tests/count_tree_tests.rs`: 7, `grovedb/src/tests/coverage_lib_paths_tests.rs`: 10, `grovedb/src/tests/coverage_round7_tests.rs`: 4, `grovedb/src/tests/delete_indexed_tree_tests.rs`: 2, `grovedb/src/tests/indexed_reference_row_tests.rs`: 4, `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs`: 6, `grovedb/src/tests/non_counted_tests.rs`: 2, `grovedb/src/tests/not_counted_or_summed_tests.rs`: 4, `grovedb/src/tests/not_summed_tests.rs`: 4, `grovedb/src/tests/provable_count_sum_tree_tests.rs`: 23, `grovedb/src/tests/sum_tree_tests.rs`: 14, `grovedb/src/tests/tree_hashes_tests.rs`: 4, `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs`: 5, `grovedb/src/tests/verify_grovedb_indexed_tests.rs`: 6, `grovedb/src/util/compat.rs`: 4, `grovedb/src/util/visitor.rs`: 3, `merk/src/element/get.rs`: 3, `merk/src/element/insert.rs`: 3, `merk/src/merk/mod.rs`: 7, `merk/src/merk/open.rs`: 3, `merk/src/test_utils/mod.rs`: 2, `merk/src/test_utils/temp_merk.rs`: 7, `merk/src/tree/put_value/mod.rs`: 2 |
| `RocksDbStorage` | 235 | 48 | `grovedb-commitment-tree/benches/seeding.rs`: 2, `grovedb/src/batch/estimated_costs/average_case_costs.rs`: 3, `grovedb/src/batch/estimated_costs/worst_case_costs.rs`: 3, `grovedb/src/batch/mod.rs`: 4, `grovedb/src/batch/move_element.rs`: 8, `grovedb/src/checkpoints.rs`: 2, `grovedb/src/element/aggregate_sum_query/mod.rs`: 8, `grovedb/src/element/local_offset_read.rs`: 5, `grovedb/src/element/path_query_push_args.rs`: 2, `grovedb/src/element/query.rs`: 17, `grovedb/src/estimated_costs/average_case_costs.rs`: 5, `grovedb/src/estimated_costs/worst_case_costs.rs`: 2, `grovedb/src/lib.rs`: 5, `grovedb/src/operations/auxiliary.rs`: 3, `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs`: 3, `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs`: 3, `grovedb/src/operations/delete/flat_drop.rs`: 3, `grovedb/src/operations/get/average_case.rs`: 6, `grovedb/src/operations/get/worst_case.rs`: 4, `grovedb/src/operations/indexed_tree.rs`: 6, `grovedb/src/replication/indexed_sync.rs`: 2, `grovedb/src/replication/state_sync_session.rs`: 3, `grovedb/src/replication/verify.rs`: 3, `grovedb/src/tests/automatic_backward_references_tests.rs`: 3, `grovedb/src/tests/commitment_tree_tests.rs`: 2, `grovedb/src/tests/coverage_lib_paths_tests.rs`: 3, `grovedb/src/tests/delete_cost_estimation_tests.rs`: 11, `grovedb/src/tests/delete_indexed_tree_tests.rs`: 3, `grovedb/src/tests/estimated_costs_average_case_tests.rs`: 8, `grovedb/src/tests/estimated_costs_worst_case_tests.rs`: 8, `grovedb/src/tests/flat_drop_tests.rs`: 4, `grovedb/src/tests/misc_coverage_tests.rs`: 26, `grovedb/src/tests/move_element_tests.rs`: 3, `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs`: 3, `grovedb/src/tests/non_merk_subtree_discovery_tests.rs`: 4, `grovedb/src/tests/replication_incremental_commit_tests.rs`: 4, `grovedb/src/tests/replication_served_node_type_tests.rs`: 2, `grovedb/src/tests/replication_serving_tree_type_tests.rs`: 3, `grovedb/src/tests/replication_session_tests.rs`: 15, `grovedb/src/util.rs`: 2, `grovedb/src/util/compat.rs`: 4, `grovedb/src/util/visitor.rs`: 3, `grovedb/tests/state_sync_serving_tree_type.rs`: 2, `merk/src/merk/mod.rs`: 8, `merk/src/merk/open.rs`: 2, `merk/src/merk/restore.rs`: 1, `merk/src/test_utils/temp_merk.rs`: 6, `merk/src/tree/put_value/mod.rs`: 3 |
| `StorageContext` | 220 | 82 | `grovedb-bulk-append-tree/src/proof/mod.rs`: 4, `grovedb-bulk-append-tree/src/test_utils.rs`: 2, `grovedb-bulk-append-tree/src/tree/append.rs`: 2, `grovedb-bulk-append-tree/src/tree/fetch.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/mod.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 3, `grovedb-dense-fixed-sized-merkle-tree/src/proof/mod.rs`: 3, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 3, `grovedb-dense-fixed-sized-merkle-tree/src/tree/mod.rs`: 2, `grovedb-dense-fixed-sized-merkle-tree/src/tree/root_maintenance/mod.rs`: 2, `grovedb-dense-fixed-sized-merkle-tree/src/tree/root_maintenance/v0.rs`: 4, `grovedb-dense-fixed-sized-merkle-tree/src/tree/root_maintenance/v1.rs`: 6, `grovedb-merkle-mountain-range/src/storage_adapter.rs`: 3, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 3, `grovedb-private-document-store/src/store.rs`: 2, `grovedb/src/batch/conditional_insert_target/mod.rs`: 2, `grovedb/src/batch/conditional_insert_target/v0.rs`: 2, `grovedb/src/batch/conditional_insert_target/v1.rs`: 2, `grovedb/src/batch/indexed_tree/mirror.rs`: 3, `grovedb/src/batch/indexed_tree/mod.rs`: 4, `grovedb/src/batch/indexed_tree/pre_state.rs`: 2, `grovedb/src/batch/just_in_time_reference_update/mod.rs`: 2, `grovedb/src/batch/mod.rs`: 5, `grovedb/src/batch/move_element.rs`: 3, `grovedb/src/bidirectional_references/mod.rs`: 1, `grovedb/src/element/aggregate_sum_query/mod.rs`: 1, `grovedb/src/element/local_offset_read.rs`: 1, `grovedb/src/element/query.rs`: 1, `grovedb/src/lib.rs`: 13, `grovedb/src/operations/auxiliary.rs`: 1, `grovedb/src/operations/auxiliary/find_subtrees/v0.rs`: 1, `grovedb/src/operations/auxiliary/find_subtrees/v1.rs`: 1, `grovedb/src/operations/delete/clear_subtree/v0.rs`: 1, `grovedb/src/operations/delete/clear_subtree/v1.rs`: 1, `grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs`: 1, `grovedb/src/operations/delete/flat_drop.rs`: 1, `grovedb/src/operations/dense_tree.rs`: 1, `grovedb/src/operations/get/mod.rs`: 1, `grovedb/src/operations/indexed_tree.rs`: 3, `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs`: 1, `grovedb/src/operations/proof/generate.rs`: 4, `grovedb/src/operations/proof/indexed_axis/generate.rs`: 2, `grovedb/src/replication/non_merk_sync.rs`: 1, `grovedb/src/replication/state_sync_session.rs`: 1, `grovedb/src/replication/verify.rs`: 2, `grovedb/src/tests/automatic_backward_references_tests.rs`: 2, `grovedb/src/tests/bidirectional_references_tests.rs`: 1, `grovedb/src/tests/commitment_tree_tests.rs`: 1, `grovedb/src/tests/coverage_lib_paths_tests.rs`: 1, `grovedb/src/tests/delete_indexed_tree_tests.rs`: 1, `grovedb/src/tests/flat_drop_tests.rs`: 1, `grovedb/src/tests/move_element_tests.rs`: 1, `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs`: 1, `grovedb/src/tests/non_merk_integrity_audit_tests.rs`: 1, `grovedb/src/tests/non_merk_subtree_discovery_tests.rs`: 1, `grovedb/src/tests/private_document_store_tests.rs`: 1, `grovedb/src/tests/provable_sum_tree_tests.rs`: 1, `grovedb/src/util/visitor.rs`: 1, `grovedb/src/visualize.rs`: 2, `merk/src/debugger.rs`: 2, `merk/src/element/delete.rs`: 5, `merk/src/element/exists.rs`: 3, `merk/src/element/get.rs`: 21, `merk/src/element/insert.rs`: 21, `merk/src/merk/apply.rs`: 2, `merk/src/merk/chunks.rs`: 4, `merk/src/merk/clear.rs`: 2, `merk/src/merk/get.rs`: 2, `merk/src/merk/meta.rs`: 2, `merk/src/merk/mod.rs`: 4, `merk/src/merk/open.rs`: 2, `merk/src/merk/prove.rs`: 2, `merk/src/merk/prove_count_offset.rs`: 2, `merk/src/merk/restore.rs`: 4, `merk/src/merk/source.rs`: 3, `merk/src/proofs/query/aggregate_count/tests.rs`: 2, `merk/src/proofs/query/aggregate_count_and_sum/tests.rs`: 1, `merk/src/proofs/query/aggregate_sum/tests.rs`: 2, `merk/src/proofs/query/count_offset/tests.rs`: 4, `merk/src/tree/encoding.rs`: 2, `merk/src/tree/put_value/mod.rs`: 2, `merk/src/visualize.rs`: 2 |
| `Error` | 96 | 9 | `grovedb-bulk-append-tree/src/test_utils.rs`: 13, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 26, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 26, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 26, `grovedb/src/batch/move_element.rs`: 1, `grovedb/src/error.rs`: 1, `grovedb/src/tests/snapshot_fetch_node_tests.rs`: 1, `merk/src/error.rs`: 1, `merk/src/proofs/query/count_offset/tests.rs`: 1 |
| `PrefixedRocksDbTransactionContext` | 96 | 28 | `grovedb/src/batch/backward_references.rs`: 2, `grovedb/src/batch/mod.rs`: 3, `grovedb/src/batch/move_element.rs`: 4, `grovedb/src/element/local_offset_read.rs`: 2, `grovedb/src/lib.rs`: 11, `grovedb/src/merk_cache.rs`: 2, `grovedb/src/operations/delete/clear_subtree/v0.rs`: 2, `grovedb/src/operations/delete/clear_subtree/v1.rs`: 3, `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs`: 4, `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs`: 5, `grovedb/src/operations/delete/delete_internal_on_transaction/v2.rs`: 2, `grovedb/src/operations/delete/flat_drop.rs`: 2, `grovedb/src/operations/dense_tree.rs`: 2, `grovedb/src/operations/get/aggregate_per_key/carrier.rs`: 2, `grovedb/src/operations/indexed_tree.rs`: 5, `grovedb/src/operations/insert/add_element_on_transaction/mod.rs`: 3, `grovedb/src/operations/insert/add_element_on_transaction/v0.rs`: 2, `grovedb/src/operations/insert/add_element_on_transaction/v1.rs`: 2, `grovedb/src/operations/insert/add_element_on_transaction/v2.rs`: 3, `grovedb/src/operations/insert/insert_on_transaction/v0.rs`: 2, `grovedb/src/operations/mmr_tree.rs`: 2, `grovedb/src/replication.rs`: 3, `grovedb/src/tests/coverage_lib_paths_tests.rs`: 4, `grovedb/src/util/compat.rs`: 4, `grovedb/src/util/visitor.rs`: 2, `merk/src/merk/mod.rs`: 3, `merk/src/merk/restore.rs`: 10, `merk/src/test_utils/temp_merk.rs`: 5 |
| `Storage` | 94 | 68 | `grovedb-commitment-tree/benches/seeding.rs`: 1, `grovedb/src/batch/mod.rs`: 1, `grovedb/src/batch/move_element.rs`: 2, `grovedb/src/bidirectional_references/mod.rs`: 1, `grovedb/src/checkpoints.rs`: 1, `grovedb/src/element/aggregate_sum_query/mod.rs`: 1, `grovedb/src/element/aggregate_sum_query/tests.rs`: 1, `grovedb/src/element/local_offset_read.rs`: 1, `grovedb/src/element/query.rs`: 2, `grovedb/src/estimated_costs/average_case_costs.rs`: 8, `grovedb/src/estimated_costs/worst_case_costs.rs`: 7, `grovedb/src/lib.rs`: 1, `grovedb/src/merk_cache.rs`: 1, `grovedb/src/operations/auxiliary.rs`: 1, `grovedb/src/operations/auxiliary/find_subtrees/v0.rs`: 1, `grovedb/src/operations/auxiliary/find_subtrees/v1.rs`: 1, `grovedb/src/operations/bulk_append_tree.rs`: 1, `grovedb/src/operations/commitment_tree.rs`: 1, `grovedb/src/operations/delete/average_case.rs`: 3, `grovedb/src/operations/delete/clear_subtree/v0.rs`: 1, `grovedb/src/operations/delete/clear_subtree/v1.rs`: 1, `grovedb/src/operations/delete/delete_internal_on_transaction/v0.rs`: 1, `grovedb/src/operations/delete/delete_internal_on_transaction/v1.rs`: 1, `grovedb/src/operations/delete/flat_drop.rs`: 1, `grovedb/src/operations/delete/mod.rs`: 1, `grovedb/src/operations/delete/worst_case.rs`: 3, `grovedb/src/operations/dense_tree.rs`: 1, `grovedb/src/operations/get/mod.rs`: 1, `grovedb/src/operations/indexed_tree.rs`: 1, `grovedb/src/operations/insert/mod.rs`: 1, `grovedb/src/operations/mmr_tree.rs`: 1, `grovedb/src/operations/private_document_store.rs`: 1, `grovedb/src/operations/proof/bind_terminal_non_merk_tree/v1.rs`: 1, `grovedb/src/operations/proof/generate.rs`: 1, `grovedb/src/operations/replace_subtree_root.rs`: 1, `grovedb/src/replication/non_merk_sync.rs`: 1, `grovedb/src/replication/state_sync_session.rs`: 1, `grovedb/src/replication/verify.rs`: 1, `grovedb/src/tests/automatic_backward_references_tests.rs`: 2, `grovedb/src/tests/bidirectional_references_tests.rs`: 4, `grovedb/src/tests/commitment_tree_tests.rs`: 1, `grovedb/src/tests/coverage_lib_paths_tests.rs`: 1, `grovedb/src/tests/coverage_round7_tests.rs`: 2, `grovedb/src/tests/delete_indexed_tree_tests.rs`: 1, `grovedb/src/tests/flat_drop_tests.rs`: 1, `grovedb/src/tests/indexed_reference_row_tests.rs`: 1, `grovedb/src/tests/indexed_tree_secondary_drift_tests.rs`: 1, `grovedb/src/tests/move_element_tests.rs`: 1, `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs`: 1, `grovedb/src/tests/non_merk_integrity_audit_tests.rs`: 1, `grovedb/src/tests/non_merk_subtree_discovery_tests.rs`: 1, `grovedb/src/tests/private_document_store_tests.rs`: 1, `grovedb/src/tests/provable_sum_tree_tests.rs`: 1, `grovedb/src/tests/verify_grovedb_aggregate_recompute_tests.rs`: 1, `grovedb/src/tests/verify_grovedb_indexed_tests.rs`: 1, `grovedb/src/util.rs`: 1, `grovedb/src/util/compat.rs`: 1, `grovedb/src/util/visitor.rs`: 1, `grovedb/src/visualize.rs`: 1, `merk/benches/merk.rs`: 1, `merk/src/element/get.rs`: 1, `merk/src/element/insert.rs`: 1, `merk/src/merk/mod.rs`: 1, `merk/src/merk/open.rs`: 1, `merk/src/merk/restore.rs`: 1, `merk/src/test_utils/mod.rs`: 3, `merk/src/test_utils/temp_merk.rs`: 1, `merk/src/tree/put_value/mod.rs`: 1 |
| `RawIterator` | 52 | 25 | `grovedb-bulk-append-tree/src/test_utils.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 1, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 2, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 1, `grovedb-query/src/query_item/mod.rs`: 3, `grovedb/src/batch/move_element.rs`: 5, `grovedb/src/element/aggregate_sum_query/mod.rs`: 1, `grovedb/src/element/elements_iterator.rs`: 5, `grovedb/src/element/query.rs`: 1, `grovedb/src/operations/auxiliary.rs`: 1, `grovedb/src/operations/indexed_tree.rs`: 5, `grovedb/src/replication/verify.rs`: 1, `grovedb/src/tests/commitment_tree_tests.rs`: 1, `grovedb/src/tests/delete_indexed_tree_tests.rs`: 1, `grovedb/src/tests/flat_drop_tests.rs`: 1, `grovedb/src/tests/move_element_tests.rs`: 1, `grovedb/src/tests/nested_indexed_secondary_cleanup_tests.rs`: 1, `grovedb/src/tests/non_merk_subtree_discovery_tests.rs`: 1, `grovedb/src/tests/private_document_store_tests.rs`: 1, `grovedb/src/tests/snapshot_fetch_node_tests.rs`: 2, `merk/src/merk/clear.rs`: 1, `merk/src/merk/mod.rs`: 5, `merk/src/merk/restore.rs`: 1, `merk/src/proofs/query/count_offset/pinned_view.rs`: 6, `merk/src/proofs/query/count_offset/tests.rs`: 2 |
| `TempStorage` | 40 | 8 | `grovedb/src/estimated_costs/worst_case_costs.rs`: 2, `merk/benches/merk.rs`: 2, `merk/src/element/get.rs`: 2, `merk/src/element/insert.rs`: 2, `merk/src/merk/open.rs`: 2, `merk/src/merk/restore.rs`: 23, `merk/src/test_utils/temp_merk.rs`: 4, `merk/src/tree/put_value/mod.rs`: 3 |
| `WorstKeyLength` | 22 | 19 | `grovedb/src/batch/batch_structure.rs`: 1, `grovedb/src/batch/estimated_costs/average_case_costs.rs`: 1, `grovedb/src/batch/estimated_costs/worst_case_costs.rs`: 1, `grovedb/src/batch/key_info.rs`: 2, `grovedb/src/estimated_costs/average_case_costs.rs`: 2, `grovedb/src/estimated_costs/average_case_costs/merk_insert_element/v0.rs`: 1, `grovedb/src/estimated_costs/average_case_costs/merk_insert_element/v1.rs`: 1, `grovedb/src/estimated_costs/average_case_costs/merk_insert_element/v2.rs`: 1, `grovedb/src/estimated_costs/average_case_costs/merk_replace_element/v0.rs`: 1, `grovedb/src/estimated_costs/average_case_costs/merk_replace_element/v1.rs`: 1, `grovedb/src/estimated_costs/worst_case_costs.rs`: 2, `grovedb/src/estimated_costs/worst_case_costs/merk_insert_element/v0.rs`: 1, `grovedb/src/estimated_costs/worst_case_costs/merk_insert_element/v1.rs`: 1, `grovedb/src/estimated_costs/worst_case_costs/merk_insert_element/v2.rs`: 1, `grovedb/src/estimated_costs/worst_case_costs/merk_replace_element/v0.rs`: 1, `grovedb/src/estimated_costs/worst_case_costs/merk_replace_element/v1.rs`: 1, `grovedb/src/operations/delete/average_case.rs`: 1, `grovedb/src/operations/delete/flat_drop.rs`: 1, `grovedb/src/operations/delete/worst_case.rs`: 1 |
| `Error::StorageError` | 17 | 7 | `grovedb-bulk-append-tree/src/test_utils.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 4, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 4, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 2, `grovedb/src/batch/move_element.rs`: 1, `grovedb/src/tests/snapshot_fetch_node_tests.rs`: 2, `merk/src/proofs/query/count_offset/tests.rs`: 2 |
| `StorageContext::RawIterator` | 17 | 7 | `grovedb-bulk-append-tree/src/test_utils.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 4, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 4, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 4, `grovedb/src/batch/move_element.rs`: 1, `grovedb/src/element/local_offset_read.rs`: 1, `merk/src/merk/mod.rs`: 1 |
| `StorageContext::Batch` | 16 | 5 | `grovedb-bulk-append-tree/src/test_utils.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 4, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 4, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 4, `merk/src/merk/restore.rs`: 2 |
| `PrefixedRocksDbImmediateStorageContext` | 10 | 3 | `grovedb/src/lib.rs`: 2, `grovedb/src/replication/state_sync_session.rs`: 4, `merk/src/merk/restore.rs`: 4 |
| `Storage::Transaction` | 10 | 5 | `grovedb/src/lib.rs`: 1, `merk/src/merk/restore.rs`: 1, `merk/src/test_utils/mod.rs`: 2, `merk/src/test_utils/temp_merk.rs`: 5, `merk/src/tree/put_value/mod.rs`: 1 |
| `Batch` | 9 | 7 | `grovedb-bulk-append-tree/src/test_utils.rs`: 2, `grovedb-commitment-tree/src/commitment_tree/tests.rs`: 1, `grovedb-dense-fixed-sized-merkle-tree/src/test_utils.rs`: 2, `grovedb-merkle-mountain-range/src/tests/test_storage_adapter.rs`: 1, `merk/src/merk/clear.rs`: 1, `merk/src/merk/mod.rs`: 1, `merk/src/merk/restore.rs`: 1 |
| `PendingPrefixDropRecord::primary_prefix` | 5 | 2 | `grovedb/src/batch/mod.rs`: 2, `grovedb/src/operations/delete/flat_drop.rs`: 3 |
| `PendingPrefixDropRecord` | 4 | 2 | `grovedb/src/batch/mod.rs`: 2, `grovedb/src/operations/delete/flat_drop.rs`: 2 |
| `PendingPrefixDropRecord::path` | 4 | 2 | `grovedb/src/batch/mod.rs`: 1, `grovedb/src/operations/delete/flat_drop.rs`: 3 |
| `pending_prefix_drops_namespace` | 3 | 3 | `grovedb/src/batch/mod.rs`: 1, `grovedb/src/operations/delete/flat_drop.rs`: 1, `grovedb/src/tests/flat_drop_tests.rs`: 1 |
| `PendingPrefixDropRecord::doomed_prefixes` | 3 | 2 | `grovedb/src/batch/mod.rs`: 1, `grovedb/src/operations/delete/flat_drop.rs`: 2 |
| `Error::StorageError::0` | 2 | 2 | `grovedb/src/tests/snapshot_fetch_node_tests.rs`: 1, `merk/src/proofs/query/count_offset/tests.rs`: 1 |
| `Storage::BatchTransactionalStorageContext` | 2 | 1 | `merk/src/test_utils/mod.rs`: 2 |
| `Error::SnapshotReadOnlyTransaction` | 1 | 1 | `grovedb/src/tests/snapshot_read_transaction_tests.rs`: 1 |
| `Error::SnapshotReadOnlyTransaction::0` | 1 | 1 | `grovedb/src/tests/snapshot_read_transaction_tests.rs`: 1 |
