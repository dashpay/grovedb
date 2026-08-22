//! The storage accounting of an append's data writes (issue #822), pinned at
//! the level of the cost information each put carries.
//!
//! The in-memory context records every `put` with its `cost_info`, which is
//! exactly what a real storage context hands the commit path to bill. A
//! buffer slot counts as holding a committed value by the `total_count` the
//! tree was opened with, so each epoch here is run on a tree re-opened with
//! `from_state` over the previous epoch's storage — the way every GroveDB
//! operation opens it.

use grovedb_costs::{
    storage_cost::{
        key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::NoStorageRemoval,
        StorageCost,
    },
    OperationCost,
};
use grovedb_version::version::{
    v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4, GroveVersion,
};

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
fn slot_puts(ctx: &MemStorageContext) -> Vec<Option<KeyValueStorageCost>> {
    ctx.puts
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 2)
        .map(|(_, c)| c.clone())
        .collect()
}

/// MMR node puts (4-byte position keys), in order.
fn mmr_puts(ctx: &MemStorageContext) -> Vec<Option<KeyValueStorageCost>> {
    ctx.puts
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

struct Run {
    ctx: MemStorageContext,
    accounting: Vec<OperationCost>,
    root: [u8; 32],
}

/// Run `VALUES` one epoch per session: each epoch re-opens the tree with
/// `from_state` over the storage the previous one left behind.
fn run(version: &GroveVersion) -> Run {
    let mut ctx = MemStorageContext::new();
    let mut accounting = Vec::new();
    let mut root = [0u8; 32];
    for (epoch, values) in VALUES.chunks(4).enumerate() {
        let mut tree = BulkAppendTree::from_state((epoch * 4) as u64, 2, ctx).expect("open");
        for v in values {
            let r = tree.append_no_state_root(v, version).expect("append");
            accounting.push(r.storage_accounting_cost);
        }
        root = tree.compute_current_state_root().expect("root");
        tree.commit_mmr(version).expect("commit");
        ctx = tree.dense_tree.storage;
    }
    Run {
        ctx,
        accounting,
        root,
    }
}

/// v0 (GROVE_V1..V3): every data put — slot, blob, MMR node — is issued
/// with no cost information, so the commit path bills each as new storage,
/// nothing is prepaid at append time and no slot is read.
#[test]
fn v0_issues_every_put_without_cost_info_and_bills_no_accounting() {
    for version in [&GROVE_V1, &GROVE_V2, &GROVE_V3] {
        let run = run(version);
        assert!(
            run.accounting
                .iter()
                .all(|c| *c == OperationCost::default()),
            "{:?}",
            run.accounting
        );
        let slots = slot_puts(&run.ctx);
        assert_eq!(slots.len(), 9, "three slots per epoch, three epochs");
        assert!(slots.iter().all(Option::is_none), "{slots:?}");
        let nodes = mmr_puts(&run.ctx);
        // 3 leaves + 1 merge (leaf count 1 -> 2) = 4 nodes; the third leaf
        // (count 2 -> 3) collapses nothing.
        assert_eq!(nodes.len(), 4);
        assert!(nodes.iter().all(Option::is_none), "{nodes:?}");
    }
}

/// v1 (GROVE_V4): a slot written for the first time is new storage; a slot
/// that already holds a committed value is a replacement — growth added,
/// shrink not credited, key not charged; every append prepays its own bytes.
#[test]
fn v1_slot_rewrites_are_replacements_and_every_append_prepays_its_bytes() {
    let run = run(&GROVE_V4);
    // The in-memory context reports no cost for its reads, so the
    // accounting cost carries the prepaid share only; the read's seek and
    // bytes are pinned against RocksDB by the GroveDB-level tests.
    let prepaid: Vec<u32> = run
        .accounting
        .iter()
        .map(|c| c.storage_cost.added_bytes)
        .collect();
    let expected_prepaid: Vec<u32> = VALUES.iter().map(|v| v.len() as u32).collect();
    assert_eq!(prepaid, expected_prepaid);
    assert!(run
        .accounting
        .iter()
        .all(|c| c.storage_cost.replaced_bytes == 0 && c.hash_node_calls == 0));

    let slots = slot_puts(&run.ctx);
    assert_eq!(slots.len(), 9);
    // Epoch 1: fresh slots.
    assert!(slots[..3].iter().all(Option::is_none), "{slots:?}");

    let rewrite = |previous: u32, new: u32| KeyValueStorageCost {
        key_storage_cost: Default::default(),
        value_storage_cost: StorageCost {
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

/// Whether a slot is rewritten is judged against the state at open: inside
/// one session a slot written for the first time and rewritten after an
/// in-session compaction is still new storage (nothing is committed), so
/// it is never read and never reported as a replacement — a `StorageBatch`
/// will charge its last put, which must describe the transition from
/// committed storage. A slot the open count says is committed but storage
/// does not hold is charged as new, the safe direction.
#[test]
fn v1_judges_committed_slots_by_the_count_at_open() {
    // Two epochs on a tree created in this session: no slot is committed.
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..8] {
        tree.append_no_state_root(v, &GROVE_V4).expect("append");
    }
    let slots = slot_puts(&tree.dense_tree.storage);
    assert_eq!(slots.len(), 6);
    assert!(slots.iter().all(Option::is_none), "{slots:?}");

    // Opened with two committed buffer entries and no chunk: slots 0 and 1
    // are committed, slot 2 is not and is written without a read.
    let mut seeded = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..2] {
        seeded.append_no_state_root(v, &GROVE_V4).expect("seed");
    }
    let mut tree = BulkAppendTree::from_state(2, 2, seeded.dense_tree.storage).expect("open");
    assert!(tree.slot_is_committed(0) && tree.slot_is_committed(1));
    assert!(!tree.slot_is_committed(2));
    tree.append_no_state_root(&[1; 8], &GROVE_V4)
        .expect("slot 2");
    let slots = slot_puts(&tree.dense_tree.storage);
    assert_eq!(slots, vec![None, None, None]);

    // Opened after a completed chunk: every slot is committed — and one
    // that storage does not hold (corruption, not a state this code
    // produces) is charged as new rather than as a rewrite of nothing.
    let mut tree = BulkAppendTree::from_state(4, 2, MemStorageContext::new()).expect("open");
    assert!((0..3).all(|p| tree.slot_is_committed(p)));
    tree.append_no_state_root(&[1; 8], &GROVE_V4)
        .expect("slot 0");
    assert_eq!(slot_puts(&tree.dense_tree.storage), vec![None]);
}

/// v1: the compaction blob is reported as a replacement of the entry bytes
/// it supersedes — all prepaid — with only its framing added; internal MMR
/// nodes are new storage.
#[test]
fn v1_commit_mmr_reports_blob_as_replacement_of_prepaid_entry_bytes() {
    let run = run(&GROVE_V4);
    let nodes = mmr_puts(&run.ctx);
    assert_eq!(nodes.len(), 4, "{nodes:?}");

    let leaf = |entry_bytes: u32, blob_len: u32| {
        // MmrNode leaf envelope: flag (1) + hash (32) + length (4) + blob.
        let node_len = 37 + blob_len;
        KeyValueStorageCost {
            key_storage_cost: Default::default(),
            value_storage_cost: StorageCost {
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
    let r3 = run(&GROVE_V3);
    let r4 = run(&GROVE_V4);
    assert_eq!(r3.root, r4.root);
    assert_eq!(
        *r3.ctx.data.borrow(),
        *r4.ctx.data.borrow(),
        "byte-identical storage"
    );
}

/// The cost-propagating append bills the accounting cost in its returned
/// cost and mirrors it in the result; the plain appends leave billing to
/// the caller.
#[test]
fn append_deferred_roots_bills_accounting_cost() {
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    let ctx = tree.append_deferred_roots(&[7u8; 20], &GROVE_V4);
    let r = ctx.value.expect("append");
    assert_eq!(r.storage_accounting_cost.storage_cost.added_bytes, 20);
    assert_eq!(ctx.cost.storage_cost.added_bytes, 20);
    assert_eq!(ctx.cost.storage_cost.replaced_bytes, 0);

    let mut legacy = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    let ctx = legacy.append_deferred_roots(&[7u8; 20], &GROVE_V3);
    assert_eq!(
        ctx.value.expect("append").storage_accounting_cost,
        OperationCost::default()
    );
    assert_eq!(ctx.cost.storage_cost.added_bytes, 0);
}

/// A storage fault on the committed-slot read surfaces as an error and
/// nothing is written; the shipped accounting never performs that read.
#[test]
fn committed_slot_read_failure_surfaces() {
    let ctx = MemStorageContext::new();
    ctx.fail_reads();
    let mut tree = BulkAppendTree::from_state(4, 2, ctx).expect("open");
    let err = tree
        .append_no_state_root(&[1; 8], &GROVE_V4)
        .expect_err("read failed");
    assert!(
        matches!(&err, BulkAppendError::StorageError(m) if m.contains("committed slot")),
        "{err:?}"
    );
    let err = tree
        .append_deferred_roots(&[1; 8], &GROVE_V4)
        .value
        .expect_err("read failed");
    assert!(matches!(err, BulkAppendError::StorageError(_)));
    assert!(tree.dense_tree.storage.puts.borrow().is_empty());

    // v0 reads nothing, so it writes straight through the broken reader.
    tree.append_no_state_root(&[1; 8], &GROVE_V3)
        .expect("no read under the shipped accounting");
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
    assert_eq!(mmr_puts(&tree.dense_tree.storage).len(), 1);
}
