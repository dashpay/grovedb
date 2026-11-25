//! GroveDB on the LigninDB engine, and the engine at the seam.
//!
//! - `cases`: the cases that `transactions.md` records of the RocksDB
//!   engine, run on LigninDB and, where the RocksDB engine is compiled in,
//!   on RocksDB by the same code.
//! - here: the acceptance tests of the engine (a grove that is opened,
//!   written, read and opened again; the trace of section 14.3), the
//!   same history on both engines with every cost and root hash compared,
//!   and what the seam asks of an engine, on LigninDB.
//!
//! The tests that call `check` close the database they leave, open it
//! again and have the physical checker of the engine read every page of
//! it, with the stored hashes of the nodes of every Merk in use checked
//! by Merk's rules for a check (`MerkHashRules::for_check`): each is made
//! again and compared, but for the value hashes that those rules take as
//! they are stored. In a debug build every database of these tests is
//! checked so when it is closed, by the database itself.

mod append_only;
mod axes;
mod batches;
mod both_kinds;
mod cases;
mod checkpoint;
mod deletes;
mod full_pages;
#[cfg(feature = "unsafe-dump-load")]
mod hatch;
mod hosted;
mod indexed;
mod inspection;
mod kinds;
mod left_behind;
mod legacy;
#[cfg(feature = "rocksdb_storage")]
mod made_again;
mod many_drops;
mod many_moves;
mod many_trees;
#[cfg(target_os = "linux")]
mod measured;
mod moves;
mod nested;
mod over;
mod plain_delete;
mod queries;
#[cfg(feature = "rocksdb_storage")]
mod refused;
mod restore;
mod root_keys;
mod scopes;
mod side_by_side;
mod sync;

use grovedb_costs::OperationCost;
use grovedb_merk::store::{lignin::hash::MerkHashRules, NodeStore};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    error::{CommitError, WriteScopeError},
    flat::{pending_prefix_drops_namespace, PendingPrefixDrops},
    prefix::build_prefix,
    Error as StorageError, FlatStore, RawIterator,
};
use grovedb_version::version::GroveVersion;
use lignindb::{txn::Subtree, Db, Options};

use self::cases::{Grove, A, B};
use super::{error, LigninEngine};
use crate::{
    engine::{Engine, SubtreeHandle, SubtreeMove, WriteGroup},
    Element, Error, GroveDb, TransactionArg,
};

type Lignin = GroveDb<LigninEngine>;

fn version() -> &'static GroveVersion {
    GroveVersion::latest()
}

const ROOT: SubtreePath<'static, [u8; 0]> = SubtreePath::empty();

/// The path of the segments given, as an operation of GroveDB takes one.
macro_rules! path {
    ($($segment:expr),+) => {
        [$(&$segment[..]),+].as_ref()
    };
}

/// Opens the database in `dir` again, which nothing has open, closes it,
/// and has the physical checker read it as a store that is still: every
/// rule of the layout, the data column of every subtree by the tree type
/// of its entry, and the stored hashes of the nodes of every Merk in use
/// by Merk's rules, as the module documentation says.
fn check(dir: &std::path::Path) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    db.check_kinds_by(grovedb_merk::store::lignin::keeps_flat_records);
    let mut rules = MerkHashRules::for_check(version());
    let report = db.close_and_verify(Some(&mut rules)).expect("a close");
    assert!(report.is_sound(), "{:#?}", report.findings);
}

fn compare(name: &str, seen: Vec<(&'static str, String)>, expected: &[(&str, &str)]) {
    let names: Vec<&str> = seen.iter().map(|(case, _)| *case).collect();
    let listed: Vec<&str> = expected.iter().map(|(case, _)| *case).collect();
    assert_eq!(names, listed, "the cases that ran on {name}");
    for ((case, seen), (_, expected)) in seen.iter().zip(expected) {
        assert_eq!(seen, expected, "case {case} on {name}");
    }
}

#[test]
fn the_recorded_raw_cases_give_their_results_on_lignindb() {
    compare("LigninDB", cases::raw_cases::<LigninEngine>(), cases::RAW);
}

#[test]
fn the_recorded_grovedb_cases_give_their_results_on_lignindb() {
    compare(
        "LigninDB",
        cases::grove_cases::<LigninEngine>(),
        cases::GROVE,
    );
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn the_recorded_raw_cases_give_their_results_on_rocksdb() {
    let seen = cases::raw_cases::<crate::RocksDbEngine>();
    compare("RocksDB", seen, cases::RAW);
}

#[cfg(feature = "rocksdb_storage")]
#[test]
fn the_recorded_grovedb_cases_give_their_results_on_rocksdb() {
    let seen = cases::grove_cases::<crate::RocksDbEngine>();
    compare("RocksDB", seen, cases::GROVE);
}

/// The root hash of a grove that holds exactly `items`, each in the tree
/// of its first letter, written with no transaction.
fn root_of<E: Engine>(items: &[&[u8]]) -> [u8; 32] {
    let grove = Grove::<E>::new();
    for item in items {
        let tree = if item[0] == b'a' { A } else { B };
        grove.insert(tree, item, None);
    }
    grove
        .db
        .root_hash(None, version())
        .unwrap()
        .expect("a root")
}

/// The seven steps of `transactions.md` 14.3, with the shape of mainnet
/// heights 32326 to 32328: a write with no transaction lands while a write
/// transaction is open and has written. What each step observed, and what
/// each call was charged.
fn mainnet_trace<E: Engine>() -> (Vec<String>, Vec<OperationCost>, Vec<[u8; 32]>) {
    fn root<'db, E: Engine>(
        grove: &'db Grove<E>,
        through: TransactionArg<'db, '_, E>,
        costs: &mut Vec<OperationCost>,
        roots: &mut Vec<[u8; 32]>,
    ) -> [u8; 32] {
        let read = grove.db.root_hash(through, version());
        costs.push(read.cost.clone());
        let root = read.unwrap().expect("a root");
        roots.push(root);
        root
    }
    fn insert<'db, E: Engine>(
        grove: &'db Grove<E>,
        tree: &[u8],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        costs: &mut Vec<OperationCost>,
    ) {
        let item = Element::new_item(key.to_vec());
        let inserted = (grove.db).insert([tree].as_ref(), key, item, None, through, version());
        costs.push(inserted.cost.clone());
        inserted.unwrap().expect("an insert");
    }
    let h = |items: &[&[u8]]| root_of::<E>(items);
    let grove = Grove::<E>::new();
    let mut seen = Vec::new();
    let mut costs = Vec::new();
    let mut roots = Vec::new();
    let t = grove.db.start_transaction();

    // 1. T inserts A/a1.
    insert(&grove, A, b"a1", Some(&t), &mut costs);
    let own = root(&grove, Some(&t), &mut costs, &mut roots) == h(&[b"a1"]);
    seen.push(format!("1 {own}"));
    // 2. A write with no transaction inserts B/b1: it commits at once.
    insert(&grove, B, b"b1", None, &mut costs);
    let none = root(&grove, None, &mut costs, &mut roots) == h(&[b"b1"]);
    // T reads its own root node, which carries the old hash of B.
    let own = root(&grove, Some(&t), &mut costs, &mut roots) == h(&[b"a1"]);
    seen.push(format!("2 {none} {own}"));
    // 3. T reads B/b1: it never wrote that node.
    let read = (grove.db).get([B].as_ref(), b"b1", Some(&t), version());
    costs.push(read.cost.clone());
    let read = read.unwrap().expect("a read through the open transaction");
    seen.push(format!("3 {}", read == Element::new_item(b"b1".to_vec())));
    // 4. T inserts A/a2: still without b1.
    insert(&grove, A, b"a2", Some(&t), &mut costs);
    let own = root(&grove, Some(&t), &mut costs, &mut roots) == h(&[b"a1", b"a2"]);
    seen.push(format!("4 {own}"));
    // 5. T inserts B/b2: this write loaded B as committed.
    insert(&grove, B, b"b2", Some(&t), &mut costs);
    let own = root(&grove, Some(&t), &mut costs, &mut roots) == h(&[b"a1", b"a2", b"b1", b"b2"]);
    seen.push(format!("5 {own}"));
    // 6. T commits: a conflict.
    let committed = grove.db.commit_transaction(t);
    costs.push(committed.cost.clone());
    let conflict = committed.unwrap().is_err_and(|error| {
        error.commit_error() == Some(CommitError::Conflict) && error.is_commit_conflict()
    });
    seen.push(format!("6 {conflict}"));
    // 7. The database keeps the write with no transaction and nothing of
    // T's.
    let none = root(&grove, None, &mut costs, &mut roots) == h(&[b"b1"]);
    seen.push(format!("7 {none}"));
    (seen, costs, roots)
}

const MAINNET_TRACE: [&str; 7] = [
    "1 true",
    "2 true true",
    "3 true",
    "4 true",
    "5 true",
    "6 true",
    "7 true",
];

#[test]
fn a_write_with_no_transaction_beside_an_open_one_behaves_as_on_rocksdb() {
    let (seen, costs, roots) = mainnet_trace::<LigninEngine>();
    assert_eq!(seen, MAINNET_TRACE);
    assert_eq!((costs.len(), roots.len()), (12, 6));
    #[cfg(feature = "rocksdb_storage")]
    {
        let (on_rocksdb, rocksdb_costs, rocksdb_roots) = mainnet_trace::<crate::RocksDbEngine>();
        assert_eq!(on_rocksdb, MAINNET_TRACE);
        // Every root that was read, whole, is the root RocksDB read there.
        assert_eq!(roots, rocksdb_roots);
        for (step, (lignin, rocksdb)) in costs.iter().zip(&rocksdb_costs).enumerate() {
            assert_eq!(lignin, rocksdb, "the cost of call {step}");
        }
        // The roots that the two engines compared with are the same roots.
        let items: &[&[u8]] = &[b"a1", b"a2", b"b1", b"b2"];
        assert_eq!(
            root_of::<LigninEngine>(items),
            root_of::<crate::RocksDbEngine>(items)
        );
    }
}

