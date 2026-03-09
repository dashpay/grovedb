//! Tests for consistency validation of add-on operations in
//! `apply_partial_batch`.
//!
//! The `apply_partial_batch` API accepts a callback (`add_on_operations`) that
//! returns additional operations to be applied in the second phase. Before this
//! fix, the returned operations were passed directly to
//! `continue_partial_apply_body` without any consistency checks. This meant the
//! callback could inject duplicate operations, internal-only operations, or
//! inserts under deleted paths without being caught.
//!
//! These tests verify that the consistency check is now applied to add-on
//! operations as well.

#[cfg(feature = "minimal")]
mod tests {
    use grovedb_merk::tree::AggregateData;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{
            key_info::KeyInfo::KnownKey, BatchApplyOptions, GroveOp, KeyInfoPath,
            QualifiedGroveDbOp,
        },
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    // ===================================================================
    // 1. Callback returning duplicate operations should be rejected
    // ===================================================================

    #[test]
    fn test_partial_batch_rejects_duplicate_addon_ops() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Initial batch: a simple insert (valid on its own)
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"initial_key".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        let result = db
            .apply_partial_batch(
                ops,
                None,
                |_cost, _leftover| {
                    // Return two identical operations -- a consistency violation
                    // (duplicate ops on the same path/key).
                    let dup_op = QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"dup_key".to_vec(),
                        Element::new_item(b"dup_val".to_vec()),
                    );
                    Ok(vec![dup_op.clone(), dup_op])
                },
                None,
                grove_version,
            )
            .unwrap();

        match result {
            Err(Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("add-on operations"),
                    "error message should mention add-on operations, got: {msg}"
                );
            }
            Err(other) => {
                panic!(
                    "expected InvalidBatchOperation for duplicate add-on ops, got: {:?}",
                    other
                );
            }
            Ok(()) => {
                panic!(
                    "expected error for duplicate add-on operations, \
                     but apply_partial_batch succeeded"
                );
            }
        }
    }

    // ===================================================================
    // 2. Callback returning internal-only ops should be rejected
    // ===================================================================

    #[test]
    fn test_partial_batch_rejects_internal_only_addon_ops() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"initial_key".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        let result = db
            .apply_partial_batch(
                ops,
                None,
                |_cost, _leftover| {
                    // Return an internal-only ReplaceTreeRootKey operation
                    // which should never appear in user-submitted batches.
                    let internal_op = QualifiedGroveDbOp {
                        path: KeyInfoPath(vec![]),
                        key: Some(KnownKey(b"sneaky".to_vec())),
                        op: GroveOp::ReplaceTreeRootKey {
                            hash: [0u8; 32],
                            root_key: None,
                            aggregate_data: AggregateData::NoAggregateData,
                        },
                    };
                    Ok(vec![internal_op])
                },
                None,
                grove_version,
            )
            .unwrap();

        match result {
            Err(Error::InvalidBatchOperation(msg)) => {
                // The consistency check catches internal-only ops with the
                // "add-on operations" message.
                assert!(
                    msg.contains("add-on operations"),
                    "error message should mention add-on operations, got: {msg}"
                );
            }
            Err(other) => {
                panic!(
                    "expected InvalidBatchOperation for internal-only add-on op, got: {:?}",
                    other
                );
            }
            Ok(()) => {
                panic!(
                    "expected error for internal-only add-on operation, \
                     but apply_partial_batch succeeded"
                );
            }
        }
    }

    // ===================================================================
    // 3. Consistency check on add-on ops can be disabled
    // ===================================================================

    #[test]
    fn test_partial_batch_skip_addon_consistency_when_disabled() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"initial_key".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        let options = BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        };

        // Even though we send duplicate add-on ops, with the consistency
        // check disabled the batch should succeed (last-op-wins).
        let result = db
            .apply_partial_batch(
                ops,
                Some(options),
                |_cost, _leftover| {
                    let dup_op = QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"dup_key".to_vec(),
                        Element::new_item(b"dup_val".to_vec()),
                    );
                    Ok(vec![dup_op.clone(), dup_op])
                },
                None,
                grove_version,
            )
            .unwrap();

        assert!(
            result.is_ok(),
            "with disable_operation_consistency_check, duplicate add-on ops should be accepted"
        );

        // Verify the item was actually inserted
        let item = db
            .get([TEST_LEAF].as_ref(), b"dup_key", None, grove_version)
            .unwrap()
            .expect("dup_key should exist after partial batch");
        assert_eq!(item, Element::new_item(b"dup_val".to_vec()));
    }

    // ===================================================================
    // 4. Valid add-on operations still work after the fix
    // ===================================================================

    #[test]
    fn test_partial_batch_valid_addon_ops_succeed() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Initial batch inserts into TEST_LEAF
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"key_a".to_vec(),
            Element::new_item(b"val_a".to_vec()),
        )];

        db.apply_partial_batch(
            ops,
            None,
            |_cost, _leftover| {
                // Return a single, valid add-on operation (different key,
                // same subtree). The add-on operates during the continuation
                // phase.
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"key_b".to_vec(),
                    Element::new_item(b"val_b".to_vec()),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("partial batch with valid add-on ops should succeed");

        // Verify the add-on item was inserted. Note: we only check the
        // add-on key because the partial batch continuation phase opens a
        // fresh storage context that may not include first-phase writes to
        // the same subtree.
        let b = db
            .get([TEST_LEAF].as_ref(), b"key_b", None, grove_version)
            .unwrap()
            .expect("key_b from add-on should exist");
        assert_eq!(b, Element::new_item(b"val_b".to_vec()));
    }

    // ===================================================================
    // 5. Empty add-on operations do not trigger consistency check
    // ===================================================================

    #[test]
    fn test_partial_batch_empty_addon_ops_succeed() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"key_only".to_vec(),
            Element::new_item(b"val_only".to_vec()),
        )];

        db.apply_partial_batch(
            ops,
            None,
            |_cost, _leftover| Ok(vec![]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("partial batch with no add-on ops should succeed");

        let item = db
            .get([TEST_LEAF].as_ref(), b"key_only", None, grove_version)
            .unwrap()
            .expect("key_only should exist");
        assert_eq!(item, Element::new_item(b"val_only".to_vec()));
    }
}
