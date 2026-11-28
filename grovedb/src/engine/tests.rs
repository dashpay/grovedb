//! Tests of the [`Engine`] seam on the RocksDB engine.
//!
//! Where a test compares the engine with the storage underneath, it runs the
//! same history on two databases: one through [`RocksDbEngine`], one with the
//! calls on `RocksDbStorage` that GroveDB made before the seam.

use grovedb_costs::{
    storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost},
    CostContext, CostsExt, OperationCost,
};
use grovedb_merk::{store::NodeStore, tree::kv::ValueDefinedCostType, tree_type::TreeType, Merk};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{
        pending_prefix_drops_namespace, stage_pending_prefix_drop, PendingPrefixDropRecord,
        PendingPrefixDrops, SubtreePrefix,
    },
    rocksdb_storage::RocksDbStorage,
    Error, FlatStore, RawIterator, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;
use tempfile::TempDir;

use super::{
    move_record, CommitError, Engine, EngineBatch, RocksDbEngine, SubtreeHandle, SubtreeMove,
    WriteGroup, WriteScopeError,
};
use crate::{tests::common::EMPTY_PATH, Element, GroveDb};

const A: &[&[u8]] = &[b"a"];
const B: &[&[u8]] = &[b"a", b"b"];

fn open_engine() -> (TempDir, RocksDbEngine) {
    let dir = TempDir::new().expect("temporary directory");
    let engine = RocksDbEngine::open(dir.path()).expect("open");
    (dir, engine)
}

fn prefix_of(path: &[&[u8]]) -> SubtreePrefix {
    RocksDbStorage::build_prefix(path.into()).unwrap()
}

/// Commits `records` to the data keyspace of the subtree at `path`.
fn commit_records<E: Engine>(engine: &E, path: &[&[u8]], records: &[(&[u8], &[u8])]) {
    let transaction = engine.start_transaction();
    let batch = E::Batch::default();
    let handle = engine
        .subtree(path.into(), Some(&batch), &transaction)
        .unwrap();
    for (key, value) in records {
        put(&handle, key, value);
    }
    drop(handle);
    engine
        .apply_batch(batch, &transaction)
        .unwrap()
        .expect("apply");
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("commit");
}

/// The value under `key` in the data keyspace of the subtree at `path`, read
/// with no transaction of the caller's.
fn committed<E: Engine>(engine: &E, path: &[&[u8]], key: &[u8]) -> Option<Vec<u8>> {
    let transaction = engine.start_transaction();
    let handle = engine.subtree(path.into(), None, &transaction).unwrap();
    get(&handle, key)
}

fn put(store: &impl FlatStore, key: &[u8], value: &[u8]) {
    FlatStore::put(store, key, value, None)
        .unwrap()
        .expect("put");
}

fn get(store: &impl FlatStore, key: &[u8]) -> Option<Vec<u8>> {
    FlatStore::get(store, key).unwrap().expect("get")
}

/// Every record a cursor shows from its first, as keys and values.
fn records_of<I: RawIterator>(mut cursor: I) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut records = Vec::new();
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        records.push((
            cursor.key().unwrap().expect("key").to_vec(),
            cursor.value().unwrap().expect("value").to_vec(),
        ));
        cursor.next().unwrap();
    }
    records
}

#[test]
fn a_handle_is_the_storage_context_of_its_subtree() {
    let (_dir, engine) = open_engine();
    let storage = engine.storage();
    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();

    // Opening by path charges the hashing of the prefix, by prefix nothing.
    let by_path = engine.subtree(B.into(), Some(&batch), &transaction);
    assert_eq!(by_path.cost, RocksDbStorage::build_prefix(B.into()).cost);
    assert_ne!(by_path.cost, OperationCost::default());
    let by_prefix = engine.subtree_by_prefix(prefix_of(B), Some(&batch), &transaction);
    assert_eq!(by_prefix.cost, OperationCost::default());
    let immediate = engine.immediate_subtree(B.into(), &transaction);
    assert_eq!(immediate.cost, by_path.cost);
    let immediate_by_prefix = engine.immediate_subtree_by_prefix(prefix_of(B), &transaction);
    assert_eq!(immediate_by_prefix.cost, OperationCost::default());

    // An immediate handle writes to the transaction at once, in the three
    // keyspaces of its subtree, and the storage contexts read it there.
    let immediate = immediate.unwrap();
    put(&immediate, b"k", b"data");
    put(&SubtreeHandle::aux(&immediate), b"k", b"aux");
    put(&SubtreeHandle::meta(&immediate), b"k", b"meta");
    let context = storage
        .get_transactional_storage_context(B.into(), None, &transaction)
        .unwrap();
    assert_eq!(
        StorageContext::get(&context, b"k").unwrap().unwrap(),
        Some(b"data".to_vec())
    );
    assert_eq!(
        context.get_aux(b"k").unwrap().unwrap(),
        Some(b"aux".to_vec())
    );
    assert_eq!(
        context.get_meta(b"k").unwrap().unwrap(),
        Some(b"meta".to_vec())
    );

    // A deferred handle reads them, by path and by prefix alike, and stages
    // its own writes in the batch, where no handle reads them.
    let by_path = by_path.unwrap();
    let by_prefix = by_prefix.unwrap();
    for handle in [&by_path, &by_prefix] {
        assert_eq!(get(handle, b"k"), Some(b"data".to_vec()));
        assert_eq!(
            get(&SubtreeHandle::aux(handle), b"k"),
            Some(b"aux".to_vec())
        );
        assert_eq!(
            get(&SubtreeHandle::meta(handle), b"k"),
            Some(b"meta".to_vec())
        );
    }
    put(&by_path, b"staged", b"value");
    assert_eq!(get(&by_prefix, b"staged"), None);
    assert_eq!(batch.len(), 1);

    // A deferred handle with no batch is read-only.
    let read_only = engine.subtree(B.into(), None, &transaction).unwrap();
    assert!(FlatStore::put(&read_only, b"k", b"v", None)
        .unwrap()
        .is_err());
}

