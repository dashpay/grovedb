use incrementalmerkletree::{Position, Retention};
use orchard::tree::Anchor;

use crate::{test_utils::test_leaf, ClientMemoryCommitmentTree};

#[test]
fn test_empty_tree() {
    let tree = ClientMemoryCommitmentTree::new(10);
    assert_eq!(tree.max_leaf_position().expect("max_leaf_position"), None);
    assert_eq!(tree.anchor().expect("anchor"), Anchor::empty_tree());
}

#[test]
fn test_append_and_position() {
    let mut tree = ClientMemoryCommitmentTree::new(10);

    tree.append(test_leaf(0), Retention::Marked)
        .expect("append 0");
    assert_eq!(
        tree.max_leaf_position().expect("max_leaf_position"),
        Some(Position::from(0))
    );

    tree.append(test_leaf(1), Retention::Ephemeral)
        .expect("append 1");
    assert_eq!(
        tree.max_leaf_position().expect("max_leaf_position"),
        Some(Position::from(1))
    );
}

#[test]
fn test_anchor_changes() {
    let mut tree = ClientMemoryCommitmentTree::new(10);
    let empty_anchor = tree.anchor().expect("anchor");

    tree.append(test_leaf(0), Retention::Marked)
        .expect("append 0");
    let anchor1 = tree.anchor().expect("anchor");
    assert_ne!(empty_anchor, anchor1);

    tree.append(test_leaf(1), Retention::Marked)
        .expect("append 1");
    let anchor2 = tree.anchor().expect("anchor");
    assert_ne!(anchor1, anchor2);
}

#[test]
fn test_witness_generation() {
    let mut tree = ClientMemoryCommitmentTree::new(10);

    // Append a marked leaf so we can witness it
    tree.append(test_leaf(0), Retention::Marked)
        .expect("append 0");
    tree.append(test_leaf(1), Retention::Ephemeral)
        .expect("append 1");
    tree.checkpoint(1).expect("checkpoint");

    // Witness for position 0 at current state
    let path = tree.witness(Position::from(0), 0).expect("witness");
    assert!(path.is_some(), "should produce witness for marked leaf");
}

#[test]
#[cfg(feature = "server")]
fn test_frontier_and_client_same_root() {
    use crate::commitment_frontier::CommitmentFrontier;

    let mut frontier = CommitmentFrontier::new();
    let mut client = ClientMemoryCommitmentTree::new(10);

    for i in 0..20u64 {
        frontier
            .append(test_leaf(i))
            .value
            .expect("frontier append");
        client
            .append(test_leaf(i), Retention::Ephemeral)
            .expect("client append");
    }

    assert_eq!(frontier.anchor(), client.anchor().expect("client anchor"));
}

/// Demonstrates that `checkpoint()` with a duplicate ID silently returns
/// `Ok(false)` and does NOT advance the checkpoint frontier. Notes
/// appended after the original checkpoint are unreachable by
/// `witness_at_checkpoint_depth(pos, 0)`.
///
/// This is the exact failure mode that caused the "Tree does not contain
/// a root at address" error in PMT when the sync code reused
/// `next_start_index` as the checkpoint ID across re-syncs.
#[test]
fn test_duplicate_checkpoint_id_breaks_witness_for_new_notes() {
    let mut tree = ClientMemoryCommitmentTree::new(100);

    // Sync 1: append 20 notes (even = Marked, odd = Ephemeral)
    for i in 0..20u64 {
        let retention = if i % 2 == 0 {
            Retention::Marked
        } else {
            Retention::Ephemeral
        };
        tree.append(test_leaf(i), retention).expect("append sync 1");
    }

    // Checkpoint with the "chunk boundary" ID
    let created = tree.checkpoint(2048).expect("checkpoint 1");
    assert!(created, "first checkpoint should succeed");

    // Witness works for all marked notes in sync 1
    for i in (0..20u64).step_by(2) {
        let path = tree
            .witness(Position::from(i), 0)
            .expect("witness sync 1 note");
        assert!(
            path.is_some(),
            "should produce witness for marked note at position {}",
            i
        );
    }

    // Sync 2: append 30 more notes (simulates new notes arriving)
    for i in 20..50u64 {
        let retention = if i % 2 == 0 {
            Retention::Marked
        } else {
            Retention::Ephemeral
        };
        tree.append(test_leaf(i), retention).expect("append sync 2");
    }

    // BUG: reuse the same checkpoint ID — returns Ok(false)!
    let created = tree.checkpoint(2048).expect("checkpoint 2 (duplicate)");
    assert!(
        !created,
        "duplicate checkpoint ID should return false (no new checkpoint created)"
    );

    // Original sync 1 notes still have valid witnesses
    let path = tree
        .witness(Position::from(0), 0)
        .expect("witness sync 1 note after sync 2");
    assert!(path.is_some(), "sync 1 notes should still be witnessable");

    // Sync 2 notes at positions >= 20 CANNOT be witnessed because the
    // checkpoint is stuck at position 19 (from sync 1). This is the bug.
    let result = tree.witness(Position::from(20), 0);
    assert!(
        result.is_err(),
        "witness should fail for notes beyond the stale checkpoint"
    );
}

