//! Every public operation of GroveDB, and whether the workloads produce it.
//!
//! The golden corpus and the differential fuzzer compare what the
//! generators produce and nothing else, so an operation no workload produces
//! is one whose cost, result and proof no test compares between two
//! engines. [`PUBLIC_OPERATIONS`] says of every public method of `GroveDb`,
//! and of the transaction it hands out, which of four things is true:
//!
//! - [`Status::Produced`]: [`super::apply()`] calls it, for the
//!   [`Operation`]s named, each of which the suite produces.
//! - [`Status::Inside`]: a method that is produced calls it.
//! - [`Status::Consumer`]: whoever applies a stream calls it, outside
//!   [`super::apply()`]: opening the database, proving a read, checking the
//!   result.
//! - [`Status::LeftOut`]: no stream reaches it, for the reason given.
//!
//! The list is checked against GroveDB's source by `tests/surface.rs`, which
//! reads the public methods from the source when it runs. A method added to
//! GroveDB fails that test until it is classified here, and a method listed
//! as produced fails it unless [`super::apply()`] calls it.

use std::fmt;

use super::coverage::Operation;

/// Whether the workloads produce a public operation of GroveDB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// [`super::apply()`] calls it when it applies these operations.
    Produced(&'static [Operation]),
    /// A method that is produced, or one the consumer calls, calls it.
    Inside {
        /// The method that calls it.
        caller: &'static str,
        /// What it can do that this caller never asks of it.
        unreached: Option<&'static str>,
    },
    /// The consumer of a stream calls it, for the reason given.
    Consumer(&'static str),
    /// No stream reaches it, for the reason given.
    LeftOut(&'static str),
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Produced(operations) => {
                write!(f, "produced as")?;
                for (index, operation) in operations.iter().enumerate() {
                    let separator = if index == 0 { " " } else { ", " };
                    write!(f, "{separator}{}", operation.name())?;
                }
                Ok(())
            }
            Status::Inside {
                caller,
                unreached: None,
            } => write!(f, "called by `{caller}`"),
            Status::Inside {
                caller,
                unreached: Some(unreached),
            } => write!(f, "called by `{caller}`; not reached: {unreached}"),
            Status::Consumer(reason) => write!(f, "called by the consumer of a stream: {reason}"),
            Status::LeftOut(reason) => write!(f, "left out: {reason}"),
        }
    }
}

const fn produced(operations: &'static [Operation]) -> Status {
    Status::Produced(operations)
}

const fn inside(caller: &'static str) -> Status {
    Status::Inside {
        caller,
        unreached: None,
    }
}

const fn inside_but(caller: &'static str, unreached: &'static str) -> Status {
    Status::Inside {
        caller,
        unreached: Some(unreached),
    }
}

const ESTIMATE: Status = Status::LeftOut(
    "estimates a cost from what the caller says the trees hold and reads no database; SEAM-08 \
     keeps the estimates free of the engine",
);

const VERIFIER: Status = Status::LeftOut(
    "checks proof bytes and reads no database, so an engine cannot change it; the acceptance \
     harness and the golden corpus call the verifier that matches each proof",
);

const RECEIVING: Status = Status::LeftOut(
    "the receiving side of state sync, which writes a second database; GROVE-07 tests it in \
     both directions between the engines",
);

const CHECKPOINT: Status = Status::LeftOut(
    "works on database directories, not on the state of one; TXN-06 and GROVE-08 build and \
     test checkpoints",
);

const DUMP_LOAD: Status = Status::LeftOut(
    "an escape hatch behind the `unsafe-dump-load` feature, which works on storage beneath \
     GroveDB's operations; GROVE-09 replaces the hatch",
);

const AGGREGATE_READER: Status = inside("run_path_query");

const WIDE_INDEXED_ENTRY: Status = Status::LeftOut(
    "writes an entry of an indexed tree with a 128-bit sum, which GroveDB #1062 added and no \
     workload produces yet; the BASE-01 follow-up that produces the two trees owes the call",
);