/// The same staged writes on the engine's database and on a plain storage:
/// two puts, one of them with the cost the caller worked out, a delete of a
/// committed record with no cost given, and an aux and a meta record.
fn stage<'db, S: StorageContext<'db>>(context: &S) {
    context.put(b"new", b"value", None, None).unwrap().unwrap();
    context
        .put(
            b"billed",
            b"value",
            None,
            Some(landing_cost(b"billed", b"value")),
        )
        .unwrap()
        .unwrap();
    context.delete(b"old", None).unwrap().unwrap();
    context.put_aux(b"aux", b"value", None).unwrap().unwrap();
    context.put_meta(b"meta", b"value", None).unwrap().unwrap();
}

#[test]
fn a_batch_is_applied_with_the_calls_and_costs_of_a_multi_context_commit() {
    let (_engine_dir, engine) = open_engine();
    let (_plain_dir, plain) = open_engine();
    let storage = plain.storage();
    let old: &[(&[u8], &[u8])] = &[(b"old", b"committed value")];
    commit_records(&engine, A, old);
    commit_records(&plain, A, old);

    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    stage(
        &engine
            .subtree(A.into(), Some(&batch), &transaction)
            .unwrap(),
    );
    let applied = engine.apply_batch(batch, &transaction);

    let plain_transaction = storage.start_transaction();
    let batch = StorageBatch::new();
    stage(
        &storage
            .get_transactional_storage_context(A.into(), Some(&batch), &plain_transaction)
            .unwrap(),
    );
    let committed_batch = storage.commit_multi_context_batch(batch, Some(&plain_transaction));

    applied.value.as_ref().expect("apply");
    committed_batch.value.as_ref().expect("commit batch");
    assert_eq!(applied.cost, committed_batch.cost);
    // Five writes, and the delete read the committed record it removes.
    assert_eq!(applied.cost.seek_count, 6);
    assert_eq!(applied.cost.storage_loaded_bytes, 15);

    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    let context = storage
        .get_transactional_storage_context(A.into(), None, &plain_transaction)
        .unwrap();
    assert_eq!(
        records_of(NodeStore::cursor(&handle)),
        records_of(context.raw_iter())
    );
    assert_eq!(
        records_of(FlatStore::cursor(&handle)),
        [
            (b"billed".to_vec(), b"value".to_vec()),
            (b"new".to_vec(), b"value".to_vec())
        ]
    );
    assert_eq!(
        records_of(FlatStore::cursor(&SubtreeHandle::aux(&handle))),
        [(b"aux".to_vec(), b"value".to_vec())]
    );
    assert_eq!(
        records_of(FlatStore::cursor(&SubtreeHandle::meta(&handle))),
        [(b"meta".to_vec(), b"value".to_vec())]
    );
}

#[test]
fn a_batch_that_cannot_be_settled_is_not_applied() {
    let (_engine_dir, engine) = open_engine();
    let (_plain_dir, plain) = open_engine();
    let storage = plain.storage();
    // A bill that does not match the record it is for.
    let mismatched = || Some(KeyValueStorageCost::for_updated_root_cost(None, 5));

    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    let handle = engine
        .subtree(A.into(), Some(&batch), &transaction)
        .unwrap();
    FlatStore::put(&handle, b"k", b"value", mismatched())
        .unwrap()
        .expect("staged");
    drop(handle);
    let applied = engine.apply_batch(batch, &transaction);

    let plain_transaction = storage.start_transaction();
    let batch = StorageBatch::new();
    let context = storage
        .get_transactional_storage_context(A.into(), Some(&batch), &plain_transaction)
        .unwrap();
    StorageContext::put(&context, b"k", b"value", None, mismatched())
        .unwrap()
        .expect("staged");
    drop(context);
    let committed_batch = storage.commit_multi_context_batch(batch, Some(&plain_transaction));

    assert!(matches!(applied.value, Err(Error::CostError(_))));
    assert!(matches!(committed_batch.value, Err(Error::CostError(_))));
    // The seek of the put is charged though nothing was written.
    assert_eq!(applied.cost, committed_batch.cost);
    assert_eq!(applied.cost, OperationCost::with_seek_count(1));
    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    assert_eq!(records_of(FlatStore::cursor(&handle)), []);
}

