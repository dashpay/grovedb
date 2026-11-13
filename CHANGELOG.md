# Changelog

All notable changes to GroveDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `GROVE_V5` (`protocol_version` 5). `GROVE_V4` as 6.1.1 shipped it is what
  Dash Platform 5.0 (protocol version 14) runs, so behaviour changes from
  here on are gated in `GROVE_V5` and leave V1..V4 as they are. V5 is V4
  plus the store range reads and proofs, the store creation estimates, the
  query changes and repeated keyless appends below.
- A `PrivateDocumentStore`'s entries can be read and proved by position
  range from `GROVE_V5`. `GroveDb::private_document_store_get_range` reads
  `[start, start + limit)` in append order, clamped to the store's count
  (`PrivateDocumentStore::get_range` in the store crate).
  `prove_bulk_position_range` / `verify_bulk_position_range_proof` accept a
  store, and a V1 proof's subquery into a store carries its bulk append
  tree's proof, which the verifier binds through the store's committed
  config (`blake3("pds_state" || config_hash || bulk_state_root)`, from the
  authenticated element). Before, V1 proofs refused subqueries into a store.
  Gated by the new `private_document_store.get_range` and
  `private_document_store.range_proof` slots (0 in V1..V4, 1 in V5;
  **BREAKING** for code building `GroveDBOperationsPrivateDocumentStoreVersions`
  by hand). The proof envelope gains no variant; the `verify` feature now
  builds `grovedb-private-document-store` (without storage) for the store's
  root.
- `StorageFlags::update_element_flags_with_written_owner`: a flags update
  where the owner the new flags name owns the element afterwards, whether the
  update grows it, shrinks it or keeps its size. The writer keeps an element
  with its owner by writing that owner, and hands it over by writing another.
  The byte accounting is unchanged: bytes the update frees are refunded to the
  old owner through the old flags, and bytes that stay stored move with the
  element. `StorageFlags::update_element_flags` is unchanged, including keeping
  the old owner on an update that keeps the size, or that shrinks an element
  holding bytes of one epoch when the new flags' epoch is later than that one.

