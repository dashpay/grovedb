//! Tests for the versioned root maintenance: version 1 (per-position hash
//! records, GROVE_V4+) must produce exactly the roots version 0 (recompute
//! from every filled position, GROVE_V1..V3) produces, under every fill
//! level, across sessions, after a buffer filled under version 0, and over
//! records left by an earlier epoch — while doing O(height) work per insert.

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
use grovedb_version::version::GroveVersion;

use crate::{
    position_key, record_key, test_utils::MemStorageContext, DenseFixedSizedMerkleTree,
    DenseMerkleError, HashRecord, SlotWriteAccounting, HASH_RECORD_LEN,
};

/// GROVE_V1: root-maintenance version 0.
fn v0() -> &'static GroveVersion {
    let v = GroveVersion::first();
    assert_eq!(v.dense_tree_versions.root_maintenance, 0);
    v
}

/// GROVE_V4 (latest): root-maintenance version 1.
fn v1() -> &'static GroveVersion {
    let v = GroveVersion::latest();
    assert_eq!(v.dense_tree_versions.root_maintenance, 1);
    v
}

/// Deterministic value for a position: varies in length too.
fn value(pos: u16, salt: u8) -> Vec<u8> {
    let mut v = pos.to_be_bytes().to_vec();
    v.extend(std::iter::repeat_n(salt, (pos % 5) as usize + 1));
    v
}

/// Depth of a BFS position (root = 0).
fn depth(pos: u16) -> u32 {
    (pos as u32 + 1).ilog2()
}

/// Record puts (3-byte keys) made on `ctx`, in order.
fn record_puts(ctx: &MemStorageContext) -> Vec<(u16, Option<KeyValueStorageCost>)> {
    ctx.puts
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 3)
        .map(|(k, c)| (u16::from_be_bytes([k[1], k[2]]), c.clone()))
        .collect()
}

/// Storage gets of hash records (3-byte keys) made on `ctx`, in order.
fn record_gets(ctx: &MemStorageContext) -> Vec<u16> {
    ctx.gets
        .borrow()
        .iter()
        .filter(|k| k.len() == 3)
        .map(|k| u16::from_be_bytes([k[1], k[2]]))
        .collect()
}

/// Storage gets of values (2-byte keys) made on `ctx`, in order.
fn value_gets(ctx: &MemStorageContext) -> Vec<u16> {
    ctx.gets
        .borrow()
        .iter()
        .filter(|k| k.len() == 2)
        .map(|k| u16::from_be_bytes([k[0], k[1]]))
        .collect()
}

fn clear_logs(ctx: &MemStorageContext) {
    ctx.puts.borrow_mut().clear();
    ctx.gets.borrow_mut().clear();
}

/// The stored record for `pos`, if any.
fn stored_record(ctx: &MemStorageContext, pos: u16) -> Option<HashRecord> {
    ctx.data
        .borrow()
        .get(record_key(pos).as_slice())
        .and_then(|b| HashRecord::from_bytes(b))
}

// ── Equivalence ────────────────────────────────────────────────────────

/// For every height up to 5 and every fill level, the root version 1 returns
/// from `insert`, from `root_hash` in the same session, and from `root_hash`
/// in a fresh session (cache cold, records read from storage) equals the
/// root version 0 derives from the same values — and the version-0 read over
/// the version-1 storage (the value walk, which ignores records) agrees too.
#[test]
fn v1_roots_equal_v0_roots_at_every_fill_level_and_across_sessions() {
    for height in 1..=5u8 {
        let capacity = (1u16 << height) - 1;
        let mut legacy = DenseFixedSizedMerkleTree::new(height, MemStorageContext::new()).unwrap();
        let mut ctx = MemStorageContext::new();
        for pos in 0..capacity {
            let v = value(pos, 7);
            let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();

            // A fresh session per insert: the ancestor records must be found
            // in storage, not in a cache.
            let mut tree = DenseFixedSizedMerkleTree::from_state(height, pos, ctx).unwrap();
            let (root, at) = tree.insert(&v, v1()).unwrap().unwrap();
            assert_eq!(at, pos);
            assert_eq!(root, legacy_root, "height {height} pos {pos}: insert root");
            assert_eq!(
                tree.root_hash(v1()).unwrap().unwrap(),
                legacy_root,
                "height {height} pos {pos}: same-session root read"
            );
            assert_eq!(
                tree.root_hash(v0()).unwrap().unwrap(),
                legacy_root,
                "height {height} pos {pos}: value walk over v1 storage"
            );
            ctx = tree.storage;

            let reopened = DenseFixedSizedMerkleTree::from_state(height, pos + 1, ctx).unwrap();
            let ctx_read = reopened.root_hash(v1());
            assert_eq!(
                ctx_read.value.unwrap(),
                legacy_root,
                "height {height} pos {pos}: cold root read"
            );
            assert_eq!(
                ctx_read.cost.hash_node_calls, 0,
                "a cold root read under v1 reads the record, it does not hash"
            );
            ctx = reopened.storage;
        }
    }
}