#[test]
fn a_write_group_settles_what_is_staged_and_keeps_staging() {
    let (_engine_dir, engine) = open_engine();
    let (_plain_dir, plain) = open_engine();
    let storage = plain.storage();

    // Through the engine: settle the first part, read its cost, stage the
    // second part into the same batch, settle it, apply both.
    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    let handle = engine
        .subtree(A.into(), Some(&batch), &transaction)
        .unwrap();
    put(&handle, b"first", b"1");
    let mut group = <RocksDbEngine as Engine>::WriteGroup::default();
    assert_eq!(group.pending_cost(), &OperationCost::default());
    let first = engine.settle_batch(&mut group, batch.take_pending());
    first.value.as_ref().expect("settle");
    let pending_after_first = group.pending_cost().clone();
    assert!(batch.is_empty());
    assert_eq!(get(&handle, b"first"), None);
    put(&handle, b"second", b"22");
    FlatStore::delete(&handle, b"first", None).unwrap().unwrap();
    drop(handle);
    let second = engine.settle_batch(&mut group, batch);
    second.value.as_ref().expect("settle");
    let pending = group.pending_cost().clone();
    let applied = engine.apply_write_group(group, &transaction);
    applied.value.as_ref().expect("apply");

    // On the storage: the calls of the partial batch before the seam.
    let plain_transaction = storage.start_transaction();
    let batch = StorageBatch::new();
    let context = storage
        .get_transactional_storage_context(A.into(), Some(&batch), &plain_transaction)
        .unwrap();
    StorageContext::put(&context, b"first", b"1", None, None)
        .unwrap()
        .unwrap();
    let built = storage.build_write_batch(batch.take_pending());
    let (mut write_batch, mut pending_costs) = built.value.expect("build");
    StorageContext::put(&context, b"second", b"22", None, None)
        .unwrap()
        .unwrap();
    StorageContext::delete(&context, b"first", None)
        .unwrap()
        .unwrap();
    drop(context);
    let continued = storage.continue_write_batch(&mut write_batch, batch);
    let continued_pending = continued.value.expect("continue");
    assert_eq!(first.cost, built.cost);
    assert_eq!(pending_after_first, pending_costs);
    pending_costs += continued_pending;
    assert_eq!(second.cost, continued.cost);
    assert_eq!(pending, pending_costs);
    let written =
        storage.commit_db_write_batch(write_batch, pending_costs, Some(&plain_transaction));
    written.value.as_ref().expect("write");
    assert_eq!(applied.cost, written.cost);
    assert_eq!(applied.cost, pending);
    assert_ne!(pending, OperationCost::default());

    // The parts were applied in the order they were settled: the delete of
    // the second part removed the put of the first.
    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    assert_eq!(
        records_of(FlatStore::cursor(&handle)),
        [(b"second".to_vec(), b"22".to_vec())]
    );
}

#[test]
fn a_write_transaction_reads_its_own_writes_and_the_latest_committed_state() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k", b"committed")]);

    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    let handle = engine
        .subtree(A.into(), Some(&batch), &transaction)
        .unwrap();
    put(&handle, b"own", b"written");
    drop(handle);
    engine.apply_batch(batch, &transaction).unwrap().unwrap();

    // A commit made outside the transaction while it is open.
    commit_records(&engine, A, &[(b"k", b"changed"), (b"other", b"new")]);

    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    let read = |key: &[u8]| get(&handle, key);
    assert_eq!(read(b"own"), Some(b"written".to_vec()));
    assert_eq!(read(b"k"), Some(b"changed".to_vec()));
    assert_eq!(read(b"other"), Some(b"new".to_vec()));
    assert_eq!(committed(&engine, A, b"own"), None);
    drop(handle);

    // A rollback discards the writes and leaves the transaction usable.
    engine.rollback_transaction(&transaction).expect("rollback");
    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    assert_eq!(get(&handle, b"own"), None);
    drop(handle);
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("commit");
    assert_eq!(committed(&engine, A, b"own"), None);
}

#[test]
fn a_read_transaction_of_the_rocksdb_engine_reads_the_latest_committed_state() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k", b"first")]);
    let transaction = engine.start_read_transaction();
    let read = || get(&engine.subtree(A.into(), None, &transaction).unwrap(), b"k");
    assert_eq!(read(), Some(b"first".to_vec()));
    // RocksDB does not pin it: a commit that lands meanwhile is read.
    commit_records(&engine, A, &[(b"k", b"second")]);
    assert_eq!(read(), Some(b"second".to_vec()));
}

#[test]
fn a_snapshot_read_transaction_is_pinned_and_refuses_writes() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k", b"before")]);

    let snapshot = engine.start_snapshot_read_transaction();
    commit_records(&engine, A, &[(b"k", b"after")]);
    let handle = engine.subtree(A.into(), None, &snapshot).unwrap();
    assert_eq!(get(&handle, b"k"), Some(b"before".to_vec()));
    drop(handle);
    assert_eq!(committed(&engine, A, b"k"), Some(b"after".to_vec()));

    let read_only = |result: Result<(), Error>| {
        assert!(matches!(result, Err(Error::SnapshotReadOnlyTransaction(_))))
    };
    let immediate = engine.immediate_subtree(A.into(), &snapshot).unwrap();
    read_only(FlatStore::put(&immediate, b"k", b"v", None).unwrap());
    drop(immediate);
    let batch = StorageBatch::new();
    let handle = engine.subtree(A.into(), Some(&batch), &snapshot).unwrap();
    put(&handle, b"k", b"v");
    drop(handle);
    read_only(engine.apply_batch(batch, &snapshot).unwrap());
    read_only(engine.open_write_scope(&snapshot).map(drop));
    // Rolling back is allowed and changes nothing.
    engine.rollback_transaction(&snapshot).expect("rollback");
    read_only(engine.commit_transaction(snapshot).unwrap());
}

#[test]
fn savepoints_form_a_stack() {
    let (_dir, engine) = open_engine();
    let transaction = engine.start_transaction();
    let write = |key: &[u8]| {
        let handle = engine.immediate_subtree(A.into(), &transaction).unwrap();
        put(&handle, key, b"v");
    };
    let keys = || {
        let handle = engine.subtree(A.into(), None, &transaction).unwrap();
        records_of(FlatStore::cursor(&handle))
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>()
    };

    write(b"1");
    engine.set_savepoint(&transaction).expect("savepoint");
    write(b"2");
    engine.set_savepoint(&transaction).expect("savepoint");
    write(b"3");
    assert_eq!(keys(), [b"1", b"2", b"3"]);
    engine
        .rollback_to_savepoint(&transaction)
        .expect("rollback to the newest savepoint");
    assert_eq!(keys(), [b"1", b"2"]);
    engine
        .rollback_to_savepoint(&transaction)
        .expect("rollback to the savepoint before it");
    assert_eq!(keys(), [b"1"]);

    // With no savepoint left there is nothing to roll back to. That is an
    // error of the engine and not a commit outcome.
    let error = engine
        .rollback_to_savepoint(&transaction)
        .expect_err("no savepoint is left");
    assert_eq!(error.commit_error(), None);
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("commit");
    assert_eq!(committed(&engine, A, b"1"), Some(b"v".to_vec()));
    assert_eq!(committed(&engine, A, b"2"), None);
}

