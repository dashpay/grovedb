use incrementalmerkletree::{Hashable, Level};
use orchard::{
    tree::{Anchor, MerkleHashOrchard},
    NOTE_COMMITMENT_TREE_DEPTH,
};

use crate::{
    commitment_frontier::{empty_sinsemilla_root, CommitmentFrontier, EMPTY_SINSEMILLA_ROOT},
    test_utils::test_leaf,
};

#[test]
fn test_empty_frontier() {
    let f = CommitmentFrontier::new();
    assert_eq!(f.position(), None);
    assert_eq!(f.tree_size(), 0);

    let empty_anchor = Anchor::empty_tree();
    assert_eq!(f.anchor(), empty_anchor);
}

#[test]
fn test_append_changes_root() {
    let mut f = CommitmentFrontier::new();
    let empty_root = f.root_hash();

    let result = f.append(test_leaf(0));
    let new_root = result.value.expect("append should succeed");
    assert_ne!(empty_root, new_root);
    assert_eq!(f.root_hash(), new_root);
}

#[test]
fn test_append_tracks_position() {
    let mut f = CommitmentFrontier::new();
    assert_eq!(f.position(), None);
    assert_eq!(f.tree_size(), 0);

    f.append(test_leaf(0)).value.expect("append 0");
    assert_eq!(f.position(), Some(0));
    assert_eq!(f.tree_size(), 1);

    f.append(test_leaf(1)).value.expect("append 1");
    assert_eq!(f.position(), Some(1));
    assert_eq!(f.tree_size(), 2);

    for i in 2..100u64 {
        f.append(test_leaf(i)).value.expect("append loop");
    }
    assert_eq!(f.position(), Some(99));
    assert_eq!(f.tree_size(), 100);
}

#[test]
fn test_deterministic_roots() {
    let mut f1 = CommitmentFrontier::new();
    let mut f2 = CommitmentFrontier::new();

    for i in 0..10u64 {
        f1.append(test_leaf(i)).value.expect("append f1");
        f2.append(test_leaf(i)).value.expect("append f2");
    }

    assert_eq!(f1.root_hash(), f2.root_hash());
}

#[test]
fn test_different_leaves_different_roots() {
    let mut f1 = CommitmentFrontier::new();
    let mut f2 = CommitmentFrontier::new();

    f1.append(test_leaf(0)).value.expect("append f1");
    f2.append(test_leaf(1)).value.expect("append f2");

    assert_ne!(f1.root_hash(), f2.root_hash());
}

#[test]
fn test_serialize_empty() {
    let f = CommitmentFrontier::new();
    let data = f.serialize();
    let f2 = CommitmentFrontier::deserialize(&data).expect("deserialize empty frontier");

    assert_eq!(f.root_hash(), f2.root_hash());
    assert_eq!(f.position(), f2.position());
}

#[test]
fn test_serialize_roundtrip() {
    let mut f = CommitmentFrontier::new();
    for i in 0..100u64 {
        f.append(test_leaf(i)).value.expect("append");
    }

    let data = f.serialize();
    let f2 = CommitmentFrontier::deserialize(&data).expect("deserialize frontier");

    assert_eq!(f.root_hash(), f2.root_hash());
    assert_eq!(f.position(), f2.position());
    assert_eq!(f.tree_size(), f2.tree_size());
}

#[test]
#[ignore] // ~60s: runs 1000 Sinsemilla appends; use `cargo test -- --ignored`
fn test_serialize_roundtrip_with_many_leaves() {
    let mut f = CommitmentFrontier::new();
    for i in 0..1000u64 {
        f.append(test_leaf(i)).value.expect("append");
    }

    let data = f.serialize();
    // Frontier should be small regardless of leaf count
    // 1 (flag) + 8 (position) + 32 (leaf) + 1 (ommer_count) + N*32 (ommers)
    // Max ommers for depth 32 = 32, so max ~1.1KB
    assert!(
        data.len() < 1200,
        "frontier serialized to {} bytes",
        data.len()
    );

    let f2 = CommitmentFrontier::deserialize(&data).expect("deserialize frontier with many leaves");
    assert_eq!(f.root_hash(), f2.root_hash());
    assert_eq!(f.tree_size(), f2.tree_size());
}

