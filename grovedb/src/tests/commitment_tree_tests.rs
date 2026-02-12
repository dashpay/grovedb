//! Commitment tree integration tests
//!
//! Tests for CommitmentTree as a GroveDB subtree type.

use grovedb_commitment_tree::{Anchor, ExtractedNoteCommitment, Hashable, MerkleHashOrchard};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element,
};

#[test]
fn test_insert_commitment_tree_at_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful commitment tree insert at root");

    let element = db
        .get(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get commitment tree element");

    assert!(element.is_commitment_tree());
}

#[test]
fn test_insert_commitment_tree_under_normal_tree() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a parent normal tree
    db.insert(
        EMPTY_PATH,
        b"shielded_pools",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful tree insert");

    // Insert commitment tree under it
    db.insert(
        [b"shielded_pools"].as_ref(),
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful commitment tree insert under normal tree");

    let element = db
        .get(
            [b"shielded_pools"].as_ref(),
            b"commitments",
            None,
            grove_version,
        )
        .unwrap()
        .expect("should get commitment tree");

    assert!(element.is_commitment_tree());
}

#[test]
fn test_commitment_tree_with_flags() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let flags = Some(vec![1, 2, 3]);

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree_with_flags(flags.clone()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful commitment tree insert with flags");

    let element = db
        .get(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get commitment tree");

    assert!(element.is_commitment_tree());
    assert_eq!(element.get_flags(), &flags);
}

#[test]
fn test_commitment_tree_root_hash_changes_on_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful commitment tree insert");

    let root_hash_after = db.root_hash(None, grove_version).unwrap().unwrap();

    assert_ne!(
        root_hash_before, root_hash_after,
        "Root hash should change when commitment tree is inserted"
    );
}

#[test]
fn test_commitment_tree_nested_pool_structure() {
    // Tests the shielded pool layout:
    //   root -> shielded_pools -> credits -> commitments (CommitmentTree)
    //                                     -> nullifiers  (NormalTree)
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // root -> shielded_pools
    db.insert(
        EMPTY_PATH,
        b"shielded_pools",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert shielded_pools");

    // shielded_pools -> credits
    db.insert(
        [b"shielded_pools"].as_ref(),
        b"credits",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert credits");

    // credits -> commitments (CommitmentTree)
    db.insert(
        [b"shielded_pools".as_slice(), b"credits"].as_ref(),
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // credits -> nullifiers (NormalTree)
    db.insert(
        [b"shielded_pools".as_slice(), b"credits"].as_ref(),
        b"nullifiers",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert nullifiers tree");

    // Verify all elements exist
    let commitments = db
        .get(
            [b"shielded_pools".as_slice(), b"credits"].as_ref(),
            b"commitments",
            None,
            grove_version,
        )
        .unwrap()
        .expect("should get commitments");
    assert!(commitments.is_commitment_tree());

    let nullifiers = db
        .get(
            [b"shielded_pools".as_slice(), b"credits"].as_ref(),
            b"nullifiers",
            None,
            grove_version,
        )
        .unwrap()
        .expect("should get nullifiers");
    assert!(nullifiers.is_any_tree());
    assert!(!nullifiers.is_commitment_tree());
}

#[test]
fn test_commitment_tree_insert_item_inside() {
    // CommitmentTree is stored as a Merk tree in GroveDB, so we can insert
    // items inside it just like a normal tree at this layer.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Insert a key-value item inside the commitment tree
    db.insert(
        [b"commitments"].as_ref(),
        b"leaf_0",
        Element::new_item(vec![1, 2, 3, 4]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item in commitment tree");

    let element = db
        .get([b"commitments"].as_ref(), b"leaf_0", None, grove_version)
        .unwrap()
        .expect("should get item from commitment tree");

    assert_eq!(element.as_item_bytes().unwrap(), &[1, 2, 3, 4]);
}

#[test]
fn test_commitment_tree_root_hash_propagates_on_item_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let root_hash_after_tree = db.root_hash(None, grove_version).unwrap().unwrap();

    // Insert item inside commitment tree
    db.insert(
        [b"commitments"].as_ref(),
        b"leaf_0",
        Element::new_item(vec![1, 2, 3]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    let root_hash_after_item = db.root_hash(None, grove_version).unwrap().unwrap();

    assert_ne!(
        root_hash_after_tree, root_hash_after_item,
        "Root hash should change when item is inserted inside commitment tree"
    );
}

#[test]
fn test_commitment_tree_multiple_items() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Insert multiple items
    for i in 0u32..10 {
        let key = format!("note_{}", i);
        let value = i.to_be_bytes().to_vec();
        db.insert(
            [b"commitments"].as_ref(),
            key.as_bytes(),
            Element::new_item(value),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");
    }

    // Verify all items exist
    for i in 0u32..10 {
        let key = format!("note_{}", i);
        let element = db
            .get(
                [b"commitments"].as_ref(),
                key.as_bytes(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get item");
        assert_eq!(element.as_item_bytes().unwrap(), &i.to_be_bytes());
    }
}

#[test]
fn test_commitment_tree_delete() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Insert an item
    db.insert(
        [b"commitments"].as_ref(),
        b"leaf_0",
        Element::new_item(vec![1, 2, 3]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    // Delete the item
    db.delete(
        [b"commitments"].as_ref(),
        b"leaf_0",
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("delete item from commitment tree");

    // Verify it's gone
    let result = db
        .get([b"commitments"].as_ref(), b"leaf_0", None, grove_version)
        .unwrap();
    assert!(result.is_err());
}

#[test]
fn test_commitment_tree_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();

    let element = Element::empty_commitment_tree();
    let serialized = element
        .serialize(grove_version)
        .expect("should serialize commitment tree");
    let deserialized =
        Element::deserialize(&serialized, grove_version).expect("should deserialize");
    assert_eq!(element, deserialized);

    let with_flags = Element::empty_commitment_tree_with_flags(Some(vec![42]));
    let serialized = with_flags
        .serialize(grove_version)
        .expect("should serialize with flags");
    let deserialized =
        Element::deserialize(&serialized, grove_version).expect("should deserialize");
    assert_eq!(with_flags, deserialized);
}

#[test]
fn test_commitment_tree_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let transaction = db.start_transaction();

    // Insert commitment tree within transaction
    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        Some(&transaction),
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree in transaction");

    // Should be visible within transaction
    let element = db
        .get(
            EMPTY_PATH,
            b"commitments",
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("should get in transaction");
    assert!(element.is_commitment_tree());

    // Should NOT be visible without transaction
    let result = db
        .get(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap();
    assert!(result.is_err());

    // Commit transaction
    db.commit_transaction(transaction)
        .unwrap()
        .expect("commit transaction");

    // Now should be visible without transaction
    let element = db
        .get(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get after commit");
    assert!(element.is_commitment_tree());
}

// =========================================================================
// Sinsemilla commitment tree operations (wired to actual orchard tree)
// =========================================================================

/// Generate a deterministic test leaf as a valid 32-byte Pallas field element.
fn test_leaf_bytes(index: u64) -> [u8; 32] {
    use grovedb_commitment_tree::Level;
    let empty = MerkleHashOrchard::empty_leaf();
    let leaf = MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty);
    leaf.to_bytes()
}

#[test]
fn test_commitment_tree_append_and_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create commitment tree
    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Get root hash of empty tree
    let empty_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get empty root");

    // Append a leaf
    let leaf = test_leaf_bytes(0);
    let (root_after_append, _pos) = db
        .commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
        .unwrap()
        .expect("should append leaf");

    assert_ne!(
        empty_root, root_after_append,
        "root should change after append"
    );

    // Root hash should match what we get from a separate query
    let root_query = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get root");
    assert_eq!(root_after_append, root_query, "root should be consistent");
}

#[test]
fn test_commitment_tree_multiple_appends() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let mut roots = Vec::new();
    for i in 0..5u64 {
        let leaf = test_leaf_bytes(i);
        let (root, _pos) = db
            .commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("should append");
        roots.push(root);
    }

    // Each append should produce a different root
    for i in 1..roots.len() {
        assert_ne!(
            roots[i - 1],
            roots[i],
            "consecutive roots should differ (append {})",
            i
        );
    }

    // Final root should match queried root
    let final_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get final root");
    assert_eq!(roots[4], final_root);
}

#[test]
fn test_commitment_tree_deterministic() {
    let grove_version = GroveVersion::latest();
    let db1 = make_empty_grovedb();
    let db2 = make_empty_grovedb();

    for db in [&db1, &db2] {
        db.insert(
            EMPTY_PATH,
            b"commitments",
            Element::empty_commitment_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    // Append same leaves to both
    for i in 0..3u64 {
        let leaf = test_leaf_bytes(i);
        db1.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append to db1");
        db2.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append to db2");
    }

    let root1 = db1
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("root1");
    let root2 = db2
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("root2");

    assert_eq!(
        root1, root2,
        "identical appends should produce identical roots"
    );
}

#[test]
fn test_commitment_tree_witness_generation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append several leaves
    for i in 0..5u64 {
        let leaf = test_leaf_bytes(i);
        db.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Checkpoint after all appends (witnesses require a checkpoint)
    db.commitment_tree_checkpoint(EMPTY_PATH, b"commitments", 0, None, grove_version)
        .unwrap()
        .expect("checkpoint");

    // Generate witness for position 0
    let witness = db
        .commitment_tree_witness(EMPTY_PATH, b"commitments", 0, None, grove_version)
        .unwrap()
        .expect("should generate witness");

    assert!(
        witness.is_some(),
        "witness should exist for marked position"
    );

    // The witness path should have 32 siblings (tree depth = 32)
    let path = witness.unwrap();
    assert_eq!(path.len(), 32, "Sinsemilla tree depth is 32");

    // Proof round-trip: verify each witness reconstructs the correct root/anchor
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("anchor");
    let root_hash = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("root_hash");

    for i in 0..5u64 {
        let leaf_bytes = test_leaf_bytes(i);
        let cmx = ExtractedNoteCommitment::from_bytes(&leaf_bytes)
            .expect("leaf should be a valid ExtractedNoteCommitment");

        // Via orchard_witness -> MerklePath::root(cmx) == anchor
        let merkle_path = db
            .commitment_tree_orchard_witness(EMPTY_PATH, b"commitments", i, None, grove_version)
            .unwrap()
            .expect("orchard_witness")
            .expect("path should exist");

        assert_eq!(
            merkle_path.root(cmx),
            anchor,
            "orchard witness for position {} should reconstruct anchor",
            i
        );

        // Via prepare_spend -> (Anchor, MerklePath)
        let (spend_anchor, spend_path) = db
            .commitment_tree_prepare_spend(EMPTY_PATH, b"commitments", i, None, grove_version)
            .unwrap()
            .expect("prepare_spend")
            .expect("spend data should exist");

        assert_eq!(
            spend_anchor, anchor,
            "prepare_spend anchor should match commitment_tree_anchor"
        );
        assert_eq!(
            spend_path.root(cmx),
            spend_anchor,
            "prepare_spend path should reconstruct its own anchor"
        );

        // Anchor should be consistent with root_hash
        let root_node = grovedb_commitment_tree::merkle_hash_from_bytes(&root_hash)
            .expect("root_hash should be valid Pallas element");
        assert_eq!(
            anchor,
            Anchor::from(root_node),
            "anchor should match root_hash for position {}",
            i
        );
    }
}

#[test]
fn test_commitment_tree_persist_across_operations() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append leaf 0
    let leaf0 = test_leaf_bytes(0);
    let (root_after_0, _pos) = db
        .commitment_tree_append(EMPTY_PATH, b"commitments", leaf0, None, grove_version)
        .unwrap()
        .expect("append 0");

    // Verify root is persisted
    let queried_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("query root");
    assert_eq!(root_after_0, queried_root);

    // Append leaf 1 (builds on the persisted state)
    let leaf1 = test_leaf_bytes(1);
    let (root_after_1, _pos) = db
        .commitment_tree_append(EMPTY_PATH, b"commitments", leaf1, None, grove_version)
        .unwrap()
        .expect("append 1");

    assert_ne!(root_after_0, root_after_1, "new append should change root");

    // Verify final root
    let final_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("final root");
    assert_eq!(root_after_1, final_root);
}

#[test]
fn test_commitment_tree_nested_with_sinsemilla() {
    // Test the shielded pool layout with actual Sinsemilla operations
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create: root -> shielded_pools -> credits -> commitments
    db.insert(
        EMPTY_PATH,
        b"shielded_pools",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert shielded_pools");

    db.insert(
        [b"shielded_pools"].as_ref(),
        b"credits",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert credits");

    db.insert(
        [b"shielded_pools".as_slice(), b"credits"].as_ref(),
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append leaves to the deeply nested commitment tree
    let path: &[&[u8]] = &[b"shielded_pools", b"credits"];
    for i in 0..3u64 {
        let leaf = test_leaf_bytes(i);
        db.commitment_tree_append(path, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append to nested commitment tree");
    }

    // Checkpoint after all appends (witnesses require a checkpoint)
    db.commitment_tree_checkpoint(path, b"commitments", 0, None, grove_version)
        .unwrap()
        .expect("checkpoint");

    // Verify root hash
    let root = db
        .commitment_tree_root_hash(path, b"commitments", None, grove_version)
        .unwrap()
        .expect("get nested root");

    // Root should not be all zeros (empty tree root bytes)
    assert_ne!(root, [0u8; 32], "root should not be all zeros");

    // Generate witness
    let witness = db
        .commitment_tree_witness(path, b"commitments", 1, None, grove_version)
        .unwrap()
        .expect("witness for nested tree");
    assert!(witness.is_some());
}

#[test]
fn test_commitment_tree_append_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let transaction = db.start_transaction();

    // Append within transaction
    let leaf = test_leaf_bytes(42);
    let (root, _pos) = db
        .commitment_tree_append(
            EMPTY_PATH,
            b"commitments",
            leaf,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("append in transaction");

    // Root from transaction should show the append
    let root_in_tx = db
        .commitment_tree_root_hash(
            EMPTY_PATH,
            b"commitments",
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("root in tx");
    assert_eq!(root, root_in_tx);

    // Root without transaction should still show empty tree
    let root_no_tx = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("root without tx");

    // These should differ since the append hasn't been committed
    assert_ne!(root_in_tx, root_no_tx);

    // Commit
    db.commit_transaction(transaction).unwrap().expect("commit");

    // Now root should match
    let root_after_commit = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("root after commit");
    assert_eq!(root, root_after_commit);
}

// =========================================================================
// Root hash propagation tests
// =========================================================================

#[test]
fn test_commitment_tree_append_propagates_to_grovedb_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    let leaf = test_leaf_bytes(0);
    db.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
        .unwrap()
        .expect("append");

    let root_hash_after = db.root_hash(None, grove_version).unwrap().unwrap();

    assert_ne!(
        root_hash_before, root_hash_after,
        "GroveDB root hash should change after commitment_tree_append"
    );
}

#[test]
fn test_commitment_tree_multiple_appends_propagate() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let mut grovedb_root_hashes = vec![db.root_hash(None, grove_version).unwrap().unwrap()];

    for i in 0..5u64 {
        let leaf = test_leaf_bytes(i);
        db.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append");
        grovedb_root_hashes.push(db.root_hash(None, grove_version).unwrap().unwrap());
    }

    // Each append should change the GroveDB root hash
    for i in 1..grovedb_root_hashes.len() {
        assert_ne!(
            grovedb_root_hashes[i - 1],
            grovedb_root_hashes[i],
            "GroveDB root hash should change after append {}",
            i
        );
    }
}

#[test]
fn test_commitment_tree_propagation_deterministic() {
    let grove_version = GroveVersion::latest();
    let db1 = make_empty_grovedb();
    let db2 = make_empty_grovedb();

    for db in [&db1, &db2] {
        db.insert(
            EMPTY_PATH,
            b"commitments",
            Element::empty_commitment_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    for i in 0..3u64 {
        let leaf = test_leaf_bytes(i);
        db1.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append to db1");
        db2.commitment_tree_append(EMPTY_PATH, b"commitments", leaf, None, grove_version)
            .unwrap()
            .expect("append to db2");
    }

    let root1 = db1.root_hash(None, grove_version).unwrap().unwrap();
    let root2 = db2.root_hash(None, grove_version).unwrap().unwrap();

    assert_eq!(
        root1, root2,
        "identical appends should produce identical GroveDB root hashes"
    );
}

#[test]
fn test_commitment_tree_nested_append_propagates() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create: root -> shielded_pools -> credits -> commitments
    db.insert(
        EMPTY_PATH,
        b"shielded_pools",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert shielded_pools");

    db.insert(
        [b"shielded_pools".as_slice()].as_ref(),
        b"credits",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert credits");

    let ct_path: &[&[u8]] = &[b"shielded_pools", b"credits"];
    db.insert(
        ct_path,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    let leaf = test_leaf_bytes(0);
    db.commitment_tree_append(ct_path, b"commitments", leaf, None, grove_version)
        .unwrap()
        .expect("append to nested commitment tree");

    let root_hash_after = db.root_hash(None, grove_version).unwrap().unwrap();

    assert_ne!(
        root_hash_before, root_hash_after,
        "GroveDB root hash should change after nested commitment_tree_append"
    );
}

// ---- Batch operation tests ----

#[test]
fn test_batch_commitment_tree_append() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create commitment tree
    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    // Append via batch
    let leaf = test_leaf_bytes(0);
    let ops = vec![QualifiedGroveDbOp::commitment_tree_append_op(
        vec![],
        b"commitments".to_vec(),
        leaf,
    )];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch append should succeed");

    let root_hash_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(
        root_hash_before, root_hash_after,
        "GroveDB root hash should change after batch append"
    );

    // Verify the Sinsemilla root also changed
    let sinsemilla_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get root hash");
    assert_ne!(
        sinsemilla_root, [0u8; 32],
        "Sinsemilla root should not be empty after append"
    );
}

#[test]
fn test_batch_commitment_tree_multiple_appends_same_key() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create commitment tree
    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append 5 leaves in a single batch
    let ops: Vec<QualifiedGroveDbOp> = (0..5u64)
        .map(|i| {
            QualifiedGroveDbOp::commitment_tree_append_op(
                vec![],
                b"commitments".to_vec(),
                test_leaf_bytes(i),
            )
        })
        .collect();

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch with 5 appends should succeed");

    // Compare with sequential single appends on a separate DB
    let db2 = make_empty_grovedb();
    db2.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    for i in 0..5u64 {
        db2.commitment_tree_append(
            EMPTY_PATH,
            b"commitments",
            test_leaf_bytes(i),
            None,
            grove_version,
        )
        .unwrap()
        .expect("sequential append");
    }

    // Both should produce the same Sinsemilla root
    let batch_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("batch root");
    let seq_root = db2
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("sequential root");

    assert_eq!(
        batch_root, seq_root,
        "batch and sequential appends should produce the same Sinsemilla root"
    );

    // Both should produce the same GroveDB root
    let batch_grovedb_root = db.root_hash(None, grove_version).unwrap().unwrap();
    let seq_grovedb_root = db2.root_hash(None, grove_version).unwrap().unwrap();

    assert_eq!(
        batch_grovedb_root, seq_grovedb_root,
        "batch and sequential appends should produce the same GroveDB root"
    );
}

#[test]
fn test_batch_commitment_tree_append_with_other_ops() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create commitment tree and a sibling tree
    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    db.insert(
        EMPTY_PATH,
        b"data",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert data tree");

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    // Mix commitment tree append with a normal insert in one batch
    let leaf = test_leaf_bytes(42);
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"data".to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"value1".to_vec()),
        ),
        QualifiedGroveDbOp::commitment_tree_append_op(vec![], b"commitments".to_vec(), leaf),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("mixed batch should succeed");

    let root_hash_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_hash_before, root_hash_after);

    // Verify both operations took effect
    let item = db
        .get([b"data"].as_ref(), b"key1", None, grove_version)
        .unwrap()
        .expect("should get inserted item");
    assert_eq!(item, Element::new_item(b"value1".to_vec()));

    let sinsemilla_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should get root hash");
    assert_ne!(sinsemilla_root, [0u8; 32]);
}

#[test]
fn test_batch_commitment_tree_append_nonexistent_tree_fails() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Don't create any commitment tree - just try to append
    let leaf = test_leaf_bytes(0);
    let ops = vec![QualifiedGroveDbOp::commitment_tree_append_op(
        vec![],
        b"nonexistent".to_vec(),
        leaf,
    )];

    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        result.is_err(),
        "batch append to nonexistent tree should fail"
    );
}

#[test]
fn test_batch_commitment_tree_append_on_non_commitment_tree_fails() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a normal tree, not a commitment tree
    db.insert(
        EMPTY_PATH,
        b"normal_tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert normal tree");

    let leaf = test_leaf_bytes(0);
    let ops = vec![QualifiedGroveDbOp::commitment_tree_append_op(
        vec![],
        b"normal_tree".to_vec(),
        leaf,
    )];

    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        result.is_err(),
        "batch append to non-commitment tree should fail"
    );
}

// ==================== Phase 2: Lifecycle Integration Tests
// ====================

#[test]
fn test_commitment_tree_position_tracking() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Empty tree should have no position
    let pos = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("query position");
    assert_eq!(pos, None, "empty tree should have no position");

    // Append first leaf -> position 0
    let (_, pos0) = db
        .commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(0), None, grove_version)
        .unwrap()
        .expect("append 0");
    assert_eq!(pos0, 0, "first leaf should be at position 0");

    // Append second leaf -> position 1
    let (_, pos1) = db
        .commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(1), None, grove_version)
        .unwrap()
        .expect("append 1");
    assert_eq!(pos1, 1, "second leaf should be at position 1");

    // Append third leaf -> position 2
    let (_, pos2) = db
        .commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(2), None, grove_version)
        .unwrap()
        .expect("append 2");
    assert_eq!(pos2, 2, "third leaf should be at position 2");

    // Querying current end position should agree
    let queried_pos = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("query position");
    assert_eq!(
        queried_pos,
        Some(2),
        "end position should be 2 after 3 appends"
    );
}

#[test]
fn test_commitment_tree_empty_position() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    let pos = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("query position on empty tree");
    assert_eq!(
        pos, None,
        "empty commitment tree should return None for position"
    );
}

#[test]
fn test_commitment_tree_nullifier_pattern() {
    // Demonstrates the complete note lifecycle at the GroveDB level:
    // 1. Create pool (CommitmentTree) and nullifier set (NormalTree)
    // 2. Append note commitment -> get position
    // 3. Generate witness for that position
    // 4. "Spend" by inserting nullifier as Item
    // 5. Check double-spend: has_raw returns true -> reject
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create the pool structure
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    db.insert(
        EMPTY_PATH,
        b"nullifiers",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert nullifier tree");

    // Step 1: Append a note commitment (simulating a deposit/receive)
    let note_commitment = test_leaf_bytes(42);
    let (root_hash, position) = db
        .commitment_tree_append(EMPTY_PATH, b"pool", note_commitment, None, grove_version)
        .unwrap()
        .expect("append note commitment");

    assert_eq!(position, 0, "first note should be at position 0");
    assert_ne!(root_hash, [0u8; 32], "root hash should be non-zero");

    // Checkpoint after append (witnesses require a checkpoint)
    db.commitment_tree_checkpoint(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("checkpoint");

    // Step 2: Generate witness for the note (needed for spending)
    let witness = db
        .commitment_tree_witness(EMPTY_PATH, b"pool", position, None, grove_version)
        .unwrap()
        .expect("witness generation should succeed");
    assert!(
        witness.is_some(),
        "witness should exist for marked position"
    );
    let witness_path = witness.unwrap();
    assert_eq!(witness_path.len(), 32, "witness path should have 32 levels");

    // Step 3: "Spend" the note by publishing its nullifier
    // In the real system, the nullifier is derived from the note's nullifier key +
    // commitment Here we simulate it with a deterministic value
    let nullifier_bytes = b"nullifier_for_note_42";

    // Check nullifier doesn't exist yet (not already spent)
    let already_spent = db
        .has_raw(
            [b"nullifiers"].as_ref(),
            nullifier_bytes.as_slice(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("should query nullifier existence");
    assert!(!already_spent, "nullifier should not exist before spending");

    // Insert nullifier (marks the note as spent)
    db.insert(
        [b"nullifiers"].as_ref(),
        nullifier_bytes.as_slice(),
        Element::new_item(vec![]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert nullifier");

    // Step 4: Attempt double-spend -> should be rejected
    let double_spend_check = db
        .has_raw(
            [b"nullifiers"].as_ref(),
            nullifier_bytes.as_slice(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("should query nullifier existence");
    assert!(
        double_spend_check,
        "nullifier should exist after spending - double spend detected"
    );
}

#[test]
fn test_commitment_tree_multi_pool_pattern() {
    // Demonstrates the multi-pool architecture:
    // - One main credit pool
    // - One shared token pool
    // - Per-token pools as needed
    // Each pool is an independent CommitmentTree at its own path.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create pool structure
    db.insert(
        EMPTY_PATH,
        b"pools",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pools container");

    // Main credit pool
    db.insert(
        [b"pools"].as_ref(),
        b"credit_pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert credit pool");

    // Shared token pool
    db.insert(
        [b"pools"].as_ref(),
        b"shared_token_pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert shared token pool");

    // Per-token pool (e.g., for a specific token contract)
    db.insert(
        [b"pools"].as_ref(),
        b"token_abc",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert token-specific pool");

    let pools_path: &[&[u8]] = &[b"pools"];

    // Append to each pool independently
    let (credit_root, credit_pos) = db
        .commitment_tree_append(
            pools_path,
            b"credit_pool",
            test_leaf_bytes(100),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append to credit pool");

    let (shared_root, shared_pos) = db
        .commitment_tree_append(
            pools_path,
            b"shared_token_pool",
            test_leaf_bytes(200),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append to shared token pool");

    let (token_root, token_pos) = db
        .commitment_tree_append(
            pools_path,
            b"token_abc",
            test_leaf_bytes(300),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append to token pool");

    // All positions should be 0 (first append to each)
    assert_eq!(credit_pos, 0);
    assert_eq!(shared_pos, 0);
    assert_eq!(token_pos, 0);

    // Each pool has independent root hashes (different leaves)
    assert_ne!(credit_root, shared_root);
    assert_ne!(credit_root, token_root);
    assert_ne!(shared_root, token_root);

    // Batch: append to multiple pools atomically
    let batch_ops = vec![
        QualifiedGroveDbOp::commitment_tree_append_op(
            vec![b"pools".to_vec()],
            b"credit_pool".to_vec(),
            test_leaf_bytes(101),
        ),
        QualifiedGroveDbOp::commitment_tree_append_op(
            vec![b"pools".to_vec()],
            b"shared_token_pool".to_vec(),
            test_leaf_bytes(201),
        ),
        QualifiedGroveDbOp::commitment_tree_append_op(
            vec![b"pools".to_vec()],
            b"token_abc".to_vec(),
            test_leaf_bytes(301),
        ),
    ];

    db.apply_batch(batch_ops, None, None, grove_version)
        .unwrap()
        .expect("batch append to multiple pools");

    // Verify all pools advanced
    let credit_end = db
        .commitment_tree_current_end_position(pools_path, b"credit_pool", None, grove_version)
        .unwrap()
        .expect("query credit position");
    assert_eq!(credit_end, Some(1), "credit pool should have 2 leaves");

    let shared_end = db
        .commitment_tree_current_end_position(pools_path, b"shared_token_pool", None, grove_version)
        .unwrap()
        .expect("query shared position");
    assert_eq!(shared_end, Some(1), "shared pool should have 2 leaves");

    let token_end = db
        .commitment_tree_current_end_position(pools_path, b"token_abc", None, grove_version)
        .unwrap()
        .expect("query token position");
    assert_eq!(token_end, Some(1), "token pool should have 2 leaves");

    // Roots should have changed from the batch appends
    let new_credit_root = db
        .commitment_tree_root_hash(pools_path, b"credit_pool", None, grove_version)
        .unwrap()
        .expect("query credit root");
    assert_ne!(
        credit_root, new_credit_root,
        "credit root should change after batch"
    );
}

// ==================== Phase 3: Convenience API Tests ====================

#[test]
fn test_commitment_tree_anchor_operation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Empty tree anchor should match Anchor::empty_tree()
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("should get anchor of empty tree");
    assert_eq!(
        anchor,
        Anchor::empty_tree(),
        "empty tree anchor should match"
    );

    // Append leaves and verify anchor changes
    for i in 0..3u64 {
        db.commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(i), None, grove_version)
            .unwrap()
            .expect("append");
    }

    let anchor_after = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("should get anchor after appends");
    assert_ne!(
        anchor_after,
        Anchor::empty_tree(),
        "anchor should change after appends"
    );

    // Anchor should be consistent with root_hash
    let root_hash = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root hash");

    // Anchor wraps the root hash — verify consistency by converting root_hash back
    let root_node =
        grovedb_commitment_tree::merkle_hash_from_bytes(&root_hash).expect("valid Pallas element");
    let expected_anchor = Anchor::from(root_node);
    assert_eq!(
        anchor_after, expected_anchor,
        "anchor should match root hash"
    );
}

#[test]
fn test_commitment_tree_orchard_witness_operation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Append several leaves
    let mut leaves = Vec::new();
    for i in 0..5u64 {
        let leaf_bytes = test_leaf_bytes(i);
        leaves.push(leaf_bytes);
        db.commitment_tree_append(EMPTY_PATH, b"pool", leaf_bytes, None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Checkpoint after all appends (witnesses require a checkpoint)
    db.commitment_tree_checkpoint(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("checkpoint");

    // Get anchor for verification
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor");

    // Generate orchard witness for each position and verify path.root(cmx) ==
    // anchor
    for (i, leaf_bytes) in leaves.iter().enumerate() {
        let merkle_path = db
            .commitment_tree_orchard_witness(EMPTY_PATH, b"pool", i as u64, None, grove_version)
            .unwrap()
            .expect("should generate orchard witness");

        assert!(
            merkle_path.is_some(),
            "witness should exist for position {}",
            i
        );
        let path = merkle_path.unwrap();

        // Verify inclusion: path.root(cmx) should equal the anchor
        let cmx = ExtractedNoteCommitment::from_bytes(leaf_bytes)
            .expect("leaf should be valid ExtractedNoteCommitment");
        let computed_anchor = path.root(cmx);
        assert_eq!(
            computed_anchor, anchor,
            "witness for position {} should produce correct anchor",
            i
        );
    }
}

#[test]
fn test_commitment_tree_prepare_spend() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Append a note commitment and record its position
    let note_bytes = test_leaf_bytes(42);
    let (_root, position) = db
        .commitment_tree_append(EMPTY_PATH, b"pool", note_bytes, None, grove_version)
        .unwrap()
        .expect("append note");
    assert_eq!(position, 0);

    // Checkpoint after append (witnesses require a checkpoint)
    db.commitment_tree_checkpoint(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("checkpoint");

    // Prepare spend: get (Anchor, MerklePath) in one call
    let spend_data = db
        .commitment_tree_prepare_spend(EMPTY_PATH, b"pool", position, None, grove_version)
        .unwrap()
        .expect("prepare spend should succeed");

    assert!(
        spend_data.is_some(),
        "spend data should exist for marked position"
    );
    let (anchor, merkle_path) = spend_data.unwrap();

    // Verify the anchor matches what we'd get separately
    let separate_anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("separate anchor");
    assert_eq!(
        anchor, separate_anchor,
        "anchor from prepare_spend should match"
    );

    // Verify inclusion proof
    let cmx = ExtractedNoteCommitment::from_bytes(&note_bytes).expect("valid commitment");
    let computed_anchor = merkle_path.root(cmx);
    assert_eq!(
        computed_anchor, anchor,
        "MerklePath should reconstruct the anchor"
    );
}

#[test]
fn test_commitment_tree_prepare_spend_multi_note() {
    // Tests prepare_spend with multiple notes in the pool
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Append several notes
    let mut notes: Vec<([u8; 32], u64)> = Vec::new();
    for i in 0..10u64 {
        let leaf = test_leaf_bytes(i);
        let (_root, pos) = db
            .commitment_tree_append(EMPTY_PATH, b"pool", leaf, None, grove_version)
            .unwrap()
            .expect("append");
        notes.push((leaf, pos));
    }

    // Checkpoint after all appends (witnesses require a checkpoint)
    db.commitment_tree_checkpoint(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("checkpoint");

    // Prepare spend for several positions — all should return the same anchor
    let mut anchors = Vec::new();
    for &(leaf_bytes, pos) in &notes {
        let (anchor, path) = db
            .commitment_tree_prepare_spend(EMPTY_PATH, b"pool", pos, None, grove_version)
            .unwrap()
            .expect("prepare spend")
            .expect("spend data should exist");

        // Verify inclusion
        let cmx = ExtractedNoteCommitment::from_bytes(&leaf_bytes).expect("valid commitment");
        assert_eq!(
            path.root(cmx),
            anchor,
            "inclusion proof should verify for position {}",
            pos
        );

        anchors.push(anchor);
    }

    // All anchors should be identical (same tree state)
    for anchor in &anchors[1..] {
        assert_eq!(*anchor, anchors[0], "all anchors should be identical");
    }
}

#[test]
fn test_commitment_tree_reexports_compile() {
    // Compile-time check: verify key orchard types are accessible through
    // the grovedb-commitment-tree re-exports.
    use grovedb_commitment_tree::{
        redpallas,
        // Bundle/Action types
        Action,
        // Already-imported types (verify they still work)
        Anchor,
        Authorized,
        BatchValidator,
        // Builder types
        Builder,
        Bundle,
        BundleType,
        ExtractedNoteCommitment,
        Flags,
        // Key types
        FullViewingKey,
        IncomingViewingKey,
        MerkleHashOrchard,
        MerklePath,
        Note,
        // Note types
        NoteValue,
        OutgoingViewingKey,
        PaymentAddress,
        // Proof types
        Proof,
        ProvingKey,
        Rho,
        Scope,
        SpendAuthorizingKey,
        SpendValidatingKey,
        SpendingKey,
        // Bundle reconstruction types
        TransmittedNoteCiphertext,
        ValueCommitment,
        VerifyingKey,
    };

    // Type assertions to ensure the re-exports resolve correctly
    fn _assert_types() {
        let _: fn() -> Anchor = Anchor::empty_tree;
        let _: Option<&Proof> = None;
        let _: Option<&ProvingKey> = None;
        let _: Option<&VerifyingKey> = None;
        let _: Option<&Note> = None;
        let _: Option<&PaymentAddress> = None;
        let _: Option<&Rho> = None;
        let _: Option<&NoteValue> = None;
        let _: Option<&SpendingKey> = None;
        let _: Option<&FullViewingKey> = None;
        let _: Option<&SpendAuthorizingKey> = None;
        let _: Option<&SpendValidatingKey> = None;
        let _: Option<&IncomingViewingKey> = None;
        let _: Option<&OutgoingViewingKey> = None;
        let _: Scope = Scope::External;
        let _: Option<&BundleType> = None;
        let _: Option<&Builder> = None;
        let _: Option<&Flags> = None;
        let _: Option<&Bundle<Authorized, i64>> = None;
        let _: Option<&Action<()>> = None;
        let _: Option<&ExtractedNoteCommitment> = None;
        let _: Option<&MerklePath> = None;
        let _: Option<&MerkleHashOrchard> = None;
        // New re-exports for bundle verification
        let _: Option<&BatchValidator> = None;
        let _: Option<&TransmittedNoteCiphertext> = None;
        let _: Option<&ValueCommitment> = None;
        let _: Option<&redpallas::Signature<redpallas::SpendAuth>> = None;
        let _: Option<&redpallas::Signature<redpallas::Binding>> = None;
        let _: Option<&redpallas::VerificationKey<redpallas::SpendAuth>> = None;
    }
}

// ==================== Audit Coverage Gap Tests ====================

#[test]
fn test_commitment_tree_proof_system_integration() {
    // Verifies that GroveDB proofs work correctly with CommitmentTree elements.
    // Proves the existence of a CommitmentTree element via
    // prove_query/verify_query.
    use grovedb_merk::proofs::Query;

    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create structure: root -> pool (CommitmentTree)
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Append some leaves so the tree is non-empty
    for i in 0..3u64 {
        db.commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(i), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Prove the CommitmentTree element exists at root -> "pool"
    let mut query = Query::new();
    query.insert_key(b"pool".to_vec());
    let path_query = crate::PathQuery::new_unsized(vec![], query);

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();

    let (root_hash, result_set) =
        crate::GroveDb::verify_query_raw(&proof, &path_query, grove_version).unwrap();

    // Root hash from proof should match the DB's root hash
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "proof root hash should match DB root hash"
    );

    // Result set should contain exactly one element (the CommitmentTree)
    assert_eq!(
        result_set.len(),
        1,
        "should have one result for the pool key"
    );

    // Verify the element is a CommitmentTree by deserializing
    let proved = &result_set[0];
    assert_eq!(proved.key, b"pool", "key should be 'pool'");
    let element = Element::deserialize(&proved.value, grove_version)
        .expect("should deserialize CommitmentTree");
    assert!(
        element.is_commitment_tree(),
        "deserialized element should be a CommitmentTree"
    );
}

#[test]
fn test_commitment_tree_delete_and_recreate() {
    // Verifies that deleting a CommitmentTree cleans up aux storage,
    // and a freshly recreated tree starts empty (no stale data).
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create and populate a commitment tree
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    for i in 0..5u64 {
        db.commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(i), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Record the Sinsemilla root before deletion
    let root_before_delete = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root before delete");
    assert_ne!(root_before_delete, [0u8; 32]);

    // Delete the commitment tree
    db.delete(EMPTY_PATH, b"pool", None, None, grove_version)
        .unwrap()
        .expect("delete pool");

    // Verify it's gone
    let result = db.get(EMPTY_PATH, b"pool", None, grove_version).unwrap();
    assert!(result.is_err(), "pool should not exist after deletion");

    // Recreate the commitment tree
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("re-insert pool");

    // The recreated tree should be empty (no stale aux data)
    let root_after_recreate = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root after recreate");

    // Empty tree root should be the Orchard empty root
    let empty_root_node = grovedb_commitment_tree::MerkleHashOrchard::empty_root(
        grovedb_commitment_tree::Level::from(
            grovedb_commitment_tree::NOTE_COMMITMENT_TREE_DEPTH as u8,
        ),
    );
    let empty_root_bytes = empty_root_node.to_bytes();

    assert_eq!(
        root_after_recreate, empty_root_bytes,
        "recreated tree should have empty root (no stale aux data)"
    );
    assert_ne!(
        root_after_recreate, root_before_delete,
        "recreated tree should differ from the populated one"
    );

    // Verify we can append fresh data
    let (new_root, pos) = db
        .commitment_tree_append(
            EMPTY_PATH,
            b"pool",
            test_leaf_bytes(99),
            None,
            grove_version,
        )
        .unwrap()
        .expect("append to recreated tree");
    assert_eq!(
        pos, 0,
        "first append to recreated tree should be at position 0"
    );
    assert_ne!(new_root, empty_root_bytes);
}

#[test]
fn test_commitment_tree_transaction_rollback() {
    // Verifies that rolling back a transaction undoes commitment tree appends.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Append a leaf without a transaction (committed)
    let (root_after_first, _) = db
        .commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(0), None, grove_version)
        .unwrap()
        .expect("append 0");

    // Start transaction and append more
    let transaction = db.start_transaction();

    let (root_in_tx, _) = db
        .commitment_tree_append(
            EMPTY_PATH,
            b"pool",
            test_leaf_bytes(1),
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("append 1 in tx");

    assert_ne!(root_after_first, root_in_tx, "tx append should change root");

    // Rollback the transaction
    db.rollback_transaction(&transaction).unwrap();

    // Root should be back to root_after_first (pre-transaction state)
    let root_after_rollback = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root after rollback");

    assert_eq!(
        root_after_first, root_after_rollback,
        "rollback should undo commitment tree append"
    );

    // Position should still be 0 (only one committed leaf)
    let pos = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("position after rollback");
    assert_eq!(
        pos,
        Some(0),
        "position should be 0 after rollback (only first append committed)"
    );
}

#[test]
fn test_commitment_tree_invalid_pallas_element_rejected() {
    // Verifies that appending bytes that are not a valid Pallas field element
    // is rejected.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // The Pallas field modulus is less than 2^255. An all-0xFF value exceeds it
    // and should be rejected as an invalid field element.
    let invalid_leaf = [0xFF; 32];

    let result = db
        .commitment_tree_append(EMPTY_PATH, b"pool", invalid_leaf, None, grove_version)
        .unwrap();

    assert!(
        result.is_err(),
        "appending an invalid Pallas field element should fail"
    );

    // The tree should still be empty
    let pos = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("position query");
    assert_eq!(
        pos, None,
        "tree should still be empty after rejected append"
    );
}

#[test]
fn test_commitment_tree_element_type_confusion() {
    // Verifies that commitment tree operations fail when called on the wrong
    // element type, and that overwriting a normal tree with a commitment tree
    // (or vice versa) works correctly.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create a normal tree
    db.insert(
        EMPTY_PATH,
        b"mytree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert normal tree");

    // Commitment tree operations on a normal tree should fail
    let result = db
        .commitment_tree_append(
            EMPTY_PATH,
            b"mytree",
            test_leaf_bytes(0),
            None,
            grove_version,
        )
        .unwrap();
    assert!(result.is_err(), "append on normal tree should fail");

    let result = db
        .commitment_tree_root_hash(EMPTY_PATH, b"mytree", None, grove_version)
        .unwrap();
    assert!(result.is_err(), "root_hash on normal tree should fail");

    let result = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"mytree", None, grove_version)
        .unwrap();
    assert!(
        result.is_err(),
        "current_end_position on normal tree should fail"
    );

    let result = db
        .commitment_tree_anchor(EMPTY_PATH, b"mytree", None, grove_version)
        .unwrap();
    assert!(result.is_err(), "anchor on normal tree should fail");
}

#[test]
fn test_commitment_tree_corrupted_aux_data() {
    // Verifies that corrupted aux storage data produces a clear error
    // rather than a panic. We write garbage bytes to the aux storage key
    // and then try to use the commitment tree.
    use grovedb_storage::{Storage, StorageContext};

    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Write corrupted data directly to aux storage using the same pattern
    // as commitment_tree.rs: immediate storage context with a transaction.
    let transaction = db.start_transaction();

    let path_vec: Vec<Vec<u8>> = vec![b"pool".to_vec()];
    let path_refs: Vec<&[u8]> = path_vec.iter().map(|v| v.as_slice()).collect();
    let ct_path = grovedb_path::SubtreePath::from(path_refs.as_slice());

    let storage_ctx = db
        .db
        .get_immediate_storage_context(ct_path, &transaction)
        .unwrap();

    let _ = storage_ctx
        .put_aux(b"__ct_data__", &[0xDE, 0xAD, 0xBE, 0xEF], None)
        .unwrap();

    #[allow(clippy::drop_non_drop)]
    drop(storage_ctx);
    db.commit_transaction(transaction)
        .unwrap()
        .expect("commit corrupted data");

    // Now try to read the commitment tree — should get an error, not a panic
    let result = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap();
    assert!(
        result.is_err(),
        "corrupted aux data should cause a deserialization error"
    );

    let result = db
        .commitment_tree_current_end_position(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap();
    assert!(
        result.is_err(),
        "corrupted aux data should cause a deserialization error"
    );
}

#[test]
fn test_batch_commitment_tree_checkpoint() {
    // Verifies that checkpoint operations work correctly in batch mode.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Batch: append 5 leaves then checkpoint
    let mut ops: Vec<QualifiedGroveDbOp> = (0..5u64)
        .map(|i| {
            QualifiedGroveDbOp::commitment_tree_append_op(
                vec![],
                b"pool".to_vec(),
                test_leaf_bytes(i),
            )
        })
        .collect();

    ops.push(QualifiedGroveDbOp::commitment_tree_checkpoint_op(
        vec![],
        b"pool".to_vec(),
        0,
    ));

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch append + checkpoint");

    // Verify witnesses work (they require a checkpoint)
    let witness = db
        .commitment_tree_witness(EMPTY_PATH, b"pool", 2, None, grove_version)
        .unwrap()
        .expect("witness after batch checkpoint");
    assert!(
        witness.is_some(),
        "witness should exist after batch checkpoint"
    );
    assert_eq!(witness.unwrap().len(), 32, "witness should have 32 levels");

    // Verify root hash and GroveDB root hash consistency
    let sinsemilla_root = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_ne!(sinsemilla_root, [0u8; 32], "root should not be empty");

    // Proof round-trip: reconstruct root from witness for each batch-appended leaf
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor");

    for i in 0..5u64 {
        let leaf_bytes = test_leaf_bytes(i);
        let cmx = ExtractedNoteCommitment::from_bytes(&leaf_bytes)
            .expect("leaf should be valid ExtractedNoteCommitment");

        let merkle_path = db
            .commitment_tree_orchard_witness(EMPTY_PATH, b"pool", i, None, grove_version)
            .unwrap()
            .expect("orchard_witness")
            .expect("path should exist");

        assert_eq!(
            merkle_path.root(cmx),
            anchor,
            "batch witness for position {} should reconstruct anchor",
            i
        );
    }

    // Also verify via prepare_spend for one position
    let (spend_anchor, spend_path) = db
        .commitment_tree_prepare_spend(EMPTY_PATH, b"pool", 3, None, grove_version)
        .unwrap()
        .expect("prepare_spend")
        .expect("spend data");
    let cmx3 = ExtractedNoteCommitment::from_bytes(&test_leaf_bytes(3)).expect("valid commitment");
    assert_eq!(spend_anchor, anchor, "prepare_spend anchor should match");
    assert_eq!(
        spend_path.root(cmx3),
        anchor,
        "prepare_spend path should reconstruct anchor"
    );

    // Anchor must be consistent with root_hash
    let root_node = grovedb_commitment_tree::merkle_hash_from_bytes(&sinsemilla_root)
        .expect("root_hash should be valid Pallas element");
    assert_eq!(
        anchor,
        Anchor::from(root_node),
        "anchor should match root_hash"
    );

    // Compare with sequential equivalent
    let db2 = make_empty_grovedb();
    db2.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    for i in 0..5u64 {
        db2.commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(i), None, grove_version)
            .unwrap()
            .expect("sequential append");
    }
    db2.commitment_tree_checkpoint(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("sequential checkpoint");

    let seq_root = db2
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("sequential root");
    assert_eq!(
        sinsemilla_root, seq_root,
        "batch and sequential should produce same root"
    );
}

#[test]
fn test_batch_commitment_tree_failure_rollback() {
    // Verifies that when a batch operation fails, committed state is unchanged.
    // We test this by mixing a valid CT append with an invalid operation.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    // Append one leaf so the tree is non-empty
    db.commitment_tree_append(EMPTY_PATH, b"pool", test_leaf_bytes(0), None, grove_version)
        .unwrap()
        .expect("initial append");

    let root_before = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root before failed batch");

    let grovedb_root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    // Create a batch with a valid CT append AND an invalid operation
    // (appending to a non-existent commitment tree)
    let ops = vec![
        QualifiedGroveDbOp::commitment_tree_append_op(vec![], b"pool".to_vec(), test_leaf_bytes(1)),
        QualifiedGroveDbOp::commitment_tree_append_op(
            vec![b"nonexistent_path".to_vec()],
            b"missing_tree".to_vec(),
            test_leaf_bytes(2),
        ),
    ];

    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(result.is_err(), "batch with invalid op should fail");

    // Verify the GroveDB root hash hasn't changed (batch was atomic)
    let grovedb_root_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(
        grovedb_root_before, grovedb_root_after,
        "GroveDB root should be unchanged after failed batch"
    );

    // Sinsemilla root should also be unchanged
    let root_after = db
        .commitment_tree_root_hash(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("root after failed batch");
    assert_eq!(
        root_before, root_after,
        "Sinsemilla root should be unchanged after failed batch"
    );
}
