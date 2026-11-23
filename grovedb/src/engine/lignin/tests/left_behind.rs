//! What a flat drop leaves behind until its drain (`engine.md`, "Flat
//! drops and their drain"; `flat-store.md`, "A step from no key").
//!
//! Between a flat drop and its drain RocksDB still holds the records of
//! the dropped tree, and a cursor of another subtree that steps from no
//! key comes to them: a read of a range from right to left that ends
//! past the last key of its subtree answers by whether a record follows
//! the subtree in the column family. LigninDB drops the tree with its
//! element and keeps a mark for what the drop left. The histories here
//! are written once, for any engine, and run on LigninDB and on RocksDB
//! on every grove version: every answer and every `OperationCost` are
//! compared, and the physical checker reads the database that LigninDB
//! leaves.
//!
//! - [`plain`], [`indexed`]: three trees at the root. The tree `s` is
//!   read, by four ranges that step from no key: two from right to left
//!   that end past its last key, and two from left to right that start
//!   before its first. The tree `h` is dropped flat: a tree of items
//!   that is the last in the order of the prefixes, or an indexed tree
//!   with three secondaries whose primary is before `s` and of whose
//!   secondaries one is after it. `s` is read before the drop, inside
//!   the dropping transaction, after a rollback, after the commit and
//!   after the drain. Then the tree before `s` is dropped, which is the
//!   mirror: the root Merk is before every tree, so a record precedes
//!   `s` whatever is dropped. Then `h` is made again and dropped, made
//!   again with nothing in it before the drain, which skips its record
//!   as live, and dropped once more and drained.
//! - [`lines`]: at the seam, a dropped tree with a record in its aux and
//!   in its meta keyspace, and the cursors of the three keyspaces of
//!   its neighbour.

use std::path::Path;

use grovedb_element::indexed::IndexAxis;
use grovedb_storage::{
    prefix::{build_prefix, secondary_prefix_for},
    FlatStore, RawIterator,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, on_both_engines, Answer, History},
    indexed::{CountSum, Indexed},
    Lignin, LigninEngine,
};
use crate::{
    engine::{Engine, SubtreeHandle},
    query_result_type::QueryResultType,
    BackwardsReferences, Element, GroveDb, PathQuery, Query, QueryItem, TransactionArg,
};

const ROOT: &[&[u8]] = &[];

fn prefix_of(name: &[u8]) -> [u8; 32] {
    let path: &[&[u8]] = &[name];
    build_prefix(path.into()).value
}

/// The names `a` to `z`, in the order of the prefixes of the trees that
/// they name at the root.
fn by_prefix() -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = (b'a'..=b'z').map(|name| vec![name]).collect();
    names.sort_by_key(|name| prefix_of(name));
    names
}