#[test]
fn test_invalid_field_element() {
    // All 0xFF bytes is not a valid Pallas field element
    let result = CommitmentFrontier::new().append([0xff; 32]);
    assert!(result.value.is_err());
}

#[test]
fn test_deserialize_invalid_data() {
    assert!(CommitmentFrontier::deserialize(&[]).is_err());
    assert!(CommitmentFrontier::deserialize(&[0x02]).is_err());
    assert!(CommitmentFrontier::deserialize(&[0x01]).is_err());
}

#[test]
fn test_root_hash_is_32_bytes() {
    let f = CommitmentFrontier::new();
    assert_eq!(f.root_hash().len(), 32);
}

#[test]
fn test_empty_tree_root_matches_orchard() {
    let f = CommitmentFrontier::new();
    let root = f.root_hash();
    let expected =
        MerkleHashOrchard::empty_root(Level::from(NOTE_COMMITMENT_TREE_DEPTH as u8)).to_bytes();
    assert_eq!(root, expected);
}

#[test]
fn test_empty_sinsemilla_root_constant() {
    // Verify the precomputed constant matches the runtime value
    let computed = empty_sinsemilla_root();
    assert_eq!(
        computed, EMPTY_SINSEMILLA_ROOT,
        "EMPTY_SINSEMILLA_ROOT constant is stale. Update it to: {:?}",
        computed
    );
}

#[test]
fn test_default_impl() {
    let f = CommitmentFrontier::default();
    assert_eq!(f.position(), None);
    assert_eq!(f.tree_size(), 0);
    assert_eq!(f.root_hash(), CommitmentFrontier::new().root_hash());
}

#[test]
fn test_deserialize_truncated_ommers() {
    // Build a valid serialized frontier with 1 leaf so we know the ommer
    // count byte, then truncate the ommer data.
    let mut f = CommitmentFrontier::new();
    // Append enough leaves to generate ommers. After 3 appends (positions
    // 0,1,2), position=2 has trailing_ones=0 so ommer_count may be 1.
    // After 4 appends position=3 has trailing_ones=2, generating ommers.
    for i in 0..4u64 {
        f.append(test_leaf(i)).value.expect("append");
    }
    let data = f.serialize();
    // data layout: 1 (flag) + 8 (position) + 32 (leaf) + 1 (ommer_count) + N*32
    let ommer_count = data[42] as usize;
    assert!(
        ommer_count > 0,
        "need at least one ommer to test truncation"
    );
    // Truncate: keep header + ommer_count byte but chop the ommer data
    let truncated = &data[..43];
    let err = CommitmentFrontier::deserialize(truncated);
    assert!(err.is_err(), "should fail on truncated ommers");
    let msg = format!("{}", err.expect_err("should be an error"));
    assert!(
        msg.contains("truncated ommers"),
        "expected 'truncated ommers' error, got: {msg}"
    );
}

#[test]
fn test_deserialize_invalid_leaf_field_element() {
    // Construct bytes with valid header but an invalid Pallas field element
    // as the leaf (all 0xFF is not a valid point).
    let mut data = vec![0x01]; // has_frontier = true
    data.extend_from_slice(&0u64.to_be_bytes()); // position = 0
    data.extend_from_slice(&[0xFF; 32]); // invalid leaf
    data.push(0); // ommer_count = 0

    let err = CommitmentFrontier::deserialize(&data);
    assert!(err.is_err(), "should fail on invalid leaf field element");
    let msg = format!("{}", err.expect_err("should be an error"));
    assert!(
        msg.contains("invalid Pallas field element"),
        "expected InvalidFieldElement error, got: {msg}"
    );
}