/// Same as above within ONE session (the batch shape): every insert's
/// returned root equals the version-0 root.
#[test]
fn v1_roots_equal_v0_roots_within_one_session() {
    for height in 1..=6u8 {
        let capacity = (1u16 << height) - 1;
        let mut legacy = DenseFixedSizedMerkleTree::new(height, MemStorageContext::new()).unwrap();
        let mut tree = DenseFixedSizedMerkleTree::new(height, MemStorageContext::new()).unwrap();
        for pos in 0..capacity {
            let v = value(pos, 3);
            let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
            let (root, _) = tree.insert(&v, v1()).unwrap().unwrap();
            assert_eq!(root, legacy_root, "height {height} pos {pos}");
        }
        assert_eq!(
            tree.root_hash(v1()).unwrap().unwrap(),
            legacy.root_hash(v0()).unwrap().unwrap()
        );
    }
}

/// `try_insert_no_root` under version 1 maintains the records exactly as
/// `insert` does — a later root read finds the record and does not walk.
#[test]
fn try_insert_no_root_maintains_records_under_v1() {
    let mut tree = DenseFixedSizedMerkleTree::new(4, MemStorageContext::new()).unwrap();
    let mut legacy = DenseFixedSizedMerkleTree::new(4, MemStorageContext::new()).unwrap();
    for pos in 0..11u16 {
        let v = value(pos, 1);
        assert_eq!(
            tree.try_insert_no_root(&v, v1()).unwrap().unwrap(),
            Some(pos)
        );
        legacy.insert(&v, v0()).unwrap().unwrap();
    }
    let reopened = DenseFixedSizedMerkleTree::from_state(4, 11, tree.storage).unwrap();
    let ctx = reopened.root_hash(v1());
    assert_eq!(ctx.cost.hash_node_calls, 0);
    assert_eq!(ctx.value.unwrap(), legacy.root_hash(v0()).unwrap().unwrap());
}

// ── Legacy buffers ─────────────────────────────────────────────────────

