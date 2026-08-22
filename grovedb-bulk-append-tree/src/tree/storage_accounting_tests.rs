//! The storage accounting of an append's data writes (issue #822), pinned at
//! the level of the cost information each put carries.
//!
//! The in-memory context records every `put` with its `cost_info`, which is
//! exactly what a real storage context hands the commit path to bill. Each
//! epoch here is run on a tree re-opened with `from_state` over the previous
//! epoch's storage — the way every GroveDB operation opens it.

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

/// Path record puts (3-byte keys), in order.
fn record_puts(ctx: &MemStorageContext) -> Vec<Option<KeyValueStorageCost>> {
    ctx.puts
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 3)
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
        root = tree.compute_current_state_root(version).expect("root");
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

/// v1 (GROVE_V4): the buffer is churn and the charge is fixed. Every slot
/// write — epoch 1 included, growth and shrink alike — and every path record
/// write is reported as an in-place replacement of its own size, nothing
/// added and no key charged; nothing is read to size it. Every append —
/// buffered or compacting — prepays its own bytes plus the epoch's share of
/// the compaction overhead as added storage, its own bytes again as its part
/// of the blob rewrite, and the buffer's fixed root-maintenance model.
#[test]
fn v1_buffer_writes_are_churn_and_every_append_is_charged_the_fixed_model() {
    use grovedb_dense_fixed_sized_merkle_tree::{path_record_len, V1InsertModel};

    let run = run(&GROVE_V4);
    let model = V1InsertModel::for_height(2);
    // 159 bytes of compaction overhead over an epoch of 4.
    let amortized: u32 = (159u32).div_ceil(4);
    for (i, (cost, value)) in run.accounting.iter().zip(VALUES.iter()).enumerate() {
        assert_eq!(
            cost.storage_cost.added_bytes,
            value.len() as u32 + amortized,
            "append {i}: prepaid share + amortized compaction overhead"
        );
        // Its part of the blob rewrite — and, for a compacting append,
        // which writes no slot and no record, their churn billed here
        // instead of through the puts.
        let churn = if i % 4 == 3 {
            paid(value.len() as u32) + paid(path_record_len(2) as u32)
        } else {
            0
        };
        assert_eq!(
            cost.storage_cost.replaced_bytes,
            value.len() as u32 + churn,
            "append {i}: its part of the blob rewrite (+ the compacting append's churn)"
        );
        assert_eq!(
            cost.hash_node_calls, 0,
            "append {i}: hashes go through hash_count"
        );
        // The model, compacting appends included.
        assert_eq!(
            cost.seek_count, model.record_reads,
            "append {i}: model reads"
        );
        assert_eq!(
            cost.storage_loaded_bytes,
            model.record_reads as u64 * model.record_len as u64,
            "append {i}: model bytes"
        );
    }

    let churn = |len: u32| KeyValueStorageCost {
        key_storage_cost: Default::default(),
        value_storage_cost: StorageCost {
            added_bytes: 0,
            replaced_bytes: paid(len),
            removed_bytes: NoStorageRemoval,
        },
        new_node: false,
        needs_value_verification: true,
    };
    let slots = slot_puts(&run.ctx);
    assert_eq!(slots.len(), 9, "three slots per epoch, three epochs");
    let mut expected = Vec::new();
    for (i, v) in VALUES.iter().enumerate() {
        if i % 4 != 3 {
            expected.push(Some(churn(v.len() as u32)));
        }
    }
    assert_eq!(slots, expected, "every slot write is churn of its own size");

    let records = record_puts(&run.ctx);
    assert_eq!(records.len(), 9, "one path record per buffered append");
    let record_len = path_record_len(2) as u32;
    assert!(
        records.iter().all(|c| *c == Some(churn(record_len))),
        "every record write is churn of the fixed record size: {records:?}"
    );
}

