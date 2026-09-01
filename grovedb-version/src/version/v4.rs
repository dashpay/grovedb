//! Grove protocol version 4.
//!
//! Introduced behaviourally identical to [`GROVE_V3`](super::v3::GROVE_V3),
//! and no longer is — gates land here as they are written. Currently flipped:
//!
//! - `apply_batch.delete_tree_cleanup_type_source: 1` — a batch `DeleteTree`
//!   uses the stored element's ACTUAL type to select cleanup namespaces,
//!   rejecting a declared/stored mismatch that involves an indexed tree.
//!   V1..V3 keep taking the declared type at face value. The stored element
//!   comes from data the apply already loads (the emptiness pre-scan's own
//!   read, or the old value the merk delete surfaces through the old-value
//!   observer), so V4 charges exactly the V1..V3 cost — the gate exists
//!   because it flips an accepted/rejected outcome, not because of cost.
//!
//! - `apply_batch.overwrite_indexed_cleanup_inspection: 1` — a batch
//!   overwrite (with tree-override protection off, references included)
//!   classifies the element it displaces to detect an indexed tree being
//!   overwritten, scheduling its per-axis secondary storage for cleanup or
//!   refusing the ambiguous case. The old bytes come from the node the merk
//!   walk fetched anyway to rewrite the key, so — like the gate above —
//!   V1..V3 cost is charged exactly and only the accepted/rejected outcome
//!   is gated.
//!
//! - `proof.terminal_non_merk_tree_child_hash: 1` — a V1 proof that reports a
//!   `CommitmentTree` / `MmrTree` / `BulkAppendTree` /
//!   `DenseAppendOnlyFixedSizeTree` as a terminal result (query targets the
//!   tree element itself, no lower layer) carries the tree's state root in a
//!   `KVValueHashFeatureTypeWithChildHash` node, and the verifier requires it.
//!   V1..V3 emit a bare `KVValueHash`, which hashes only `(key, value_hash)`
//!   and so leaves the element bytes — including the entry count callers read
//!   — free for a prover to forge under a genuine root hash. Gated because it
//!   flips a rejected/accepted outcome and because deriving the state root
//!   costs the prover extra storage reads and hash calls.
//!
//! - `proof.axis_descent_in_v1_envelope: 1` — the V1 proof envelope carries
//!   axis-ordered descents into indexed trees
//!   (`ProofBytes::IndexedTreeAxisDescent`): a proof over the queried
//!   per-axis secondary in place of the primary descent, with the
//!   secondary-root attestation recomputed by the verifier rather than
//!   supplied raw. V1..V3 refuse the shape on both sides. Gated because it
//!   adds an acceptance rule to the live V1 envelope.
//!
//! - `proof.sum_budget_in_v1_envelope: 1` — the V1 proof envelope carries
//!   sum-budget windows (`ProofBytes::SumBudgetWindow`): an ordinary Merk
//!   proof over exactly the window the budget walk scanned, whose stop
//!   condition the verifier attests by replaying the engine's fold over the
//!   proved elements. V1..V3 refuse the shape on both sides. Gated because
//!   it adds an acceptance rule to the live V1 envelope.
//!
//! - `path_query_methods.terminal_keys: 1` — `PathQuery::terminal_keys`
//!   resolves conditional subquery branches per queried item (first matching
//!   conditional wins, default branch as fallback), so keys never selected by
//!   the query no longer contribute terminal keys (issue #689). V1..V3 keep
//!   the legacy walk that expands every conditional branch before the items.
//!   Gated because terminal keys shape the absence-proof result set assembled
//!   by verifiers and the `query_keys_optional` result set.
//!
//! - `element.path_query_push: 1` — the trusted (non-proof) query walk
//!   serves per-instance limits (`Query::limit`, "top k per parent") and
//!   reconciles subquery descents by total *consumed* budget (rows plus
//!   empty-subtree charges) instead of returned rows only, aligning the
//!   read path's global-limit accounting with the prover's shared-counter
//!   accounting for nested empty subtrees. It also fixes issue #690: an
//!   empty inner result eats a limit slot only when nothing was skipped.
//!   V1..V3 keep the legacy v0 accounting, where e.g. `limit=2,
//!   offset=1` can return a single element. Proof generation rejects
//!   non-zero offsets and never runs this path, so only trusted-read
//!   result sets are gated.
//!
//! - `path_query_methods.merge: 1` — `PathQuery::merge` requires every input
//!   to agree on `left_to_right` (typed error on conflict) and propagates the
//!   shared direction to the merged root, and merges limited path queries by
//!   *lifting*: an input's global `SizedQuery::limit` becomes its merged
//!   branch's per-instance cap (`Query::limit`) — exact, because the branch
//!   instance executes once — and per-instance limits ride along on their
//!   branches. Limits merge only as exclusive grafts: a limited input
//!   landing at the merged root, or two colliding limited branches, are
//!   refused with typed errors. V1..V3 keep the long-standing behavior (all
//!   limits refused; input directions dropped). Gated because merged queries
//!   feed proofs and both sides must re-derive the identical merged query.
//!
//! - `path_query_methods.unified_read_mode: 1` — `PathQuery` read modes
//!   (axis-ordered and sum-budget reads carried in `Query::read_mode`) are
//!   served by the unified dispatch (`run_path_query`, and the unified
//!   prove/verify as they land). V1..V3 reject any read-mode-bearing query
//!   with `NotSupported` at every entry point — those versions also reject
//!   the version-2 `Query` wire encoding outright, so the slot's `0` value
//!   is the in-process mirror of that fail-closed decode.
//!
//! - `path_query_methods.per_instance_query_limits: 1` — per-instance
//!   limits (`Query::limit`: a fresh result budget per execution instance
//!   of a query node, alongside the global `SizedQuery::limit`) are served
//!   on trusted reads. Proofs still reject them until the V1
//!   prover/verifier learn the accounting. V1..V3 reject any query
//!   carrying one with `NotSupported` at every entry point — those
//!   versions also reject the version-3 `Query` wire encoding outright,
//!   so the slot's `0` value is the in-process mirror of that fail-closed
//!   decode.
//!
//! - `operations.flat_drop.drop_flat_subtree: 1` and
//!   `operations.flat_drop.batch_delete_tree_drop_flat: 1` — the
//!   flat-subtree drop family (issue #848): `GroveDb::drop_flat_subtree`
//!   and the `SubelementsDeletionBehavior::DropFlat` batch behavior remove
//!   a populated subtree's element from its parent Merk in O(1) —
//!   no emptiness check, no content sweep — and stage the subtree's
//!   storage prefixes (primary plus, for indexed primaries, all three
//!   axis secondaries) in a durable redo record committed atomically with
//!   the delete. Reclamation happens outside consensus via DB-level range
//!   tombstones (immediately when GroveDB owns the transaction, at the
//!   next `flush_pending_prefix_drops` otherwise) and never contributes
//!   to the operation's returned cost. The caller declares the subtree
//!   contains no child subtrees; a false declaration leaks the children's
//!   storage (unreachable, invisible to hashes/proofs/sync) but never
//!   corrupts state. V1..V3 hold both slots at 0 and fail closed. Gated
//!   because the drop's cost class (O(1) versus O(contents)) and the
//!   accepted/rejected outcome for non-empty trees are both
//!   consensus-observable.
//!
//! - `apply_batch.keyless_op_cost_dispatch: 1` — keyless append-only ops
//!   (`CommitmentTreeInsert`, `MmrTreeAppend`, `BulkAppend`,
//!   `DenseTreeInsert`) reach the cost dispatch in the estimated-cost batch
//!   structure, filed under unique synthetic keys so every append is
//!   charged. V1..V3 silently skip them — the append estimates as free,
//!   the under-estimate behind issue #812's admission-control bypass —
//!   preserved so historical admission decisions replay identically. The
//!   apply path is unaffected on every version (preprocessing rewrites
//!   keyless ops before the batch structure is built).
//!
//! - `operations.average_case.average_case_commitment_tree_insert: 1` and
//!   `operations.worst_case.worst_case_commitment_tree_insert: 1` — the
//!   `CommitmentTreeInsert` estimation arms charge the depth-derived
//!   upper-bound model (full ommer cascade, dense-buffer recompute, epoch
//!   compaction, flags-bounded element load). V1..V3 keep the legacy
//!   constants (average-case 33 Sinsemilla / 554-byte frontier; worst-case
//!   64 / 1066 but no compaction), which are NOT upper bounds — preserved
//!   for replay only. Gated because downstream the estimate is the
//!   admission bound: raising it ungated would make already-committed
//!   shield transitions re-validate as under-funded and brick sync.
//!
//! - `bulk_append_tree_versions.cost.append_storage_accounting: 1` and
//!   `commitment_tree_versions.cost.frontier_save_storage_accounting: 1` —
//!   the append-only family (`BulkAppendTree`, `CommitmentTree`,
//!   `PrivateDocumentStore`) charges every append the FIXED per-append model
//!   (issue #822): the entry's long-term footprint as `added_bytes` — its
//!   share of the eventual chunk blob (plus the variable format's four-byte
//!   per-entry prefix unless the owner declared a fixed entry size, as the
//!   commitment tree and the private document store do) plus the epoch's
//!   share of the blob framing and MMR nodes — and churn as `replaced_bytes`
//!   — its buffer slot and path record (epoch 1 included, nothing read to
//!   size them) and its own bytes again as its part of the blob rewrite —
//!   plus the buffer's fixed root-maintenance model and the compaction's
//!   hashes and commit-time puts amortized as per-chunk bounds over the
//!   epoch (one blake3 and one seek per append at `chunk_power` ≥ 7). The
//!   compacting append writes the blob, the MMR nodes and the persisted MMR
//!   root (new key `r`) prepaid — `KeyValueStorageCost::prepaid()`, which
//!   the commit path bills no bytes and no seek — and is charged the same
//!   as any other append, seek count included; a reopened tree reads the
//!   persisted root instead of bagging the peaks' blobs. V1..V3 keep issuing every data put with no cost
//!   information, which bills key + value as new storage every time — ≈ 2×
//!   the bytes that persist, with the whole ≈ 630 KB blob landing on one
//!   append per epoch at `chunk_power` 11. Stored chunks, roots and proofs
//!   are identical under both; gated because the figures are fees.
//!
//! - `commitment_tree_versions.cost.frontier_cost_model: 1` — the Sinsemilla
//!   frontier is charged a fixed, depth-derived model on every append: 33
//!   Sinsemilla hashes (the 32-deep root walk plus the average ommer merge)
//!   and a 554-byte frontier (the average over the position space) loaded
//!   at open and replaced at save — instead of `32 + trailing_ones(position)`
//!   hashes and the position's actual serialized size. With the gates above,
//!   a `CommitmentTreeInsert` costs the same at every position; the only
//!   residual is the commit-time seek count of a compacting append's MMR
//!   puts (bounded, once per epoch). V1..V3 keep the actual figures; gated
//!   because the figures are fees.
//!
//! - `dense_tree_versions.root_maintenance: 1` — the dense fixed-sized Merkle
//!   tree (the buffer of `BulkAppendTree`, `CommitmentTree` and
//!   `PrivateDocumentStore`, and the `DenseAppendOnlyFixedSizeTree` element)
//!   writes one fixed-size path record per insert (key `b'h' || position`:
//!   the position's value hash and the node hash of every position on its
//!   ancestor path) and derives an insert's ancestor hashes from earlier
//!   inserts' records: O(height) reads and hashes, ONE record write, and a
//!   one-record read for the root. Every insert is charged a fixed,
//!   height-derived model (`v1_insert_model_cost`: the reads and hashes
//!   averaged over a full buffer, rounded up) plus its two
//!   position-independent puts, so the cost of an append no longer depends
//!   on the buffer position. V1..V3 keep no intermediate hashes and
//!   re-derive the root from every filled position on every insert —
//!   O(count) per insert, O(2^chunk_power) at the end of each epoch, which
//!   was the dominant per-append cost of the shielded pool at `chunk_power`
//!   11 (≈ 2k reads and ≈ 4k blake3 calls on the last insert of every
//!   epoch). Stored values, positions, proofs and roots are identical under
//!   both; a buffer filled under V1..V3 is caught up from its values by the
//!   V4 inserts that need it (read-only, billed the same model, over with
//!   the epoch the switch happened in). Gated because the work — and so the
//!   fee — moves, and because V4 writes keys V1..V3 never read.
//!
//! Note that `GroveVersion::latest()` resolves to this version, so anything
//! defaulting to "latest" — tests, benchmarks, tools — exercises every gate
//! listed above rather than V3 behaviour.
//!
//! It exists so that fixes which change released behaviour have somewhere to
//! land. GROVE_V3 is live, so a fix that alters an accepted/rejected outcome,
//! a committed root hash, or a tracked cost cannot be applied unconditionally
//! — nodes carrying it would diverge from nodes that do not. Such a fix bumps
//! the relevant slot here and branches on it, leaving V1..V3 untouched.
//!
//! When adding a gate, say what changes in the slot comment, the way
//! `add_element_on_transaction: 1` documents the layered-subtree switch in
//! v3.rs. `GroveVersion::latest()` returns the last entry of
//! `GROVE_VERSIONS`, so anything defaulting to "latest" starts exercising this
//! version as soon as it is registered.