/// One call of a history, with what it answered: its cost, and what it
/// returned or how it failed, as text.
type Answer = (&'static str, OperationCost, String);

/// A history of GroveDB's writes and reads on the engine `E`: trees of
/// several kinds four deep, items, sum items and references, replaced and
/// deleted items, aux records, conditional inserts, a transaction that is
/// rolled back to a savepoint and one that is committed. Every answer is
/// returned, with the root hash after every write.
fn history<E: Engine>(db: &GroveDb<E>) -> Vec<Answer> {
    let v = version();
    let mut answers: Vec<Answer> = Vec::new();
    macro_rules! record {
        ($name:literal, $call:expr) => {{
            let answer = $call;
            let cost = answer.cost.clone();
            let value = answer.unwrap();
            answers.push(($name, cost, format!("{value:?}")));
            let root = db.root_hash(None, v);
            answers.push(("root", root.cost.clone(), format!("{:?}", root.unwrap())));
        }};
    }
    let item = |n: usize| Element::new_item(vec![n as u8; 3 + n * 7]);

    record!(
        "tree",
        db.insert(ROOT, b"docs", Element::empty_tree(), None, None, v)
    );
    record!(
        "sum tree",
        db.insert(ROOT, b"sums", Element::empty_sum_tree(), None, None, v)
    );
    record!(
        "count tree",
        db.insert(ROOT, b"counts", Element::empty_count_tree(), None, None, v)
    );
    record!(
        "nested",
        db.insert(
            path![b"docs"],
            b"contract",
            Element::empty_tree(),
            None,
            None,
            v
        )
    );
    record!(
        "nested again",
        db.insert(
            path![b"docs", b"contract"],
            b"type",
            Element::empty_tree(),
            None,
            None,
            v
        )
    );
    for n in 0..40 {
        let key = format!("doc{n:02}").into_bytes();
        record!(
            "item",
            db.insert(
                path![b"docs", b"contract", b"type"],
                &key,
                item(n),
                None,
                None,
                v
            )
        );
    }
    for n in 0..12i64 {
        let key = format!("s{n:02}").into_bytes();
        record!(
            "sum item",
            db.insert(
                path![b"sums"],
                &key,
                Element::new_sum_item(n * 3 - 7),
                None,
                None,
                v
            )
        );
        record!(
            "counted item",
            db.insert(path![b"counts"], &key, item(n as usize), None, None, v)
        );
    }
    record!(
        "reference",
        db.insert(
            path![b"docs"],
            b"first",
            Element::new_reference(
                crate::reference_path::ReferencePathType::AbsolutePathReference(vec![
                    b"docs".to_vec(),
                    b"contract".to_vec(),
                    b"type".to_vec(),
                    b"doc00".to_vec(),
                ])
            ),
            None,
            None,
            v
        )
    );
    record!(
        "replace",
        db.insert(
            path![b"docs", b"contract", b"type"],
            b"doc07",
            item(31),
            None,
            None,
            v
        )
    );
    record!(
        "the same again",
        db.insert(
            path![b"docs", b"contract", b"type"],
            b"doc07",
            item(31),
            None,
            None,
            v
        )
    );
    for n in [3, 11, 12, 13, 29] {
        let key = format!("doc{n:02}").into_bytes();
        record!(
            "delete",
            db.delete(path![b"docs", b"contract", b"type"], &key, None, None, v)
        );
    }
    record!("aux", db.put_aux(b"height", b"32326", None, None));
    record!("aux again", db.put_aux(b"height", b"32327", None, None));
    record!("read aux", db.get_aux(b"height", None));
    record!("delete aux", db.delete_aux(b"height", None, None));
    record!("read no aux", db.get_aux(b"height", None));
    record!(
        "if not there",
        db.insert_if_not_exists(path![b"sums"], b"s03", Element::new_sum_item(1), None, v)
    );
    record!(
        "if not there, and it is not",
        db.insert_if_not_exists(path![b"sums"], b"s99", Element::new_sum_item(99), None, v)
    );
    record!(
        "read item",
        db.get(path![b"docs", b"contract", b"type"], b"doc20", None, v)
    );
    record!(
        "read through the reference",
        db.get(path![b"docs"], b"first", None, v)
    );
    record!("read a tree", db.get(path![b"docs"], b"contract", None, v));
    record!(
        "read what is not there",
        db.get(path![b"docs"], b"nothing", None, v)
    );
    record!(
        "read under what is not there",
        db.get(path![b"nowhere"], b"nothing", None, v)
    );
    record!("has", db.has_raw(path![b"sums"], b"s04", None, v));
    record!(
        "empty",
        db.is_empty_tree(path![b"docs", b"contract"], None, v)
    );
    record!(
        "empty, and it is",
        db.is_empty_tree(path![b"docs", b"contract", b"type", b"x"], None, v)
    );

    // A transaction that writes, rolls a part back and commits.
    let tx = db.start_transaction();
    record!(
        "in a transaction",
        db.insert(
            path![b"sums"],
            b"t1",
            Element::new_sum_item(5),
            None,
            Some(&tx),
            v
        )
    );
    db.set_savepoint(&tx).expect("a savepoint");
    record!(
        "after the savepoint",
        db.insert(
            path![b"sums"],
            b"t2",
            Element::new_sum_item(6),
            None,
            Some(&tx),
            v
        )
    );
    record!(
        "a tree after the savepoint",
        db.insert(
            path![b"docs"],
            b"undone",
            Element::empty_tree(),
            None,
            Some(&tx),
            v
        )
    );
    record!(
        "in the tree after the savepoint",
        db.insert(path![b"docs", b"undone"], b"x", item(2), None, Some(&tx), v)
    );
    db.rollback_to_savepoint(&tx).expect("a rollback");
    record!("its own write", db.get(path![b"sums"], b"t1", Some(&tx), v));
    record!(
        "what was rolled back",
        db.get(path![b"sums"], b"t2", Some(&tx), v)
    );
    let root = db.root_hash(Some(&tx), v);
    answers.push((
        "the transaction's root",
        root.cost.clone(),
        format!("{:?}", root.unwrap()),
    ));
    record!("commit", db.commit_transaction(tx));
    answers
}

#[test]
fn grovedb_opens_inserts_reads_and_reopens_on_lignindb() {
    let dir = tempfile::tempdir().expect("a directory");
    let v = version();
    let answers = {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        assert!(db.engine().database().created());
        let answers = history(&db);
        db.flush().expect("a flush");
        answers
    };
    let root = &answers.last().expect("answers").2;

    // The next session reads what the first one wrote.
    {
        let db = Lignin::open_with_engine(dir.path()).expect("the database opens again");
        assert!(!db.engine().database().created());
        let reopened = db.root_hash(None, v).unwrap().expect("a root");
        assert_eq!(&format!("{:?}", Ok::<_, Error>(reopened)), root);
        let read = db
            .get(
                [b"docs".as_slice(), b"contract", b"type"].as_ref(),
                b"doc20",
                None,
                v,
            )
            .unwrap()
            .expect("an item of the first session");
        assert_eq!(read, Element::new_item(vec![20; 3 + 20 * 7]));
        let sum = db
            .get(ROOT, b"sums", None, v)
            .unwrap()
            .expect("the sum tree");
        // Twelve sum items, `s03` unchanged, `s99`, and `t1` of the
        // transaction: 3 * 66 - 84 + 99 + 5.
        assert_eq!(sum.as_sum_tree_value().expect("a sum tree"), 218);
        let undone = db.get([b"docs".as_slice()].as_ref(), b"undone", None, v);
        assert!(matches!(undone.unwrap(), Err(Error::PathKeyNotFound(_))));
        // And writes on.
        db.insert(
            [b"docs".as_slice(), b"contract", b"type"].as_ref(),
            b"later",
            Element::new_item(b"second session".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("an insert in the second session");
        assert_ne!(db.root_hash(None, v).unwrap().expect("a root"), reopened);
    }
    check(dir.path());

    // The same history on RocksDB: every answer, every cost and every
    // root hash.
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path()).expect("RocksDB");
        let on_rocksdb = history(&db);
        assert_eq!(answers.len(), on_rocksdb.len());
        for (step, (lignin, rocksdb)) in answers.iter().zip(&on_rocksdb).enumerate() {
            assert_eq!(lignin, rocksdb, "call {step}");
        }
    }
}

#[test]
fn a_tree_that_was_never_written_into_is_read_as_empty_and_entered_by_its_first_write() {
    let dir = tempfile::tempdir().expect("a directory");
    let v = version();
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        db.insert(ROOT, b"empty", Element::empty_tree(), None, None, v)
            .unwrap()
            .expect("a tree");
        db.insert(ROOT, b"filled", Element::empty_sum_tree(), None, None, v)
            .unwrap()
            .expect("a tree");
        let engine = db.engine();
        let entry_of = |name: &[u8]| {
            let tx = engine.start_read_transaction();
            let handle = engine.subtree([name].as_ref().into(), None, &tx).unwrap();
            handle.entry().expect("the directory answers")
        };
        // Neither is in the subtree directory, and both read as empty.
        assert_eq!(entry_of(b"empty"), None);
        assert_eq!(entry_of(b"filled"), None);
        assert!(db
            .is_empty_tree([b"empty".as_slice()].as_ref(), None, v)
            .unwrap()
            .expect("an emptiness check"));
        // The first write enters the tree under its element, with the
        // element's tree type.
        db.insert(
            [b"filled".as_slice()].as_ref(),
            b"one",
            Element::new_sum_item(1),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("a sum item");
        let entry = entry_of(b"filled").expect("the tree is entered");
        assert_eq!(
            entry.tree_type().expect("a tree type"),
            grovedb_merk::TreeType::SumTree
        );
        let host = entry.host.expect("it hangs under its element");
        assert_eq!(
            (host.subtree, host.key.as_slice(), host.position),
            ([0; 32], b"filled".as_slice(), 0)
        );
        assert_eq!(entry.root_key.as_deref(), Some(b"one".as_slice()));
        assert_eq!(entry_of(b"empty"), None);
        // The root Merk was entered when the database was made, with no
        // host.
        let tx = engine.start_read_transaction();
        let root = engine.subtree(ROOT, None, &tx).unwrap();
        let root = root
            .entry()
            .expect("the directory answers")
            .expect("the root");
        assert_eq!(root.host, None);
        assert_eq!(
            root.tree_type().expect("a tree type"),
            grovedb_merk::TreeType::NormalTree
        );
    }
    check(dir.path());
}

#[test]
fn a_wipe_leaves_a_grove_that_is_written_again() {
    let v = version();
    let empty = {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        db.root_hash(None, v).unwrap().expect("a root")
    };
    let grove = Grove::<LigninEngine>::new();
    grove.insert(A, b"a1", None);
    (grove.db.put_aux(b"x", b"aux", None, None))
        .unwrap()
        .expect("an aux record");
    let before = grove.db.start_snapshot_read_transaction();
    let at = grove.db.engine().database().version();
    grove.db.wipe().expect("a wipe");
    // One commit, in which the two subtrees that every grove has are
    // entered again.
    let database = grove.db.engine().database();
    assert_eq!(Some(database.version()), at.next());
    for prefix in [[0; 32], *pending_prefix_drops_namespace()] {
        let snapshot = database.snapshot();
        let operation = snapshot.operation();
        let opened = operation.open(Subtree::new(prefix));
        assert!(opened.expect("a lookup").is_some(), "{prefix:?}");
    }
    // Nothing is left for a reader of the latest version, and a snapshot
    // from before keeps reading its own.
    assert_eq!(grove.db.root_hash(None, v).unwrap().expect("a root"), empty);
    assert_eq!(grove.db.get_aux(b"x", None).unwrap().expect("a read"), None);
    assert!(grove.finds(A, b"a1", Some(&before)));
    drop(before);
    (grove.db)
        .insert(ROOT, A, Element::empty_tree(), None, None, v)
        .unwrap()
        .expect("a tree after the wipe");
    grove.insert(A, b"a2", None);
    assert!(grove.finds(A, b"a2", None));
    assert!(!grove.finds(A, b"a1", None));
    let Grove { dir, db } = grove;
    drop(db);
    check(dir.path());
}

fn engine() -> (tempfile::TempDir, LigninEngine) {
    let dir = tempfile::tempdir().expect("a directory");
    let engine = LigninEngine::open(dir.path()).expect("an engine");
    (dir, engine)
}

#[test]
fn a_handle_by_path_is_charged_the_prefix_of_its_path_and_one_by_prefix_nothing() {
    let (_dir, engine) = engine();
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let path: &[&[u8]] = &[b"a", b"longer segment", b"c"];
    let prefix = build_prefix(SubtreePath::from(path));
    for batch in [None, Some(&batch)] {
        let by_path = engine.subtree(path.into(), batch, &tx);
        assert_eq!(by_path.cost, prefix.cost);
        assert_ne!(by_path.cost, OperationCost::default());
        assert_eq!(by_path.value.subtree().prefix, prefix.value);
        let by_prefix = engine.subtree_by_prefix(prefix.value, batch, &tx);
        assert_eq!(by_prefix.cost, OperationCost::default());
        assert_eq!(by_prefix.value.subtree().prefix, prefix.value);
    }
    let immediate = engine.immediate_subtree(path.into(), &tx);
    assert_eq!(immediate.cost, prefix.cost);
    let immediate = engine.immediate_subtree_by_prefix(prefix.value, &tx);
    assert_eq!(immediate.cost, OperationCost::default());
    // The root has the prefix of the empty path, which costs nothing.
    let root = engine.subtree(ROOT, None, &tx);
    assert_eq!(root.cost, OperationCost::default());
    assert_eq!(root.value.subtree().prefix, [0; 32]);
}

/// A group is applied through the engine that began the transaction.
/// Another open engine refuses it for a write transaction, for the view
/// of a write scope and for a snapshot, and leaves each as it was.
#[test]
fn a_write_group_is_refused_for_a_transaction_of_another_engine() {
    let (_other_dir, other) = engine();
    let (_dir, engine) = engine();
    let tx = engine.start_transaction();
    let group = || {
        let batch = <LigninEngine as Engine>::Batch::default();
        let staged = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
        (staged.aux().put(b"k", b"v", None).unwrap()).expect("a staged put");
        drop(staged);
        batch.next_operation();
        let mut group = <LigninEngine as Engine>::WriteGroup::default();
        (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        group
    };
    let read = |tx: &<LigninEngine as Engine>::Transaction<'_>| {
        let subtree = engine.subtree(ROOT, None, tx).unwrap();
        subtree.aux().get(b"k").unwrap().expect("a read")
    };
    let refused = |tx: &<LigninEngine as Engine>::Transaction<'_>| {
        let applied = other.apply_write_group(group(), tx);
        assert_eq!(applied.cost, OperationCost::default());
        match applied.value {
            Err(StorageError::StorageError(said)) => {
                assert!(said.contains("begun by another engine"), "{said}")
            }
            other => panic!("a refusal, and not {other:?}"),
        }
    };
    refused(&tx);
    assert_eq!(read(&tx), None);
    let scope = engine.open_write_scope(&tx).expect("a scope");
    refused(LigninEngine::scope_transaction(&scope));
    assert_eq!(read(LigninEngine::scope_transaction(&scope)), None);
    engine.merge_write_scope(scope).expect("a merge");
    let snapshot = engine.start_snapshot_read_transaction();
    refused(&snapshot);
    drop(snapshot);
    // The engine that began the transaction applies the same group.
    assert_eq!(read(&tx), None);
    (engine.apply_write_group(group(), &tx).unwrap()).expect("the group is applied");
    assert_eq!(read(&tx), Some(b"v".to_vec()));
}

