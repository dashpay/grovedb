//! The views a handle is opened on, on a database of the engine's facade
//! (`lignindb::Db`): a write transaction that has no writer of its own, a
//! snapshot, and the flat keyspaces of both. And a batch that two
//! operations write one Merk into.

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
use grovedb_storage::FlatStore;
use lignindb::{
    commit::{Committed, NoStaleHashes},
    Db as Database, Options,
};

use super::{super::LigninSnapshot, *};

pub(super) struct Facade {
    pub(super) db: Database,
    _dir: tempfile::TempDir,
}

impl Facade {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Database::open(dir.path(), &Options::default()).expect("a database");
        Self { _dir: dir, db }
    }

    pub(super) fn commit(&self, transaction: &WriteTxn<'_>) {
        let committed = self.db.commit(transaction, &mut NoStaleHashes);
        assert!(
            matches!(committed, Ok(Committed::Published(_))),
            "{committed:?}"
        );
    }

    /// The physical checker finds the database sound, with every hash
    /// made again: a value hash as the hash of the value's bytes.
    pub(super) fn sound(&self) {
        let mut rules = RawValues {
            rules: MerkHashRules::for_check(version()),
            stated: false,
        };
        let report = self.db.verify(Some(&mut rules));
        assert!(report.is_sound(), "{:#?}", report.findings);
    }
}

/// A key of these tests: one for each number.
fn plain(k: u64) -> Vec<u8> {
    k.to_be_bytes().to_vec()
}

fn puts(keys: std::ops::Range<u64>, salt: u64) -> Vec<(Vec<u8>, Op)> {
    let put = |k: u64| {
        let value = value(k * 1_000 + salt, true);
        (plain(k), Op::Put(value, TreeFeatureType::BasicMerkNode))
    };
    keys.map(put).collect()
}

fn deletes(keys: std::ops::Range<u64>) -> Vec<(Vec<u8>, Op)> {
    keys.map(|k| (plain(k), Op::Delete)).collect()
}

fn rocks() -> Rocks {
    Rocks {
        storage: TempStorage::new(),
        tree_type: TreeType::NormalTree,
    }
}

fn storage_error(error: Error) -> grovedb_storage::Error {
    match error {
        Error::StorageError(error) => error,
        other => panic!("no storage error: {other}"),
    }
}

#[test]
fn a_transaction_with_no_writer_of_its_own_writes_through_a_scope_for_each_write() {
    let facade = Facade::new();
    let rocks = rocks();
    let transaction = facade.db.begin();

    // A deferred handle and its batch, as on RocksDB: the same costs, the
    // same root.
    let first = puts(1..40, 1);
    let batch = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&transaction, ROOT, Some(&batch));
    handle
        .enter(TreeType::NormalTree)
        .expect("the root is entered");
    let mut merk = open(handle, TreeType::NormalTree);
    let merk_cost = apply(&mut merk, &first);
    let root = (merk.root_hash().unwrap(), merk.root_key());
    drop(merk);
    let applied = batch.apply(&transaction);
    let (expected_merk, expected_batch, hash, root_key) = rocks.apply(&first);
    assert_eq!((merk_cost, &applied.cost), (expected_merk, &expected_batch));
    assert_eq!(root, (hash, root_key));
    let applied = applied.value.expect("the batch is applied");
    assert_eq!((applied.values_written, applied.values_kept), (39, 0));
    // No scope of the transaction is open between two writes: a savepoint
    // is taken.
    assert!(!transaction.scopes_open());
    transaction.set_savepoint().expect("a savepoint");

    // An immediate handle and a Merk that stays open: each commit is a
    // write of its own, read by every handle of the transaction.
    let mut merk = open(
        LigninNodeStore::immediate(&transaction, ROOT),
        TreeType::NormalTree,
    );
    assert_eq!(merk.root_hash().unwrap(), root.0);
    apply(&mut merk, &puts(40..55, 2));
    apply(&mut merk, &deletes(3..9));
    assert!(!transaction.scopes_open());
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    assert_eq!(reader.root_key().unwrap().expect("a read"), merk.root_key());
    assert!(reader.has_node(&plain(50)).unwrap().expect("a read"));
    assert!(!reader.has_node(&plain(5)).unwrap().expect("a read"));
    drop(merk);

    // A write that the transaction refuses leaves nothing of itself, and
    // no scope open: a change set for a subtree that is not entered.
    let nowhere = LigninSubtree::alone([9; 32]);
    let batch = LigninBatch::new();
    let mut merk = open(
        LigninNodeStore::deferred(&transaction, nowhere.clone(), Some(&batch)),
        TreeType::NormalTree,
    );
    apply(&mut merk, &puts(1..4, 3));
    drop(merk);
    assert!(batch.apply(&transaction).unwrap().is_err());
    let refused = LigninNodeStore::immediate(&transaction, nowhere);
    assert!(refused.put_meta_record(b"k", b"v").unwrap().is_err());
    assert!(!transaction.scopes_open());

    // The savepoint takes the writes of the immediate handle back.
    transaction.rollback_to_savepoint().expect("a rollback");
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    assert_eq!(reader.root_key().unwrap().expect("a read"), root.1);
    assert!(reader.has_node(&plain(5)).unwrap().expect("a read"));
    let stored = records(&reader, root.1.clone());
    assert_same_records(&stored, &rocks.records(root.1), "after the rollback");
    drop(reader);
    facade.commit(&transaction);
    facade.sound();
}

