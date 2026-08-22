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
        // No declared entry size: the variable format's four-byte prefix is
        // prepaid with every entry.
        assert_eq!(
            cost.storage_cost.added_bytes,
            value.len() as u32 + crate::VARIABLE_ENTRY_FRAMING_BYTES + amortized,
            "append {i}: prepaid share + framing + amortized compaction overhead"
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
        // The model's reads plus the compaction's commit-time puts amortized
        // over the epoch; the compacting append, whose own puts are all
        // prepaid, is charged the slot and record puts it does not issue.
        let churn_seeks = if i % 4 == 3 {
            crate::BUFFER_CHURN_PUTS
        } else {
            0
        };
        assert_eq!(
            cost.seek_count,
            model.record_reads + crate::amortized_compaction_seeks(2) + churn_seeks,
            "append {i}: model reads + amortized compaction seeks (+ churn seeks)"
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
    // Opened after a completed chunk: under the old accounting every slot
    // counted as committed and was read first.
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..4] {
        tree.append_no_state_root(v, &GROVE_V4).expect("append");
    }
    tree.commit_mmr(&GROVE_V4).expect("commit");
    let mut tree = BulkAppendTree::from_state(4, 2, tree.dense_tree.storage).expect("open");
    tree.dense_tree.storage.gets.borrow_mut().clear();
    tree.dense_tree.storage.puts.borrow_mut().clear();
    tree.append_no_state_root(&[1; 8], &GROVE_V4)
        .expect("append");
    // The only read is the persisted MMR root — one of the two root reads
    // the model charges on every state-root derivation; no slot, and at
    // buffer position 0 no record either.
    assert_eq!(
        tree.dense_tree.storage.gets.borrow().clone(),
        vec![crate::MMR_ROOT_KEY.to_vec()]
    );
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
/// append charged its share over the epoch — so their puts carry
/// `KeyValueStorageCost::prepaid()`: nothing added, nothing replaced, no
/// key, and no seek at commit.
#[test]
fn v1_commit_mmr_writes_are_prepaid() {
    let run = run(&GROVE_V4);
    let nodes = mmr_puts(&run.ctx);
    assert_eq!(nodes.len(), 4, "{nodes:?}");
    let prepaid = KeyValueStorageCost::prepaid();
    assert!(prepaid.is_prepaid());
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
    // The share (20) plus the variable format's prefix (4) plus the epoch's
    // amortized compaction overhead (159 bytes over an epoch of 4 → 40), and
    // the 20 bytes of blob rewrite.
    assert_eq!(r.storage_accounting_cost.storage_cost.added_bytes, 64);
    assert_eq!(ctx.cost.storage_cost.added_bytes, 64);
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

/// A tree whose last compaction predates the fixed model has no persisted
/// MMR root. Its first GROVE_V4 append bags the peaks ONCE, backfills the
/// key — prepaid, so the append's figure is the fixed model's — and from
/// then on every reopen reads the key instead of the peaks' blobs.
#[test]
fn legacy_mmr_root_is_backfilled_once_on_the_first_v4_append() {
    let prepaid = KeyValueStorageCost::prepaid();
    assert!(prepaid.is_prepaid());
    // Epoch 1 under the shipped accounting: chunk committed, no root key.
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..4] {
        tree.append_no_state_root(v, &GROVE_V3).expect("append");
    }
    tree.commit_mmr(&GROVE_V3).expect("commit");
    assert!(!tree
        .dense_tree
        .storage
        .data
        .borrow()
        .contains_key(crate::MMR_ROOT_KEY));
    let bagged = tree
        .bag_mmr_root_with_cost(&GROVE_V4)
        .unwrap()
        .expect("bag");

    // First V4 append on the reopened tree: backfill.
    let mut tree = BulkAppendTree::from_state(4, 2, tree.dense_tree.storage).expect("open");
    tree.dense_tree.storage.puts.borrow_mut().clear();
    let first = tree
        .append_no_state_root(VALUES[4], &GROVE_V4)
        .expect("append");
    let root_puts: Vec<_> = tree
        .dense_tree
        .storage
        .puts
        .borrow()
        .iter()
        .filter(|(k, _)| k == crate::MMR_ROOT_KEY)
        .map(|(_, c)| c.clone())
        .collect();
    assert_eq!(root_puts, vec![Some(prepaid)], "one prepaid backfill put");
    assert_eq!(
        tree.dense_tree.storage.data.borrow()[crate::MMR_ROOT_KEY],
        bagged.to_vec()
    );
    // Charged the fixed model, exactly like the same append on a tree that
    // ran under V4 from the start.
    let v4 = run(&GROVE_V4);
    assert_eq!(first.storage_accounting_cost, v4.accounting[4]);
    assert_eq!(first.hash_count, {
        let mut t = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
        for v in &VALUES[..4] {
            t.append_no_state_root(v, &GROVE_V4).expect("append");
        }
        t.append_no_state_root(VALUES[4], &GROVE_V4)
            .expect("append")
            .hash_count
    });
    tree.commit_mmr(&GROVE_V4).expect("commit");

    // Every later reopen reads the key and never the peaks.
    let mut tree = BulkAppendTree::from_state(5, 2, tree.dense_tree.storage).expect("open");
    tree.dense_tree.storage.gets.borrow_mut().clear();
    tree.dense_tree.storage.puts.borrow_mut().clear();
    tree.append_no_state_root(VALUES[5], &GROVE_V4)
        .expect("append");
    let state_root = tree.compute_current_state_root(&GROVE_V4).expect("root");
    let gets = tree.dense_tree.storage.gets.borrow().clone();
    assert!(gets.contains(&crate::MMR_ROOT_KEY.to_vec()), "{gets:?}");
    assert!(
        gets.iter().all(|k| k.len() != 4),
        "no MMR node read after the backfill: {gets:?}"
    );
    assert!(
        !tree
            .dense_tree
            .storage
            .puts
            .borrow()
            .iter()
            .any(|(k, _)| k == crate::MMR_ROOT_KEY),
        "no second backfill"
    );
    // And the state is the one a V4-only tree reaches.
    let mut v4_tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..6] {
        v4_tree.append_no_state_root(v, &GROVE_V4).expect("append");
    }
    assert_eq!(
        state_root,
        v4_tree.compute_current_state_root(&GROVE_V4).expect("root")
    );
    assert_eq!(
        state_root,
        tree.compute_current_state_root_from_values().expect("root")
    );
}