use crate::version::grovedb_versions::GroveDBAggregateSumPathQueryMethodVersions;
use crate::version::{
    bulk_append_tree_versions::{BulkAppendTreeCostVersions, BulkAppendTreeVersions},
    commitment_tree_versions::{CommitmentTreeCostVersions, CommitmentTreeVersions},
    dense_tree_versions::DenseTreeVersions,
    grovedb_versions::{
        GroveDBApplyBatchVersions, GroveDBElementMethodVersions,
        GroveDBOperationsAverageCaseVersions, GroveDBOperationsDeleteUpTreeVersions,
        GroveDBOperationsDeleteVersions, GroveDBOperationsFlatDropVersions,
        GroveDBOperationsGetVersions, GroveDBOperationsIndexedAxisVersions,
        GroveDBOperationsInsertVersions, GroveDBOperationsPrivateDocumentStoreVersions,
        GroveDBOperationsProofVersions, GroveDBOperationsQueryVersions, GroveDBOperationsVersions,
        GroveDBOperationsWorstCaseVersions, GroveDBPathQueryMethodVersions, GroveDBQueryLimits,
        GroveDBReplicationVersions, GroveDBVersions,
    },
    merk_versions::{
        MerkAverageCaseCostsVersions, MerkBatchVersions, MerkProofVersions, MerkVersions,
    },
    mmr_versions::{MmrCostVersions, MmrVersions},
    GroveVersion,
};