/// The prefixes of the three secondaries that an indexed tree named
/// `name` at the root can have.
fn secondaries(name: &[u8]) -> [[u8; 32]; 3] {
    [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
        .map(|axis| secondary_prefix_for(&prefix_of(name), axis.tag()).value)
}

/// The names of a history: the tree that is dropped in the mirror, the
/// tree that is read, and the tree that is dropped first.
///
/// For a tree of items, the three last in the order of the prefixes:
/// nothing but the dropped tree follows the one that is read. For an
/// indexed tree, three of which the primary of the dropped tree is the
/// first, the tree that is read the third, and a secondary of the
/// dropped tree after it.
fn names(indexed: bool) -> [Vec<u8>; 3] {
    let names = by_prefix();
    if !indexed {
        return [names[23].clone(), names[24].clone(), names[25].clone()];
    }
    for (at, h) in names.iter().enumerate() {
        let Some(s) = names.get(at + 2) else {
            break;
        };
        if secondaries(h)
            .iter()
            .any(|secondary| *secondary > prefix_of(s))
        {
            return [names[at + 1].clone(), s.clone(), h.clone()];
        }
    }
    panic!("no two names of which a secondary of the first follows the second");
}

/// Makes the tree that is dropped, with what it holds.
fn make<E: Engine>(h: &mut History<'_, E>, name: &[u8], indexed: bool) {
    let (db, v) = (h.db, h.v);
    let at: &[&[u8]] = &[name];
    match indexed {
        false => {
            h.write(
                "made",
                db.insert(ROOT, name, Element::empty_tree(), None, None, v),
            );
            for key in [b"k1", b"k2"] {
                let item = Element::new_item(vec![5; 9]);
                h.write("item", db.insert(at, key, item, None, None, v));
            }
        }
        true => {
            h.write(
                "made",
                db.insert(ROOT, name, CountSum::empty(), None, None, v),
            );
            for n in 0..3 {
                let key = format!("row{n}").into_bytes();
                let row = CountSum::insert(db, at, &key, CountSum::leaf(n), None, v);
                h.write("row", row);
            }
        }
    }
}

/// The four reads of the tree `s`, each with the number of its rows.
fn reads<'db, E: Engine>(
    h: &mut History<'db, E>,
    phase: &str,
    s: &[u8],
    tx: TransactionArg<'db, '_, E>,
) {
    let (db, v) = (h.db, h.v);
    let (k0, k9) = (b"k0".to_vec(), b"k9".to_vec());
    let items = [
        ("back", QueryItem::Range(k0.clone()..k9.clone()), false),
        ("to, back", QueryItem::RangeTo(..k9.clone()), false),
        ("after", QueryItem::RangeAfter(k0.clone()..), true),
        ("after, to", QueryItem::RangeAfterTo(k0..k9), true),
    ];
    for (name, item, left_to_right) in items {
        let query = Query::new_single_query_item_with_direction(item, left_to_right);
        let path_query = PathQuery::new_unsized(vec![s.to_vec()], query);
        let read = || {
            let kind = QueryResultType::QueryKeyElementPairResultType;
            db.query_raw(&path_query, true, true, true, kind, tx, v)
        };
        let rows = read().unwrap().map(|(rows, _)| rows.elements.len());
        h.elements(format!("{phase}: {name}"), read());
        h.plain(format!("rows {phase}: {name}"), rows);
    }
}

fn drain<E: Engine>(h: &mut History<'_, E>) {
    let report = h.db.flush_pending_prefix_drops(h.v);
    h.plain(
        "drained",
        report.map(|report| (report.reclaimed_records, report.skipped_live)),
    );
}

/// Drops the tree `name` flat in a transaction, reads `s` in it, and
/// commits or rolls back.
fn dropped<'db, E: Engine>(
    h: &mut History<'db, E>,
    phase: &str,
    name: &[u8],
    s: &[u8],
    commits: bool,
) {
    let (db, v) = (h.db, h.v);
    let unchecked = BackwardsReferences::DontCheck;
    let tx = db.start_transaction();
    let drop = db.drop_flat_subtree(ROOT, name, unchecked, Some(&tx), v);
    h.read("drop", drop);
    h.read("root", db.root_hash(Some(&tx), v));
    reads(h, &format!("{phase}, in its transaction"), s, Some(&tx));
    match commits {
        true => {
            h.read("commit", db.commit_transaction(tx));
        }
        false => h.plain("rolled back", db.rollback_transaction(&tx)),
    }
    h.read("root", db.root_hash(None, v));
}

fn history<E: Engine>(db: &GroveDb<E>, v: &GroveVersion, indexed: bool) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let [below, s, dropped_first] = names(indexed);
    for name in [&below, &s] {
        make(&mut h, name, false);
    }
    make(&mut h, &dropped_first, indexed);
    drain(&mut h);
    reads(&mut h, "made", &s, None);

    // The tree that a record of follows `s`.
    dropped(&mut h, "dropped", &dropped_first, &s, false);
    reads(&mut h, "rolled back", &s, None);
    dropped(&mut h, "dropped again", &dropped_first, &s, true);
    reads(&mut h, "committed", &s, None);
    drain(&mut h);
    reads(&mut h, "drained", &s, None);

    // The mirror: the tree before `s`.
    dropped(&mut h, "below dropped", &below, &s, true);
    reads(&mut h, "below committed", &s, None);
    drain(&mut h);
    reads(&mut h, "below drained", &s, None);

    // A dropped path that is made again before its record is drained.
    make(&mut h, &dropped_first, indexed);
    reads(&mut h, "made again", &s, None);
    dropped(&mut h, "dropped a third time", &dropped_first, &s, true);
    let empty = match indexed {
        false => Element::empty_tree(),
        true => CountSum::empty(),
    };
    h.write(
        "made",
        db.insert(ROOT, &dropped_first, empty, None, None, v),
    );
    reads(&mut h, "made anew", &s, None);
    drain(&mut h);
    reads(&mut h, "skipped as live", &s, None);
    dropped(&mut h, "dropped the last time", &dropped_first, &s, true);
    reads(&mut h, "committed at last", &s, None);
    drain(&mut h);
    reads(&mut h, "drained at last", &s, None);
    let issues = db.verify_grovedb(None, true, false, v);
    h.plain("verified", issues.map(|issues| issues.len()));
    h.answers
}

