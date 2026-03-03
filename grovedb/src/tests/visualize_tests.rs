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
    }
}