#[test]
fn a_snapshot_reads_the_merk_of_its_version_and_refuses_every_write() {
    let facade = Facade::new();
    let rocks = rocks();
    let write = |batch: &[(Vec<u8>, Op)], enter: bool| {
        let transaction = facade.db.begin();
        let staged = LigninBatch::new();
        let handle = LigninNodeStore::deferred(&transaction, ROOT, Some(&staged));
        if enter {
            handle
                .enter(TreeType::NormalTree)
                .expect("the root is entered");
        }
        handle
            .put_meta_record(b"round", &[enter as u8])
            .unwrap()
            .expect("a record");
        let mut merk = open(handle, TreeType::NormalTree);
        apply(&mut merk, batch);
        let put = Op::Put(vec![enter as u8; 3], TreeFeatureType::BasicMerkNode);
        let aux = [(b"a".to_vec(), put, None)];
        merk.apply::<Vec<u8>, Vec<u8>>(&[], &aux, None, version())
            .unwrap()
            .expect("an aux record");
        drop(merk);
        staged.apply(&transaction).unwrap().expect("a batch");
        facade.commit(&transaction);
        rocks.apply(batch)
    };
    let (.., hash, root) = write(&puts(1..60, 1), true);
    let expected = rocks.records(root.clone());
    let snapshot = LigninSnapshot::new(facade.db.snapshot());
    assert!(format!("{snapshot:?}").contains("LigninSnapshot"));
    assert_eq!(snapshot.snapshot().version(), facade.db.version());
    // The next version has another Merk under the same subtree.
    let mut later = deletes(10..30);
    later.extend(puts(30..45, 2));
    later.sort_by(|a, b| a.0.cmp(&b.0));
    let (.., later_hash, later_root) = write(&later, false);
    assert_ne!(hash, later_hash);

    let handle = LigninNodeStore::read_only(&snapshot, ROOT);
    let entry = handle
        .entry()
        .expect("the directory answers")
        .expect("entered");
    assert_eq!(entry.root_key, root);
    assert_eq!(
        entry.tree_type().expect("a tree type"),
        TreeType::NormalTree
    );
    assert_eq!(handle.root_key().unwrap().expect("a read"), root);
    let stored = records(&handle, root.clone());
    assert_same_records(&stored, &expected, "through the snapshot");
    assert!(handle.has_node(&plain(15)).unwrap().expect("a read"));
    assert!(!handle.has_node(&plain(70)).unwrap().expect("a read"));
    assert_eq!(
        handle.get_meta_record(b"round").unwrap().expect("a read"),
        Some(vec![1])
    );
    assert_eq!(
        handle.get_aux_record(b"a").unwrap().expect("a read"),
        Some(vec![1; 3])
    );
    assert_eq!(handle.get_aux_record(b"b").unwrap().expect("a read"), None);
    // A read through a snapshot is charged what the same read of the
    // same state through a transaction is charged: a node that is there,
    // one that the later version removed, and one that never was.
    let transaction = facade.db.begin();
    let through_transaction = LigninNodeStore::read_only(&transaction, ROOT);
    let latest = LigninSnapshot::new(facade.db.snapshot());
    let through_snapshot = LigninNodeStore::read_only(&latest, ROOT);
    for k in [35, 2, 15, 70] {
        let read = |handle: &LigninNodeStore<'_>| {
            let node = handle.get_node(&plain(k), NO_VALUE_DEFINED_COST, version());
            (
                node.cost,
                node.value.expect("a read").map(|node| node.encode()),
            )
        };
        assert_eq!(
            read(&through_snapshot),
            read(&through_transaction),
            "key {k}"
        );
        assert_eq!(
            through_snapshot.get_node_for_read(&plain(k)).cost,
            through_transaction.get_node_for_read(&plain(k)).cost,
            "key {k}"
        );
        assert_eq!(
            through_snapshot.has_node(&plain(k)).cost,
            through_transaction.has_node(&plain(k)).cost,
            "key {k}"
        );
    }
    drop(through_snapshot);
    drop(through_transaction);
    drop(transaction);

    // The Merk of the snapshot's version, and its nodes in key order.
    let merk = open(
        LigninNodeStore::read_only(&snapshot, ROOT),
        TreeType::NormalTree,
    );
    assert_eq!(merk.root_hash().unwrap(), hash);
    drop(merk);
    let mut keys: Vec<Vec<u8>> = expected.iter().map(|(key, _)| key.clone()).collect();
    keys.sort();
    let mut cursor = NodeStore::cursor(&handle);
    let mut walked = Vec::new();
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        let at = cursor.key().unwrap().expect("a key").to_vec();
        let record = cursor.value().unwrap().expect("a record").to_vec();
        walked.push((at, record));
        cursor.next().unwrap();
    }
    cursor.status().expect("the walk came to its end");
    assert_same_records(&walked, &expected, "under the snapshot's cursor");
    cursor.seek_to_last().unwrap();
    assert_eq!(cursor.key().unwrap(), keys.last().map(Vec::as_slice));
    cursor.prev().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(keys[keys.len() - 2].as_slice()));
    cursor.seek(&keys[7]).unwrap();
    assert_eq!(cursor.key().unwrap(), Some(keys[7].as_slice()));
    cursor.seek_for_prev(&keys[7]).unwrap();
    assert_eq!(cursor.key().unwrap(), Some(keys[7].as_slice()));
    // Pointed at a subtree that is not entered it stands on nothing, and
    // back at its own it walks again.
    let mut cursor = cursor.retarget(&[9; 32]);
    cursor.seek_to_first().unwrap();
    assert!(!cursor.valid().unwrap());
    cursor.status().expect("no node, and no error");
    let mut cursor = cursor.retarget(&ROOT.prefix);
    cursor.seek_to_first().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(keys[0].as_slice()));

    // The flat records of the snapshot's version.
    let mut flat = handle.meta().cursor();
    flat.seek_to_first().unwrap();
    assert_eq!(flat.key().unwrap(), Some(b"round".as_slice()));
    assert_eq!(flat.value().unwrap(), Some([1].as_slice()));
    flat.next().unwrap();
    assert!(!flat.valid().unwrap());
    flat.status().expect("the walk came to its end");

    // A subtree that the version does not have has nothing to read.
    let absent = LigninNodeStore::read_only(&snapshot, LigninSubtree::alone([9; 32]));
    assert_eq!(absent.entry().expect("the directory answers"), None);
    assert_eq!(absent.root_key().unwrap().expect("a read"), None);
    assert!(!absent.has_node(b"k").unwrap().expect("a read"));
    let mut nodes = NodeStore::cursor(&absent);
    nodes.seek_to_first().unwrap();
    assert!(!nodes.valid().unwrap());
    nodes.status().expect("no node, and no error");

    // Every write is refused, by its typed error.
    let read_only = |error: Error| {
        let error = storage_error(error);
        assert!(
            matches!(
                error,
                grovedb_storage::Error::SnapshotReadOnlyTransaction(_)
            ),
            "{error}"
        );
    };
    let immediate = LigninNodeStore::immediate(&snapshot, ROOT);
    read_only(
        immediate
            .put_meta_record(b"k", b"v")
            .unwrap()
            .expect_err("a put"),
    );
    read_only(immediate.enter(TreeType::NormalTree).expect_err("an enter"));
    let batch = LigninBatch::new();
    let deferred = LigninNodeStore::deferred(&snapshot, ROOT, Some(&batch));
    deferred
        .put_meta_record(b"k", b"v")
        .unwrap()
        .expect("staging");
    read_only(batch.apply(&snapshot).unwrap().expect_err("a batch"));

    // A snapshot of the latest version reads the later Merk.
    let latest = LigninSnapshot::new(facade.db.snapshot());
    let handle = LigninNodeStore::read_only(&latest, ROOT);
    let stored = records(&handle, later_root.clone());
    assert_same_records(&stored, &rocks.records(later_root), "at the latest version");
    assert_eq!(
        handle.get_meta_record(b"round").unwrap().expect("a read"),
        Some(vec![0])
    );
    facade.sound();
}

#[test]
fn a_merk_that_two_operations_of_one_batch_write_is_charged_for_what_stands() {
    let facade = Facade::new();
    let rocks = rocks();
    // What is committed before: the second operation removes some of it.
    let before = puts(1..20, 1);
    let transaction = facade.db.begin();
    let batch = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&transaction, ROOT, Some(&batch));
    handle
        .enter(TreeType::NormalTree)
        .expect("the root is entered");
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &before);
    drop(merk);
    batch.apply(&transaction).unwrap().expect("a batch");
    facade.commit(&transaction);
    rocks.apply(&before);

    // Two operations of one Merk that stays open, into one batch: the
    // first puts, and the second removes nodes the first put, nodes that
    // were committed and writes others again.
    let first = puts(15..40, 2);
    let mut second = deletes(10..25);
    second.extend(puts(30..50, 3));
    second.sort_by(|a, b| a.0.cmp(&b.0));

    let rocks_transaction = rocks.storage.start_transaction();
    let storage_batch = StorageBatch::new();
    let context = rocks
        .storage
        .get_transactional_storage_context(
            SubtreePath::empty(),
            Some(&storage_batch),
            &rocks_transaction,
        )
        .unwrap();
    let mut on_rocks = open(context, TreeType::NormalTree);
    let transaction = facade.db.begin();
    let batch = LigninBatch::new();
    let mut on_lignin = open(
        LigninNodeStore::deferred(&transaction, ROOT, Some(&batch)),
        TreeType::NormalTree,
    );
    assert_eq!(apply(&mut on_lignin, &first), apply(&mut on_rocks, &first));
    storage_batch.next_operation();
    batch.next_operation();
    assert_eq!(
        apply(&mut on_lignin, &second),
        apply(&mut on_rocks, &second)
    );
    let root = (on_rocks.root_hash().unwrap(), on_rocks.root_key());
    assert_eq!((on_lignin.root_hash().unwrap(), on_lignin.root_key()), root);
    drop((on_lignin, on_rocks));

    let expected = rocks
        .storage
        .commit_multi_context_batch(storage_batch, Some(&rocks_transaction));
    let applied = batch.apply(&transaction);
    assert_eq!(applied.cost, expected.cost);
    expected.value.expect("the storage batch is applied");
    applied.value.expect("the batch is applied");
    (rocks.storage.commit_transaction(rocks_transaction))
        .unwrap()
        .expect("a commit");
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    let stored = records(&reader, root.1.clone());
    assert_same_records(
        &stored,
        &rocks.records(root.1.clone()),
        "in the transaction",
    );
    drop(reader);
    facade.commit(&transaction);
    let snapshot = LigninSnapshot::new(facade.db.snapshot());
    let reader = LigninNodeStore::read_only(&snapshot, ROOT);
    let stored = records(&reader, root.1.clone());
    assert_same_records(&stored, &rocks.records(root.1), "committed");
    facade.sound();
}