fn plain<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    history(db, v, false)
}

fn indexed<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    history(db, v, true)
}

/// What a history must have answered on the grove versions that drop a
/// tree flat: the reads from right to left find the two rows of `s`
/// while a record follows it, which is until the drain of the drop, and
/// none afterwards; the reads from left to right find them throughout.
fn told(answers: &[Answer], number: usize) {
    if number < 4 {
        return;
    }
    let said = |call: &str| {
        let mut said = answers.iter().filter(|(named, ..)| named == call);
        let (.., first) = said.next().unwrap_or_else(|| panic!("no call {call}"));
        assert!(said.next().is_none(), "{call} twice");
        first.as_str()
    };
    let drops = answers
        .iter()
        .filter(|(named, _, said)| named == "drop" && said == "Ok(())");
    assert_eq!(drops.count(), 5, "v{number}");
    let phases = [
        ("made", true),
        ("dropped, in its transaction", true),
        ("rolled back", true),
        ("dropped again, in its transaction", true),
        ("committed", true),
        ("drained", false),
        ("below dropped, in its transaction", false),
        ("below committed", false),
        ("below drained", false),
        ("made again", true),
        ("dropped a third time, in its transaction", true),
        ("made anew", true),
        ("skipped as live", true),
        ("dropped the last time, in its transaction", true),
        ("committed at last", true),
        ("drained at last", false),
    ];
    for (phase, follows) in phases {
        let back = if follows { "Ok(2)" } else { "Ok(0)" };
        for name in ["back", "to, back"] {
            assert_eq!(
                said(&format!("rows {phase}: {name}")),
                back,
                "{phase}, v{number}"
            );
        }
        for name in ["after", "after, to"] {
            assert_eq!(
                said(&format!("rows {phase}: {name}")),
                "Ok(2)",
                "{phase}, v{number}"
            );
        }
    }
    let drained: Vec<&str> = (answers.iter())
        .filter(|(named, ..)| named == "drained")
        .map(|(.., said)| said.as_str())
        .collect();
    let expected = [
        "Ok((0, 0))",
        "Ok((1, 0))",
        "Ok((1, 0))",
        "Ok((0, 1))",
        "Ok((1, 0))",
    ];
    assert_eq!(drained, expected, "v{number}");
    assert_eq!(said("verified"), "Ok(0)", "v{number}");
}

#[test]
fn a_range_read_beside_a_tree_that_was_dropped_flat_answers_as_on_rocksdb() {
    on_both_engines!(plain, told);
}

#[test]
fn a_range_read_beside_an_indexed_tree_that_was_dropped_flat_answers_as_on_rocksdb() {
    on_both_engines!(indexed, told);
}

/// The keys of the marks that the engine keeps, as a read transaction
/// shows the aux keyspace of the namespace of the pending-drop records.
fn marks(db: &Lignin) -> Vec<Vec<u8>> {
    let engine = db.engine();
    let tx = engine.start_read_transaction();
    let namespace = *grovedb_storage::flat::pending_prefix_drops_namespace();
    let handle = engine.subtree_by_prefix(namespace, None, &tx).unwrap();
    let aux = handle.aux();
    let mut cursor = aux.cursor();
    let mut keys = Vec::new();
    cursor.seek_to_first().unwrap();
    while let Some(key) = cursor.key().unwrap() {
        keys.push(key.to_vec());
        cursor.next().unwrap();
    }
    cursor.status().expect("the cursor read to its end");
    keys
}

