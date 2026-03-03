#[cfg(test)]
mod branch_tests {
    use crate::{
        proofs::branch::{BranchQueryResult, TrunkQueryResult},
        proofs::{Node, Op},
        tree::CryptoHash,
    };

    fn dummy_hash(id: u8) -> CryptoHash {
        let mut h = [0u8; 32];
        h[0] = id;
        h
    }

    /// Build a simple 3-node proof tree:
    ///       B(key=5)
    ///      / \
    ///  Hash   Hash
    ///
    /// This creates a tree where node B has two Hash children (terminals).
    fn simple_trunk_proof() -> Vec<Op> {
        vec![
            Op::Push(Node::Hash(dummy_hash(1))),   // left child (hash)
            Op::Push(Node::KV(vec![5], vec![50])), // root node
            Op::Parent,                            // attach left child
            Op::Push(Node::Hash(dummy_hash(2))),   // right child (hash)
            Op::Child,                             // attach right child
        ]
    }

    /// Build a deeper proof tree:
    ///           D(key=10)
    ///          /         \
    ///      B(key=5)    Hash(3)
    ///     /    \
    ///  Hash(1) Hash(2)
    fn deeper_trunk_proof() -> Vec<Op> {
        vec![
            Op::Push(Node::Hash(dummy_hash(1))),     // B's left child
            Op::Push(Node::KV(vec![5], vec![50])),   // B node
            Op::Parent,                              // B gets left child
            Op::Push(Node::Hash(dummy_hash(2))),     // B's right child
            Op::Child,                               // B gets right child
            Op::Push(Node::KV(vec![10], vec![100])), // D (root) node
            Op::Parent,                              // D gets left child (B subtree)
            Op::Push(Node::Hash(dummy_hash(3))),     // D's right child
            Op::Child,                               // D gets right child
        ]
    }

    /// Build a tree with no Hash children (all real nodes):
    ///       B(key=5)
    ///      /       \
    ///  A(key=2)  C(key=8)
    fn no_terminal_proof() -> Vec<Op> {
        vec![
            Op::Push(Node::KV(vec![2], vec![20])), // A (left child)
            Op::Push(Node::KV(vec![5], vec![50])), // B (root)
            Op::Parent,                            // B gets A as left
            Op::Push(Node::KV(vec![8], vec![80])), // C (right child)
            Op::Child,                             // B gets C as right
        ]
    }

    // ─── TrunkQueryResult::terminal_node_keys ─────────────────────────

