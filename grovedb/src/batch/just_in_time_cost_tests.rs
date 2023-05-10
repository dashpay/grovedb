// MIT LICENSE
//
// Copyright (c) 2023 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! This tests just in time costs
//! Just in time costs modify the tree in the same batch

#[cfg(feature = "full")]
mod tests {
    use std::option::Option::None;

    use crate::{
        batch::GroveDbOp,
        reference_path::ReferencePathType::UpstreamFromElementHeightReference,
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element,
    };

    #[test]
    fn test_partial_costs_with_no_new_operations_are_same_as_apply_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(EMPTY_PATH, b"documents", Element::empty_tree(), None, None)
            .cost_as_result()
            .expect("expected to insert successfully");
        db.insert(EMPTY_PATH, b"balances", Element::empty_tree(), None, None)
            .cost_as_result()
            .expect("expected to insert successfully");
        let ops = vec![
            GroveDbOp::insert_op(
                vec![b"documents".to_vec()],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            GroveDbOp::insert_op(
                vec![b"documents".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"documents".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
            ),
        ];

        let full_cost = db
            .apply_batch(ops.clone(), None, Some(&tx))
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_root_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("expected to get root hash");

        db.get([b"documents".as_slice()].as_ref(), b"key2", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get([b"documents".as_slice()].as_ref(), b"key3", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
        )
        .unwrap()
        .expect("cannot get element");

        tx.rollback().expect("expected to rollback");

        let cost = db
            .apply_partial_batch(ops, None, |_cost, _left_over_ops| Ok(vec![]), Some(&tx))
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_partial_root_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("expected to get root hash");

        db.get([b"documents".as_slice()].as_ref(), b"key2", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get([b"documents".as_slice()].as_ref(), b"key3", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
        )
        .unwrap()
        .expect("cannot get element");

        assert_eq!(full_cost, cost);

        assert_eq!(apply_root_hash, apply_partial_root_hash);
    }

    #[test]
    fn test_partial_costs_with_add_balance_operations() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(EMPTY_PATH, b"documents", Element::empty_tree(), None, None)
            .cost_as_result()
            .expect("expected to insert successfully");
        db.insert(
            EMPTY_PATH,
            b"balances",
            Element::empty_sum_tree(),
            None,
            None,
        )
        .cost_as_result()
        .expect("expected to insert successfully");
        let ops = vec![
            GroveDbOp::insert_op(
                vec![b"documents".to_vec()],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            GroveDbOp::insert_op(
                vec![b"documents".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_op(
                vec![b"documents".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
            ),
        ];

        let full_cost = db
            .apply_batch(ops.clone(), None, Some(&tx))
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_root_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("expected to get root hash");

        db.get([b"documents".as_slice()].as_ref(), b"key2", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get([b"documents".as_slice()].as_ref(), b"key3", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
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
                    let new_ops = vec![GroveDbOp::insert_op(
                        vec![b"balances".to_vec()],
                        b"person".to_vec(),
                        Element::new_sum_item_with_flags(1000, Some([0, 1].to_vec())),
                    )];
                    Ok(new_ops)
                },
                Some(&tx),
            )
            .cost_as_result()
            .expect("expected to apply batch");

        let apply_partial_root_hash = db
            .root_hash(Some(&tx))
            .unwrap()
            .expect("expected to get root hash");

        db.get([b"documents".as_slice()].as_ref(), b"key2", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get([b"documents".as_slice()].as_ref(), b"key3", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        db.get(
            [b"documents".as_slice(), b"key3".as_slice()].as_ref(),
            b"key4",
            Some(&tx),
        )
        .unwrap()
        .expect("cannot get element");

        let balance = db
            .get([b"balances".as_slice()].as_ref(), b"person", Some(&tx))
            .unwrap()
            .expect("cannot get element");

        assert_eq!(
            balance.as_sum_item_value().expect("expected sum item"),
            1000
        );

        assert!(full_cost.storage_cost.added_bytes < cost.storage_cost.added_bytes);

        assert_ne!(apply_root_hash, apply_partial_root_hash);
    }
}