#[cfg(not(lignin_cost_model))]
#[test]
fn a_commit_that_loses_is_the_error_rocksdb_returned_and_is_classed_as_a_conflict() {
    use grovedb_storage::rocksdb_storage::ErrorKind;

    let (_dir, engine) = open_engine();
    let write =
        |transaction: &<RocksDbEngine as Engine>::Transaction<'_>, path: &[&[u8]], value: &[u8]| {
            let batch = StorageBatch::new();
            let handle = engine
                .subtree(path.into(), Some(&batch), transaction)
                .unwrap();
            put(&handle, b"k", value);
            drop(handle);
            engine.apply_batch(batch, transaction).unwrap().unwrap();
        };

    // The transaction writes a key, and another commit then writes it too.
    let loser = engine.start_transaction();
    write(&loser, A, b"first");
    commit_records(&engine, A, &[(b"k", b"second")]);
    let error = engine
        .commit_transaction(loser)
        .unwrap()
        .expect_err("the commit loses");
    assert!(
        matches!(&error, Error::RocksDBError(e) if e.kind() == ErrorKind::Busy),
        "the RocksDB engine returns what it returned before the seam: {error:?}"
    );
    assert_eq!(error.commit_error(), Some(CommitError::Conflict));
    assert!(error.is_commit_conflict());
    // GroveDB hands the error on unchanged, and classes it the same way.
    let error = crate::Error::from(error);
    assert!(matches!(
        &error,
        crate::Error::StorageError(Error::RocksDBError(e)) if e.kind() == ErrorKind::Busy
    ));
    assert_eq!(error.commit_error(), Some(CommitError::Conflict));
    assert!(error.is_commit_conflict());
    assert_eq!(committed(&engine, A, b"k"), Some(b"second".to_vec()));

    // Two transactions whose keys do not meet both commit, and so does one
    // that first writes its key after the other commit landed.
    let first = engine.start_transaction();
    write(&first, A, b"third");
    let second = engine.start_transaction();
    write(&second, B, b"elsewhere");
    engine
        .commit_transaction(second)
        .unwrap()
        .expect("disjoint keys");
    let late = engine.start_transaction();
    engine
        .commit_transaction(first)
        .unwrap()
        .expect("disjoint keys");
    write(&late, A, b"fourth");
    engine
        .commit_transaction(late)
        .unwrap()
        .expect("written after the other commit");
    assert_eq!(committed(&engine, A, b"k"), Some(b"fourth".to_vec()));
}

#[test]
fn an_error_that_is_not_a_commit_outcome_has_no_class_in_grovedb_either() {
    for error in [
        crate::Error::StorageError(Error::StorageError("disk".to_owned())),
        crate::Error::CorruptedData("data".to_owned()),
    ] {
        assert_eq!(error.commit_error(), None);
        assert!(!error.is_commit_conflict());
    }
    for outcome in [CommitError::NotDurable, CommitError::Unsettled] {
        let error = crate::Error::StorageError(Error::Commit(outcome));
        assert_eq!(error.commit_error(), Some(outcome));
        assert!(!error.is_commit_conflict());
    }
}

#[test]
fn the_rocksdb_engine_allows_one_write_scope_at_a_time() {
    let (_dir, engine) = open_engine();
    let transaction = engine.start_transaction();
    let other = engine.start_transaction();
    let overlap = |result: Result<_, Error>| {
        assert!(matches!(
            result.map(drop),
            Err(Error::WriteScope(WriteScopeError::Overlap))
        ))
    };
    let scopes_open = |result: Result<(), Error>| {
        assert!(matches!(
            result,
            Err(Error::WriteScope(WriteScopeError::ScopesOpen))
        ))
    };

    let scope = engine.open_write_scope(&transaction).expect("first scope");
    overlap(engine.open_write_scope(&transaction));
    overlap(engine.open_write_scope(&other));
    scopes_open(engine.set_savepoint(&transaction));
    scopes_open(engine.rollback_to_savepoint(&transaction));
    // The savepoints of another transaction are its own.
    engine.set_savepoint(&other).expect("savepoint");
    engine.rollback_to_savepoint(&other).expect("rollback");

    // What takes a transaction takes the scope's view of it, and the
    // scope's writes are the transaction's.
    let view = RocksDbEngine::scope_transaction(&scope);
    let handle = engine.immediate_subtree(A.into(), view).unwrap();
    put(&handle, b"k", b"scoped");
    drop(handle);
    engine.merge_write_scope(scope).expect("merge");
    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    assert_eq!(get(&handle, b"k"), Some(b"scoped".to_vec()));
    drop(handle);

    // With the scope closed, savepoints work again and another scope opens.
    engine.set_savepoint(&transaction).expect("savepoint");
    let write_in_a_scope_of_other = |key: &[u8]| {
        let scope = engine.open_write_scope(&other).expect("next scope");
        let view = RocksDbEngine::scope_transaction(&scope);
        put(
            &engine.immediate_subtree(A.into(), view).unwrap(),
            key,
            b"v",
        );
        scope
    };
    let other_reads = |key: &[u8]| get(&engine.subtree(A.into(), None, &other).unwrap(), key);

    // This engine cannot undo a scope on its own. A rollback is refused, a
    // scope that is dropped is closed, and either way what the scope wrote
    // stays in its transaction.
    assert!(matches!(
        engine.rollback_write_scope(write_in_a_scope_of_other(b"refused")),
        Err(Error::WriteScope(WriteScopeError::RollbackUnsupported))
    ));
    assert_eq!(other_reads(b"refused"), Some(b"v".to_vec()));
    // The savepoint a caller set before the scope is what undoes it.
    engine.set_savepoint(&other).expect("savepoint");
    drop(write_in_a_scope_of_other(b"dropped"));
    assert_eq!(other_reads(b"dropped"), Some(b"v".to_vec()));
    engine.rollback_to_savepoint(&other).expect("rollback");
    assert_eq!(other_reads(b"dropped"), None);
    assert_eq!(other_reads(b"refused"), Some(b"v".to_vec()));

    let scope = engine.open_write_scope(&transaction).expect("scope");
    engine.merge_write_scope(scope).expect("merge");
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("commit");
    assert_eq!(committed(&engine, A, b"k"), Some(b"scoped".to_vec()));
}

