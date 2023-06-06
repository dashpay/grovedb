// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
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

//! Tests

#[cfg(feature = "full")]
mod tests {

    use crate::{
        batch::GroveDbOp,
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element,
    };

    #[test]
    fn test_batch_one_deletion_sum_tree_costs_match_non_batch_on_transaction() {
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(EMPTY_PATH, b"key1", Element::empty_sum_tree(), None, None)
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::delete_tree_op(vec![], b"key1".to_vec(), false)];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_sum_item_costs_match_non_batch_on_transaction() {
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"sum_tree".as_slice(),
            Element::empty_sum_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("expected to insert sum tree");

        let insertion_cost = db
            .insert(
                [b"sum_tree".as_slice()].as_ref(),
                b"key1",
                Element::new_sum_item(15),
                None,
                None,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete([b"sum_tree".as_slice()].as_ref(), b"key1", None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");

        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::delete_op(
            vec![b"sum_tree".to_vec()],
            b"key1".to_vec(),
        )];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_sum_tree_with_flags_costs_match_non_batch_on_transaction() {
        let db = make_empty_grovedb();

        let insertion_cost = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_sum_tree_with_flags(Some(b"dog".to_vec())),
                None,
                None,
            )
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(EMPTY_PATH, b"key1", None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");

        assert_eq!(insertion_cost.storage_cost.added_bytes, 128);
        assert_eq!(
            insertion_cost.storage_cost.added_bytes,
            non_batch_cost
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        );

        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::delete_tree_op(vec![], b"key1".to_vec(), false)];
        let batch_cost = db
            .apply_batch(ops, None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, batch_cost.storage_cost);
    }
}
