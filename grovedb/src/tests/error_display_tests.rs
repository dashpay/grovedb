//! Tests for Error add_context method and From impls.
//!
//! Note: Display tests are omitted because thiserror's derive macro generates
//! Display impls that get inlined by LLVM, making them invisible to llvm-cov.

#[cfg(test)]
mod tests {
    use crate::error::{Error, GroveDbErrorExt};
    use crate::PathQuery;
    use grovedb_costs::CostsExt;
    use grovedb_merk::proofs::Query;

    // ---------------------------------------------------------------
    // add_context tests — each covers unique match arms in error.rs
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

    #[test]
    fn test_add_context_on_string_variants() {
        // NotSupported(String)
        {
            let mut e = Error::NotSupported("base".to_string());
            e.add_context("extra");
            match e {
                Error::NotSupported(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "NotSupported should have context appended"
                    );
                }
                _ => panic!("expected NotSupported"),
            }
        }

        // CorruptedStorage(String)
        {
            let mut e = Error::CorruptedStorage("base".to_string());
            e.add_context("extra");
            match e {
                Error::CorruptedStorage(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "CorruptedStorage should have context appended"
                    );
                }
                _ => panic!("expected CorruptedStorage"),
            }
        }

        // PathParentLayerNotFound(String)
        {
            let mut e = Error::PathParentLayerNotFound("base".to_string());
            e.add_context("extra");
            match e {
                Error::PathParentLayerNotFound(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "PathParentLayerNotFound should have context appended"
                    );
                }
                _ => panic!("expected PathParentLayerNotFound"),
            }
        }

        // PathKeyNotFound(String)
        {
            let mut e = Error::PathKeyNotFound("base".to_string());
            e.add_context("extra");
            match e {
                Error::PathKeyNotFound(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "PathKeyNotFound should have context appended"
                    );
                }
                _ => panic!("expected PathKeyNotFound"),
            }
        }

        // CorruptedReferencePathKeyNotFound(String)
        {
            let mut e = Error::CorruptedReferencePathKeyNotFound("base".to_string());
            e.add_context("extra");
            match e {
                Error::CorruptedReferencePathKeyNotFound(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "CorruptedReferencePathKeyNotFound should have context appended"
                    );
                }
                _ => panic!("expected CorruptedReferencePathKeyNotFound"),
            }
        }

        // CorruptedReferencePathNotFound(String)
        {
            let mut e = Error::CorruptedReferencePathNotFound("base".to_string());
            e.add_context("extra");
            match e {
                Error::CorruptedReferencePathNotFound(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "CorruptedReferencePathNotFound should have context appended"
                    );
                }
                _ => panic!("expected CorruptedReferencePathNotFound"),
            }
        }

        // CorruptedReferencePathParentLayerNotFound(String)
        {
            let mut e = Error::CorruptedReferencePathParentLayerNotFound("base".to_string());
            e.add_context("extra");
            match e {
                Error::CorruptedReferencePathParentLayerNotFound(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "CorruptedReferencePathParentLayerNotFound should have context appended"
                    );
                }
                _ => panic!("expected CorruptedReferencePathParentLayerNotFound"),
            }
        }

        // InvalidParentLayerPath(String)
        {
            let mut e = Error::InvalidParentLayerPath("base".to_string());
            e.add_context("extra");
            match e {
                Error::InvalidParentLayerPath(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "InvalidParentLayerPath should have context appended"
                    );
                }
                _ => panic!("expected InvalidParentLayerPath"),
            }
        }

        // InvalidPath(String)
        {
            let mut e = Error::InvalidPath("base".to_string());
            e.add_context("extra");
            match e {
                Error::InvalidPath(ref s) => {
                    assert_eq!(s, "base, extra", "InvalidPath should have context appended");
                }
                _ => panic!("expected InvalidPath"),
            }
        }

        // CorruptedPath(String)
        {
            let mut e = Error::CorruptedPath("base".to_string());
            e.add_context("extra");
            match e {
                Error::CorruptedPath(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "CorruptedPath should have context appended"
                    );
                }
                _ => panic!("expected CorruptedPath"),
            }
        }

        // DeleteUpTreeStopHeightMoreThanInitialPathSize(String)
        {
            let mut e = Error::DeleteUpTreeStopHeightMoreThanInitialPathSize("base".to_string());
            e.add_context("extra");
            match e {
                Error::DeleteUpTreeStopHeightMoreThanInitialPathSize(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "DeleteUpTreeStopHeightMoreThanInitialPathSize should have context appended"
                    );
                }
                _ => panic!("expected DeleteUpTreeStopHeightMoreThanInitialPathSize"),
            }
        }

        // JustInTimeElementFlagsClientError(String)
        {
            let mut e = Error::JustInTimeElementFlagsClientError("base".to_string());
            e.add_context("extra");
            match e {
                Error::JustInTimeElementFlagsClientError(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "JustInTimeElementFlagsClientError should have context appended"
                    );
                }
                _ => panic!("expected JustInTimeElementFlagsClientError"),
            }
        }

        // SplitRemovalBytesClientError(String)
        {
            let mut e = Error::SplitRemovalBytesClientError("base".to_string());
            e.add_context("extra");
            match e {
                Error::SplitRemovalBytesClientError(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "SplitRemovalBytesClientError should have context appended"
                    );
                }
                _ => panic!("expected SplitRemovalBytesClientError"),
            }
        }

        // ClientReturnedNonClientError(String)
        {
            let mut e = Error::ClientReturnedNonClientError("base".to_string());
            e.add_context("extra");
            match e {
                Error::ClientReturnedNonClientError(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "ClientReturnedNonClientError should have context appended"
                    );
                }
                _ => panic!("expected ClientReturnedNonClientError"),
            }
        }

        // PathNotFoundInCacheForEstimatedCosts(String)
        {
            let mut e = Error::PathNotFoundInCacheForEstimatedCosts("base".to_string());
            e.add_context("extra");
            match e {
                Error::PathNotFoundInCacheForEstimatedCosts(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "PathNotFoundInCacheForEstimatedCosts should have context appended"
                    );
                }
                _ => panic!("expected PathNotFoundInCacheForEstimatedCosts"),
            }
        }

        // CommitmentTreeError(String)
        {
            let mut e = Error::CommitmentTreeError("base".to_string());
            e.add_context("extra");
            match e {
                Error::CommitmentTreeError(ref s) => {
                    assert_eq!(
                        s, "base, extra",
                        "CommitmentTreeError should have context appended"
                    );
                }
                _ => panic!("expected CommitmentTreeError"),
            }
        }
    }

    // ---------------------------------------------------------------
    // add_context wildcard arm (line 197: `_ => {}`)
    // ---------------------------------------------------------------

    #[test]
    fn test_add_context_noop_on_non_string_variants() {
        // Each of these variants hits the `_ => {}` wildcard arm in add_context.
        // We verify that calling add_context does not alter the Display output.

        // Unit variants (no payload)
        {
            let mut e = Error::Infallible;
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "Infallible should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::CyclicReference;
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "CyclicReference should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::ReferenceLimit;
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "ReferenceLimit should be unchanged by add_context"
            );
        }

        // &'static str variants (not in the String match arm)
        {
            let mut e = Error::InvalidQuery("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "InvalidQuery should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::MissingParameter("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "MissingParameter should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::InvalidParameter("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "InvalidParameter should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::InvalidCodeExecution("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "InvalidCodeExecution should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::CorruptedCodeExecution("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "CorruptedCodeExecution should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::InvalidBatchOperation("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "InvalidBatchOperation should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::DeletingNonEmptyTree("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "DeletingNonEmptyTree should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::ClearingTreeWithSubtreesNotAllowed("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "ClearingTreeWithSubtreesNotAllowed should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::OverrideNotAllowed("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "OverrideNotAllowed should be unchanged by add_context"
            );
        }
        {
            let mut e = Error::CyclicError("original");
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "CyclicError should be unchanged by add_context"
            );
        }

        // Complex wrapper variants (also hit the wildcard arm)
        {
            let ver_err = grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                method: "test".to_string(),
                known_versions: vec![0],
                received: 1,
            };
            let mut e = Error::VersionError(ver_err);
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "VersionError should be unchanged by add_context"
            );
        }
        {
            let elem_err = grovedb_element::error::ElementError::WrongElementType("expected tree");
            let mut e = Error::ElementError(elem_err);
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "ElementError should be unchanged by add_context"
            );
        }
        {
            let query_err = grovedb_query::error::Error::InvalidOperation("bad op");
            let mut e = Error::QueryError(query_err);
            let before = e.to_string();
            e.add_context("extra");
            let after = e.to_string();
            assert_eq!(
                before, after,
                "QueryError should be unchanged by add_context"
            );
        }
    }

    // ---------------------------------------------------------------
    // GroveDbErrorExt on CostResult
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

    // ---------------------------------------------------------------
    // From impl tests
    // ---------------------------------------------------------------

    #[test]
    fn test_from_merk_error_variants() {
        // Verify each specifically-mapped variant produces the right grovedb Error
        let e: Error = grovedb_merk::error::Error::PathKeyNotFound("k".to_string()).into();
        assert!(
            matches!(e, Error::PathKeyNotFound(ref s) if s == "k"),
            "PathKeyNotFound should map directly"
        );

        let e: Error = grovedb_merk::error::Error::PathNotFound("p".to_string()).into();
        assert!(
            matches!(e, Error::PathNotFound(ref s) if s == "p"),
            "PathNotFound should map directly"
        );

        let e: Error = grovedb_merk::error::Error::PathParentLayerNotFound("pp".to_string()).into();
        assert!(
            matches!(e, Error::PathParentLayerNotFound(ref s) if s == "pp"),
            "PathParentLayerNotFound should map directly"
        );

        let elem_err = grovedb_element::error::ElementError::CorruptedData("elem".to_string());
        let e: Error = grovedb_merk::error::Error::ElementError(elem_err).into();
        assert!(
            matches!(e, Error::ElementError(_)),
            "ElementError should map directly"
        );

        let e: Error = grovedb_merk::error::Error::InvalidInputError("inp").into();
        assert!(
            matches!(e, Error::InvalidInput("inp")),
            "InvalidInputError should map to InvalidInput"
        );

        // Wildcard: variants not specifically matched become MerkError
        let e: Error = grovedb_merk::error::Error::CorruptedCodeExecution("merk corrupt").into();
        assert!(
            matches!(e, Error::MerkError(_)),
            "CorruptedCodeExecution should fall through to MerkError"
        );

        let e: Error = grovedb_merk::error::Error::CorruptedState("bad state").into();
        assert!(
            matches!(e, Error::MerkError(_)),
            "CorruptedState should fall through to MerkError"
        );

        let e: Error = grovedb_merk::error::Error::DivideByZero("div0").into();
        assert!(
            matches!(e, Error::MerkError(_)),
            "DivideByZero should fall through to MerkError"
        );
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
}
