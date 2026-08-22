//! The storage accounting of an append's data writes (issue #822), pinned at
//! the level of the cost information each put carries.
//!
//! The in-memory context records every `put` with its `cost_info`, which is
//! exactly what a real storage context hands the commit path to bill.
//! Note that the in-memory context is immediate — reads see earlier writes
//! of the same session — so the "slot holds a committed value" cases here
//! stand in for a slot written in an earlier, committed session; the
//! batch-dedup behaviour of a real transactional context is pinned by the
//! GroveDB-level tests.

use grovedb_costs::storage_cost::{
    key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::NoStorageRemoval,
};
use grovedb_version::version::{v1::GROVE_V1, v3::GROVE_V3, v4::GROVE_V4, GroveVersion};

use crate::{test_utils::MemStorageContext, BulkAppendError, BulkAppendTree};

fn varint_len(mut n: u32) -> u32 {
    let mut len = 1;
    while n >= 0x80 {
        n >>= 7;
        len += 1;
    }
    len
}

/// The paid size of a stored value: its length plus the varint of it.
fn paid(len: u32) -> u32 {
    len + varint_len(len)
}

/// Dense-buffer slot puts (2-byte position keys), in order.
fn slot_puts(tree: &BulkAppendTree<MemStorageContext>) -> Vec<Option<KeyValueStorageCost>> {
    tree.dense_tree
        .storage
        .puts
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 2)
        .map(|(_, c)| c.clone())
        .collect()
}

/// MMR node puts (4-byte position keys), in order.
fn mmr_puts(tree: &BulkAppendTree<MemStorageContext>) -> Vec<Option<KeyValueStorageCost>> {
    tree.dense_tree
        .storage
        .puts
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 4)
        .map(|(_, c)| c.clone())
        .collect()
}

/// Twelve appends at `chunk_power` 2 (epoch 4): a fixed-size first epoch,
/// a variable-size second epoch, and a compacting third.
const VALUES: [&[u8]; 12] = [
    &[1; 8], &[2; 8], &[3; 8], &[4; 8],  // compaction 1 (fixed-format blob)
    &[5; 8],  // slot 0: same size
    &[6; 16], // slot 1: grows
    &[7; 4],  // slot 2: shrinks
    &[8; 8],  // compaction 2 (variable-format blob), one merge
    &[9; 8],  // slot 0: same size
    &[10; 8], // slot 1: shrinks back
    &[11; 8], // slot 2: grows back
    &[12; 8], // compaction 3
];

fn run(version: &GroveVersion) -> (BulkAppendTree<MemStorageContext>, Vec<u32>, [u8; 32]) {
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    let mut prepaid = Vec::new();
    for v in VALUES {
        let r = tree.append_no_state_root(v, version).expect("append");
        prepaid.push(r.prepaid_chunk_bytes);
    }
    let root = tree.compute_current_state_root().expect("root");
    tree.commit_mmr(version).expect("commit");
    (tree, prepaid, root)
}

/// v0 (GROVE_V1..V3): every data put — slot, blob, MMR node — is issued
/// with no cost information, so the commit path bills each as new storage,
/// and nothing is prepaid at append time.
#[test]
fn v0_issues_every_put_without_cost_info_and_prepays_nothing() {
    for version in [&GROVE_V1, &GROVE_V3] {
        let (tree, prepaid, _) = run(version);
        assert!(prepaid.iter().all(|&p| p == 0), "{prepaid:?}");
        let slots = slot_puts(&tree);
        assert_eq!(slots.len(), 9, "three slots per epoch, three epochs");
        assert!(slots.iter().all(Option::is_none), "{slots:?}");
        let nodes = mmr_puts(&tree);
        // 3 leaves + 1 merge (leaf count 1 -> 2) = 4 nodes; the third leaf
        // (count 2 -> 3) collapses nothing.
        assert_eq!(nodes.len(), 4);
        assert!(nodes.iter().all(Option::is_none), "{nodes:?}");
    }
}

/// v1 (GROVE_V4): a slot written for the first time is new storage; a slot
/// that already holds a value is a replacement — growth added, shrink not
/// credited, key not charged; every append prepays its own bytes.
#[test]
fn v1_slot_rewrites_are_replacements_and_every_append_prepays_its_bytes() {
    let (tree, prepaid, _) = run(&GROVE_V4);
    let expected_prepaid: Vec<u32> = VALUES.iter().map(|v| v.len() as u32).collect();
    assert_eq!(prepaid, expected_prepaid);

    let slots = slot_puts(&tree);
    assert_eq!(slots.len(), 9);
    // Epoch 1: fresh slots.
    assert!(slots[..3].iter().all(Option::is_none), "{slots:?}");

    let rewrite = |previous: u32, new: u32| KeyValueStorageCost {
        key_storage_cost: Default::default(),
        value_storage_cost: grovedb_costs::storage_cost::StorageCost {
            added_bytes: paid(new).saturating_sub(paid(previous)),
            replaced_bytes: paid(new).min(paid(previous)),
            removed_bytes: NoStorageRemoval,
        },
        new_node: false,
        needs_value_verification: true,
    };
    // Epoch 2 over epoch 1's 8-byte values.
    assert_eq!(slots[3], Some(rewrite(8, 8)), "same size: fully replaced");
    assert_eq!(
        slots[4],
        Some(rewrite(8, 16)),
        "growth: 9 replaced, 8 added"
    );
    assert_eq!(
        slots[5],
        Some(rewrite(8, 4)),
        "shrink: 5 replaced, nothing credited"
    );
    assert_eq!(slots[4].as_ref().unwrap().value_storage_cost.added_bytes, 8);
    assert_eq!(slots[5].as_ref().unwrap().value_storage_cost.added_bytes, 0);
    // Epoch 3 over epoch 2's values.
    assert_eq!(slots[6], Some(rewrite(8, 8)));
    assert_eq!(slots[7], Some(rewrite(16, 8)));
    assert_eq!(slots[8], Some(rewrite(4, 8)));
}