/// A buffer filled under version 0 has no records. The first version-1
/// insert derives what it needs from the values (costing no more than the
/// version-0 walk), records it, and every later insert is O(height) again;
/// the roots match version 0 throughout.
#[test]
fn buffer_filled_under_v0_is_caught_up_by_the_first_v1_insert() {
    const HEIGHT: u8 = 5;
    const LEGACY_FILL: u16 = 19;
    let mut legacy = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    let mut under_test = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    for pos in 0..LEGACY_FILL {
        legacy.insert(&value(pos, 9), v0()).unwrap().unwrap();
        under_test.insert(&value(pos, 9), v0()).unwrap().unwrap();
    }
    let ctx = under_test.storage;
    assert!(
        (0..LEGACY_FILL).all(|p| stored_record(&ctx, p).is_none()),
        "version 0 writes no records"
    );
    // A version-1 root read of the legacy buffer falls back to the walk.
    let reopened = DenseFixedSizedMerkleTree::from_state(HEIGHT, LEGACY_FILL, ctx).unwrap();
    let read = reopened.root_hash(v1());
    assert_eq!(read.cost.hash_node_calls, 2 * LEGACY_FILL as u32);
    assert_eq!(
        read.value.unwrap(),
        legacy.root_hash(v0()).unwrap().unwrap()
    );
    let ctx = reopened.storage;

    // First version-1 insert: catch-up.
    let mut tree = DenseFixedSizedMerkleTree::from_state(HEIGHT, LEGACY_FILL, ctx).unwrap();
    clear_logs(&tree.storage);
    let v = value(LEGACY_FILL, 9);
    let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
    let first = tree.insert(&v, v1());
    let (root, _) = first.value.unwrap();
    assert_eq!(root, legacy_root);
    // No more hashing than the version-0 walk over the now-filled buffer.
    assert!(
        first.cost.hash_node_calls <= 2 * (LEGACY_FILL as u32 + 1),
        "catch-up must not exceed the version-0 walk: {:?}",
        first.cost
    );
    // Every position on the path and every off-path sibling subtree root now
    // has a current record; later inserts need nothing else.
    let touched: Vec<u16> = record_puts(&tree.storage)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert!(touched.contains(&0) && touched.contains(&LEGACY_FILL));

    // Second version-1 insert: O(height).
    clear_logs(&tree.storage);
    let pos = LEGACY_FILL + 1;
    let v = value(pos, 9);
    let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
    let second = tree.insert(&v, v1());
    let (root, _) = second.value.unwrap();
    assert_eq!(root, legacy_root);
    assert_eq!(
        second.cost.hash_node_calls,
        2 + depth(pos),
        "after catch-up an insert hashes the leaf twice and once per ancestor"
    );
    assert!(
        value_gets(&tree.storage).is_empty(),
        "after catch-up no value is read back: {:?}",
        value_gets(&tree.storage)
    );

    // And the rest of the buffer, in fresh sessions, keeps agreeing.
    let mut ctx = tree.storage;
    for pos in (LEGACY_FILL + 2)..((1u16 << HEIGHT) - 1) {
        let v = value(pos, 9);
        let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
        let mut t = DenseFixedSizedMerkleTree::from_state(HEIGHT, pos, ctx).unwrap();
        let (root, _) = t.insert(&v, v1()).unwrap().unwrap();
        assert_eq!(root, legacy_root, "pos {pos}");
        ctx = t.storage;
    }
}

// ── Generations ────────────────────────────────────────────────────────

/// After `reset` the slot keys are reused with different values. Records
/// from the earlier epoch are still in storage; they carry the old
/// generation and must never be read as current — the root of the new epoch
/// must be what a walk over the new values gives.
#[test]
fn records_from_an_earlier_epoch_are_never_trusted() {
    const HEIGHT: u8 = 3;
    let mut tree = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    for pos in 0..7u16 {
        tree.insert(&value(pos, 0xAA), v1()).unwrap().unwrap();
    }
    assert_eq!(tree.generation(), 0);
    tree.reset();
    assert_eq!(tree.generation(), 1);
    assert_eq!(tree.count(), 0);
    // Stale records for every position, tagged generation 0.
    assert!((0..7u16).all(|p| stored_record(&tree.storage, p).map(|r| r.generation) == Some(0)));

    let mut legacy = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    for pos in 0..7u16 {
        let v = value(pos, 0xBB);
        let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
        let (root, _) = tree
            .try_insert_with_accounting(
                &v,
                SlotWriteAccounting::Overwrite {
                    previous_value_len: value(pos, 0xAA).len() as u32,
                },
                v1(),
            )
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(root, legacy_root, "epoch 2 pos {pos}");
        assert_eq!(
            stored_record(&tree.storage, pos).map(|r| r.generation),
            Some(1)
        );
    }

    // The same over a reopened tree whose owner sets the generation: storage
    // holds generation-0 records for values that are no longer there.
    let ctx = MemStorageContext::new();
    let mut epoch0 = DenseFixedSizedMerkleTree::new(HEIGHT, ctx).unwrap();
    for pos in 0..7u16 {
        epoch0.insert(&value(pos, 0xAA), v1()).unwrap().unwrap();
    }
    let ctx = epoch0.storage;
    // Overwrite the VALUES directly (as the next epoch would), leaving the
    // generation-0 records behind.
    for pos in 0..5u16 {
        ctx.data
            .borrow_mut()
            .insert(position_key(pos).to_vec(), value(pos, 0xCC));
    }
    let mut reopened = DenseFixedSizedMerkleTree::from_state(HEIGHT, 5, ctx).unwrap();
    reopened.set_generation(1);
    let mut legacy = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    for pos in 0..5u16 {
        legacy.insert(&value(pos, 0xCC), v0()).unwrap().unwrap();
    }
    // Root read: the generation-0 record at 0 is not current → walk.
    assert_eq!(
        reopened.root_hash(v1()).unwrap().unwrap(),
        legacy.root_hash(v0()).unwrap().unwrap()
    );
    // Insert at 5: siblings 3 and 4 / parent 2 / root 0 have only stale
    // records → recomputed from the new values.
    let v = value(5, 0xCC);
    let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
    let (root, _) = reopened.insert(&v, v1()).unwrap().unwrap();
    assert_eq!(root, legacy_root);
    // And a record whose bytes do not parse is not trusted either.
    reopened
        .storage
        .data
        .borrow_mut()
        .insert(record_key(0).to_vec(), vec![1, 2, 3]);
    let fresh = DenseFixedSizedMerkleTree::from_state(HEIGHT, 6, reopened.storage).unwrap();
    assert_eq!(fresh.root_hash(v1()).unwrap().unwrap(), legacy_root);
}

