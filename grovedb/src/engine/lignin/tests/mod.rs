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
//! it, with every hash of every Merk node made again by Merk's rules.

mod append_only;
mod cases;
mod indexed;
mod nested;
mod restore;

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
use lignindb::{verify::Scope, Db, Options};

use self::cases::{Grove, A, B};
use super::{error, LigninEngine};
use crate::{
    engine::{Engine, SubtreeMove, WriteGroup},
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

/// Opens the database in `dir` again, which nothing has open, and has
/// the physical checker read it: every rule of the layout, and every
/// stored hash of every Merk node against the hash that Merk's rules make.
fn check(dir: &std::path::Path) {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    let mut rules = MerkHashRules::for_check(version());
    let report = db.check(Scope::Still, Some(&mut rules));
    assert!(report.is_sound(), "{:#?}", report.findings);
    db.close().expect("a close");
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
    grove.db.wipe().expect("a wipe");
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
        assert!(snapshot.write_transaction().is_none());
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

#[test]
fn a_write_scope_is_one_writers_part_of_its_transaction() {
    let (_dir, engine) = engine();
    let tx = engine.start_transaction();
    assert_eq!(format!("{tx:?}"), "LigninTransaction(write)");
    assert!(!tx.is_snapshot_read() && tx.write_transaction().is_some());
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
    assert!(!view.is_snapshot_read() && view.write_transaction().is_none());
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
    let not_supported = |error: &dyn std::fmt::Display, storage: bool| {
        assert!(storage, "{error}");
        assert!(error.to_string().contains("lignindb"), "{error}");
    };
    let checkpoint = dir.path().join("checkpoint");
    let error = engine
        .create_checkpoint(&checkpoint)
        .expect_err("a checkpoint");
    not_supported(&error, matches!(error, StorageError::NotSupported(_)));
    assert!(!checkpoint.exists());
    // GroveDB hands it on as its own error of that name.
    assert!(matches!(Error::from(error), Error::NotSupported(_)));
    let error = (engine.delete_prefix_ranges(&[[1; 32]])).expect_err("a range delete");
    not_supported(&error, matches!(error, StorageError::NotSupported(_)));

    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let mut moving = engine.move_subtree([1; 32], [2; 32], &batch, &tx).unwrap();
    let moved = moving.move_next(|_, _| Ok(()), |_, _| Default::default());
    assert_eq!(moved.cost, OperationCost::default());
    let error = moved.unwrap().expect_err("a move");
    not_supported(&error, matches!(error, Error::NotSupported(_)));
    assert_eq!(format!("{moving:?}"), "LigninSubtreeMove");
    drop((batch, tx));

    // No checkpoint is opened either, so none is deleted: the directory
    // of a database is not taken for one.
    drop(engine);
    let error = LigninEngine::open_checkpoint(dir.path()).expect_err("a checkpoint");
    not_supported(&error, matches!(error, StorageError::NotSupported(_)));
    let deleted = Lignin::delete_checkpoint_with_engine(dir.path());
    assert!(matches!(deleted, Err(Error::NotSupported(_))));
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
    // A write that enters a subtree which a commit beside it entered is
    // told from every other way a write ends.
    let entered = || {
        TxnError::Batch(lignindb::txn::BatchError::Entered {
            subtree: lignindb::txn::Subtree::new([1; 32]),
        })
    };
    assert!(error::entered_already(&WriteError::Commit(
        Commit::Refused(entered())
    )));
    assert!(error::entered_already(&WriteError::Commit(Commit::Failed(
        lignindb::commit::Failure::Txn(entered())
    ))));
    assert!(!error::entered_already(&WriteError::Commit(
        Commit::Refused(too_deep())
    )));
    assert!(!error::entered_already(&WriteError::Commit(
        Commit::Conflict
    )));
    // A commit that found the log ahead of the session has failed, and
    // says that the database has to be opened again.
    let version = engine().1.database().version();
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
fn a_tree_that_holds_elements_is_not_deleted_yet_and_stays_whole() {
    let v = version();
    let grove = Grove::<LigninEngine>::new();
    grove.insert(A, b"a1", None);
    let root_before = grove.db.root_hash(None, v).unwrap().expect("a root");
    let options = crate::operations::delete::DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    };
    // Its nodes are no flat records, and the removal of the records of
    // its keyspace says so.
    let deleted = (grove.db).delete(ROOT, A, Some(options), None, v).unwrap();
    let refused = deleted.expect_err("a tree with elements");
    assert!(refused.to_string().contains("lignindb"), "{refused}");
    let root = grove.db.root_hash(None, v).unwrap().expect("a root");
    assert_eq!(root, root_before);
    assert!(grove.finds(A, b"a1", None));
    // A tree that nothing was written into is deleted, as on RocksDB.
    (grove.db.delete(ROOT, B, None, None, v))
        .unwrap()
        .expect("an empty tree");
    let gone = grove.db.get(ROOT, B, None, v).unwrap();
    assert!(matches!(gone, Err(Error::PathKeyNotFound(_))));
    let root = grove.db.root_hash(None, v).unwrap().expect("a root");
    assert_ne!(root, root_before);
    #[cfg(feature = "rocksdb_storage")]
    {
        let rocks = Grove::<crate::RocksDbEngine>::new();
        rocks.insert(A, b"a1", None);
        (rocks.db.delete(ROOT, B, None, None, v))
            .unwrap()
            .expect("an empty tree");
        assert_eq!(rocks.db.root_hash(None, v).unwrap().expect("a root"), root);
    }
    let Grove { dir, db } = grove;
    drop(db);
    check(dir.path());
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
        let report = engine.database().check(Scope::Reader, Some(&mut rules));
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
    // and nothing has entered: the two transactions share no key, and the
    // second to commit is refused all the same, by the subtree directory,
    // which it would enter a second time. It is no conflict, and nothing
    // of it is kept.
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
    let lost = engine.commit_transaction(first).unwrap();
    let lost = lost.expect_err("the second commit");
    assert_eq!(lost.commit_error(), None, "{lost}");
    assert!(lost.to_string().contains("lignindb"), "{lost}");
    let read = |key: &[u8]| {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(keyspace, None, &tx).unwrap();
        handle.aux().get(key).unwrap().expect("a read")
    };
    assert_eq!((read(b"a"), read(b"b")), (None, Some(b"v".to_vec())));
    // Made again, the write finds the keyspace entered and is committed.
    (engine.commit_transaction(write(b"a")).unwrap()).expect("the write made again");
    assert_eq!(read(b"a"), Some(b"v".to_vec()));
    // The namespace of the pending-drop records is entered with the
    // database, so two transactions that stage a record each both commit.
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