/// A persisted MMR root of any length but 32 is corruption (or the key
/// collision the layout rules out), never a legacy tree: reported, not
/// silently bagged over.
#[test]
fn wrong_length_persisted_mmr_root_is_corrupted_data() {
    let mut tree = BulkAppendTree::new(2, MemStorageContext::new()).expect("new");
    for v in &VALUES[..4] {
        tree.append_no_state_root(v, &GROVE_V4).expect("append");
    }
    tree.commit_mmr(&GROVE_V4).expect("commit");
    tree.dense_tree
        .storage
        .data
        .borrow_mut()
        .insert(crate::MMR_ROOT_KEY.to_vec(), vec![7u8; 5]);
    let mut tree = BulkAppendTree::from_state(4, 2, tree.dense_tree.storage).expect("open");
    assert!(matches!(
        tree.get_mmr_root_with_cost(&GROVE_V4).unwrap(),
        Err(BulkAppendError::CorruptedData(_))
    ));
    assert!(matches!(
        tree.append_no_state_root(VALUES[4], &GROVE_V4),
        Err(BulkAppendError::CorruptedData(_))
    ));
    assert!(matches!(
        tree.append_deferred_roots(VALUES[4], &GROVE_V4).unwrap(),
        Err(BulkAppendError::CorruptedData(_))
    ));
    // The shipped accounting never consults the key.
    assert!(tree.get_mmr_root_with_cost(&GROVE_V3).unwrap().is_ok());
}