/// Two commits of one Merk in one operation of a batch (`transactions.md`
/// GM0): a node that the first put and the second removed stays a put, as
/// a storage batch keeps it, and the root that the second stated does not
/// reach it. Both engines are charged the same for the batch and hold the
/// same records afterwards, by key and under a cursor: in the transaction
/// that applied the batch, and in a snapshot once it is committed.
#[test]
fn a_put_that_stands_over_a_removal_of_its_operation_is_held_as_on_rocksdb() {
    use super::cursor::{act, Act};

    let facade = Facade::new();
    let rocks = rocks();
    let before = puts(1..20, 1);
    let transaction = facade.db.begin();
    let batch = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&transaction, ROOT, Some(&batch));
    handle
        .enter(TreeType::NormalTree)
        .expect("the root is entered");
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &before);
    drop(merk);
    batch.apply(&transaction).unwrap().expect("a batch");
    facade.commit(&transaction);
    rocks.apply(&before);

    // The first commit writes nodes that are committed again and new
    // ones, and the second removes some of both, and nodes that only the
    // committed state has. No boundary of an operation is between them.
    let first = puts(15..40, 2);
    let mut second = deletes(10..25);
    second.extend(puts(30..50, 3));
    second.sort_by(|a, b| a.0.cmp(&b.0));

    let rocks_transaction = rocks.storage.start_transaction();
    let storage_batch = StorageBatch::new();
    let context = rocks
        .storage
        .get_transactional_storage_context(
            SubtreePath::empty(),
            Some(&storage_batch),
            &rocks_transaction,
        )
        .unwrap();
    let mut on_rocks = open(context, TreeType::NormalTree);
    let transaction = facade.db.begin();
    let batch = LigninBatch::new();
    let mut on_lignin = open(
        LigninNodeStore::deferred(&transaction, ROOT, Some(&batch)),
        TreeType::NormalTree,
    );
    assert_eq!(apply(&mut on_lignin, &first), apply(&mut on_rocks, &first));
    assert_eq!(
        apply(&mut on_lignin, &second),
        apply(&mut on_rocks, &second)
    );
    let root = (on_rocks.root_hash().unwrap(), on_rocks.root_key());
    assert_eq!((on_lignin.root_hash().unwrap(), on_lignin.root_key()), root);
    drop((on_lignin, on_rocks));

    let expected = rocks
        .storage
        .commit_multi_context_batch(storage_batch, Some(&rocks_transaction));
    let applied = batch.apply(&transaction);
    assert_eq!(applied.cost, expected.cost);
    expected.value.expect("the storage batch is applied");
    applied.value.expect("the batch is applied");
    (rocks.storage.commit_transaction(rocks_transaction))
        .unwrap()
        .expect("a commit");

    // What each engine answers: every read of each key that the second
    // commit removed, the whole Merk under a cursor both ways, and the
    // keys the tree links.
    let mut looks: Vec<Act> = (10..25).map(|k| Act::Read(plain(k))).collect();
    looks.extend([Act::Scan(true), Act::Scan(false), Act::Linked]);
    fn answers<'db, S: NodeStore<'db>>(store: S, looks: &[Act]) -> Vec<(String, OperationCost)> {
        let mut merk = open(store, TreeType::NormalTree);
        looks.iter().map(|look| act(&mut merk, look)).collect()
    }
    let rocks_transaction = rocks.storage.start_transaction();
    let context = rocks
        .storage
        .get_transactional_storage_context(SubtreePath::empty(), None, &rocks_transaction)
        .unwrap();
    let expected = answers(context, &looks);
    // Every node that the first commit wrote stays, in no tree: those it
    // put, and those among the committed ones that it wrote again on its
    // way. A scan shows what a read by key finds.
    let (scan, linked) = (&expected[15].0, &expected[17].0);
    let mut rewritten = 0;
    for k in 10..25 {
        let key = plain(k);
        let held = expected[(k - 10) as usize].0.ends_with("held Ok(true)");
        assert!(held || k < 15, "the record under {k}");
        rewritten += usize::from(held && k < 15);
        assert_eq!(
            scan.matches(&format!("({key:?}, ")).count(),
            usize::from(held)
        );
        assert!(!linked.contains(&format!("{key:?}")), "{k} is linked");
    }
    // One of them had a cell, whose value its record keeps.
    assert!(
        rewritten > 0,
        "no committed node was written again and removed"
    );

    let in_transaction = answers(LigninNodeStore::read_only(&transaction, ROOT), &looks);
    assert_eq!(in_transaction, expected, "in the transaction");
    let reader = LigninNodeStore::read_only(&transaction, ROOT);
    assert_same_records(
        &records(&reader, root.1.clone()),
        &rocks.records(root.1.clone()),
        "in the transaction",
    );
    drop(reader);
    facade.commit(&transaction);
    let snapshot = LigninSnapshot::new(facade.db.snapshot());
    let committed = answers(LigninNodeStore::read_only(&snapshot, ROOT), &looks);
    assert_eq!(committed, expected, "in a snapshot");
    let reader = LigninNodeStore::read_only(&snapshot, ROOT);
    assert_same_records(
        &records(&reader, root.1.clone()),
        &rocks.records(root.1),
        "committed",
    );
    facade.sound();
}

/// The calls of one flat keyspace, each with what it was charged and what
/// it answered, as text.
fn flat_script<S: FlatStore>(keyspace: &S) -> Vec<(OperationCost, String)> {
    let mut seen = Vec::new();
    let costed = || Some(KeyValueStorageCost::default());
    let mut call = |answer: grovedb_costs::CostResult<String, grovedb_storage::Error>| {
        seen.push((answer.cost.clone(), format!("{:?}", answer.unwrap().ok())));
    };
    let shown = |found: Option<Vec<u8>>| format!("{found:?}");
    call(keyspace.get(b"committed").map_ok(shown));
    call(keyspace.get(b"absent").map_ok(shown));
    call(
        keyspace
            .put(b"new", b"a value", None)
            .map_ok(|()| "put".to_owned()),
    );
    call(
        keyspace
            .put(b"costed", &[7; 300], costed())
            .map_ok(|()| "put".to_owned()),
    );
    call(
        keyspace
            .put(b"committed", b"again", None)
            .map_ok(|()| "put".to_owned()),
    );
    call(
        keyspace
            .delete(b"gone", None)
            .map_ok(|()| "delete".to_owned()),
    );
    call(
        keyspace
            .delete(b"e", costed())
            .map_ok(|()| "delete".to_owned()),
    );
    call(
        keyspace
            .delete(b"never there", None)
            .map_ok(|()| "delete".to_owned()),
    );
    // A deferred handle reads nothing that is staged, and an immediate
    // one its own writes.
    call(keyspace.get(b"new").map_ok(shown));
    call(keyspace.get(b"gone").map_ok(shown));
    let mut cursor = keyspace.cursor();
    let mut moved = |cost: OperationCost, cursor: &S::Cursor<'_>| {
        let key = cursor.key();
        let value = cursor.value();
        let valid = cursor.valid();
        seen.push((cost, format!("valid {:?}", valid.value)));
        seen.push((valid.cost, "valid".to_owned()));
        seen.push((key.cost, format!("key {:?}", key.value)));
        seen.push((value.cost, format!("value {:?}", value.value)));
    };
    let first = cursor.seek_to_first().cost;
    moved(first, &cursor);
    for _ in 0..5 {
        let next = cursor.next().cost;
        moved(next, &cursor);
    }
    let last = cursor.seek_to_last().cost;
    moved(last, &cursor);
    let prev = cursor.prev().cost;
    moved(prev, &cursor);
    let sought = cursor.seek(b"d").cost;
    moved(sought, &cursor);
    let back = cursor.seek_for_prev(b"d").cost;
    moved(back, &cursor);
    assert!(cursor.status().is_ok());
    seen
}

