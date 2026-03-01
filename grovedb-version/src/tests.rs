#[cfg(test)]
mod tests {
    use crate::error::GroveVersionError;
    use crate::version::grovedb_versions::*;
    use crate::version::merk_versions::*;
    use crate::version::v1::GROVE_V1;
    use crate::version::v2::GROVE_V2;
    use crate::version::{GroveVersion, GROVE_VERSIONS};
    use crate::{TryFromVersioned, TryIntoVersioned};

    // ── GroveVersionError tests ──────────────────────────────────────────

    #[test]
    fn error_unknown_version_mismatch_display() {
        let err = GroveVersionError::UnknownVersionMismatch {
            method: "test_method".to_string(),
            known_versions: vec![0, 1],
            received: 5,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("test_method"));
        assert!(msg.contains("5"));
    }

    #[test]
    fn error_version_not_active_display() {
        let err = GroveVersionError::VersionNotActive {
            method: "inactive_method".to_string(),
            known_versions: vec![0],
        };
        let msg = format!("{}", err);
        assert!(msg.contains("inactive_method"));
        assert!(msg.contains("not active"));
    }

    #[test]
    fn error_debug_formatting() {
        let err = GroveVersionError::UnknownVersionMismatch {
            method: "debug_test".to_string(),
            known_versions: vec![0],
            received: 99,
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("UnknownVersionMismatch"));
        assert!(debug.contains("debug_test"));
    }

    #[test]
    fn error_version_not_active_debug() {
        let err = GroveVersionError::VersionNotActive {
            method: "my_method".to_string(),
            known_versions: vec![0, 1, 2],
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("VersionNotActive"));
        assert!(debug.contains("my_method"));
    }

    // ── GroveVersion::first() and latest() ───────────────────────────────

    #[test]
    fn grove_version_first_returns_v1() {
        let first = GroveVersion::first();
        assert_eq!(first.protocol_version, GROVE_V1.protocol_version);
    }

    #[test]
    fn grove_version_latest_returns_v2() {
        let latest = GroveVersion::latest();
        assert_eq!(latest.protocol_version, GROVE_V2.protocol_version);
    }

    #[test]
    fn grove_versions_count() {
        assert_eq!(GROVE_VERSIONS.len(), 2);
    }

    #[test]
    fn grove_versions_ordered_by_protocol_version() {
        for window in GROVE_VERSIONS.windows(2) {
            assert!(window[0].protocol_version < window[1].protocol_version);
        }
    }

    // ── Version constant field differences (V1 vs V2) ────────────────────

    #[test]
    fn v1_protocol_version_is_zero() {
        assert_eq!(GROVE_V1.protocol_version, 0);
    }

    #[test]
    fn v2_protocol_version_is_one() {
        assert_eq!(GROVE_V2.protocol_version, 1);
    }

    #[test]
    fn v2_has_updated_get_optional_from_storage() {
        assert_eq!(
            GROVE_V1.grovedb_versions.element.get_optional_from_storage,
            0
        );
        assert_eq!(
            GROVE_V2.grovedb_versions.element.get_optional_from_storage,
            1
        );
    }

    #[test]
    fn v2_has_updated_average_case_merk_replace_tree() {
        assert_eq!(
            GROVE_V1
                .grovedb_versions
                .operations
                .average_case
                .average_case_merk_replace_tree,
            0
        );
        assert_eq!(
            GROVE_V2
                .grovedb_versions
                .operations
                .average_case
                .average_case_merk_replace_tree,
            1
        );
    }

    #[test]
    fn v2_has_updated_merk_average_case_costs() {
        assert_eq!(
            GROVE_V1
                .merk_versions
                .average_case_costs
                .add_average_case_merk_propagate,
            0
        );
        assert_eq!(
            GROVE_V2
                .merk_versions
                .average_case_costs
                .add_average_case_merk_propagate,
            1
        );
        assert_eq!(
            GROVE_V1
                .merk_versions
                .average_case_costs
                .sum_tree_estimated_size,
            0
        );
        assert_eq!(
            GROVE_V2
                .merk_versions
                .average_case_costs
                .sum_tree_estimated_size,
            1
        );
    }

    // ── Default trait for version structs ─────────────────────────────────

    #[test]
    fn grove_version_default() {
        let v = GroveVersion::default();
        assert_eq!(v.protocol_version, 0);
    }