/// Every public method of `GroveDb`, the methods of the traits implemented
/// for it included, and every public method of the transaction it hands
/// out under the name `Transaction::…`, in alphabetical order.
pub const PUBLIC_OPERATIONS: &[(&str, Status)] = &[
    ("Transaction::commit", inside("commit_transaction")),
    (
        "Transaction::is_snapshot_read",
        produced(&[Operation::SnapshotRead]),
    ),
    ("Transaction::rollback", inside("rollback_transaction")),
    (
        "Transaction::rollback_to_savepoint",
        produced(&[Operation::SavepointRollback]),
    ),
    (
        "Transaction::set_savepoint",
        produced(&[
            Operation::Savepoint,
            Operation::NestedSavepoint,
            Operation::SavepointKept,
        ]),
    ),
    (
        "Transaction::snapshot_age",
        Status::LeftOut(
            "reads a clock: how long a snapshot has been held, which no stream can pin",
        ),
    ),
    ("add_average_case_get_cost", ESTIMATE),
    ("add_average_case_get_cost_engine_free", ESTIMATE),
    ("add_average_case_get_merk_at_path", ESTIMATE),
    ("add_average_case_get_merk_at_path_engine_free", ESTIMATE),
    ("add_average_case_get_raw_cost", ESTIMATE),
    ("add_average_case_get_raw_cost_engine_free", ESTIMATE),
    ("add_average_case_get_raw_tree_cost", ESTIMATE),
    ("add_average_case_get_raw_tree_cost_engine_free", ESTIMATE),
    ("add_average_case_has_raw_cost", ESTIMATE),
    ("add_average_case_has_raw_cost_engine_free", ESTIMATE),
    ("add_average_case_has_raw_tree_cost", ESTIMATE),
    ("add_average_case_has_raw_tree_cost_engine_free", ESTIMATE),
    (
        "add_delete_operations_for_delete_up_tree_while_empty",
        inside_but(
            "delete_up_tree_while_empty_with_sectional_storage",
            "a batch that already holds operations for it to add to",
        ),
    ),
    ("add_worst_case_get_cost", ESTIMATE),
    ("add_worst_case_get_cost_engine_free", ESTIMATE),
    ("add_worst_case_get_merk_at_path", ESTIMATE),
    ("add_worst_case_get_merk_at_path_engine_free", ESTIMATE),
    ("add_worst_case_get_raw_cost", ESTIMATE),
    ("add_worst_case_get_raw_cost_engine_free", ESTIMATE),
    ("add_worst_case_get_raw_tree_cost", ESTIMATE),
    ("add_worst_case_get_raw_tree_cost_engine_free", ESTIMATE),
    ("add_worst_case_has_raw_cost", ESTIMATE),
    ("add_worst_case_has_raw_cost_engine_free", ESTIMATE),
    (
        "apply_batch",
        produced(&[
            Operation::Batch,
            Operation::Patch,
            Operation::Move,
            Operation::FlatDropInBatch,
            Operation::DeleteUpTreeBuilt,
            Operation::DeleteBuilt,
        ]),
    ),
    (
        "apply_batch_with_element_flags_update",
        produced(&[
            Operation::FlagsBatch,
            Operation::FlagsHandoverBatch,
            Operation::FlagsHandover,
            Operation::FlagsFullHandover,
            Operation::FlagsGrowth,
            Operation::FlagsShrinkAcrossEpochs,
            Operation::FlagsSameSize,
            Operation::FlagsOwnerChange,
            Operation::FlagsRemoved,
            Operation::FlagsDelete,
        ]),
    ),
    (
        "apply_operations_without_batching",
        Status::LeftOut(
            "applies batch operations one at a time through the direct methods, which are \
             produced; GroveDB documents it as for tests and debugging, and Platform does not \
             call it",
        ),
    ),
    ("apply_partial_batch", produced(&[Operation::PartialBatch])),
    (
        "apply_partial_batch_with_element_flags_update",
        produced(&[
            Operation::FlagsPartialBatch,
            Operation::PartialFlagsGrowth,
            Operation::PartialFlagsShrinkAcrossEpochs,
            Operation::PartialFlagsSameSize,
            Operation::PartialFlagsOwnerChange,
            Operation::PartialFlagsHistoryDropped,
            Operation::PartialFlagsDelete,
        ]),
    ),
    ("average_case_delete_operation_for_delete", ESTIMATE),
    (
        "average_case_delete_operation_for_delete_engine_free",
        ESTIMATE,
    ),
    (
        "average_case_delete_operations_for_delete_up_tree_while_empty",
        ESTIMATE,
    ),
    (
        "average_case_delete_operations_for_delete_up_tree_while_empty_engine_free",
        ESTIMATE,
    ),
    ("average_case_for_get", ESTIMATE),
    ("average_case_for_get_raw", ESTIMATE),
    ("average_case_for_get_tree", ESTIMATE),
    ("average_case_for_has_raw", ESTIMATE),
    ("average_case_for_has_raw_tree", ESTIMATE),
    ("average_case_indexed_secondary_mirror", ESTIMATE),
    ("average_case_merk_delete_element", ESTIMATE),
    ("average_case_merk_delete_tree", ESTIMATE),
    ("average_case_merk_insert_element", ESTIMATE),
    ("average_case_merk_insert_tree", ESTIMATE),
    ("average_case_merk_patch_element", ESTIMATE),
    ("average_case_merk_replace_element", ESTIMATE),
    ("bulk_append", produced(&[Operation::BulkAppend])),
    ("bulk_chunk_count", produced(&[Operation::BulkChunkCount])),
    ("bulk_count", produced(&[Operation::BulkCount])),
    ("bulk_get_buffer", produced(&[Operation::BulkBuffer])),
    ("bulk_get_chunk", produced(&[Operation::BulkChunk])),
    ("bulk_get_range", produced(&[Operation::BulkRange])),
    ("bulk_get_value", produced(&[Operation::BulkValue])),
    (
        "check_subtree_exists_invalid_path",
        Status::LeftOut(
            "nothing in GroveDB or in Platform calls it; `is_empty_tree`, which is produced, \
             makes the same check through its private twin",
        ),
    ),
    ("clear_subtree", produced(&[Operation::ClearSubtree])),
    ("commit_session", RECEIVING),
    ("commit_transaction", produced(&[Operation::Commit])),
    (
        "commitment_tree_anchor",
        produced(&[Operation::CommitmentAnchor]),
    ),
    (
        "commitment_tree_count",
        produced(&[Operation::CommitmentCount]),
    ),
    (
        "commitment_tree_get_range",
        produced(&[Operation::CommitmentRange]),
    ),
    (
        "commitment_tree_get_value",
        produced(&[Operation::CommitmentValue]),
    ),
    (
        "commitment_tree_insert",
        produced(&[Operation::CommitmentInsert]),
    ),
    ("create_checkpoint", CHECKPOINT),
    (
        "delete",
        produced(&[
            Operation::Delete,
            Operation::TreeDeleteFilledInTransaction,
            Operation::TreeDeleteRewrittenInTransaction,
            Operation::TreeDeleteCommitted,
        ]),
    ),
    ("delete_aux", produced(&[Operation::AuxDelete])),
    ("delete_checkpoint", CHECKPOINT),
    ("delete_checkpoint_with_engine", CHECKPOINT),
    (
        "delete_from_count_indexed_tree",
        produced(&[Operation::IndexedDeleteCount]),
    ),
    (
        "delete_from_provable_big_sum_indexed_tree",
        WIDE_INDEXED_ENTRY,
    ),
    (
        "delete_from_provable_count_provable_big_sum_indexed_tree",
        WIDE_INDEXED_ENTRY,
    ),
    (
        "delete_from_provable_count_provable_sum_indexed_tree",
        produced(&[Operation::IndexedDeleteCountSum]),
    ),
    (
        "delete_from_provable_sum_indexed_tree",
        produced(&[Operation::IndexedDeleteSum]),
    ),
    (
        "delete_if_empty_tree",
        produced(&[Operation::DeleteIfEmptyTree]),
    ),
    (
        "delete_operation_for_delete_internal",
        produced(&[Operation::DeleteBuilt]),
    ),
    (
        "delete_operations_for_delete_up_tree_while_empty",
        produced(&[Operation::DeleteUpTreeBuilt]),
    ),
    (
        "delete_up_tree_while_empty",
        produced(&[Operation::DeleteUpTree]),
    ),
    (
        "delete_up_tree_while_empty_with_sectional_storage",
        produced(&[Operation::SectionalDeleteUpTree]),
    ),
    (
        "delete_with_sectional_storage_function",
        produced(&[Operation::SectionalDelete]),
    ),
    ("dense_tree_count", produced(&[Operation::DenseCount])),
    ("dense_tree_get", produced(&[Operation::DenseValue])),
    ("dense_tree_insert", produced(&[Operation::DenseInsert])),
    (
        "dense_tree_root_hash",
        produced(&[Operation::DenseRootHash]),
    ),
    ("drop_flat_subtree", produced(&[Operation::FlatDrop])),
    (
        "engine",
        Status::LeftOut(
            "hands out the engine the database runs on and does nothing to the database; what \
             an engine does is tested at the `Engine` seam",
        ),
    ),
    ("estimated_case_operations_for_batch", ESTIMATE),
    (
        "fetch_chunk",
        produced(&[
            Operation::RootChunk,
            Operation::RootTreeChunk,
            Operation::MerkChunk,
            Operation::MerkChunkBelowRoot,
            Operation::EntriesChunk,
        ]),
    ),
    (
        "find_subtrees",
        inside_but(
            "delete",
            "on the grove versions that delete through the third version of the internal \
             delete, which finds the subtrees itself",
        ),
    ),
    (
        "flush",
        Status::LeftOut(
            "forces what is committed to disk and changes no state, hash or cost; DEC-04 \
             settles when the engine syncs and TXN-07 tests it",
        ),
    ),
    (
        "flush_pending_prefix_drops",
        produced(&[Operation::PendingDropsFlush]),
    ),
    ("follow_reference", inside("get_caching_optional")),
    ("follow_reference_as_stored", inside("follow_reference")),
    (
        "from_engine",
        inside_but("open", "an engine the caller opened itself"),
    ),
    ("get", produced(&[Operation::Get])),
    ("get_aux", produced(&[Operation::AuxGet])),
    ("get_aux_by_key_prefix", produced(&[Operation::AuxByPrefix])),
    ("get_caching_optional", produced(&[Operation::GetUncached])),
    (
        "get_proved_path_query",
        produced(&[Operation::ProvedPathQuery]),
    ),
    (
        "get_raw",
        produced(&[
            Operation::GetRaw,
            Operation::MerkChunk,
            Operation::MerkChunkBelowRoot,
            Operation::EntriesChunk,
        ]),
    ),
    (
        "get_raw_caching_optional",
        produced(&[Operation::GetRawUncached]),
    ),
    ("get_raw_optional", produced(&[Operation::GetRawOptional])),
    (
        "get_raw_optional_caching_optional",
        produced(&[Operation::GetRawOptionalUncached]),
    ),
    ("has_incomplete_restore", RECEIVING),
    (
        "has_raw",
        produced(&[Operation::HasRaw, Operation::BlockRead]),
    ),
    ("ingest_subtree_sst", DUMP_LOAD),
    ("insert", produced(&[Operation::Insert])),
    (
        "insert_if_changed_value",
        produced(&[Operation::InsertIfChanged]),
    ),
    (
        "insert_if_not_exists",
        produced(&[Operation::InsertIfNotExists]),
    ),
    (
        "insert_if_not_exists_return_existing_element",
        produced(&[Operation::InsertIfNotExistsReturnExisting]),
    ),
    (
        "insert_into_count_indexed_tree",
        produced(&[Operation::IndexedInsertCount]),
    ),
    (
        "insert_into_provable_big_sum_indexed_tree",
        WIDE_INDEXED_ENTRY,
    ),
    (
        "insert_into_provable_count_provable_big_sum_indexed_tree",
        WIDE_INDEXED_ENTRY,
    ),
    (
        "insert_into_provable_count_provable_sum_indexed_tree",
        produced(&[Operation::IndexedInsertCountSum]),
    ),
    (
        "insert_into_provable_sum_indexed_tree",
        produced(&[Operation::IndexedInsertSum]),
    ),
    ("is_empty_tree", produced(&[Operation::IsEmptyTree])),
    ("mmr_tree_append", produced(&[Operation::MmrAppend])),
    ("mmr_tree_get_value", produced(&[Operation::MmrValue])),
    ("mmr_tree_leaf_count", produced(&[Operation::MmrLeafCount])),
    ("mmr_tree_root_hash", produced(&[Operation::MmrRootHash])),
    (
        "open",
        Status::Consumer("opens the database a stream is applied to; TXN-07 tests reopening one"),
    ),
    (
        "open_batch_transactional_merk_at_path",
        Status::LeftOut(
            "hands a Merk on RocksDB's storage types to the caller, who works on it outside \
             GroveDB; SEAM-07 removes the leak",
        ),
    ),
    ("open_checkpoint", CHECKPOINT),
    ("open_checkpoint_with_engine", CHECKPOINT),
    (
        "open_with_cidx_integrity_check",
        Status::LeftOut(
            "`open` followed by `verify_grovedb`, each of which the consumer of a stream \
             calls by itself",
        ),
    ),
    (
        "open_with_engine",
        inside_but("open", "an engine other than the default one"),
    ),
    (
        "private_document_store_count",
        produced(&[Operation::PrivateDocumentCount]),
    ),
    (
        "private_document_store_get_range",
        produced(&[Operation::PrivateDocumentRange]),
    ),
    (
        "private_document_store_get_value",
        produced(&[Operation::PrivateDocumentValue]),
    ),
    (
        "private_document_store_insert",
        produced(&[Operation::PrivateDocumentInsert]),
    ),
    ("prove_branch_chunk", produced(&[Operation::BranchChunk])),
    (
        "prove_branch_chunk_non_serialized",
        inside("prove_branch_chunk"),
    ),
    (
        "prove_bulk_position_range",
        produced(&[Operation::BulkPositionRangeProof]),
    ),
    (
        "prove_query",
        Status::Consumer(
            "proves every read a stream marks as proven, from committed state, once \
             `workload::apply` has answered it",
        ),
    ),
    ("prove_query_many", produced(&[Operation::ProveMany])),
    ("prove_query_non_serialized", inside("prove_query")),
    ("prove_trunk_chunk", produced(&[Operation::TrunkChunk])),
    (
        "prove_trunk_chunk_non_serialized",
        inside("prove_trunk_chunk"),
    ),
    ("put_aux", produced(&[Operation::AuxPut])),
    (
        "query",
        produced(&[
            Operation::Query,
            Operation::QueryOffset,
            Operation::ConditionalSubquery,
        ]),
    ),
    ("query_aggregate_big_sum", produced(&[Operation::BigSum])),
    (
        "query_aggregate_big_sum_per_key",
        produced(&[Operation::BigSumPerKey]),
    ),
    ("query_aggregate_count", AGGREGATE_READER),
    (
        "query_aggregate_count_and_big_sum",
        produced(&[Operation::CountAndBigSum]),
    ),
    (
        "query_aggregate_count_and_big_sum_per_key",
        produced(&[Operation::CountAndBigSumPerKey]),
    ),
    ("query_aggregate_count_and_sum", AGGREGATE_READER),
    ("query_aggregate_count_and_sum_per_key", AGGREGATE_READER),
    ("query_aggregate_count_per_key", AGGREGATE_READER),
    ("query_aggregate_sum", AGGREGATE_READER),
    ("query_aggregate_sum_per_key", AGGREGATE_READER),
    (
        "query_aggregate_sums",
        Status::LeftOut(
            "`query_aggregate_sums_with_options`, which is produced, with two of its four \
             options left at GroveDB's defaults; `workload::apply` spells every option out, \
             so that a changed default cannot change how a stream replays",
        ),
    ),
    (
        "query_aggregate_sums_with_options",
        produced(&[Operation::SumBudget]),
    ),
    (
        "query_encoded_many",
        Status::LeftOut(
            "deprecated, and Platform does not call it; it reads through the same private \
             reader as `query_many_raw`, which is produced, and then follows each row's \
             reference as `get` does",
        ),
    ),
    ("query_item_value", produced(&[Operation::ItemValues])),
    (
        "query_item_value_or_sum",
        produced(&[Operation::ItemValuesOrSums]),
    ),
    (
        "query_keys_optional",
        produced(&[Operation::KeysOptional, Operation::AbsenceProof]),
    ),
    ("query_many_raw", produced(&[Operation::ManyRaw])),
    (
        "query_raw",
        produced(&[
            Operation::QueryRaw,
            Operation::QueryOffset,
            Operation::ConditionalSubquery,
        ]),
    ),
    (
        "query_raw_keys_optional",
        produced(&[Operation::RawKeysOptional, Operation::AbsenceProof]),
    ),
    ("query_sums", produced(&[Operation::SumValues])),
    ("raw_storage", DUMP_LOAD),
    (
        "reconcile_indexed_tree_secondaries",
        produced(&[Operation::IndexedReconcile]),
    ),
    (
        "replace_subtree_root",
        Status::LeftOut(
            "takes the root hash of the subtree from its caller, which a stream of plain data \
             cannot name, and is an escape hatch behind the `unsafe-dump-load` feature; \
             GROVE-09 replaces the hatch",
        ),
    ),
    (
        "rollback_to_savepoint",
        Status::LeftOut(
            "`Transaction::rollback_to_savepoint` reached through the engine, which refuses it \
             while a write scope of the transaction is open. No stream opens a scope, so on \
             RocksDB it is the operation the streams reach through the transaction. An engine \
             with a transaction of its own has no other way to it: the BASE-01 follow-up owes \
             `workload::apply` the call",
        ),
    ),
    ("rollback_transaction", produced(&[Operation::Rollback])),
    ("root_hash", produced(&[Operation::RootChunk])),
    ("root_key", produced(&[Operation::RootTreeChunk])),
    (
        "run_path_query",
        produced(&[
            Operation::PathQueryRun,
            Operation::QueryOffset,
            Operation::CountOffsetPage,
            Operation::InstanceOffset,
            Operation::ConditionalSubquery,
            Operation::InstanceLimit,
            Operation::AggregateCount,
            Operation::AggregateSum,
            Operation::AggregateCountAndSum,
            Operation::AggregateCountPerKey,
            Operation::AggregateSumPerKey,
            Operation::AggregateCountAndSumPerKey,
            Operation::SumBudgetWindow,
            Operation::AxisRankedPage,
            Operation::AxisBounded,
            Operation::AxisRankOfKey,
            Operation::AxisAggregate,
            Operation::BranchedAxisRead,
        ]),
    ),
    (
        "set_savepoint",
        Status::LeftOut(
            "`Transaction::set_savepoint` reached through the engine, which refuses it while a \
             write scope of the transaction is open. No stream opens a scope, so on RocksDB it \
             is the operation the streams reach through the transaction. An engine with a \
             transaction of its own has no other way to it: the BASE-01 follow-up owes \
             `workload::apply` the call",
        ),
    ),
    (
        "start_snapshot_read_transaction",
        produced(&[Operation::SnapshotRead, Operation::SnapshotReadAfterCommit]),
    ),
    ("start_snapshot_syncing", RECEIVING),
    ("start_snapshot_syncing_with_mode", RECEIVING),
    ("start_syncing_session", RECEIVING),
    ("start_syncing_session_with_mode", RECEIVING),
    (
        "start_transaction",
        produced(&[Operation::Commit, Operation::Abandon]),
    ),
    (
        "start_visualizer",
        Status::LeftOut(
            "starts the web server of the debugger, behind the `grovedbg` feature; it serves \
             a person, not a caller",
        ),
    ),
    ("verify_aggregate_big_sum_query", VERIFIER),
    ("verify_aggregate_big_sum_query_per_key", VERIFIER),
    (
        "verify_aggregate_big_sum_query_per_key_up_to_limit",
        VERIFIER,
    ),
    ("verify_aggregate_count_and_big_sum_query", VERIFIER),
    ("verify_aggregate_count_and_big_sum_query_per_key", VERIFIER),
    (
        "verify_aggregate_count_and_big_sum_query_per_key_up_to_limit",
        VERIFIER,
    ),
    ("verify_aggregate_count_and_sum_query", VERIFIER),
    ("verify_aggregate_count_and_sum_query_per_key", VERIFIER),
    (
        "verify_aggregate_count_and_sum_query_per_key_up_to_limit",
        VERIFIER,
    ),
    ("verify_aggregate_count_query", VERIFIER),
    ("verify_aggregate_count_query_per_key", VERIFIER),
    ("verify_aggregate_count_query_per_key_up_to_limit", VERIFIER),
    ("verify_aggregate_sum_query", VERIFIER),
    ("verify_aggregate_sum_query_per_key", VERIFIER),
    ("verify_aggregate_sum_query_per_key_up_to_limit", VERIFIER),
    ("verify_branch_chunk_proof", VERIFIER),
    ("verify_bulk_position_range_proof", VERIFIER),
    (
        "verify_grovedb",
        Status::Consumer(
            "walks every subtree of the database a stream leaves and reports the hashes that \
             do not match; the acceptance harness runs it after each workload",
        ),
    ),
    ("verify_path_query", VERIFIER),
    ("verify_query", VERIFIER),
    ("verify_query_get_parent_tree_info", VERIFIER),
    ("verify_query_get_parent_tree_info_with_options", VERIFIER),
    ("verify_query_raw", VERIFIER),
    ("verify_query_with_absence_proof", VERIFIER),
    ("verify_query_with_chained_path_queries", VERIFIER),
    ("verify_query_with_options", VERIFIER),
    ("verify_subset_query", VERIFIER),
    ("verify_subset_query_get_parent_tree_info", VERIFIER),
    ("verify_subset_query_with_absence_proof", VERIFIER),
    ("verify_trunk_chunk_proof", VERIFIER),
    (
        "visualize",
        Status::LeftOut(
            "the `Visualize` trait's drawing of the whole database for a person to read; no \
             caller computes with it",
        ),
    ),
    (
        "visualize_verify_grovedb",
        Status::LeftOut(
            "`verify_grovedb`, which the consumer of a stream calls, with its report \
             formatted for a person",
        ),
    ),
    (
        "wipe",
        Status::LeftOut(
            "empties every column family with range deletes, outside any transaction and at \
             no cost; TXN-06 and TXN-07 specify and test it",
        ),
    ),
    ("worst_case_delete_operation_for_delete", ESTIMATE),
    (
        "worst_case_delete_operation_for_delete_engine_free",
        ESTIMATE,
    ),
    (
        "worst_case_delete_operations_for_delete_up_tree_while_empty",
        ESTIMATE,
    ),
    (
        "worst_case_delete_operations_for_delete_up_tree_while_empty_engine_free",
        ESTIMATE,
    ),
    ("worst_case_for_get", ESTIMATE),
    ("worst_case_for_get_raw", ESTIMATE),
    ("worst_case_for_has_raw", ESTIMATE),
    ("worst_case_merk_delete_element", ESTIMATE),
    ("worst_case_merk_delete_tree", ESTIMATE),
    ("worst_case_merk_insert_element", ESTIMATE),
    ("worst_case_merk_insert_tree", ESTIMATE),
    ("worst_case_merk_patch_element", ESTIMATE),
    ("worst_case_merk_replace_element", ESTIMATE),
];