#[test]
fn a_flat_keyspace_costs_what_it_costs_in_a_storage_context() {
    #[derive(Clone, Copy, Debug)]
    enum Space {
        Data,
        Aux,
        Meta,
    }
    /// Calls `$call` with the keyspace `$space` of `$handle`, a handle of
    /// either engine.
    macro_rules! in_space {
        ($space:expr, $handle:expr, $call:expr) => {
            match $space {
                Space::Data => ($call)(&$handle),
                Space::Aux => ($call)(&$handle.aux()),
                Space::Meta => ($call)(&$handle.meta()),
            }
        };
    }
    /// How the handle under test writes.
    #[derive(Clone, Copy, Debug)]
    enum Through {
        Batch,
        AtOnce,
        Scope,
    }
    let flat = LigninSubtree::alone([0; 32]);
    for space in [Space::Data, Space::Aux, Space::Meta] {
        for through in [Through::Batch, Through::AtOnce, Through::Scope] {
            let at = format!("{space:?}, {through:?}");
            let facade = Facade::new();
            let storage = TempStorage::new();

            // What is committed before, on both.
            let committed: [(&[u8], &[u8]); 3] =
                [(b"committed", b"1"), (b"gone", &[3; 280]), (b"e", b"5")];
            let transaction = facade.db.begin();
            let handle = LigninNodeStore::immediate(&transaction, flat.clone());
            handle
                .enter(TreeType::NormalTree)
                .expect("the subtree is entered");
            let rocks_transaction = storage.start_transaction();
            let context = storage
                .get_immediate_storage_context(SubtreePath::empty(), &rocks_transaction)
                .unwrap();
            for (key, value) in committed {
                in_space!(space, handle, |keyspace| {
                    FlatStore::put(keyspace, key, value, None)
                        .unwrap()
                        .expect("a put")
                });
                in_space!(space, context, |keyspace| {
                    FlatStore::put(keyspace, key, value, None)
                        .unwrap()
                        .expect("a put")
                });
            }
            drop((handle, context));
            facade.commit(&transaction);
            storage
                .commit_transaction(rocks_transaction)
                .unwrap()
                .expect("a commit");

            // The same calls on both, and the same application.
            let transaction = facade.db.begin();
            let rocks_transaction = storage.start_transaction();
            let batch = LigninBatch::new();
            let storage_batch = StorageBatch::new();
            let (on_lignin, on_rocks) = match through {
                Through::AtOnce => {
                    let handle = LigninNodeStore::immediate(&transaction, flat.clone());
                    let context = storage
                        .get_immediate_storage_context(SubtreePath::empty(), &rocks_transaction)
                        .unwrap();
                    (
                        in_space!(space, handle, flat_script),
                        in_space!(space, context, flat_script),
                    )
                }
                // A writer's handle: its cursor starts each move from the
                // key it stands on.
                Through::Scope => {
                    let writer = LigninWriter::new(transaction.scope().expect("a scope"));
                    let handle = LigninNodeStore::immediate(&writer, flat.clone());
                    let context = storage
                        .get_immediate_storage_context(SubtreePath::empty(), &rocks_transaction)
                        .unwrap();
                    let answers = (
                        in_space!(space, handle, flat_script),
                        in_space!(space, context, flat_script),
                    );
                    drop(handle);
                    writer.into_scope().merge();
                    answers
                }
                Through::Batch => {
                    let handle =
                        LigninNodeStore::deferred(&transaction, flat.clone(), Some(&batch));
                    let context = storage
                        .get_transactional_storage_context(
                            SubtreePath::empty(),
                            Some(&storage_batch),
                            &rocks_transaction,
                        )
                        .unwrap();
                    (
                        in_space!(space, handle, flat_script),
                        in_space!(space, context, flat_script),
                    )
                }
            };
            assert_eq!(on_lignin.len(), on_rocks.len(), "{at}");
            for (call, (lignin, rocks)) in on_lignin.iter().zip(&on_rocks).enumerate() {
                assert_eq!(lignin, rocks, "call {call}, {at}");
            }
            let applied = batch.apply(&transaction);
            let expected =
                storage.commit_multi_context_batch(storage_batch, Some(&rocks_transaction));
            assert_eq!(applied.cost, expected.cost, "{at}");
            applied.value.expect("the batch is applied");
            expected.value.expect("the storage batch is applied");
            // What both hold afterwards, read through a handle that only
            // reads.
            let handle = LigninNodeStore::read_only(&transaction, flat.clone());
            let context = storage
                .get_transactional_storage_context(SubtreePath::empty(), None, &rocks_transaction)
                .unwrap();
            let unstaged = |answers: Vec<(OperationCost, String)>| -> Vec<String> {
                answers
                    .into_iter()
                    .skip(10)
                    .map(|(_, answer)| answer)
                    .collect()
            };
            let after_lignin = in_space!(space, handle, flat_script);
            let after_rocks = in_space!(space, context, flat_script);
            assert_eq!(unstaged(after_lignin), unstaged(after_rocks), "{at}");
            // A handle that only reads refuses a write, at the charge of
            // a refused one.
            let refused = in_space!(space, handle, |keyspace| FlatStore::put(
                keyspace, b"k", b"v", None
            ));
            let expected = in_space!(space, context, |keyspace| FlatStore::put(
                keyspace, b"k", b"v", None
            ));
            assert_eq!(refused.cost, expected.cost, "{at}");
            assert!(refused.value.is_err() && expected.value.is_err(), "{at}");
            drop((handle, context));
            facade.commit(&transaction);
        }
    }
}

#[test]
fn a_writers_flat_cursor_starts_each_move_from_the_key_it_stands_on() {
    let facade = Facade::new();
    let transaction = facade.db.begin();
    let writer = LigninWriter::new(transaction.scope().expect("a scope"));
    let handle = LigninNodeStore::immediate(&writer, ROOT);
    handle
        .enter(TreeType::NormalTree)
        .expect("the root is entered");
    let aux = handle.aux();
    for key in [b"a", b"c", b"e"] {
        aux.put(key, key, None).unwrap().expect("a put");
    }
    let mut cursor = aux.cursor();
    assert!(format!("{cursor:?}").contains("LigninFlatCursor"));
    // A step from no key goes nowhere.
    cursor.next().unwrap();
    assert!(!cursor.valid().unwrap());
    cursor.seek(b"b").unwrap();
    assert_eq!(cursor.key().unwrap(), Some(b"c".as_slice()));
    // The key it stands on goes and another is written past it: the
    // cursor shows what the move that came to the key read, and the step
    // is from where the cursor stood.
    aux.delete(b"c", None).unwrap().expect("a delete");
    aux.put(b"d", b"d", None).unwrap().expect("a put");
    assert_eq!(cursor.value().unwrap(), Some(b"c".as_slice()));
    cursor.next().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(b"d".as_slice()));
    assert_eq!(cursor.value().unwrap(), Some(b"d".as_slice()));
    aux.delete(b"d", None).unwrap().expect("a delete");
    cursor.prev().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(b"a".as_slice()));
    cursor.prev().unwrap();
    assert!(!cursor.valid().unwrap());
    assert_eq!(cursor.value().unwrap(), None);
    cursor.seek_to_last().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(b"e".as_slice()));
    cursor.next().unwrap();
    assert!(!cursor.valid().unwrap());
    assert!(cursor.status().is_ok());
    // A clear through the scope walks with that cursor, and removes every
    // record.
    aux.clear().unwrap().expect("a clear");
    cursor.seek_to_first().unwrap();
    assert!(!cursor.valid().unwrap());
    assert_eq!(aux.get(b"a").unwrap().expect("a read"), None);
    // A walk that the transaction refuses is no walk to the end: a clear
    // through a handle outside the scope, which holds the keyspace, fails
    // with the refusal and its class.
    aux.put(b"k", b"v", None).unwrap().expect("a put");
    let outside = LigninNodeStore::immediate(&transaction, ROOT);
    let refused = outside
        .aux()
        .clear()
        .unwrap()
        .expect_err("the scope holds it");
    assert!(
        matches!(
            refused,
            grovedb_storage::Error::WriteScope(grovedb_storage::error::WriteScopeError::Overlap)
        ),
        "{refused}"
    );
    assert_eq!(aux.get(b"k").unwrap().expect("a read"), Some(b"v".to_vec()));
    // Batches that were settled apart are not applied together through a
    // scope, which could not take a part of them back.
    let batch = LigninBatch::new();
    let deferred = LigninNodeStore::deferred(&writer, ROOT, Some(&batch));
    deferred
        .meta()
        .put(b"a", b"1", None)
        .unwrap()
        .expect("staging");
    let first = batch.settle(&writer).unwrap().expect("a settled batch");
    assert!(!first.is_empty() && !first.enters(&ROOT.prefix));
    assert_eq!(first.written().count(), 1);
    assert_eq!(first.node(&ROOT.prefix, b"a"), None);
    deferred
        .meta()
        .put(b"b", b"2", None)
        .unwrap()
        .expect("staging");
    let second = batch.settle(&writer).unwrap().expect("a settled batch");
    let together = LigninSettled::apply(vec![first, second], &writer, &[]);
    let refused = together.unwrap().expect_err("two parts through a scope");
    assert!(matches!(refused, Error::NotSupported(_)), "{refused}");
    assert_eq!(deferred.meta().get(b"a").unwrap().expect("a read"), None);
}

