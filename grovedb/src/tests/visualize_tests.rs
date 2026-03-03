//! Visualize tests — exercises all three branches of `draw_subtree`:
//! 1. Non-Merk tree (MmrTree) -> "[non-Merk tree]" branch
//! 2. Regular Merk tree (subtree) -> recursive `draw_subtree` branch
//! 3. Leaf element (Item) -> `else` branch

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;
    use grovedb_visualize::{Drawer, Visualize};

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    /// Helper: visualize a TempGroveDb into a String.
    fn visualize_to_string(db: &impl Visualize) -> String {
        let mut buf = Vec::new();
        let drawer = Drawer::new(&mut buf);
        db.visualize(drawer).expect("should visualize");
        String::from_utf8(buf).expect("should be valid utf8")
    }

    /// Single test that exercises all three branches of `draw_subtree` in one
    /// tree, covering all 67 new lines in visualize.rs.
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
