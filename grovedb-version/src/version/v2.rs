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

pub const GROVE_V2: GroveVersion = GroveVersion {
    protocol_version: 2,
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
            delete_tree_cleanup_type_source: 0,
            overwrite_indexed_cleanup_inspection: 0,
            keyless_op_cost_dispatch: 0,
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
            path_query_push: 0,
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
                add_element_on_transaction: 0,
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
                delete_internal_on_transaction: 0,
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
                prove_query_non_serialized: 0,
                prove_trunk_chunk: 0,
                prove_trunk_chunk_non_serialized: 0,
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
                terminal_non_merk_tree_child_hash: 0,
                axis_descent_in_v1_envelope: 0,
                sum_budget_in_v1_envelope: 0,
            },
            average_case: GroveDBOperationsAverageCaseVersions {
                add_average_case_get_merk_at_path: 0,
                average_case_merk_replace_tree: 1, // changed
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
                average_case_commitment_tree_insert: 0,
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
                worst_case_commitment_tree_insert: 0,
            },
            // PrivateDocumentStore is unavailable before GROVE_V4: every
            // slot is 0 and the operations fail closed.
            private_document_store: GroveDBOperationsPrivateDocumentStoreVersions {
                element_creation: 0,
                insert: 0,
                get_value: 0,
                count: 0,
            },
            // Flat-subtree drop (issue #848) is unavailable before
            // GROVE_V4: every slot is 0 and the operations fail closed.
            flat_drop: GroveDBOperationsFlatDropVersions {
                drop_flat_subtree: 0,
                batch_delete_tree_drop_flat: 0,
            },
        },
        aggregate_sum_path_query_methods: GroveDBAggregateSumPathQueryMethodVersions { merge: 0 },
        path_query_methods: GroveDBPathQueryMethodVersions {
            terminal_keys: 0,
            merge: 0,
            query_items_at_path: 0,
            should_add_parent_tree_at_path: 0,
            unified_read_mode: 0,
            per_instance_query_limits: 0,
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
        batch: MerkBatchVersions { commit: 0 },
        average_case_costs: MerkAverageCaseCostsVersions {
            add_average_case_merk_propagate: 1, // changed
            sum_tree_estimated_size: 1,         // changed
            // grove v2 is consensus-locked to the legacy unweighted
            // Mix average; v3 bumps this to 1 with the fixed formula.
            value_with_feature_and_flags_size: 0,
        },
        // See the comment in v1.rs — `prove_count_offset_on_range` is
        // not reachable from v2's prove path (V0 envelope rejects
        // offsets), but the version field is kept consistent so a
        // direct caller doesn't trip the version gate.
        proof: MerkProofVersions {
            prove_count_offset_on_range: 0,
        },
    },
    // MMR hash charges: the shipped accounting, which billed the
    // storage reads each operation performed but not the blake3 merges
    // those reads fed. Locked here — these versions are released and a
    // replayed block must be charged what it was admitted under.
    mmr_versions: MmrVersions {
        cost: MmrCostVersions {
            push: 0,
            get_root: 0,
            gen_proof: 0,
        },
    },
    // Compaction hash count: the shipped figure, which omits the peak
    // bagging a compaction's own `get_root` performs. Locked — the
    // shielded pool has been charged this since mainnet activation.
    bulk_append_tree_versions: BulkAppendTreeVersions {
        cost: BulkAppendTreeCostVersions {
            compaction_hash_count: 0,
            // Append storage accounting: the shipped figure, which bills
            // every data put (buffer slot, chunk blob, MMR node) as new
            // storage. Locked — released versions replay what they were
            // admitted under.
            append_storage_accounting: 0,
        },
    },
    // Frontier save: the shipped figure, which bills the rewritten frontier
    // (key and value) as new storage on every append. Locked for the same
    // reason.
    commitment_tree_versions: CommitmentTreeVersions {
        cost: CommitmentTreeCostVersions {
            frontier_save_storage_accounting: 0,
            // The frontier's actual per-position work. Locked.
            frontier_cost_model: 0,
        },
    },
    // Dense-buffer root maintenance: the shipped behaviour, which keeps no
    // intermediate hashes and re-derives the root from every filled position
    // on each insert. Locked — released versions replay what they were
    // admitted under.
    dense_tree_versions: DenseTreeVersions {
        root_maintenance: 0,
    },
};