#[test]
fn a_clear_removes_flat_records_and_refuses_the_nodes_of_a_merk_where_it_does_not_stage() {
    let facade = Facade::new();
    let storage = TempStorage::new();
    let transaction = facade.db.begin();
    let rocks_transaction = storage.start_transaction();
    let context = |path: &'static [&'static [u8]]| {
        storage
            .get_immediate_storage_context(path.into(), &rocks_transaction)
            .unwrap()
    };

    // A Merk with nodes: its nodes are no flat records. A handle that
    // stages removes them as the records they are on RocksDB (`cost`,
    // `a_merk_cleared_as_the_records_of_its_keyspace_..`); one that writes
    // at once says that it does not, and removes nothing.
    let merk_at = LigninSubtree::alone([1; 32]);
    let handle = LigninNodeStore::immediate(&transaction, merk_at.clone());
    handle
        .enter(TreeType::NormalTree)
        .expect("the subtree is entered");
    let mut merk = open(
        LigninNodeStore::immediate(&transaction, merk_at.clone()),
        TreeType::NormalTree,
    );
    apply(&mut merk, &puts(1..6, 1));
    let refused = FlatStore::clear(&handle);
    assert_eq!(refused.cost, OperationCost::default());
    let refused = refused.value.expect_err("a Merk with nodes");
    assert!(
        matches!(refused, grovedb_storage::Error::NotSupported(_)),
        "{refused}"
    );
    assert!(refused.to_string().contains("lignindb"), "{refused}");
    assert!(handle.has_node(&plain(3)).unwrap().expect("a read"));
    // Its aux keyspace is cleared as the flat keyspace it is.
    handle.aux().put(b"a", b"1", None).unwrap().expect("a put");
    handle.aux().clear().unwrap().expect("a clear");
    assert_eq!(handle.aux().get(b"a").unwrap().expect("a read"), None);
    // A Merk that has no node left has nothing to walk past: the clear is
    // the walk of an empty keyspace, at its cost.
    apply(&mut merk, &deletes(1..6));
    drop(merk);
    let empty = context(&[b"empty"]).clear();
    let cleared = FlatStore::clear(&handle);
    assert_eq!(cleared.cost, empty.cost);
    cleared.value.expect("an empty Merk");

    // A tree that keeps its data flat is cleared by the walk, at the cost
    // of a storage context.
    let flat_at = LigninSubtree::alone([2; 32]);
    let handle = LigninNodeStore::immediate(&transaction, flat_at);
    handle
        .enter(TreeType::NormalTree)
        .expect("the subtree is entered");
    let rocks_flat = context(&[b"flat"]);
    for key in [b"a", b"b", b"c"] {
        FlatStore::put(&handle, key, &[7; 40], None)
            .unwrap()
            .expect("a put");
        FlatStore::put(&rocks_flat, key, &[7; 40], None)
            .unwrap()
            .expect("a put");
    }
    let expected = rocks_flat.clear();
    let cleared = FlatStore::clear(&handle);
    assert_eq!(cleared.cost, expected.cost);
    cleared.value.expect("a flat keyspace");
    expected.value.expect("a flat keyspace");
    assert_eq!(
        FlatStore::get(&handle, b"b").unwrap().expect("a read"),
        None
    );

    // A subtree that is not entered has no record to remove.
    let nowhere = LigninNodeStore::immediate(&transaction, LigninSubtree::alone([3; 32]));
    let cleared = FlatStore::clear(&nowhere);
    assert_eq!(cleared.cost, empty.cost);
    cleared.value.expect("nothing to clear");

    // A directory that the transaction does not let the handle read
    // refuses the clear: another open scope writes the subtree's Merk.
    let merk_at = LigninSubtree::alone([4; 32]);
    let writer = LigninWriter::new(transaction.scope().expect("a scope"));
    LigninNodeStore::immediate(&writer, merk_at.clone())
        .enter(TreeType::NormalTree)
        .expect("the subtree is entered");
    let mut merk = open(
        LigninNodeStore::immediate(&writer, merk_at.clone()),
        TreeType::NormalTree,
    );
    apply(&mut merk, &puts(1..3, 1));
    drop(merk);
    let outside = LigninNodeStore::immediate(&transaction, merk_at);
    let refused = FlatStore::clear(&outside).unwrap().expect_err("a scope");
    assert_eq!(
        refused.to_string(),
        grovedb_storage::Error::WriteScope(grovedb_storage::error::WriteScopeError::Overlap)
            .to_string()
    );
    // An error of the store that is no storage error is one by its words.
    let worded = super::super::flat::storage_error(Error::CorruptedState("a state"));
    assert!(matches!(worded, grovedb_storage::Error::StorageError(_)));
    assert!(worded.to_string().contains("a state"));
}

#[test]
fn settled_batches_are_applied_together_or_not_at_all() {
    let facade = Facade::new();
    let transaction = facade.db.begin();
    let parent = LigninSubtree::alone([0; 32]);
    let child_prefix = [8; 32];
    let hosted = LigninSubtree {
        prefix: child_prefix,
        host: Some(LigninHost {
            subtree: parent.prefix,
            key: plain(2),
            hosted: vec![child_prefix],
        }),
    };
    LigninNodeStore::immediate(&transaction, parent.clone())
        .enter(TreeType::NormalTree)
        .expect("the root is entered");
    // A Merk beside it that holds two nodes.
    let beside = LigninSubtree::alone([9; 32]);
    LigninNodeStore::immediate(&transaction, beside.clone())
        .enter(TreeType::NormalTree)
        .expect("the subtree is entered");
    let mut merk = open(
        LigninNodeStore::immediate(&transaction, beside.clone()),
        TreeType::NormalTree,
    );
    apply(&mut merk, &puts(5..7, 1));
    drop(merk);

    // What a settled batch says of a key: a put with the value it states,
    // a removal, and nothing for a key it does not write or a record that
    // is no node.
    let batch = LigninBatch::new();
    assert!(format!("{batch:?}").contains("LigninBatch"));
    let handle = LigninNodeStore::deferred(&transaction, parent.clone(), Some(&batch));
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &puts(1..4, 1));
    drop(merk);
    let view = LigninView::from(&transaction);
    assert!(format!("{view:?}").contains("LigninView"));
    let first = batch.settle(view).unwrap().expect("a settled batch");
    assert!(format!("{first:?}").contains("LigninSettled"));
    let stated = value(2_001, true);
    assert_eq!(
        first.node(&parent.prefix, &plain(2)),
        Some(LigninOutcome::Put(Some(stated.as_slice())))
    );
    assert_eq!(first.node(&parent.prefix, &plain(9)), None);
    assert_eq!(first.node(&child_prefix, &plain(2)), None);

    // The second batch writes into a subtree that nothing enters: the
    // transaction refuses it, and keeps nothing of the first either.
    let by_prefix = LigninNodeStore::deferred(
        &transaction,
        LigninSubtree::alone(child_prefix),
        Some(&batch),
    );
    by_prefix
        .meta()
        .put(b"m", b"1", None)
        .unwrap()
        .expect("staging");
    let by_path = LigninNodeStore::deferred(&transaction, hosted.clone(), Some(&batch));
    by_path
        .aux()
        .put(b"a", b"1", None)
        .unwrap()
        .expect("staging");
    by_prefix
        .aux()
        .put(b"b", b"2", None)
        .unwrap()
        .expect("staging");
    let second = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    // Of the handles of one subtree, the one that names its host is kept.
    let written: Vec<&LigninSubtree> = second.written().collect();
    assert_eq!(written, [&hosted]);
    assert!(!second.enters(&child_prefix));
    let refused = LigninSettled::apply(vec![first, second], &transaction, &[]);
    assert!(refused.unwrap().is_err());
    assert!(!transaction.scopes_open());
    let reader = LigninNodeStore::read_only(&transaction, parent.clone());
    assert!(!reader.has_node(&plain(2)).unwrap().expect("a read"));

    // With the subtree entered beside them, both are taken.
    let handle = LigninNodeStore::deferred(&transaction, parent.clone(), Some(&batch));
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &puts(1..4, 1));
    drop(merk);
    let first = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    let by_path = LigninNodeStore::deferred(&transaction, hosted.clone(), Some(&batch));
    by_path
        .aux()
        .put(b"a", b"1", None)
        .unwrap()
        .expect("staging");
    let mut merk = open(
        LigninNodeStore::deferred(&transaction, beside.clone(), Some(&batch)),
        TreeType::NormalTree,
    );
    apply(&mut merk, &deletes(5..6));
    drop(merk);
    let second = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert_eq!(
        second.node(&beside.prefix, &plain(5)),
        Some(LigninOutcome::Removed)
    );
    let enters = [(hosted.clone(), tree_type_bytes(TreeType::NormalTree))];
    // Subtrees are entered with a batch, and with none there is none to
    // enter them with.
    let alone = LigninSettled::apply(Vec::new(), &transaction, &enters);
    assert!(alone.unwrap().is_err());
    let applied = LigninSettled::apply(vec![first, second], &transaction, &enters);
    applied.unwrap().expect("both batches");
    let reader = LigninNodeStore::read_only(&transaction, hosted);
    assert_eq!(
        reader.aux().get(b"a").unwrap().expect("a read"),
        Some(b"1".to_vec())
    );
    let entry = reader.entry().expect("the directory answers");
    assert_eq!(entry.expect("entered").host.expect("a host").key, plain(2));
    // A subtree that the batch enters itself, and one under a node of it
    // that is entered for the batch: the outer one first.
    let outer = LigninSubtree::alone([10; 32]);
    let inner = LigninSubtree {
        prefix: [11; 32],
        host: Some(LigninHost {
            subtree: outer.prefix,
            key: plain(1),
            hosted: vec![[11; 32]],
        }),
    };
    batch
        .enter(&outer, &tree_type_bytes(TreeType::NormalTree))
        .expect("an enter");
    let mut merk = open(
        LigninNodeStore::deferred(&transaction, outer.clone(), Some(&batch)),
        TreeType::NormalTree,
    );
    apply(&mut merk, &puts(1..2, 1));
    drop(merk);
    let below = LigninNodeStore::deferred(&transaction, inner.clone(), Some(&batch));
    (below.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    let part = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert!(part.enters(&outer.prefix) && !part.enters(&inner.prefix));
    let enters = [(inner.clone(), tree_type_bytes(TreeType::NormalTree))];
    let applied = LigninSettled::apply(vec![part], &transaction, &enters);
    applied
        .unwrap()
        .expect("the outer subtree is entered first");
    let entry = below.entry().expect("the directory answers");
    assert_eq!(entry.expect("entered").host.expect("a host").key, plain(1));

    // A subtree whose every write a later operation of the batch takes
    // back is written into no more: nothing is left to enter it for.
    let gone = LigninSubtree::alone([12; 32]);
    batch
        .enter(&gone, &tree_type_bytes(TreeType::NormalTree))
        .expect("an enter");
    let handle = LigninNodeStore::deferred(&transaction, gone.clone(), Some(&batch));
    (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    batch.next_operation();
    batch.drop_subtree(&gone);
    let part = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert_eq!(part.written().count(), 0);
    assert!(part.is_empty() && !part.enters(&gone.prefix));
    // A statement anew that whoever applies the batch makes of it is a
    // write of the batch, until it is taken back.
    let mut part = part;
    part.state_anew(&inner, &tree_type_bytes(TreeType::SumTree))
        .expect("a statement anew");
    assert!(!part.is_empty());
    part.forget_stated();
    assert!(part.is_empty());
    // So is a drop that it was told.
    part.drop_subtree(&inner.prefix);
    assert!(!part.is_empty());
    part.forget_drops();
    assert!(part.is_empty());
    // A batch that removes the one node of a Merk leaves it with none,
    // and one that puts a node beside it does not.
    let handle = LigninNodeStore::deferred(&transaction, outer.clone(), Some(&batch));
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &puts(2..3, 1));
    drop(merk);
    let put = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert!(put.writes_merk(&outer.prefix) && !put.empties(&outer.prefix));
    let handle = LigninNodeStore::deferred(&transaction, outer.clone(), Some(&batch));
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &deletes(1..2));
    drop(merk);
    let emptied = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert!(emptied.writes_merk(&outer.prefix) && emptied.empties(&outer.prefix));
    assert!(!emptied.empties(&inner.prefix));

    let reader = LigninNodeStore::read_only(&transaction, parent);
    assert!(reader.has_node(&plain(2)).unwrap().expect("a read"));
    let reader = LigninNodeStore::read_only(&transaction, beside);
    assert!(!reader.has_node(&plain(5)).unwrap().expect("a read"));
    assert!(reader.has_node(&plain(6)).unwrap().expect("a read"));
    drop(reader);
    facade.commit(&transaction);
    facade.sound();
}