    #[test]
    fn grovedb_versions_default() {
        let v = GroveDBVersions::default();
        assert_eq!(v.apply_batch.apply_batch, 0);
        assert_eq!(v.element.get, 0);
        assert_eq!(v.operations.get.get, 0);
    }

    #[test]
    fn merk_versions_default() {
        let v = MerkVersions::default();
        assert_eq!(v.average_case_costs.add_average_case_merk_propagate, 0);
        assert_eq!(v.average_case_costs.sum_tree_estimated_size, 0);
    }

    #[test]
    fn grovedb_operations_all_sub_structs_default() {
        let _ = GroveDBOperationsGetVersions::default();
        let _ = GroveDBOperationsInsertVersions::default();
        let _ = GroveDBOperationsDeleteVersions::default();
        let _ = GroveDBOperationsDeleteUpTreeVersions::default();
        let _ = GroveDBOperationsQueryVersions::default();
        let _ = GroveDBOperationsProofVersions::default();
        let _ = GroveDBOperationsAverageCaseVersions::default();
        let _ = GroveDBOperationsWorstCaseVersions::default();
        let _ = GroveDBPathQueryMethodVersions::default();
        let _ = GroveDBReplicationVersions::default();
        let _ = GroveDBApplyBatchVersions::default();
        let _ = GroveDBElementMethodVersions::default();
        let _ = MerkAverageCaseCostsVersions::default();
    }

    // ── Clone trait for version structs ───────────────────────────────────

    #[test]
    fn grove_version_clone() {
        let v = GroveVersion::latest();
        let cloned = v.clone();
        assert_eq!(cloned.protocol_version, v.protocol_version);
    }

    #[test]
    fn grovedb_versions_clone() {
        let v = GroveDBVersions::default();
        let cloned = v.clone();
        assert_eq!(cloned.element.get, v.element.get);
    }

    #[test]
    fn merk_versions_clone() {
        let v = MerkVersions::default();
        let cloned = v.clone();
        assert_eq!(
            cloned.average_case_costs.sum_tree_estimated_size,
            v.average_case_costs.sum_tree_estimated_size
        );
    }

    // ── Debug trait for version structs ───────────────────────────────────

    #[test]
    fn grove_version_debug() {
        let debug = format!("{:?}", GroveVersion::latest());
        assert!(debug.contains("GroveVersion"));
        assert!(debug.contains("protocol_version"));
    }

    #[test]
    fn grovedb_versions_debug() {
        let debug = format!("{:?}", GroveDBVersions::default());
        assert!(debug.contains("GroveDBVersions"));
    }

    // ── TryFromVersioned / TryIntoVersioned blanket impls ────────────────

    #[test]
    fn try_from_versioned_delegates_to_try_from() {
        let grove_version = GroveVersion::latest();
        let result: Result<u32, _> = TryFromVersioned::try_from_versioned(42u64, grove_version);
        assert_eq!(result.unwrap(), 42u32);
    }

    #[test]
    fn try_from_versioned_error_on_overflow() {
        let grove_version = GroveVersion::latest();
        let result: Result<u32, _> = TryFromVersioned::try_from_versioned(u64::MAX, grove_version);
        assert!(result.is_err());
    }

    #[test]
    fn try_into_versioned_delegates_to_try_from_versioned() {
        let grove_version = GroveVersion::latest();
        let result: Result<u32, _> = 100u64.try_into_versioned(grove_version);
        assert_eq!(result.unwrap(), 100u32);
    }

    #[test]
    fn try_into_versioned_error_on_overflow() {
        let grove_version = GroveVersion::latest();
        let result: Result<u32, _> = u64::MAX.try_into_versioned(grove_version);
        assert!(result.is_err());
    }

    // ── Macro tests: check_grovedb_v0 ────────────────────────────────────

    fn grovedb_v0_check(version: u16) -> Result<(), GroveVersionError> {
        check_grovedb_v0!("test_grovedb_v0", version);
        Ok(())
    }

    #[test]
    fn check_grovedb_v0_passes_on_zero() {
        assert!(grovedb_v0_check(0).is_ok());
    }

    #[test]
    fn check_grovedb_v0_fails_on_nonzero() {
        let err = grovedb_v0_check(1).unwrap_err();
        match err {
            GroveVersionError::UnknownVersionMismatch {
                method,
                known_versions,
                received,
            } => {
                assert_eq!(method, "test_grovedb_v0");
                assert_eq!(known_versions, vec![0]);
                assert_eq!(received, 1);
            }
            _ => panic!("Expected UnknownVersionMismatch"),
        }
    }