/// The marks of a flat drop are those of the prefixes that held a
/// record, are in the dropping transaction and in no other until it
/// commits, leave with a rollback, stay over a drain that skips the
/// record of a path that is live, and are gone after the drain that
/// reclaims it.
#[test]
fn the_marks_of_a_flat_drop_stay_until_the_drain_that_reclaims_its_record() {
    use grovedb_costs::cost_model::Keyspace;
    use grovedb_merk::store::lignin::left_behind_key;

    for v in &GROVE_VERSIONS[3..] {
        for indexed in [false, true] {
            let dir = tempfile::tempdir().expect("a directory");
            {
                let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                let mut h = History::new(&db, v);
                let [below, s, name] = names(indexed);
                for tree in [&below, &s] {
                    make(&mut h, tree, false);
                }
                make(&mut h, &name, indexed);
                // A tree that nothing was written into leaves no mark.
                h.write(
                    "made",
                    db.insert(ROOT, b"empty", Element::empty_tree(), None, None, v),
                );
                let mark = |prefix| left_behind_key(Keyspace::Data, &prefix).expect("a key");
                let mut expected = vec![mark(prefix_of(&name))];
                if indexed {
                    expected.extend(secondaries(&name).map(mark));
                }
                expected.sort();
                assert_eq!(marks(&db), Vec::<Vec<u8>>::new());

                let unchecked = BackwardsReferences::DontCheck;
                let tx = db.start_transaction();
                for dropped in [name.as_slice(), b"empty"] {
                    (db.drop_flat_subtree(ROOT, dropped, unchecked, Some(&tx), v)
                        .unwrap())
                    .expect("a flat drop");
                }
                assert_eq!(marks(&db), Vec::<Vec<u8>>::new(), "before the commit");
                db.rollback_transaction(&tx).expect("a rollback");
                (db.commit_transaction(tx).unwrap()).expect("a commit");
                assert_eq!(marks(&db), Vec::<Vec<u8>>::new(), "after a rollback");

                let tx = db.start_transaction();
                for dropped in [name.as_slice(), b"empty"] {
                    (db.drop_flat_subtree(ROOT, dropped, unchecked, Some(&tx), v)
                        .unwrap())
                    .expect("a flat drop");
                }
                (db.commit_transaction(tx).unwrap()).expect("a commit");
                assert_eq!(marks(&db), expected, "after the commit");

                // The path of the dropped tree is live: its record is
                // skipped, and what the drop left stays.
                let empty = match indexed {
                    false => Element::empty_tree(),
                    true => CountSum::empty(),
                };
                (db.insert(ROOT, &name, empty, None, None, v).unwrap()).expect("a tree");
                let report = db.flush_pending_prefix_drops(v).expect("a drain");
                assert_eq!((report.reclaimed_records, report.skipped_live), (1, 1));
                assert_eq!(marks(&db), expected, "after a drain that skips the record");

                let tx = db.start_transaction();
                (db.drop_flat_subtree(ROOT, &name, unchecked, Some(&tx), v)
                    .unwrap())
                .expect("a flat drop");
                (db.commit_transaction(tx).unwrap()).expect("a commit");
                assert_eq!(
                    marks(&db),
                    expected,
                    "after the drop of a tree with no record"
                );
                let report = db.flush_pending_prefix_drops(v).expect("a drain");
                assert_eq!((report.reclaimed_records, report.skipped_live), (1, 0));
                assert_eq!(marks(&db), Vec::<Vec<u8>>::new(), "after the drain");
                db.flush().expect("a flush");
            }
            check(dir.path(), v);
        }
    }
}