/// The batch that a subtree given with a group is entered with
/// ([`LigninSettled::apply`]), where no batch enters the subtree its
/// host is in. Under the key of each, another subtree holds the place
/// or none does.
///
/// With the first batch that puts the node of its host, after what that
/// batch was told to drop, which frees the place:
///
/// - one that no batch names;
/// - one that a batch after that one drops.
///
/// With the first batch:
///
/// - one that a batch before the host's put drops, which finds it;
/// - one under an element of which another subtree of the group hangs
///   that the first batch writes into, directly or two levels down;
/// - one under an element of which the first batch enters a subtree
///   itself;
/// - one that a batch holds a write for: where its place is taken until
///   a later batch drops what holds it, the group is refused.
///
/// A batch that moves a subtree needs the one it moves away, and not
/// the prefix it moves it to; it holds a statement of both.
#[test]
fn a_subtree_given_with_a_group_is_entered_with_the_batch_that_its_group_needs_it_in() {
    /// A settled batch of `transaction` that puts the aux record `key`
    /// of the subtree at `at`.
    fn record(transaction: &WriteTxn<'_>, at: &LigninSubtree, key: &[u8]) -> LigninSettled {
        let batch = LigninBatch::new();
        let handle = LigninNodeStore::deferred(transaction, at.clone(), Some(&batch));
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        batch.settle(transaction).unwrap().expect("a settled batch")
    }
    /// A settled batch of `transaction` that puts the node `key` of the
    /// Merk at `at`.
    fn node(transaction: &WriteTxn<'_>, at: &LigninSubtree, key: u64, salt: u64) -> LigninSettled {
        let batch = LigninBatch::new();
        let handle = LigninNodeStore::deferred(transaction, at.clone(), Some(&batch));
        let mut merk = open(handle, TreeType::NormalTree);
        apply(&mut merk, &puts(key..key + 1, salt));
        drop(merk);
        batch.settle(transaction).unwrap().expect("a settled batch")
    }
    /// A settled batch of `transaction` that drops the subtree at `at`.
    fn drops(transaction: &WriteTxn<'_>, at: &LigninSubtree) -> LigninSettled {
        let batch = LigninBatch::new();
        batch.drop_subtree(at);
        batch.settle(transaction).unwrap().expect("a settled batch")
    }
    fn under(prefix: [u8; 32], above: [u8; 32], key: u64) -> LigninSubtree {
        let host = LigninHost {
            subtree: above,
            key: plain(key),
            hosted: vec![prefix],
        };
        LigninSubtree {
            prefix,
            host: Some(host),
        }
    }
    fn entered(transaction: &WriteTxn<'_>, at: &LigninSubtree) -> bool {
        let reader = LigninNodeStore::read_only(transaction, at.clone());
        reader.entry().expect("the directory answers").is_some()
    }

    let facade = Facade::new();
    let tree_type = tree_type_bytes(TreeType::NormalTree);
    let given = |subtrees: &[&LigninSubtree]| -> Vec<(LigninSubtree, Vec<u8>)> {
        let given = subtrees.iter().map(|subtree| (*subtree).clone());
        given.map(|subtree| (subtree, tree_type.clone())).collect()
    };
    let parent = LigninSubtree::alone([0; 32]);
    let beside = LigninSubtree::alone([9; 32]);
    // Three subtrees that hold the first place under a key of the
    // parent each.
    let old = [1, 2, 7].map(|key| under([20 + key as u8; 32], parent.prefix, key));
    let setup = facade.db.begin();
    for alone in [&parent, &beside] {
        LigninNodeStore::immediate(&setup, alone.clone())
            .enter(TreeType::NormalTree)
            .expect("the subtree is entered");
    }
    let batch = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&setup, parent.clone(), Some(&batch));
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &puts(1..9, 1));
    drop(merk);
    let mut made = vec![batch.settle(&setup).unwrap().expect("a settled batch")];
    made.extend(old.iter().map(|old| record(&setup, old, b"o")));
    let applied = LigninSettled::apply(made, &setup, &given(&[&old[0], &old[1], &old[2]]));
    applied
        .unwrap()
        .expect("the parent and a subtree under three of its keys");
    facade.commit(&setup);
    let transaction = facade.db.begin();

    // Named by no batch: with the batch that puts the node of its host.
    let new = under([8; 32], parent.prefix, 2);
    let first = record(&transaction, &beside, b"1");
    let mut second = node(&transaction, &parent, 2, 2);
    second.drop_subtree(&old[1].prefix);
    let applied = LigninSettled::apply(vec![first, second], &transaction, &given(&[&new]));
    applied
        .unwrap()
        .expect("the place is free where the subtree is entered");
    assert!(entered(&transaction, &new) && !entered(&transaction, &old[1]));

    // Dropped by a batch after the one that puts the node of its host:
    // with that one all the same.
    let late = under([13; 32], parent.prefix, 1);
    let first = record(&transaction, &beside, b"2");
    let mut second = node(&transaction, &parent, 1, 2);
    second.drop_subtree(&old[0].prefix);
    let third = drops(&transaction, &late);
    let applied = LigninSettled::apply(vec![first, second, third], &transaction, &given(&[&late]));
    applied
        .unwrap()
        .expect("the place is free where the subtree is entered");
    assert!(!entered(&transaction, &late) && !entered(&transaction, &old[0]));

    // Dropped by a batch before the one that puts the node of its host:
    // with the first batch.
    let dropped = under([10; 32], parent.prefix, 3);
    let first = record(&transaction, &beside, b"3");
    let second = drops(&transaction, &dropped);
    let third = node(&transaction, &parent, 3, 2);
    let parts = vec![first, second, third];
    let applied = LigninSettled::apply(parts, &transaction, &given(&[&dropped]));
    applied
        .unwrap()
        .expect("the drop finds the subtree entered");
    assert!(!entered(&transaction, &dropped));

    // Another subtree of the group hangs under an element of it, and
    // the first batch writes into that one.
    let outer = under([11; 32], parent.prefix, 4);
    let inner = under([12; 32], outer.prefix, 1);
    let first = record(&transaction, &inner, b"4");
    let second = node(&transaction, &parent, 4, 2);
    let enters = given(&[&outer, &inner]);
    let applied = LigninSettled::apply(vec![first, second], &transaction, &enters);
    applied
        .unwrap()
        .expect("the first batch writes into a subtree that is entered");
    assert!(entered(&transaction, &outer) && entered(&transaction, &inner));
    let reader = LigninNodeStore::read_only(&transaction, inner);
    let read = reader.aux().get(b"4").unwrap().expect("a read");
    assert_eq!(read, Some(b"v".to_vec()));
    drop(reader);

    // The same two levels down.
    let top = under([14; 32], parent.prefix, 5);
    let mid = under([15; 32], top.prefix, 1);
    let leaf = under([16; 32], mid.prefix, 1);
    let first = record(&transaction, &leaf, b"5");
    let second = node(&transaction, &parent, 5, 2);
    let enters = given(&[&top, &mid, &leaf]);
    let applied = LigninSettled::apply(vec![first, second], &transaction, &enters);
    applied
        .unwrap()
        .expect("the first batch writes into a subtree that is entered");
    assert!([&top, &mid, &leaf]
        .into_iter()
        .all(|at| entered(&transaction, at)));

    // The first batch enters a subtree under an element of it itself.
    let above = under([17; 32], parent.prefix, 6);
    let own = under([18; 32], above.prefix, 1);
    let batch = LigninBatch::new();
    batch.enter(&own, &tree_type).expect("an enter");
    let first = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert!(first.enters(&own.prefix));
    let second = node(&transaction, &parent, 6, 2);
    let applied = LigninSettled::apply(vec![first, second], &transaction, &given(&[&above]));
    applied
        .unwrap()
        .expect("the first batch enters a subtree under one that is entered");
    assert!(entered(&transaction, &above) && entered(&transaction, &own));

    // A batch holds a write for it: with the first batch, where its
    // place is still taken.
    let written = under([19; 32], parent.prefix, 7);
    let first = record(&transaction, &beside, b"7");
    let mut second = node(&transaction, &parent, 7, 2);
    second.drop_subtree(&old[2].prefix);
    let third = record(&transaction, &written, b"w");
    let parts = vec![first, second, third];
    let refused = LigninSettled::apply(parts, &transaction, &given(&[&written]));
    assert!(refused.unwrap().is_err());
    assert!(!entered(&transaction, &written) && entered(&transaction, &old[2]));

    // What a batch that moves a subtree says: it needs the subtree
    // that it moves away, and not the prefix that the subtree goes to,
    // where the move brings the entry; and it holds a statement of
    // both, so that no enter of either is put off to it.
    let batch = LigninBatch::new();
    batch.move_subtree(&[30; 32], &[31; 32]);
    let moving = batch
        .settle(&transaction)
        .unwrap()
        .expect("a settled batch");
    assert!(moving.states(&[30; 32]) && moving.needs(&[30; 32]));
    assert!(moving.states(&[31; 32]) && !moving.needs(&[31; 32]));
}

