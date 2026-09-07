//! Tests for the versioned root maintenance: version 1 (one path record per
//! insert, GROVE_V4+) must produce exactly the roots version 0 (recompute
//! from every filled position, GROVE_V1..V3) produces, under every fill
//! level, across sessions, after a buffer filled under version 0, and over
//! records left by an earlier epoch — while doing O(height) work per insert
//! and charging a fixed, height-derived figure for it.

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
use grovedb_version::version::GroveVersion;

use crate::{
    depth_of, last_filled_in_subtree, path_record_len, position_key, record_key,
    test_utils::MemStorageContext, v1_insert_model_cost, DenseFixedSizedMerkleTree,
    DenseMerkleError, PathRecord, SlotWriteAccounting, V1InsertModel,
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

/// Record puts (3-byte keys) made on `ctx`, in order.
fn record_puts(ctx: &MemStorageContext) -> Vec<(u16, Option<KeyValueStorageCost>)> {
    ctx.puts
        .borrow()
        .iter()
        .filter(|(k, _)| k.len() == 3)
        .map(|(k, c)| (u16::from_be_bytes([k[1], k[2]]), c.clone()))
        .collect()
}

/// Storage gets of path records (3-byte keys) made on `ctx`, in order.
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

/// The stored record for the insert at `pos`, if any, for a tree of `height`.
fn stored_record(ctx: &MemStorageContext, pos: u16, height: u8) -> Option<PathRecord> {
    ctx.data
        .borrow()
        .get(record_key(pos).as_slice())
        .and_then(|b| PathRecord::from_bytes(b, height))
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

            // A fresh session per insert: the path records must be found in
            // storage, not in a cache.
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
            assert_eq!(
                tree.recorded_root().unwrap().unwrap(),
                Some(legacy_root),
                "height {height} pos {pos}: recorded root"
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

/// `try_insert_no_root` under version 1 writes the path record exactly as
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

/// `last_filled_in_subtree` locates the insert whose record holds a
/// subtree's current hash: the largest filled position below it.
#[test]
fn last_filled_in_subtree_is_the_largest_filled_descendant() {
    // Height 4, 11 filled (positions 0..=10).
    assert_eq!(last_filled_in_subtree(0, 11, 4), Some(10));
    assert_eq!(last_filled_in_subtree(1, 11, 4), Some(10)); // subtree of 1: 3,4,7..10
    assert_eq!(last_filled_in_subtree(2, 11, 4), Some(6)); // subtree of 2: 5,6,11..14
    assert_eq!(last_filled_in_subtree(5, 11, 4), Some(5)); // children 11,12 not filled
    assert_eq!(last_filled_in_subtree(3, 11, 4), Some(8)); // children 7,8
    assert_eq!(last_filled_in_subtree(4, 11, 4), Some(10)); // children 9,10
    assert_eq!(last_filled_in_subtree(10, 11, 4), Some(10));
    assert_eq!(last_filled_in_subtree(11, 11, 4), None);
    assert_eq!(depth_of(0), 0);
    assert_eq!(depth_of(2), 1);
    assert_eq!(depth_of(6), 2);
    assert_eq!(depth_of(7), 3);
}

// ── Legacy buffers ─────────────────────────────────────────────────────

/// A buffer filled under version 0 has no records. The first version-1
/// insert derives what it needs from the values (the walk) and records it;
/// every later insert is O(height) again and reads no value back; the roots
/// match version 0 throughout — and every insert is charged the same fixed
/// model.
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
        (0..LEGACY_FILL).all(|p| stored_record(&ctx, p, HEIGHT).is_none()),
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
    assert_eq!(reopened.recorded_root().unwrap().unwrap(), None);
    let ctx = reopened.storage;

    // First version-1 insert: catch-up — reads legacy values, writes only
    // its own record, and is charged the model.
    let mut tree = DenseFixedSizedMerkleTree::from_state(HEIGHT, LEGACY_FILL, ctx).unwrap();
    clear_logs(&tree.storage);
    let v = value(LEGACY_FILL, 9);
    let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
    let first = tree.insert(&v, v1());
    let (root, _) = first.value.unwrap();
    assert_eq!(root, legacy_root);
    assert_eq!(first.cost, v1_insert_model_cost(HEIGHT), "billed the model");
    assert!(
        !value_gets(&tree.storage).is_empty(),
        "the catch-up walks values"
    );
    assert_eq!(
        record_puts(&tree.storage).len(),
        1,
        "the catch-up writes nothing but the insert's own record"
    );

    // Second version-1 insert: one record put, the model, and only legacy
    // values read back (its parent 9 and the sibling subtrees that have no
    // version-1 insert yet).
    clear_logs(&tree.storage);
    let pos = LEGACY_FILL + 1;
    let v = value(pos, 9);
    let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
    let second = tree.insert(&v, v1());
    let (root, _) = second.value.unwrap();
    assert_eq!(root, legacy_root);
    assert_eq!(second.cost, v1_insert_model_cost(HEIGHT));
    assert_eq!(
        record_puts(&tree.storage).len(),
        1,
        "one path record per insert"
    );
    assert!(
        value_gets(&tree.storage).iter().all(|p| *p < LEGACY_FILL),
        "only legacy values are read back: {:?}",
        value_gets(&tree.storage)
    );

    // And the rest of the buffer, in fresh sessions, keeps agreeing. Only
    // legacy (V0-inserted) values are ever read back — a legacy parent's
    // value hash, or a walk of a legacy sibling subtree no version-1 insert
    // has landed in yet — every insert writes exactly one record and is
    // charged the model.
    let mut ctx = tree.storage;
    for pos in (LEGACY_FILL + 2)..((1u16 << HEIGHT) - 1) {
        let v = value(pos, 9);
        let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
        let mut t = DenseFixedSizedMerkleTree::from_state(HEIGHT, pos, ctx).unwrap();
        clear_logs(&t.storage);
        let out = t.insert(&v, v1());
        assert_eq!(out.cost, v1_insert_model_cost(HEIGHT), "pos {pos}: model");
        let (root, _) = out.value.unwrap();
        assert_eq!(root, legacy_root, "pos {pos}");
        assert_eq!(record_puts(&t.storage).len(), 1, "pos {pos}: one record");
        for read in value_gets(&t.storage) {
            assert!(
                read < LEGACY_FILL,
                "pos {pos}: read a V4-inserted value {read}"
            );
        }
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
    assert!(
        (0..7u16).all(|p| stored_record(&tree.storage, p, HEIGHT).map(|r| r.generation) == Some(0))
    );

    let mut legacy = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    for pos in 0..7u16 {
        let v = value(pos, 0xBB);
        let (legacy_root, _) = legacy.insert(&v, v0()).unwrap().unwrap();
        let (root, _) = tree
            .try_insert_with_accounting(&v, SlotWriteAccounting::Churn, v1())
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(root, legacy_root, "epoch 2 pos {pos}");
        assert_eq!(
            stored_record(&tree.storage, pos, HEIGHT).map(|r| r.generation),
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
    // Root read: the generation-0 record of the last insert is not current
    // → walk.
    assert_eq!(
        reopened.root_hash(v1()).unwrap().unwrap(),
        legacy.root_hash(v0()).unwrap().unwrap()
    );
    assert_eq!(reopened.recorded_root().unwrap().unwrap(), None);
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
        .insert(record_key(5).to_vec(), vec![1, 2, 3]);
    let fresh = DenseFixedSizedMerkleTree::from_state(HEIGHT, 6, reopened.storage).unwrap();
    assert_eq!(fresh.root_hash(v1()).unwrap().unwrap(), legacy_root);
    assert_eq!(fresh.recorded_root().unwrap().unwrap(), None);
}

// ── Cost: fixed model, bounded work ────────────────────────────────────

/// Under version 1 every insert into a tree of a given height is charged
/// the same figure — the height's model — whatever its position and
/// whatever the session has cached; and the work it actually performs is
/// bounded by depth: exactly one record put, at most `2 · depth` record
/// reads, no value read back. (Storage I/O is counted from the storage log:
/// this in-memory context charges reads like a real store.)
#[test]
fn v1_insert_is_charged_the_fixed_model_and_works_within_depth() {
    const HEIGHT: u8 = 8;
    let model = v1_insert_model_cost(HEIGHT);
    let mut ctx = MemStorageContext::new();
    let mut single = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    let capacity = (1u16 << HEIGHT) - 1;
    for pos in 0..capacity {
        // Fresh session each time: every record read goes to storage.
        let mut tree = DenseFixedSizedMerkleTree::from_state(HEIGHT, pos, ctx).unwrap();
        clear_logs(&tree.storage);
        let out = tree.insert(&value(pos, 5), v1());
        let in_session = single.insert(&value(pos, 5), v1());
        assert_eq!(out.value.unwrap(), in_session.value.unwrap());
        assert_eq!(
            out.cost, model,
            "pos {pos}: cold session is charged the model"
        );
        assert_eq!(
            in_session.cost, model,
            "pos {pos}: warm session is charged the model"
        );

        let d = depth_of(pos) as usize;
        let puts = record_puts(&tree.storage);
        assert_eq!(puts.len(), 1, "pos {pos}: exactly one record put");
        assert_eq!(
            puts[0].0, pos,
            "pos {pos}: under the inserting position's key"
        );
        assert!(
            puts[0].1.is_none(),
            "pos {pos}: a never-written key is new storage for an AsNew owner"
        );
        let reads = record_gets(&tree.storage);
        assert!(
            reads.len() <= 2 * d,
            "pos {pos}: {} record reads > 2 * depth {d}",
            reads.len()
        );
        assert!(
            value_gets(&tree.storage).is_empty(),
            "pos {pos}: values read back: {:?}",
            value_gets(&tree.storage)
        );
        // The record is the fixed size for the height.
        let stored = tree.storage.data.borrow();
        assert_eq!(
            stored.get(record_key(pos).as_slice()).map(|b| b.len()),
            Some(path_record_len(HEIGHT))
        );
        drop(stored);
        ctx = tree.storage;
    }
}

/// The model's figures: the epoch averages, rounded up, for the heights the
/// append-only family uses.
#[test]
fn v1_model_is_the_rounded_up_epoch_average() {
    // Height 1: one position at depth 0 — two hashes, no reads.
    let m = V1InsertModel::for_height(1);
    assert_eq!((m.hash_node_calls, m.record_reads), (2, 0));
    // Height 2: depths 0,1,1 → average 2/3 → 3 hashes; reads 2·2 − 1 = 3 over
    // 3 positions → 1.
    let m = V1InsertModel::for_height(2);
    assert_eq!((m.hash_node_calls, m.record_reads), (3, 1));
    // Height 11 (the shielded pool): Σdepth = 9·2048 + 2 = 18434 over 2047
    // positions → 9.005 → 12 hashes; reads (2·18434 − 1023) / 2047 = 17.5 →
    // 18; a record is 42 + 32·11 = 394 bytes.
    let m = V1InsertModel::for_height(11);
    assert_eq!(
        (m.hash_node_calls, m.record_reads, m.record_len),
        (12, 18, 394)
    );
    assert_eq!(
        m.cost().storage_loaded_bytes,
        18 * 394,
        "loaded bytes are the reads times the record size"
    );
    // Height 16 (the ceiling): 14·65536 + 2 over 65535 → 14.0002 → 17
    // hashes; reads (2·917506 − 32767)/65535 = 27.5 → 28.
    let m = V1InsertModel::for_height(16);
    assert_eq!((m.hash_node_calls, m.record_reads), (17, 28));
    // Monotone in height, and never below the true epoch average.
    for height in 2..=8u8 {
        let model = V1InsertModel::for_height(height);
        let prev = V1InsertModel::for_height(height - 1);
        assert!(model.hash_node_calls >= prev.hash_node_calls);
        assert!(model.record_reads >= prev.record_reads);
        let capacity = (1u32 << height) - 1;
        let total_depth: u32 = (0..capacity as u16).map(|p| depth_of(p) as u32).sum();
        assert!(
            model.hash_node_calls as u64 * capacity as u64 >= (2 * capacity + total_depth) as u64
        );
    }
}

/// Record writes follow the owner's slot accounting: `Churn` reports an
/// in-place replacement of the fixed record size and never reads to size
/// it; `Overwrite` reads the key and reports a rewrite only if it exists;
/// `AsNew` is new storage.
#[test]
fn record_put_sizing_follows_the_slot_accounting() {
    const HEIGHT: u8 = 2;
    let len = path_record_len(HEIGHT) as u32;
    let paid = len + 1;

    // Churn on a fresh tree: replaced = paid size, nothing added, no key.
    let mut churn = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
    churn
        .try_insert_with_accounting(&value(0, 2), SlotWriteAccounting::Churn, v1())
        .unwrap()
        .unwrap();
    let puts = record_puts(&churn.storage);
    let c = puts[0].1.as_ref().expect("churn carries cost info");
    assert!(!c.new_node);
    assert_eq!(c.value_storage_cost.replaced_bytes, paid);
    assert_eq!(c.value_storage_cost.added_bytes, 0);
    assert_eq!(c.key_storage_cost, Default::default());
    assert!(
        record_gets(&churn.storage).is_empty(),
        "churn never reads to size"
    );
    // The slot put is churn too.
    let slot = churn
        .storage
        .puts
        .borrow()
        .iter()
        .find(|(k, _)| k.len() == 2)
        .map(|(_, c)| c.clone())
        .expect("slot put");
    let slot = slot.expect("churn slot carries cost info");
    assert_eq!(slot.value_storage_cost.added_bytes, 0);
    assert_eq!(
        slot.value_storage_cost.replaced_bytes,
        value(0, 2).len() as u32 + 1
    );

    // Overwrite after a reset: the key exists from the earlier epoch → a
    // rewrite, sized after one read.
    let mut tree = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
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

    // AsNew: no read, new storage.
    let mut fresh = DenseFixedSizedMerkleTree::new(HEIGHT, MemStorageContext::new()).unwrap();
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
    tree.insert(&value(1, 1), v1()).unwrap().unwrap();
    let same_session = tree.root_hash(v1());
    assert_eq!(same_session.cost.hash_node_calls, 0);
    assert_eq!(same_session.cost.seek_count, 1);
    assert_eq!(
        same_session.cost.storage_loaded_bytes,
        path_record_len(3) as u64
    );
    let reopened = DenseFixedSizedMerkleTree::from_state(3, 2, tree.storage).unwrap();
    clear_logs(&reopened.storage);
    let cold = reopened.root_hash(v1());
    assert_eq!(cold.value.unwrap(), same_session.value.unwrap());
    assert_eq!(cold.cost.hash_node_calls, 0);
    assert_eq!(
        record_gets(&reopened.storage),
        vec![1],
        "the last insert's record"
    );
}

// ── Failure and version handling ───────────────────────────────────────

/// A storage fault while writing the path record rolls the in-memory tree
/// back (count, cached value, cached records) so the session can go on — or
/// be discarded — consistently.
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
    assert_eq!(tree.root_hash(v1()).unwrap().unwrap(), root_before);
    let mut legacy = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    for pos in 0..4u16 {
        legacy.insert(&value(pos, 1), v0()).unwrap().unwrap();
    }
    let (root, pos) = tree.insert(&value(3, 1), v1()).unwrap().unwrap();
    assert_eq!(pos, 3);
    assert_eq!(root, legacy.root_hash(v0()).unwrap().unwrap());
}

/// A storage fault on a record READ surfaces as a store error from every
/// path that reads records — the lookups of an insert, the sizing read of
/// an `Overwrite` slot, the root read — and the insert rolls back.
#[test]
fn v1_record_read_faults_surface_and_roll_back() {
    let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new()).unwrap();
    for pos in 0..3u16 {
        tree.insert(&value(pos, 1), v1()).unwrap().unwrap();
    }
    let mut tree = DenseFixedSizedMerkleTree::from_state(3, 3, tree.storage).unwrap();
    tree.storage.fail_record_gets.set(true);
    assert!(matches!(
        tree.root_hash(v1()).value,
        Err(DenseMerkleError::StoreError(_))
    ));
    let failed = tree.insert(&value(3, 1), v1());
    assert!(matches!(failed.value, Err(DenseMerkleError::StoreError(_))));
    assert_eq!(tree.count(), 3);
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
    let (_, pos) = tree.insert(&value(0, 2), v1()).unwrap().unwrap();
    assert_eq!(pos, 0);
}

/// A parent whose record is missing AND whose value is missing is store
/// corruption, reported as such rather than hashed over nothing.
#[test]
fn v1_missing_parent_value_is_a_store_error() {
    let mut tree = DenseFixedSizedMerkleTree::new(2, MemStorageContext::new()).unwrap();
    tree.insert(&value(0, 1), v0()).unwrap().unwrap();
    tree.storage
        .data
        .borrow_mut()
        .remove(position_key(0).as_slice());
    let mut cold = DenseFixedSizedMerkleTree::from_state(2, 1, tree.storage).unwrap();
    let failed = cold.insert(&value(1, 1), v1());
    assert!(matches!(failed.value, Err(DenseMerkleError::StoreError(_))));
    assert_eq!(cold.count(), 1);
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

/// The record encoding round-trips, is fixed-size per height, and rejects
/// other lengths.
#[test]
fn path_record_encoding_round_trips() {
    let mut record = PathRecord::new(0x0102_0304_0506_0708, [0xAB; 32], 4);
    record.set_entry(0, [1; 32]);
    record.set_entry(2, [3; 32]);
    assert_eq!(record.entry(0), Some([1; 32]));
    assert_eq!(record.entry(1), None);
    assert_eq!(record.entry(2), Some([3; 32]));
    assert_eq!(record.entry(3), None);
    let bytes = record.to_bytes();
    assert_eq!(bytes.len(), path_record_len(4));
    assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(PathRecord::from_bytes(&bytes, 4), Some(record));
    assert_eq!(PathRecord::from_bytes(&bytes, 5), None);
    assert_eq!(PathRecord::from_bytes(&bytes[..bytes.len() - 1], 4), None);
    assert_eq!(record_key(0x0102), [b'h', 1, 2]);
    assert_eq!(path_record_len(11), 394);
}
