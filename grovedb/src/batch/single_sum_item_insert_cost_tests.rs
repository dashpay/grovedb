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

    use crate::{batch::GroveDbOp, tests::make_empty_grovedb, Element};

    #[test]
    fn test_batch_one_sum_item_insert_costs_match_non_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"sum_tree", Element::empty_sum_tree(), None, None)
            .unwrap()
            .expect("expected to insert sum tree");

        let non_batch_cost = db
            .insert(
                [b"sum_tree".as_slice()],
                b"key1",
                Element::new_sum_item(150),
                None,
                Some(&tx),
            )
            .cost;
        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::insert_op(
            vec![b"sum_tree".to_vec()],
            b"key1".to_vec(),
            Element::new_sum_item(150),
        )];
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(non_batch_cost.storage_cost, cost.storage_cost);
    }
}