#[test]
fn a_deferred_handle_stages_and_an_immediate_one_writes_at_once() {
    let (_dir, engine) = engine();
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let deferred = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
    let unbatched = engine.subtree(ROOT, None, &tx).unwrap();
    let immediate = engine.immediate_subtree(ROOT, &tx).unwrap();

    // With no batch a deferred handle refuses a write.
    assert!(unbatched.aux().put(b"k", b"v", None).unwrap().is_err());
    // A staged write is read by nobody until its batch is applied.
    deferred
        .aux()
        .put(b"k", b"staged", None)
        .unwrap()
        .expect("a staged put");
    deferred
        .meta()
        .put(b"k", b"meta", None)
        .unwrap()
        .expect("a staged put");
    assert_eq!(deferred.aux().get(b"k").unwrap().expect("a read"), None);
    assert_eq!(immediate.aux().get(b"k").unwrap().expect("a read"), None);
    // An immediate handle writes at once, and every handle of the
    // transaction reads it.
    immediate
        .aux()
        .put(b"now", b"1", None)
        .unwrap()
        .expect("a put");
    assert_eq!(
        deferred.aux().get(b"now").unwrap().expect("a read"),
        Some(b"1".to_vec())
    );
    immediate
        .aux()
        .delete(b"now", None)
        .unwrap()
        .expect("a delete");
    assert_eq!(unbatched.aux().get(b"now").unwrap().expect("a read"), None);
    drop((deferred, unbatched, immediate));

    // A group settles what is staged and keeps staging (the partial
    // batch): two parts, applied together.
    let mut group = <LigninEngine as Engine>::WriteGroup::default();
    (engine.settle_batch(&mut group, batch.take_pending()))
        .unwrap()
        .expect("a settled batch");
    let later = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
    later
        .aux()
        .put(b"k", b"later", None)
        .unwrap()
        .expect("a staged put");
    later
        .aux()
        .put(b"k2", b"second", None)
        .unwrap()
        .expect("a staged put");
    drop(later);
    batch.next_operation();
    engine
        .settle_batch(&mut group, batch)
        .unwrap()
        .expect("a settled batch");
    // Nothing of the group is read before it is applied, and its storage
    // cost is charged when it is.
    let unapplied = engine.subtree(ROOT, None, &tx).unwrap();
    assert_eq!(unapplied.aux().get(b"k").unwrap().expect("a read"), None);
    drop(unapplied);
    let pending = group.pending_cost().storage_cost.clone();
    let applied = engine.apply_write_group(group, &tx);
    assert_eq!(applied.cost.storage_cost, pending);
    applied.unwrap().expect("the group is applied");
    let read = engine.subtree(ROOT, None, &tx).unwrap();
    for (key, value) in [(b"k".as_slice(), b"later".as_slice()), (b"k2", b"second")] {
        assert_eq!(
            read.aux().get(key).unwrap().expect("a read"),
            Some(value.to_vec())
        );
    }
    assert_eq!(
        read.meta().get(b"k").unwrap().expect("a read"),
        Some(b"meta".to_vec())
    );
    // The data keyspace of a subtree that keeps no Merk is flat records.
    let flat = [5; 32];
    let batch = <LigninEngine as Engine>::Batch::default();
    let handle = engine.subtree_by_prefix(flat, Some(&batch), &tx).unwrap();
    FlatStore::put(&handle, b"b", b"2", None)
        .unwrap()
        .expect("a put");
    FlatStore::put(&handle, b"a", b"1", None)
        .unwrap()
        .expect("a put");
    FlatStore::put(&handle, b"c", b"3", None)
        .unwrap()
        .expect("a put");
    FlatStore::delete(&handle, b"c", None)
        .unwrap()
        .expect("a delete");
    drop(handle);
    engine.apply_batch(batch, &tx).unwrap().expect("a batch");
    let handle = engine.subtree_by_prefix(flat, None, &tx).unwrap();
    assert_eq!(
        FlatStore::get(&handle, b"a").unwrap().expect("a read"),
        Some(b"1".to_vec())
    );
    let mut cursor = FlatStore::cursor(&handle);
    let mut records = Vec::new();
    cursor.seek_to_last().unwrap();
    while cursor.valid().unwrap() {
        let key = cursor.key().unwrap().expect("a key").to_vec();
        records.push((key, cursor.value().unwrap().expect("a value").to_vec()));
        cursor.prev().unwrap();
    }
    cursor.status().expect("a cursor that walked");
    // A put stands against a delete of its key inside one operation.
    assert_eq!(
        records,
        [
            (b"c".to_vec(), b"3".to_vec()),
            (b"b".to_vec(), b"2".to_vec()),
            (b"a".to_vec(), b"1".to_vec())
        ]
    );
    cursor.seek(b"b").unwrap();
    assert_eq!(cursor.key().unwrap(), Some(b"b".as_slice()));
    cursor.seek_for_prev(b"bb").unwrap();
    assert_eq!(cursor.key().unwrap(), Some(b"b".as_slice()));
    cursor.next().unwrap();
    cursor.next().unwrap();
    assert!(!cursor.valid().unwrap());
    assert_eq!(cursor.value().unwrap(), None);
}

#[test]
fn a_snapshot_reads_one_version_and_refuses_every_write() {
    let (_dir, engine) = engine();
    let put = |key: &[u8], value: &[u8]| {
        let tx = engine.start_transaction();
        let root = engine.immediate_subtree(ROOT, &tx).unwrap();
        root.aux().put(key, value, None).unwrap().expect("a put");
        drop(root);
        engine.commit_transaction(tx).unwrap().expect("a commit");
    };
    put(b"k", b"before");
    for snapshot in [
        engine.start_snapshot_read_transaction(),
        engine.start_read_transaction(),
    ] {
        assert!(snapshot.is_snapshot_read());
        assert!(snapshot.with_write_transaction(|_| ()).is_none());
        assert_eq!(format!("{snapshot:?}"), "LigninTransaction(snapshot)");
        let root = engine.subtree(ROOT, None, &snapshot).unwrap();
        let aux = root.aux();
        let mut cursor = aux.cursor();
        put(b"k", b"after");
        put(b"later", b"1");
        assert_eq!(
            aux.get(b"k").unwrap().expect("a read"),
            Some(b"before".to_vec())
        );
        assert_eq!(aux.get(b"later").unwrap().expect("a read"), None);
        cursor.seek_to_first().unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"k".as_slice()));
        assert_eq!(cursor.value().unwrap(), Some(b"before".as_slice()));
        cursor.next().unwrap();
        assert!(!cursor.valid().unwrap());
        // A subtree that is not entered at the snapshot's version has
        // nothing, in records and in nodes.
        let nowhere: &[&[u8]] = &[b"nowhere"];
        let absent = engine.subtree(nowhere.into(), None, &snapshot).unwrap();
        assert_eq!(absent.aux().get(b"k").unwrap().expect("a read"), None);
        let mut records = absent.meta().cursor();
        records.seek_to_first().unwrap();
        assert!(!records.valid().unwrap());
        records.status().expect("no record, and no error");
        let mut nodes = NodeStore::cursor(&absent);
        nodes.seek_to_first().unwrap();
        assert!(!nodes.valid().unwrap());
        nodes.status().expect("no node, and no error");
        assert!(!absent.has_node(b"k").unwrap().expect("a read"));

        // Every write is refused, by its typed error.
        let refused = |error: StorageError| {
            assert!(
                matches!(error, StorageError::SnapshotReadOnlyTransaction(_)),
                "{error}"
            );
        };
        let immediate = engine.immediate_subtree(ROOT, &snapshot).unwrap();
        refused(
            immediate
                .aux()
                .put(b"k", b"v", None)
                .unwrap()
                .expect_err("a put"),
        );
        let batch = <LigninEngine as Engine>::Batch::default();
        let deferred = engine.subtree(ROOT, Some(&batch), &snapshot).unwrap();
        deferred
            .aux()
            .put(b"k", b"v", None)
            .unwrap()
            .expect("staging");
        drop(deferred);
        refused(
            engine
                .apply_batch(batch, &snapshot)
                .unwrap()
                .expect_err("a batch"),
        );
        refused(engine.open_write_scope(&snapshot).expect_err("a scope"));
        // Savepoints form a stack on it, with nothing to undo.
        engine.set_savepoint(&snapshot).expect("a savepoint");
        engine.rollback_to_savepoint(&snapshot).expect("a rollback");
        assert!(engine.rollback_to_savepoint(&snapshot).is_err());
        engine
            .rollback_transaction(&snapshot)
            .expect("a rollback does nothing");
        drop((cursor, records, nodes, root, absent, immediate));
        refused(
            engine
                .commit_transaction(snapshot)
                .unwrap()
                .expect_err("a commit"),
        );
        put(b"k", b"before");
    }
}

/// A caller holds a transaction beside a borrow of the database that is
/// shorter than the one the transaction was begun with, and gives both to
/// a call that takes them with one lifetime. So what a caller holds
/// across a call takes a shorter lifetime: a transaction, the argument
/// that `GroveDb` takes one as, a write scope and a handle. Each line of
/// `shortens` compiles only if its type does.
///
/// A transaction and a write scope go to another thread and are not
/// shared between threads: the view of a write scope is `Send` alone,
/// and both types can hold one.
#[test]
fn what_a_caller_holds_across_a_call_takes_a_shorter_lifetime() {
    type Transaction<'db> = <LigninEngine as Engine>::Transaction<'db>;
    type WriteScope<'db> = <LigninEngine as Engine>::WriteScope<'db>;
    type Handle<'db> = <LigninEngine as Engine>::Subtree<'db>;
    type Immediate<'db> = <LigninEngine as Engine>::ImmediateSubtree<'db>;
    type Argument<'db, 'a> = TransactionArg<'db, 'a, LigninEngine>;

    #[allow(clippy::type_complexity)]
    fn shortens<'long: 'short, 'short, 'a>(
        held: (
            Transaction<'long>,
            &'a Transaction<'long>,
            Argument<'long, 'a>,
            WriteScope<'long>,
            Handle<'long>,
            Immediate<'long>,
        ),
    ) -> (
        Transaction<'short>,
        &'a Transaction<'short>,
        Argument<'short, 'a>,
        WriteScope<'short>,
        Handle<'short>,
        Immediate<'short>,
    ) {
        held
    }

    /// A function with one lifetime for the database and the transaction,
    /// as every operation of `GroveDb` is.
    fn one_lifetime<'db>(db: &'db Lignin, transaction: Argument<'db, '_>) -> bool {
        db.is_empty_tree(ROOT, transaction, version())
            .unwrap()
            .expect("a read")
    }
    /// A function of a caller, whose two lifetimes are unrelated.
    fn two_lifetimes(db: &Lignin, transaction: Argument) -> bool {
        one_lifetime(db, transaction)
    }

    fn sent<T: Send>() {}
    sent::<Transaction<'static>>();
    sent::<WriteScope<'static>>();
    // Not `Sync`: with it, both of these would apply and the call below
    // would not say which.
    trait NotShared<Which> {
        fn holds() {}
    }
    impl<T: ?Sized> NotShared<()> for T {}
    impl<T: ?Sized + Sync> NotShared<u8> for T {}
    <Transaction<'static> as NotShared<_>>::holds();
    <WriteScope<'static> as NotShared<_>>::holds();

    let dir = tempfile::tempdir().expect("a directory");
    let db = Lignin::open_with_engine(dir.path()).expect("a database");
    let _ = shortens;
    let transaction = db.start_transaction();
    let snapshot = db.start_snapshot_read_transaction();
    assert!(two_lifetimes(&db, Some(&transaction)));
    assert!(two_lifetimes(&db, Some(&snapshot)));
    assert!(two_lifetimes(&db, None));
}

#[test]
fn a_write_scope_is_one_writers_part_of_its_transaction() {
    let (_dir, engine) = engine();
    let tx = engine.start_transaction();
    assert_eq!(format!("{tx:?}"), "LigninTransaction(write)");
    assert!(!tx.is_snapshot_read() && tx.with_write_transaction(|_| ()).is_some());
    let put = |through: &<LigninEngine as Engine>::Transaction<'_>, key: &[u8]| {
        let root = engine.immediate_subtree(ROOT, through).unwrap();
        root.aux().put(key, b"v", None).unwrap()
    };
    let has = |through: &<LigninEngine as Engine>::Transaction<'_>, key: &[u8]| {
        let root = engine.subtree(ROOT, None, through).unwrap();
        root.aux().get(key).unwrap().map(|found| found.is_some())
    };
    put(&tx, b"before").expect("a put");

    // A scope that is merged: its writes become the transaction's.
    let scope = engine.open_write_scope(&tx).expect("a scope");
    assert!(format!("{scope:?}").starts_with("LigninWriteScope"));
    let view = LigninEngine::scope_transaction(&scope);
    assert_eq!(format!("{view:?}"), "LigninTransaction(write scope)");
    assert!(!view.is_snapshot_read() && view.with_write_transaction(|_| ()).is_none());
    put(view, b"merged").expect("a put through the scope");
    assert!(has(view, b"merged").expect("a read"));
    assert!(has(view, b"before").expect("a read"));
    // While it is open the transaction takes no savepoint, no rollback
    // and no nested scope, and what the scope holds is read by no other
    // writer of the transaction.
    let scopes_open = |error: StorageError| {
        assert!(
            matches!(error, StorageError::WriteScope(WriteScopeError::ScopesOpen)),
            "{error}"
        );
    };
    scopes_open(engine.set_savepoint(&tx).expect_err("a savepoint"));
    scopes_open(engine.rollback_to_savepoint(&tx).expect_err("a rollback"));
    scopes_open(engine.rollback_transaction(&tx).expect_err("a rollback"));
    scopes_open(engine.set_savepoint(view).expect_err("a savepoint"));
    scopes_open(engine.rollback_to_savepoint(view).expect_err("a rollback"));
    scopes_open(engine.rollback_transaction(view).expect_err("a rollback"));
    assert!(matches!(
        engine
            .open_write_scope(view)
            .expect_err("a scope of a scope"),
        StorageError::NotSupported(_)
    ));
    let overlap = has(&tx, b"merged").expect_err("a read of what the scope holds");
    assert!(
        matches!(overlap, StorageError::WriteScope(WriteScopeError::Overlap)),
        "{overlap}"
    );
    let overlap = put(&tx, b"merged").expect_err("a write of what the scope holds");
    assert!(
        matches!(overlap, StorageError::WriteScope(WriteScopeError::Overlap)),
        "{overlap}"
    );
    // A batch through the scope's view is applied through the scope.
    let batch = <LigninEngine as Engine>::Batch::default();
    let deferred = engine.subtree(ROOT, Some(&batch), view).unwrap();
    deferred
        .aux()
        .put(b"batched", b"v", None)
        .unwrap()
        .expect("staging");
    drop(deferred);
    engine
        .apply_batch(batch, view)
        .unwrap()
        .expect("a batch through the scope");
    engine.merge_write_scope(scope).expect("a merge");
    assert!(has(&tx, b"merged").expect("a read"));
    assert!(has(&tx, b"batched").expect("a read"));
    engine.set_savepoint(&tx).expect("a savepoint");

    // A scope that is rolled back, and one that is dropped: the
    // transaction is as if they had never been opened.
    let scope = engine.open_write_scope(&tx).expect("a scope");
    put(LigninEngine::scope_transaction(&scope), b"undone").expect("a put");
    engine
        .rollback_write_scope(scope)
        .expect("a rollback of the scope");
    let scope = engine.open_write_scope(&tx).expect("a scope");
    put(LigninEngine::scope_transaction(&scope), b"dropped").expect("a put");
    drop(scope);
    assert!(!has(&tx, b"undone").expect("a read"));
    assert!(!has(&tx, b"dropped").expect("a read"));
    engine.rollback_to_savepoint(&tx).expect("a rollback");

    // The view of a scope is not committed: a commit takes the
    // transaction.
    engine.commit_transaction(tx).unwrap().expect("a commit");
    let read = engine.start_read_transaction();
    for (key, there) in [
        (b"before".as_slice(), true),
        (b"merged", true),
        (b"batched", true),
        (b"undone", false),
        (b"dropped", false),
    ] {
        assert_eq!(has(&read, key).expect("a read"), there);
    }
}

