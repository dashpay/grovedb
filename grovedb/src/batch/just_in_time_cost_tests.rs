//! This tests just in time costs
//! Just in time costs modify the tree in the same batch

#[cfg(feature = "full")]
mod tests {
    use std::option::Option::None;

    use grovedb_costs::storage_cost::{
        removal::StorageRemovedBytes::BasicStorageRemoval,
        transition::OperationStorageTransitionType,
    };
    use grovedb_version::version::GroveVersion;
    use integer_encoding::VarInt;

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::{
            ReferencePathType, ReferencePathType::UpstreamFromElementHeightReference,
        },
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element,
    };

    #[test]
    fn test_partial_costs_with_no_new_operations_are_same_as_apply_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"documents",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .cost_as_result()
        .expect("expected to insert successfully");
        db.insert(
            EMPTY_PATH,
            b"balances",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .cost_as_result()
        .expect("expected to insert successfully");
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"documents".to_vec()],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"documents".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"documents".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
            ),
        ];

        let full_cost = db
            .apply_batch(ops.clone(), None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_root_hash = db
            .root_hash(Some(&tx), grove_version)
            .unwrap()
            .expect("expected to get root hash");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key2",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key3",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        tx.rollback().expect("expected to rollback");

        let cost = db
            .apply_partial_batch(
                ops,
                None,
                |_cost, _left_over_ops| Ok(vec![]),
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_partial_root_hash = db
            .root_hash(Some(&tx), grove_version)
            .unwrap()
            .expect("expected to get root hash");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key2",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key3",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        assert_eq!(full_cost, cost);

        assert_eq!(apply_root_hash, apply_partial_root_hash);
    }

    #[test]
    fn test_partial_costs_with_add_balance_operations() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"documents",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .cost_as_result()
        .expect("expected to insert successfully");
        db.insert(
            EMPTY_PATH,
            b"balances",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .cost_as_result()
        .expect("expected to insert successfully");
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"documents".to_vec()],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"documents".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"documents".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
            ),
        ];

        let full_cost = db
            .apply_batch(ops.clone(), None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_root_hash = db
            .root_hash(Some(&tx), grove_version)
            .unwrap()
            .expect("expected to get root hash");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key2",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key3",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        tx.rollback().expect("expected to rollback");

        let cost = db
            .apply_partial_batch(
                ops,
                None,
                |_cost, left_over_ops| {
                    assert!(left_over_ops.is_some());
                    assert_eq!(left_over_ops.as_ref().unwrap().len(), 1);
                    let ops_by_root_path = left_over_ops
                        .as_ref()
                        .unwrap()
                        .get(&0)
                        .expect("expected to have root path");
                    assert_eq!(ops_by_root_path.len(), 1);
                    let new_ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"balances".to_vec()],
                        b"person".to_vec(),
                        Element::new_sum_item_with_flags(1000, Some([0, 1].to_vec())),
                    )];
                    Ok(new_ops)
                },
                Some(&tx),
                grove_version,
            )
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_partial_root_hash = db
            .root_hash(Some(&tx), grove_version)
            .unwrap()
            .expect("expected to get root hash");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key2",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice()].as_ref(),
            b"key3",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        let balance = db
            .get(
                [b"balances".as_slice()].as_ref(),
                b"person",
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("cannot get element");

        assert_eq!(
            balance.as_sum_item_value().expect("expected sum item"),
            1000
        );

        assert!(full_cost.storage_cost.added_bytes < cost.storage_cost.added_bytes);

        assert_ne!(apply_root_hash, apply_partial_root_hash);
    }

    #[test]
    fn test_batch_root_one_update_tree_bigger_flags_with_reference() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(
            EMPTY_PATH,
            b"tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert tree");

        db.insert(
            EMPTY_PATH,
            b"refs",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert tree");

        db.insert(
            [b"tree".as_slice()].as_ref(),
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0, 0])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"tree".to_vec()],
                b"key1".to_vec(),
                Element::new_item_with_flags(b"value100".to_vec(), Some(vec![0, 1])),
            ),
            QualifiedGroveDbOp::insert_only_op(
                vec![b"refs".to_vec()],
                b"ref_key".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        b"tree".to_vec(),
                        b"key1".to_vec(),
                    ]),
                    Some(1),
                ),
            ),
        ];

        let _ = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |cost, old_flags, new_flags| match cost.transition_type() {
                    OperationStorageTransitionType::OperationUpdateBiggerSize => {
                        if new_flags[0] == 0 {
                            new_flags[0] = 1;
                            let new_flags_epoch = new_flags[1];
                            new_flags[1] = old_flags.unwrap()[1];
                            new_flags.push(new_flags_epoch);
                            new_flags.extend(cost.added_bytes.encode_var_vec());
                            assert_eq!(new_flags, &vec![1u8, 0, 1, 2]);
                            Ok(true)
                        } else {
                            assert_eq!(new_flags[0], 1);
                            Ok(false)
                        }
                    }
                    OperationStorageTransitionType::OperationUpdateSmallerSize => {
                        new_flags.extend(vec![1, 2]);
                        Ok(true)
                    }
                    _ => Ok(false),
                },
                |_flags, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                Some(&tx),
                grove_version,
            )
            .cost;

        let issues = db
            .visualize_verify_grovedb(Some(&tx), true, &Default::default())
            .unwrap();
        assert_eq!(
            issues.len(),
            0,
            "reference issue: {}",
            issues
                .iter()
                .map(|(hash, (a, b, c))| format!("{}: {} {} {}", hash, a, b, c))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
}
