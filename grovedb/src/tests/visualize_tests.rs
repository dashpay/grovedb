//! Visualize tests

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;
    use grovedb_visualize::{Drawer, Visualize};

    use crate::{
        tests::{make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element,
    };

    /// Helper: visualize a TempGroveDb into a String.
    fn visualize_to_string(db: &impl Visualize) -> String {
        let mut buf = Vec::new();
        let drawer = Drawer::new(&mut buf);
        db.visualize(drawer).expect("should visualize");
        String::from_utf8(buf).expect("should be valid utf8")
    }

    #[test]
    fn visualize_empty_grovedb() {
        let db = make_empty_grovedb();
        let output = visualize_to_string(&db);
        assert!(
            output.contains("root"),
            "visualization of empty db should contain 'root', got: {}",
            output
        );
    }

    #[test]
    fn visualize_with_items() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert some items under TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item key1");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"key2",
            Element::new_item(b"value2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item key2");

        let output = visualize_to_string(&db);

        assert!(
            output.contains("root"),
            "visualization should contain 'root', got: {}",
            output
        );
        assert!(
            output.contains("key1"),
            "visualization should contain 'key1', got: {}",
            output
        );
        assert!(
            output.contains("key2"),
            "visualization should contain 'key2', got: {}",
            output
        );
    }

    #[test]
    fn visualize_with_nested_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a subtree under TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"nested_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert nested tree");

        // Insert an item inside the nested subtree
        db.insert(
            [TEST_LEAF, b"nested_tree"].as_ref(),
            b"inner_key",
            Element::new_item(b"inner_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item inside nested tree");

        let output = visualize_to_string(&db);

        assert!(
            !output.is_empty(),
            "visualization output should not be empty"
        );
        assert!(
            output.contains("nested_tree"),
            "visualization should contain 'nested_tree', got: {}",
            output
        );
        assert!(
            output.contains("inner_key"),
            "visualization should contain 'inner_key', got: {}",
            output
        );
    }

    #[test]
    fn visualize_with_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a SumTree under TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"my_sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        // Insert SumItems inside the SumTree
        db.insert(
            [TEST_LEAF, b"my_sum_tree"].as_ref(),
            b"sum_key1",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item 1");

        db.insert(
            [TEST_LEAF, b"my_sum_tree"].as_ref(),
            b"sum_key2",
            Element::new_sum_item(25),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item 2");

        let output = visualize_to_string(&db);

        assert!(
            output.contains("root"),
            "visualization should contain 'root', got: {}",
            output
        );
        assert!(
            output.contains("my_sum_tree"),
            "visualization should contain 'my_sum_tree', got: {}",
            output
        );
        assert!(
            output.contains("sum_key1"),
            "visualization should contain 'sum_key1', got: {}",
            output
        );
        assert!(
            output.contains("sum_key2"),
            "visualization should contain 'sum_key2', got: {}",
            output
        );
    }

    /// Exercises the `uses_non_merk_data_storage()` branch in `draw_subtree`
    /// (visualize.rs lines 47-51) by inserting an MmrTree element.
    #[test]
    fn visualize_with_mmr_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an MmrTree under TEST_LEAF — MmrTree is a non-Merk tree type.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"my_mmr",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert mmr tree");

        let output = visualize_to_string(&db);

        assert!(
            output.contains("my_mmr"),
            "visualization should contain 'my_mmr', got: {}",
            output
        );
        // The non-Merk branch writes "[non-Merk tree]" before the element.
        assert!(
            output.contains("[non-Merk tree]"),
            "visualization should contain '[non-Merk tree]' for MmrTree, got: {}",
            output
        );
    }

    /// Exercises the `uses_non_merk_data_storage()` branch with a
    /// BulkAppendTree element.
    #[test]
    fn visualize_with_bulk_append_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a BulkAppendTree under TEST_LEAF.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"my_bulk",
            Element::empty_bulk_append_tree(3), // chunk_power = 3 (epoch size 8)
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert bulk append tree");

        let output = visualize_to_string(&db);

        assert!(
            output.contains("my_bulk"),
            "visualization should contain 'my_bulk', got: {}",
            output
        );
        assert!(
            output.contains("[non-Merk tree]"),
            "visualization should contain '[non-Merk tree]' for BulkAppendTree, got: {}",
            output
        );
    }

    /// Exercises the `uses_non_merk_data_storage()` branch with a
    /// CommitmentTree element.
    #[test]
    fn visualize_with_commitment_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a CommitmentTree under TEST_LEAF.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"my_ct",
            Element::empty_commitment_tree(4), // chunk_power = 4
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert commitment tree");

        let output = visualize_to_string(&db);

        assert!(
            output.contains("my_ct"),
            "visualization should contain 'my_ct', got: {}",
            output
        );
        assert!(
            output.contains("[non-Merk tree]"),
            "visualization should contain '[non-Merk tree]' for CommitmentTree, got: {}",
            output
        );
    }

    /// Exercises the leaf element `else` branch (visualize.rs line 63-64)
    /// with a Reference element. Items are already tested in
    /// `visualize_with_items`, but References are a distinct leaf type.
    #[test]
    fn visualize_with_reference() {
        use crate::reference_path::ReferencePathType;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item that the reference will point to.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"target_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert target item");

        // Insert a reference to that item.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"my_ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert reference");

        let output = visualize_to_string(&db);

        assert!(
            output.contains("my_ref"),
            "visualization should contain 'my_ref', got: {}",
            output
        );
        // The Reference element's Visualize impl writes "ref".
        assert!(
            output.contains("ref"),
            "visualization should contain 'ref' for Reference element, got: {}",
            output
        );
    }

    /// Exercises all three branches of `draw_subtree` in a single tree:
    /// 1. Non-Merk tree (MmrTree) -> "[non-Merk tree]" branch
    /// 2. Regular Merk tree (subtree) -> recursive `draw_subtree` branch
    /// 3. Leaf element (Item) -> `else` branch
    #[test]
    fn visualize_mixed_elements() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Branch 1: non-Merk tree element
        db.insert(
            [TEST_LEAF].as_ref(),
            b"mmr_child",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert mmr tree");

        // Branch 2: regular Merk subtree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert subtree");

        // Branch 3: leaf item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"leaf_item",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert leaf item");

        let output = visualize_to_string(&db);

        // Verify all three element types appear in the visualization.
        assert!(
            output.contains("mmr_child"),
            "visualization should contain 'mmr_child', got: {}",
            output
        );
        assert!(
            output.contains("[non-Merk tree]"),
            "visualization should contain '[non-Merk tree]', got: {}",
            output
        );
        assert!(
            output.contains("sub_tree"),
            "visualization should contain 'sub_tree', got: {}",
            output
        );
        assert!(
            output.contains("leaf_item"),
            "visualization should contain 'leaf_item', got: {}",
            output
        );
        assert!(
            output.contains("item:"),
            "visualization should contain 'item:' for the leaf Item element, got: {}",
            output
        );
    }
}
