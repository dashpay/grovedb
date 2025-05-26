mod tests {
    //! Query tests

    use std::ops::RangeFull;

    use grovedb_merk::proofs::{
        query::{QueryItem, SubqueryBranch},
        Query,
    };
    use grovedb_version::version::GroveVersion;
    use indexmap::IndexMap;
    use rand::random;
    use tempfile::TempDir;

    use crate::{
        batch::QualifiedGroveDbOp,
        operations::proof::GroveDBProof,
        query_result_type::{
            PathKeyOptionalElementTrio, QueryResultElement::PathKeyElementTrioResultItem,
            QueryResultElements, QueryResultType,
        },
        reference_path::ReferencePathType,
        tests::{
            common::compare_result_sets, make_deep_tree, make_empty_grovedb, make_test_grovedb,
            TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF,
        },
        Element, GroveDb, PathQuery, SizedQuery,
    };

    fn populate_tree_for_non_unique_range_subquery(db: &TempGroveDb, grove_version: &GroveVersion) {
        // Insert a couple of subtrees first
        for i in 1985u32..2000 {
            let i_vec = i.to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF].as_ref(),
                &i_vec,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");
            // Insert element 0
            // Insert some elements into subtree
            db.insert(
                [TEST_LEAF, i_vec.as_slice()].as_ref(),
                b"\0",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");

            for j in 100u32..150 {
                let mut j_vec = i_vec.clone();
                j_vec.append(&mut j.to_be_bytes().to_vec());
                db.insert(
                    [TEST_LEAF, i_vec.as_slice(), b"\0"].as_ref(),
                    &j_vec.clone(),
                    Element::new_item(j_vec),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("successful value insert");
            }
        }
    }

    fn populate_tree_for_non_unique_double_range_subquery(
        db: &TempGroveDb,
        grove_version: &GroveVersion,
    ) {
        // Insert a couple of subtrees first
        for i in 0u32..10 {
            let i_vec = i.to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF].as_ref(),
                &i_vec,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");
            // Insert element 0
            // Insert some elements into subtree
            db.insert(
                [TEST_LEAF, i_vec.as_slice()].as_ref(),
                b"a",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");

            for j in 25u32..50 {
                let j_vec = j.to_be_bytes().to_vec();
                db.insert(
                    [TEST_LEAF, i_vec.as_slice(), b"a"].as_ref(),
                    &j_vec,
                    Element::empty_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("successful value insert");

                // Insert element 0
                // Insert some elements into subtree
                db.insert(
                    [TEST_LEAF, i_vec.as_slice(), b"a", j_vec.as_slice()].as_ref(),
                    b"\0",
                    Element::empty_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("successful subtree insert");

                for k in 100u32..110 {
                    let k_vec = k.to_be_bytes().to_vec();
                    db.insert(
                        [TEST_LEAF, i_vec.as_slice(), b"a", &j_vec, b"\0"].as_ref(),
                        &k_vec.clone(),
                        Element::new_item(k_vec),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("successful value insert");
                }
            }
        }
    }

    fn populate_tree_by_reference_for_non_unique_range_subquery(
        db: &TempGroveDb,
        grove_version: &GroveVersion,
    ) {
        // This subtree will be holding values
        db.insert(
            [TEST_LEAF].as_ref(),
            b"\0",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        // This subtree will be holding references
        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert a couple of subtrees first
        for i in 1985u32..2000 {
            let i_vec = i.to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF, b"1"].as_ref(),
                &i_vec,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");
            // Insert element 0
            // Insert some elements into subtree
            db.insert(
                [TEST_LEAF, b"1", i_vec.as_slice()].as_ref(),
                b"\0",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");

            for j in 100u32..150 {
                let random_key = random::<[u8; 32]>();
                let mut j_vec = i_vec.clone();
                j_vec.append(&mut j.to_be_bytes().to_vec());

                // We should insert every item to the tree holding items
                db.insert(
                    [TEST_LEAF, b"\0"].as_ref(),
                    &random_key,
                    Element::new_item(j_vec.clone()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("successful value insert");

                db.insert(
                    [TEST_LEAF, b"1", i_vec.clone().as_slice(), b"\0"].as_ref(),
                    &random_key,
                    Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"\0".to_vec(),
                        random_key.to_vec(),
                    ])),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("successful value insert");
            }
        }
    }

    fn populate_tree_for_unique_range_subquery(db: &TempGroveDb, grove_version: &GroveVersion) {
        // Insert a couple of subtrees first
        for i in 1985u32..2000 {
            let i_vec = i.to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF].as_ref(),
                &i_vec,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");

            db.insert(
                [TEST_LEAF, &i_vec.clone()].as_ref(),
                b"\0",
                Element::new_item(i_vec),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful value insert");
        }
    }

    fn populate_tree_by_reference_for_unique_range_subquery(
        db: &TempGroveDb,
        grove_version: &GroveVersion,
    ) {
        // This subtree will be holding values
        db.insert(
            [TEST_LEAF].as_ref(),
            b"\0",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        // This subtree will be holding references
        db.insert(
            [TEST_LEAF].as_ref(),
            b"1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        for i in 1985u32..2000 {
            let i_vec = i.to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF, b"1"].as_ref(),
                &i_vec,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful subtree insert");

            // We should insert every item to the tree holding items
            db.insert(
                [TEST_LEAF, b"\0"].as_ref(),
                &i_vec,
                Element::new_item(i_vec.clone()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful value insert");

            // We should insert a reference to the item
            db.insert(
                [TEST_LEAF, b"1", i_vec.clone().as_slice()].as_ref(),
                b"\0",
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"\0".to_vec(),
                    i_vec.clone(),
                ])),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful value insert");
        }
    }

    fn populate_tree_for_unique_range_subquery_with_non_unique_null_values(
        db: &mut TempGroveDb,
        grove_version: &GroveVersion,
    ) {
        populate_tree_for_unique_range_subquery(db, grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            &[],
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF, &[]].as_ref(),
            b"\0",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert a couple of subtrees first
        for i in 100u32..200 {
            let i_vec = i.to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF, &[], b"\0"].as_ref(),
                &i_vec,
                Element::new_item(i_vec.clone()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("successful value insert");
        }
    }

    fn populate_tree_for_uneven_keys(db: &TempGroveDb, grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            "b".as_ref(),
            Element::new_item(1u8.to_be_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF].as_ref(),
            "ab".as_ref(),
            Element::new_item(2u8.to_be_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF].as_ref(),
            "x".as_ref(),
            Element::new_item(3u8.to_be_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF].as_ref(),
            &[3; 32],
            Element::new_item(4u8.to_be_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF].as_ref(),
            "k".as_ref(),
            Element::new_item(5u8.to_be_bytes().to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
    }

    fn populate_tree_create_two_by_two_hierarchy(db: &TempGroveDb) {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();

        let mut ops = Vec::new();

        // Loop from A to M
        for c in b'A'..=b'M' {
            let node = vec![c];
            let child1 = vec![c, b'1'];
            let child2 = vec![c, b'2'];

            // Insert the parent node as a tree
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                node.clone(),
                Element::new_tree(None),
            ));

            // Insert two children as items with their corresponding values
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone()],
                child1.clone(),
                Element::new_item(vec![1]), // A1, B1, etc. has a value of 1
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone()],
                child2.clone(),
                Element::new_item(vec![2]), // A2, B2, etc. has a value of 2
            ));
        }

        // Apply the batch of operations to the database
        let _ = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to create test data");
    }

    fn populate_tree_create_two_by_two_hierarchy_with_intermediate_value(db: &TempGroveDb) {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();

        let mut ops = Vec::new();

        // Loop from A to M
        for c in b'A'..=b'M' {
            let node = vec![c];
            let intermediate = vec![0];
            let child1 = vec![c, b'1'];
            let child2 = vec![c, b'2'];

            // Insert the parent node as a tree
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                node.clone(),
                Element::new_tree(None),
            ));

            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone()],
                intermediate.clone(),
                Element::new_tree(None),
            ));

            // Insert two children as items with their corresponding values
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone(), intermediate.clone()],
                child1.clone(),
                Element::new_item(vec![1]), // A1, B1, etc. has a value of 1
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone(), intermediate.clone()],
                child2.clone(),
                Element::new_item(vec![2]), // A2, B2, etc. has a value of 2
            ));
        }

        // Apply the batch of operations to the database
        let _ = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to create test data");
    }

    fn populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(
        db: &TempGroveDb,
    ) {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2
        let grove_version = GroveVersion::latest();

        let mut ops = Vec::new();

        // Loop from A to M
        for c in b'a'..=b'm' {
            let node = vec![c];
            let intermediate = vec![0];
            let intermediate_ref_1 = vec![1];
            let intermediate_ref_2 = vec![2];
            let child1 = vec![c.to_ascii_uppercase(), b'1'];
            let child2 = vec![c.to_ascii_uppercase(), b'2'];
            let child1ref = vec![b'r', b'e', b'f', c.to_ascii_uppercase(), b'1'];
            let child2ref = vec![b'r', b'e', b'f', c.to_ascii_uppercase(), b'2'];

            // Insert the parent node as a tree
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                node.clone(),
                Element::new_tree(None),
            ));

            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone()],
                intermediate.clone(),
                Element::new_tree(None),
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone()],
                intermediate_ref_1.clone(),
                Element::new_tree(None),
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone(), intermediate_ref_1.clone()],
                intermediate_ref_2.clone(),
                Element::new_tree(None),
            ));

            // Insert two children as items with their corresponding values
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone(), intermediate.clone()],
                child1.clone(),
                Element::new_item(vec![1]), // A1, B1, etc. has a value of 1
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![node.clone(), intermediate.clone()],
                child2.clone(),
                Element::new_item(vec![2]), // A2, B2, etc. has a value of 2
            ));

            // Insert the references
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    node.clone(),
                    intermediate_ref_1.clone(),
                    intermediate_ref_2.clone(),
                ],
                child1ref.clone(),
                Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                    1,
                    vec![intermediate.clone(), child1.clone()],
                )), // refA1
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    node.clone(),
                    intermediate_ref_1.clone(),
                    intermediate_ref_2.clone(),
                ],
                child2ref.clone(),
                Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                    1,
                    vec![intermediate.clone(), child2.clone()],
                )), // refA2
            ));
        }

        // Apply the batch of operations to the database
        let _ = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to create test data");
    }

    fn populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(
        db: &TempGroveDb,
    ) {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();

        let mut ops = Vec::new();

        let top_holder = vec![0];
        let top_ref = vec![1];

        ops.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            top_holder.clone(),
            Element::empty_tree(),
        ));
        ops.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            top_ref.clone(),
            Element::empty_tree(),
        ));

        // Loop from A to M
        for c in b'a'..=b'm' {
            let node = vec![c];
            let intermediate = vec![0];
            let child1 = vec![c.to_ascii_uppercase(), b'1'];
            let child2 = vec![c.to_ascii_uppercase(), b'2'];
            let child1ref = vec![b'r', b'e', b'f', c.to_ascii_uppercase(), b'1'];
            let child2ref = vec![b'r', b'e', b'f', c.to_ascii_uppercase(), b'2'];

            // Insert the parent node as a tree
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![top_ref.clone()],
                node.clone(),
                Element::new_tree(None),
            ));

            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![top_ref.clone(), node.clone()],
                intermediate.clone(),
                Element::new_tree(None),
            ));

            // Insert two children as items with their corresponding values
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![top_holder.clone()],
                child1.clone(),
                Element::new_item(vec![1]), // A1, B1, etc. has a value of 1
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![top_holder.clone()],
                child2.clone(),
                Element::new_item(vec![2]), // A2, B2, etc. has a value of 2
            ));

            // Insert the references
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![top_ref.clone(), node.clone(), intermediate.clone()],
                child1ref.clone(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    top_holder.clone(),
                    child1.clone(),
                ])), // refA1
            ));
            ops.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![top_ref.clone(), node.clone(), intermediate.clone()],
                child2ref.clone(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    top_holder.clone(),
                    child2.clone(),
                ])), // refA2
            ));
        }

        // Apply the batch of operations to the database
        let _ = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("expected to create test data");
    }

    #[test]
    fn test_get_correct_order() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_uneven_keys(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let query = Query::new_range_full();

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements, vec![vec![4], vec![2], vec![1], vec![5], vec![3]]);
    }

    #[test]
    /// Tests a range query with a non-unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a tree containing multiple subtrees and items, then performs a range query with a subquery that selects all items under a specific key. It asserts that the correct number of elements are returned, checks the first and last values, and verifies that the proof matches the expected root hash and result set.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range(1988_u32.to_be_bytes().to_vec()..1992_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 200);

        let mut first_value = 1988_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1991_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 200);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range query with a unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a tree structure where each key in the range 1985..2000 has a unique item at key `\0`.
    /// It then performs a range query for keys 1988..1992 with a subquery on `\0`, asserting that the correct elements are returned,
    /// and that the generated proof can be successfully verified against the database root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_query_with_unique_subquery();
    /// ```
    fn test_get_range_query_with_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let mut db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery(&mut db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range(1988_u32.to_be_bytes().to_vec()..1992_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 4);

        let first_value = 1988_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1991_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 4);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests unique range queries on a tree containing references, verifying that the correct elements are returned and that proof generation and verification yield consistent results.
    ///
    /// This test populates a subtree with references, performs a range query with a unique subquery key, and checks that the returned elements match the expected values. It also generates a proof for the query and verifies that the proof matches the database's root hash and yields the same result set.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_query_with_unique_subquery_on_references();
    /// ```
    fn test_get_range_query_with_unique_subquery_on_references() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_by_reference_for_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec(), b"1".to_vec()];
        let mut query = Query::new();
        query.insert_range(1988_u32.to_be_bytes().to_vec()..1992_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 4);

        let first_value = 1988_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1991_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 4);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range query with a unique subquery on a tree containing non-unique null values, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a structure where each key in a range has a unique value, and an additional subtree with non-unique null values is present. It performs a query with a subquery on the null key, checks that the correct number of elements and their values are returned, and verifies that the generated proof matches the database root hash and result set.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_query_with_unique_subquery_with_non_unique_null_values();
    /// ```
    fn test_get_range_query_with_unique_subquery_with_non_unique_null_values() {
        let grove_version = GroveVersion::latest();
        let mut db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery_with_non_unique_null_values(&mut db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_all();

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let mut subquery = Query::new();
        subquery.insert_all();

        query.add_conditional_subquery(
            QueryItem::Key(b"".to_vec()),
            Some(vec![b"\0".to_vec()]),
            Some(subquery),
        );

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 115);

        let first_value = 100_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1999_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 115);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests that a unique range query with a conditional subquery correctly ignores non-unique null values.
    ///
    /// This test populates a tree with unique range values and additional non-unique null values, then performs a query that excludes the null values using a conditional subquery. It verifies that only the expected unique values are returned and that the proof generated for the query is valid and matches the database root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_query_with_unique_subquery_ignore_non_unique_null_values();
    /// ```
    fn test_get_range_query_with_unique_subquery_ignore_non_unique_null_values() {
        let grove_version = GroveVersion::latest();
        let mut db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery_with_non_unique_null_values(&mut db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_all();

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let subquery = Query::new();

        // This conditional subquery expresses that we do not want to get values in ""
        // tree
        query.add_conditional_subquery(
            QueryItem::Key(b"".to_vec()),
            Some(vec![b"\0".to_vec()]), // We want to go into 0, but we don't want to get anything
            Some(subquery),
        );

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 15);

        let first_value = 1985_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1999_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 15);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range queries with non-unique subqueries, verifying that all expected elements are returned in order and that proof verification matches the query results.
    ///
    /// This test populates the database with a tree containing subtrees for keys 1985..2000, each with multiple items. It then performs an inclusive range query from 1988 to 1995, using a subquery to select all items under each matching subtree. The test asserts the correct number of elements, checks the first and last values, and verifies that the proof matches the query results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_inclusive_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_inclusive_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_inclusive(
            1988_u32.to_be_bytes().to_vec()..=1995_u32.to_be_bytes().to_vec(),
        );

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 400);

        let mut first_value = 1988_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1995_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 400);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range queries with non-unique subqueries on reference trees.
    ///
    /// Verifies that querying a tree containing references with an inclusive range and a non-unique subquery returns the expected number of elements, and that proof generation and verification yield consistent results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_inclusive_query_with_non_unique_subquery_on_references();
    /// ```
    fn test_get_range_inclusive_query_with_non_unique_subquery_on_references() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_by_reference_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec(), b"1".to_vec()];
        let mut query = Query::new();
        query.insert_range_inclusive(
            1988_u32.to_be_bytes().to_vec()..=1995_u32.to_be_bytes().to_vec(),
        );

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 400);

        let mut first_value = 1988_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        // using contains as the elements get stored at random key locations
        // hence impossible to predict the final location
        // but must exist
        assert!(elements.contains(&first_value));

        let mut last_value = 1995_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert!(elements.contains(&last_value));

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 400);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range queries with a unique subquery, verifying that the correct elements are returned and that proof generation and verification yield consistent results.
    ///
    /// This test populates the database with a tree containing unique items for keys 1985 to 2000, then performs an inclusive range query from 1988 to 1995 using a subquery key. It asserts that the correct number of elements are returned, checks the first and last values, and verifies that the proof matches the database root hash and yields the same result set.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_inclusive_query_with_unique_subquery();
    /// ```
    fn test_get_range_inclusive_query_with_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_inclusive(
            1988_u32.to_be_bytes().to_vec()..=1995_u32.to_be_bytes().to_vec(),
        );

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 8);

        let first_value = 1988_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1995_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 8);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range-from query with a non-unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a tree containing multiple subtrees and items, then performs a range-from query starting at key 1995 with a subquery that selects all items under each subtree. It asserts that the correct number of elements is returned, checks the first and last values, and verifies that the proof matches the query results and the database root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_from_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_from_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_from(1995_u32.to_be_bytes().to_vec()..);

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 250);

        let mut first_value = 1995_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1999_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 250);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range-from query with a unique subquery, verifying correct retrieval and proof verification.
    ///
    /// This test populates the database with a tree containing unique items keyed by years, then performs a range-from query starting at 1995 with a subquery on key `\0`. It asserts that the correct elements are returned, verifies the first and last values, and checks that the generated proof matches the database root hash and result set.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_from_query_with_unique_subquery();
    /// ```
    fn test_get_range_from_query_with_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_from(1995_u32.to_be_bytes().to_vec()..);

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 5);

        let first_value = 1995_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1999_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 5);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range-to query with a non-unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates a tree with multiple subtrees and items, performs a range-to query up to a specified key with a subquery that selects all items, and asserts that the correct number of elements are returned in the expected order. It also generates and verifies a proof for the query, ensuring the result set matches the direct query output and the root hash is correct.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_to_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_to_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_to(..1995_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 500);

        let mut first_value = 1985_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1994_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 500);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range-to query with a unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a tree containing unique items, performs a range-to query up to a specified key using a subquery, and asserts that the correct elements are returned. It also generates and verifies a proof for the query, ensuring the result set matches the expected elements and the root hash is correct.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_to_query_with_unique_subquery();
    /// ```
    fn test_get_range_to_query_with_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_to(..1995_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 10);

        let first_value = 1985_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1994_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 10);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range-to queries with non-unique subqueries in GroveDB.
    ///
    /// This test populates the database with a tree containing subtrees for keys 1985 to 2000, each with multiple items. It then performs a query for all items in subtrees up to and including key 1995, using a non-unique subquery. The test verifies the number of returned elements, checks the first and last values, and confirms that proof generation and verification yield consistent results.
    fn test_get_range_to_inclusive_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_to_inclusive(..=1995_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 550);

        let mut first_value = 1985_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1995_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 550);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests an inclusive range-to query with a non-unique subquery where the upper bound key is out of the populated range.
    ///
    /// Verifies that all elements up to the highest existing key are returned, checks the order and values of the first and last elements, and ensures proof verification matches the query results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_to_inclusive_query_with_non_unique_subquery_and_key_out_of_bounds();
    /// ```
    fn test_get_range_to_inclusive_query_with_non_unique_subquery_and_key_out_of_bounds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new_with_direction(false);
        query.insert_range_to_inclusive(..=5000_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new_with_direction(false);
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 750);

        let mut first_value = 1999_u32.to_be_bytes().to_vec();
        first_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1985_u32.to_be_bytes().to_vec();
        last_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 750);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range queries with a unique subquery, verifying that the correct elements are returned and that proof verification matches the query results.
    ///
    /// This test populates the database with a tree containing unique items, performs an inclusive range query up to a specified key using a subquery, and asserts that the returned elements and proof verification are consistent.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_to_inclusive_query_with_unique_subquery();
    /// ```
    fn test_get_range_to_inclusive_query_with_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_to_inclusive(..=1995_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 11);

        let first_value = 1985_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1995_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 11);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range-after query with a non-unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a tree containing subtrees keyed by years and items with composite keys. It then performs a range-after query starting from key 1995, using a subquery to retrieve all items under each matching subtree. The test asserts the number of elements, checks the first and last values, and verifies that the proof matches the query results and root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_after_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_after_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_after(1995_u32.to_be_bytes().to_vec()..);

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 200);

        let mut first_value = 1996_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1999_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 200);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests a range-after-to query with a non-unique subquery, verifying correct element retrieval and proof verification.
    ///
    /// This test populates the database with a tree containing non-unique subqueries, performs a range-after-to query for keys between 1995 and 1997, and asserts that the correct number of elements are returned in the expected order. It also verifies that the generated proof matches the database root hash and that the result set from proof verification matches the original query results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_after_to_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_after_to_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_after_to(
            1995_u32.to_be_bytes().to_vec()..1997_u32.to_be_bytes().to_vec(),
        );

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 50);

        let mut first_value = 1996_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1996_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 50);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range-after-to queries with non-unique subqueries, verifying that the correct number of elements are returned and that proof verification matches the query results.
    ///
    /// This test populates the database with a tree containing subtrees for keys in the range 1985..2000, each with multiple items. It then performs a range-after-to-inclusive query from key 1995 to 1997 (inclusive), using a subquery that selects all items under the subtree at key `\0`. The test asserts that 100 elements are returned, checks the first and last values, and verifies that the proof matches the query results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_after_to_inclusive_query_with_non_unique_subquery();
    /// ```
    fn test_get_range_after_to_inclusive_query_with_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_after_to_inclusive(
            1995_u32.to_be_bytes().to_vec()..=1997_u32.to_be_bytes().to_vec(),
        );

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 100);

        let mut first_value = 1996_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1997_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 100);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests an inclusive range-after-to query with a non-unique subquery where the upper bound key is out of range.
    ///
    /// Verifies that querying a tree with a range starting after key 1995 up to and including key 5000, using a non-unique subquery, returns the expected number of elements and correct ordering. Also checks that the generated proof is valid and matches the database root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_after_to_inclusive_query_with_non_unique_subquery_and_key_out_of_bounds();
    /// ```
    fn test_get_range_after_to_inclusive_query_with_non_unique_subquery_and_key_out_of_bounds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new_with_direction(false);
        query.insert_range_after_to_inclusive(
            1995_u32.to_be_bytes().to_vec()..=5000_u32.to_be_bytes().to_vec(),
        );

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new_with_direction(false);
        subquery.insert_all();

        query.set_subquery_key(subquery_key);
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 200);

        let mut first_value = 1999_u32.to_be_bytes().to_vec();
        first_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1996_u32.to_be_bytes().to_vec();
        last_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 200);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests inclusive range queries with double non-unique subqueries, verifying correct retrieval and proof verification.
    ///
    /// This test constructs a tree with three levels of non-unique keys, performs an inclusive range query with nested subqueries, and asserts that the correct number of elements are returned in the expected order. It also verifies that the generated proof matches the database root hash and that the result set from proof verification matches the direct query result.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_inclusive_query_with_double_non_unique_subquery();
    /// ```
    fn test_get_range_inclusive_query_with_double_non_unique_subquery() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_double_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new();
        query.insert_range_inclusive(3u32.to_be_bytes().to_vec()..=4u32.to_be_bytes().to_vec());

        query.set_subquery_key(b"a".to_vec());

        let mut subquery = Query::new();
        subquery
            .insert_range_inclusive(29u32.to_be_bytes().to_vec()..=31u32.to_be_bytes().to_vec());

        subquery.set_subquery_key(b"\0".to_vec());

        let mut subsubquery = Query::new();
        subsubquery.insert_all();

        subquery.set_subquery(subsubquery);

        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 60);

        let first_value = 100_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 109_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 60);
        compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests range queries with various combinations of limits and offsets, verifying correct ordering, element counts, and proof verification for both non-unique and unique subqueries.
    ///
    /// This test covers ascending and descending queries, applies different limits and offsets, and checks that the returned elements and proofs match expectations. It also verifies behavior when limits or offsets exceed the number of available elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_get_range_query_with_limit_and_offset();
    /// ```
    fn test_get_range_query_with_limit_and_offset() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        populate_tree_for_non_unique_range_subquery(&db, grove_version);

        let path = vec![TEST_LEAF.to_vec()];
        let mut query = Query::new_with_direction(true);
        query.insert_range(1990_u32.to_be_bytes().to_vec()..1995_u32.to_be_bytes().to_vec());

        let subquery_key: Vec<u8> = b"\0".to_vec();
        let mut subquery = Query::new();
        subquery.insert_all();

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery.clone());

        // Baseline query: no offset or limit + left to right
        let path_query = PathQuery::new(path.clone(), SizedQuery::new(query.clone(), None, None));

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 250);

        let mut first_value = 1990_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1994_u32.to_be_bytes().to_vec();
        last_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 250);
        compare_result_sets(&elements, &result_set);

        subquery.left_to_right = false;

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery.clone());

        query.left_to_right = false;

        // Baseline query: no offset or limit + right to left
        let path_query = PathQuery::new(path.clone(), SizedQuery::new(query.clone(), None, None));

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 250);

        let mut first_value = 1994_u32.to_be_bytes().to_vec();
        first_value.append(&mut 149_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1990_u32.to_be_bytes().to_vec();
        last_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 250);
        compare_result_sets(&elements, &result_set);

        subquery.left_to_right = true;

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery.clone());

        query.left_to_right = true;

        // Limit the result to just 55 elements
        let path_query =
            PathQuery::new(path.clone(), SizedQuery::new(query.clone(), Some(55), None));

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 55);

        let mut first_value = 1990_u32.to_be_bytes().to_vec();
        first_value.append(&mut 100_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        // Second tree 5 element [100, 101, 102, 103, 104]
        let mut last_value = 1991_u32.to_be_bytes().to_vec();
        last_value.append(&mut 104_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 55);
        compare_result_sets(&elements, &result_set);

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery.clone());

        // Limit the result set to 60 elements but skip the first 14 elements
        let path_query = PathQuery::new(
            path.clone(),
            SizedQuery::new(query.clone(), Some(60), Some(14)),
        );

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 60);

        // Skips the first 14 elements, starts from the 15th
        // i.e. skips [100 - 113] starts from 114
        let mut first_value = 1990_u32.to_be_bytes().to_vec();
        first_value.append(&mut 114_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        // Continues for 60 iterations
        // Takes 36 elements from the first tree (50 - 14)
        // takes the remaining 24 from the second three (60 - 36)
        let mut last_value = 1991_u32.to_be_bytes().to_vec();
        last_value.append(&mut 123_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery.clone());

        query.left_to_right = false;

        // Limit the result set to 60 element but skip first 10 elements (this time
        // right to left)
        let path_query = PathQuery::new(
            path.clone(),
            SizedQuery::new(query.clone(), Some(60), Some(10)),
        );

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 60);

        // Skips the first 10 elements from the back
        // last tree and starts from the 11th before the end
        // Underlying subquery is ascending
        let mut first_value = 1994_u32.to_be_bytes().to_vec();
        first_value.append(&mut 110_u32.to_be_bytes().to_vec());
        assert_eq!(elements[0], first_value);

        let mut last_value = 1993_u32.to_be_bytes().to_vec();
        last_value.append(&mut 119_u32.to_be_bytes().to_vec());
        assert_eq!(elements[elements.len() - 1], last_value);

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery.clone());

        query.left_to_right = true;

        // Offset bigger than elements in range
        let path_query = PathQuery::new(
            path.clone(),
            SizedQuery::new(query.clone(), None, Some(5000)),
        );

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 0);

        query.set_subquery_key(subquery_key.clone());
        query.set_subquery(subquery);

        // Limit bigger than elements in range
        let path_query = PathQuery::new(
            path.clone(),
            SizedQuery::new(query.clone(), Some(5000), None),
        );

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 250);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 250);

        // Test on unique subtree build
        let db = make_test_grovedb(grove_version);
        populate_tree_for_unique_range_subquery(&db, grove_version);

        let mut query = Query::new_with_direction(true);
        query.insert_range(1990_u32.to_be_bytes().to_vec()..2000_u32.to_be_bytes().to_vec());

        query.set_subquery_key(subquery_key);

        let path_query = PathQuery::new(path, SizedQuery::new(query.clone(), Some(5), Some(2)));

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 5);

        let first_value = 1992_u32.to_be_bytes().to_vec();
        assert_eq!(elements[0], first_value);

        let last_value = 1996_u32.to_be_bytes().to_vec();
        assert_eq!(elements[elements.len() - 1], last_value);
    }

    #[test]
    /// Tests that child root hashes are correctly propagated to parent nodes when multiple levels of trees and references are inserted in the same batch.
    ///
    /// This test constructs a multi-level tree structure with nested subtrees and references, applies all insertions in batches, and verifies that the resulting root hash matches the expected value after proof verification.
    ///
    /// # Examples
    ///
    /// ```
    /// test_correct_child_root_hash_propagation_for_parent_in_same_batch();
    /// ```
    fn test_correct_child_root_hash_propagation_for_parent_in_same_batch() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();
        let tree_name_slice: &[u8] = &[
            2, 17, 40, 46, 227, 17, 179, 211, 98, 50, 130, 107, 246, 26, 147, 45, 234, 189, 245,
            77, 252, 86, 99, 107, 197, 226, 188, 54, 239, 64, 17, 37,
        ];

        let batch = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            vec![1],
            Element::empty_tree(),
        )];
        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");

        let batch = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![vec![1]],
                tree_name_slice.to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![vec![1], tree_name_slice.to_vec()],
                b"\0".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![vec![1], tree_name_slice.to_vec()],
                vec![1],
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![vec![1], tree_name_slice.to_vec(), vec![1]],
                b"person".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    vec![1],
                    tree_name_slice.to_vec(),
                    vec![1],
                    b"person".to_vec(),
                ],
                b"\0".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    vec![1],
                    tree_name_slice.to_vec(),
                    vec![1],
                    b"person".to_vec(),
                ],
                b"firstName".to_vec(),
                Element::empty_tree(),
            ),
        ];
        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");

        let batch = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    vec![1],
                    tree_name_slice.to_vec(),
                    vec![1],
                    b"person".to_vec(),
                    b"\0".to_vec(),
                ],
                b"person_id_1".to_vec(),
                Element::new_item(vec![50]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    vec![1],
                    tree_name_slice.to_vec(),
                    vec![1],
                    b"person".to_vec(),
                    b"firstName".to_vec(),
                ],
                b"cammi".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    vec![1],
                    tree_name_slice.to_vec(),
                    vec![1],
                    b"person".to_vec(),
                    b"firstName".to_vec(),
                    b"cammi".to_vec(),
                ],
                b"\0".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    vec![1],
                    tree_name_slice.to_vec(),
                    vec![1],
                    b"person".to_vec(),
                    b"firstName".to_vec(),
                    b"cammi".to_vec(),
                    b"\0".to_vec(),
                ],
                b"person_ref_id".to_vec(),
                Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                    4,
                    vec![b"\0".to_vec(), b"person_id_1".to_vec()],
                )),
            ),
        ];
        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("should apply batch");

        let path = vec![
            vec![1],
            tree_name_slice.to_vec(),
            vec![1],
            b"person".to_vec(),
            b"firstName".to_vec(),
        ];
        let mut query = Query::new();
        query.insert_all();
        query.set_subquery_key(b"\0".to_vec());
        let mut subquery = Query::new();
        subquery.insert_all();
        query.set_subquery(subquery);
        let path_query = PathQuery::new(
            path,
            SizedQuery {
                query: query.clone(),
                limit: Some(100),
                offset: Some(0),
            },
        );

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .expect("expected successful proving");
        let (hash, _result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
    }

    #[test]
    /// Tests mixed-level queries and proof verification involving trees, items, and references with various limits and offsets.
    ///
    /// This test constructs a tree with both direct items and references, then performs queries that include subqueries, limits, and offsets. It verifies that the returned elements and the results obtained from proof verification are consistent and correct for each query scenario.
    ///
    /// # Examples
    ///
    /// ```
    /// test_mixed_level_proofs();
    /// ```
    fn test_mixed_level_proofs() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        //                              TEST_LEAF
        //               /          |              |            \
        //              key1       key2 : [1]     key3         key4 : (Ref -> Key2)
        //            /   |   \
        //           k1   k2   k3
        //          /    /    /
        //         2    3    4

        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key2",
            Element::new_item(vec![1]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key4",
            Element::new_reference(ReferencePathType::SiblingReference(b"key2".to_vec())),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"k1",
            Element::new_item(vec![2]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"k2",
            Element::new_item(vec![3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"k3",
            Element::new_item(vec![4]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");

        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        query.set_subquery(subquery);

        let path = vec![TEST_LEAF.to_vec()];

        let path_query = PathQuery::new_unsized(path.clone(), query.clone());
        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("successful get_path_query");

        assert_eq!(elements.len(), 5);
        assert_eq!(elements, vec![vec![2], vec![3], vec![4], vec![1], vec![1]]);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        // println!(
        //     "{}",
        //     result_set
        //         .iter()
        //         .map(|a| a.to_string())
        //         .collect::<Vec<String>>()
        //         .join(" | ")
        // );
        assert_eq!(result_set.len(), 5);
        compare_result_sets(&elements, &result_set);

        // Test mixed element proofs with limit and offset
        let path_query = PathQuery::new_unsized(path.clone(), query.clone());
        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("successful get_path_query");

        assert_eq!(elements.len(), 5);
        assert_eq!(elements, vec![vec![2], vec![3], vec![4], vec![1], vec![1]]);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 5);
        compare_result_sets(&elements, &result_set);

        // TODO: Fix noticed bug when limit and offset are both set to Some(0)

        let path_query =
            PathQuery::new(path.clone(), SizedQuery::new(query.clone(), Some(1), None));
        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("successful get_path_query");

        assert_eq!(elements.len(), 1);
        assert_eq!(elements, vec![vec![2]]);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 1);
        compare_result_sets(&elements, &result_set);

        let path_query = PathQuery::new(
            path.clone(),
            SizedQuery::new(query.clone(), Some(3), Some(0)),
        );
        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("successful get_path_query");

        assert_eq!(elements.len(), 3);
        assert_eq!(elements, vec![vec![2], vec![3], vec![4]]);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 3);
        compare_result_sets(&elements, &result_set);

        let path_query = PathQuery::new(
            path.clone(),
            SizedQuery::new(query.clone(), Some(4), Some(0)),
        );
        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("successful get_path_query");

        assert_eq!(elements.len(), 4);
        assert_eq!(elements, vec![vec![2], vec![3], vec![4], vec![1]]);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 4);
        compare_result_sets(&elements, &result_set);

        let path_query = PathQuery::new(path, SizedQuery::new(query.clone(), Some(10), Some(4)));
        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("successful get_path_query");

        assert_eq!(elements.len(), 1);
        assert_eq!(elements, vec![vec![1]]);
    }

    #[test]
    /// Tests mixed-level queries and proof verification involving both trees and items, including conditional subqueries and query limits.
    ///
    /// This test inserts multiple subtrees and items into the database, constructs a query with a conditional subquery, and verifies that both direct query results and proof verification yield the expected elements. It also checks that limiting the query returns the correct number of elements and that the proof verification matches the database root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_mixed_level_proofs_with_tree();
    /// ```
    fn test_mixed_level_proofs_with_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"k1",
            Element::new_item(vec![2]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"k2",
            Element::new_item(vec![3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"k3",
            Element::new_item(vec![4]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");
        db.insert(
            [TEST_LEAF, b"key2"].as_ref(),
            b"k1",
            Element::new_item(vec![5]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful item insert");

        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        query.add_conditional_subquery(QueryItem::Key(b"key1".to_vec()), None, Some(subquery));

        let path = vec![TEST_LEAF.to_vec()];

        let path_query = PathQuery::new_unsized(path.clone(), query.clone());

        let (elements, _) = db
            .query_raw(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 5);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());

        // println!(
        //     "{}",
        //     result_set
        //         .iter()
        //         .map(|a| a.to_string())
        //         .collect::<Vec<_>>()
        //         .join(", ")
        // );
        assert_eq!(result_set.len(), 5);

        // TODO: verify that the result set is exactly the same
        // compare_result_sets(&elements, &result_set);

        let path_query = PathQuery::new(path, SizedQuery::new(query.clone(), Some(1), None));

        let (elements, _) = db
            .query_raw(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 1);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 1);
        // TODO: verify that the result set is exactly the same
        // compare_result_sets(&elements, &result_set);
    }

    #[test]
    /// Tests mixed-level GroveDB queries with subquery paths and verifies proof correctness.
    ///
    /// This test constructs a multi-level tree structure with various items and subtrees, then performs queries using subquery paths, subqueries, and conditional subqueries. It verifies that the returned elements and proof verifications match expectations for each query scenario, including path translations and nested queries.
    ///
    /// # Examples
    ///
    /// ```
    /// test_mixed_level_proofs_with_subquery_paths();
    /// ```
    fn test_mixed_level_proofs_with_subquery_paths() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        //                        TEST_LEAF
        //              /             |            \
        //             a              b             c
        //         /   |   \        /     \
        //        d   e:2   f:3    g:4     d
        //      /                         / | \
        //    d:6                        i  j  k
        //

        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"b",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"c",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, b"a"].as_ref(),
            b"d",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF, b"a"].as_ref(),
            b"e",
            Element::new_item(vec![2]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF, b"a"].as_ref(),
            b"f",
            Element::new_item(vec![3]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, b"a", b"d"].as_ref(),
            b"d",
            Element::new_item(vec![6]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, b"b"].as_ref(),
            b"g",
            Element::new_item(vec![4]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF, b"b"].as_ref(),
            b"d",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, b"b", b"d"].as_ref(),
            b"i",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF, b"b", b"d"].as_ref(),
            b"j",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        db.insert(
            [TEST_LEAF, b"b", b"d"].as_ref(),
            b"k",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful subtree insert");
        // // if you don't have an item at the subquery path translation, you shouldn't
        // be // added to the result set.
        let mut query = Query::new();
        query.insert_all();
        query.set_subquery_path(vec![b"d".to_vec()]);

        let path = vec![TEST_LEAF.to_vec()];

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_raw(
                &path_query,
                false,
                true,
                false,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(
            elements,
            QueryResultElements::from_elements(vec![
                PathKeyElementTrioResultItem((
                    vec![b"test_leaf".to_vec(), b"a".to_vec()],
                    b"d".to_vec(),
                    Element::Tree(Some(b"d".to_vec()), None)
                )),
                PathKeyElementTrioResultItem((
                    vec![b"test_leaf".to_vec(), b"b".to_vec()],
                    b"d".to_vec(),
                    Element::Tree(Some(b"j".to_vec()), None)
                ))
            ])
        );

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        // println!(
        //     "{}",
        //     result_set
        //         .iter()
        //         .map(|a| a.to_string())
        //         .collect::<Vec<_>>()
        //         .join("| ")
        // );
        assert_eq!(result_set.len(), 2);

        // apply path translation then query
        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        query.set_subquery_path(vec![b"d".to_vec()]);
        query.set_subquery(subquery);

        let path = vec![TEST_LEAF.to_vec()];

        let path_query = PathQuery::new_unsized(path, query.clone());

        let (elements, _) = db
            .query_raw(
                &path_query,
                false,
                true,
                false,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(
            elements,
            QueryResultElements::from_elements(vec![
                PathKeyElementTrioResultItem((
                    vec![b"test_leaf".to_vec(), b"a".to_vec(), b"d".to_vec()],
                    b"d".to_vec(),
                    Element::Item(vec![6], None)
                )),
                PathKeyElementTrioResultItem((
                    vec![b"test_leaf".to_vec(), b"b".to_vec(), b"d".to_vec()],
                    b"i".to_vec(),
                    Element::Tree(None, None)
                )),
                PathKeyElementTrioResultItem((
                    vec![b"test_leaf".to_vec(), b"b".to_vec(), b"d".to_vec()],
                    b"j".to_vec(),
                    Element::Tree(None, None)
                )),
                PathKeyElementTrioResultItem((
                    vec![b"test_leaf".to_vec(), b"b".to_vec(), b"d".to_vec()],
                    b"k".to_vec(),
                    Element::Tree(None, None)
                ))
            ])
        );

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 4);

        // apply empty path translation
        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        query.set_subquery_path(vec![]);
        query.set_subquery(subquery);

        let path = vec![TEST_LEAF.to_vec()];

        let path_query = PathQuery::new_unsized(path, query.clone());

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 5);

        // use conditionals to return from more than 2 depth
        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        let mut deeper_subquery = Query::new();
        deeper_subquery.insert_all();
        subquery.add_conditional_subquery(
            QueryItem::Key(b"d".to_vec()),
            None,
            Some(deeper_subquery),
        );
        query.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(subquery.clone()));
        query.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(subquery.clone()));

        let path = vec![TEST_LEAF.to_vec()];

        let path_query = PathQuery::new_unsized(path, query.clone());

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 8);
    }

    #[test]
    /// Tests that attempting to generate a proof for a query with a limit of zero results in an error.
    ///
    /// # Examples
    ///
    /// ```
    /// test_proof_with_limit_zero();
    /// // The test will pass if an error is returned when proving with limit 0.
    /// ```
    fn test_proof_with_limit_zero() {
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);
        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(0), Some(0)),
        );

        db.prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .expect_err("expected error when trying to prove with limit 0");
    }

    #[test]
    /// Tests that result set paths are correctly tracked after proof verification for various query types, including subqueries and subquery paths.
    ///
    /// This test verifies that the `path` field in each result set entry returned by `GroveDb::verify_query_raw` matches the expected query path, even when using subqueries, subquery paths, and conditional subqueries. It checks that the result set keys and their associated paths are consistent with the structure of the queried tree.
    ///
    /// # Examples
    ///
    /// ```
    /// test_result_set_path_after_verification();
    /// ```
    fn test_result_set_path_after_verification() {
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);
        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 3);

        // assert the result set path
        assert_eq!(
            result_set[0].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[1].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[2].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );

        assert_eq!(result_set[0].key, b"key1".to_vec());
        assert_eq!(result_set[1].key, b"key2".to_vec());
        assert_eq!(result_set[2].key, b"key3".to_vec());

        // Test path tracking with subquery
        let mut query = Query::new();
        query.insert_all();
        let mut subq = Query::new();
        subq.insert_all();
        query.set_subquery(subq);
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 5);

        assert_eq!(
            result_set[0].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[1].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[2].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[3].path,
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()]
        );
        assert_eq!(
            result_set[4].path,
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()]
        );

        // Test path tracking with subquery path
        // perform a query, do a translation, perform another query
        let mut query = Query::new();
        query.insert_key(b"deep_leaf".to_vec());
        query.set_subquery_path(vec![b"deep_node_1".to_vec(), b"deeper_1".to_vec()]);
        let mut subq = Query::new();
        subq.insert_all();
        query.set_subquery(subq);
        let path_query = PathQuery::new_unsized(vec![], query);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 3);

        assert_eq!(
            result_set[0].path,
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec()
            ]
        );
        assert_eq!(
            result_set[1].path,
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec()
            ]
        );
        assert_eq!(
            result_set[2].path,
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec()
            ]
        );

        assert_eq!(result_set[0].key, b"key1".to_vec());
        assert_eq!(result_set[1].key, b"key2".to_vec());
        assert_eq!(result_set[2].key, b"key3".to_vec());

        // Test path tracking for mixed level result set
        let mut query = Query::new();
        query.insert_all();
        let mut subq = Query::new();
        subq.insert_all();
        query.add_conditional_subquery(QueryItem::Key(b"innertree".to_vec()), None, Some(subq));

        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 4);

        assert_eq!(
            result_set[0].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[1].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[2].path,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(result_set[3].path, vec![TEST_LEAF.to_vec()]);

        assert_eq!(result_set[0].key, b"key1".to_vec());
        assert_eq!(result_set[1].key, b"key2".to_vec());
        assert_eq!(result_set[2].key, b"key3".to_vec());
        assert_eq!(result_set[3].key, b"innertree4".to_vec());
    }

    #[test]
    /// Tests that proof verification returns the correct set of (path, key, optional element) tuples for a query over a subtree.
    ///
    /// This test constructs a query for all items in a specific subtree, generates a proof, verifies it, and asserts that the result set contains the expected path-key-element triples. Also checks that the verified root hash matches the database root hash.
    fn test_verification_with_path_key_optional_element_trio() {
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);
        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 3);

        assert_eq!(
            result_set[0],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key1".to_vec(),
                Some(Element::new_item(b"value1".to_vec()))
            )
        );
        assert_eq!(
            result_set[1],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key2".to_vec(),
                Some(Element::new_item(b"value2".to_vec()))
            )
        );
        assert_eq!(
            result_set[2],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key3".to_vec(),
                Some(Element::new_item(b"value3".to_vec()))
            )
        );
    }

    #[test]
    /// Tests proof generation and verification for absent and present keys in a subtree.
    ///
    /// Verifies that a proof can be generated for a set of keys, some of which are present and some absent, and that the proof correctly distinguishes between them after verification. Also checks that the root hash matches and the result set contains the expected presence or absence for each key.
    ///
    /// # Examples
    ///
    /// ```
    /// test_absence_proof();
    /// ```
    fn test_absence_proof() {
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // simple case, request for items k2..=k5 under inner tree
        // we pass them as keys as terminal keys does not handle ranges with start or
        // end len greater than 1 k2, k3 should be Some, k4, k5 should be None, k1,
        // k6.. should not be in map
        let mut query = Query::new();
        query.insert_key(b"key2".to_vec());
        query.insert_key(b"key3".to_vec());
        query.insert_key(b"key4".to_vec());
        query.insert_key(b"key5".to_vec());
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            SizedQuery::new(query, Some(4), None),
        );

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query_with_absence_proof(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 4);

        assert_eq!(
            result_set[0].0,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[1].0,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[2].0,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );
        assert_eq!(
            result_set[3].0,
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
        );

        assert_eq!(result_set[0].1, b"key2".to_vec());
        assert_eq!(result_set[1].1, b"key3".to_vec());
        assert_eq!(result_set[2].1, b"key4".to_vec());
        assert_eq!(result_set[3].1, b"key5".to_vec());

        assert_eq!(result_set[0].2, Some(Element::new_item(b"value2".to_vec())));
        assert_eq!(result_set[1].2, Some(Element::new_item(b"value3".to_vec())));
        assert_eq!(result_set[2].2, None);
        assert_eq!(result_set[3].2, None);
    }

    #[test]
    /// Tests that a proof generated for a superset query can be used to verify a subset query, ensuring correct result extraction and root hash consistency.
    ///
    /// This test constructs a tree, generates a proof for a query that retrieves all items, and then verifies that the same proof can be used to validate a subset query for a specific key. It asserts that the result set and root hash are correct for both the superset and subset queries.
    ///
    /// # Examples
    ///
    /// ```
    /// test_subset_proof_verification();
    /// ```
    fn test_subset_proof_verification() {
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // original path query
        let mut query = Query::new();
        query.insert_all();
        let mut subq = Query::new();
        subq.insert_all();
        query.set_subquery(subq);

        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 5);
        assert_eq!(
            result_set[0],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key1".to_vec(),
                Some(Element::new_item(b"value1".to_vec()))
            )
        );
        assert_eq!(
            result_set[1],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key2".to_vec(),
                Some(Element::new_item(b"value2".to_vec()))
            )
        );
        assert_eq!(
            result_set[2],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key3".to_vec(),
                Some(Element::new_item(b"value3".to_vec()))
            )
        );
        assert_eq!(
            result_set[3],
            (
                vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
                b"key4".to_vec(),
                Some(Element::new_item(b"value4".to_vec()))
            )
        );
        assert_eq!(
            result_set[4],
            (
                vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
                b"key5".to_vec(),
                Some(Element::new_item(b"value5".to_vec()))
            )
        );

        // subset path query
        let mut query = Query::new();
        query.insert_key(b"innertree".to_vec());
        let mut subq = Query::new();
        subq.insert_key(b"key1".to_vec());
        query.set_subquery(subq);
        let subset_path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let (hash, result_set) =
            GroveDb::verify_subset_query(&proof, &subset_path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 1);
        assert_eq!(
            result_set[0],
            (
                vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
                b"key1".to_vec(),
                Some(Element::new_item(b"value1".to_vec()))
            )
        );
    }
    #[test]
    /// Tests chained path query verification by generating a proof for a nested query, then verifying the proof and chaining additional path queries based on the results.
    ///
    /// This test constructs a deep tree, generates a proof for a multi-level subquery, and verifies the proof. It then defines a chained path query generator to query additional paths based on the initial results, and verifies that the chained queries return the expected elements and root hash.
    ///
    /// # Examples
    ///
    /// ```
    /// test_chained_path_query_verification();
    /// ```
    fn test_chained_path_query_verification() {
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let mut subq = Query::new();
        subq.insert_all();
        let mut subsubq = Query::new();
        subsubq.insert_all();

        subq.set_subquery(subsubq);
        query.set_subquery(subq);

        let path_query = PathQuery::new_unsized(vec![b"deep_leaf".to_vec()], query);

        // first prove non verbose
        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 14);

        // init deeper_1 path query
        let mut query = Query::new();
        query.insert_all();

        let deeper_1_path_query = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec(),
            ],
            query,
        );

        // define the path query generators
        let mut chained_path_queries = vec![];
        chained_path_queries.push(|_elements: Vec<PathKeyOptionalElementTrio>| {
            let mut query = Query::new();
            query.insert_all();

            let deeper_2_path_query = PathQuery::new_unsized(
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_2".to_vec(),
                ],
                query,
            );
            Some(deeper_2_path_query)
        });

        // verify the path query chain
        let (root_hash, results) = GroveDb::verify_query_with_chained_path_queries(
            &proof,
            &deeper_1_path_query,
            chained_path_queries,
            grove_version,
        )
        .unwrap();
        assert_eq!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().unwrap()
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].len(), 3);
        assert_eq!(
            results[0][0],
            (
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_1".to_vec()
                ],
                b"key1".to_vec(),
                Some(Element::new_item(b"value1".to_vec()))
            )
        );
        assert_eq!(
            results[0][1],
            (
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_1".to_vec()
                ],
                b"key2".to_vec(),
                Some(Element::new_item(b"value2".to_vec()))
            )
        );
        assert_eq!(
            results[0][2],
            (
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_1".to_vec()
                ],
                b"key3".to_vec(),
                Some(Element::new_item(b"value3".to_vec()))
            )
        );

        assert_eq!(results[1].len(), 3);
        assert_eq!(
            results[1][0],
            (
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_2".to_vec()
                ],
                b"key4".to_vec(),
                Some(Element::new_item(b"value4".to_vec()))
            )
        );
        assert_eq!(
            results[1][1],
            (
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_2".to_vec()
                ],
                b"key5".to_vec(),
                Some(Element::new_item(b"value5".to_vec()))
            )
        );
        assert_eq!(
            results[1][2],
            (
                vec![
                    b"deep_leaf".to_vec(),
                    b"deep_node_1".to_vec(),
                    b"deeper_2".to_vec()
                ],
                b"key6".to_vec(),
                Some(Element::new_item(b"value6".to_vec()))
            )
        );
    }

    #[test]
    /// Tests chained query proof generation and verification where the result of one query determines the parameters of the next.
    ///
    /// This test sets up two trees: one mapping IDs to names, and another mapping names to ages. It verifies that a proof can be generated and verified for a query that retrieves the age of all entries with IDs greater than a specified value, ordered by name. The test demonstrates that the verifier can use the result of the first proof (ID to name) to construct and verify the second proof (name to age), and that merged and chained path queries yield correct results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_query_b_depends_on_query_a();
    /// ```
    fn test_query_b_depends_on_query_a() {
        let grove_version = GroveVersion::latest();
        // we have two trees
        // one with a mapping of id to name
        // another with a mapping of name to age
        // we want to get the age of every one after a certain id ordered by name
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF contains the id to name mapping
        db.insert(
            [TEST_LEAF].as_ref(),
            &[1],
            Element::new_item(b"d".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            &[2],
            Element::new_item(b"b".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            &[3],
            Element::new_item(b"c".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            &[4],
            Element::new_item(b"a".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        // ANOTHER_TEST_LEAF contains the name to age mapping
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(vec![10]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"b",
            Element::new_item(vec![30]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"c",
            Element::new_item(vec![12]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"d",
            Element::new_item(vec![46]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        // Query: return the age of everyone greater than id 2 ordered by name
        // id 2 - b
        // we want to return the age for c and d = 12, 46 respectively
        // the proof generator knows that id 2 = b, but the verifier doesn't
        // hence we need to generate two proofs
        // prove that 2 - b then prove age after b
        // the verifier has to use the result of the first proof 2 - b
        // to generate the path query for the verification of the second proof

        // query name associated with id 2
        let mut query = Query::new();
        query.insert_key(vec![2]);
        let mut path_query_one = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // first we show that this returns the correct output
        let proof = db
            .prove_query(&path_query_one, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query(&proof, &path_query_one, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 1);
        assert_eq!(result_set[0].2, Some(Element::new_item(b"b".to_vec())));

        // next query should return the age for elements above b
        let mut query = Query::new();
        query.insert_range_after(b"b".to_vec()..);
        let path_query_two = PathQuery::new_unsized(vec![ANOTHER_TEST_LEAF.to_vec()], query);

        // show that we get the correct output
        let proof = db
            .prove_query(&path_query_two, None, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) =
            GroveDb::verify_query(&proof, &path_query_two, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 2);
        assert_eq!(result_set[0].2, Some(Element::new_item(vec![12])));
        assert_eq!(result_set[1].2, Some(Element::new_item(vec![46])));

        // now we merge the path queries
        let mut merged_path_queries =
            PathQuery::merge(vec![&path_query_one, &path_query_two], grove_version).unwrap();
        merged_path_queries.query.limit = Some(3);
        let proof = db
            .prove_query(&merged_path_queries, None, None, grove_version)
            .unwrap()
            .unwrap();

        // verifier only has access to the statement age > 2
        // need to first get the name associated with 2 from the proof
        // then use that to construct the next path query
        let mut chained_path_queries = vec![];
        chained_path_queries.push(|prev_elements: Vec<PathKeyOptionalElementTrio>| {
            let mut query = Query::new();
            let name_element = prev_elements[0].2.as_ref().unwrap();
            if let Element::Item(name, ..) = name_element {
                query.insert_range_after(name.to_owned()..);
                Some(PathQuery::new(
                    vec![ANOTHER_TEST_LEAF.to_vec()],
                    SizedQuery::new(query, Some(2), None),
                ))
            } else {
                None
            }
        });

        // add limit to path query one
        path_query_one.query.limit = Some(1);

        let (_, result_set) = GroveDb::verify_query_with_chained_path_queries(
            proof.as_slice(),
            &path_query_one,
            chained_path_queries,
            grove_version,
        )
        .unwrap();
        assert_eq!(result_set.len(), 2);
        assert_eq!(result_set[0].len(), 1);
        assert_eq!(result_set[1].len(), 2);

        let age_result = result_set[1].clone();
        assert_eq!(age_result[0].2, Some(Element::new_item(vec![12])));
        assert_eq!(age_result[1].2, Some(Element::new_item(vec![46])));
    }

    #[test]
    /// Tests that a proof can be generated and verified for the absence of a key in a path containing an intermediate empty tree.
    ///
    /// This test creates a tree structure where the intermediate node is empty, then attempts to prove the absence of a key in a non-existent subtree. It verifies that the proof is valid, the result set is empty, and the root hash matches the database.
    ///
    /// # Examples
    ///
    /// ```
    /// test_prove_absent_path_with_intermediate_emtpy_tree();
    /// ```
    fn test_prove_absent_path_with_intermediate_emtpy_tree() {
        let grove_version = GroveVersion::latest();
        //         root
        // test_leaf (empty)
        let grovedb = make_test_grovedb(grove_version);

        // prove the absence of key "book" in ["test_leaf", "invalid"]
        let mut query = Query::new();
        query.insert_key(b"book".to_vec());
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"invalid".to_vec()], query);

        let proof = grovedb
            .prove_query(&path_query, None, None, grove_version)
            .unwrap()
            .expect("should generate proofs");

        let (root_hash, result_set) =
            GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
                .expect("should verify proof");
        assert_eq!(result_set.len(), 0);
        assert_eq!(
            root_hash,
            grovedb.root_hash(None, grove_version).unwrap().unwrap()
        );
    }

    #[test]
    /// Tests that a path query with a subquery and a limit of 2, ascending from the start, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a two-level tree hierarchy, performs a range query with a subquery and a limit of 2 in ascending order, and asserts that both the query and its proof return exactly two elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_subquery_and_limit_2_asc_from_start();
    /// ```
    fn test_path_query_items_with_subquery_and_limit_2_asc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a descending path query with a subquery and a limit of 2 returns the correct elements and verifies the proof.
    ///
    /// This test constructs a two-level tree hierarchy, performs a descending range-to-inclusive query limited to two elements, and checks that both the query and its proof return the expected number of results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_subquery_and_limit_2_desc_from_start();
    /// ```
    fn test_path_query_items_with_subquery_and_limit_2_desc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"A".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with a subquery and a limit of 2, starting in the middle of a two-by-two hierarchy, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a tree with nodes A, B, and C, each having two children, and performs a range query starting from "B" with a limit of 2. It asserts that the correct number of elements is returned and that the proof can be successfully verified.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_subquery_and_limit_2_asc_in_middle();
    /// ```
    fn test_path_query_items_with_subquery_and_limit_2_asc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"B".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a descending range query with a subquery and a limit of 2 returns the correct elements from the middle of a two-by-two hierarchy tree.
    ///
    /// This test constructs a tree with nodes A, B, and C, each having two children. It performs a descending range query up to and including "B", applies a subquery to each branch, and limits the result to 2 elements. The test verifies both the direct query result and the result obtained from proof verification.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_subquery_and_limit_2_desc_in_middle();
    /// ```
    fn test_path_query_items_with_subquery_and_limit_2_desc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"B".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with a subquery and a limit of 2, ascending from the end of the key range, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a two-by-two hierarchy, performs a range query starting from key "M" with a limit of 2 in ascending order, and checks that both the query and its proof return exactly two elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_subquery_and_limit_2_asc_at_end();
    /// ```
    fn test_path_query_items_with_subquery_and_limit_2_asc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"M".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a descending path query with a subquery and a limit of 2 at the end of the key range returns the correct elements and verifies the proof.
    ///
    /// This test constructs a two-level hierarchy (A..M, each with two children), performs a descending range-to-inclusive query limited to 2 elements, and checks both the query result and proof verification for correctness.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_subquery_and_limit_2_desc_at_end();
    /// ```
    fn test_path_query_items_with_subquery_and_limit_2_desc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"M".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with an intermediate path translation and a limit of 2 returns the first two elements in ascending order.
    ///
    /// This test constructs a two-level tree hierarchy with intermediate nodes, performs a path query that translates through the intermediate node (`0`), and verifies that both direct query results and proof verification yield exactly two elements in ascending order.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_asc_from_start();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_asc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with an intermediate path translation and a descending order limit returns the correct elements and proof.
    ///
    /// This test constructs a tree with an intermediate node under each parent, then performs a descending range query with a limit of 2, starting from the beginning. It verifies that both the direct query and the proof verification return the expected number of elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_desc_from_start();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_desc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"A".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with an intermediate path translation and a limit of 2, ascending from a middle key, returns the correct elements and verifies proof correctness.
    ///
    /// This test constructs a two-level tree hierarchy with intermediate nodes, performs a range query starting from key "B" with a subquery path through the intermediate node `0`, and asserts that exactly two elements are returned. It also verifies that the proof generated for this query is valid and yields the same result set after verification.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_asc_in_middle();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_asc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"B".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with an intermediate path translation, limit 2, starting from the middle of the key range.
    ///
    /// This test constructs a two-level tree hierarchy with intermediate nodes, performs a descending range query up to and including key "B" with a limit of 2, and verifies both the query results and proof verification.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_desc_in_middle();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_desc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"B".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with an intermediate path and a range excluding middle keys returns the correct two elements in ascending order, and verifies the proof for correctness.
    ///
    /// This test constructs a two-level tree hierarchy with intermediate nodes, performs a range-to query up to key "F" (excluding middle keys), applies a limit of 2, and checks that both the query and its proof return exactly two elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_asc_not_included_in_middle();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_asc_not_included_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeTo(..b"F".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with intermediate path translation, a limit of 2, and a range excluding middle keys.
    ///
    /// This test verifies that querying a two-level tree structure with an intermediate node and a descending range up to (but not including) key "F" returns the correct two elements. It also checks that the generated proof can be successfully verified and matches the query result.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_desc_not_included_in_middle();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_desc_not_included_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeTo(..b"F".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with an intermediate path translation and a limit of 2, ascending from the end of the key range, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a two-level tree hierarchy with intermediate nodes, performs a range query starting from key "M", and checks that only the last two elements are returned. It also verifies that the proof generated for this query is valid and matches the query results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_asc_at_end();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_asc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"M".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with an intermediate path translation, limit 2, at the end of the range.
    ///
    /// This test constructs a two-level tree hierarchy with intermediate nodes, then performs a descending range-to-inclusive query with a limit of 2, using a subquery path translation. It verifies that both the direct query and the proof verification return exactly two elements, confirming correct query and proof behavior for this scenario.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_intermediate_path_limit_2_desc_at_end();
    /// ```
    fn test_path_query_items_with_intermediate_path_limit_2_desc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //          A ---------------- B ---------------------C
        //          |                  |                      |
        //          0                  0                      0
        //         / \                /  \                  /    \
        //        A1  A2             B1  B2                C1    C2
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"M".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with references and a limit of 2 returns the correct elements in ascending order from the start.
    ///
    /// This test constructs a two-level tree with references, performs a path query with a limit of 2, and verifies that both the direct query and proof verification return exactly two elements in ascending order.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_asc_from_start();
    /// ```
    fn test_path_query_items_with_reference_limit_2_asc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with references and a limit of 2 from the start of the range.
    ///
    /// Constructs a hierarchical tree with references, performs a descending range-to-inclusive query with a limit of 2, and verifies both the query results and proof verification return the expected number of elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_desc_from_start();
    /// ```
    fn test_path_query_items_with_reference_limit_2_desc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"a".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with references and a limit of 2, ascending from the middle of the key range, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a tree with a two-level hierarchy and references, performs a range query starting from key "b" with a limit of 2, and checks that both the query results and the proof verification yield the expected number of elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_asc_in_middle();
    /// ```
    fn test_path_query_items_with_reference_limit_2_asc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"b".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with references, limit 2, starting from the middle of the key range.
    ///
    /// Constructs a tree with parent nodes 'a', 'b', and 'c', each containing intermediate nodes and reference nodes.  
    /// Executes a descending range-to-inclusive query up to key 'b', following a subquery path through reference nodes, and limits the result to 2 elements.  
    /// Verifies that both direct query results and proof verification return exactly 2 elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_desc_in_middle();
    /// ```
    fn test_path_query_items_with_reference_limit_2_desc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"b".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests a path query with references, limit 2, ascending order, excluding middle keys.
    ///
    /// Constructs a hierarchical tree with references and performs a range-to query up to key "f",
    /// following a subquery path through intermediate nodes. Verifies that only two elements are returned
    /// and that the proof verification yields the same result set length.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_asc_not_included_in_middle();
    /// ```
    fn test_path_query_items_with_reference_limit_2_asc_not_included_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeTo(..b"f".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests a descending path query with references, limit 2, excluding middle keys.
    ///
    /// Constructs a tree with a two-level reference hierarchy and performs a descending range-to query up to key "f" (exclusive), following a subquery path through intermediate nodes. Verifies that only two elements are returned and that the proof verification matches the query result.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_desc_not_included_in_middle();
    /// ```
    fn test_path_query_items_with_reference_limit_2_desc_not_included_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeTo(..b"f".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with references and a limit of 2, ascending from the end of the key range, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a hierarchical tree with references, performs a range query starting from key "m" with a limit of 2 in ascending order, and asserts that both the query and its proof return the expected number of elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_asc_at_end();
    /// ```
    fn test_path_query_items_with_reference_limit_2_asc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"m".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with references, limit 2, at the end of the key range.
    ///
    /// Constructs a hierarchical tree with references, performs a descending range-to-inclusive query limited to two elements at the end of the range, and verifies both the query results and proof verification.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_with_reference_limit_2_desc_at_end();
    /// ```
    fn test_path_query_items_with_reference_limit_2_desc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //        a ------------------------b------------------------c
        //     /      \                 /      \                 /      \
        //   0          1             0          1             0          1
        // /  \        /            /  \        /            /  \        /
        // A1  A2      2            B1  B2      2           C1  C2      2
        //           /  \                     /  \                     /  \
        //        refA1  refA2            refB1  refB2            refC1  refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"m".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![1], vec![2]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with references in a top-level tree returns the first two elements in ascending order from the start, and verifies the proof for correctness.
    ///
    /// This test constructs a hierarchical tree with references, performs a path query with a limit of 2 and ascending order, and asserts that both the query result and the verified proof contain exactly two elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_from_start();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with references in a top-level tree, using a limit of 2 from the start.
    ///
    /// Constructs a hierarchical tree with references under a top-level node, then performs a descending range query limited to two elements. Verifies that both the direct query and proof verification return the expected number of elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_from_start();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_from_start() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"a".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with references in a top-level tree, using a limit of 2 and ascending order from a middle key, returns the correct elements and verifies proof correctness.
    ///
    /// This test constructs a hierarchical tree with references, performs a range query starting from key `b` under the top-level node `1`, and asserts that only two elements are returned in ascending order. It also verifies that the generated proof matches the query results.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_in_middle();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"b".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with references in a top-level tree, using a limit of 2 and a range ending in the middle of the keyspace.
    ///
    /// This test verifies that querying a top-level tree containing references, with a descending order and a limit of 2, correctly returns the expected elements and that the proof generated for this query is valid.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_in_middle();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"b".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with a limit of 2, ascending order, and a range not including middle keys returns the correct referenced elements from a top-level tree.
    ///
    /// This test constructs a hierarchical tree with references, performs a range-to query (excluding keys beyond "f") with a limit of 2, and verifies both the direct query result and the proof verification result for correctness.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_not_included_in_middle();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_not_included_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeTo(..b"f".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with references in a top-level tree, using a limit of 2 and a range that excludes middle keys.
    ///
    /// This test constructs a hierarchical tree with references, performs a descending range-to query with a limit of 2 on a top-level subtree, and verifies that the correct elements are returned and that the proof can be successfully verified.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_not_included_in_middle();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_not_included_in_middle() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeTo(..b"f".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: Some(IndexMap::from([(
                        QueryItem::Key(vec![]),
                        SubqueryBranch {
                            subquery_path: Some(vec![vec![0]]),
                            subquery: Some(Query::new_range_full().into()),
                        },
                    )])),
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();

        let gproof: GroveDBProof = bincode::decode_from_slice(&proof, config)
            .expect("expected no error")
            .0;

        println!("{}", gproof);

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests that a path query with references in a top-level tree, limited to 2 elements in ascending order starting at the end of the range, returns the correct elements and verifies the proof.
    ///
    /// This test constructs a hierarchical tree with references, performs a range query starting from key "m" with a limit of 2, and checks that both the query and its proof return the expected number of elements.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_at_end();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_asc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFrom(b"m".to_vec()..)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }

    #[test]
    /// Tests descending path queries with a limit of 2 at the end of a top-level tree containing references.
    ///
    /// This test constructs a hierarchical tree with references under a top-level node, then performs a descending range query with a limit of 2 at the end of the range. It verifies that the correct elements are returned and that the generated proof can be successfully verified, matching the expected result set.
    ///
    /// # Examples
    ///
    /// ```
    /// test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_at_end();
    /// ```
    fn test_path_query_items_held_in_top_tree_with_refs_limit_2_desc_at_end() {
        // The structure is the following
        // ---------------------------------------------------------->
        //   0  -------------------------------- 1
        //  /  \                       /         |       \
        // A1 .. C2    a ------------------------b------------------------c
        //             |                         |                        |
        //             0                         0                        0
        //            / \                       /  \                    /    \
        //        refA1  refA2              refB1  refB2             refC1   refC2

        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        populate_tree_create_two_by_two_reference_higher_up_hierarchy_with_intermediate_value(&db);

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![vec![1]],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeToInclusive(..=b"m".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![vec![0]]),
                        subquery: Some(Query::new_range_full().into()),
                    },
                    left_to_right: false,
                    conditional_subquery_branches: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let (elements, _) = db
            .query_item_value(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .expect("expected successful get_path_query");

        assert_eq!(elements.len(), 2);

        let proof = db
            .prove_query(&path_query, None, None, grove_version)
            .value
            .expect("expected successful get_path_query");

        let (_, result_set) = GroveDb::verify_query(proof.as_slice(), &path_query, grove_version)
            .expect("should verify proof");
        assert_eq!(result_set.len(), 2);
    }
}