/// The tree `h` at the root has a record in its aux and in its meta
/// keyspace, written through a handle by its prefix, and is dropped
/// flat. The cursors of the three keyspaces of the tree before it are
/// each sought past their last key and stepped back: before the drop,
/// in the dropping transaction, after the commit and after the drain.
fn lines<E: Engine>(dir: &Path) -> Vec<String> {
    let v = GroveVersion::latest();
    let db = GroveDb::<E>::open_with_engine(dir).expect("a database");
    let engine = db.engine();
    let names = by_prefix();
    let (s, h) = (names[24].clone(), names[25].clone());
    for name in [&s, &h] {
        (db.insert(ROOT, name, Element::empty_tree(), None, None, v)
            .unwrap())
        .expect("a tree");
        let at: &[&[u8]] = &[name];
        for key in [b"k1", b"k2"] {
            let item = Element::new_item(vec![5; 9]);
            (db.insert(at, key, item, None, None, v).unwrap()).expect("an item");
        }
        let tx = engine.start_transaction();
        let batch = E::Batch::default();
        let handle = engine
            .subtree_by_prefix(prefix_of(name), Some(&batch), &tx)
            .unwrap();
        (handle.aux().put(b"k1", b"aux", None).unwrap()).expect("staging");
        (handle.meta().put(b"k1", b"meta", None).unwrap()).expect("staging");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    }
    let mut log = Vec::new();
    macro_rules! stepped_back {
        ($name:expr, $cursor:expr) => {{
            let mut cursor = $cursor;
            let mut cost = cursor.seek(b"k9").cost;
            let past = cursor.valid().unwrap();
            cost += cursor.prev().cost;
            let key = cursor.key().unwrap().map(<[u8]>::to_vec);
            cursor.status().expect("a cursor that read");
            format!("{}: past {past}, back on {key:?}, {cost:?}", $name)
        }};
    }
    let mut look = |phase: &str, tx: &E::Transaction<'_>| {
        let path: &[&[u8]] = &[&s];
        let prefix = prefix_of(&s);
        let handle = engine.subtree_by_prefix(prefix, None, tx).unwrap();
        log.push(phase.to_owned());
        let cursor = crate::engine::NodeStore::cursor(&handle);
        log.push(stepped_back!("data", cursor));
        let (aux, meta) = (handle.aux(), handle.meta());
        log.push(stepped_back!("aux", aux.cursor()));
        log.push(stepped_back!("meta", meta.cursor()));
        // Each other kind of handle that the seam gives.
        let by_path = engine.subtree(path.into(), None, tx).unwrap();
        let cursor = crate::engine::NodeStore::cursor(&by_path);
        log.push(stepped_back!("by path", cursor));
        let beside = engine
            .subtree_beside(path.into(), prefix, None, tx)
            .unwrap();
        let cursor = crate::engine::NodeStore::cursor(&beside);
        log.push(stepped_back!("beside", cursor));
        let at_once = engine.immediate_subtree(path.into(), tx).unwrap();
        let cursor = crate::engine::NodeStore::cursor(&at_once);
        log.push(stepped_back!("at once", cursor));
        let at_once = engine.immediate_subtree_by_prefix(prefix, tx).unwrap();
        let cursor = crate::engine::NodeStore::cursor(&at_once);
        log.push(stepped_back!("at once, by prefix", cursor));
    };
    look("made", &engine.start_read_transaction());
    let tx = db.start_transaction();
    (db.drop_flat_subtree(ROOT, &h, BackwardsReferences::DontCheck, Some(&tx), v)
        .unwrap())
    .expect("a flat drop");
    look("dropped", &tx);
    (db.commit_transaction(tx).unwrap()).expect("a commit");
    look("committed", &engine.start_read_transaction());
    let report = db.flush_pending_prefix_drops(v).expect("a drain");
    assert_eq!(report.reclaimed_records, 1);
    look("drained", &engine.start_read_transaction());
    log
}

#[test]
fn the_cursors_of_each_keyspace_beside_a_flat_drop_step_as_on_rocksdb() {
    let dir = tempfile::tempdir().expect("a directory");
    let on_lignin = lines::<LigninEngine>(dir.path());
    check(dir.path(), GroveVersion::latest());
    // Each cursor comes back onto the last key until the drain, and
    // stays on no key after it.
    let back = |line: &&String| line.contains("back on Some(");
    assert_eq!(on_lignin.iter().filter(back).count(), 21, "{on_lignin:#?}");
    assert_eq!(on_lignin.len(), 32);
    let drained = &on_lignin[25..];
    assert!(drained.iter().all(|line| line.contains("back on None")));
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let on_rocksdb = lines::<crate::RocksDbEngine>(dir.path());
        assert_eq!(on_lignin, on_rocksdb);
    }
}