#[test]
fn a_cursor_is_pointed_at_another_subtree_in_the_state_it_was_opened_on() {
    let grove = Grove::<LigninEngine>::new();
    grove.insert(A, b"a1", None);
    grove.insert(B, b"b1", None);
    let engine = grove.db.engine();
    let keys = |cursor: &mut crate::engine::SubtreeCursor<'_, LigninEngine>| {
        let mut keys = Vec::new();
        cursor.seek_to_first().unwrap();
        while cursor.valid().unwrap() {
            keys.push(cursor.key().unwrap().expect("a key").to_vec());
            cursor.next().unwrap();
        }
        cursor.status().map(|()| keys)
    };
    let a = build_prefix(SubtreePath::from([A].as_ref())).value;
    let b = build_prefix(SubtreePath::from([B].as_ref())).value;

    let snapshot = engine.start_snapshot_read_transaction();
    let handle = engine.subtree_by_prefix(a, None, &snapshot).unwrap();
    let mut cursor = NodeStore::cursor(&handle);
    assert_eq!(keys(&mut cursor).expect("a walk"), [b"a1".to_vec()]);
    // A commit lands. The cursor is pointed at B and does not show it.
    grove.insert(B, b"b2", None);
    let mut cursor = LigninEngine::retarget_cursor(cursor, b);
    assert_eq!(keys(&mut cursor).expect("a walk"), [b"b1".to_vec()]);
    // At a subtree that is not entered it shows nothing, and back again.
    let mut cursor = LigninEngine::retarget_cursor(cursor, [7; 32]);
    assert_eq!(keys(&mut cursor).expect("a walk"), Vec::<Vec<u8>>::new());
    let mut cursor = LigninEngine::retarget_cursor(cursor, a);
    assert_eq!(keys(&mut cursor).expect("a walk"), [b"a1".to_vec()]);
    // The next cursor of a new read shows the commit.
    let read = engine.start_read_transaction();
    let handle = engine.subtree_by_prefix(b, None, &read).unwrap();
    let mut later = NodeStore::cursor(&handle);
    assert_eq!(
        keys(&mut later).expect("a walk"),
        [b"b1".to_vec(), b"b2".to_vec()]
    );

    // A cursor of a write transaction is pointed elsewhere too. It shows
    // the other subtree as the committed state had it when the cursor was
    // opened, with what its transaction has written there.
    let tx = engine.start_transaction();
    let handle = engine.subtree_by_prefix(a, None, &tx).unwrap();
    let cursor = NodeStore::cursor(&handle);
    grove.insert(B, b"b3", None);
    grove.insert(B, b"b0", Some(&tx));
    let mut cursor = LigninEngine::retarget_cursor(cursor, b);
    assert_eq!(
        keys(&mut cursor).expect("a walk"),
        [b"b0".to_vec(), b"b1".to_vec(), b"b2".to_vec()]
    );
    let mut cursor = LigninEngine::retarget_cursor(cursor, a);
    assert_eq!(keys(&mut cursor).expect("a walk"), [b"a1".to_vec()]);
}

#[test]
fn what_the_engine_does_not_do_yet_is_not_supported_and_changes_nothing() {
    let (dir, engine) = engine();

    // A subtree that is not entered has no record and no entry to move:
    // its walk ends at once, and the batch is told of nothing.
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    {
        let mut moving = engine.move_subtree([1; 32], [2; 32], &batch, &tx).unwrap();
        let moved = moving.move_next(|_, _| Ok(()), |_, _| Default::default());
        assert_eq!(moved.unwrap().expect("a walk of no record"), None);
        assert!(format!("{moving:?}").starts_with("LigninSubtreeMove"));
    }
    assert!(batch.is_empty());
    drop((batch, tx));

    drop(engine);
    let reopened = LigninEngine::open(dir.path()).expect("the database is where it was");
    reopened.flush().expect("a flush");
    assert!(format!("{reopened:?}").starts_with("LigninEngine"));
}

#[test]
fn pending_drop_records_are_read_and_removed_outside_any_transaction() {
    let (_dir, engine) = engine();
    assert_eq!(engine.pending_prefix_drop_records().expect("no record"), []);
    let record = grovedb_storage::flat::PendingPrefixDropRecord {
        primary_prefix: [3; 32],
        doomed_prefixes: vec![[3; 32], [4; 32]],
        path: vec![b"a".to_vec(), b"dropped".to_vec()],
    };
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let namespace = *pending_prefix_drops_namespace();
    let handle = engine
        .subtree_by_prefix(namespace, Some(&batch), &tx)
        .unwrap();
    grovedb_storage::flat::stage_pending_prefix_drop(&handle.meta(), &record)
        .unwrap()
        .expect("a staged record");
    drop(handle);
    engine.apply_batch(batch, &tx).unwrap().expect("a batch");
    // Nothing is read of a transaction that is not committed.
    assert_eq!(engine.pending_prefix_drop_records().expect("no record"), []);
    engine.commit_transaction(tx).unwrap().expect("a commit");
    assert_eq!(
        engine.pending_prefix_drop_records().expect("a record"),
        std::slice::from_ref(&record)
    );
    (engine.remove_pending_prefix_drop_record(&record.primary_prefix)).expect("a removal");
    assert_eq!(engine.pending_prefix_drop_records().expect("no record"), []);
}

#[test]
fn the_errors_of_lignindb_are_named_by_what_the_seam_knows() {
    use lignin_tree::directory::DirectoryError;
    use lignindb::{
        commit::CommitError as Commit, db::WriteError, savepoint::SavepointError, txn::TxnError,
    };

    let scopes_open = StorageError::WriteScope(WriteScopeError::ScopesOpen).to_string();
    let named = |error: StorageError| error.to_string();
    assert_eq!(named(error::txn_error(TxnError::ScopeOpen)), scopes_open);
    assert_eq!(
        named(error::savepoint_error(SavepointError::ScopesOpen)),
        scopes_open
    );
    assert!(matches!(
        error::savepoint_error(SavepointError::NoSavepoint),
        StorageError::StorageError(_)
    ));
    // A Merk too deep for the subtree directory is a limit of the engine,
    // from an operation and from a commit alike.
    let too_deep = || TxnError::Directory(DirectoryError::TooDeep);
    assert!(matches!(
        error::txn_error(too_deep()),
        StorageError::NotSupported(_)
    ));
    assert!(matches!(
        error::write_error(WriteError::Commit(Commit::Refused(too_deep()))),
        StorageError::NotSupported(_)
    ));
    assert!(matches!(
        Error::from(error::write_error(WriteError::Commit(Commit::Failed(
            lignindb::commit::Failure::Txn(too_deep())
        )))),
        Error::NotSupported(_)
    ));
    // A write that the transaction refused because a commit took a value
    // the write keeps is the conflict of the seam, from an operation and
    // from a commit that made the write.
    let taken = || {
        let subtree = lignindb::txn::Subtree::new([7; 32]);
        TxnError::Batch(lignindb::txn::BatchError::ValueTaken { subtree })
    };
    assert_eq!(
        error::txn_error(taken()).commit_error(),
        Some(CommitError::Conflict)
    );
    assert!(error::write_error(WriteError::Commit(Commit::Refused(taken()))).is_commit_conflict());
    // A Merk hands the refusal of its store on as its own error, and
    // GroveDB's error of a Merk keeps the class.
    let passed = Error::from(grovedb_merk::Error::StorageError(error::txn_error(taken())));
    assert!(matches!(passed, Error::MerkError(_)));
    assert!(passed.is_commit_conflict());
    // The outcomes of a commit by their class.
    let class = |error: WriteError| error::write_error(error).commit_error();
    assert_eq!(
        class(WriteError::Commit(Commit::Conflict)),
        Some(CommitError::Conflict)
    );
    assert_eq!(class(WriteError::Commit(Commit::NotFinalized)), None);
    // A commit that halted the session is settled by the next open, as
    // one that the log could not settle.
    let (_dir, halted) = engine();
    let halted = Commit::Halted {
        version: halted.database().version(),
        source: lignindb::commit::Failure::PlaceLost,
    };
    assert_eq!(
        class(WriteError::Commit(halted)),
        Some(CommitError::Unsettled)
    );
    // A subtree that is entered as another tree than a write states, or
    // under another element, is a storage error that names both.
    let otherwise = TxnError::Batch(lignindb::txn::BatchError::EnteredOtherwise {
        subtree: Subtree::new([1; 32]),
        differs: Box::new(lignindb::txn::Differs::TreeType {
            entered: (*b"a").into(),
            stated: (*b"b").into(),
        }),
    });
    let otherwise = error::write_error(WriteError::Commit(Commit::Refused(otherwise)));
    assert_eq!(otherwise.commit_error(), None);
    let words = otherwise.to_string();
    assert!(
        matches!(otherwise, StorageError::StorageError(_))
            && words.contains("0x61")
            && words.contains("0x62"),
        "{words}"
    );
    // A commit that found the log ahead of the session has failed, and
    // says that the database has to be opened again.
    let version = {
        let (_dir, engine) = engine();
        engine.database().version()
    };
    let gap = WriteError::Commit(Commit::Failed(lignindb::commit::Failure::VersionGap {
        log: version,
        latest: version,
    }));
    assert_eq!(error::write_error(gap).commit_error(), None);
    let gap = WriteError::Commit(Commit::Failed(lignindb::commit::Failure::VersionGap {
        log: version,
        latest: version,
    }));
    assert!(named(error::write_error(gap)).contains("opened again"));
    assert_eq!(
        named(error::write_error(WriteError::Commit(Commit::ScopesOpen))),
        scopes_open
    );
    assert!(
        named(error::write_error(WriteError::Commit(Commit::NotFinalized))).contains("lignindb")
    );
    // The node store's errors: its storage error as it is.
    let merk = |error| named(error::merk_error(error));
    assert_eq!(
        merk(grovedb_merk::Error::StorageError(StorageError::WriteScope(
            WriteScopeError::Overlap
        ))),
        StorageError::WriteScope(WriteScopeError::Overlap).to_string()
    );
    assert!(matches!(
        error::merk_error(grovedb_merk::Error::NotSupported("deep".to_owned())),
        StorageError::NotSupported(_)
    ));
    assert!(merk(grovedb_merk::Error::CorruptedState("a state")).contains("a state"));
    assert!(named(error::not_supported("this")).contains("this"));

    // A directory of another engine is refused by the open, with the
    // engine's words.
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::write(dir.path().join("CURRENT"), b"MANIFEST-000001\n").expect("a file");
    let refused = LigninEngine::open(dir.path()).expect_err("a directory of RocksDB");
    assert!(named(refused).contains("lignindb"));
}

