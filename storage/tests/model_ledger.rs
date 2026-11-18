//! Everything the RocksDB engine reports is a cost the cost model handed
//! out, and everything the model handed out is reported.
//!
//! The cost model keeps a ledger for tests: the sum, per thread, of every
//! `OperationCost` its functions returned to be reported. This test calls
//! every operation of the engine that reports a cost, adds up what the
//! calls returned, and compares the two sums. A charge the engine worked
//! out beside the model would be in the first and not in the second, and a
//! charge of the model that the engine dropped would be in the second
//! only. It does not matter how such code is written, which is what the
//! check of the source in `charging.rs` cannot say.
//!
//! The test calls no function of the cost model itself.

#![cfg(feature = "rocksdb_storage")]

use grovedb_costs::{
    cost_model::ledger,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{StorageRemovalPerEpochByIdentifier, StorageRemovedBytes},
        StorageCost,
    },
    CostContext, OperationCost,
};
use grovedb_storage::{
    rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
    worst_case_costs::WorstKeyLength,
    Batch, Error, FlatStore, RawIterator, Storage, StorageBatch, StorageContext,
};

const SUBTREE: &[&[u8]] = &[b"ledger"];
const DEEP_SUBTREE: &[&[u8]] = &[b"ledger", &[7; 60], b""];

/// What the engine reported, added up in the order it was reported.
#[derive(Default)]
struct Reported(OperationCost);

impl Reported {
    fn by<T>(&mut self, call: CostContext<T>) -> T {
        self.0 += call.cost;
        call.value
    }

    fn cost(&mut self, cost: OperationCost) {
        self.0 += cost;
    }

    /// Asserts that the model handed out, since the last call, what the
    /// engine reported since then.
    fn is_what_the_model_issued(&mut self, of: &str) -> OperationCost {
        let reported = std::mem::take(&mut self.0);
        assert_eq!(reported, ledger::take(), "{of}");
        reported
    }
}

struct Longest(u8);

impl WorstKeyLength for Longest {
    fn max_length(&self) -> u8 {
        self.0
    }
}