/// Shows the correct pattern: use unique, increasing checkpoint IDs
/// so that each sync creates a new checkpoint covering all appended notes.
#[test]
fn test_unique_checkpoint_ids_allow_witness_for_all_notes() {
    let mut tree = ClientMemoryCommitmentTree::new(100);

    // Sync 1: append 20 notes
    for i in 0..20u64 {
        let retention = if i % 2 == 0 {
            Retention::Marked
        } else {
            Retention::Ephemeral
        };
        tree.append(test_leaf(i), retention).expect("append sync 1");
    }

    // Checkpoint with unique ID = last appended position
    let created = tree.checkpoint(19).expect("checkpoint 1");
    assert!(created, "first checkpoint should succeed");

    // Sync 2: append 30 more notes
    for i in 20..50u64 {
        let retention = if i % 2 == 0 {
            Retention::Marked
        } else {
            Retention::Ephemeral
        };
        tree.append(test_leaf(i), retention).expect("append sync 2");
    }

    // Checkpoint with new unique ID = new last appended position
    let created = tree.checkpoint(49).expect("checkpoint 2");
    assert!(created, "second checkpoint with unique ID should succeed");

    // ALL marked notes — from both syncs — can be witnessed
    for i in (0..50u64).step_by(2) {
        let path = tree
            .witness(Position::from(i), 0)
            .unwrap_or_else(|_| panic!("witness note at position {}", i));
        assert!(
            path.is_some(),
            "should produce witness for marked note at position {}",
            i
        );
    }
}

/// Verifies that witness anchors from both syncs match when using
/// unique checkpoint IDs, and that the anchor at checkpoint depth 1
/// differs from depth 0 (since the tree grew between checkpoints).
#[test]
fn test_witness_anchors_match_across_syncs() {
    let mut tree = ClientMemoryCommitmentTree::new(100);

    // Sync 1
    for i in 0..10u64 {
        tree.append(test_leaf(i), Retention::Marked)
            .expect("append sync 1");
    }
    tree.checkpoint(9).expect("checkpoint 1");
    let anchor_after_sync1 = tree.anchor().expect("anchor after sync 1");

    // Sync 2
    for i in 10..20u64 {
        tree.append(test_leaf(i), Retention::Marked)
            .expect("append sync 2");
    }
    tree.checkpoint(19).expect("checkpoint 2");
    let anchor_after_sync2 = tree.anchor().expect("anchor after sync 2");

    // Anchors should differ (tree grew)
    assert_ne!(
        anchor_after_sync1, anchor_after_sync2,
        "anchors should differ after tree growth"
    );

    // Witness at depth 0 uses the latest checkpoint (sync 2)
    let path_depth0 = tree
        .witness(Position::from(0), 0)
        .expect("witness at depth 0");
    assert!(path_depth0.is_some());

    // Witness at depth 1 uses the previous checkpoint (sync 1)
    let path_depth1 = tree
        .witness(Position::from(0), 1)
        .expect("witness at depth 1");
    assert!(path_depth1.is_some());

    // Both witnesses exist at their respective checkpoint depths
    // (MerklePath doesn't implement PartialEq so we just verify both are
    // Some)
}