#[test]
fn a_retargeted_cursor_keeps_the_state_it_was_opened_on() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"a1", b"1")]);
    commit_records(&engine, B, &[(b"b1", b"1")]);

    let transaction = engine.start_transaction();
    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    let cursor = NodeStore::cursor(&handle);
    // A commit lands after the cursor was opened.
    commit_records(&engine, B, &[(b"b2", b"2")]);

    let mut cursor = RocksDbEngine::retarget_cursor(cursor, prefix_of(B));
    let retargeted = cursor.seek_to_first();
    assert_eq!(retargeted.cost, OperationCost::with_seek_count(1));
    assert_eq!(cursor.key().unwrap(), Some(b"b1".as_slice()));
    cursor.next().unwrap();
    assert!(!cursor.valid().unwrap());

    // The next cursor sees the commit.
    let handle = engine.subtree(B.into(), None, &transaction).unwrap();
    assert_eq!(records_of(NodeStore::cursor(&handle)).len(), 2);
    // And the cursor goes back to where it came from.
    let cursor = RocksDbEngine::retarget_cursor(cursor, prefix_of(A));
    assert_eq!(records_of(cursor), [(b"a1".to_vec(), b"1".to_vec())]);
}

/// The cost of a record that replaces one of its own size, at its stored
/// size: its key behind the 32 bytes of the subtree prefix.
fn landing_cost(key: &[u8], value: &[u8]) -> KeyValueStorageCost {
    KeyValueStorageCost {
        key_storage_cost: StorageCost {
            replaced_bytes: (32 + key.len()) as u32 + 1,
            ..Default::default()
        },
        value_storage_cost: StorageCost {
            replaced_bytes: value.len() as u32 + 1,
            ..Default::default()
        },
        new_node: false,
        needs_value_verification: true,
        prepaid: false,
    }
}

#[test]
fn a_subtree_moves_record_by_record_with_the_calls_of_a_copy() {
    let (_engine_dir, engine) = open_engine();
    let (_plain_dir, plain) = open_engine();
    let storage = plain.storage();
    let records: &[(&[u8], &[u8])] = &[(b"k1", b"one"), (b"k2", b"two"), (b"k3", b"three")];
    commit_records(&engine, A, records);
    commit_records(&plain, A, records);
    let (from, to) = (prefix_of(A), prefix_of(B));

    let mut cost = OperationCost::default();
    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    let mut subtree_move = engine
        .move_subtree(from, to, &batch, &transaction)
        .unwrap_add_cost(&mut cost);
    let mut seen = Vec::new();
    while let Some(key) = subtree_move
        .move_next(|key, _| Ok(key.to_vec()), landing_cost)
        .unwrap_add_cost(&mut cost)
        .expect("move")
    {
        seen.push(key);
    }
    drop(subtree_move);
    engine
        .apply_batch(batch, &transaction)
        .unwrap_add_cost(&mut cost)
        .expect("apply");
    assert_eq!(seen, [b"k1", b"k2", b"k3"]);

    // The copy as GroveDB made it before the seam.
    let mut plain_cost = OperationCost::default();
    let plain_transaction = storage.start_transaction();
    let batch = StorageBatch::new();
    let source = storage
        .get_transactional_storage_context_by_subtree_prefix(from, Some(&batch), &plain_transaction)
        .unwrap_add_cost(&mut plain_cost);
    let destination = storage
        .get_transactional_storage_context_by_subtree_prefix(to, Some(&batch), &plain_transaction)
        .unwrap_add_cost(&mut plain_cost);
    let mut iter = source.raw_iter();
    iter.seek_to_first().unwrap_add_cost(&mut plain_cost);
    while iter.valid().unwrap_add_cost(&mut plain_cost) {
        let key = iter
            .key()
            .unwrap_add_cost(&mut plain_cost)
            .expect("key")
            .to_vec();
        let value = iter
            .value()
            .unwrap_add_cost(&mut plain_cost)
            .expect("value")
            .to_vec();
        StorageContext::put(
            &destination,
            &key,
            &value,
            None,
            Some(landing_cost(&key, &value)),
        )
        .unwrap_add_cost(&mut plain_cost)
        .expect("put");
        StorageContext::delete(&source, &key, Some(KeyValueStorageCost::default()))
            .unwrap_add_cost(&mut plain_cost)
            .expect("delete");
        iter.next().unwrap_add_cost(&mut plain_cost);
    }
    drop((iter, source, destination));
    storage
        .commit_multi_context_batch(batch, Some(&plain_transaction))
        .unwrap_add_cost(&mut plain_cost)
        .expect("commit batch");
    assert_eq!(cost, plain_cost);
    assert_ne!(cost.storage_cost.replaced_bytes, 0);

    let moved = engine.subtree(B.into(), None, &transaction).unwrap();
    let expected: Vec<_> = records
        .iter()
        .map(|(key, value)| (key.to_vec(), value.to_vec()))
        .collect();
    assert_eq!(records_of(FlatStore::cursor(&moved)), expected);
    let left = engine.subtree(A.into(), None, &transaction).unwrap();
    assert_eq!(records_of(FlatStore::cursor(&left)), []);
}