/// The keys a cursor shows from the first node to the last, each with a
/// record under it.
fn keys_under(cursor: &mut crate::store::lignin::LigninCursor<'_>) -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        keys.push(cursor.key().unwrap().expect("a key").to_vec());
        assert!(cursor.value().unwrap().is_some(), "a record under the key");
        cursor.next().unwrap();
    }
    cursor.status().expect("the walk came to its end");
    keys
}

const BESIDE: LigninSubtree = LigninSubtree::alone([5; 32]);

/// Writes `batch` into the Merk of `subtree` through a deferred handle on
/// `view`, and applies the batch it staged.
fn write_through<'v>(
    view: LigninView<'v>,
    subtree: &LigninSubtree,
    batch: &[(Vec<u8>, Op)],
    enter: bool,
) {
    let staged = LigninBatch::new();
    let handle = LigninNodeStore::deferred(view, subtree.clone(), Some(&staged));
    if enter {
        handle.enter(TreeType::NormalTree).expect("an enter");
    }
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, batch);
    drop(merk);
    staged.apply(view).unwrap().expect("the batch is applied");
}

/// A cursor of a write transaction is pointed at another subtree
/// (`LigninCursor::retarget`), on a handle of the transaction and on a
/// handle of a writer: it shows that subtree as the committed state had it
/// when the cursor was opened, with the transaction's own writes, and
/// stands on no node until it is sought.
fn pointed_elsewhere<'v>(facade: &Facade, view: LigninView<'v>, round: u64) {
    let keys = |range: std::ops::Range<u64>| -> Vec<Vec<u8>> { range.map(plain).collect() };
    let at_open = (keys(1..20 + round), keys(100..130 + round));
    let handle = LigninNodeStore::read_only(view, ROOT);
    let mut cursor = NodeStore::cursor(&handle);
    assert_eq!(keys_under(&mut cursor), at_open.0);

    // A commit outside the transaction writes both Merks.
    let outside = facade.db.begin();
    write_through(
        (&outside).into(),
        &ROOT,
        &puts(20 + round..21 + round, 9),
        false,
    );
    write_through(
        (&outside).into(),
        &BESIDE,
        &puts(130 + round..131 + round, 9),
        false,
    );
    facade.commit(&outside);

    // Pointed at the other Merk, the cursor stands on no node, and a step
    // goes nowhere. Sought, it shows the Merk of the state it was opened
    // on. A cursor that is opened now shows the commit.
    cursor.seek(plain(5)).unwrap();
    let mut cursor = cursor.retarget(&BESIDE.prefix);
    assert!(!cursor.valid().unwrap());
    assert_eq!(cursor.key().unwrap(), None);
    assert_eq!(cursor.value().unwrap(), None);
    cursor.next().unwrap();
    cursor.prev().unwrap();
    assert!(!cursor.valid().unwrap());
    cursor.status().expect("no node, and no error");
    assert_eq!(keys_under(&mut cursor), at_open.1);
    cursor.seek_for_prev(plain(104)).unwrap();
    cursor.prev().unwrap();
    assert_eq!(cursor.key().unwrap(), Some(plain(103).as_slice()));
    let later = LigninNodeStore::read_only(view, BESIDE);
    assert_eq!(
        keys_under(&mut NodeStore::cursor(&later)),
        keys(100..131 + round)
    );

    // At a subtree that is not entered it shows nothing, and back at the
    // first Merk what it showed before.
    let mut cursor = cursor.retarget(&[9; 32]);
    assert_eq!(keys_under(&mut cursor), Vec::<Vec<u8>>::new());
    let mut cursor = cursor.retarget(&ROOT.prefix);
    assert_eq!(keys_under(&mut cursor), at_open.0);

    // The transaction writes the other Merk: a node the cursor's state
    // does not have, and the removal of one it has. Pointed there, the
    // cursor shows both, and nothing of it in the first Merk.
    let mut own = puts(50..51, 3);
    own.push((plain(110), Op::Delete));
    write_through(view, &BESIDE, &own, false);
    let mut cursor = cursor.retarget(&BESIDE.prefix);
    let mut expected = keys(50..51);
    expected.extend(at_open.1.iter().filter(|key| **key != plain(110)).cloned());
    assert_eq!(keys_under(&mut cursor), expected);
    let mut cursor = cursor.retarget(&ROOT.prefix);
    assert_eq!(keys_under(&mut cursor), at_open.0);
}

#[test]
fn a_cursor_of_a_write_transaction_is_pointed_at_another_subtree_in_the_state_it_was_opened_on() {
    let facade = Facade::new();
    let setup = facade.db.begin();
    write_through((&setup).into(), &ROOT, &puts(1..20, 1), true);
    write_through((&setup).into(), &BESIDE, &puts(100..130, 1), true);
    facade.commit(&setup);

    // A handle of the transaction, whose cursor is the transaction's.
    let transaction = facade.db.begin();
    pointed_elsewhere(&facade, (&transaction).into(), 0);
    drop(transaction);

    // A handle of a writer, whose cursor is parked between two calls.
    let transaction = facade.db.begin();
    let writer = LigninWriter::new(transaction.scope().expect("a scope"));
    pointed_elsewhere(&facade, (&writer).into(), 1);
    writer.into_scope().rollback();
    drop(transaction);
    facade.sound();
}