fn sectioned_removal() -> KeyValueStorageCost {
    let mut by_owner = StorageRemovalPerEpochByIdentifier::default();
    by_owner.entry([0; 32]).or_default().insert(3, 50);
    KeyValueStorageCost {
        key_storage_cost: StorageCost {
            removed_bytes: StorageRemovedBytes::SectionedStorageRemoval(by_owner),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn plain_removal() -> KeyValueStorageCost {
    KeyValueStorageCost {
        key_storage_cost: StorageCost {
            removed_bytes: StorageRemovedBytes::BasicStorageRemoval(7),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn mismatched() -> KeyValueStorageCost {
    KeyValueStorageCost {
        value_storage_cost: StorageCost {
            added_bytes: 1,
            ..Default::default()
        },
        needs_value_verification: true,
        ..Default::default()
    }
}

fn new_record(value_cost: u32) -> KeyValueStorageCost {
    KeyValueStorageCost {
        value_storage_cost: StorageCost {
            added_bytes: value_cost,
            ..Default::default()
        },
        new_node: true,
        needs_value_verification: true,
        ..Default::default()
    }
}

/// Every write of a storage context, once with no cost and once with one.
fn write_everything<'db, C: StorageContext<'db>>(context: &C, reported: &mut Reported) {
    let results = [
        context.put(b"node", b"a record", None, None),
        context.put(b"new", b"value", None, Some(new_record(6))),
        context.put(
            b"paid",
            b"value",
            None,
            Some(KeyValueStorageCost::prepaid()),
        ),
        context.put_aux(b"aux", b"an aux record", None),
        context.put_root(b"r", b"node", None),
        context.put_root(
            b"s",
            b"node",
            Some(KeyValueStorageCost::for_updated_root_cost(None, 4)),
        ),
        context.put_meta(b"meta", b"a meta record", None),
        context.delete(b"absent", None),
        context.delete(b"removed", Some(sectioned_removal())),
        context.delete_aux(b"absent", None),
        context.delete_aux(b"removed", Some(plain_removal())),
        context.delete_root(b"absent", None),
        context.delete_root(b"removed", Some(plain_removal())),
        context.delete_meta(b"absent", None),
        context.delete_meta(b"removed", Some(plain_removal())),
    ];
    for result in results {
        reported.by(result).expect("a write");
    }
}

fn read_everything<'db, C: StorageContext<'db>>(context: &C, reported: &mut Reported) {
    for key in [b"node".as_slice(), b"aux", b"r", b"meta", b"absent"] {
        reported.by(context.get(key)).expect("a read");
        reported.by(context.get_aux(key)).expect("a read");
        reported.by(context.get_root(key)).expect("a read");
        reported.by(context.get_meta(key)).expect("a read");
    }
}

/// Every move and every question of a cursor, on records, past the last
/// record and before the first.
fn walk<I: RawIterator>(mut cursor: I, reported: &mut Reported) -> usize {
    let mut records = 0;
    reported.by(cursor.valid());
    reported.by(cursor.key());
    reported.by(cursor.value());
    reported.by(cursor.seek_to_first());
    while reported.by(cursor.valid()) {
        reported.by(cursor.key()).expect("a key");
        reported.by(cursor.value()).expect("a value");
        reported.by(cursor.next());
        records += 1;
    }
    reported.by(cursor.key());
    reported.by(cursor.value());
    reported.by(cursor.seek_to_last());
    reported.by(cursor.valid());
    reported.by(cursor.prev());
    reported.by(cursor.prev());
    reported.by(cursor.valid());
    reported.by(cursor.seek(b"m"));
    reported.by(cursor.key());
    reported.by(cursor.seek_for_prev(b"m"));
    reported.by(cursor.value());
    records
}

fn fill<B: Batch>(batch: &mut B) {
    batch.put(b"node", b"a record", None, None).expect("a put");
    batch
        .put(b"new", b"value", None, Some(new_record(6)))
        .expect("a put");
    batch
        .put_aux(b"aux", b"an aux record", None)
        .expect("a put");
    batch.put_root(b"r", b"node", None).expect("a put");
    batch.delete(b"absent", None);
    batch.delete_aux(b"removed", Some(plain_removal()));
    batch.delete_root(b"removed", Some(sectioned_removal()));
}

#[test]
fn the_engine_reports_what_the_model_hands_out_and_nothing_else() {
    let storage = TempStorage::new();
    let mut reported = Reported::default();
    let mut total = OperationCost::default();
    ledger::take();

    for path in [&[][..], SUBTREE, DEEP_SUBTREE] {
        reported.by(RocksDbStorage::build_prefix(path.into()));
        let longest: Vec<Longest> = path
            .iter()
            .map(|segment| Longest(segment.len() as u8))
            .collect();
        reported.cost(RocksDbStorage::get_storage_context_cost(&longest));
    }
    let prefix = reported.by(RocksDbStorage::build_prefix(SUBTREE.into()));
    reported.by(RocksDbStorage::secondary_prefix_for(&prefix, 1));
    total += reported.is_what_the_model_issued("prefixes");

    let tx = storage.start_transaction();
    let batch = StorageBatch::new();
    let deferred =
        reported.by(storage.get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx));
    reported.by(storage.get_transactional_storage_context(DEEP_SUBTREE.into(), None, &tx));
    reported.by(storage.get_transactional_storage_context_by_subtree_prefix(prefix, None, &tx));
    total += reported.is_what_the_model_issued("deferred handles");

    write_everything(&deferred, &mut reported);
    let mut part = deferred.new_batch();
    fill(&mut part);
    reported
        .by(deferred.commit_batch(part))
        .expect("a part merged");
    reported
        .by(FlatStore::put(&deferred.aux(), b"flat aux", b"value", None))
        .expect("a put");
    reported
        .by(FlatStore::delete(&deferred.meta(), b"flat meta", None))
        .expect("a delete");
    read_everything(&deferred, &mut reported);
    total += reported.is_what_the_model_issued("staged writes and reads of an empty subtree");

    reported
        .by(storage.commit_multi_context_batch(batch.take_pending(), Some(&tx)))
        .expect("the batch applies");
    total += reported.is_what_the_model_issued("a batch applied");

    let unstaged =
        reported.by(storage.get_transactional_storage_context(SUBTREE.into(), None, &tx));
    read_everything(&unstaged, &mut reported);
    assert_eq!(walk(unstaged.raw_iter(), &mut reported), 3);
    assert_eq!(walk(unstaged.raw_iter_aux(), &mut reported), 2);
    assert_eq!(walk(unstaged.raw_iter_meta(), &mut reported), 1);
    assert_eq!(walk(FlatStore::cursor(&unstaged.aux()), &mut reported), 2);
    let refusals = [
        StorageContext::put(&unstaged, b"key", b"value", None, None),
        unstaged.put_aux(b"key", b"value", None),
        unstaged.put_root(b"key", b"value", None),
        unstaged.put_meta(b"key", b"value", None),
        StorageContext::delete(&unstaged, b"key", None),
        unstaged.delete_aux(b"key", None),
        unstaged.delete_root(b"key", None),
        unstaged.delete_meta(b"key", None),
        unstaged.commit_batch(unstaged.new_batch()),
        FlatStore::clear(&unstaged),
    ];
    for refused in refusals {
        assert!(matches!(reported.by(refused), Err(Error::StorageError(_))));
    }
    total += reported.is_what_the_model_issued("reads, cursors and refused writes");

    let batch = StorageBatch::new();
    let refusing =
        reported.by(storage.get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx));
    reported
        .by(refusing.put_meta(b"before", b"value", None))
        .expect("a put");
    reported
        .by(refusing.delete_meta(b"meta", None))
        .expect("a delete");
    reported
        .by(refusing.put_aux(b"refused", b"value", Some(mismatched())))
        .expect("a put");
    reported
        .by(StorageContext::put(
            &refusing, b"after", b"value", None, None,
        ))
        .expect("a put");
    assert!(matches!(
        reported.by(storage.commit_multi_context_batch(batch.take_pending(), Some(&tx))),
        Err(Error::CostError(_))
    ));
    total += reported.is_what_the_model_issued("a batch with a refused put");

    let batch = StorageBatch::new();
    let clearing =
        reported.by(storage.get_transactional_storage_context(SUBTREE.into(), Some(&batch), &tx));
    reported
        .by(FlatStore::clear(&clearing))
        .expect("a keyspace cleared");
    reported
        .by(FlatStore::clear(&clearing.aux()))
        .expect("a keyspace cleared");
    reported
        .by(FlatStore::clear(&clearing.meta()))
        .expect("a keyspace cleared");
    let (mut db_batch, mut pending) = reported
        .by(storage.build_write_batch(batch.take_pending()))
        .expect("the batch is built");
    reported
        .by(StorageContext::put(
            &clearing, b"later", b"value", None, None,
        ))
        .expect("a put");
    reported
        .by(clearing.delete_root(b"r", None))
        .expect("a delete");
    pending += reported
        .by(storage.continue_write_batch(&mut db_batch, batch.take_pending()))
        .expect("the batch is continued");
    reported
        .by(storage.commit_db_write_batch(db_batch, pending, Some(&tx)))
        .expect("the batch is written");
    total += reported.is_what_the_model_issued("keyspaces cleared, and a batch applied in parts");

    let read_only = storage.start_snapshot_read_transaction();
    let batch = StorageBatch::new();
    let refusing = reported.by(storage.get_transactional_storage_context(
        SUBTREE.into(),
        Some(&batch),
        &read_only,
    ));
    write_everything(&refusing, &mut reported);
    let (db_batch, pending) = reported
        .by(storage.build_write_batch(batch.take_pending()))
        .expect("the batch is built");
    assert!(reported
        .by(storage.commit_db_write_batch(db_batch, pending, Some(&read_only)))
        .is_err());
    write_everything(&refusing, &mut reported);
    assert!(matches!(
        reported.by(storage.commit_multi_context_batch(batch.take_pending(), Some(&read_only))),
        Err(Error::SnapshotReadOnlyTransaction(_))
    ));
    let refusing = reported.by(storage.get_immediate_storage_context(SUBTREE.into(), &read_only));
    let mut immediate_batch = refusing.new_batch();
    fill(&mut immediate_batch);
    assert!(reported.by(refusing.commit_batch(immediate_batch)).is_err());
    total += reported.is_what_the_model_issued("writes a read-only transaction refuses");

    let immediate = reported.by(storage.get_immediate_storage_context(SUBTREE.into(), &tx));
    reported.by(storage.get_immediate_storage_context_by_subtree_prefix(prefix, &tx));
    write_everything(&immediate, &mut reported);
    read_everything(&immediate, &mut reported);
    let mut immediate_batch = immediate.new_batch();
    fill(&mut immediate_batch);
    immediate_batch
        .put_aux(b"refused", b"value", Some(mismatched()))
        .expect_err("a cost that does not match");
    reported
        .by(immediate.commit_batch(immediate_batch))
        .expect("the batch is written");
    assert_eq!(walk(immediate.raw_iter(), &mut reported), 4);
    assert_eq!(walk(immediate.raw_iter_aux(), &mut reported), 1);
    assert_eq!(walk(immediate.raw_iter_meta(), &mut reported), 1);
    reported
        .by(FlatStore::clear(&immediate))
        .expect("a keyspace cleared");
    reported
        .by(FlatStore::clear(&immediate.meta()))
        .expect("a keyspace cleared");
    total += reported.is_what_the_model_issued("an immediate handle");

    reported
        .by(storage.commit_transaction(tx))
        .expect("the transaction commits");
    reported
        .by(storage.commit_multi_context_batch(StorageBatch::new(), None))
        .expect("an empty batch applies");
    total += reported.is_what_the_model_issued("a commit");

    assert!(total.seek_count > 150, "{total:?}");
    assert!(total.storage_loaded_bytes > 5_000, "{total:?}");
    assert!(total.hash_node_calls > 8, "{total:?}");
    assert!(total.storage_cost.added_bytes > 300, "{total:?}");
    assert!(total.storage_cost.removed_bytes.has_removal(), "{total:?}");
}

#[test]
fn a_cost_from_beside_the_model_does_not_balance() {
    let storage = TempStorage::new();
    let tx = storage.start_transaction();
    let mut reported = Reported::default();
    ledger::take();

    let context = reported.by(storage.get_immediate_storage_context(SUBTREE.into(), &tx));
    reported
        .by(StorageContext::get(&context, b"key"))
        .expect("a read");
    assert_eq!(reported.0, ledger::issued());

    reported.cost(OperationCost::with_seek_count(1));
    assert_ne!(reported.0, ledger::issued());

    let reported = Reported::default();
    ledger::take();
    drop(storage.get_transactional_storage_context(SUBTREE.into(), None, &tx));
    assert_ne!(reported.0, ledger::issued());
}