#[test]
fn a_record_its_inspection_refuses_stays_where_it_is() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k1", b"one")]);
    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    let mut subtree_move = engine
        .move_subtree(prefix_of(A), prefix_of(B), &batch, &transaction)
        .unwrap();
    let refused = subtree_move
        .move_next(
            |_, _| Err::<(), _>(crate::Error::InvalidInput("refused")),
            landing_cost,
        )
        .unwrap();
    assert!(matches!(
        refused,
        Err(crate::Error::InvalidInput("refused"))
    ));
    drop(subtree_move);
    assert!(batch.is_empty());
}

/// A cursor over in-memory records that can stop on an error once they run
/// out, or claim a record it does not hand over.
struct ScriptedCursor {
    records: Vec<(Vec<u8>, Vec<u8>)>,
    position: usize,
    fails_after_last: bool,
    withholds_records: bool,
}

impl RawIterator for ScriptedCursor {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.position = 0;
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        unimplemented!("a move only steps forward")
    }

    fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        unimplemented!("a move only steps forward")
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
        unimplemented!("a move only steps forward")
    }

    fn next(&mut self) -> CostContext<()> {
        self.position += 1;
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn prev(&mut self) -> CostContext<()> {
        unimplemented!("a move only steps forward")
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        let record = (!self.withholds_records)
            .then(|| self.records.get(self.position))
            .flatten();
        record
            .map(|(_, value)| value.as_slice())
            .wrap_with_cost(OperationCost::default())
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let record = (!self.withholds_records)
            .then(|| self.records.get(self.position))
            .flatten();
        record
            .map(|(key, _)| key.as_slice())
            .wrap_with_cost(OperationCost::default())
    }

    fn valid(&self) -> CostContext<bool> {
        (self.position < self.records.len()).wrap_with_cost(OperationCost::default())
    }

    fn status(&self) -> Result<(), Error> {
        if self.fails_after_last && self.position >= self.records.len() {
            Err(Error::StorageError("the cursor failed".to_owned()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn a_move_fails_when_its_cursor_stopped_on_an_error_or_withholds_a_record() {
    let records = vec![(b"k1".to_vec(), b"one".to_vec())];
    let landed = std::cell::RefCell::new(Vec::new());
    let cleared = std::cell::RefCell::new(Vec::new());
    let step = |cursor: &mut ScriptedCursor| {
        move_record(
            cursor,
            |key, _| Ok(key.to_vec()),
            |key, value| {
                landed.borrow_mut().push((key.to_vec(), value.to_vec()));
                Ok(()).wrap_with_cost(OperationCost::with_seek_count(10))
            },
            |key| {
                cleared.borrow_mut().push(key.to_vec());
                Ok(()).wrap_with_cost(OperationCost::with_seek_count(100))
            },
        )
    };

    // A record is inspected, landed and cleared, and the cursor steps on.
    let mut cursor = ScriptedCursor {
        records: records.clone(),
        position: 0,
        fails_after_last: true,
        withholds_records: false,
    };
    let moved = step(&mut cursor);
    assert_eq!(moved.cost, OperationCost::with_seek_count(111));
    assert_eq!(moved.value.expect("move"), Some(b"k1".to_vec()));
    assert_eq!(*landed.borrow(), records);
    assert_eq!(*cleared.borrow(), [b"k1".to_vec()]);
    // The end of the records is an error when the cursor says so.
    assert!(matches!(
        step(&mut cursor).unwrap(),
        Err(crate::Error::StorageError(Error::StorageError(_)))
    ));
    cursor.fails_after_last = false;
    assert_eq!(step(&mut cursor).unwrap().expect("the end"), None);

    // A cursor that stands on a record and hands none over is refused, and
    // nothing lands.
    let mut cursor = ScriptedCursor {
        records,
        position: 0,
        fails_after_last: false,
        withholds_records: true,
    };
    assert!(matches!(
        step(&mut cursor).unwrap(),
        Err(crate::Error::CorruptedData(_))
    ));
    assert_eq!(landed.borrow().len(), 1);

    // A landing or a clearing that fails ends the step with its error.
    cursor.withholds_records = false;
    let failed_landing = move_record(
        &mut cursor,
        |_, _| Ok(()),
        |_, _| Err(crate::Error::InvalidInput("landing")).wrap_with_cost(Default::default()),
        |_| unreachable!("a record that did not land is not cleared"),
    );
    assert!(matches!(
        failed_landing.unwrap(),
        Err(crate::Error::InvalidInput("landing"))
    ));
    let failed_clearing = move_record(
        &mut cursor,
        |_, _| Ok(()),
        |_, _| Ok(()).wrap_with_cost(Default::default()),
        |_| Err(crate::Error::InvalidInput("clearing")).wrap_with_cost(Default::default()),
    );
    assert!(matches!(
        failed_clearing.unwrap(),
        Err(crate::Error::InvalidInput("clearing"))
    ));
}

#[test]
fn a_checkpoint_opens_with_the_state_it_captured() {
    let (dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k", b"captured")]);
    let checkpoints = TempDir::new().expect("temporary directory");
    let path = checkpoints.path().join("checkpoint");
    engine.create_checkpoint(&path).expect("checkpoint");
    commit_records(&engine, A, &[(b"k", b"later")]);

    let checkpoint = RocksDbEngine::open_checkpoint(&path).expect("open the checkpoint");
    assert_eq!(committed(&checkpoint, A, b"k"), Some(b"captured".to_vec()));
    assert_eq!(committed(&engine, A, b"k"), Some(b"later".to_vec()));
    // An existing target is refused and left as it is.
    assert!(engine.create_checkpoint(&path).is_err());
    assert_eq!(committed(&checkpoint, A, b"k"), Some(b"captured".to_vec()));
    // A checkpoint is opened where one was made, and nowhere else.
    assert!(RocksDbEngine::open_checkpoint(checkpoints.path().join("none")).is_err());

    // What was committed and flushed is there when the database is opened
    // again.
    engine.flush().expect("flush");
    drop(engine);
    let engine = RocksDbEngine::open(dir.path()).expect("reopen");
    assert_eq!(committed(&engine, A, b"k"), Some(b"later".to_vec()));
    // An engine on a storage the caller opened.
    drop(engine);
    let storage = RocksDbStorage::default_rocksdb_with_path(dir.path()).expect("reopen");
    let engine = RocksDbEngine::new(storage);
    assert_eq!(committed(&engine, A, b"k"), Some(b"later".to_vec()));
}

/// The removal of a checkpoint that the trait provides, which is this
/// engine's: the checkpoint is opened first, so what does not open as one
/// stays, and a checkpoint goes with its directory.
#[test]
fn the_provided_removal_of_a_checkpoint_opens_it_first() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k", b"captured")]);
    let checkpoints = TempDir::new().expect("temporary directory");
    let path = checkpoints.path().join("checkpoint");
    engine.create_checkpoint(&path).expect("checkpoint");

    let other = checkpoints.path().join("other");
    std::fs::create_dir(&other).expect("a directory");
    std::fs::write(other.join("notes"), b"kept").expect("a file");
    let refused = RocksDbEngine::delete_checkpoint(&other).expect_err("no checkpoint");
    assert!(
        matches!(refused, crate::Error::StorageError(_)),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read(other.join("notes")).expect("the file"),
        b"kept"
    );
    let nothing = checkpoints.path().join("none");
    assert!(RocksDbEngine::delete_checkpoint(&nothing).is_err());

    RocksDbEngine::delete_checkpoint(&path).expect("the checkpoint goes");
    assert!(!path.exists());
    assert_eq!(committed(&engine, A, b"k"), Some(b"captured".to_vec()));

    // `GroveDb` asks the engine, after its own look at the path.
    engine.create_checkpoint(&path).expect("checkpoint");
    GroveDb::<RocksDbEngine>::delete_checkpoint_with_engine(&path).expect("it goes");
    assert!(!path.exists());
    let short = GroveDb::<RocksDbEngine>::delete_checkpoint_with_engine("/");
    assert!(matches!(short, Err(crate::Error::CorruptedData(_))));
}

#[test]
fn wipe_empties_every_keyspace() {
    let (_dir, engine) = open_engine();
    let transaction = engine.start_transaction();
    let handle = engine.immediate_subtree(A.into(), &transaction).unwrap();
    put(&handle, b"k", b"data");
    put(&SubtreeHandle::aux(&handle), b"k", b"aux");
    put(&SubtreeHandle::meta(&handle), b"k", b"meta");
    NodeStore::set_root_key(&handle, Some(b"k"))
        .unwrap()
        .expect("root key");
    drop(handle);
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("commit");

    engine.wipe().expect("wipe");
    let transaction = engine.start_transaction();
    let handle = engine.subtree(A.into(), None, &transaction).unwrap();
    assert_eq!(records_of(FlatStore::cursor(&handle)), []);
    assert_eq!(
        records_of(FlatStore::cursor(&SubtreeHandle::aux(&handle))),
        []
    );
    assert_eq!(
        records_of(FlatStore::cursor(&SubtreeHandle::meta(&handle))),
        []
    );
    assert_eq!(NodeStore::root_key(&handle).unwrap().unwrap(), None);
}

#[test]
fn pending_prefix_drops_are_the_storages() {
    let (_dir, engine) = open_engine();
    commit_records(&engine, A, &[(b"k", b"doomed")]);
    let record = PendingPrefixDropRecord {
        primary_prefix: prefix_of(A),
        path: vec![b"a".to_vec()],
        doomed_prefixes: vec![prefix_of(A)],
    };
    let transaction = engine.start_transaction();
    let batch = StorageBatch::new();
    let namespace = engine
        .subtree_by_prefix(
            *pending_prefix_drops_namespace(),
            Some(&batch),
            &transaction,
        )
        .unwrap();
    stage_pending_prefix_drop(&SubtreeHandle::meta(&namespace), &record)
        .unwrap()
        .expect("stage");
    drop(namespace);
    engine.apply_batch(batch, &transaction).unwrap().unwrap();
    // A record of a transaction that has not committed is not listed.
    assert_eq!(engine.pending_prefix_drop_records().expect("list"), []);
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("commit");

    assert_eq!(
        engine.pending_prefix_drop_records().expect("list"),
        std::slice::from_ref(&record)
    );
    engine
        .delete_prefix_ranges(&record.doomed_prefixes)
        .expect("delete the ranges");
    assert_eq!(committed(&engine, A, b"k"), None);
    engine
        .remove_pending_prefix_drop_record(&record.primary_prefix)
        .expect("remove the record");
    assert_eq!(engine.pending_prefix_drop_records().expect("list"), []);
}

#[test]
fn a_storage_batch_is_an_engine_batch() {
    fn check<B: EngineBatch>(
        put: impl Fn(&B, &[u8]),
        delete: impl Fn(&B, &[u8]),
        keys: impl Fn(B) -> Vec<Vec<u8>>,
    ) {
        // Inside one operation a put wins over a delete of the same key.
        let batch = B::default();
        put(&batch, b"k");
        delete(&batch, b"k");
        // Across the boundary the later operation wins.
        put(&batch, b"gone");
        batch.next_operation();
        delete(&batch, b"gone");
        let taken = batch.take_pending();
        assert_eq!(keys(batch.take_pending()), Vec::<Vec<u8>>::new());

        // A merged batch is a later operation, whose outcomes replace what
        // the batch holds, and whatever follows it is later still.
        put(&batch, b"replaced");
        let other = B::default();
        delete(&other, b"replaced");
        put(&other, b"merged");
        batch.merge_overwriting(other);
        delete(&batch, b"merged");
        assert_eq!(keys(batch), Vec::<Vec<u8>>::new());
        assert_eq!(keys(taken), [b"k".to_vec()]);
    }

    let (_dir, engine) = open_engine();
    let transaction = engine.start_transaction();
    let write = |batch: &StorageBatch, key: &[u8], is_put: bool| {
        let handle = engine.subtree(A.into(), Some(batch), &transaction).unwrap();
        if is_put {
            put(&handle, key, b"v");
        } else {
            FlatStore::delete(&handle, key, None).unwrap().unwrap();
        }
    };
    check::<StorageBatch>(
        |batch, key| write(batch, key, true),
        |batch, key| write(batch, key, false),
        |batch| {
            let transaction = engine.start_transaction();
            engine.apply_batch(batch, &transaction).unwrap().unwrap();
            let handle = engine.subtree(A.into(), None, &transaction).unwrap();
            records_of(FlatStore::cursor(&handle))
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        },
    );
}

/// What GroveDB does with a handle, written for any engine: open a Merk on
/// it, write through the batch, and read the root back in the transaction.
fn merk_root_key_after_a_put<E: Engine>(engine: &E) -> Option<Vec<u8>> {
    use grovedb_merk::{tree::Op, TreeFeatureType::BasicMerkNode};

    let grove_version = GroveVersion::latest();
    let no_cost = None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
    let transaction = engine.start_transaction();
    let batch = E::Batch::default();
    let handle = engine
        .subtree(SubtreePath::empty(), Some(&batch), &transaction)
        .unwrap();
    let mut merk = Merk::open_base(handle, TreeType::NormalTree, no_cost, grove_version)
        .unwrap()
        .expect("open");
    merk.apply::<_, Vec<u8>>(
        &[(b"key".to_vec(), Op::Put(b"value".to_vec(), BasicMerkNode))],
        &[],
        None,
        grove_version,
    )
    .unwrap()
    .expect("apply");
    drop(merk);
    engine
        .apply_batch(batch, &transaction)
        .unwrap()
        .expect("apply the batch");
    let handle = engine
        .subtree(SubtreePath::empty(), None, &transaction)
        .unwrap();
    assert!(handle.has_node(b"key").unwrap().expect("has node"));
    assert!(!handle.has_node(b"none").unwrap().expect("has node"));
    let merk = Merk::open_base(handle, TreeType::NormalTree, no_cost, grove_version)
        .unwrap()
        .expect("open");
    merk.root_key()
}

#[test]
fn a_merk_runs_on_a_subtree_handle_of_any_engine() {
    let (_dir, engine) = open_engine();
    assert_eq!(merk_root_key_after_a_put(&engine), Some(b"key".to_vec()));
}

#[test]
fn the_engine_and_the_database_are_shared_between_threads() {
    fn shared<T: Send + Sync + 'static>() {}
    shared::<RocksDbEngine>();
    shared::<GroveDb>();
    shared::<GroveDb<RocksDbEngine>>();
    // A RocksDB transaction may move to another thread.
    fn sent<T: Send>() {}
    sent::<<RocksDbEngine as Engine>::Transaction<'static>>();
}

#[test]
fn a_node_store_error_keeps_a_storage_error_as_one() {
    let failed = grovedb_merk::Error::StorageError(Error::StorageError("failed".to_owned()));
    assert!(matches!(
        super::node_store_error(failed),
        crate::Error::StorageError(Error::StorageError(message)) if message == "failed"
    ));
    let absent = grovedb_merk::Error::KeyNotFoundError("absent");
    assert!(matches!(
        super::node_store_error(absent),
        crate::Error::MerkError(grovedb_merk::Error::KeyNotFoundError("absent"))
    ));
}

#[test]
fn a_grovedb_is_opened_on_an_engine_and_sets_savepoints_through_it() {
    let grove_version = GroveVersion::latest();
    let dir = TempDir::new().expect("temporary directory");
    let tree: &[&[u8]] = &[b"tree"];
    let item = |value: &[u8]| Element::new_item(value.to_vec());
    let db = GroveDb::<RocksDbEngine>::open_with_engine(dir.path()).expect("open");
    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the tree");

    let transaction = db.start_transaction();
    let insert = |key: &[u8]| {
        db.insert(
            tree,
            key,
            item(key),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("insert")
    };
    insert(b"kept");
    db.set_savepoint(&transaction).expect("savepoint");
    insert(b"undone");
    db.rollback_to_savepoint(&transaction).expect("rollback");
    // Neither is allowed while a write scope of the transaction is open.
    let scope = db.engine().open_write_scope(&transaction).expect("scope");
    for refused in [
        db.set_savepoint(&transaction),
        db.rollback_to_savepoint(&transaction),
    ] {
        assert!(matches!(
            refused,
            Err(crate::Error::StorageError(Error::WriteScope(
                WriteScopeError::ScopesOpen
            )))
        ));
    }
    db.engine().merge_write_scope(scope).expect("merge");
    db.commit_transaction(transaction).unwrap().expect("commit");
    drop(db);

    // The same database on an engine the caller opened.
    let db = GroveDb::from_engine(RocksDbEngine::open(dir.path()).expect("reopen"));
    let stored = |key: &[u8]| {
        db.get_raw_optional(tree.into(), key, None, grove_version)
            .unwrap()
            .expect("get")
    };
    assert_eq!(stored(b"kept"), Some(item(b"kept")));
    assert_eq!(stored(b"undone"), None);
}