/// A tree is dropped flat by a batch that is told not to look at what
/// its operations do to each other: with the removal of one of its two
/// items, which GroveDB refuses all the same; with the removal of both,
/// after which RocksDB holds no record of the tree; and, made again,
/// with the drop alone.
fn in_batches<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    use grovedb_merk::TreeType;

    use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp, SubelementsDeletionBehavior};

    let mut h = History::new(db, v);
    let [below, s, name] = names(false);
    for tree in [&below, &s, &name] {
        make(&mut h, tree, false);
    }
    for removed in [1usize, 2, 0] {
        if removed == 0 {
            make(&mut h, &name, false);
        }
        reads(&mut h, &format!("made, {removed}"), &s, None);
        let mut ops: Vec<QualifiedGroveDbOp> = [b"k1", b"k2"][..removed]
            .iter()
            .map(|key| QualifiedGroveDbOp::delete_op(vec![name.clone()], key.to_vec()))
            .collect();
        let drop = QualifiedGroveDbOp::delete_tree_op(
            Vec::new(),
            name.clone(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DropFlat,
        );
        ops.push(drop.dont_check_for_backwards_references());
        let tx = db.start_transaction();
        let options = BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        };
        let batch = db.apply_batch(ops, Some(options), Some(&tx), v);
        h.read(format!("batch, {removed}"), batch);
        h.read("root", db.root_hash(Some(&tx), v));
        let phase = format!("dropped, {removed}, in its transaction");
        reads(&mut h, &phase, &s, Some(&tx));
        h.read("commit", db.commit_transaction(tx));
        reads(&mut h, &format!("committed, {removed}"), &s, None);
        drain(&mut h);
        reads(&mut h, &format!("drained, {removed}"), &s, None);
    }
    h.answers
}

fn in_batches_told(answers: &[Answer], number: usize) {
    if number < 4 {
        return;
    }
    let said = |call: String| {
        let mut said = answers.iter().filter(|(named, ..)| *named == call);
        let (.., first) = said.next().unwrap_or_else(|| panic!("no call {call}"));
        first.as_str()
    };
    for removed in [1, 2, 0] {
        let batch = said(format!("batch, {removed}"));
        // What the batch answers, and the rows of a read from right to
        // left once it is applied, and once it is drained.
        let (applied, left, drained) = match removed {
            1 => ("Err(InvalidBatchOperation(", "Ok(2)", "Ok(2)"),
            2 => ("Ok(())", "Ok(0)", "Ok(0)"),
            _ => ("Ok(())", "Ok(2)", "Ok(0)"),
        };
        assert!(batch.starts_with(applied), "{batch}, v{number}");
        for (phase, rows) in [
            (format!("made, {removed}"), "Ok(2)"),
            (format!("dropped, {removed}, in its transaction"), left),
            (format!("committed, {removed}"), left),
            (format!("drained, {removed}"), drained),
        ] {
            let back = said(format!("rows {phase}: back"));
            assert_eq!(back, rows, "{phase}, v{number}");
            let after = said(format!("rows {phase}: after"));
            assert_eq!(after, "Ok(2)", "{phase}, v{number}");
        }
    }
    let drained: Vec<&str> = (answers.iter())
        .filter(|(named, ..)| named == "drained")
        .map(|(.., said)| said.as_str())
        .collect();
    assert_eq!(
        drained,
        ["Ok((0, 0))", "Ok((1, 0))", "Ok((1, 0))"],
        "v{number}"
    );
}

/// A batch that drops a tree flat leaves what RocksDB keeps of the tree:
/// nothing where the batch itself removes every record of it, and a
/// record where it removes none.
#[test]
fn a_batch_that_drops_a_tree_flat_answers_as_on_rocksdb() {
    on_both_engines!(in_batches, in_batches_told);
}