    #[test]
    fn terminal_node_keys_simple_trunk() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        let keys = result.terminal_node_keys();
        // Node with key=5 has two Hash children
        assert_eq!(keys, vec![vec![5]]);
    }

    #[test]
    fn terminal_node_keys_deeper_trunk() {
        let result = TrunkQueryResult {
            proof: deeper_trunk_proof(),
            chunk_depths: vec![2],
            tree_depth: 2,
        };
        let keys = result.terminal_node_keys();
        // B(key=5) has Hash children, D(key=10) has B (real) as left and Hash(3) as right
        // Both B and D have at least one Hash child
        assert!(keys.contains(&vec![5]));
        assert!(keys.contains(&vec![10]));
    }

    #[test]
    fn terminal_node_keys_no_terminals() {
        let result = TrunkQueryResult {
            proof: no_terminal_proof(),
            chunk_depths: vec![2],
            tree_depth: 2,
        };
        let keys = result.terminal_node_keys();
        assert!(keys.is_empty());
    }

    #[test]
    fn terminal_node_keys_empty_proof() {
        let result = TrunkQueryResult {
            proof: vec![],
            chunk_depths: vec![],
            tree_depth: 0,
        };
        // Empty proof — execute should fail, returns empty
        let keys = result.terminal_node_keys();
        assert!(keys.is_empty());
    }

    // ─── TrunkQueryResult::trace_key_to_terminal ──────────────────────

    #[test]
    fn trace_key_found_in_proof_returns_none() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        // Key 5 is in the proof itself — not under a terminal
        assert_eq!(result.trace_key_to_terminal(&[5]), None);
    }

    #[test]
    fn trace_key_in_left_hash_subtree() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        // Key 3 < 5, would go left — left is Hash => terminal is key 5
        assert_eq!(result.trace_key_to_terminal(&[3]), Some(vec![5]));
    }

    #[test]
    fn trace_key_in_right_hash_subtree() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        // Key 8 > 5, would go right — right is Hash => terminal is key 5
        assert_eq!(result.trace_key_to_terminal(&[8]), Some(vec![5]));
    }

    #[test]
    fn trace_key_through_deeper_tree() {
        let result = TrunkQueryResult {
            proof: deeper_trunk_proof(),
            chunk_depths: vec![2],
            tree_depth: 2,
        };
        // Key 3 < 10 => go left to B(5), then 3 < 5 => go left, which is Hash(1)
        // => terminal is B(key=5)
        assert_eq!(result.trace_key_to_terminal(&[3]), Some(vec![5]));

        // Key 7: 7 < 10 => go left to B(5), then 7 > 5 => go right, which is Hash(2)
        // => terminal is B(key=5)
        assert_eq!(result.trace_key_to_terminal(&[7]), Some(vec![5]));

        // Key 15: 15 > 10 => go right, which is Hash(3)
        // => terminal is D(key=10)
        assert_eq!(result.trace_key_to_terminal(&[15]), Some(vec![10]));

        // Key 10 is in the proof => None
        assert_eq!(result.trace_key_to_terminal(&[10]), None);
    }

    #[test]
    fn trace_key_no_child_returns_none() {
        // Single-node proof (no children at all)
        let result = TrunkQueryResult {
            proof: vec![Op::Push(Node::KV(vec![5], vec![50]))],
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        // Key 3 < 5, no left child => None
        assert_eq!(result.trace_key_to_terminal(&[3]), None);
        // Key 8 > 5, no right child => None
        assert_eq!(result.trace_key_to_terminal(&[8]), None);
    }

    #[test]
    fn trace_key_empty_proof_returns_none() {
        let result = TrunkQueryResult {
            proof: vec![],
            chunk_depths: vec![],
            tree_depth: 0,
        };
        assert_eq!(result.trace_key_to_terminal(&[5]), None);
    }

    // ─── TrunkQueryResult::verify_terminal_nodes_at_expected_depth ────

    #[test]
    fn verify_terminal_depth_simple_trunk_correct() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        // Hash nodes are at depth 1 (children of root), chunk_depths[0] = 1
        assert!(result.verify_terminal_nodes_at_expected_depth().is_ok());
    }

    #[test]
    fn verify_terminal_depth_wrong_depth() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![3], // expect depth 3, but hashes are at depth 1
            tree_depth: 3,
        };
        assert!(result.verify_terminal_nodes_at_expected_depth().is_err());
    }

    #[test]
    fn verify_terminal_depth_no_hash_nodes() {
        let result = TrunkQueryResult {
            proof: no_terminal_proof(),
            chunk_depths: vec![2],
            tree_depth: 2,
        };
        // No Hash nodes at all — should pass (nothing to verify)
        assert!(result.verify_terminal_nodes_at_expected_depth().is_ok());
    }

    #[test]
    fn verify_terminal_depth_empty_chunk_depths() {
        let result = TrunkQueryResult {
            proof: simple_trunk_proof(),
            chunk_depths: vec![], // empty => expected_depth = 0
            tree_depth: 0,
        };
        // Hashes are at depth 1, expected 0 => should fail
        assert!(result.verify_terminal_nodes_at_expected_depth().is_err());
    }

    // ─── TrunkQueryResult::get_key_from_node ──────────────────────────
    // (tested indirectly through terminal_node_keys, but let's cover
    //  node variants that return None)

    #[test]
    fn terminal_keys_with_kv_value_hash_nodes() {
        // Use KVValueHash nodes instead of KV
        let proof = vec![
            Op::Push(Node::Hash(dummy_hash(1))),
            Op::Push(Node::KVValueHash(vec![5], vec![50], dummy_hash(10))),
            Op::Parent,
            Op::Push(Node::Hash(dummy_hash(2))),
            Op::Child,
        ];
        let result = TrunkQueryResult {
            proof,
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        let keys = result.terminal_node_keys();
        assert_eq!(keys, vec![vec![5]]);
    }

    #[test]
    fn terminal_keys_with_kv_digest_nodes() {
        let proof = vec![
            Op::Push(Node::Hash(dummy_hash(1))),
            Op::Push(Node::KVDigest(vec![5], dummy_hash(10))),
            Op::Parent,
            Op::Push(Node::Hash(dummy_hash(2))),
            Op::Child,
        ];
        let result = TrunkQueryResult {
            proof,
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        let keys = result.terminal_node_keys();
        assert_eq!(keys, vec![vec![5]]);
    }

    #[test]
    fn terminal_keys_with_kv_count_nodes() {
        let proof = vec![
            Op::Push(Node::Hash(dummy_hash(1))),
            Op::Push(Node::KVCount(vec![5], vec![50], 42)),
            Op::Parent,
            Op::Push(Node::Hash(dummy_hash(2))),
            Op::Child,
        ];
        let result = TrunkQueryResult {
            proof,
            chunk_depths: vec![1],
            tree_depth: 1,
        };
        let keys = result.terminal_node_keys();
        assert_eq!(keys, vec![vec![5]]);
    }

    // ─── BranchQueryResult ────────────────────────────────────────────

    #[test]
    fn branch_trace_key_found_returns_none() {
        let result = BranchQueryResult {
            proof: simple_trunk_proof(),
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[5]), None);
    }

    #[test]
    fn branch_trace_key_in_left_subtree() {
        let result = BranchQueryResult {
            proof: simple_trunk_proof(),
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[3]), Some(vec![5]));
    }

    #[test]
    fn branch_trace_key_in_right_subtree() {
        let result = BranchQueryResult {
            proof: simple_trunk_proof(),
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[8]), Some(vec![5]));
    }

    #[test]
    fn branch_trace_key_deeper() {
        let result = BranchQueryResult {
            proof: deeper_trunk_proof(),
            branch_root_key: vec![10],
            returned_depth: 2,
            branch_root_hash: dummy_hash(99),
        };
        // Key 3: 3 < 10 => left to B(5), 3 < 5 => left is Hash(1) => terminal B(5)
        assert_eq!(result.trace_key_to_terminal(&[3]), Some(vec![5]));
        // Key 15: > 10 => right is Hash(3) => terminal D(10)
        assert_eq!(result.trace_key_to_terminal(&[15]), Some(vec![10]));
    }

    #[test]
    fn branch_trace_key_no_child() {
        let result = BranchQueryResult {
            proof: vec![Op::Push(Node::KV(vec![5], vec![50]))],
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[3]), None);
        assert_eq!(result.trace_key_to_terminal(&[8]), None);
    }

    #[test]
    fn branch_trace_key_empty_proof() {
        let result = BranchQueryResult {
            proof: vec![],
            branch_root_key: vec![],
            returned_depth: 0,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[5]), None);
    }

    // ─── BranchQueryResult::get_key_from_node variants ─────────────────

    #[test]
    fn branch_trace_with_kv_ref_value_hash_node() {
        let proof = vec![
            Op::Push(Node::Hash(dummy_hash(1))),
            Op::Push(Node::KVRefValueHash(vec![5], vec![50], dummy_hash(10))),
            Op::Parent,
            Op::Push(Node::Hash(dummy_hash(2))),
            Op::Child,
        ];
        let result = BranchQueryResult {
            proof,
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[3]), Some(vec![5]));
    }

    #[test]
    fn branch_trace_with_kv_digest_count_node() {
        let proof = vec![
            Op::Push(Node::Hash(dummy_hash(1))),
            Op::Push(Node::KVDigestCount(vec![5], dummy_hash(10), 3)),
            Op::Parent,
            Op::Push(Node::Hash(dummy_hash(2))),
            Op::Child,
        ];
        let result = BranchQueryResult {
            proof,
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[8]), Some(vec![5]));
    }

    #[test]
    fn branch_trace_with_kv_ref_value_hash_count_node() {
        let proof = vec![
            Op::Push(Node::Hash(dummy_hash(1))),
            Op::Push(Node::KVRefValueHashCount(
                vec![5],
                vec![50],
                dummy_hash(10),
                7,
            )),
            Op::Parent,
            Op::Push(Node::Hash(dummy_hash(2))),
            Op::Child,
        ];
        let result = BranchQueryResult {
            proof,
            branch_root_key: vec![5],
            returned_depth: 1,
            branch_root_hash: dummy_hash(99),
        };
        assert_eq!(result.trace_key_to_terminal(&[3]), Some(vec![5]));
    }
}