#[test]
fn a_subtree_is_entered_under_what_its_handle_and_its_batch_say() {
    let (_dir, engine) = engine();
    let db = GroveDb::from_engine(engine);
    let v = version();
    for (name, element) in [
        (b"tree", Element::empty_tree()),
        (b"item", Element::new_item(b"no tree".to_vec())),
    ] {
        (db.insert(ROOT, name, element, None, None, v))
            .unwrap()
            .expect("an element under the root");
    }
    let engine = db.engine();
    let entry_of = |path: &[&[u8]]| {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree(path.into(), None, &tx).unwrap();
        handle.entry().expect("the directory answers")
    };
    let write = |path: &[&[u8]]| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree(path.into(), Some(&batch), &tx).unwrap();
        (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
        drop(handle);
        let applied = engine.apply_batch(batch, &tx).unwrap();
        applied.and_then(|_| engine.commit_transaction(tx).unwrap())
    };

    // The physical checker reads the open database.
    let sound = || {
        let mut rules = MerkHashRules::for_check(v);
        let report = engine.database().verify(Some(&mut rules));
        assert!(report.is_sound(), "{:#?}", report.findings);
    };
    // Under an element that is no tree the directory keeps no tree type,
    // and the subtree is no Merk that opens.
    write(path![b"item"]).expect("a record under an item");
    let entry = entry_of(path![b"item"]).expect("entered");
    assert!(entry.tree_type.is_empty() && entry.tree_type().is_err());
    assert_eq!(entry.host.expect("a host").key, b"item");
    sound();
    // A first write under a key that holds no element is refused with
    // everything of its group: the directory names no host that is no
    // node, and none for a subtree that an element could point at later.
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    for path in [path![b"tree", b"x"], path![b"tree"]] {
        let handle = engine.subtree(path.into(), Some(&batch), &tx).unwrap();
        (handle.meta().put(b"k", b"v", None).unwrap()).expect("staging");
    }
    let refused = engine.apply_batch(batch, &tx).unwrap();
    let refused = refused.expect_err("a key that holds no element");
    assert!(
        matches!(refused, StorageError::NotSupported(_)),
        "{refused}"
    );
    drop(tx);
    assert_eq!(entry_of(path![b"tree"]), None);
    assert_eq!(entry_of(path![b"tree", b"x"]), None);
    // A tree, a tree in it and an item in that, all made by one batch:
    // each subtree is entered under its element, which the batch states,
    // and the outer one first, whatever the order of their prefixes.
    let nested = |path: &[&[u8]], key: &[u8], element: Element| {
        let path = path.iter().map(|segment| segment.to_vec()).collect();
        crate::batch::QualifiedGroveDbOp::insert_or_replace_op(path, key.to_vec(), element)
    };
    let ops = vec![
        nested(&[], b"outer", Element::empty_tree()),
        nested(&[b"outer"], b"inner", Element::empty_sum_tree()),
        nested(&[b"outer", b"inner"], b"s", Element::new_sum_item(7)),
    ];
    (db.apply_batch(ops, None, None, v))
        .unwrap()
        .expect("a batch of nested trees");
    let outer = entry_of(path![b"outer"]).expect("entered");
    let host = outer.host.clone().expect("a host");
    assert_eq!(
        (host.subtree, host.key.as_slice()),
        ([0; 32], &b"outer"[..])
    );
    assert_eq!(
        outer.tree_type().expect("a tree type"),
        grovedb_merk::TreeType::NormalTree
    );
    let inner = entry_of(path![b"outer", b"inner"]).expect("entered");
    let host = inner.host.clone().expect("a host");
    assert_eq!(
        (host.subtree, host.key.as_slice()),
        (build_prefix(path![b"outer"].into()).unwrap(), &b"inner"[..])
    );
    assert_eq!(
        inner.tree_type().expect("a tree type"),
        grovedb_merk::TreeType::SumTree
    );
    sound();
    // A tree that is entered by the first write into it has the tree type
    // of its element, which is read from the transaction.
    (db.insert(ROOT, b"sums", Element::empty_sum_tree(), None, None, v))
        .unwrap()
        .expect("a sum tree");
    (db.insert(
        path![b"sums"],
        b"s",
        Element::new_sum_item(4),
        None,
        None,
        v,
    ))
    .unwrap()
    .expect("a sum item");
    let entry = entry_of(path![b"sums"]).expect("entered");
    assert_eq!(
        entry.tree_type().expect("a tree type"),
        grovedb_merk::TreeType::SumTree
    );
    sound();

    // A handle by prefix names the element its subtree is entered under,
    // so a write through it says the same host as one by path.
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let prefix = build_prefix(path![b"outer"].into()).unwrap();
    let by_prefix = engine.subtree_by_prefix(prefix, Some(&batch), &tx).unwrap();
    let host = by_prefix.subtree().host.clone().expect("a host");
    assert_eq!(
        (host.subtree, host.key.as_slice()),
        ([0; 32], &b"outer"[..])
    );
    // With no batch it writes nothing, and names none.
    let reader = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
    assert_eq!(reader.subtree().host, None);
    // A subtree that is not entered has no host to name.
    let nowhere = engine
        .subtree_by_prefix([7; 32], Some(&batch), &tx)
        .unwrap();
    assert_eq!(nowhere.subtree().host, None);
    drop((by_prefix, reader, nowhere));
    drop((batch, tx));

    // The root Merk is entered again by the first write that finds it
    // gone: a wipe of the database underneath, with no engine beside it.
    engine.database().wipe().expect("a wipe");
    assert_eq!(entry_of(&[]), None);
    (db.insert(ROOT, b"again", Element::empty_tree(), None, None, v))
        .unwrap()
        .expect("an element under the root");
    let root = entry_of(&[]).expect("the root is entered");
    assert_eq!(
        (root.tree_type().expect("a tree type"), root.host),
        (grovedb_merk::TreeType::NormalTree, None)
    );
}

#[test]
fn two_transactions_that_both_write_first_into_one_subtree() {
    // Two inserts into a tree that nothing was written into: both enter
    // its subtree and both change the root Merk, so the second to commit
    // meets the first as a conflict, as on RocksDB.
    let grove = Grove::<LigninEngine>::new();
    let first = grove.db.start_transaction();
    let second = grove.db.start_transaction();
    grove.insert(A, b"a1", Some(&first));
    grove.insert(A, b"a2", Some(&second));
    assert_eq!(grove.commit(second), "OK");
    let lost = grove.db.commit_transaction(first).unwrap();
    let lost = lost.expect_err("the second commit");
    assert!(lost.is_commit_conflict(), "{lost}");
    assert!(grove.finds(A, b"a2", None) && !grove.finds(A, b"a1", None));
    let Grove { dir, db } = grove;
    drop(db);
    check(dir.path());

    // Two records with different keys in a keyspace that no element names
    // and nothing has entered: the two transactions share no key, and
    // both commit, as on RocksDB. Each batch enters the keyspace if it is
    // absent, and the second to commit finds the entry of the first.
    let (dir, engine) = engine();
    let keyspace = [7; 32];
    let write = |key: &[u8]| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(keyspace, Some(&batch), &tx)
            .unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, &tx).unwrap().expect("a batch");
        tx
    };
    let (first, second) = (write(b"a"), write(b"b"));
    (engine.commit_transaction(second).unwrap()).expect("the first commit");
    (engine.commit_transaction(first).unwrap()).expect("the second commit");
    let read = |key: &[u8]| {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(keyspace, None, &tx).unwrap();
        handle.aux().get(key).unwrap().expect("a read")
    };
    let written = Some(b"v".to_vec());
    assert_eq!((read(b"a"), read(b"b")), (written.clone(), written));
    // Two writers of one key of it meet in that key.
    let (first, second) = (write(b"c"), write(b"c"));
    (engine.commit_transaction(second).unwrap()).expect("the first commit");
    let lost = engine.commit_transaction(first).unwrap();
    let lost = lost.expect_err("the second commit");
    assert!(lost.is_commit_conflict(), "{lost}");
    // The namespace of the pending-drop records is entered with the
    // database: two transactions that stage a record each both commit.
    let namespace = *pending_prefix_drops_namespace();
    let stage = |key: &[u8]| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(namespace, Some(&batch), &tx)
            .unwrap();
        (handle.meta().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, &tx).unwrap().expect("a batch");
        tx
    };
    let (first, second) = (stage(b"a"), stage(b"b"));
    (engine.commit_transaction(second).unwrap()).expect("a commit");
    (engine.commit_transaction(first).unwrap()).expect("a commit of other keys");
    drop(engine);
    check(dir.path());
}

#[test]
fn the_element_of_a_subtree_is_the_one_its_group_states_last() {
    use grovedb_merk::{
        store::lignin::LigninOutcome, tree::kv::ValueDefinedCostType, Merk, Op, TreeFeatureType,
        TreeType,
    };

    const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;
    let v = version();
    let (_dir, engine) = engine();
    let db = GroveDb::from_engine(engine);
    (db.insert(ROOT, b"t", Element::empty_tree(), None, None, v))
        .unwrap()
        .expect("a tree that nothing is written into");
    let engine = db.engine();
    let put = |element: Element| {
        let bytes = element.serialize(v).expect("the bytes of an element");
        Op::Put(bytes, TreeFeatureType::BasicMerkNode)
    };
    // A group of two batches that were made on one state, as the parts of
    // a partial batch are. The first writes the element `t` again, as a
    // tree of another type. The second writes a key beside it, which
    // writes the node of `t` with the value it has, and writes into the
    // subtree of `t`. The subtree is entered for the group under the
    // element as the first batch states it. The group is never committed:
    // the two batches hashed the root Merk each for itself.
    let group_of = |stated: Element| {
        let tx = engine.start_transaction();
        let mut group = <LigninEngine as Engine>::WriteGroup::default();
        for part in [true, false] {
            let batch = <LigninEngine as Engine>::Batch::default();
            let root = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
            let mut merk = Merk::open_base(root, TreeType::NormalTree, NO_COST, v)
                .unwrap()
                .expect("the root Merk");
            let (key, op) = match part {
                true => (b"t".to_vec(), put(stated.clone())),
                false => (b"u".to_vec(), put(Element::new_item(b"u".to_vec()))),
            };
            (merk.apply::<_, Vec<u8>>(&[(key, op)], &[], None, v))
                .unwrap()
                .expect("a write of the root Merk");
            drop(merk);
            if !part {
                let below = engine
                    .subtree(path![b"t"].into(), Some(&batch), &tx)
                    .unwrap();
                (below.aux().put(b"k", b"v", None).unwrap()).expect("staging");
            }
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        }
        // The first batch states the value of `t`, and the second holds
        // its node with the value kept.
        let held: Vec<_> = (group.parts.iter())
            .map(|part| part.node(&[0; 32], b"t"))
            .collect();
        assert!(matches!(held[0], Some(LigninOutcome::Put(Some(_)))));
        assert_eq!(held[1], Some(LigninOutcome::Put(None)));
        let applied = engine.apply_write_group(group, &tx).unwrap();
        applied.map(|_| {
            let below = engine.subtree(path![b"t"].into(), None, &tx).unwrap();
            let entry = below.entry().expect("the directory answers");
            entry.expect("entered").tree_type().expect("a tree type")
        })
    };
    assert_eq!(
        group_of(Element::empty_sum_tree()).expect("a group"),
        TreeType::SumTree
    );
    // An indexed tree is what the group states, although the transaction
    // holds a tree of one Merk under the key: the subtree is entered as
    // its primary.
    assert_eq!(
        group_of(Element::empty_provable_count_indexed_tree()).expect("a group"),
        TreeType::ProvableCountIndexedTree
    );
}

/// The engine gives Merk's rules to the check that a close of its
/// database makes in a debug build (`lignindb::Db::check_hashes_by`), so
/// that check holds the stored hashes of the nodes of every Merk in use
/// to those rules. A node of the root Merk is written with a key-value
/// hash that is not its own, by a write of one node through the engine's
/// handle, and no rule of the layout is
/// broken by it: the close finds it, as a hash that differs from its
/// recomputation. A grove that GroveDB wrote is closed with nothing
/// found.
#[cfg(debug_assertions)]
#[test]
fn a_close_in_a_debug_build_makes_the_hashes_of_a_grove_again() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use grovedb_merk::{
        store::NodeChange,
        tree::{kv::ValueDefinedCostType, TreeNode},
    };

    const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;
    let v = version();
    let grove = || {
        let (dir, engine) = engine();
        let db = GroveDb::from_engine(engine);
        for n in 0..5u8 {
            (db.insert(
                ROOT,
                &[b'k', n],
                Element::new_item(vec![n; 20]),
                None,
                None,
                v,
            ))
            .unwrap()
            .expect("an item under the root");
        }
        (dir, db)
    };
    let (_dir, db) = grove();
    drop(db);

    let (_dir, db) = grove();
    {
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree(ROOT, Some(&batch), &tx).unwrap();
        let root = handle.root_key().unwrap().expect("a read");
        let root = root.expect("the root Merk has a root");
        let node = handle.get_node(&root, NO_COST, v).unwrap();
        let node = node.expect("a read").expect("the root is stored");
        let value = node.value_as_slice().to_vec();
        let (left, right) = (node.link(true).cloned(), node.link(false).cloned());
        let feature = node.feature_type();
        let wrong = TreeNode::from_fields(root, value, [0xee; 32], left, right, feature).unwrap();
        let written = handle.write_node(&wrong, NodeChange::HashOnly);
        written.unwrap().expect("the node is written");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("the batch is applied");
        (engine.commit_transaction(tx).unwrap()).expect("the commit");
    }
    let closed = catch_unwind(AssertUnwindSafe(move || drop(db)));
    let panic = closed.expect_err("the grove holds a hash that is not its node's");
    let said = panic.downcast_ref::<String>().expect("a text");
    assert!(
        said.contains("is left unsound at version")
            && said.contains("the stored hash differs from its recomputation"),
        "{said}"
    );
}

/// A keyspace that no element names leaves with a wipe, and a wipe can be
/// published between the look that says what a group enters and the
/// group's application. The look found the keyspace, so nothing is stated
/// for it, and the transaction refuses the write. The engine looks again
/// and applies the group with the keyspace to be entered: the write is
/// taken, as on RocksDB. The order is forced: the wipe runs inside the
/// application, after the look.
#[test]
fn a_keyspace_that_a_wipe_takes_after_the_look_is_entered_by_the_write() {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    let (dir, engine) = engine();
    let engine = Arc::new(engine);
    let keyspace = [7; 32];
    let put = |key: &[u8], between: Option<Box<dyn FnOnce()>>| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(keyspace, Some(&batch), &tx)
            .unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        super::AFTER_A_LOOK.set(between.into_iter().map(apart).collect());
        let applied = engine.apply_batch(batch, &tx).unwrap();
        assert!(super::AFTER_A_LOOK.take().is_empty(), "the group looked");
        applied.expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    };
    put(b"seed", None);
    let at = engine.database().version();
    let wiped = Rc::new(Cell::new(false));
    let between = {
        let (engine, wiped) = (engine.clone(), wiped.clone());
        Box::new(move || {
            engine.wipe().expect("a wipe");
            wiped.set(true);
        })
    };
    put(b"late", Some(between));
    assert!(wiped.get());
    // The wipe and the write are two commits.
    let after = at.next().and_then(|wipe| wipe.next());
    assert_eq!(Some(engine.database().version()), after);
    let read = |key: &[u8]| {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(keyspace, None, &tx).unwrap();
        handle.aux().get(key).unwrap().expect("a read")
    };
    assert_eq!((read(b"seed"), read(b"late")), (None, Some(b"v".to_vec())));
    drop(Arc::into_inner(engine).expect("the only handle on the engine"));
    check(dir.path());
}