/// The per-chunk compaction hash bound holds at every chunk index the
/// 32-bit MMR keys admit, and the amortized charge — the bound spread over
/// the epoch, rounded up — keeps every prefix of the tree's life prepaid at
/// every height, the smallest included. (One hash per append would not: at
/// `chunk_power` 1 the first six chunks perform 13 compaction hashes against
/// 12 prepaid, and the bagging term keeps growing with the peak count.)
#[test]
fn amortized_compaction_hashes_prepay_every_prefix_at_every_height() {
    // The blake3 calls chunk `i` (0-based) costs: the leaf hash, one merge
    // per trailing one bit of `i`, and the root bagging over the peaks of
    // the `i + 1`-leaf MMR.
    fn actual(i: u64) -> u32 {
        1 + i.trailing_ones() + (i + 1).count_ones().saturating_sub(1)
    }
    // The bound at the extremes and around every power of two.
    for k in 0..31u32 {
        let p = 1u64 << k;
        for i in [p - 1, p, p + 1, 2 * p - 2, 2 * p - 1] {
            assert!(
                actual(i) <= crate::MAX_COMPACTION_HASHES_PER_CHUNK,
                "chunk {i}: {}",
                actual(i)
            );
        }
    }
    assert_eq!(actual((1u64 << 31) - 1), 1 + 31 + 0);
    assert_eq!(actual((1u64 << 31) - 2), 1 + 0 + 30);
    // Prefix sums at the smallest heights, exhaustively over many chunks.
    for chunk_power in 1..=4u8 {
        let epoch = 1u64 << chunk_power;
        let per_chunk_charge = epoch * crate::amortized_compaction_hashes(chunk_power) as u64;
        let (mut charged, mut performed) = (0u64, 0u64);
        for i in 0..(1u64 << 16) {
            charged += per_chunk_charge;
            performed += actual(i) as u64;
            assert!(
                charged >= performed,
                "chunk_power {chunk_power}, chunk {i}: charged {charged} < performed {performed}"
            );
        }
    }
    // The figures.
    assert_eq!(crate::amortized_compaction_hashes(1), 33);
    assert_eq!(crate::amortized_compaction_hashes(2), 17);
    assert_eq!(crate::amortized_compaction_hashes(4), 5);
    assert_eq!(crate::amortized_compaction_hashes(6), 2);
    assert_eq!(crate::amortized_compaction_hashes(7), 1);
    assert_eq!(crate::amortized_compaction_hashes(11), 1);
    assert_eq!(crate::amortized_compaction_hashes(16), 1);
    assert_eq!(
        crate::max_amortized_compaction_hashes(),
        crate::amortized_compaction_hashes(1)
    );
}

/// A height-1 tree run for many epochs under the fixed model: every append
/// reports the model plus the amortized bound, and the total reported never
/// falls below the dense work plus the compaction work actually performed.
#[test]
fn height_one_tree_stays_prepaid_over_its_life() {
    let mut tree = BulkAppendTree::new(1, MemStorageContext::new()).expect("new");
    let model = grovedb_dense_fixed_sized_merkle_tree::V1InsertModel::for_height(1);
    let amortized = crate::amortized_compaction_hashes(1);
    let (mut reported, mut performed) = (0u64, 0u64);
    for i in 0..4096u64 {
        let r = tree
            .append_no_state_root(&[i as u8; 4], &GROVE_V4)
            .expect("append");
        assert_eq!(
            r.hash_count,
            model.hash_node_calls + amortized,
            "append {i}"
        );
        reported += r.hash_count as u64;
        // Height 1: one leaf insert (2 hashes) per epoch, then a compaction
        // per 2 appends.
        if i % 2 == 0 {
            performed += 2;
        } else {
            let chunk = i / 2;
            performed += 1 + chunk.trailing_ones() as u64 + (chunk + 1).count_ones() as u64 - 1;
        }
        assert!(
            reported >= performed,
            "append {i}: {reported} < {performed}"
        );
    }
}