#[test]
fn test_deserialize_invalid_ommer_field_element() {
    // Build a valid frontier, then replace one ommer with 0xFF bytes.
    let mut f = CommitmentFrontier::new();
    for i in 0..4u64 {
        f.append(test_leaf(i)).value.expect("append");
    }
    let mut data = f.serialize();
    let ommer_count = data[42] as usize;
    assert!(ommer_count > 0, "need at least one ommer");
    // First ommer starts at byte 43, replace it with all 0xFF
    for b in &mut data[43..43 + 32] {
        *b = 0xFF;
    }
    let err = CommitmentFrontier::deserialize(&data);
    assert!(err.is_err(), "should fail on invalid ommer field element");
    let msg = format!("{}", err.expect_err("should be an error"));
    assert!(
        msg.contains("invalid Pallas field element"),
        "expected InvalidFieldElement error, got: {msg}"
    );
}

#[test]
fn test_deserialize_from_parts_failure() {
    // Construct technically valid field elements but with an inconsistent
    // position/ommer combination that `Frontier::from_parts` rejects.
    // Position 0 should have 0 ommers; providing 1 ommer triggers the
    // from_parts validation error.
    let valid_leaf = test_leaf(0);
    let valid_ommer = test_leaf(1);

    let mut data = vec![0x01]; // has_frontier
    data.extend_from_slice(&0u64.to_be_bytes()); // position = 0
    data.extend_from_slice(&valid_leaf); // leaf
    data.push(1); // ommer_count = 1 (wrong for position 0)
    data.extend_from_slice(&valid_ommer); // ommer

    let err = CommitmentFrontier::deserialize(&data);
    assert!(err.is_err(), "should fail on inconsistent from_parts");
    let msg = format!("{}", err.expect_err("should be an error"));
    assert!(
        msg.contains("frontier reconstruction"),
        "expected 'frontier reconstruction' error, got: {msg}"
    );
}

#[test]
fn test_append_cost_sinsemilla_hash_calls() {
    let mut f = CommitmentFrontier::new();

    // First append (position 0): 32 hashes + 0 trailing_ones(empty) = 32
    let r0 = f.append(test_leaf(0));
    r0.value.expect("append 0");
    assert_eq!(r0.cost.sinsemilla_hash_calls, 32);

    // Second append (position 0 in frontier before append): trailing_ones(0)
    // = 0 0 in binary is ...0, trailing_ones = 0, so 32 + 0 = 32
    let r1 = f.append(test_leaf(1));
    r1.value.expect("append 1");
    assert_eq!(r1.cost.sinsemilla_hash_calls, 32);

    // Third append (position 1): trailing_ones(1) = 1, so 32 + 1 = 33
    let r2 = f.append(test_leaf(2));
    r2.value.expect("append 2");
    assert_eq!(r2.cost.sinsemilla_hash_calls, 33);

    // Fourth append (position 2): trailing_ones(2=0b10) = 0, so 32
    let r3 = f.append(test_leaf(3));
    r3.value.expect("append 3");
    assert_eq!(r3.cost.sinsemilla_hash_calls, 32);

    // Fifth append (position 3): trailing_ones(3=0b11) = 2, so 34
    let r4 = f.append(test_leaf(4));
    r4.value.expect("append 4");
    assert_eq!(r4.cost.sinsemilla_hash_calls, 34);
}

#[test]
fn test_deserialize_invalid_frontier_flag() {
    // Test with a frontier flag value that is neither 0x00 nor 0x01
    let err = CommitmentFrontier::deserialize(&[0x42]);
    assert!(err.is_err());
    let msg = format!("{}", err.expect_err("should be an error"));
    assert!(
        msg.contains("invalid frontier flag: 0x42"),
        "expected 'invalid frontier flag' error, got: {msg}"
    );
}