/// A group looks and finds a keyspace that no element names. Before the
/// group is applied the keyspace is wiped and entered again by another
/// writer, under an element. The group stated nothing for the keyspace,
/// so its write is taken into the one that is there, as on RocksDB.
#[test]
fn a_keyspace_that_is_entered_under_an_element_after_the_look_takes_the_write() {
    use std::sync::Arc;

    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    let engine = db.engine();
    let keyspace = build_prefix(path![b"t"].into()).unwrap();
    let put = |key: &[u8], between: Option<Box<dyn FnOnce()>>| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(keyspace, Some(&batch), &tx)
            .unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        super::AFTER_A_LOOK.set(between.into_iter().map(apart).collect());
        let applied = engine.apply_batch(batch, &tx).unwrap();
        assert!(super::AFTER_A_LOOK.take().is_empty(), "the group looked");
        applied.expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    };
    put(b"seed", None);
    let between = {
        let db = db.clone();
        Box::new(move || {
            db.wipe().expect("a wipe");
            (db.insert(ROOT, b"t", Element::empty_tree(), None, None, v))
                .unwrap()
                .expect("a tree");
            let engine = db.engine();
            let tx = engine.start_transaction();
            let batch = <LigninEngine as Engine>::Batch::default();
            let handle = engine.subtree(path![b"t"].into(), Some(&batch), &tx);
            let handle = handle.unwrap();
            (handle.aux().put(b"other", b"v", None).unwrap()).expect("staging");
            drop(handle);
            engine.apply_batch(batch, &tx).unwrap().expect("a batch");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
        })
    };
    put(b"late", Some(between));
    let tx = engine.start_read_transaction();
    let handle = engine.subtree_by_prefix(keyspace, None, &tx).unwrap();
    let read = |key: &[u8]| handle.aux().get(key).unwrap().expect("a read");
    let written = Some(b"v".to_vec());
    assert_eq!(
        (read(b"seed"), read(b"other"), read(b"late")),
        (None, written.clone(), written)
    );
    // The keyspace is the one the other writer entered, under its element.
    let entry = handle.entry().expect("the directory answers");
    let host = entry.expect("it is entered").host.expect("a host");
    assert_eq!((host.subtree, host.key), ([0; 32], b"t".to_vec()));
    drop(handle);
    drop(tx);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// A group looks and finds the subtree of a tree. Before the group is
/// applied a wipe takes the subtree, and the tree is inserted again and
/// nothing is written into it. The transaction refuses the write into a
/// subtree that is not entered, the engine looks again, finds none, and
/// applies the group with the subtree to be entered under its element: the
/// write is taken, as that of a writer that began after the wipe is.
#[test]
fn a_tree_that_a_wipe_takes_after_the_look_is_entered_again_by_the_write() {
    use std::sync::Arc;

    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
    let engine = db.engine();
    let tree = |db: &Lignin| {
        (db.insert(ROOT, b"t", Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
    };
    let put = |key: &[u8], between: Option<Box<dyn FnOnce()>>| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree(path![b"t"].into(), Some(&batch), &tx);
        let handle = handle.unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        super::AFTER_A_LOOK.set(between.into_iter().map(apart).collect());
        let applied = engine.apply_batch(batch, &tx).unwrap();
        assert!(super::AFTER_A_LOOK.take().is_empty(), "the group looked");
        applied.expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    };
    tree(&db);
    put(b"seed", None);
    let between = {
        let db = db.clone();
        Box::new(move || {
            db.wipe().expect("a wipe");
            tree(&db);
        })
    };
    put(b"late", Some(between));
    let tx = engine.start_read_transaction();
    let handle = engine.subtree(path![b"t"].into(), None, &tx).unwrap();
    let read = |key: &[u8]| handle.aux().get(key).unwrap().expect("a read");
    assert_eq!((read(b"seed"), read(b"late")), (None, Some(b"v".to_vec())));
    let entry = handle.entry().expect("the directory answers");
    let host = entry.expect("it is entered").host.expect("a host");
    assert_eq!((host.subtree, host.key), ([0; 32], b"t".to_vec()));
    drop(handle);
    drop(tx);
    drop(Arc::into_inner(db).expect("the only handle on the grove"));
    check(dir.path());
}

/// What happens beside a group and asks nothing of the transaction the
/// group is applied to.
fn apart(happens: Box<dyn FnOnce()>) -> super::Between {
    Box::new(move |_| happens())
}

/// An engine whose tests can have commits of other writers published
/// while a group is applied: after a look of the group at the subtree
/// directory, and after an application that the transaction refused.
trait Looks: Engine {
    /// Has `between` happen during the next application of a group on
    /// this thread, pair by pair: the first of a pair after a look of
    /// the group, and the second after the transaction refused the
    /// application that followed that look. An engine that does not look
    /// applies a group once: there all of them happen now, in their
    /// order, before that application.
    fn between_looks(between: Vec<[Box<dyn FnOnce()>; 2]>);

    /// Why each application failed since [`Self::between_looks`], in
    /// their order. What was given there and has not happened is given
    /// up.
    fn refused() -> Vec<String>;
}

impl Looks for LigninEngine {
    fn between_looks(between: Vec<[Box<dyn FnOnce()>; 2]>) {
        super::REFUSED.take();
        let (mut looked, mut refused) = (Vec::new(), Vec::new());
        for [after_a_look, after_a_refusal] in between {
            looked.push(apart(after_a_look));
            refused.push(apart(after_a_refusal));
        }
        super::AFTER_A_LOOK.set(looked.into());
        super::AFTER_A_REFUSAL.set(refused.into());
    }

    fn refused() -> Vec<String> {
        let why = |refused: String| {
            if refused.contains("is not entered") {
                "not entered".to_owned()
            } else if refused.contains("and the enter states") {
                "entered otherwise".to_owned()
            } else {
                refused
            }
        };
        super::AFTER_A_LOOK.take();
        super::AFTER_A_REFUSAL.take();
        super::REFUSED.take().into_iter().map(why).collect()
    }
}

#[cfg(feature = "rocksdb_storage")]
impl Looks for crate::RocksDbEngine {
    fn between_looks(between: Vec<[Box<dyn FnOnce()>; 2]>) {
        between.into_iter().flatten().for_each(|between| between());
    }

    fn refused() -> Vec<String> {
        Vec::new()
    }
}

/// What a commit of another writer does to the tree `t` and to its
/// keyspace.
#[derive(Debug, Clone, Copy)]
enum Beside {
    /// A wipe: the tree and its keyspace are gone.
    Wipe,
    /// The tree is inserted, and nothing is written into it.
    EmptyTree,
    /// The tree is inserted, and a writer puts a record into its subtree
    /// by its path: on LigninDB that enters the keyspace under the tree's
    /// element.
    Tree,
    /// A writer puts a record into the keyspace by its prefix: on
    /// LigninDB that enters the keyspace with no host, where it is not
    /// entered.
    Record,
}

/// A history of [`written_beside`].
#[derive(Debug, Clone, Copy)]
struct Stale {
    /// Whether the writer's handle is by the path of the tree `t`, which
    /// is then there from the start, and not by the prefix of its
    /// keyspace.
    by_path: bool,
    /// Whether the tree `t` is in a tree `a`, not at the root: on
    /// LigninDB the subtree that holds its element is then one that a
    /// wipe takes, and that the insert of `t` enters.
    nested: bool,
    /// Whether the keyspace holds a record from the start, through a
    /// handle of the writer's kind: on LigninDB it is entered then.
    seeded: bool,
    /// What is published while the writer's group is applied, pair by
    /// pair: on LigninDB the first of a pair after a look of the group,
    /// and the second after the transaction refused the application that
    /// followed that look.
    beside: &'static [(&'static [Beside], &'static [Beside])],
}

/// A writer stages one aux record for the keyspace of the tree `t`, and
/// commits of other writers are published while its group is applied
/// ([`Looks::between_looks`]), all of them. What the application and
/// the commit answered and what is read of the keyspace afterwards, what
/// the writer's calls were charged (its handle, its put, the application
/// and the commit), and why each application failed.
fn written_beside<E: Looks + 'static>(
    case: Stale,
) -> (Vec<String>, Vec<OperationCost>, Vec<String>) {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    fn put<E: Engine>(db: &GroveDb<E>, nested: bool, by_path: bool, key: &[u8]) {
        let engine = db.engine();
        let tx = engine.start_transaction();
        let batch = E::Batch::default();
        let handle = keyspace(engine, nested, by_path, Some(&batch), &tx).unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, &tx).unwrap().expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    }
    fn keyspace<'a, E: Engine>(
        engine: &'a E,
        nested: bool,
        by_path: bool,
        batch: Option<&'a E::Batch>,
        transaction: &'a E::Transaction<'_>,
    ) -> grovedb_costs::CostContext<E::Subtree<'a>> {
        match (by_path, nested) {
            (true, true) => engine.subtree(path![b"a", b"t"].into(), batch, transaction),
            (true, false) => engine.subtree(path![b"t"].into(), batch, transaction),
            (false, _) => {
                let prefix = match nested {
                    true => build_prefix(path![b"a", b"t"].into()),
                    false => build_prefix(path![b"t"].into()),
                };
                engine.subtree_by_prefix(prefix.unwrap(), batch, transaction)
            }
        }
    }
    fn tree<E: Engine>(db: &GroveDb<E>, nested: bool) {
        let (v, tree) = (version(), Element::empty_tree);
        if nested {
            (db.insert(ROOT, b"a", tree(), None, None, v))
                .unwrap()
                .expect("a tree");
            (db.insert(path![b"a"], b"t", tree(), None, None, v))
                .unwrap()
                .expect("a tree in it");
        } else {
            (db.insert(ROOT, b"t", tree(), None, None, v))
                .unwrap()
                .expect("a tree");
        }
    }
    fn answer(result: Result<(), StorageError>) -> String {
        match result {
            Ok(()) => "OK".to_owned(),
            Err(error) if error.is_commit_conflict() => "Conflict".to_owned(),
            Err(StorageError::NotSupported(what)) => format!("not supported: {what}"),
            Err(error) => format!("refused: {error}"),
        }
    }

    let dir = tempfile::tempdir().expect("a directory");
    let db = Arc::new(GroveDb::<E>::open_with_engine(dir.path()).expect("a database"));
    let nested = case.nested;
    if case.by_path {
        tree(&db, nested);
    }
    if case.seeded {
        put(&db, nested, case.by_path, b"seed");
    }
    let happened = Rc::new(Cell::new(0));
    let between = |beside: &'static [Beside]| {
        let (db, happened) = (db.clone(), happened.clone());
        Box::new(move || {
            for beside in beside {
                match beside {
                    Beside::Wipe => db.wipe().expect("a wipe"),
                    Beside::EmptyTree => tree(&db, nested),
                    Beside::Tree => {
                        tree(&db, nested);
                        put(&db, nested, true, b"other");
                    }
                    Beside::Record => put(&db, nested, false, b"other"),
                }
                happened.set(happened.get() + 1);
            }
        }) as Box<dyn FnOnce()>
    };

    let engine = db.engine();
    let (mut seen, mut costs) = (Vec::new(), Vec::new());
    let tx = engine.start_transaction();
    let batch = E::Batch::default();
    let grovedb_costs::CostContext {
        value: opened,
        cost,
    } = keyspace(engine, nested, case.by_path, Some(&batch), &tx);
    costs.push(cost);
    let staged = opened.aux().put(b"late", b"v", None);
    costs.push(staged.cost);
    staged.value.expect("staging");
    drop(opened);
    let pairs = case.beside.iter();
    E::between_looks(
        pairs
            .map(|(looked, refused)| [between(looked), between(refused)])
            .collect(),
    );
    let applied = engine.apply_batch(batch, &tx);
    let refused = E::refused();
    let all = case.beside.iter();
    let all: usize = all
        .map(|(looked, refused)| looked.len() + refused.len())
        .sum();
    assert_eq!(happened.get(), all, "{case:?}: all of it was published");
    costs.push(applied.cost);
    let taken = applied.value.is_ok();
    seen.push(answer(applied.value.map(drop)));
    if taken {
        let committed = engine.commit_transaction(tx);
        costs.push(committed.cost);
        seen.push(answer(committed.value));
    } else {
        drop(tx);
    }
    let reader = engine.start_read_transaction();
    let opened = keyspace(engine, nested, false, None, &reader).unwrap();
    for key in [&b"seed"[..], b"other", b"late"] {
        let read = opened.aux().get(key).unwrap().expect("a read");
        seen.push(format!("{}", read.is_some()));
    }
    (seen, costs, refused)
}