/// v1: the compaction blob is reported as a replacement of the entry bytes
/// it supersedes — all prepaid — with only its framing added; internal MMR
/// nodes are new storage.
#[test]
fn v1_commit_mmr_reports_blob_as_replacement_of_prepaid_entry_bytes() {
    let (tree, _, _) = run(&GROVE_V4);
    let nodes = mmr_puts(&tree);
    assert_eq!(nodes.len(), 4, "{nodes:?}");

    let leaf = |entry_bytes: u32, blob_len: u32| {
        // MmrNode leaf envelope: flag (1) + hash (32) + length (4) + blob.
        let node_len = 37 + blob_len;
        KeyValueStorageCost {
            key_storage_cost: Default::default(),
            value_storage_cost: grovedb_costs::storage_cost::StorageCost {
                added_bytes: paid(node_len) - entry_bytes,
                replaced_bytes: entry_bytes,
                removed_bytes: NoStorageRemoval,
            },
            new_node: true,
            needs_value_verification: true,
        }
    };
    // Blob 1: four 8-byte entries, fixed format: 9-byte header + 32.
    assert_eq!(nodes[0], Some(leaf(32, 9 + 32)));
    // Blob 2: 8, 16, 4, 8 -> variable format: 1 + sum(4 + len) = 53, 36 entry bytes.
    assert_eq!(nodes[1], Some(leaf(36, 53)));
    // The merge of leaves 1 and 2 is an internal node: new storage.
    assert_eq!(nodes[2], None);
    // Blob 3: fixed again.
    assert_eq!(nodes[3], Some(leaf(32, 9 + 32)));
}

/// The tree itself must not depend on the accounting version.
#[test]
fn stored_state_and_roots_are_identical_across_accounting_versions() {
    let (t3, _, root3) = run(&GROVE_V3);
    let (t4, _, root4) = run(&GROVE_V4);
    assert_eq!(root3, root4);
    assert_eq!(
        *t3.dense_tree.storage.data.borrow(),
        *t4.dense_tree.storage.data.borrow(),
        "byte-identical storage"
    );
}

/// The cost-propagating append bills the prepaid share in its returned cost
/// and mirrors it in the result; the plain appends leave billing to the
/// caller.
#[test]
fn append_deferred_roots_bills_prepaid_share_in_cost() {
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    let ctx = tree.append_deferred_roots(&[7u8; 20], &GROVE_V4);
    let r = ctx.value.expect("append");
    assert_eq!(r.prepaid_chunk_bytes, 20);
    assert_eq!(ctx.cost.storage_cost.added_bytes, 20);
    assert_eq!(ctx.cost.storage_cost.replaced_bytes, 0);

    let mut legacy = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    let ctx = legacy.append_deferred_roots(&[7u8; 20], &GROVE_V3);
    assert_eq!(ctx.value.expect("append").prepaid_chunk_bytes, 0);
    assert_eq!(ctx.cost.storage_cost.added_bytes, 0);
}

/// An unknown accounting version is rejected at every entry that consults
/// it, never silently treated as one of the implemented ones.
#[test]
fn unknown_storage_accounting_version_is_rejected() {
    let mut bad = GROVE_V4.clone();
    bad.bulk_append_tree_versions.cost.append_storage_accounting = 99;

    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    assert!(matches!(
        tree.append_no_state_root(&[1; 8], &bad),
        Err(BulkAppendError::VersionError(_))
    ));
    assert!(matches!(
        tree.append_deferred_roots(&[1; 8], &bad).value,
        Err(BulkAppendError::VersionError(_))
    ));
    // A flush with staged nodes consults the gate too.
    for v in &VALUES[..4] {
        tree.append_no_state_root(v, &GROVE_V4).expect("append");
    }
    assert!(matches!(
        tree.commit_mmr(&bad),
        Err(BulkAppendError::VersionError(_))
    ));
    // The overlay survives the rejected flush.
    tree.commit_mmr(&GROVE_V4)
        .expect("flush with a known version");
    assert_eq!(mmr_puts(&tree).len(), 1);
}
