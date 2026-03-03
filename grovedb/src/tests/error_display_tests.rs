//! Tests for Error Display/From impls and add_context method.

#[cfg(test)]
mod tests {
    use crate::error::{Error, GroveDbErrorExt};
    use crate::PathQuery;
    use grovedb_costs::CostsExt;
    use grovedb_merk::proofs::Query;

    // ---------------------------------------------------------------
    // Display tests: every variant must produce a non-empty string
    // ---------------------------------------------------------------

    #[test]
    fn test_display_infallible() {
        let e = Error::Infallible;
        let s = format!("{}", e);
        assert!(!s.is_empty(), "Display for Infallible should be non-empty");
        assert!(s.contains("infallible"), "expected 'infallible' in: {}", s);
    }

    #[test]
    fn test_display_cyclic_reference() {
        let e = Error::CyclicReference;
        let s = e.to_string();
        assert!(
            !s.is_empty(),
            "Display for CyclicReference should be non-empty"
        );
        assert!(
            s.contains("cyclic reference path"),
            "expected 'cyclic reference path' in: {}",
            s
        );
    }

    #[test]
    fn test_display_reference_limit() {
        let e = Error::ReferenceLimit;
        let s = e.to_string();
        assert!(
            s.contains("hops limit exceeded"),
            "expected 'hops limit exceeded' in: {}",
            s
        );
    }

    #[test]
    fn test_display_missing_reference() {
        let e = Error::MissingReference("ref_abc".to_string());
        let s = e.to_string();
        assert!(s.contains("ref_abc"), "expected payload in: {}", s);
        assert!(
            s.contains("missing reference"),
            "expected 'missing reference' in: {}",
            s
        );
    }

    #[test]
    fn test_display_internal_error() {
        let e = Error::InternalError("something broke".to_string());
        let s = e.to_string();
        assert!(s.contains("something broke"), "expected payload in: {}", s);
        assert!(
            s.contains("internal error"),
            "expected 'internal error' in: {}",
            s
        );
    }

    #[test]
    fn test_display_invalid_proof() {
        let pq = PathQuery::new_unsized(Default::default(), Query::new());
        let e = Error::InvalidProof(pq, "bad proof data".to_string());
        let s = e.to_string();
        assert!(s.contains("bad proof data"), "expected payload in: {}", s);
        assert!(
            s.contains("invalid proof"),
            "expected 'invalid proof' in: {}",
            s
        );
    }