/// With a declared fixed entry size every append is charged exactly its own
/// bytes (no per-entry framing) and any other length is rejected before a
/// write; without it the variable format's four-byte prefix is prepaid on
/// every entry, and a mixed-size epoch's added bytes cover the blob and MMR
/// bytes the compaction persists.
#[test]
fn entry_framing_is_charged_unless_a_fixed_entry_size_is_declared() {
    // Declared: exact, and enforced.
    let mut fixed = BulkAppendTree::new(2, MemStorageContext::new())
        .expect("new")
        .with_fixed_entry_size(8);
    let r = fixed
        .append_no_state_root(&[1u8; 8], &GROVE_V4)
        .expect("append");
    assert_eq!(
        r.storage_accounting_cost.storage_cost.added_bytes,
        8 + (159u32).div_ceil(4)
    );
    assert!(matches!(
        fixed.append_no_state_root(&[2u8; 9], &GROVE_V4),
        Err(BulkAppendError::InvalidInput(_))
    ));
    assert!(matches!(
        fixed.append_deferred_roots(&[2u8; 7], &GROVE_V4).unwrap(),
        Err(BulkAppendError::InvalidInput(_))
    ));
    assert_eq!(fixed.total_count, 1, "a rejected append writes nothing");
    assert_eq!(slot_puts(&fixed.dense_tree.storage).len(), 1);

    // Undeclared: a full mixed-size epoch at chunk_power 4. The epoch's
    // added bytes must cover everything the compaction persists — the blob
    // (variable format: 1 + Σ(4 + len)) and the MMR nodes, keys and length
    // varints included.
    let mut tree = BulkAppendTree::new(4, MemStorageContext::new()).expect("new");
    let mut added = 0u64;
    for i in 0..16u32 {
        let value = vec![i as u8; 1 + (i as usize % 5) * 3];
        let r = tree
            .append_no_state_root(&value, &GROVE_V4)
            .expect("append");
        assert_eq!(
            r.storage_accounting_cost.storage_cost.added_bytes,
            value.len() as u32 + crate::VARIABLE_ENTRY_FRAMING_BYTES + (159u32).div_ceil(16)
        );
        added += r.storage_accounting_cost.storage_cost.added_bytes as u64;
    }
    tree.commit_mmr(&GROVE_V4).expect("commit");
    let persisted: u64 = tree
        .dense_tree
        .storage
        .data
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 4 || k.as_slice() == crate::MMR_ROOT_KEY)
        .map(|(k, v)| 32 + k.len() as u64 + paid(v.len() as u32) as u64)
        .sum();
    assert!(persisted > 0);
    assert!(
        added >= persisted,
        "mixed-size epoch: added {added} < persisted {persisted}"
    );
}

/// The per-chunk put bound holds at every chunk index the 32-bit MMR keys
/// admit, and the amortized seek share keeps every prefix of the tree's
/// life prepaid at every height.
#[test]
fn amortized_compaction_seeks_prepay_every_prefix_at_every_height() {
    // The puts chunk `i` (0-based) issues at commit: the blob, one MMR
    // internal node per trailing one bit of `i`, and the persisted root.
    fn actual(i: u64) -> u32 {
        1 + i.trailing_ones() + 1
    }
    for k in 0..31u32 {
        let p = 1u64 << k;
        for i in [p - 1, p, p + 1, 2 * p - 2, 2 * p - 1] {
            assert!(
                actual(i) <= crate::MAX_COMPACTION_PUTS_PER_CHUNK,
                "chunk {i}"
            );
        }
    }
    for chunk_power in 1..=4u8 {
        let epoch = 1u64 << chunk_power;
        let per_chunk_charge = epoch * crate::amortized_compaction_seeks(chunk_power) as u64;
        let (mut charged, mut performed) = (0u64, 0u64);
        for i in 0..(1u64 << 16) {
            charged += per_chunk_charge;
            performed += actual(i) as u64;
            assert!(charged >= performed, "chunk_power {chunk_power}, chunk {i}");
        }
    }
    assert_eq!(crate::amortized_compaction_seeks(1), 17);
    assert_eq!(crate::amortized_compaction_seeks(2), 9);
    assert_eq!(crate::amortized_compaction_seeks(4), 3);
    assert_eq!(crate::amortized_compaction_seeks(5), 2);
    assert_eq!(crate::amortized_compaction_seeks(6), 1);
    assert_eq!(crate::amortized_compaction_seeks(11), 1);
    assert_eq!(crate::amortized_compaction_seeks(16), 1);
    assert_eq!(
        crate::max_amortized_compaction_seeks(),
        crate::amortized_compaction_seeks(1)
    );
}

/// Every put a compaction issues — the blob, the MMR nodes, the persisted
/// root, and a legacy tree's backfilled root — is prepaid, so the commit
/// path charges it no seek; the slot and record puts of a buffered append
/// are not.
#[test]
fn compaction_puts_are_prepaid_and_buffer_puts_are_not() {
    let run = run(&GROVE_V4);
    let puts = run.ctx.puts.borrow();
    let (prepaid, billed): (Vec<_>, Vec<_>) = puts
        .iter()
        .partition(|(_, c)| c.as_ref().is_some_and(|c| c.is_prepaid()));
    assert!(
        prepaid
            .iter()
            .all(|(k, _)| k.len() == 4 || k.as_slice() == crate::MMR_ROOT_KEY),
        "prepaid puts are the MMR nodes and the persisted root: {prepaid:?}"
    );
    assert!(
        billed.iter().all(|(k, _)| k.len() == 2 || k.len() == 3),
        "billed puts are the slots and records: {billed:?}"
    );
    assert!(!prepaid.is_empty() && !billed.is_empty());
}