/// The subtrees that are given with a group are entered if they are
/// absent (`SubtreeWrite::EnterIfAbsent`). One that a commit of another
/// transaction entered, before the group was made, before it was applied
/// or after it, is found and keeps what it holds, and a value that the
/// group states and such a subtree holds already is left where it is.
#[test]
fn a_subtree_given_with_a_group_is_entered_only_where_it_is_absent() {
    /// Writes `ops` into the Merk at `at` through one batch of
    /// `transaction`, applied with the subtree to be entered, and says how
    /// many values were written and how many were kept.
    fn write(
        transaction: &WriteTxn<'_>,
        at: &LigninSubtree,
        ops: &[(Vec<u8>, Op)],
    ) -> (usize, usize) {
        let batch = LigninBatch::new();
        let handle = LigninNodeStore::deferred(transaction, at.clone(), Some(&batch));
        let mut merk = open(handle, TreeType::NormalTree);
        apply(&mut merk, ops);
        drop(merk);
        let part = batch.settle(transaction).unwrap().expect("a settled batch");
        let enters = [(at.clone(), tree_type_bytes(TreeType::NormalTree))];
        let applied = LigninSettled::apply(vec![part], transaction, &enters);
        let applied = applied.unwrap().expect("the group is applied");
        (applied.values_written, applied.values_kept)
    }
    /// A settled batch of `transaction` that puts the aux record `key`
    /// of the subtree at `at`.
    fn record(transaction: &WriteTxn<'_>, at: &LigninSubtree, key: &[u8]) -> LigninSettled {
        let batch = LigninBatch::new();
        let handle = LigninNodeStore::deferred(transaction, at.clone(), Some(&batch));
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        batch.settle(transaction).unwrap().expect("a settled batch")
    }

    let facade = Facade::new();
    let merk_at = LigninSubtree::alone([12; 32]);
    let first = facade.db.begin();
    assert_eq!(write(&first, &merk_at, &puts(1..4, 1)), (3, 0));
    facade.commit(&first);
    // The subtree is entered now, and is given again: it keeps its nodes,
    // and of the values that are stated the one that changed is written.
    let second = facade.db.begin();
    let mut again = puts(1..3, 1);
    again.extend(puts(3..4, 2));
    assert_eq!(write(&second, &merk_at, &again), (1, 2));
    facade.commit(&second);
    let reader = facade.db.begin();
    let handle = LigninNodeStore::read_only(&reader, merk_at.clone());
    for key in 1..4 {
        assert!(handle.has_node(&plain(key)).unwrap().expect("a read"));
    }
    drop(handle);
    drop(reader);

    // Three transactions look while a subtree is not entered, and each
    // has a record of its own for it. One commits. Of the others, one had
    // applied its group before that commit and one applies it afterwards:
    // both find the subtree that the first entered, and commit.
    let flat_at = LigninSubtree::alone([13; 32]);
    let enters = [(flat_at.clone(), Vec::new())];
    let (third, fourth, fifth) = (facade.db.begin(), facade.db.begin(), facade.db.begin());
    let three = record(&third, &flat_at, b"3");
    let four = record(&fourth, &flat_at, b"4");
    let five = record(&fifth, &flat_at, b"5");
    let applied = LigninSettled::apply(vec![four], &fourth, &enters);
    applied.unwrap().expect("a group that enters the subtree");
    let applied = LigninSettled::apply(vec![three], &third, &enters);
    applied.unwrap().expect("a group that enters the subtree");
    facade.commit(&third);
    let applied = LigninSettled::apply(vec![five], &fifth, &enters);
    applied.unwrap().expect("a group that finds the subtree");
    facade.commit(&fourth);
    facade.commit(&fifth);
    let reader = facade.db.begin();
    let handle = LigninNodeStore::read_only(&reader, flat_at);
    for key in [b"3", b"4", b"5"] {
        let read = handle.aux().get(key).unwrap().expect("a read");
        assert_eq!(read, Some(b"v".to_vec()));
    }
    drop(handle);
    drop(reader);

    // A group that the transaction refuses for the entry of a subtree
    // says which, and is left to its caller: a record of a subtree that
    // is not entered and that no enter is given for, and an enter of a
    // subtree that is entered as a tree of another type.
    let absent_at = LigninSubtree::alone([14; 32]);
    let sixth = facade.db.begin();
    let mut parts = vec![record(&sixth, &absent_at, b"6")];
    let refused = LigninSettled::apply_kept(&mut parts, &sixth, &[]);
    let refused = refused.unwrap().expect_err("the subtree is not entered");
    let not_entered = LigninEntryRefused::NotEntered([14; 32]);
    assert_eq!((refused.entry, parts.len()), (Some(not_entered), 1));
    let as_sum_tree = [(merk_at.clone(), tree_type_bytes(TreeType::SumTree))];
    let mut other = vec![record(&sixth, &merk_at, b"6")];
    let refused = LigninSettled::apply_kept(&mut other, &sixth, &as_sum_tree);
    let refused = refused.unwrap().expect_err("the subtree is a normal tree");
    let otherwise = LigninEntryRefused::EnteredOtherwise([12; 32]);
    assert_eq!((refused.entry, other.len()), (Some(otherwise), 1));
    // Another refusal says none, also one that is of an entry: an enter
    // of a subtree as one that is new, where the subtree is entered.
    let batch = LigninBatch::new();
    (batch.enter(&merk_at, &tree_type_bytes(TreeType::NormalTree))).expect("staging");
    let mut entered = vec![batch.settle(&sixth).unwrap().expect("a settled batch")];
    assert!(entered[0].writes_directory());
    let refused = LigninSettled::apply_kept(&mut entered, &sixth, &[]);
    let refused = refused.unwrap().expect_err("the subtree is entered");
    assert_eq!((refused.entry, entered.len()), (None, 1));
    // A batch says whether it enters, drops or clears a subtree itself.
    let batch = LigninBatch::new();
    batch.drop_subtree(&merk_at);
    let drops = batch.settle(&sixth).unwrap().expect("a settled batch");
    assert!(drops.writes_directory() && !parts[0].writes_directory());
    // The transaction holds no write of either, and takes the first
    // with the enter it lacked.
    let enters = [(absent_at.clone(), Vec::new())];
    let applied = LigninSettled::apply_kept(&mut parts, &sixth, &enters);
    applied.unwrap().expect("the group is applied");
    assert!(parts.is_empty());
    facade.commit(&sixth);
    let reader = facade.db.begin();
    let handle = LigninNodeStore::read_only(&reader, absent_at);
    let read = handle.aux().get(b"6").unwrap().expect("a read");
    assert_eq!(read, Some(b"v".to_vec()));
    drop(handle);
    let handle = LigninNodeStore::read_only(&reader, merk_at);
    assert_eq!(handle.aux().get(b"6").unwrap().expect("a read"), None);
    drop(handle);
    drop(reader);
    facade.sound();
}

/// A subtree that whoever applies a batch says is not under a node keeps
/// what it holds: a value that the batch puts again as it is is kept, and
/// counted so, as without the statement.
#[test]
fn a_subtree_that_is_said_not_to_be_under_a_node_keeps_its_values() {
    let facade = Facade::new();
    let transaction = facade.db.begin();
    let beside = LigninSubtree::alone([9; 32]);
    LigninNodeStore::immediate(&transaction, beside.clone())
        .enter(TreeType::NormalTree)
        .expect("the subtree is entered");
    let mut merk = open(
        LigninNodeStore::immediate(&transaction, beside.clone()),
        TreeType::NormalTree,
    );
    apply(&mut merk, &puts(5..7, 1));
    drop(merk);
    let again = |stated: bool| {
        let batch = LigninBatch::new();
        let handle = LigninNodeStore::deferred(&transaction, beside.clone(), Some(&batch));
        let mut merk = open(handle, TreeType::NormalTree);
        apply(&mut merk, &puts(5..7, 1));
        drop(merk);
        let settled = batch.settle(&transaction).unwrap();
        let mut settled = settled.expect("a settled batch");
        if stated {
            settled.not_under(&beside.prefix, &[0; 32], b"node");
        }
        let applied = LigninSettled::apply(vec![settled], &transaction, &[]).unwrap();
        let Ok(applied) = applied else {
            panic!("the batch is refused");
        };
        (applied.values_written, applied.values_kept)
    };
    assert_eq!(again(false), (0, 2));
    assert_eq!(again(true), (0, 2));

    // Said of one subtree with two nodes, both are handed over: the
    // transaction refuses a batch that names a subtree so twice.
    let batch = LigninBatch::new();
    let handle = LigninNodeStore::deferred(&transaction, beside.clone(), Some(&batch));
    let mut merk = open(handle, TreeType::NormalTree);
    apply(&mut merk, &puts(5..7, 2));
    drop(merk);
    let settled = batch.settle(&transaction).unwrap();
    let mut settled = settled.expect("a settled batch");
    settled.not_under(&beside.prefix, &[0; 32], b"node");
    settled.not_under(&beside.prefix, &[0; 32], b"node");
    settled.not_under(&beside.prefix, &[0; 32], b"other");
    let refused = LigninSettled::apply(vec![settled], &transaction, &[]).unwrap();
    let said = match refused {
        Ok(_) => panic!("the batch is taken"),
        Err(refused) => refused.to_string(),
    };
    assert!(said.contains("twice"), "{said}");
}