/// The churn accounting never reads a slot to size a rewrite — a broken
/// reader for slot keys does not stop an append — and the same slot written
/// twice in one session is churn both times.
#[test]
fn v1_never_reads_to_size_a_buffer_write() {
    let ctx = MemStorageContext::new();
    ctx.fail_reads();
    // Opened after a completed chunk: under the old accounting every slot
    // counted as committed and was read first.
    let mut tree = BulkAppendTree::from_state(4, 2, ctx).expect("open");
    tree.append_no_state_root(&[1; 8], &GROVE_V4)
        .expect("no read under the churn accounting");
    // (The dense tree's own reads go to records the broken reader also
    // fails; at buffer position 0 there are none.)
    assert_eq!(slot_puts(&tree.dense_tree.storage).len(), 1);
    assert!(slot_puts(&tree.dense_tree.storage)[0]
        .as_ref()
        .is_some_and(|c| !c.new_node && c.value_storage_cost.added_bytes == 0));

    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..8] {
        tree.append_no_state_root(v, &GROVE_V4).expect("append");
    }
    let slots = slot_puts(&tree.dense_tree.storage);
    assert_eq!(slots.len(), 6);
    assert!(
        slots.iter().all(|c| c
            .as_ref()
            .is_some_and(|c| c.value_storage_cost.added_bytes == 0)),
        "{slots:?}"
    );
}

/// v1: the compaction blob and the MMR internal nodes are prepaid — every
/// append charged its share over the epoch — so their puts carry zero-byte
/// cost information: nothing added, nothing replaced, no key.
#[test]
fn v1_commit_mmr_writes_are_prepaid() {
    let run = run(&GROVE_V4);
    let nodes = mmr_puts(&run.ctx);
    assert_eq!(nodes.len(), 4, "{nodes:?}");
    let prepaid = KeyValueStorageCost {
        key_storage_cost: Default::default(),
        value_storage_cost: StorageCost::default(),
        new_node: false,
        needs_value_verification: false,
    };
    assert!(
        nodes.iter().all(|n| *n == Some(prepaid.clone())),
        "every MMR put — three chunk leaves and one merge — is prepaid: {nodes:?}"
    );
}

/// The tree itself must not depend on the accounting version: the values,
/// the chunk blobs, the MMR nodes and the roots are byte-identical. The only
/// keys GROVE_V4 adds are the dense buffer's hash records (3-byte keys,
/// root-maintenance version 1), which GROVE_V3 never writes or reads.
#[test]
fn stored_state_and_roots_are_identical_across_accounting_versions() {
    let r3 = run(&GROVE_V3);
    let r4 = run(&GROVE_V4);
    assert_eq!(r3.root, r4.root);
    // GROVE_V4 adds the path records (3-byte keys) and the persisted MMR
    // root (the 1-byte key `r`); everything else is byte-identical.
    let without_derived = |ctx: &MemStorageContext| -> std::collections::HashMap<Vec<u8>, Vec<u8>> {
        ctx.data
            .borrow()
            .iter()
            .filter(|(k, _)| k.len() != 3 && k.len() != 1)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    assert!(
        r3.ctx
            .data
            .borrow()
            .keys()
            .all(|k| k.len() != 3 && k.len() != 1),
        "GROVE_V3 writes no records and no persisted MMR root"
    );
    assert!(
        r4.ctx.data.borrow().keys().any(|k| k.len() == 3),
        "GROVE_V4 writes path records"
    );
    let persisted_root = r4
        .ctx
        .data
        .borrow()
        .get(crate::MMR_ROOT_KEY)
        .cloned()
        .expect("GROVE_V4 persists the MMR root at commit");
    let reopened = BulkAppendTree::from_state(12, 2, r4.ctx).expect("reopen");
    assert_eq!(
        persisted_root.as_slice(),
        reopened
            .bag_mmr_root_with_cost(&GROVE_V4)
            .unwrap()
            .expect("bag")
            .as_slice(),
        "the persisted MMR root is the bagged root"
    );
    let r4_ctx = reopened.dense_tree.storage;
    assert_eq!(
        without_derived(&r3.ctx),
        without_derived(&r4_ctx),
        "byte-identical storage apart from the records and the persisted MMR root"
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
    // The share (20) plus the epoch's amortized compaction overhead
    // (159 bytes over an epoch of 4 → 40), and the 20 bytes of blob rewrite.
    assert_eq!(r.storage_accounting_cost.storage_cost.added_bytes, 60);
    assert_eq!(ctx.cost.storage_cost.added_bytes, 60);
    assert_eq!(ctx.cost.storage_cost.replaced_bytes, 20);
    assert_eq!(r.storage_accounting_cost.storage_cost.replaced_bytes, 20);

    let mut legacy = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    let ctx = legacy.append_deferred_roots(&[7u8; 20], &GROVE_V3);
    assert_eq!(
        ctx.value.expect("append").storage_accounting_cost,
        OperationCost::default()
    );
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
    assert_eq!(mmr_puts(&tree.dense_tree.storage).len(), 1);
}