    #[test]
    fn test_display_invalid_input() {
        let e = Error::InvalidInput("bad input");
        let s = e.to_string();
        assert!(s.contains("bad input"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_path_key_not_found() {
        let e = Error::PathKeyNotFound("key_x".to_string());
        let s = e.to_string();
        assert!(s.contains("key_x"), "expected payload in: {}", s);
        assert!(
            s.contains("path key not found"),
            "expected 'path key not found' in: {}",
            s
        );
    }

    #[test]
    fn test_display_path_not_found() {
        let e = Error::PathNotFound("/a/b/c".to_string());
        let s = e.to_string();
        assert!(s.contains("/a/b/c"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_path_parent_layer_not_found() {
        let e = Error::PathParentLayerNotFound("parent_layer".to_string());
        let s = e.to_string();
        assert!(s.contains("parent_layer"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_reference_path_key_not_found() {
        let e = Error::CorruptedReferencePathKeyNotFound("ref_key".to_string());
        let s = e.to_string();
        assert!(s.contains("ref_key"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_reference_path_not_found() {
        let e = Error::CorruptedReferencePathNotFound("ref_path".to_string());
        let s = e.to_string();
        assert!(s.contains("ref_path"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_reference_path_parent_layer_not_found() {
        let e = Error::CorruptedReferencePathParentLayerNotFound("ref_parent".to_string());
        let s = e.to_string();
        assert!(s.contains("ref_parent"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_invalid_parent_layer_path() {
        let e = Error::InvalidParentLayerPath("bad_parent".to_string());
        let s = e.to_string();
        assert!(s.contains("bad_parent"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_invalid_path() {
        let e = Error::InvalidPath("inv_path".to_string());
        let s = e.to_string();
        assert!(s.contains("inv_path"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_path() {
        let e = Error::CorruptedPath("bad_path".to_string());
        let s = e.to_string();
        assert!(s.contains("bad_path"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_invalid_query() {
        let e = Error::InvalidQuery("bad query");
        let s = e.to_string();
        assert!(s.contains("bad query"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_missing_parameter() {
        let e = Error::MissingParameter("param_x");
        let s = e.to_string();
        assert!(s.contains("param_x"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_invalid_parameter() {
        let e = Error::InvalidParameter("param_y");
        let s = e.to_string();
        assert!(s.contains("param_y"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_data() {
        let e = Error::CorruptedData("data is wrong".to_string());
        let s = e.to_string();
        assert!(s.contains("data is wrong"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_storage() {
        let e = Error::CorruptedStorage("storage broke".to_string());
        let s = e.to_string();
        assert!(s.contains("storage broke"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_invalid_code_execution() {
        let e = Error::InvalidCodeExecution("bad exec");
        let s = e.to_string();
        assert!(s.contains("bad exec"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_corrupted_code_execution() {
        let e = Error::CorruptedCodeExecution("corrupt exec");
        let s = e.to_string();
        assert!(s.contains("corrupt exec"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_invalid_batch_operation() {
        let e = Error::InvalidBatchOperation("bad batch");
        let s = e.to_string();
        assert!(s.contains("bad batch"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_delete_up_tree_stop_height() {
        let e = Error::DeleteUpTreeStopHeightMoreThanInitialPathSize("too tall".to_string());
        let s = e.to_string();
        assert!(s.contains("too tall"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_deleting_non_empty_tree() {
        let e = Error::DeletingNonEmptyTree("not empty");
        let s = e.to_string();
        assert!(s.contains("not empty"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_clearing_tree_with_subtrees() {
        let e = Error::ClearingTreeWithSubtreesNotAllowed("has subtrees");
        let s = e.to_string();
        assert!(s.contains("has subtrees"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_just_in_time_element_flags_client_error() {
        let e = Error::JustInTimeElementFlagsClientError("jit err".to_string());
        let s = e.to_string();
        assert!(s.contains("jit err"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_split_removal_bytes_client_error() {
        let e = Error::SplitRemovalBytesClientError("split err".to_string());
        let s = e.to_string();
        assert!(s.contains("split err"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_client_returned_non_client_error() {
        let e = Error::ClientReturnedNonClientError("non client".to_string());
        let s = e.to_string();
        assert!(s.contains("non client"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_override_not_allowed() {
        let e = Error::OverrideNotAllowed("no override");
        let s = e.to_string();
        assert!(s.contains("no override"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_path_not_found_in_cache() {
        let e = Error::PathNotFoundInCacheForEstimatedCosts("cache miss".to_string());
        let s = e.to_string();
        assert!(s.contains("cache miss"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_not_supported() {
        let e = Error::NotSupported("feature xyz".to_string());
        let s = e.to_string();
        assert!(s.contains("feature xyz"), "expected payload in: {}", s);
    }

    #[test]
    fn test_display_merk_error() {
        let merk_err = grovedb_merk::error::Error::InvalidInputError("merk bad");
        let e = Error::MerkError(merk_err);
        let s = e.to_string();
        assert!(!s.is_empty(), "Display for MerkError should be non-empty");
        assert!(s.contains("merk error"), "expected 'merk error' in: {}", s);
    }

    #[test]
    fn test_display_version_error() {
        let ver_err = grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
            method: "test_method".to_string(),
            known_versions: vec![0],
            received: 99,
        };
        let e = Error::VersionError(ver_err);
        let s = e.to_string();
        assert!(
            !s.is_empty(),
            "Display for VersionError should be non-empty"
        );
    }

    #[test]
    fn test_display_element_error() {
        let elem_err = grovedb_element::error::ElementError::WrongElementType("expected tree");
        let e = Error::ElementError(elem_err);
        let s = e.to_string();
        assert!(
            !s.is_empty(),
            "Display for ElementError should be non-empty"
        );
    }

    #[test]
    fn test_display_cyclic_error() {
        let e = Error::CyclicError("cycle detected");
        let s = e.to_string();
        assert!(
            s.contains("cyclic error"),
            "expected 'cyclic error' in: {}",
            s
        );
    }

    #[test]
    fn test_display_commitment_tree_error() {
        let e = Error::CommitmentTreeError("ct fail".to_string());
        let s = e.to_string();
        assert!(s.contains("ct fail"), "expected payload in: {}", s);
    }

    // ---------------------------------------------------------------
    // add_context tests
    // ---------------------------------------------------------------

    #[test]
    fn test_add_context_appends_to_string_variants() {
        let mut e = Error::CorruptedData("original".to_string());
        e.add_context("extra context");
        let s = e.to_string();
        assert!(
            s.contains("original, extra context"),
            "expected appended context in: {}",
            s
        );
    }

    #[test]
    fn test_add_context_appends_to_missing_reference() {
        let mut e = Error::MissingReference("ref".to_string());
        e.add_context("more info");
        let s = e.to_string();
        assert!(
            s.contains("ref, more info"),
            "expected appended context in: {}",
            s
        );
    }

    #[test]
    fn test_add_context_appends_to_internal_error() {
        let mut e = Error::InternalError("internal".to_string());
        e.add_context("ctx");
        match e {
            Error::InternalError(ref s) => {
                assert_eq!(s, "internal, ctx");
            }
            _ => panic!("expected InternalError"),
        }
    }

    #[test]
    fn test_add_context_appends_to_invalid_proof() {
        let pq = PathQuery::new_unsized(Default::default(), Query::new());
        let mut e = Error::InvalidProof(pq, "proof_msg".to_string());
        e.add_context("proof_ctx");
        match e {
            Error::InvalidProof(_, ref s) => {
                assert_eq!(s, "proof_msg, proof_ctx");
            }
            _ => panic!("expected InvalidProof"),
        }
    }

    #[test]
    fn test_add_context_appends_to_commitment_tree_error() {
        let mut e = Error::CommitmentTreeError("ct".to_string());
        e.add_context("details");
        match e {
            Error::CommitmentTreeError(ref s) => {
                assert_eq!(s, "ct, details");
            }
            _ => panic!("expected CommitmentTreeError"),
        }
    }

    #[test]
    fn test_add_context_noop_for_static_str_variants() {
        // Variants with &'static str cannot be mutated
        let mut e = Error::InvalidInput("original");
        e.add_context("should not appear");
        match e {
            Error::InvalidInput(s) => {
                assert_eq!(s, "original", "static str variants should be unchanged");
            }
            _ => panic!("expected InvalidInput"),
        }
    }

    #[test]
    fn test_add_context_noop_for_merk_error() {
        let merk_err = grovedb_merk::error::Error::InvalidInputError("merk");
        let mut e = Error::MerkError(merk_err);
        e.add_context("should not appear");
        // MerkError is in the wildcard arm, so context is not appended
        let s = e.to_string();
        assert!(
            !s.contains("should not appear"),
            "MerkError should not get context appended"
        );
    }

    #[test]
    fn test_add_context_multiple_calls() {
        let mut e = Error::PathNotFound("start".to_string());
        e.add_context("first");
        e.add_context("second");
        match e {
            Error::PathNotFound(ref s) => {
                assert_eq!(s, "start, first, second");
            }
            _ => panic!("expected PathNotFound"),
        }
    }

    // ---------------------------------------------------------------
    // GroveDbErrorExt on CostResult tests
    // ---------------------------------------------------------------

    #[test]
    fn test_cost_result_add_context_on_error() {
        let result: grovedb_costs::CostResult<(), Error> =
            Err(Error::CorruptedStorage("bad".to_string())).wrap_with_cost(Default::default());
        let updated = result.add_context("ctx");
        let err = updated.unwrap().expect_err("should be error");
        match err {
            Error::CorruptedStorage(s) => {
                assert_eq!(s, "bad, ctx");
            }
            _ => panic!("expected CorruptedStorage"),
        }
    }

    #[test]
    fn test_cost_result_add_context_on_ok() {
        let result: grovedb_costs::CostResult<i32, Error> =
            Ok(42).wrap_with_cost(Default::default());
        let updated = result.add_context("ctx");
        let val = updated
            .unwrap()
            .expect("should still be Ok after add_context on success");
        assert_eq!(val, 42);
    }

    // ---------------------------------------------------------------
    // From impl tests
    // ---------------------------------------------------------------

    #[test]
    fn test_from_infallible() {
        // We cannot actually create an Infallible value, but we can test
        // the type signature compiles by verifying the impl exists.
        fn _assert_from_impl(_: impl From<std::convert::Infallible>) {}
        _assert_from_impl(Error::Infallible);
    }

    #[test]
    fn test_from_merk_error_path_key_not_found() {
        let merk_err = grovedb_merk::error::Error::PathKeyNotFound("merk_key".to_string());
        let e: Error = merk_err.into();
        match e {
            Error::PathKeyNotFound(s) => assert_eq!(s, "merk_key"),
            _ => panic!("expected PathKeyNotFound, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_merk_error_path_not_found() {
        let merk_err = grovedb_merk::error::Error::PathNotFound("merk_path".to_string());
        let e: Error = merk_err.into();
        match e {
            Error::PathNotFound(s) => assert_eq!(s, "merk_path"),
            _ => panic!("expected PathNotFound, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_merk_error_path_parent_layer_not_found() {
        let merk_err =
            grovedb_merk::error::Error::PathParentLayerNotFound("merk_parent".to_string());
        let e: Error = merk_err.into();
        match e {
            Error::PathParentLayerNotFound(s) => assert_eq!(s, "merk_parent"),
            _ => panic!("expected PathParentLayerNotFound, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_merk_error_element_error() {
        let elem_err = grovedb_element::error::ElementError::CorruptedData("elem_bad".to_string());
        let merk_err = grovedb_merk::error::Error::ElementError(elem_err);
        let e: Error = merk_err.into();
        match e {
            Error::ElementError(_) => {} // correct mapping
            _ => panic!("expected ElementError, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_merk_error_invalid_input() {
        let merk_err = grovedb_merk::error::Error::InvalidInputError("merk_input");
        let e: Error = merk_err.into();
        match e {
            Error::InvalidInput(s) => assert_eq!(s, "merk_input"),
            _ => panic!("expected InvalidInput, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_merk_error_fallback_to_merk_error() {
        // Variants that don't have specific mappings should become MerkError
        let merk_err = grovedb_merk::error::Error::Overflow("too big");
        let e: Error = merk_err.into();
        match e {
            Error::MerkError(_) => {} // correct fallback
            _ => panic!("expected MerkError fallback, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_grove_version_error() {
        let ver_err = grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
            method: "do_thing".to_string(),
            known_versions: vec![0, 1],
            received: 42,
        };
        let e: Error = ver_err.into();
        match e {
            Error::VersionError(_) => {} // correct mapping
            _ => panic!("expected VersionError, got: {:?}", e),
        }
    }

    #[test]
    fn test_from_element_error() {
        let elem_err = grovedb_element::error::ElementError::WrongElementType("wrong type");
        let e: Error = elem_err.into();
        match e {
            Error::ElementError(_) => {} // correct mapping
            _ => panic!("expected ElementError, got: {:?}", e),
        }
    }

    // ---------------------------------------------------------------
    // Error is Debug
    // ---------------------------------------------------------------

    #[test]
    fn test_debug_impl() {
        let e = Error::CorruptedData("debug test".to_string());
        let dbg = format!("{:?}", e);
        assert!(
            !dbg.is_empty(),
            "Debug output should be non-empty for Error"
        );
        assert!(
            dbg.contains("CorruptedData"),
            "Debug should contain variant name"
        );
    }
}