// ── Cost shape ─────────────────────────────────────────────────────────

/// Under version 1 an insert at depth `d` hashes `2 + d` times, writes `d +
/// 1` records, reads at most `2d` records and never reads a value back —
/// whatever the fill level. (Storage reads are counted from the storage
/// log: this in-memory context does not charge seeks itself.)
#[test]
fn v1_insert_work_is_bounded_by_depth_not_by_count() {
    const HEIGHT: u8 = 8;
    let mut ctx = MemStorageContext::new();
    let capacity = (1u16 << HEIGHT) - 1;
    for pos in 0..capacity {
        // Fresh session each time: every record read goes to storage.
        let mut tree = DenseFixedSizedMerkleTree::from_state(HEIGHT, pos, ctx).unwrap();
        clear_logs(&tree.storage);
        let out = tree.insert(&value(pos, 5), v1());
        out.value.unwrap();
        let d = depth(pos);
        assert_eq!(out.cost.hash_node_calls, 2 + d, "pos {pos}: hashes");
        let puts = record_puts(&tree.storage);
        assert_eq!(puts.len() as u32, d + 1, "pos {pos}: record writes");
        let reads = record_gets(&tree.storage);
        assert!(
            reads.len() as u32 <= 2 * d,
            "pos {pos}: {} record reads > 2 * depth {d}",
            reads.len()
        );
        assert!(
            value_gets(&tree.storage).is_empty(),
            "pos {pos}: values read back: {:?}",
            value_gets(&tree.storage)
        );
        // The leaf's record is new storage (the slot was never written);
        // the ancestors' records were written by earlier sessions and are
        // rewritten in place.
        for (p, cost_info) in &puts {
            if *p == pos {
                assert!(cost_info.is_none(), "pos {pos}: own record is new");
            } else {
                let c = cost_info.as_ref().expect("ancestor record is a rewrite");
                assert!(!c.new_node);
                assert_eq!(
                    c.value_storage_cost.replaced_bytes,
                    HASH_RECORD_LEN as u32 + 1
                );
                assert_eq!(c.value_storage_cost.added_bytes, 0);
            }
        }
        ctx = tree.storage;
    }
}

/// Within one session the cost is the same as across sessions (cache hits
/// are charged like the reads they stand in for), and the record writes of
/// positions first written in this session stay reported as new storage
/// however often the session rewrites them — the batch keeps one put per
/// key and the key did not exist before the session.
#[test]
fn v1_costs_are_session_independent_and_new_keys_stay_new_within_a_session() {
    const HEIGHT: u8 = 4;
    let mut sessions = MemStorageContext::new();
    let mut single = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    for pos in 0..((1u16 << HEIGHT) - 1) {
        let mut tree = DenseFixedSizedMerkleTree::from_state(HEIGHT, pos, sessions).unwrap();
        let per_session = tree.insert(&value(pos, 2), v1());
        let in_session = single.insert(&value(pos, 2), v1());
        assert_eq!(
            per_session.cost, in_session.cost,
            "pos {pos}: cost must not depend on what the session has cached"
        );
        assert_eq!(per_session.value.unwrap(), in_session.value.unwrap());
        sessions = tree.storage;
    }
    assert!(
        record_puts(&single.storage)
            .iter()
            .all(|(_, c)| c.is_none()),
        "every record key was created in this session: no put may claim a rewrite"
    );
}