- A flags update can have an update in place accounted as the removal of the
  old element plus the insertion of the new one, so a handover can refund the
  old owner in full and charge the new owner for everything. The flags
  callback may now answer an `ElementFlagsUpdate` (`Unchanged`, `Changed`,
  `AccountAsReplacement`, new in `grovedb_costs`) in place of a `bool`; a
  closure answering `bool` keeps working through `apply_batch_with_element_flags_update`,
  `apply_partial_batch_with_element_flags_update` and
  `estimated_case_operations_for_batch`, but a closure whose `Ok` type cannot be
  inferred (one that only returns `Err(..)`, `panic!()` or `todo!()`) now needs
  an explicit `-> Result<bool, Error>`. Every byte of the old element, key
  included, counts as removed and is sectioned by the removal callback as a
  deletion sections it, every byte of the new element, key included, counts as
  added, and the element keeps the flags the callback leaves it with. GroveDB
  does not look at owners: `StorageFlags::update_element_flags_replacing_on_owner_change`
  answers it when the new flags name a different owner. A batch refuses it on
  a write whose flags are byte-equal to the stored ones (which covers an
  untrusted `RefreshReference` and a tree's root update), with
  `Error::InvalidBatchOperation`. Merk accepts the answer from
  `merk_versions.tree.just_in_time_value_update` version 2 (V5); earlier
  versions refuse it. Estimates do not model it, because an estimator cannot
  ask the callback: estimate such a write as an insertion. A callback that never
  answers it sees every cost, stored element and hash as before. **BREAKING**
  for callers of Merk's just-in-time apply, whose update callback now answers
  `(ElementFlagsUpdate, Option<ValueDefinedCostType>)`.
### Changed
- **BREAKING**: `Query` adds the public `offset: Option<u16>` field for
  branch-local committed-count-unit pagination. Exhaustive struct literals
  must supply the field or use a default update. Offset-bearing queries use
  a new native version-3 flag and a new serde version-4 frame, which older
  decoders reject; offset-free encodings remain unchanged.
- **BREAKING**: `QueryOptions` adds `include_parent_tree_on_subquery`.
  Exhaustive struct literals must supply the field or use a default update.
  It defaults to `true`, and applies to V5 raw reads (`allow_get_raw`) only:
  non-raw reads and value projections never return parent echoes. A V5 raw
  read refuses a nonzero global offset together with a parent echo, since
  echoes are not rows the offset can skip. Earlier query engines ignore the
  field.
- **BREAKING**: `QueryBudget` adds the public `rows` field; build one with
  `QueryBudget::new(..)`. `GroveDBPathQueryMethodVersions` adds
  `per_instance_query_offsets`; hand-built version tables set it to 0 for
  historical snapshots.
- `GroveVersion::latest()` now selects `GROVE_V5`. V5 enables per-instance
  query offsets on supported provable count trees, merge v2 for exclusive
  branch-local pagination, and the v2 trusted query engine with parent-tree
  echoes. Earlier Grove versions continue to reject local offsets. Merge v2
  carries a branch's own `Query::offset` but, like v1, refuses a nonzero
  global `SizedQuery::offset`, which skips rows rather than count units. It
  also refuses a global limit on a body with subqueries, which a branch cap
  could not represent because it is not charged for empty subtrees, and a node
  that echoes parents while shared with other inputs at any depth.
- From `GROVE_V5` a batch may carry the same keyless append
  (`CommitmentTreeInsert`, `MmrTreeAppend`, `BulkAppend`, `DenseTreeInsert`,
  `PrivateDocumentStoreInsert`) more than once (issue #1031). Each lands at
  its tree's next position, so the batch stores what the same appends applied
  one by one store. The batch consistency check counts keyed operations only
  for repeats there; `GROVE_V1`..`V4` still count every operation and refuse
  such a batch, as 6.1.1 does. Every other consistency check, stored data,
  proofs and costs are unchanged. Gated by the new
  `apply_batch.keyless_append_repeats` slot (0 in V1..V4, 1 in V5).
  **BREAKING** for API users:
  - `QualifiedGroveDbOp::verify_consistency_of_operations(ops, grove_version)`
    takes the caller's `&GroveVersion` and returns
    `Result<GroveDbOpConsistencyResults, Error>`, an error for a slot value it
    does not know. A caller passes the version it applies the batch at and
    handles the `Result` (for example with `?`) before reading
    `is_empty()`.
  - `GroveDBApplyBatchVersions` gains `keyless_append_repeats`; code building
    it by hand sets it (0 for the V4 rule, 1 for the V5 rule).
- **BREAKING**: with the `sqlite` feature, `grovedb-commitment-tree` now
  depends on and re-exports rusqlite 0.41 (libsqlite3-sys 0.39), up from
  0.38. With the bundled SQLite (3.54) this clears RUSTSEC-2026-0459; builds
  that enable `bundled-sqlcipher` or link a system SQLite need their own
  check against the advisory. Callers that pass their own `Connection` to
  `ClientPersistentCommitmentTree::open` or `open_on_shared_connection`, or
  to `SqliteShardStore::new` or `new_shared`, must build it with rusqlite
  0.41. Because libsqlite3-sys declares `links = "sqlite3"`, a build can
  hold only one version of it, so other rusqlite users in the same build
  must move to 0.41 as well.

### Fixed
- From `GROVE_V5`, a batch that inserts a sum or count tree and leaves the
  level below it empty stores that tree. The batch carries operations at the
  tree's own path and none leaves a key there, for example a delete of a key
  that is not stored. An empty Merk reports no aggregate and the batch
  rebuilt the stored element from the aggregate alone, so it stored a plain
  `Tree` in place of any of the eight Merk aggregate trees: sums and counts
  written under it later never reached its parent, and `verify_grovedb`
  reported nothing. A `NonCounted` wrapper stayed around that plain tree,
  and a tree wrapped in `NotSummed` or `NotCountedOrSummed` failed the batch
  with `CorruptedCodeExecution`. Now the tree the caller sent is stored with
  a zero aggregate, wrapper included, the state the same insert without
  those operations leaves. The worst-case batch estimate, whose levels
  report no aggregate, sizes a sum or count tree inserted with operations
  under it as that tree too (9 more added bytes for a sum tree), as the
  average-case estimate has since `GROVE_V4`. Gated by the new
  `apply_batch.tree_insert_above_emptied_level` slot (0 in V1..V4, which
  keep their outcome, 1 in V5; **BREAKING** for code building
  `GroveDBApplyBatchVersions` by hand), because it changes a stored element,
  so the root hash and the cost, and accepts a batch V1..V4 refuse. A
  conditional insert (`InsertIfNotExists`) keeps the V1..V4 outcome: with
  operations under its tree it is written whether or not the key already
  holds one, so its type is not supplied over an existing empty tree's. A
  batch that inserts a tree over one that already holds rows still orphans
  them (#779).
- From `GROVE_V5`, a batch applied with
  `BatchApplyOptions::disable_operation_consistency_check` is refused with
  `InvalidBatchOperation` when it inserts trees of different tree types at
  one key. The child Merk was reserved with the first insert's tree type
  and a later insert did not change it, so when the batch also wrote under
  the key a `Tree` then a `SumTree` was stored as a `Tree` with the sum
  lost, and a `SumTree` then a `Tree` was stored as a `Tree` over a child
  written as a sum tree, which `verify_grovedb` reports. `GROVE_V1`..`V4`
  still apply such a batch, as 6.1.1 does. Repeated tree inserts of one
  tree type, the consistency check (which refuses any two operations at one
  key) and the cost estimates are unchanged. Gated by the new
  `apply_batch.same_key_tree_type_conflict` slot (0 in V1..V4, 1 in V5;
  **BREAKING** for code building `GroveDBApplyBatchVersions` by hand).
- From `GROVE_V5`, removing an entry from an indexed tree (PCIT, PSIT or
  PCPSIT) always removes its row from every secondary. After a level's apply
  the mirror read each changed key back from the primary. The level's writes
  are still pending at that point, and for some shapes of the primary the
  read returned an entry the level had just removed, so the mirror saw no
  change and the secondaries kept the row. The mirror now reads an entry by
  walking the primary's tree from its root (`Merk::observe_value_and_value_hash`),
  before and after the apply. The walk follows the tree as it stands, so a
  removed key is absent, in a partial batch's later segment as well. Gated
  by the new
  `apply_batch.indexed_mirror_entry_read` slot (0 in V1..V4,
  1 in V5; **BREAKING** for code building `GroveDBApplyBatchVersions` by
  hand): on V5 such a removal commits a different secondary root and is
  charged for the row's delete, and a write arriving from inside a child of
  the primary is charged fewer seeks and loaded bytes (the walk loads the
  path once and the apply reuses it, where the read fetched the key on its
  own); storage costs are otherwise what they were. The batch estimate
  follows the same slot: on V5 it reserves two walks of the primary per
  touched entry in place of four node fetches, so it grows with the
  primary's estimated levels and stays above a removal whose post-state
  walk loads nodes the removal did not. `GROVE_V4` is unchanged
  from 6.1.1 and still keeps the row; a tree written under it may hold such
  rows, which `verify_grovedb` reports as secondary orphans and
  `reconcile_indexed_tree_secondaries` removes.
- MMR proof verification (`MerkleProof::calculate_root`, `verify`,
  `calculate_root_with_new_leaf` and `verify_incremental`,
  `MmrTreeProof::verify` and `verify_and_get_root`, and through them the
  chunk proofs of bulk append, commitment and private document store trees)
  accepts a proof only in the layout `MMR::gen_proof` emits for the proved
  leaves: one item for each peak with no proved leaf that is left of the
  last proved leaf, and exactly one item for all the peaks right of it (the
  peak itself when there is one, their bag when there are more). The
  verifier used to stop at the peak where the proof's items ran out and bag
  what it had, so the number of items chose how many peaks were walked; the
  layout now follows from `mmr_size` and the leaf positions alone. A proof
  with too few items, with the right-hand peaks sent one by one, or with
  items for a one-leaf MMR is `InvalidProof`, and so is
  `MerkleProof::calculate_root` with no leaves. Not version-gated: no hash
  changes and every proof `gen_proof` emits verifies as before.
- An indexed tree's axis proof carries, for each row, the entry the row
  points at and a label saying how that entry's bytes fold into its committed
  hash (`IndexedTargetCommitment`). The verifier now requires the label to be
  the one the entry's own element takes: `Reference` for a reference,
  `Simple` for an item, `IndexedSingle` / `IndexedMulti` for an indexed tree
  and `Layered` for any other tree. It used to take the label as given, and a
  reference and a tree fold the same way, so a reference entry labelled
  `Layered` over its target's hash verified and returned the reference itself
  instead of the element it resolves to. Such a proof is now refused with
  `CorruptedData`. Not gated by grove version: the prover has always written
  the element's own label, and proof verification is not part of block
  execution.
- Reads through a pinned iterator view, indexed-tree pages and V5 local count
  pages alike, report a failed seek as the storage error. A failed seek reads
  as no record, so an indexed-tree page called it corruption and a local page
  could have come back empty.
- From `GROVE_V5`, insert estimates price creating a `PrivateDocumentStore`
  as the apply does. The average- and worst-case `merk_insert_element`
  estimators v2 (selected by `average_case_merk_insert_element: 1` and
  `worst_case_merk_insert_element: 1`) charge the existence check a
  write-once store makes before it is created (priced as `InsertIfNotExists`
  prices its check) and the two blake3 calls deriving its empty root (the
  new `EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS`), and the worst
  case prices the store as the layered tree the apply writes instead of an
  item at its serialized size. v1, which `GROVE_V4` keeps, is unchanged
  from 6.1.1: it estimates a store's creation one seek and two hashes (three
  in the worst case) below its apply, which the batch estimate's
  propagation headroom happens to cover. Estimates of every other element,
  applied costs, stored data and proofs do not change.
- From `GROVE_V5`, count-offset proof verification accepts pages whose rows
  resolve through a reference to an empty tree, which earlier versions could
  persist. The empty-tree hash check applies only to direct tree rows on the
  global offset path (the new `proof.count_offset_reference_empty_tree_rows`
  slot, 0 in V1..V4 and 1 in V5; **BREAKING** for code building
  `GroveDBOperationsProofVersions` by hand) and on V5 local pages; resolved
  references keep the target binding Merk already verifies. V1..V4 refuse
  such a global page, as 6.1.1 did.
- `verify_query_get_parent_tree_info` and
  `verify_subset_query_get_parent_tree_info` report the aggregate of the tree
  the query's path ends in when a result row is itself a tree whose lower
  layer is in a V1 proof. They reported that row's own aggregate instead: for
  a sum tree holding 1005 whose row `child` is a sum tree holding 1000, a
  subset verification of the row returned `SummedMerkNode(1000)`. A populated
  indexed tree reported as a row always carries such a layer, so that case
  needed no subset verification. Not gated by grove version: proof
  verification is not part of block execution. V0 proofs are unchanged.
- From `GROVE_V5`, deleting an element of a `NormalTree` refunds what its
  insertion was charged. Before, a delete there that did not declare a tree
  refunded the raw value bytes: an empty `Tree` removed by a batch `Delete`
  gave back 31 bytes more than it was charged (23 for a `SumTree` or
  `CountTree`), because a tree does not pay for its value hash, and a
  `SumItem` gave back up to 8 bytes less, because it is charged a fixed
  size. Such a delete now reads the stored element, as deletes in every
  other tree type already did (the `element.delete`,
  `element.delete_with_sectioned_removal_bytes` and
  `element.delete_into_batch_operations` slots, 0 in V1..V4 and 1 in V5).
  Refunds of items, references and declared tree deletes, stored data and
  root hashes do not change.
- `GroveDb::fetch_chunk` refuses a state sync request whose tree type does
  not match the stored rows with `CorruptedData`. The prefix, root key and
  tree type of a chunk request come from the requesting peer, and only the
  root the request named was compared with the tree type. That settles it
  for a prefix holding a Merk, whose nodes share one aggregate family, but
  not for rows that are not nodes of one Merk of that type, where a request
  could stop the serving process instead of being refused. The Merk chunk
  producer (`ChunkProducer`) now checks every node it emits or hashes
  against the tree type it serves under (`CorruptedState` on a mismatch)
  and takes the hashes at a chunk's boundary fallibly; the root hashes of
  an indexed header are taken the same way. New
  `TreeNode::try_hash_for_link` returns an error where `hash_for_link`
  panics; `hash_for_link` and the write path are unchanged. Not gated by
  grove version: an honest target never sends such a request, and the
  chunks it is served do not change.

## [6.1.1] - 2026-10-20

### Fixed
- The `grovedbg` feature unpacks the bundled GroveDBG web UI with `zip` 9
  instead of `zip-extensions` 0.13, which pulled `zip` 6 (#1023). `zip` 6
  and 8 are covered by RUSTSEC-2026-0367, RUSTSEC-2026-0368 and
  RUSTSEC-2026-0369. The archive is now read from memory, so no temporary
  `grovedbg.zip` file is written. Nothing changes outside `grovedbg`, and
  every `GroveVersion` behaves as in 6.1.0.

## [6.1.0] - 2026-10-13

### Changed
- From `GROVE_V4` a `ProvableCountProvableSumIndexedTree` primary accepts a
  bare `SumItem` child, on the direct and the batch path, as it accepts an
  `ItemWithSumItem`: the tree aggregates it as a count of one and its sum,
  which is every input its count, sum and average axes read. A tree of
  per-group counters can then rank its groups by how many counters they
  hold, their total and their average without wrapping each counter in an
  empty item. V1..V3 keep refusing it, through the new
  `insert.validate_indexed_child_for_variant` slot (**BREAKING** for code
  building `GroveDBOperationsInsertVersions` by hand); a wrapped `SumItem`
  stays refused at every version.

### Added
- `GroveOp::Move` and `QualifiedGroveDbOp::move_op(path, key, new_key)`
  rename a key within its parent in a batch (issue #1014). The element keeps
  its bytes and node value hash, and for a tree every record under it — nested
  Merk trees, non-Merk trees' data, indexed trees' secondary Merks — is
  copied byte for byte to the storage prefixes of the new path, so nothing
  below the moved key is re-hashed, rebalanced or re-resolved. A moved
  reference is followed from its new key; when its path reaches something
  the same batch writes, it commits to what it reaches there after the
  batch, like a reference the batch writes, and otherwise keeps the hash it
  was stored with. Moves can swap or chain keys in one batch; a target
  occupied before the batch must be moved away by the same batch. The move
  is billed as storage that stays where it is: replaced bytes, plus the
  change in key length. Refused for backward-reference participants, a
  moved reference that resolves through its own key (cousin and
  removed-cousin) or that no longer resolves from the new key (it would
  reach a key the batch moves, close a cycle, or find no target), a
  reference directly under a moved element that appends its parent's key,
  moves inside an indexed tree, elements with flags moving to a key of
  another length, partial batches and cost estimates. Other references are
  carried as stored and not followed, so keeping them valid is the
  caller's job, as for a delete: one that names the old key reaches
  whatever is there after the batch, which in a swap or chain can be the
  reference itself or one leading back to it. Reads of such a reference,
  proofs through it, `verify_grovedb` with `verify_references` and state-sync
  finalization then fail with `CyclicReference`, as they fail for a
  dangling reference. Likewise a moved reference whose target lies under a
  tree the same batch deletes or replaces is not refused: it keeps its
  stored hash and dangles afterwards. Batches accept moves from `GROVE_V4`,
  through the new `apply_batch.move_element` slot (**BREAKING** for code
  building `GroveDBApplyBatchVersions` by hand, or matching `GroveOp` or
  merk's `Op` exhaustively: merk gains `Op::PutMoved` and `Op::DeleteMoved`;
  merk's apply refuses a `PutMoved` whose former key length exceeds 255).
  `RawIterator` gains `status()` (default `Ok(())`, overridden by the RocksDB
  iterators), which the move's copy checks so that a read error fails the
  batch instead of committing a partial copy; other scans still stop at a
  read error as at the end of their records.
  `ElementType::supports_backward_references` classifies a serialized
  element as a backward-reference participant; `Element`'s method of the
  same name now reads it, so it also looks through wrappers. Decoders and
  constructors never produce a wrapped participant, but a hand-built
  wrapper chain around one (`NonCounted(NonCounted(ItemWithBackwardsReferences(..)))`,
  say) is now refused up front by the insert and batch gates at every
  version: `NotSupported` / `InvalidBatchOperation` at no cost, where V1..V3
  returned `InvalidInput` after opening the Merk, or panicked in a batch
  under a count tree.
- `ProofLimitMode::UpperBound` and `QueryProofVerify::execute_proof_with_limit_mode`
  verify a proof with `limit` as a ceiling instead of the exact limit the
  prover used. Once the proof has returned a result, the walk stops at the
  first hidden node inside a queried range, or at the end of the proof with
  query items unproven. After that only hidden nodes may follow: a V1 prover
  reveals no key once its limit runs out, so a node revealing one is
  rejected, and the results stay a gap-free prefix. A proof from a prover
  that still reveals a range bound after its cut (before the V1 shape hid
  it) is therefore rejected. `ProofVerificationResult::exhausted` reports
  whether the proof shows nothing further matches.
  `GroveDb::verify_aggregate_{count,sum,count_and_sum}_query_per_key_up_to_limit`
  verify carrier aggregates this way and return the root hash with a
  `CarrierAggregatePage`: `Complete` when no further outer key matches,
  otherwise `MoreAfter`, which carries the last outer key to continue after.
  A ceiling of `Some(0)` is refused. A client no longer has to know the
  server's limit to verify an honest proof. Existing verification calls
  behave as before. The new trait
  method has a default body (exact mode through `execute_proof`, upper-bound
  mode refused), so existing `QueryProofVerify` implementations keep
  compiling. Code outside the crate that builds a `ProofVerificationResult`
  or destructures it exhaustively must account for the new `exhausted`
  field. (#1008)
- `StorageContext::raw_iter_aux`, a raw iterator over the aux column family
  scoped to the context's subtree prefix like `raw_iter`, and
  `GroveDb::get_aux_by_key_prefix`, which lists every aux entry under a key
  prefix in key order, including a transaction's own uncommitted writes when
  read through it. Aux storage was reachable by exact key only, so a
  collection kept as one aux entry per member could not be read back. (#968)

### Changed
- **BREAKING**: `TreeNode::put_value_with_fixed_cost`,
  `TreeNode::put_value_with_reference_value_hash_and_value_cost`,
  `TreeNode::put_value_with_two_reference_value_hashes_and_value_cost` and
  their `Walker` counterparts take a `&GroveVersion`, which selects the
  just-in-time value update (see the `merk_versions.tree.just_in_time_value_update`
  entry under Fixed). `TreeNode::provided_value_hash_put_final_value` is
  renamed `TreeNode::predict_put_final_value` and takes the value-defined
  cost the stored node is loaded with, the put it predicts (the new
  `grovedb_merk::tree::PredictedPut`: an ordinary put as
  `TreeNode::put_value` installs it at the grove version, a
  provided-value-hash put, or a put at a specialized cost) and a
  `&GroveVersion`. `grovedb_merk::element::insert::specialized_put_cost`,
  the value-defined cost an ordinary write puts an element with, and
  `Merk::replacement_runs_value_update`, whether a write over a key runs
  the just-in-time value update, are new.
- **BREAKING**: `delete_operation_for_delete_internal` and
  `delete_operations_for_delete_up_tree_while_empty` take the operations
  already pending in the batch as any `impl PendingOperations` in place of a
  slice and an owned `Vec`. A caller that keeps its batch in its own operation
  type had to copy the whole batch for every delete it built, so building k
  deletes into one batch cost O(k^2) copies. The builders ask for the pending
  operations at one path, the deleted tree's own, and only when the deleted
  element is a tree: never for a non-tree delete, and once per tree an
  up-tree chain climbs through. `PendingOperations` is implemented for every
  `IntoIterator<Item = &QualifiedGroveDbOp> + Clone` (a slice, `&Vec`,
  `ops.iter()` or an adapter over the caller's own operation type), which
  scans all of its operations each time it is asked; a caller building many
  deletes into one large batch can implement it over an index of its batch by
  path, so that each question costs only the operations at that path. `&[]`,
  `&ops` for a `Vec` or slice `ops`, and `vec![]` compile as before; the
  argument no longer deref-coerces, so a `&mut Vec`, `&&Vec`, `&Box<[_]>` or
  `&Arc<Vec<_>>` is passed as `ops.iter()`. The operations and costs built are
  unchanged apart from the `GROVE_V4` fix under Fixed.
  `add_delete_operations_for_delete_up_tree_while_empty` keeps its signature,
  but appends the chain's deletes to the caller's `Vec` once every level is
  built, and leaves the `Vec` as it was on an error, where it used to keep the
  deletes built below the failing level. (#995)
- **BREAKING**: `StorageFlagsError` has a new variant,
  `NonCanonicalStorageFlags`, for multi-epoch storage flag bytes that are
  complete but not the encoding `serialize` emits (see the storage flag entry
  under Fixed). The enum is not `#[non_exhaustive]`, so an exhaustive `match`
  over it must handle the new variant to compile. (#977)
- Single-path axis reads (`PathQuery::new_axis*`) over a path that does not
  exist — a missing segment, or an empty tree above one — now answer with the
  traversal's empty result (no entries and `skipped: Some(0)` for a ranked
  page, `None` for a bounded walk, `0` for a value-range aggregate) on
  `run_path_query`, `prove_query` and `verify_path_query` alike, matching
  key selection and the branched axis shape. Previously the trusted read
  failed with a storage error and the prover refused to generate ("must
  produce exactly one axis descent"), so a ranking over a window nothing had
  been written to yet was unanswerable. A path that is present but does not
  lead to an indexed tree is still refused, and `RankOfKey` still fails over
  an absent path. `AxisKeys::empty_for_axis` is new. (#965)

### Fixed
- From `GROVE_V4`, an average-case batch estimate sizes a sum, big sum,
  count or count-sum tree inserted in the same batch as its contents as the
  tree it is. Before, every level except an indexed tree reported no
  aggregate to the op writing its tree element in the parent, so the
  element was sized as a plain tree and the estimate's added bytes came in
  below the applied ones by the aggregate's bytes (9 per axis, 16 for a big
  sum); a replaced root key of such a tree was under-sized the same way in
  replaced bytes. V1..V3 keep the old sizing for replay, through the new
  `operations.average_case.average_case_propagated_aggregate_kind` slot
  (**BREAKING** for code building `GroveDBOperationsAverageCaseVersions` by
  hand).
- From `GROVE_V4`, an average-case estimate sizes a `SumItem` or
  `ItemWithSumItem` (wrapped or not) by the specialized cost the apply
  writes it with, which charges the sum value at its fixed 9-byte width
  (`SUM_ITEM_COST_SIZE`). Before, `average_case_merk_insert_element` sized
  it by its serialized size, which encodes the sum as a varint, and
  `average_case_merk_replace_element` did the same for an
  `ItemWithSumItem`, so the estimate came in below the applied bytes by up
  to 8 per item: a `SumItem(-3)` was estimated at 3 value bytes and applied
  at 11. V1..V3 keep the old sizing for replay, through the new
  `operations.average_case.average_case_sum_item_cost_size` slot
  (**BREAKING** for code building `GroveDBOperationsAverageCaseVersions` by
  hand).
- From `GROVE_V4`, a worst-case estimate sizes a `SumItem` or
  `ItemWithSumItem` (wrapped or not) by the specialized cost the apply
  writes it with, as the average-case estimate does since the entry above.
  Before, `worst_case_merk_insert_element` sized it by its serialized size,
  which encodes the sum as a varint, and `worst_case_merk_replace_element`
  did the same for an `ItemWithSumItem` and left out a wrapped `SumItem`'s
  wrapper byte, so the worst case came in below the applied bytes by up to
  8 per item: a `SumItem(-3)` inserted into a normal tree was estimated at
  142 added bytes and applied at 150. V1..V3 keep the old sizing for
  replay, through the new
  `operations.worst_case.worst_case_sum_item_cost_size` slot (**BREAKING**
  for code building `GroveDBOperationsWorstCaseVersions` by hand).
- From `GROVE_V4`, a reference a batch writes or refreshes alongside an
  update of its target item commits to the bytes the update stores. Before,
  the batch predicted those bytes with a hand-written copy of Merk's
  just-in-time value update that took a sum item (`SumItem`,
  `ItemWithSumItem`) to keep its stored flags, while the apply stores the
  flags the flags update leaves it. When the flags update merges them (an
  owned item updated in a later epoch, an unowned one gaining an owner) the
  reference committed to bytes that are never stored, which
  `verify_grovedb` reports. Drive writes this batch for a document of a
  summable type: a refresh of a deeper index reference next to the
  primary-storage update. The prediction also refused a sum item replaced
  by an item, or the reverse, that applies on its own. Now a reference to
  a target the batch has already written hashes the bytes stored there, and
  one to a target still to be written commits to the bytes Merk's own
  just-in-time value update gives for the put the apply performs, once per
  target. V1..V3 keep the old prediction for replay, through the new
  `apply_batch.same_batch_reference_target_prediction` slot (**BREAKING**
  for code building `GroveDBApplyBatchVersions` by hand).
- From `GROVE_V4`, a replacement whose flags update leaves the flags
  unchanged is charged for the bytes it stores. Before, Merk measured the
  replacement with the new value carrying the old value's flags and kept
  that measurement, so when the new value's own flags had another length
  (a plain `apply_batch` replacing a flagged item with longer, shorter or
  no flags, or Drive's merging callback on an unowned item gaining an owner
  as it grows) the commit failed with `storage_cost cost mismatch`. A round
  that rewrites the flags and then restores the put's value now also
  restores its value-defined cost, so a sum item or tree measured again is
  measured with the cost of the bytes it holds. Every replacement V1..V3
  accept is charged the same. V1..V3 keep the old measurement for replay,
  through the new `merk_versions.tree.just_in_time_value_update` slot
  (**BREAKING** for code building `MerkTreeVersions` by hand).
- From `GROVE_V4`, a reference a batch resolves through a pending
  `InsertIfNotExists` reaches the element already stored at that key when
  there is one. Before, it always reached the op's element, so a reference
  inserted (or moved) in the same batch as a skip-mode
  `insert_if_not_exists_or_skip_op` over an existing key committed to an
  element that is never written, which `verify_grovedb` reports and proofs
  reject. The existence check is the one the insert's own level makes.
  V1..V3 keep the old resolution for replay, through the new
  `apply_batch.reference_through_conditional_insert` slot (**BREAKING** for
  code building `GroveDBApplyBatchVersions` by hand).
- A batch carrying a nested element wrapper, such as a hand-built
  `Element::NonCounted(Box::new(Element::NonCounted(..)))`, no longer panics.
  The checked constructors and both decoders refuse nesting, and the direct
  insert path has refused it with an error since `NotSummed` landed. But
  batch execution and batch reference following dispatch on `underlying()`,
  which unwraps one level, and their wrapper arms were `unreachable!`. Any
  element-carrying op whose parent accepted the outer wrapper panicked there,
  and so did a reference in the same batch following into such an op. Full
  batches, partial batches and partial-batch add-ons now refuse a nested
  wrapper up front with `InvalidBatchOperation`, so the batch writes nothing.
  The fix is not version-gated. A batch carrying a nested wrapper either
  panicked or failed at every grove version, and no production caller can
  build one, so no stored state or replayed block can change.
- A batch whose reference chain reaches another pending reference that
  resolves to an empty path no longer panics. `apply_batch` refused an empty
  path only for the reference op it was processing. Keys in a path run in
  byte order, so an earlier op could follow into a later pending reference
  (an `AbsolutePathReference(vec![])`, say, or a trusted `RefreshReference`
  carrying one) before that op's own check ran. The next hop then reached
  `process_reference(&[])`, where `split_last().expect(..)` panicked. Every
  hop now refuses an empty path with `InvalidBatchOperation`, so the batch
  fails and writes nothing. The fix is not version-gated. A batch that took
  this path never committed at any grove version, so no stored state or
  replayed block can change, and a batch that succeeded before resolves the
  same way now.
- A V1 proof whose limit runs out before its query does verifies at its own
  limit. The walk hides what it has not reached, but still passes back up
  through the ancestors of its last result, and one of them can be a range
  bound: an exclusive range end, or a later query item's bound. The prover
  revealed that key as a boundary node behind the nodes it hid, where the
  verifier cannot check it as a bound, so `verify_query` rejected the honest
  proof with "Cannot verify lower bound of queried range". Whether that
  happens depends on the tree's shape; it is common for subtrees written in
  one batch. V1 proofs now hide the key like the rest of the unwalked tree.
  The proof hashes to the same root, the verifier is unchanged, and every
  released verifier accepts the new shape. So the fix needs no new version
  slot, and clients verify these proofs as soon as the node serving them is
  upgraded. Proofs the limit does not cut are byte-identical. Merk's prover
  takes the shape from the grove version's proof envelope version
  (`prove_query_non_serialized`), so V0 proofs (grove v1 and v2) keep their
  shape and every prover under grove v3 and later, including the
  indexed-axis secondary proofs, uses the new one. An unknown envelope
  version is refused.
- An aggregate-on-range proof (`AggregateCountOnRange`,
  `AggregateSumOnRange`, `AggregateCountAndSumOnRange`, leaf or carrier
  shape) whose path, or whose carrier key followed by a `subquery_path`, runs
  through an EMPTY `ProvableCountTree`, `ProvableCountSumTree`,
  `ProvableSumTree` or `ProvableCountProvableSumTree` above the aggregate
  terminal is generated instead of failing with "Cannot create proof for
  empty tree". The prover descended into such a tree so that an empty
  terminal answers zero, but above the terminal the descent reached an
  ordinary layer over an empty Merk, which has no Merk proof. That tree is
  now proved in its parent layer with no lower layer, as an empty tree of
  any other type there already was: its value hash binds it to the empty
  child root, and `verify_query` with the single-key query over the same
  path verifies the proof. The aggregate verifiers are unchanged and still
  refuse a path that ends before the terminal. An empty tree that is the
  terminal itself is still descended into, to its authenticated zero. This
  holds at every grove version that proves aggregate queries (`GROVE_V3` and
  later): the descent above the terminal always failed, so no proof that
  generated before changes, and the change needs no version slot. (#1010)
- A path with a segment longer than 255 bytes no longer panics. Storage
  panicked while building the subtree prefix for it, since a prefix records
  each segment length in one byte. Where it did, the call now returns
  `Error::InvalidInput("path segment length must be at most 255 bytes")`.
  Keys are capped at 255 bytes on insert, so no subtree has such a path. The
  check runs only where a prefix would be built from the caller's path, so
  every call that never built one keeps its old result. Do not read
  `InvalidInput` as the only answer to such a path:
  - a lookup whose overlong segment is the last one still reports the key as
    not found;
  - proofs still prove the path absent;
  - axis reads still return an empty result.

  Not version-gated, since every call the check refuses used to panic. (#680)
- From `GROVE_V4`, `delete_operation_for_delete_internal`, and the up-tree
  builders through it, no longer count a pending `DeleteTree` with
  `SubelementsDeletionBehavior::Skip` as removing its child when deciding
  whether a tree they delete is empty. The batch drops such a delete when the
  child is not empty, so a parent emptied only by it was deleted with
  `DontCheckWithNoCleanup` while the child was still in it, leaving the child
  and its contents orphaned in storage with `verify_grovedb` reporting
  nothing. The check now counts only the deletes the apply path counts in its
  own emptiness check. `GROVE_V1`..`GROVE_V3` keep the old rule. (#995)
- MMR proof verification (`MmrTreeProof::verify`, `verify_and_get_root`,
  `MerkleProof::calculate_root`, `calculate_root_with_new_leaf` and
  `verify_incremental`) now rejects an `mmr_size` no MMR can have. The peak
  helpers round a non-canonical size (2, 5, 6, ...) down to the last valid MMR
  below it, so an honest proof relabelled with such a size recomputed the same
  root, and `u64::MAX` maps to `2^63` leaves, whose last leaf index overflowed
  the position arithmetic (a panic under overflow checks) before the proof was
  bound to a trusted root. `MmrTreeProof::generate` refuses the same sizes.
  The new `checked_mmr_size_to_leaf_count` helper is the single definition of
  a canonical size and also backs the state-sync check. Not version-gated:
  every size a real MMR reaches is canonical, so no honest proof changes.
  (closes #692)
- `GroveDb::fetch_chunk` no longer panics when a state-sync peer names a tree
  type whose aggregate family differs from that of the subtree it addresses. The
  source opened the subtree with the peer's type as-is, so naming a `Provable*`
  type for a plain, sum or count subtree (plain chunk route), or an indexed
  type with a header request (indexed header route), reached the fail-closed
  `panic!` in `TreeNode::hash_for_link`: any peer could crash a serving node
  at will. Such a request is now refused with `Error::CorruptedData` before
  anything is hashed. Serving is not consensus, so this is not version-gated;
  an honest target always names the type its restored element carries, which
  is never refused. (#989)
- `BulkAppendTree` / `CommitmentTree` proof verification no longer lets a
  forged proof drive unbounded allocation before the proof is bound to a
  trusted root. The V1 lower-layer verifier took `total_count` and `height`
  from element bytes carried in the proof and extracted values before its
  caller compared the root. A 9-byte fixed-format chunk blob can declare
  2^20 zero-sized entries, and under a forged `height = 1` every blob's
  positions overlapped the query window. So a proof of a few KB made the
  verifier decode 2^20 entries per blob and return one row per blob per
  queried position, hundreds of thousands of duplicate rows for 64 blobs.
  Completeness is now checked first, from the proof's chunk indices and
  buffer positions, without decoding anything. Extraction then walks the
  query's position intervals in the query's direction, stops at the row
  limit, copies only the entries it returns, and refuses a chunk blob that
  does not hold exactly `2^height` entries without allocating per entry. It
  also refuses chunk indices that repeat, descend or reach the buffer. The
  new `BulkAppendTreeProofResult::values_in_ranges_directed`,
  `proved_position_spans`, `values_in_ranges`, `completed_chunk_entries`
  and `deserialize_completed_chunk_blob` carry these checks, and a bad
  chunk blob in a proof result is now reported as `InvalidProof`. The MMR,
  dense, bulk and commitment-tree lower layers now also bind their computed
  root to the parent row's value hash before reporting any row. Not
  version-gated: a client-side verifier change that honest proofs never
  trigger. (incomplete fix of #856 / #872)
- Multi-epoch storage flag decoding (`StorageFlags::deserialize_multi_epoch`,
  `deserialize_multi_epoch_owned` and everything routed through
  `deserialize`) now accepts only the bytes `serialize` emits. The record loop
  stopped with one or two bytes unread, so junk after valid flags (including a
  non-base epoch with no byte count) decoded successfully; a repeated epoch
  silently replaced the earlier record; and a non-base record on the base
  epoch was accepted, where removal splitting then overwrote that record's
  share with the base epoch's residue and reported fewer removed bytes than it
  was asked to split. Records must now be complete, minimally varint-encoded
  with a byte count that fits in a `u32`, and strictly ascending above the
  base epoch; missing or partial bytes are reported as
  `StorageFlagsWrongSize` and complete but non-canonical records as the new
  `StorageFlagsError::NonCanonicalStorageFlags` (see Changed).
  `split_storage_removed_bytes` now also adds the base epoch's residue to a
  share already taken at the base epoch instead of replacing it, for flags
  built in memory that never pass through the decoder, and `serialize`
  debug-asserts that non-base epochs are above the base epoch. Not
  version-gated: the combine functions only ever produce ascending records
  above the base epoch, so no stored flags decode or split differently.
  (closes #701)

## [6.0.1] - 2026-09-13

GroveDB 6.0.1 is the 6.0 release to depend on. It supersedes 6.0.0, which is
withdrawn (see its yanked entry below for what 6.0.1 changes relative to it).
The entries below are the notable changes recorded in this file; the full list
of merged pull requests is in the GitHub release notes for v6.0.0 and v6.0.1.
The package version is independent of the `GroveVersion` runtime compatibility
versions, which gain `GROVE_V4` in this release.

### Added
- Bidirectional references (#345): four new `Element` variants —
  `BidirectionalReference` (discriminant 25), `ItemWithBackwardsReferences`
  (26), `SumItemWithBackwardsReferences` (27), and
  `ItemWithSumItemWithBackwardsReferences` (28, the `ItemWithSumItem`
  twin) — plus a
  backward-references subsystem that keeps reference chains consistent:
  updating a referenced element propagates the new hash along every chain,
  and deleting/overwriting it cascades the chains away (each affected
  reference must opt in via `cascade_on_update`). Maintenance is automatic
  on V4, and every operation declares what it displaces (`BackwardsReferences`,
  see Changed); batch maintenance is gated by the new
  `apply_batch.backward_references_maintenance` version slot. The referrer list is stored on the element itself under a
  two-layer hash (`combine(inner, backrefs)`), so registering a referrer
  never re-hashes what existing referrers committed to; public reads return
  the stripped element, and proofs authenticate these elements through the
  new `Node::KVBackwardsReferencesValueHash` wire node whose value hash the
  verifier recomputes. Requires `GROVE_V4`; earlier versions, V0 proofs, and
  `Provable*` aggregate parents reject the new variants (fail closed).
  `apply_batch` supports the whole family by default: a preparation pass
  expands the batch into the derived registration/propagation/cascade
  operations the live flow performs (shared semantic core, so batch
  and non-batch execution produce byte-identical root hashes), including
  references whose targets are created in the same batch; conflicting
  combinations (a reference plus its target's deletion, a cascade hitting
  another op's position, `RefreshReference` on a bidirectional reference)
  fail closed. Each backward-references item declares how many referrers it
  accepts (`BackwardReferences::max_incoming`, authenticated with the
  element, at most the `MAX_BACKWARD_REFERENCES` ceiling of 256; the plain
  constructors declare 32, the `_with_capacity` constructors take an explicit
  value): registration past the capacity fails and an update may not lower
  it below the registered referrers. Average/worst-case batch estimation
  charges the derived fan-out on `GROVE_V4` (a written item's declared
  capacity, the ceiling for writes that cannot see the element they
  displace, ≤10-hop chains, 1 referrer per reference) while pre-V4
  estimation stays byte-stable for replay. See
  `adr/bidirectional_references.md`. `clear_subtree` exposes the same
  declaration: `Check` refuses participant-containing subtrees
  before mutation, `DontCheck` is trusted for a raw clear.
  `drop_flat_subtree` takes the declaration as a required argument, and both
  it and batch `DropFlat` require `DontCheck` to preserve O(1) cost.
  Ordinary batches that touch no participants retain their original executor
  semantics. Estimation charges the displaced-participant fan-out only for
  ops declared `Check`.
- **BREAKING**: Added `add_parent_tree_on_subquery` feature to PathQuery (#379)
  - New field in `Query` struct: `add_parent_tree_on_subquery: bool`
  - When set to `true`, parent tree elements (like CountTree or SumTree) are included in query results when performing subqueries
  - Particularly useful for aggregate trees where you need both the aggregate value and individual elements
  - Requires GroveVersion v2 or higher
  - Updated proof verification logic to handle parent tree inclusion

### Changed
- **BREAKING**: Replace `propagate_backward_references` with a per-operation
  declaration of the stored value an operation displaces. The live
  `InsertOptions`, `DeleteOptions` and `ClearOptions` carry
  `BackwardsReferences::{Check, DontCheck}` (`Check`
  by default), and every displacing batch op has a `DontCheckForBackwardsReferences` twin that
  makes the `DontCheck` declaration: `DeleteDontCheckForBackwardsReferences`,
  `DeleteTreeDontCheckForBackwardsReferences`, `InsertOrReplaceDontCheckForBackwardsReferences`, `ReplaceDontCheckForBackwardsReferences`,
  `PatchDontCheckForBackwardsReferences` (`GroveOp::dont_check_for_backwards_references` / `QualifiedGroveDbOp::dont_check_for_backwards_references`
  convert a checked op). `BatchApplyOptions` carries no backward-references
  policy. V4 has one write
  path: the displaced value is read for the write anyway, so
  `Check` maintains a participant it finds and `DontCheck`
  refuses the operation before anything commits; where nothing reads the
  contents (a flat drop, a raw `clear_subtree`, replacing a populated
  subtree, a live recursive delete) `DontCheck` is trusted. A batch
  `DeleteTree` is never pre-scanned: `DontCheckWithNoCleanup` declares that
  the batch's own deletes emptied the subtree, `Error` and `Skip` verify
  that at apply time, and `DeleteChildren` checks the declaration on the
  cleanup walk it makes anyway, refusing a participant the batch does not
  explicitly delete. A delete-up-tree chain and a batch recursive removal
  therefore cost the plain removal on V4. A flat drop must be a
  `DeleteTreeDontCheckForBackwardsReferences`. The batch ops GroveDB builds for a caller carry
  the caller's declaration: `delete_operation_for_delete_internal` reads it
  from `DeleteOptions`, the delete-up-tree chain from the new
  `DeleteUpTreeOptions::backwards_references`, and the average- and worst-case
  delete builders take a `BackwardsReferences` parameter
  (`QualifiedGroveDbOp::with_backwards_references` applies one to any op).
  Each `DeleteTree` removal a batch performs carries its own cleanup
  behavior and its own exemption from the post-apply participant scan, read
  off the op that performed it, so a `DeleteTree` in another partial-batch
  segment can neither change how it is cleaned up nor exempt a replacement
  at the same path.
  Partial batches still refuse participant mutations, and earlier protocol
  versions retain their historical behavior.
- **BREAKING**: `EstimatedLayerInformation::may_contain_backward_references`
  and the `*WithBackwardReferences` variants of `WorstCaseLayerInformation`
  are removed. The estimators charge the displaced-participant fan-out and
  delete probe per op declared `Check` instead of per layer; ops
  that write a participant themselves are charged from the op regardless.
- Bumped the GroveDB workspace crates and their internal dependency requirements
  to **6.0.1** for the public API changes since 5.0.1 (6.0.0 was published on
  2026-09-08 and withdrawn; see its entry below). This package version is
  independent of the existing `GroveVersion` runtime compatibility versions.
- Bumped `grovedb-bincode` and `grovedb-bincode-derive` together to **2.1.0**.
  Their new opt-in untrusted decoding traits, derives, and entry points are
  backward-compatible API additions to the fork's upstream 2.0.1 baseline.
- **BREAKING**: Git consumers of the GroveDB workspace now resolve the
  published `grovedb-bincode` / `grovedb-bincode-derive` 2.1.0 registry crates
  instead of the repository-local copies: the workspace `path` entries are
  removed and the exact `=2.1.0` requirements stay. Types that use GroveDB's
  codec share one `Encode` / `Decode` trait identity with SDKs that use the
  published crates; serialized bytes and decoding behavior are unchanged
  (#948)
- Updated delete function to include grove_version parameter (#377)
- Adjusted batch size type for better performance (#377)
- Renamed `prove_internal` to `prove_query_non_serialized` for clarity (#373)

### Fixed
- Recursive subtree discovery (`find_subtrees`, used by recursive deletion and
  `delete_up_tree`) no longer tries to decode populated non-Merk descendants
  (MMR, bulk append, dense, commitment and private document store trees) as
  Merk nodes, which failed with a Merk decoding error and prevented deleting an
  otherwise healthy ancestor. On `GROVE_V4`
  (`operations.non_merk_tree.subtree_discovery: 1`) descendants are classified
  from their already-decoded elements: every tree namespace stays in the
  cleanup result, only Merk-backed descendants are traversed, and the cleanup
  still clears non-Merk payloads and all indexed-axis namespaces at no extra
  storage reads. `GROVE_V1`..`GROVE_V3` keep the historical traversal, costs
  and failures. `GroveDBOperationsNonMerkTreeVersions` gains the
  `subtree_discovery` slot (#949, closes #890)
- `verify_query_get_parent_tree_info_with_options` no longer rejects every `SizedQuery::offset` before decoding the proof; it defers to the same envelope gate as `verify_query_with_options`, so an offset-paginated V1 count-offset proof over a `ProvableCountTree` / `ProvableCountSumTree` / `ProvableCountProvableSumTree` verifies through it (V0 envelopes still reject a nonzero offset, and `Some(0)` is no offset). Empty targets report their own zero aggregates instead of stale ancestor metadata or a missing-parent-info error. Absence-proof verification (`absence_proofs_for_non_existing_searched_keys`, the `*_with_absence_proof` entry points and the decoded `GroveDBProof` variants) now refuses a nonzero offset with `NotSupported` on every entry: the expected-key projection (`terminal_keys`) enumerates a range from its first key and a count-offset proof does not reveal which rows the offset skipped, so a V1 proof for the `offset 2, limit 5` page of `a..=e` reported the existing rows `a` and `b` as proven absent (#707)
- An add-on operation returned by a partial batch's callback (`apply_partial_batch*`) no longer discards the ancestor update the paused batch is still carrying, on `GROVE_V4` (`apply_batch.add_on_op_collision: 1`). The leftover root-level `ReplaceTreeRootKey` / `InsertTreeWithRootHash` for a modified child tree was silently replaced by an add-on op at the same path and key, so the child's writes were committed but its parent element was rewritten from the callback's bytes with no root key and a stale aggregate, orphaning the subtree. A colliding unconditional add-on insert is now merged with the pending root state only if it preserves the tree type; conditional inserts retain their error-or-skip semantics without changing pending flags or child state. A colliding delete is accepted only if the child ended the batch empty, a collision with a pending non-Merk root update is refused with `InvalidBatchOperation`, and an add-on op duplicating a still-unexecuted user op of the initial batch is refused by the consistency check (last op wins when that check is disabled, as within a single batch). `GROVE_V3` cannot compose these collisions and the cross-segment safety gate refuses them (#708)
- Count-offset paginated proofs (`SizedQuery::offset` over a `ProvableCountTree` / `ProvableCountSumTree` / `ProvableCountProvableSumTree`) no longer paginate by descendant totals. A nested count-bearing tree contributes its own aggregate count to the host (0 when empty) but is one row to ordinary pagination, so a collapsed `HashWithCount` over it consumed several rows of offset for one physical row and proved a different page than the trusted read, with a valid count commitment. The prover now walks every subtree it is about to collapse for offset and refuses the query unless each row contributes exactly one count unit, and refuses a non-unit row it descends through; Disjoint and past-limit collapses are unchanged. Proof format and verifier are unchanged and honest proofs are byte-identical, so no version gate. The verifier's `skipped` is documented as count units (#864)
- Ordinary (Item / Reference) replacements of a specialized value (a tree or a sum item) are charged from their own serialized bytes on `GROVE_V4` (`merk_versions.tree.put_value: 1`). Before, the node kept the predecessor's fixed `value_defined_cost`, so the replacement was charged as the predecessor's fixed size and the physical-size verification was skipped. Old-value removal accounting and the ordinary -> specialized direction are unchanged; grove v1..v3 keep the legacy charges (#908)
- V1 trunk and branch chunk proof generation reads the chunk, composite-row bindings, and ancestor layers from one snapshot, preventing concurrent commits from producing proofs with mismatched parent and child hashes (follow-up to #919).
- Trunk and branch chunk proofs (`prove_trunk_chunk` / `prove_branch_chunk`) bind every tree and reference row to the root hash on `GROVE_V4` (`proof.chunk_proof_row_binding: 1`). Released versions waived the value-hash check for any row that deserialized as a tree, so the returned tree metadata (type, aggregate count or sum, root key) was unbound and an item could be disguised as a tree under a genuine root hash; reference rows never verified. The prover now emits `KVValueHashFeatureTypeWithChildHash` for those rows and the verifier requires it. Chunks containing an indexed-tree row are refused, since no proof node carries its three-input binding (#859)
- A directly inserted reference whose chain runs back through the position being written is refused with `CyclicReference` on `GROVE_V4` (`add_element_on_transaction: 2`). Overwriting the item terminal of `A -> B` with a reference back to `A` was accepted — the chain resolved against the stale stored item — and committed a cycle that every later `get`, proof and `verify_grovedb` on either key failed on. `apply_batch` already refused it; `GROVE_V3` keeps the legacy outcome for replay
- Direct and batch references to `NonCounted`-wrapped items now commit to identical stored terminal bytes on `GROVE_V4` (`add_element_on_transaction: 2`). `verify_grovedb` and V1 proof generation select the terminal representation matching each reference's stored commitment, preserving older direct-write hashes across upgrades. Offset-paginated proof rows resolved through a reference may surface the wrapped target (`CountOffsetReturnedItem::resolved_from_reference`) (#858)
- Corrected proof verification logic in GroveDb (#371)
- Added ASCII check before appending string to hex display for better visualization (#376)

## [6.0.0] - 2026-09-08 [YANKED]

Do not use 6.0.0. It is superseded by 6.0.1 and should be treated as
withdrawn. Relative to 6.0.0, 6.0.1:

- Maintains backward references by default. In 6.0.0 maintenance was opt-in
  per call (`propagate_backward_references`, default off), so an ordinary
  insert, delete or batch over a referenced element left stale hashes and
  dangling registrations behind. 6.0.1 maintains the chains on every V4 write
  and replaces the flag with a per-operation `Check` / `DontCheck`
  declaration whose false `DontCheck` claims are refused (#951, #953).
- Fixes recursive discovery and deletion, which failed with a Merk decoding
  error on any tree holding a populated non-Merk descendant (#949, #890).
- Makes Git consumers resolve the published `grovedb-bincode` 2.1.0 crates,
  so their types share `Encode` / `Decode` trait identities with SDKs that use
  the registry crates (#948).
- Publishes the workspace crates through the release workflow in dependency
  order, skipping versions already on crates.io (#947). The 6.0.0 publish was
  partial: `grovedb` and `grovedb-merk` were never published at 6.0.0.

## Version History

For previous versions, see commit history.