    #[test]
    fn check_grovedb_v0_fails_on_large_version() {
        assert!(grovedb_v0_check(u16::MAX).is_err());
    }

    // ── Macro tests: check_merk_v0 ──────────────────────────────────────

    fn merk_v0_check(version: u16) -> Result<(), GroveVersionError> {
        check_merk_v0!("test_merk_v0", version);
        Ok(())
    }

    #[test]
    fn check_merk_v0_passes_on_zero() {
        assert!(merk_v0_check(0).is_ok());
    }

    #[test]
    fn check_merk_v0_fails_on_nonzero() {
        let err = merk_v0_check(2).unwrap_err();
        match err {
            GroveVersionError::UnknownVersionMismatch {
                method,
                known_versions,
                received,
            } => {
                assert_eq!(method, "test_merk_v0");
                assert_eq!(known_versions, vec![0]);
                assert_eq!(received, 2);
            }
            _ => panic!("Expected UnknownVersionMismatch"),
        }
    }

    // ── Macro tests: check_grovedb_v0_or_v1 ─────────────────────────────

    fn grovedb_v0_or_v1_check(version: u16) -> Result<u16, GroveVersionError> {
        let v = check_grovedb_v0_or_v1!("test_v0_or_v1", version);
        Ok(v)
    }

    #[test]
    fn check_grovedb_v0_or_v1_passes_on_zero() {
        assert_eq!(grovedb_v0_or_v1_check(0).unwrap(), 0);
    }

    #[test]
    fn check_grovedb_v0_or_v1_passes_on_one() {
        assert_eq!(grovedb_v0_or_v1_check(1).unwrap(), 1);
    }

    #[test]
    fn check_grovedb_v0_or_v1_fails_on_two() {
        let err = grovedb_v0_or_v1_check(2).unwrap_err();
        match err {
            GroveVersionError::UnknownVersionMismatch {
                method,
                known_versions,
                received,
            } => {
                assert_eq!(method, "test_v0_or_v1");
                assert_eq!(known_versions, vec![0, 1]);
                assert_eq!(received, 2);
            }
            _ => panic!("Expected UnknownVersionMismatch"),
        }
    }

    #[test]
    fn check_grovedb_v0_or_v1_fails_on_max() {
        assert!(grovedb_v0_or_v1_check(u16::MAX).is_err());
    }

    // ── Macro tests: check_grovedb_v0_with_cost ──────────────────────────

    fn grovedb_v0_with_cost_check(
        version: u16,
    ) -> grovedb_costs::CostResult<(), GroveVersionError> {
        check_grovedb_v0_with_cost!("test_grovedb_v0_cost", version);
        grovedb_costs::CostContext {
            value: Ok(()),
            cost: Default::default(),
        }
    }

    #[test]
    fn check_grovedb_v0_with_cost_passes_on_zero() {
        let result = grovedb_v0_with_cost_check(0);
        assert!(result.value.is_ok());
    }

    #[test]
    fn check_grovedb_v0_with_cost_fails_on_nonzero() {
        let result = grovedb_v0_with_cost_check(3);
        assert!(result.value.is_err());
    }

    // ── Macro tests: check_merk_v0_with_cost ─────────────────────────────

    fn merk_v0_with_cost_check(version: u16) -> grovedb_costs::CostResult<(), GroveVersionError> {
        check_merk_v0_with_cost!("test_merk_v0_cost", version);
        grovedb_costs::CostContext {
            value: Ok(()),
            cost: Default::default(),
        }
    }

    #[test]
    fn check_merk_v0_with_cost_passes_on_zero() {
        let result = merk_v0_with_cost_check(0);
        assert!(result.value.is_ok());
    }

    #[test]
    fn check_merk_v0_with_cost_fails_on_nonzero() {
        let result = merk_v0_with_cost_check(7);
        assert!(result.value.is_err());
    }

    // ── Comprehensive version field verification ─────────────────────────