/// D-181 item 2, the look of the engine: what a group enters is read
/// before the group is applied, and a commit of another writer can come
/// between the two. Each history here has such commits published while
/// one group is applied, by construction: the engine runs them after a
/// look of the group, or after an application that the transaction
/// refused. The eleven histories that are held against RocksDB have no
/// application refused, or one, two or three, each for the cause its
/// history says. The write is then taken, and the answers, the records
/// and the writer's costs are those of RocksDB, where the same commits
/// come before the one application. In five of them a second commit
/// comes after the refusal, and the look after it finds what the look
/// before it found. One more history is answered otherwise than on
/// RocksDB: a wipe took the element of the tree.
#[test]
fn a_group_is_applied_again_for_each_commit_after_its_look_and_answers_as_rocksdb() {
    use Beside::{EmptyTree, Record, Tree, Wipe};

    let taken = |seed: bool, other: bool| {
        let read = |there: bool| format!("{there}");
        vec![
            "OK".to_owned(),
            "OK".to_owned(),
            read(seed),
            read(other),
            read(true),
        ]
    };
    let by_prefix = |seeded, beside| Stale {
        by_path: false,
        nested: false,
        seeded,
        beside,
    };
    let by_path = |beside| Stale {
        by_path: true,
        nested: false,
        seeded: true,
        beside,
    };
    let histories: [(Stale, Vec<String>, &[&str]); 11] = [
        // The look finds the keyspace, and a wipe takes it: the write is
        // refused, the second look finds none, and the keyspace is
        // entered with the write.
        (
            by_prefix(true, &[(&[Wipe], &[])]),
            taken(false, false),
            &["not entered"],
        ),
        // The look finds none and states the enter, and another writer
        // enters the keyspace under an element: the enter is refused, the
        // second look finds the keyspace, and the write goes into it.
        (
            by_prefix(false, &[(&[Tree], &[])]),
            taken(false, true),
            &["entered otherwise"],
        ),
        // Both, one after the other: the second application fails too.
        (
            by_prefix(true, &[(&[Wipe], &[]), (&[Tree], &[])]),
            taken(false, true),
            &["not entered", "entered otherwise"],
        ),
        // And a wipe after the third look: the third application fails,
        // and the fourth is taken.
        (
            by_prefix(true, &[(&[Wipe], &[]), (&[Tree], &[]), (&[Wipe], &[])]),
            taken(false, false),
            &["not entered", "entered otherwise", "not entered"],
        ),
        // A wipe and the entered tree come after one look: the keyspace is
        // there at the application, which states nothing and is taken.
        (
            by_prefix(true, &[(&[Wipe, Tree], &[])]),
            taken(false, true),
            &[],
        ),
        // By the path of the tree: a wipe takes the tree and its subtree,
        // and the tree is inserted again. The second look states the
        // enter under its element.
        (
            by_path(&[(&[Wipe, EmptyTree], &[])]),
            taken(false, false),
            &["not entered"],
        ),
        // The look finds the keyspace and a wipe takes it. After the
        // refusal another writer enters it again, by its prefix: the
        // second look finds the keyspace, as the first did, and the write
        // goes into it.
        (
            by_prefix(true, &[(&[Wipe], &[Record])]),
            taken(false, true),
            &["not entered"],
        ),
        // The same, and the other writer enters it under an element.
        (
            by_prefix(true, &[(&[Wipe], &[Tree])]),
            taken(false, true),
            &["not entered"],
        ),
        // And by the path of the tree, which is inserted and written
        // into again after the refusal.
        (
            by_path(&[(&[Wipe], &[Tree])]),
            taken(false, true),
            &["not entered"],
        ),
        // The look finds none and states the enter, and another writer
        // enters the keyspace under an element. After the refusal a wipe
        // takes it: the second look states the enter the first stated,
        // and the keyspace is entered with the write.
        (
            by_prefix(false, &[(&[Tree], &[Wipe])]),
            taken(false, false),
            &["entered otherwise"],
        ),
        // A tree in a tree, by its path, with nothing written into it:
        // the look states the enter under its element. A wipe takes the
        // subtree that holds the element, and the enter is refused. After
        // the refusal both trees are inserted again: the second look
        // states the enter the first stated.
        (
            Stale {
                by_path: true,
                nested: true,
                seeded: false,
                beside: &[(&[Wipe], &[EmptyTree])],
            },
            taken(false, false),
            &["not entered"],
        ),
    ];
    for (case, expected, failed) in histories {
        let (seen, costs, refused) = written_beside::<LigninEngine>(case);
        assert_eq!(seen, expected, "{case:?}");
        assert_eq!(refused, failed, "{case:?}");
        assert_eq!(costs.len(), 4, "{case:?}");
        #[cfg(feature = "rocksdb_storage")]
        {
            let (on_rocksdb, charged, _) = written_beside::<crate::RocksDbEngine>(case);
            assert_eq!((seen, costs), (on_rocksdb, charged), "{case:?}");
        }
    }

    // One history is answered otherwise, as it is with no commit beside
    // it (`engine.md`, "Subtrees are entered by the first batch that
    // writes into them"): a wipe takes the tree's element with its
    // subtree, and nothing puts it back. The second look refuses the
    // group as a first write under a key that holds no element, and
    // RocksDB takes the write into a keyspace that no element leads to.
    let no_element = by_path(&[(&[Wipe], &[])]);
    let (seen, _, refused) = written_beside::<LigninEngine>(no_element);
    assert!(seen[0].starts_with("not supported: "), "{seen:?}");
    assert!(seen[0].contains("holds no element"), "{seen:?}");
    assert_eq!(seen[1..], ["false", "false", "false"]);
    assert_eq!(refused, ["not entered"]);
    #[cfg(feature = "rocksdb_storage")]
    {
        let (on_rocksdb, ..) = written_beside::<crate::RocksDbEngine>(no_element);
        assert_eq!(on_rocksdb, taken(false, false));
    }
}

/// A refusal that is of no entry which the look found otherwise is the
/// caller's answer, whatever is published beside the group: the group is
/// applied once and looked for once. Here the group goes through a write
/// scope while another scope holds the unit it writes, and a wipe, which
/// takes the keyspace that the look found entered, is published after the
/// refusal.
#[test]
fn a_refusal_that_is_of_no_entry_is_answered_after_one_application() {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    let (dir, engine) = engine();
    let engine = Arc::new(engine);
    let keyspace = [7; 32];
    let put = |through: &<LigninEngine as Engine>::Transaction<'_>, key: &[u8]| {
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(keyspace, Some(&batch), through)
            .unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, through).unwrap()
    };
    let seeded = engine.start_transaction();
    put(&seeded, b"seed").expect("a batch");
    (engine.commit_transaction(seeded).unwrap()).expect("a commit");

    let tx = engine.start_transaction();
    let first = engine.open_write_scope(&tx).expect("a scope");
    let second = engine.open_write_scope(&tx).expect("a scope beside it");
    put(LigninEngine::scope_transaction(&first), b"a").expect("the first scope's record");
    let wiped = Rc::new(Cell::new(false));
    let wipe: super::Between = {
        let (engine, wiped) = (engine.clone(), wiped.clone());
        Box::new(move |_| {
            engine.wipe().expect("a wipe");
            wiped.set(true);
        })
    };
    super::REFUSED.take();
    // A second look would take the second of these.
    super::AFTER_A_LOOK.set([apart(Box::new(|| ())), apart(Box::new(|| ()))].into());
    super::AFTER_A_REFUSAL.set([wipe].into());
    let at = engine.database().version();
    let overlap = put(LigninEngine::scope_transaction(&second), b"c");
    let overlap = overlap.expect_err("the first scope wrote the unit");
    assert!(
        matches!(overlap, StorageError::WriteScope(WriteScopeError::Overlap)),
        "{overlap}"
    );
    assert!(wiped.get());
    assert_eq!(at.next(), Some(engine.database().version()));
    assert_eq!(super::REFUSED.take().len(), 1);
    assert_eq!(super::AFTER_A_LOOK.take().len(), 1, "one look");
    assert!(super::AFTER_A_REFUSAL.take().is_empty());
    engine.rollback_write_scope(first).expect("a rollback");
    engine.rollback_write_scope(second).expect("a rollback");
    drop(tx);
    drop(Arc::into_inner(engine).expect("the only handle on the engine"));
    check(dir.path());
}

/// A group in which a batch enters, drops or clears a subtree itself,
/// or states one anew, is not looked for again, whatever is published
/// beside it: the entry it
/// is refused for can be gone by its own doing. Here the first batch of
/// a group drops a keyspace and the second writes a record into it,
/// which a transaction that lays the group out where it is applied
/// refuses, and a commit that has nothing to do with the keyspace is
/// published after the refusal.
#[test]
fn a_group_that_drops_a_subtree_is_answered_after_one_application() {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    use grovedb_merk::store::lignin::LigninSubtree;

    let (dir, engine) = engine();
    let engine = Arc::new(engine);
    let (keyspace, elsewhere) = ([7; 32], [8; 32]);
    let put = |engine: &LigninEngine, into: [u8; 32], key: &[u8]| {
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine.subtree_by_prefix(into, Some(&batch), &tx).unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, &tx).unwrap().expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    };
    put(&engine, keyspace, b"seed");

    let tx = engine.start_transaction();
    let mut group = <LigninEngine as Engine>::WriteGroup::default();
    let drops = <LigninEngine as Engine>::Batch::default();
    drops.drop_subtree(&LigninSubtree::alone(keyspace));
    (engine.settle_batch(&mut group, drops).unwrap()).expect("a settled batch");
    let writes = <LigninEngine as Engine>::Batch::default();
    let handle = engine
        .subtree_by_prefix(keyspace, Some(&writes), &tx)
        .unwrap();
    (handle.aux().put(b"late", b"v", None).unwrap()).expect("staging");
    drop(handle);
    (engine.settle_batch(&mut group, writes).unwrap()).expect("a settled batch");

    let published = Rc::new(Cell::new(false));
    let unrelated: super::Between = {
        let (engine, published) = (engine.clone(), published.clone());
        Box::new(move |_| {
            put(&engine, elsewhere, b"other");
            published.set(true);
        })
    };
    super::REFUSED.take();
    // A second look would take the second of these.
    super::AFTER_A_LOOK.set([apart(Box::new(|| ())), apart(Box::new(|| ()))].into());
    super::AFTER_A_REFUSAL.set([unrelated].into());
    let at = engine.database().version();
    let refused = engine.apply_write_group(group, &tx).unwrap();
    let refused = refused.expect_err("the group drops the keyspace it writes into");
    assert!(refused.to_string().contains("is not entered"), "{refused}");
    assert!(published.get());
    assert_eq!(at.next(), Some(engine.database().version()));
    assert_eq!(super::REFUSED.take().len(), 1);
    assert_eq!(super::AFTER_A_LOOK.take().len(), 1, "one look");
    assert!(super::AFTER_A_REFUSAL.take().is_empty());
    drop(tx);
    let reader = engine.start_read_transaction();
    let handle = engine.subtree_by_prefix(keyspace, None, &reader).unwrap();
    let read = |key: &[u8]| handle.aux().get(key).unwrap().expect("a read");
    assert_eq!((read(b"seed"), read(b"late")), (Some(b"v".to_vec()), None));
    drop(handle);
    drop(reader);
    drop(Arc::into_inner(engine).expect("the only handle on the engine"));
    check(dir.path());
}

/// A subtree that the look states an enter for, and that the transaction
/// then finds with no entry, waits for an enter of the group itself: no
/// commit made that so, and the group is not looked for again. Here a
/// handle names its subtree as the one that holds its own element, and
/// the group states that element. The look states the enter under it,
/// the transaction refuses the enter of a subtree whose host is in a
/// subtree with no entry, and a commit that has nothing to do with it is
/// published after the refusal.
#[test]
fn a_subtree_that_is_named_as_its_own_host_is_answered_after_one_application() {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    use grovedb_merk::{
        store::lignin::{LigninHost, LigninNodeStore, LigninSubtree},
        tree::kv::ValueDefinedCostType,
        Merk, Op, TreeFeatureType, TreeType,
    };

    const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;
    let v = version();
    let (dir, engine) = engine();
    let engine = Arc::new(engine);
    let (ring, elsewhere) = ([7; 32], [8; 32]);

    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let named = LigninSubtree {
        prefix: ring,
        host: Some(LigninHost {
            subtree: ring,
            key: b"k".to_vec(),
            hosted: vec![ring],
        }),
    };
    let handle = LigninNodeStore::deferred(tx.view(), named, Some(&batch));
    let mut merk = Merk::open_base(handle, TreeType::NormalTree, NO_COST, v)
        .unwrap()
        .expect("a Merk with no node");
    let element = Element::empty_tree().serialize(v).expect("the bytes");
    let put = Op::Put(element, TreeFeatureType::BasicMerkNode);
    (merk.apply::<_, Vec<u8>>(&[(b"k".to_vec(), put)], &[], None, v))
        .unwrap()
        .expect("a write of the Merk");
    drop(merk);

    let published = Rc::new(Cell::new(false));
    let unrelated: super::Between = {
        let (engine, published) = (engine.clone(), published.clone());
        Box::new(move |_| {
            let tx = engine.start_transaction();
            let batch = <LigninEngine as Engine>::Batch::default();
            let handle = engine
                .subtree_by_prefix(elsewhere, Some(&batch), &tx)
                .unwrap();
            (handle.aux().put(b"other", b"v", None).unwrap()).expect("staging");
            drop(handle);
            engine.apply_batch(batch, &tx).unwrap().expect("a batch");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
            published.set(true);
        })
    };
    super::REFUSED.take();
    // A second look would take the second of these.
    super::AFTER_A_LOOK.set([apart(Box::new(|| ())), apart(Box::new(|| ()))].into());
    super::AFTER_A_REFUSAL.set([unrelated].into());
    let at = engine.database().version();
    let refused = engine.apply_batch(batch, &tx).unwrap();
    let refused = refused.expect_err("the subtree that holds the element is not entered");
    assert!(refused.to_string().contains("is not entered"), "{refused}");
    assert!(published.get());
    assert_eq!(at.next(), Some(engine.database().version()));
    assert_eq!(super::REFUSED.take().len(), 1);
    assert_eq!(super::AFTER_A_LOOK.take().len(), 1, "one look");
    assert!(super::AFTER_A_REFUSAL.take().is_empty());
    drop(tx);
    let reader = engine.start_read_transaction();
    let handle = engine.subtree_by_prefix(ring, None, &reader).unwrap();
    assert_eq!(handle.entry().expect("the directory answers"), None);
    drop(handle);
    drop(reader);
    drop(Arc::into_inner(engine).expect("the only handle on the engine"));
    check(dir.path());
}