pub const GROVE_V4: GroveVersion = GroveVersion {
    protocol_version: 4,
    grovedb_versions: GroveDBVersions {
        apply_batch: GroveDBApplyBatchVersions {
            apply_batch_structure: 0,
            apply_body: 0,
            continue_partial_apply_body: 0,
            apply_operations_without_batching: 0,
            apply_batch: 0,
            apply_partial_batch: 0,
            open_batch_transactional_merk_at_path: 0,
            open_batch_merk_at_path: 0,
            apply_batch_with_element_flags_update: 0,
            apply_partial_batch_with_element_flags_update: 0,
            estimated_case_operations_for_batch: 0,
            delete_tree_cleanup_type_source: 1,
            overwrite_indexed_cleanup_inspection: 1,
            keyless_op_cost_dispatch: 1,
        },
        element: GroveDBElementMethodVersions {
            delete: 0,
            delete_with_sectioned_removal_bytes: 0,
            delete_into_batch_operations: 0,
            element_at_key_already_exists: 0,
            get: 0,
            get_optional: 0,
            get_from_storage: 0,
            get_optional_from_storage: 1,
            get_with_absolute_refs: 0,
            get_value_hash: 0,
            get_specialized_cost: 0,
            value_defined_cost: 0,
            value_defined_cost_for_serialized_value: 0,
            specialized_costs_for_key_value: 0,
            required_item_space: 0,
            required_item_with_sum_item_space: 0,
            required_reference_with_sum_item_space: 0,
            insert: 0,
            insert_into_batch_operations: 0,
            insert_if_not_exists: 0,
            insert_if_not_exists_into_batch_operations: 0,
            insert_if_changed_value: 0,
            insert_if_changed_value_into_batch_operations: 0,
            insert_reference: 0,
            insert_reference_into_batch_operations: 0,
            insert_subtree: 0,
            insert_subtree_into_batch_operations: 0,
            get_query: 0,
            get_aggregate_sum_query: 0,
            get_query_values: 0,
            get_query_apply_function: 0,
            get_path_query: 0,
            get_sized_query: 0,
            get_aggregate_sum_query_apply_function: 0,
            // Bumped from 0 → 1: v1 no longer decrements the outer limit when
            // a subquery's emptiness was caused by offset skips rather than a
            // true no-match (issue #690), serves per-instance limits
            // (`Query::limit`) and reconciles subquery descents by total
            // consumed budget (rows plus empty-subtree charges) instead of
            // returned rows only — see the module-level doc above. v0 keeps
            // the legacy accounting for shipped grove versions.
            path_query_push: 1,
            aggregate_sum_path_query_push: 0,
            query_item: 0,
            basic_push: 0,
            basic_aggregate_sum_push: 0,
            serialize: 0,
            serialized_size: 0,
            deserialize: 0,
            get_with_value_hash: 0,
            insert_reference_if_changed_value: 0,
            aggregate_sum_query_item: 0,
        },
        operations: GroveDBOperationsVersions {
            get: GroveDBOperationsGetVersions {
                get: 0,
                get_caching_optional: 0,
                follow_reference: 0,
                get_raw: 0,
                get_raw_caching_optional: 0,
                get_raw_optional: 0,
                get_raw_optional_caching_optional: 0,
                has_raw: 0,
                check_subtree_exists_invalid_path: 0,
                average_case_for_has_raw: 0,
                average_case_for_has_raw_tree: 0,
                average_case_for_get_raw: 0,
                average_case_for_get: 0,
                average_case_for_get_tree: 0,
                worst_case_for_has_raw: 0,
                worst_case_for_get_raw: 0,
                worst_case_for_get: 0,
                is_empty_tree: 0,
                follow_reference_once: 0,
            },
            insert: GroveDBOperationsInsertVersions {
                insert: 0,
                insert_on_transaction: 0,
                insert_without_transaction: 0,
                // v1: non-batch insert writes CountSumTree / ProvableCountTree /
                // ProvableCountSumTree as layered subtrees, consistent with the
                // batch path. GROVE_V1 / GROVE_V2 keep v0 (Op::Put) to preserve
                // the protocol-v11 consensus root (testnet block 245,344).
                add_element_on_transaction: 1,
                add_element_without_transaction: 0,
                insert_if_not_exists: 0,
                insert_if_not_exists_return_existing_element: 0,
                insert_if_changed_value: 0,
            },
            delete: GroveDBOperationsDeleteVersions {
                delete: 0,
                clear_subtree: 0,
                delete_with_sectional_storage_function: 0,
                delete_if_empty_tree: 0,
                delete_if_empty_tree_with_sectional_storage_function: 0,
                delete_operation_for_delete_internal: 0,
                // v1: reuse the already-open parent Merk when deleting a
                // non-empty child tree instead of reopening the parent layer
                // with the child's tree type (issue #686). v0 (GROVE_V1..V3)
                // keeps the legacy reopen byte-for-byte for replay
                // compatibility.
                delete_internal_on_transaction: 1,
                delete_internal_without_transaction: 0,
                average_case_delete_operation_for_delete: 0,
                worst_case_delete_operation_for_delete: 0,
            },
            delete_up_tree: GroveDBOperationsDeleteUpTreeVersions {
                delete_up_tree_while_empty: 0,
                delete_up_tree_while_empty_with_sectional_storage: 0,
                delete_operations_for_delete_up_tree_while_empty: 0,
                add_delete_operations_for_delete_up_tree_while_empty: 0,
                average_case_delete_operations_for_delete_up_tree_while_empty: 0,
                worst_case_delete_operations_for_delete_up_tree_while_empty: 0,
            },
            query: GroveDBOperationsQueryVersions {
                query_encoded_many: 0,
                query_many_raw: 0,
                get_proved_path_query: 0,
                query: 0,
                query_item_value: 0,
                query_item_value_or_sum: 0,
                query_aggregate_sums: 0,
                query_aggregate_count_on_range: 0,
                query_aggregate_sum_on_range: 0,
                query_aggregate_count_and_sum_on_range: 0,
                query_sums: 0,
                query_raw: 0,
                query_keys_optional: 0,
                query_raw_keys_optional: 0,
                follow_element: 0,
                run_path_query: 0,
            },
            indexed_axis: GroveDBOperationsIndexedAxisVersions {
                read: 0,
                prove_single_path: 0,
                verify_single_path: 0,
            },
            proof: GroveDBOperationsProofVersions {
                prove_query: 0,
                prove_query_many: 0,
                prove_query_non_serialized: 1, // v1 supports MmrTree/BulkAppendTree proof generation
                prove_trunk_chunk: 1,
                prove_trunk_chunk_non_serialized: 1,
                prove_branch_chunk: 0,
                prove_branch_chunk_non_serialized: 0,
                prove_bulk_position_range: 0,
                verify_bulk_position_range_proof: 0,
                verify_query_with_options: 0,
                verify_query_raw: 0,
                verify_layer_proof: 0,
                verify_query: 0,
                verify_subset_query: 0,
                verify_query_with_absence_proof: 0,
                verify_subset_query_with_absence_proof: 0,
                verify_query_with_chained_path_queries: 0,
                verify_query_get_parent_tree_info_with_options: 0,
                terminal_non_merk_tree_child_hash: 1, // bind terminal non-Merk tree element bytes to the parent value_hash
                axis_descent_in_v1_envelope: 1, // axis-ordered descents in the V1 envelope (ReadMode::Axis)
                sum_budget_in_v1_envelope: 1, // sum-budget windows in the V1 envelope (ReadMode::SumBudget)
            },
            average_case: GroveDBOperationsAverageCaseVersions {
                add_average_case_get_merk_at_path: 0,
                average_case_merk_replace_tree: 1,
                average_case_merk_insert_tree: 0,
                average_case_merk_delete_tree: 0,
                average_case_merk_insert_element: 0,
                average_case_merk_replace_element: 0,
                average_case_merk_patch_element: 0,
                average_case_merk_delete_element: 0,
                add_average_case_has_raw_cost: 0,
                add_average_case_has_raw_tree_cost: 0,
                add_average_case_get_raw_cost: 0,
                add_average_case_get_raw_tree_cost: 0,
                add_average_case_get_cost: 0,
                average_case_commitment_tree_insert: 1,
            },
            worst_case: GroveDBOperationsWorstCaseVersions {
                add_worst_case_get_merk_at_path: 0,
                worst_case_merk_replace_tree: 0,
                worst_case_merk_insert_tree: 0,
                worst_case_merk_delete_tree: 0,
                worst_case_merk_insert_element: 0,
                worst_case_merk_replace_element: 0,
                worst_case_merk_patch_element: 0,
                worst_case_merk_delete_element: 0,
                add_worst_case_has_raw_cost: 0,
                add_worst_case_get_raw_tree_cost: 0,
                add_worst_case_get_raw_cost: 0,
                add_worst_case_get_cost: 0,
                worst_case_commitment_tree_insert: 1,
            },
            // PrivateDocumentStore activates in GROVE_V4.
            private_document_store: GroveDBOperationsPrivateDocumentStoreVersions {
                element_creation: 1,
                insert: 1,
                get_value: 1,
                count: 1,
            },
            // Flat-subtree drop (issue #848) activates in GROVE_V4.
            flat_drop: GroveDBOperationsFlatDropVersions {
                drop_flat_subtree: 1,
                batch_delete_tree_drop_flat: 1,
            },
        },
        aggregate_sum_path_query_methods: GroveDBAggregateSumPathQueryMethodVersions { merge: 0 },
        path_query_methods: GroveDBPathQueryMethodVersions {
            terminal_keys: 1, // per-item conditional resolution (V4+), see issue #689
            merge: 1,         // direction agreement + per-instance limit lifting (V4+)
            query_items_at_path: 0,
            should_add_parent_tree_at_path: 0,
            unified_read_mode: 1,
            per_instance_query_limits: 1, // Query::limit served on trusted reads (V4+)
        },
        replication: GroveDBReplicationVersions {
            get_subtrees_metadata: 0,
            fetch_chunk: 0,
            start_snapshot_syncing: 0,
            apply_chunk: 0,
        },
        query_limits: GroveDBQueryLimits {
            max_aggregate_sum_query_elements_scanned: 1024,
        },
    },
    merk_versions: MerkVersions {
        batch: MerkBatchVersions { commit: 1 }, // return accumulated batch costs
        average_case_costs: MerkAverageCaseCostsVersions {
            // Bumped from 1 → 2: v2 fixes the Mix-arm divisor on both
            // `replaced_bytes` and `storage_loaded_bytes`. v1 computed
            // `Σ(weight_i · cost_i) / (nodes_updated · total_weight)`
            // (off by a factor of `nodes_updated²` vs the equivalent
            // homogeneous-layer cost); v2 computes
            // `nodes_updated · Σ(weight_i · cost_i) / total_weight`.
            add_average_case_merk_propagate: 2,
            // Bumped from 1 → 2: v2 folds the four `Provable*` tree-type
            // weight fields on `EstimatedSumTrees::SomeSumTrees` into the
            // weighted-average formula. v0/v1 outputs are preserved for
            // already-shipped grove versions (v1 → v0 formula, v2 → v1
            // formula).
            sum_tree_estimated_size: 2,
            // Bumped from 0 → 1: v1 fixes the Mix-arm "average" to a
            // proper weighted average. v0 returned
            // `Σ size_i / Σ weight_i` (low-biased and ignored weights);
            // v1 returns `Σ (size_i · weight_i) / Σ weight_i`.
            value_with_feature_and_flags_size: 1,
        },
        proof: MerkProofVersions {
            // Initial implementation; introduced alongside the V1
            // proof envelope.
            prove_count_offset_on_range: 0,
        },
    },
    // MMR hash charges: one hash per blake3 merge actually computed —
    // `push` per collapsed peak, `get_root` and `gen_proof` per peak
    // folded during bagging. V1..V3 bill the reads but not these
    // merges. Roots and proofs are bit-identical across both versions;
    // only `hash_node_calls` differs.
    mmr_versions: MmrVersions {
        cost: MmrCostVersions {
            push: 1,
            get_root: 1,
            gen_proof: 1,
        },
    },
    // Compaction hash count: adds the peak-bagging merges the shipped
    // figure omitted, so a compacting append is charged the hashes it
    // actually performs. Chunk bytes and roots are unchanged.
    bulk_append_tree_versions: BulkAppendTreeVersions {
        cost: BulkAppendTreeCostVersions {
            compaction_hash_count: 1,
            // Append storage accounting: each entry's permanent bytes are
            // charged once (its chunk-blob share, at its own append); buffer
            // slot rewrites and the compaction blob are reported as
            // replacements of the bytes they supersede instead of as new
            // storage (issue #822). Stored bytes and roots are unchanged.
            append_storage_accounting: 1,
        },
    },
    // Frontier: the in-place rewrite of `__ct_data__` is reported as a
    // replacement, not new storage (issue #822), and — with the cost model —
    // every append is charged the fixed, depth-derived figure (33 Sinsemilla
    // hashes, a 554-byte frontier loaded and replaced) whatever its
    // position. Frontier bytes and anchors are unchanged.
    commitment_tree_versions: CommitmentTreeVersions {
        cost: CommitmentTreeCostVersions {
            frontier_save_storage_accounting: 1,
            frontier_cost_model: 1,
        },
    },
    // Dense-buffer root maintenance: per-position hash records, so an insert
    // reads, hashes and writes O(height) instead of walking every filled
    // position. Root hashes are unchanged; what moves is the work an append
    // performs and is charged, and the records written beside the values.
    dense_tree_versions: DenseTreeVersions {
        root_maintenance: 1,
    },
};
