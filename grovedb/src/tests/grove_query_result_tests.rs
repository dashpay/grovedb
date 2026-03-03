//! Tests for GroveTrunkQueryResult and GroveBranchQueryResult methods.
//!
//! These test `trace_key_to_leaf`, `get_ancestor`, `get_node_count`,
//! `collect_path_to_key_with_tree`, and `trace_key_in_tree` by
//! constructing proof Tree structures manually.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use grovedb_merk::{
        proofs::{
            tree::{Child, Tree},
            Node,
        },
        TreeFeatureType,
    };

    use crate::{Element, GroveBranchQueryResult, GroveTrunkQueryResult, LeafInfo};

    // ---------------------------------------------------------------
    // Helper: build a Tree with optional left/right children
    // ---------------------------------------------------------------

    /// Build a leaf-level Tree node (no children) from a KV node.
    fn make_kv_tree(key: &[u8], value: &[u8]) -> Tree {
        Tree::from(Node::KV(key.to_vec(), value.to_vec()))
    }

    /// Build a tree node with KVValueHashFeatureType for count testing.
    fn make_counted_tree(key: &[u8], value: &[u8], count: u64) -> Tree {
        let value_hash = [0u8; 32];
        Tree::from(Node::KVValueHashFeatureType(
            key.to_vec(),
            value.to_vec(),
            value_hash,
            TreeFeatureType::ProvableCountedMerkNode(count),
        ))
    }

    /// Build a tree node with KVCount for count testing.
    fn make_kv_count_tree(key: &[u8], value: &[u8], count: u64) -> Tree {
        Tree::from(Node::KVCount(key.to_vec(), value.to_vec(), count))
    }

    /// Attach a left child to a tree. Returns the tree with child attached.
    fn with_left(mut parent: Tree, child: Tree) -> Tree {
        let child_hash = child.hash().unwrap();
        parent.left = Some(Child {
            tree: Box::new(child),
            hash: child_hash,
        });
        parent.height = 1 + std::cmp::max(
            parent.left.as_ref().map_or(0, |c| c.tree.height),
            parent.right.as_ref().map_or(0, |c| c.tree.height),
        );
        parent.child_heights = (
            parent.left.as_ref().map_or(0, |c| c.tree.height),
            parent.right.as_ref().map_or(0, |c| c.tree.height),
        );
        parent
    }

    /// Attach a right child to a tree. Returns the tree with child attached.
    fn with_right(mut parent: Tree, child: Tree) -> Tree {
        let child_hash = child.hash().unwrap();
        parent.right = Some(Child {
            tree: Box::new(child),
            hash: child_hash,
        });
        parent.height = 1 + std::cmp::max(
            parent.left.as_ref().map_or(0, |c| c.tree.height),
            parent.right.as_ref().map_or(0, |c| c.tree.height),
        );
        parent.child_heights = (
            parent.left.as_ref().map_or(0, |c| c.tree.height),
            parent.right.as_ref().map_or(0, |c| c.tree.height),
        );
        parent
    }

    /// Build a simple BST:
    ///        b"m" (root)
    ///       /         \
    ///     b"d"       b"t"
    ///    /    \      /    \
    ///  b"a"  b"f"  b"p"  b"z"
    ///
    /// Leaf keys (truncated subtrees): b"a", b"f", b"p", b"z"
    fn build_test_tree() -> Tree {
        let a = make_kv_tree(b"a", b"val_a");
        let f = make_kv_tree(b"f", b"val_f");
        let p = make_kv_tree(b"p", b"val_p");
        let z = make_kv_tree(b"z", b"val_z");

        let d = make_kv_tree(b"d", b"val_d");
        let d = with_left(d, a);
        let d = with_right(d, f);

        let t = make_kv_tree(b"t", b"val_t");
        let t = with_left(t, p);
        let t = with_right(t, z);

        let m = make_kv_tree(b"m", b"val_m");
        let m = with_left(m, d);
        with_right(m, t)
    }

    fn build_leaf_keys(keys: &[&[u8]]) -> BTreeMap<Vec<u8>, LeafInfo> {
        let mut map = BTreeMap::new();
        for key in keys {
            map.insert(
                key.to_vec(),
                LeafInfo {
                    hash: [0u8; 32],
                    count: None,
                },
            );
        }
        map
    }

    fn build_elements(keys: &[&[u8]]) -> BTreeMap<Vec<u8>, Element> {
        let mut map = BTreeMap::new();
        for key in keys {
            map.insert(key.to_vec(), Element::new_item(b"val".to_vec()));
        }
        map
    }

    // ===================================================================
    // GroveTrunkQueryResult tests
    // ===================================================================

    #[test]
    fn test_trunk_trace_key_to_leaf_key_in_elements_returns_none() {
        let tree = build_test_tree();
        let leaf_keys = build_leaf_keys(&[b"a", b"f", b"p", b"z"]);
        // b"m" is in elements, so trace_key_to_leaf returns None
        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 3,
            tree,
        };

        assert!(
            result.trace_key_to_leaf(b"m").is_none(),
            "key in elements should return None"
        );
    }

    #[test]
    fn test_trunk_trace_key_to_leaf_finds_leaf() {
        let tree = build_test_tree();
        let leaf_keys = build_leaf_keys(&[b"a", b"f", b"p", b"z"]);
        // Interior nodes are in elements
        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 3,
            tree,
        };

        // Tracing b"a" should find the leaf at b"a"
        let traced = result.trace_key_to_leaf(b"a");
        assert!(traced.is_some(), "should find leaf for key b\"a\"");
        let (leaf_key, _leaf_info) = traced.expect("already checked is_some");
        assert_eq!(leaf_key, b"a".to_vec());
    }

    #[test]
    fn test_trunk_trace_key_to_leaf_navigates_bst_to_correct_leaf() {
        let tree = build_test_tree();
        let leaf_keys = build_leaf_keys(&[b"a", b"f", b"p", b"z"]);
        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 3,
            tree,
        };

        // b"b" < b"d", go left from d, find leaf at b"a"
        let traced = result.trace_key_to_leaf(b"b");
        assert!(traced.is_some(), "should find leaf for key b\"b\"");
        let (leaf_key, _) = traced.expect("already checked is_some");
        assert_eq!(leaf_key, b"a".to_vec(), "b\"b\" should map to leaf b\"a\"");

        // b"r" < b"t", go left from t, find leaf at b"p"
        let traced = result.trace_key_to_leaf(b"r");
        assert!(traced.is_some(), "should find leaf for key b\"r\"");
        let (leaf_key, _) = traced.expect("already checked is_some");
        assert_eq!(leaf_key, b"p".to_vec(), "b\"r\" should map to leaf b\"p\"");
    }

    #[test]
    fn test_trunk_trace_key_not_in_any_subtree() {
        // Tree with no leaf keys at all
        let tree = make_kv_tree(b"m", b"val_m");
        let leaf_keys = BTreeMap::new();
        let elements = build_elements(&[b"m"]);

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 1,
            tree,
        };

        // key not in elements and tree is a single node with no leaves
        let traced = result.trace_key_to_leaf(b"x");
        assert!(
            traced.is_none(),
            "should return None when key not found in tree"
        );
    }

    #[test]
    fn test_trunk_get_ancestor_returns_none_for_unknown_key() {
        let tree = make_kv_tree(b"m", b"val_m");
        let leaf_keys = BTreeMap::new();
        let elements = BTreeMap::new();

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 1,
            tree,
        };

        assert!(
            result.get_ancestor(b"unknown", 10).is_none(),
            "get_ancestor on unknown key should return None"
        );
    }

    #[test]
    fn test_trunk_get_ancestor_with_kv_count_nodes() {
        // Build a tree where interior nodes have KVCount:
        //         b"m" (count=100)
        //        /              \
        //  b"d" (count=50)    b"t" (count=50)
        //     |                  |
        //   b"a" (leaf)       b"z" (leaf)
        let a = make_kv_tree(b"a", b"val_a");
        let z = make_kv_tree(b"z", b"val_z");

        let d = make_kv_count_tree(b"d", b"val_d", 50);
        let d = with_left(d, a);

        let t = make_kv_count_tree(b"t", b"val_t", 50);
        let t = with_right(t, z);

        let m = make_kv_count_tree(b"m", b"val_m", 100);
        let m = with_left(m, d);
        let m = with_right(m, t);

        let elements = BTreeMap::new();
        let leaf_keys = BTreeMap::new();

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 3,
            tree: m,
        };

        // Looking for ancestor of b"a" with min count 40
        // Path: m -> d -> a
        // Walking back: d has count=50 >= 40, so returns d
        let ancestor = result.get_ancestor(b"a", 40);
        assert!(ancestor.is_some(), "should find ancestor with count >= 40");
        let (levels_up, count, key, _hash) = ancestor.expect("already checked");
        assert_eq!(levels_up, 1, "d is 1 level above a");
        assert_eq!(count, 50);
        assert_eq!(key, b"d".to_vec());
    }

    #[test]
    fn test_trunk_get_ancestor_with_provable_counted_feature_type() {
        // Build tree with KVValueHashFeatureType nodes:
        //         b"m" (count=200)
        //        /              \
        //  b"d" (count=5)     b"t" (count=150)
        //     |                  |
        //   b"a"              b"z"
        let a = make_kv_tree(b"a", b"val_a");
        let z = make_kv_tree(b"z", b"val_z");

        let d = make_counted_tree(b"d", b"val_d", 5);
        let d = with_left(d, a);

        let t = make_counted_tree(b"t", b"val_t", 150);
        let t = with_right(t, z);

        let m = make_counted_tree(b"m", b"val_m", 200);
        let m = with_left(m, d);
        let m = with_right(m, t);

        let result = GroveTrunkQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            chunk_depths: vec![],
            max_tree_depth: 3,
            tree: m,
        };

        // Looking for ancestor of b"a" with min count 100
        // Path: m -> d -> a
        // d has count=5 < 100, so skip
        // m is root (index 0), never returned
        // Falls through to path[1] = d
        let ancestor = result.get_ancestor(b"a", 100);
        assert!(ancestor.is_some(), "should return fallback ancestor");
        let (levels_up, count, key, _hash) = ancestor.expect("already checked");
        assert_eq!(key, b"d".to_vec(), "fallback is one below root");
        assert_eq!(levels_up, 1);
        assert_eq!(count, 5, "d has count 5");
    }

    #[test]
    fn test_trunk_get_ancestor_returns_none_when_path_too_short() {
        // Single node tree - leaf is root's direct child (actually IS root)
        let m = make_kv_count_tree(b"m", b"val_m", 100);

        let result = GroveTrunkQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            chunk_depths: vec![],
            max_tree_depth: 1,
            tree: m,
        };

        // Path to b"m" is just [m], so path.len()==1, min_idx==leaf_idx, returns None
        let ancestor = result.get_ancestor(b"m", 10);
        assert!(ancestor.is_none(), "single-node path should return None");
    }

    #[test]
    fn test_trunk_get_ancestor_with_provable_counted_summed() {
        // Test ProvableCountedSummedMerkNode path in get_node_count
        let a = make_kv_tree(b"a", b"val_a");

        let value_hash = [0u8; 32];
        let d = Tree::from(Node::KVValueHashFeatureType(
            b"d".to_vec(),
            b"val_d".to_vec(),
            value_hash,
            TreeFeatureType::ProvableCountedSummedMerkNode(75, 42),
        ));
        let d = with_left(d, a);

        let m = make_kv_count_tree(b"m", b"val_m", 200);
        let m = with_left(m, d);

        let result = GroveTrunkQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            chunk_depths: vec![],
            max_tree_depth: 3,
            tree: m,
        };

        // Path to b"a": m -> d -> a
        // d has ProvableCountedSummedMerkNode(75, 42), count=75
        let ancestor = result.get_ancestor(b"a", 50);
        assert!(ancestor.is_some(), "should find d as ancestor");
        let (_, count, key, _) = ancestor.expect("already checked");
        assert_eq!(key, b"d".to_vec());
        assert_eq!(
            count, 75,
            "should extract count from ProvableCountedSummedMerkNode"
        );
    }

    #[test]
    fn test_trunk_get_node_count_returns_none_for_basic_kv() {
        // A basic KV node has no count.
        // get_node_count is private, but we test it indirectly via get_ancestor
        // If all interior nodes are basic KV, get_ancestor falls back to min_idx
        let a = make_kv_tree(b"a", b"val_a");
        let m = make_kv_tree(b"m", b"val_m");
        let m = with_left(m, a);

        let result = GroveTrunkQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            chunk_depths: vec![],
            max_tree_depth: 2,
            tree: m,
        };

        // get_ancestor for b"a", path is [m, a]
        // m is root (index 0, never returned), a is leaf (index 1)
        // min_idx=1, leaf_idx=1, so min_idx < leaf_idx is false -> None
        let ancestor = result.get_ancestor(b"a", 1);
        assert!(
            ancestor.is_none(),
            "path with 2 nodes: root and leaf, no valid ancestor"
        );
    }

    // ===================================================================
    // GroveBranchQueryResult tests
    // ===================================================================

    #[test]
    fn test_branch_trace_key_to_leaf_key_in_elements_returns_none() {
        let tree = build_test_tree();
        let leaf_keys = build_leaf_keys(&[b"a", b"f", b"p", b"z"]);
        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveBranchQueryResult {
            elements,
            leaf_keys,
            branch_root_hash: [0u8; 32],
            tree,
        };

        assert!(
            result.trace_key_to_leaf(b"d").is_none(),
            "key in elements should return None"
        );
    }

    #[test]
    fn test_branch_trace_key_to_leaf_finds_correct_leaf() {
        let tree = build_test_tree();
        let leaf_keys = build_leaf_keys(&[b"a", b"f", b"p", b"z"]);
        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveBranchQueryResult {
            elements,
            leaf_keys,
            branch_root_hash: [0u8; 32],
            tree,
        };

        // b"z" is a leaf key
        let traced = result.trace_key_to_leaf(b"z");
        assert!(traced.is_some(), "should find leaf for key b\"z\"");
        let (leaf_key, _) = traced.expect("already checked");
        assert_eq!(leaf_key, b"z".to_vec());
    }

    #[test]
    fn test_branch_trace_key_to_leaf_navigates_bst() {
        let tree = build_test_tree();
        let leaf_keys = build_leaf_keys(&[b"a", b"f", b"p", b"z"]);
        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveBranchQueryResult {
            elements,
            leaf_keys,
            branch_root_hash: [0u8; 32],
            tree,
        };

        // b"e" < b"f", go right from d, find leaf at b"f"
        let traced = result.trace_key_to_leaf(b"e");
        assert!(traced.is_some(), "should find leaf for key b\"e\"");
        let (leaf_key, _) = traced.expect("already checked");
        assert_eq!(leaf_key, b"f".to_vec());
    }

    #[test]
    fn test_branch_trace_key_not_found() {
        let tree = make_kv_tree(b"m", b"val_m");
        let leaf_keys = BTreeMap::new();
        let elements = BTreeMap::new();

        let result = GroveBranchQueryResult {
            elements,
            leaf_keys,
            branch_root_hash: [0u8; 32],
            tree,
        };

        // No leaves and not in elements (but IS a node key)
        // key == node_key returns None in trace_key_in_tree
        let traced = result.trace_key_to_leaf(b"m");
        assert!(
            traced.is_none(),
            "key matching node_key but not in leaf_keys should return None"
        );
    }

    #[test]
    fn test_branch_get_ancestor_with_counts() {
        let a = make_kv_tree(b"a", b"val_a");
        let z = make_kv_tree(b"z", b"val_z");

        let d = make_kv_count_tree(b"d", b"val_d", 30);
        let d = with_left(d, a);

        let t = make_kv_count_tree(b"t", b"val_t", 80);
        let t = with_right(t, z);

        let m = make_kv_count_tree(b"m", b"val_m", 200);
        let m = with_left(m, d);
        let m = with_right(m, t);

        let result = GroveBranchQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            branch_root_hash: [0u8; 32],
            tree: m,
        };

        // Ancestor of b"z" with min count 50
        // Path: m -> t -> z
        // t has count=80 >= 50
        let ancestor = result.get_ancestor(b"z", 50);
        assert!(ancestor.is_some(), "should find ancestor with count >= 50");
        let (levels_up, count, key, _hash) = ancestor.expect("already checked");
        assert_eq!(levels_up, 1);
        assert_eq!(count, 80);
        assert_eq!(key, b"t".to_vec());
    }

    #[test]
    fn test_branch_get_ancestor_falls_back_to_one_below_root() {
        let a = make_kv_tree(b"a", b"val_a");
        let z = make_kv_tree(b"z", b"val_z");

        // d has count=2 which is below the threshold
        let d = make_kv_count_tree(b"d", b"val_d", 2);
        let d = with_left(d, a);

        let t = make_kv_count_tree(b"t", b"val_t", 3);
        let t = with_right(t, z);

        let m = make_kv_count_tree(b"m", b"val_m", 500);
        let m = with_left(m, d);
        let m = with_right(m, t);

        let result = GroveBranchQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            branch_root_hash: [0u8; 32],
            tree: m,
        };

        // Ancestor of b"a" with min count 1000 (unreachable)
        // Path: m -> d -> a
        // d count=2 < 1000
        // Falls back to path[1] = d
        let ancestor = result.get_ancestor(b"a", 1000);
        assert!(ancestor.is_some(), "should fall back to one below root");
        let (levels_up, count, key, _hash) = ancestor.expect("already checked");
        assert_eq!(key, b"d".to_vec());
        assert_eq!(levels_up, 1);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_branch_get_ancestor_returns_none_for_unknown_key() {
        let tree = make_kv_tree(b"m", b"val_m");

        let result = GroveBranchQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            branch_root_hash: [0u8; 32],
            tree,
        };

        assert!(
            result.get_ancestor(b"nonexistent", 10).is_none(),
            "unknown key should return None"
        );
    }

    // ===================================================================
    // LeafInfo with actual hash values
    // ===================================================================

    #[test]
    fn test_leaf_info_with_count() {
        let leaf_info = LeafInfo {
            hash: [1u8; 32],
            count: Some(42),
        };
        assert_eq!(leaf_info.count, Some(42));
        assert_eq!(leaf_info.hash, [1u8; 32]);

        // Copy and PartialEq
        let cloned = leaf_info;
        assert_eq!(leaf_info, cloned);
    }

    #[test]
    fn test_leaf_info_without_count() {
        let leaf_info = LeafInfo {
            hash: [2u8; 32],
            count: None,
        };
        assert_eq!(leaf_info.count, None);
    }

    // ===================================================================
    // trace_key_to_leaf with Hash-only nodes
    // ===================================================================

    #[test]
    fn test_trace_key_in_tree_with_hash_node_returns_none() {
        // A Hash node has no key, so key() returns None, trace returns None
        let tree = Tree::from(Node::Hash([99u8; 32]));
        let leaf_keys = BTreeMap::new();
        let elements = BTreeMap::new();

        let result = GroveTrunkQueryResult {
            elements,
            leaf_keys,
            chunk_depths: vec![],
            max_tree_depth: 1,
            tree,
        };

        assert!(
            result.trace_key_to_leaf(b"anything").is_none(),
            "Hash node should return None since it has no key"
        );
    }

    // ===================================================================
    // Edge case: deep tree with multiple levels
    // ===================================================================

    #[test]
    fn test_trunk_get_ancestor_deep_tree() {
        // Build:
        //             b"h" (count=1000)
        //            /
        //         b"d" (count=500)
        //        /
        //     b"b" (count=100)
        //    /
        //  b"a"
        let a = make_kv_tree(b"a", b"val_a");

        let b_node = make_kv_count_tree(b"b", b"val_b", 100);
        let b_node = with_left(b_node, a);

        let d = make_kv_count_tree(b"d", b"val_d", 500);
        let d = with_left(d, b_node);

        let h = make_kv_count_tree(b"h", b"val_h", 1000);
        let h = with_left(h, d);

        let result = GroveTrunkQueryResult {
            elements: BTreeMap::new(),
            leaf_keys: BTreeMap::new(),
            chunk_depths: vec![],
            max_tree_depth: 4,
            tree: h,
        };

        // Path: h -> d -> b -> a
        // Ancestor of b"a" with min count 200
        // Walk back: b(count=100) < 200, d(count=500) >= 200
        let ancestor = result.get_ancestor(b"a", 200);
        assert!(ancestor.is_some(), "should find d as ancestor");
        let (levels_up, count, key, _hash) = ancestor.expect("already checked");
        assert_eq!(key, b"d".to_vec());
        assert_eq!(count, 500);
        assert_eq!(levels_up, 2, "d is 2 levels above a");
    }

    #[test]
    fn test_branch_trace_key_with_leaf_info_hash() {
        // Verify that traced leaf info carries the correct hash
        let tree = build_test_tree();
        let specific_hash = [42u8; 32];
        let mut leaf_keys = BTreeMap::new();
        leaf_keys.insert(
            b"a".to_vec(),
            LeafInfo {
                hash: specific_hash,
                count: Some(7),
            },
        );
        leaf_keys.insert(
            b"f".to_vec(),
            LeafInfo {
                hash: [0u8; 32],
                count: None,
            },
        );
        leaf_keys.insert(
            b"p".to_vec(),
            LeafInfo {
                hash: [0u8; 32],
                count: None,
            },
        );
        leaf_keys.insert(
            b"z".to_vec(),
            LeafInfo {
                hash: [0u8; 32],
                count: None,
            },
        );

        let elements = build_elements(&[b"m", b"d", b"t"]);

        let result = GroveBranchQueryResult {
            elements,
            leaf_keys,
            branch_root_hash: [0u8; 32],
            tree,
        };

        let traced = result.trace_key_to_leaf(b"a").expect("should find leaf a");
        assert_eq!(traced.0, b"a".to_vec());
        assert_eq!(
            traced.1.hash, specific_hash,
            "should carry the specific hash"
        );
        assert_eq!(traced.1.count, Some(7), "should carry the count");
    }
}