/// The rule of [`left_behind`](super::super::behind::left_behind), on a
/// group that is settled at the seam: three pending-drop records, of a
/// tree whose two aux records the group removes, of a tree of whose two
/// aux records it removes one, and of a tree that nothing is entered
/// for, under whose prefix the group puts an aux record. A mark is
/// stated for a prefix that leaves with the group, by name or with a
/// subtree it hangs under, and for a keyspace in which a record stays:
/// one that the transaction shows and the group does not remove, or one
/// that the group puts. A record that a later batch of the group removes
/// names no prefix.
#[test]
fn a_mark_is_stated_for_each_keyspace_in_which_a_record_stays_under_a_prefix_that_leaves() {
    use std::collections::BTreeSet;

    use grovedb_storage::flat::{
        pending_prefix_drops_namespace, stage_pending_prefix_drop, PendingPrefixDropRecord,
    };

    use super::super::{behind::left_behind, LigninWriteGroup};

    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
    let engine = db.engine();
    let (x, held, absent) = (b"x".to_vec(), b"h".to_vec(), b"absent".to_vec());
    for name in [&x, &held] {
        let mut h = History::new(&db, v);
        make(&mut h, name, false);
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let handle = engine
            .subtree_by_prefix(prefix_of(name), Some(&batch), &tx)
            .unwrap();
        for key in [b"a1", b"a2"] {
            (handle.aux().put(key, b"aux", None).unwrap()).expect("staging");
        }
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    }

    let tx = engine.start_transaction();
    let namespace = *pending_prefix_drops_namespace();
    let mut group = LigninWriteGroup::default();
    let batch = <LigninEngine as Engine>::Batch::default();
    {
        let by_prefix = |prefix| engine.subtree_by_prefix(prefix, Some(&batch), &tx).unwrap();
        let records = by_prefix(namespace);
        for name in [&x, &held, &absent] {
            let record = PendingPrefixDropRecord {
                primary_prefix: prefix_of(name),
                path: vec![name.clone()],
                doomed_prefixes: vec![prefix_of(name)],
            };
            (stage_pending_prefix_drop(&records.meta(), &record).unwrap()).expect("staging");
        }
        let of_x = by_prefix(prefix_of(&x));
        for key in [b"a1", b"a2"] {
            (of_x.aux().delete(key, None).unwrap()).expect("staging");
        }
        let of_held = by_prefix(prefix_of(&held));
        (of_held.aux().delete(b"a1", None).unwrap()).expect("staging");
        let of_absent = by_prefix(prefix_of(&absent));
        (of_absent.aux().put(b"a1", b"aux", None).unwrap()).expect("staging");
    }
    (engine.settle_batch(&mut group, batch).unwrap()).expect("the batch is settled");

    type Stated = Vec<(usize, String, Vec<u8>)>;
    let stated = |group: &LigninWriteGroup, dropped: &[&[u8]]| -> Stated {
        let root = [0; 32];
        let dropped: BTreeSet<[u8; 32]> = (dropped.iter())
            .map(|name| match name.is_empty() {
                true => root,
                false => prefix_of(name),
            })
            .collect();
        let marks = left_behind(tx.view(), &group.parts, &dropped).expect("the look");
        let name_of = |prefix: [u8; 32]| {
            let mut names = [&x, &held, &absent].into_iter();
            let named = names.find(|name| prefix_of(name) == prefix);
            named.expect("a prefix of the three").clone()
        };
        let mut marks: Stated = (marks.into_iter())
            .map(|(part, keyspace, prefix)| (part, format!("{keyspace:?}"), name_of(prefix)))
            .collect();
        marks.sort();
        marks
    };
    let sorted = |mut marks: Stated| {
        marks.sort();
        marks
    };
    let mark = |keyspace: &str, name: &[u8]| (0usize, keyspace.to_owned(), name.to_vec());
    let all = sorted(vec![
        mark("Aux", &absent),
        mark("Aux", &held),
        mark("Data", &held),
        mark("Data", &x),
    ]);
    assert_eq!(stated(&group, &[&x, &held, &absent]), all);
    assert_eq!(stated(&group, &[]), Stated::new());
    let of_held = sorted(vec![mark("Aux", &held), mark("Data", &held)]);
    assert_eq!(stated(&group, &[&held]), of_held);
    // Both trees that are entered hang under the root Merk, and nothing
    // is entered for the third.
    let under_the_root = sorted(vec![
        mark("Aux", &held),
        mark("Data", &held),
        mark("Data", &x),
    ]);
    assert_eq!(stated(&group, &[b""]), under_the_root);

    // A later batch of the group removes one of the records.
    let batch = <LigninEngine as Engine>::Batch::default();
    {
        let records = engine
            .subtree_by_prefix(namespace, Some(&batch), &tx)
            .unwrap();
        (records.meta().delete(prefix_of(&held), None).unwrap()).expect("staging");
    }
    (engine.settle_batch(&mut group, batch).unwrap()).expect("the batch is settled");
    let left = sorted(vec![mark("Aux", &absent), mark("Data", &x)]);
    assert_eq!(stated(&group, &[&x, &held, &absent]), left);
    drop(group);
    engine.rollback_transaction(&tx).expect("a rollback");
}