/// The bound of the engine's look: a refusal for an entry that the look
/// found otherwise is the caller's answer where no commit was published
/// since before the look. The seam has one thread at a time write through
/// a transaction, so the look and the application after it then read one
/// state. This test has the writing thread itself change the transaction
/// between the two, from a hook, to have such a refusal with no commit:
/// the transaction entered the keyspace itself, and is rolled back to a
/// savepoint from before that. The group is applied once and its refusal
/// is answered; the transaction stays open, and takes the same write when
/// it is applied again.
#[test]
fn a_refusal_for_an_entry_is_answered_where_no_commit_was_published() {
    let (dir, engine) = engine();
    let keyspace = [7; 32];
    let put = |tx: &<LigninEngine as Engine>::Transaction<'_>, key: &[u8]| {
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(keyspace, Some(&batch), tx)
            .unwrap();
        (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
        drop(handle);
        engine.apply_batch(batch, tx).unwrap()
    };
    let tx = engine.start_transaction();
    engine.set_savepoint(&tx).expect("a savepoint");
    put(&tx, b"own").expect("a batch that enters the keyspace");
    let back: super::Between = Box::new(|tx| {
        tx.with_write_transaction(|tx| tx.rollback_to_savepoint().expect("a rollback"))
            .expect("a write transaction");
    });
    super::REFUSED.take();
    super::AFTER_A_LOOK.set([back].into());
    let at = engine.database().version();
    let refused = put(&tx, b"late").expect_err("the keyspace is not entered any more");
    assert!(refused.to_string().contains("is not entered"), "{refused}");
    assert_eq!(super::REFUSED.take().len(), 1);
    assert!(super::AFTER_A_LOOK.take().is_empty(), "the group looked");
    assert_eq!(engine.database().version(), at);
    put(&tx, b"late").expect("a batch that enters the keyspace");
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    let reader = engine.start_read_transaction();
    let handle = engine.subtree_by_prefix(keyspace, None, &reader).unwrap();
    let read = |key: &[u8]| handle.aux().get(key).unwrap().expect("a read");
    assert_eq!((read(b"own"), read(b"late")), (None, Some(b"v".to_vec())));
    drop(handle);
    drop(reader);
    drop(engine);
    check(dir.path());
}

/// Two write scopes of one transaction that each write a record of a
/// keyspace which is entered already, in different columns, are held
/// apart by nothing: each group finds the keyspace and states nothing for
/// it, so neither claims its entry in the subtree directory as a write.
#[test]
fn two_write_scopes_write_into_one_keyspace_that_is_entered() {
    let (dir, engine) = engine();
    let keyspace = [7; 32];
    let put = |through: &<LigninEngine as Engine>::Transaction<'_>, meta: bool, key: &[u8]| {
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(keyspace, Some(&batch), through)
            .unwrap();
        let staged = match meta {
            true => handle.meta().put(key, b"v", None),
            false => handle.aux().put(key, b"v", None),
        };
        staged.unwrap().expect("staging");
        drop(handle);
        engine.apply_batch(batch, through).unwrap()
    };
    let seeded = engine.start_transaction();
    put(&seeded, false, b"seed").expect("a batch");
    (engine.commit_transaction(seeded).unwrap()).expect("a commit");

    let tx = engine.start_transaction();
    let first = engine.open_write_scope(&tx).expect("a scope");
    let second = engine.open_write_scope(&tx).expect("a scope beside it");
    put(LigninEngine::scope_transaction(&first), false, b"a").expect("the first scope's record");
    put(LigninEngine::scope_transaction(&second), true, b"b").expect("the second scope's record");
    // A group that is refused for another cause than an entry the look
    // found otherwise is applied once, and the caller is answered the
    // refusal. Here the first scope holds the unit the second writes.
    super::REFUSED.take();
    let overlap = put(LigninEngine::scope_transaction(&second), false, b"c");
    let overlap = overlap.expect_err("the first scope wrote the unit");
    assert!(
        matches!(overlap, StorageError::WriteScope(WriteScopeError::Overlap)),
        "{overlap}"
    );
    assert_eq!(super::REFUSED.take().len(), 1);
    engine.merge_write_scope(first).expect("a merge");
    engine.merge_write_scope(second).expect("a merge");
    (engine.commit_transaction(tx).unwrap()).expect("a commit");
    let read = engine.start_read_transaction();
    let handle = engine.subtree_by_prefix(keyspace, None, &read).unwrap();
    let written = Some(b"v".to_vec());
    let aux = handle.aux().get(b"a").unwrap().expect("a read");
    let meta = handle.meta().get(b"b").unwrap().expect("a read");
    assert_eq!((aux, meta), (written.clone(), written));
    drop(handle);
    drop(read);
    drop(engine);
    check(dir.path());
}

/// One of the cases of two first writers: the subtree both write into,
/// the keyspace of it, and whether they write one key.
#[derive(Debug, Clone, Copy)]
struct FirstWriters {
    /// The subtree of a tree that was inserted and never written into,
    /// through a handle by its path, or a keyspace that no element names,
    /// through a handle by its prefix.
    by_path: bool,
    meta: bool,
    same_key: bool,
}

/// Two transactions that each make the first write into one subtree
/// nothing was written into, both begun and both written before either
/// commits (D-181 item 2). What each commit answered and what is read
/// afterwards, and what each call was charged: the insert of the tree,
/// for each writer its handle, its put and the application of its batch,
/// the two commits, and the handle and the two reads of the reader.
fn two_first_writers<E: Engine>(case: FirstWriters) -> (Vec<String>, Vec<OperationCost>) {
    fn handle<'a, E: Engine>(
        engine: &'a E,
        case: FirstWriters,
        batch: Option<&'a E::Batch>,
        transaction: &'a E::Transaction<'_>,
    ) -> grovedb_costs::CostContext<E::Subtree<'a>> {
        match case.by_path {
            true => engine.subtree(path![b"t"].into(), batch, transaction),
            false => engine.subtree_by_prefix([7; 32], batch, transaction),
        }
    }
    let v = version();
    let dir = tempfile::tempdir().expect("a directory");
    let db = GroveDb::<E>::open_with_engine(dir.path()).expect("a database");
    let inserted = db.insert(ROOT, b"t", Element::empty_tree(), None, None, v);
    let (mut seen, mut costs) = (Vec::new(), vec![inserted.cost]);
    (inserted.value).expect("a tree that nothing is written into");
    let engine = db.engine();
    let key = |writer: u8| match case.same_key {
        true => vec![b'k'],
        false => vec![b'k', writer],
    };
    let mut write = |writer: u8| {
        let transaction = engine.start_transaction();
        let batch = E::Batch::default();
        let grovedb_costs::CostContext {
            value: opened,
            cost,
        } = handle(engine, case, Some(&batch), &transaction);
        costs.push(cost);
        let staged = match case.meta {
            true => opened.meta().put(key(writer), &[writer], None),
            false => opened.aux().put(key(writer), &[writer], None),
        };
        costs.push(staged.cost);
        staged.value.expect("staging");
        drop(opened);
        let applied = engine.apply_batch(batch, &transaction);
        costs.push(applied.cost);
        applied.value.expect("a batch");
        transaction
    };
    let (first, second) = (write(1), write(2));
    for transaction in [second, first] {
        let committed = engine.commit_transaction(transaction);
        costs.push(committed.cost);
        seen.push(match committed.value {
            Ok(()) => "OK".to_owned(),
            Err(error) if error.is_commit_conflict() => "Conflict".to_owned(),
            Err(error) => format!("refused: {error}"),
        });
    }
    let reader = engine.start_read_transaction();
    let grovedb_costs::CostContext {
        value: opened,
        cost,
    } = handle(engine, case, None, &reader);
    costs.push(cost);
    for writer in [1, 2] {
        let read = match case.meta {
            true => opened.meta().get(key(writer)),
            false => opened.aux().get(key(writer)),
        };
        costs.push(read.cost);
        seen.push(format!("{:?}", read.value.expect("a read")));
    }
    (seen, costs)
}

/// The case of two first writers of one subtree, on LigninDB and beside
/// RocksDB: with records under different keys both commit and both
/// records are read, with one key the second commit is the conflict, and
/// every answer and every `OperationCost` is the one of the same calls on
/// RocksDB.
#[test]
fn two_first_writers_of_one_subtree_are_answered_and_charged_as_on_rocksdb() {
    for by_path in [true, false] {
        for meta in [false, true] {
            for same_key in [false, true] {
                let case = FirstWriters {
                    by_path,
                    meta,
                    same_key,
                };
                let (seen, costs) = two_first_writers::<LigninEngine>(case);
                // The second writer to be begun commits first.
                let expected = match same_key {
                    true => ["OK", "Conflict", "Some([2])", "Some([2])"],
                    false => ["OK", "OK", "Some([1])", "Some([2])"],
                };
                assert_eq!(seen, expected, "{case:?}");
                assert_eq!(costs.len(), 12, "{case:?}");
                #[cfg(feature = "rocksdb_storage")]
                assert_eq!(
                    (seen, costs),
                    two_first_writers::<crate::RocksDbEngine>(case),
                    "{case:?}"
                );
            }
        }
    }
}

/// D-181 item 5: a wipe is one commit, with the two subtrees that every
/// grove has. The appends of the file layer fail from the first, the
/// second or the third on, around a wipe, and a failed append is undone
/// or leaves a part of its bytes in the file, or all of them. Whatever
/// the wipe returned, the grove is as it was or it is wiped and has both
/// subtrees, in this session and in the next: it is never wiped without
/// them. Where the first append fails and is undone, the wipe has failed
/// and no session finds it.
#[test]
fn a_wipe_that_fails_leaves_the_grove_as_it_was() {
    use std::sync::Arc;

    use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet, SimConfig, SimVfs};

    let v = version();
    let hostless = [[0; 32], *pending_prefix_drops_namespace()];
    for (skip, torn) in (0..3u64).flat_map(|skip| [(skip, false), (skip, true)]) {
        let case = format!("append {skip}, torn {torn}");
        let fs = SimVfs::new(SimConfig::new(40 + skip));
        let options = Options {
            vfs: Arc::new(fs.clone()),
            ..Options::default()
        };
        let engine = LigninEngine::open_with("/grove", &options).expect("an engine");
        let db = GroveDb::from_engine(engine);
        (db.insert(ROOT, A, Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree");
        let item = Element::new_item(b"a1".to_vec());
        (db.insert(path![A], b"a1", item, None, None, v))
            .unwrap()
            .expect("an item");
        (db.put_aux(b"x", b"aux", None, None))
            .unwrap()
            .expect("an aux record");
        db.engine().flush().expect("the log is synced");
        let before = db.root_hash(None, v).unwrap().expect("a root");
        let at = db.engine().database().version();

        let appends = OpSet::of(&[OpKind::Append]);
        let rule = FaultRule::new(Fault::Eio).on(appends).skip(skip);
        fs.add_rule(rule.torn_append(torn).forever());
        let wiped = db.wipe();
        fs.clear_rules();
        // Whether a reader of `db` finds the grove as it was, and not
        // wiped. Either has the two subtrees.
        let as_it_was = |db: &Lignin, at: &str| {
            let aux = db.get_aux(b"x", None).unwrap().expect("a read");
            let root = db.root_hash(None, v).unwrap().expect("a root");
            let database = db.engine().database();
            let snapshot = database.snapshot();
            let operation = snapshot.operation();
            let entered = hostless.map(|prefix| {
                let opened = operation.open(Subtree::new(prefix));
                opened.expect("a lookup").is_some()
            });
            assert_eq!(entered, [true; 2], "{case}, {at}: both are entered");
            match aux {
                Some(aux) => {
                    assert_eq!((aux, root), (b"aux".to_vec(), before), "{case}, {at}");
                    true
                }
                None => {
                    assert_ne!(root, before, "{case}, {at}");
                    let item = db.get(path![A], b"a1", None, v).unwrap();
                    assert!(item.is_err(), "{case}, {at}: the item is gone");
                    false
                }
            }
        };
        // Whether the wipe is published is read from the version, and the
        // grove is as that says, whatever the wipe returned.
        let published = db.engine().database().version() != at;
        let kept = as_it_was(&db, "in this session");
        assert_eq!(kept, !published, "{case}: {wiped:?}");
        assert!(wiped.is_err() || published, "{case}: a wipe that is made");
        let undone = skip == 0 && !torn;
        if undone {
            assert!(wiped.is_err() && !published, "{case}: {wiped:?}");
        }
        drop(db);
        // The next session finds a wipe that was published, and none
        // whose group was taken back. The database is read before the
        // engine has it, which would enter a subtree of the two that it
        // found gone: both are there, and the engine writes nothing.
        let database = Db::open("/grove", &options).expect("it opens again");
        let found = {
            let snapshot = database.snapshot();
            let operation = snapshot.operation();
            hostless.map(|prefix| {
                let opened = operation.open(Subtree::new(prefix));
                opened.expect("a lookup").is_some()
            })
        };
        assert_eq!(found, [true; 2], "{case}: both are entered at the open");
        let opened_at = database.version();
        let engine = LigninEngine::new(database).expect("an engine");
        assert_eq!(engine.database().version(), opened_at, "{case}");
        let db = GroveDb::from_engine(engine);
        let kept = as_it_was(&db, "in the next session");
        assert!(!(published && kept), "{case}: a published wipe is lost");
        assert!(
            !undone || kept,
            "{case}: a wipe that was taken back is made"
        );
        // The grove is written again either way.
        (db.insert(ROOT, b"after", Element::empty_tree(), None, None, v))
            .unwrap()
            .expect("a tree after the wipe");
    }
}