/// A slot the owner reports as a committed rewrite may carry a record from
/// an earlier epoch: the record write is then sized as a rewrite, and the
/// key is read once to find out. A slot reported new is not read.
#[test]
fn overwrite_slots_read_their_record_to_size_the_write() {
    let mut tree = DenseFixedSizedMerkleTree::new(2, MemStorageContext::new()).unwrap();
    for pos in 0..3u16 {
        tree.insert(&value(pos, 1), v1()).unwrap().unwrap();
    }
    tree.reset();
    clear_logs(&tree.storage);
    tree.try_insert_with_accounting(
        &value(0, 2),
        SlotWriteAccounting::Overwrite {
            previous_value_len: value(0, 1).len() as u32,
        },
        v1(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(record_gets(&tree.storage), vec![0]);
    let puts = record_puts(&tree.storage);
    assert_eq!(puts.len(), 1);
    assert!(puts[0].1.as_ref().is_some_and(|c| !c.new_node));

    // The same slot reported new: no record read, record written as new.
    let mut fresh = DenseFixedSizedMerkleTree::new(2, MemStorageContext::new()).unwrap();
    fresh
        .try_insert_with_accounting(&value(0, 2), SlotWriteAccounting::AsNew, v1())
        .unwrap()
        .unwrap();
    assert!(record_gets(&fresh.storage).is_empty());
    assert!(record_puts(&fresh.storage)[0].1.is_none());
}

/// A root read under version 1 is one record read (charged as a seek and
/// the record bytes) and no hashing; an empty tree costs nothing.
#[test]
fn v1_root_read_costs_one_record_read() {
    let tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    let empty = tree.root_hash(v1());
    assert_eq!(empty.value.unwrap(), [0u8; 32]);
    assert_eq!(empty.cost, Default::default());

    let mut tree = tree;
    tree.insert(&value(0, 1), v1()).unwrap().unwrap();
    let same_session = tree.root_hash(v1());
    assert_eq!(same_session.cost.hash_node_calls, 0);
    assert_eq!(same_session.cost.seek_count, 1);
    assert_eq!(
        same_session.cost.storage_loaded_bytes,
        HASH_RECORD_LEN as u64
    );
    let reopened = DenseFixedSizedMerkleTree::from_state(3, 1, tree.storage).unwrap();
    clear_logs(&reopened.storage);
    let cold = reopened.root_hash(v1());
    assert_eq!(cold.value.unwrap(), same_session.value.unwrap());
    assert_eq!(cold.cost.hash_node_calls, 0);
    assert_eq!(record_gets(&reopened.storage), vec![0]);
}

// ── Failure and version handling ───────────────────────────────────────

/// A storage fault while rewriting the ancestor path rolls the in-memory
/// tree back (count, cached value, cached records) so the session can go on
/// — or be discarded — consistently.
#[test]
fn v1_insert_failure_rolls_back_the_in_memory_state() {
    let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    for pos in 0..3u16 {
        tree.insert(&value(pos, 1), v1()).unwrap().unwrap();
    }
    let root_before = tree.root_hash(v1()).unwrap().unwrap();
    tree.storage.fail_record_puts.set(true);
    let failed = tree.insert(&value(3, 1), v1());
    assert!(matches!(failed.value, Err(DenseMerkleError::StoreError(_))));
    assert_eq!(tree.count(), 3, "count rolled back");
    assert_eq!(tree.get(3).unwrap().unwrap(), None, "value not visible");
    tree.storage.fail_record_puts.set(false);
    // The records were cleared from memory; the root is re-read from storage
    // and still describes the three committed positions.
    assert_eq!(tree.root_hash(v1()).unwrap().unwrap(), root_before);
    // And the session can continue.
    let mut legacy = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    for pos in 0..4u16 {
        legacy.insert(&value(pos, 1), v0()).unwrap().unwrap();
    }
    let (root, pos) = tree.insert(&value(3, 1), v1()).unwrap().unwrap();
    assert_eq!(pos, 3);
    assert_eq!(root, legacy.root_hash(v0()).unwrap().unwrap());
}

/// Every versioned entry point rejects a root-maintenance version this
/// crate does not know, without touching storage.
#[test]
fn unknown_root_maintenance_version_is_rejected() {
    let mut bad = GroveVersion::latest().clone();
    bad.dense_tree_versions.root_maintenance = 7;
    let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    assert!(matches!(
        tree.insert(b"x", &bad).value,
        Err(DenseMerkleError::VersionError(_))
    ));
    assert!(matches!(
        tree.try_insert(b"x", &bad).value,
        Err(DenseMerkleError::VersionError(_))
    ));
    assert!(matches!(
        tree.try_insert_no_root(b"x", &bad).value,
        Err(DenseMerkleError::VersionError(_))
    ));
    assert!(matches!(
        tree.root_hash(&bad).value,
        Err(DenseMerkleError::VersionError(_))
    ));
    assert_eq!(tree.count(), 0);
    assert!(tree.storage.puts.borrow().is_empty());
}

/// The record encoding round-trips and rejects other lengths.
#[test]
fn hash_record_encoding_round_trips() {
    let record = HashRecord {
        generation: 0x0102_0304_0506_0708,
        value_hash: [0xAB; 32],
        node_hash: [0xCD; 32],
    };
    let bytes = record.to_bytes();
    assert_eq!(bytes.len(), HASH_RECORD_LEN);
    assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(HashRecord::from_bytes(&bytes), Some(record));
    assert_eq!(HashRecord::from_bytes(&bytes[..71]), None);
    assert_eq!(HashRecord::from_bytes(&[0u8; 73]), None);
    assert_eq!(record_key(0x0102), [b'h', 1, 2]);
}

/// A storage fault on a record READ surfaces as a store error from every
/// path that reads records — the sibling / parent lookups of an insert, the
/// sizing read of an `Overwrite` slot, and the root read — and the insert
/// rolls back exactly as on a write fault.
#[test]
fn v1_record_read_faults_surface_and_roll_back() {
    let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    for pos in 0..3u16 {
        tree.insert(&value(pos, 1), v1()).unwrap().unwrap();
    }
    // A cold session so the reads go to storage.
    let mut tree = DenseFixedSizedMerkleTree::from_state(3, 3, tree.storage).unwrap();
    tree.storage.fail_record_gets.set(true);
    assert!(matches!(
        tree.root_hash(v1()).value,
        Err(DenseMerkleError::StoreError(_))
    ));
    let failed = tree.insert(&value(3, 1), v1());
    assert!(matches!(failed.value, Err(DenseMerkleError::StoreError(_))));
    assert_eq!(tree.count(), 3);
    // An `Overwrite` slot reads its own record to size the write.
    tree.reset();
    let failed = tree.try_insert_with_accounting(
        &value(0, 2),
        SlotWriteAccounting::Overwrite {
            previous_value_len: 4,
        },
        v1(),
    );
    assert!(matches!(failed.value, Err(DenseMerkleError::StoreError(_))));
    assert_eq!(tree.count(), 0);
    tree.storage.fail_record_gets.set(false);
    // Healthy again: the session continues from the rolled-back state.
    let (_, pos) = tree.insert(&value(0, 2), v1()).unwrap().unwrap();
    assert_eq!(pos, 0);
}

/// A parent whose record is missing AND whose value is missing is store
/// corruption, reported as such rather than hashed over nothing.
#[test]
fn v1_missing_parent_value_is_a_store_error() {
    let mut tree = DenseFixedSizedMerkleTree::new(2, MemStorageContext::new()).unwrap();
    tree.insert(&value(0, 1), v0()).unwrap().unwrap();
    // Drop the root's value behind the tree's back (no record exists: v0).
    tree.storage
        .data
        .borrow_mut()
        .remove(position_key(0).as_slice());
    let mut cold = DenseFixedSizedMerkleTree::from_state(2, 1, tree.storage).unwrap();
    let failed = cold.insert(&value(1, 1), v1());
    assert!(matches!(failed.value, Err(DenseMerkleError::StoreError(_))));
    assert_eq!(cold.count(), 1);
}