    #[test]
    fn v1_all_grovedb_operation_versions_are_zero() {
        let ops = &GROVE_V1.grovedb_versions.operations;
        assert_eq!(ops.get.get, 0);
        assert_eq!(ops.get.follow_reference, 0);
        assert_eq!(ops.get.is_empty_tree, 0);
        assert_eq!(ops.insert.insert, 0);
        assert_eq!(ops.insert.insert_if_changed_value, 0);
        assert_eq!(ops.delete.delete, 0);
        assert_eq!(ops.delete.clear_subtree, 0);
        assert_eq!(ops.delete_up_tree.delete_up_tree_while_empty, 0);
        assert_eq!(ops.query.query, 0);
        assert_eq!(ops.query.query_sums, 0);
        assert_eq!(ops.proof.prove_query, 0);
        assert_eq!(ops.proof.verify_query, 0);
        assert_eq!(ops.average_case.add_average_case_get_merk_at_path, 0);
        assert_eq!(ops.worst_case.add_worst_case_get_merk_at_path, 0);
    }

    #[test]
    fn v1_all_element_versions_are_zero() {
        let elem = &GROVE_V1.grovedb_versions.element;
        assert_eq!(elem.delete, 0);
        assert_eq!(elem.get, 0);
        assert_eq!(elem.insert, 0);
        assert_eq!(elem.serialize, 0);
        assert_eq!(elem.deserialize, 0);
        assert_eq!(elem.get_optional_from_storage, 0);
        assert_eq!(elem.insert_reference, 0);
        assert_eq!(elem.insert_subtree, 0);
    }

    #[test]
    fn v1_path_query_methods_all_zero() {
        let pq = &GROVE_V1.grovedb_versions.path_query_methods;
        assert_eq!(pq.terminal_keys, 0);
        assert_eq!(pq.merge, 0);
        assert_eq!(pq.query_items_at_path, 0);
        assert_eq!(pq.should_add_parent_tree_at_path, 0);
    }

    #[test]
    fn v1_replication_all_zero() {
        let rep = &GROVE_V1.grovedb_versions.replication;
        assert_eq!(rep.get_subtrees_metadata, 0);
        assert_eq!(rep.fetch_chunk, 0);
        assert_eq!(rep.start_snapshot_syncing, 0);
        assert_eq!(rep.apply_chunk, 0);
    }

    #[test]
    fn v1_apply_batch_all_zero() {
        let ab = &GROVE_V1.grovedb_versions.apply_batch;
        assert_eq!(ab.apply_batch_structure, 0);
        assert_eq!(ab.apply_body, 0);
        assert_eq!(ab.continue_partial_apply_body, 0);
        assert_eq!(ab.apply_operations_without_batching, 0);
        assert_eq!(ab.apply_batch, 0);
        assert_eq!(ab.apply_partial_batch, 0);
        assert_eq!(ab.open_batch_transactional_merk_at_path, 0);
        assert_eq!(ab.open_batch_merk_at_path, 0);
        assert_eq!(ab.apply_batch_with_element_flags_update, 0);
        assert_eq!(ab.apply_partial_batch_with_element_flags_update, 0);
        assert_eq!(ab.estimated_case_operations_for_batch, 0);
    }

    #[test]
    fn v2_unchanged_fields_remain_zero() {
        let v2 = &GROVE_V2;
        assert_eq!(v2.grovedb_versions.element.get, 0);
        assert_eq!(v2.grovedb_versions.element.insert, 0);
        assert_eq!(v2.grovedb_versions.element.delete, 0);
        assert_eq!(v2.grovedb_versions.operations.get.get, 0);
        assert_eq!(v2.grovedb_versions.operations.insert.insert, 0);
        assert_eq!(v2.grovedb_versions.operations.delete.delete, 0);
        assert_eq!(v2.grovedb_versions.operations.query.query, 0);
        assert_eq!(v2.grovedb_versions.operations.proof.prove_query, 0);
        assert_eq!(
            v2.grovedb_versions
                .operations
                .worst_case
                .add_worst_case_get_merk_at_path,
            0
        );
    }

    // ── Re-exported versioned_feature_core types ─────────────────────────

    #[test]
    fn feature_version_bounds_check_version() {
        use crate::version::FeatureVersionBounds;
        let bounds = FeatureVersionBounds {
            min_version: 0,
            max_version: 2,
            default_current_version: 1,
        };
        assert!(bounds.check_version(0));
        assert!(bounds.check_version(1));
        assert!(bounds.check_version(2));
        assert!(!bounds.check_version(3));
    }

    #[test]
    fn feature_version_bounds_default() {
        use crate::version::FeatureVersionBounds;
        let bounds = FeatureVersionBounds::default();
        assert_eq!(bounds.min_version, 0);
        assert_eq!(bounds.max_version, 0);
        assert_eq!(bounds.default_current_version, 0);
        assert!(bounds.check_version(0));
        assert!(!bounds.check_version(1));
    }
}
