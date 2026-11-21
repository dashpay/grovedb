//! Reads and proofs of indexed trees (GROVE-06): the reads of an axis and
//! their proofs on LigninDB and on RocksDB, call for call, for each kind
//! of indexed tree on every grove version.
//!
//! A history has an indexed tree of its kind at the root and one under
//! each of two owners, with children that are no trees and a child that
//! is a tree with elements of its own. Every axis of the kind is read as a
//! ranking from both ends with and without an offset, as a band of
//! values, as the rank of a key, as a fold over a band, as keys alone, and
//! across the owners' trees at once from both ends. The reads are made
//! with no
//! transaction, through a snapshot that later commits do not show, and
//! through a write transaction that has moved rows of every axis and not
//! committed, before and after a commit beside it. A ranking with an
//! offset reads the element of its tree and the nodes of a secondary
//! through one cursor, which a write transaction's could not be pointed
//! at before this task. The committed state is proven twice, and the
//! verifier takes every proof and comes to the committed root hash.
//!
//! A band is read in descending order only where a value above it is in
//! the ranking: the read seeks to the end of the band and steps back, and
//! from past the last record of a subtree RocksDB's answer depends on what
//! follows the subtree in its column family (`queries.rs`).

use std::{ops::Range, path::Path};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::proofs::query::{AggregateFold, AxisProjection, AxisQuery};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    indexed::{BigSum, Count, CountBigSum, CountSum, Indexed, Sum, SumAndAverage},
    nested::{check, open, segments, Answer, Log, Recorder},
    queries::{
        assert_same_calls, exact, kind_of, named, note, path, tally, verification, ANOTHER_ROOT,
        NOT_VERIFIED,
    },
    LigninEngine,
};
use crate::{
    engine::Engine, query_result_type::QueryResultType, Element, GroveDb, PathQuery, PathQueryRun,
    Query,
};

const IDX: &[u8] = b"idx";
const OWNERS: &[u8] = b"owners";

fn leaf_key(n: usize) -> Vec<u8> {
    format!("leaf{n:02}").into_bytes()
}

/// The indexed trees of a history: one at the root, and one under each of
/// two owners.
fn trees() -> [Vec<Vec<u8>>; 3] {
    [
        path(&[IDX]),
        path(&[OWNERS, b"a", IDX]),
        path(&[OWNERS, b"b", IDX]),
    ]
}

/// Writes children into the indexed tree at `tree`, through the
/// transaction of `recorder` or through none: the leaves `leaves`, through
/// the entry point of the kind, and a child that is a tree, with
/// `salt + 2` elements put into it below the primary.
fn fill<T: Indexed, E: Engine>(
    recorder: &mut Recorder<'_, '_, '_, E>,
    tree: &[Vec<u8>],
    leaves: Range<usize>,
    salt: usize,
) {
    let (db, through, version) = (recorder.db, recorder.through, recorder.version);
    let at = segments(tree);
    for n in leaves {
        let inserted = T::insert(db, &at, &leaf_key(n), T::leaf(n + salt), through, version);
        recorder.log.answer("insert a leaf".to_owned(), inserted);
    }
    let branch = format!("branch{salt}").into_bytes();
    let inserted = T::insert(db, &at, &branch, T::branch(), through, version);
    recorder.log.answer("insert a branch".to_owned(), inserted);
    let mut below = tree.to_vec();
    below.push(branch);
    for n in 0..salt + 2 {
        recorder.insert(&below, &leaf_key(n), T::in_branch(n + salt));
    }
}

/// Makes the grove of a history, with no transaction.
fn grove<T: Indexed, E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>) {
    recorder.insert(&[], IDX, T::empty());
    recorder.insert(&[], OWNERS, Element::empty_tree());
    for owner in [b"a", b"b", b"c"] {
        recorder.insert(&path(&[OWNERS]), owner, Element::empty_tree());
    }
    for owner in [b"a", b"b"] {
        recorder.insert(&path(&[OWNERS, owner]), IDX, T::empty());
    }
    for (at, tree) in trees().iter().enumerate() {
        fill::<T, E>(recorder, tree, 0..9 - 3 * at, at);
    }
}

/// What a history writes between two rounds of reads, through the
/// transaction of `recorder` or through none. In each indexed tree: a new
/// leaf, a leaf written again with another value, a leaf deleted, and an
/// element more in the child that is a tree, so that rows of every axis
/// move. `salt` tells the rounds apart.
fn change<T: Indexed, E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, salt: usize) {
    let (db, through, version) = (recorder.db, recorder.through, recorder.version);
    for (at, tree) in trees().iter().enumerate() {
        let segments = segments(tree);
        let new = T::leaf(11 * salt + at);
        let inserted = T::insert(db, &segments, &leaf_key(20 + salt), new, through, version);
        recorder.log.answer("insert a leaf".to_owned(), inserted);
        let again = T::leaf(30 + salt + at);
        let inserted = T::insert(db, &segments, &leaf_key(0), again, through, version);
        recorder.log.answer("replace a leaf".to_owned(), inserted);
        let deleted = T::delete(db, &segments, &leaf_key(salt), through, version);
        recorder.log.answer("delete a leaf".to_owned(), deleted);
        let mut below = tree.to_vec();
        below.push(format!("branch{at}").into_bytes());
        recorder.insert(&below, &leaf_key(10 + salt), T::in_branch(40 + salt));
    }
}

/// The rankings of a tree that are read by their place, each with the
/// rows it asks for, the place it starts at and whether it is read from
/// the highest value down: from either end with no offset, and from a
/// place the read has to count to.
const RANKINGS: [(&str, u16, u64, bool); 5] = [
    ("the first five", 5, 0, true),
    ("the first four, ascending", 4, 0, false),
    ("three from the third, ascending", 3, 2, false),
    ("two from the seventh", 2, 6, true),
    ("a page past the end", 4, 40, true),
];

/// The indexed trees whose axes are read one tree at a time, each with
/// the number of its children, which no round of writes changes: a round
/// adds a leaf and deletes one.
const READ_ALONE: [(&str, usize); 2] = [("root", 10), ("owned", 7)];

/// The writes of a history into its indexed trees, by the names of their
/// calls.
const WRITES: [&str; 4] = [
    "insert a leaf",
    "insert a branch",
    "replace a leaf",
    "delete a leaf",
];

/// The call under which the size of a page of an axis is noted.
const PAGE: &str = "page";

/// The reads of the axes of a kind of indexed tree, each with a name.
fn axis_queries<T: Indexed>() -> Vec<(String, PathQuery)> {
    let mut queries = Vec::new();
    for axis in T::axes() {
        for (tree, at) in [
            (READ_ALONE[0].0, path(&[IDX])),
            (READ_ALONE[1].0, path(&[OWNERS, b"a", IDX])),
        ] {
            let mut push = |what: &str, query: PathQuery| {
                queries.push((format!("{axis:?} of the {tree} tree: {what}"), query));
            };
            let at = || at.clone();
            let ranking = |k, offset, descending| {
                PathQuery::new_axis_top_k(at(), axis, k, offset, descending)
            };
            for (name, k, offset, descending) in RANKINGS {
                push(name, ranking(k, offset, descending));
            }
            let band =
                |lo, hi, limit| PathQuery::new_axis_bounded(at(), axis, lo, hi, limit, false);
            push("a band", band(-30, 30, 6));
            push("a band of the positive", band(0, i128::from(i64::MAX), 3));
            push("every value", band(i128::MIN, i128::MAX, 4));
            let rank = |n, descending| {
                PathQuery::new_axis_rank_of_key(at(), axis, leaf_key(n), descending)
            };
            push("the rank of a key", rank(0, true));
            push("the rank of a key, ascending", rank(5, false));
            push("the rank of no key", rank(77, true));
            for fold in [AggregateFold::Population, AggregateFold::Total] {
                let query =
                    PathQuery::new_axis_aggregate_over_value_range(at(), axis, -20, 60, fold);
                push(&format!("a fold, {fold:?}"), query);
            }
            let mut keys = AxisQuery::top_k(axis, 4, 1, true);
            keys.projection = AxisProjection::Keys;
            push("keys", PathQuery::new_axis(at(), keys));
        }
        // A band in descending order, where sums above it are in the
        // ranking: the leaves of the tree at the root that no round of
        // writes touches have sums of 44 and 54.
        if matches!(axis, IndexAxis::Sum | IndexAxis::BigSum) {
            queries.push((
                format!("{axis:?} of the root tree: a band, descending"),
                PathQuery::new_axis_bounded(path(&[IDX]), axis, -30, 0, 4, true),
            ));
        }
        // The tree of each owner at once: of two that have one, of one
        // that has none, and of one that is not there.
        for (offset, descending) in [(0, true), (1, true), (0, false), (1, false)] {
            let end = match descending {
                true => "",
                false => ", ascending",
            };
            queries.push((
                format!("{axis:?} of each owner's tree, from {offset}{end}"),
                PathQuery::new_branched_axis(
                    path(&[OWNERS]),
                    path(&[b"a", b"b", b"c", b"nobody"]),
                    path(&[IDX]),
                    AxisQuery::top_k(axis, 2, offset, descending),
                ),
            ));
        }
    }
    queries
}

/// The children of each indexed tree as a query reads them: the primary
/// alone, and with the elements of the child that is a tree.
fn child_queries() -> Vec<(String, PathQuery)> {
    let mut queries = Vec::new();
    for tree in trees() {
        let name = String::from_utf8_lossy(&tree.concat()).into_owned();
        let children = PathQuery::new_unsized(tree.clone(), Query::new_range_full());
        queries.push((format!("the children of {name}"), children));
        let mut below = Query::new_range_full();
        below.set_subquery(Query::new_range_full());
        let below = PathQuery::new_unsized(tree, below);
        queries.push((format!("the children of {name} and theirs"), below));
    }
    queries
}

/// Reads every axis, the children and the element of each indexed tree
/// through the transaction of `recorder`, or through none.
fn read<T: Indexed, E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, phase: &str) {
    let (db, through, version) = (recorder.db, recorder.through, recorder.version);
    let rows = QueryResultType::QueryPathKeyElementTrioResultType;
    for (name, query) in axis_queries::<T>() {
        let run = db.run_path_query(&query, true, true, true, rows, through, version);
        // The size of a page of entries, which `on_both_engines` holds to
        // the children the history wrote.
        if let Ok(PathQueryRun::AxisEntries { entries, skipped }) = &run.value {
            let size = format!("{} rows, skipped {skipped:?}", entries.len());
            note(recorder.log, named(phase, PAGE, &name), size);
        }
        recorder
            .log
            .answer(named(phase, "run", &name), exact(run, version));
    }
    for (name, query) in child_queries() {
        let read = db.query_raw(&query, true, true, true, rows, through, version);
        recorder
            .log
            .answer(named(phase, "query raw", &name), exact(read, version));
    }
    for tree in trees() {
        let (key, above) = tree.split_last().expect("a tree under a key");
        let element = db.get(segments(above).as_slice(), key, through, version);
        let name = String::from_utf8_lossy(&tree.concat()).into_owned();
        recorder
            .log
            .answer(named(phase, "get", &name), exact(element, version));
    }
}

/// Proves every read of [`read`] that is a path query from the committed
/// state, and has the verifier check each proof that was made.
fn prove<T: Indexed, E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, phase: &str) {
    let (db, version) = (recorder.db, recorder.version);
    let root = db.root_hash(None, version).unwrap().expect("a root hash");
    let mut queries = axis_queries::<T>();
    queries.extend(child_queries());
    for (name, query) in queries {
        let proved = db.prove_query(&query, None, version);
        if let Ok(proof) = &proved.value {
            let verified = GroveDb::verify_path_query(proof, &query, version);
            let verified =
                verified.map(|verified| (*verified.root_hash(), format!("{verified:?}")));
            let (call, verified) = verification(verified, &root);
            note(recorder.log, named(phase, call, &name), verified);
        }
        recorder.log.answer(named(phase, "prove", &name), proved);
    }
}

/// The rounds of reads of a history, in their order.
const PHASES: [&str; 5] = [
    "committed",
    "snapshot",
    "transaction",
    "transaction, after a commit beside it",
    "committed again",
];

/// The history of one kind of indexed tree on one engine.
fn reads_and_proofs<T: Indexed, E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    grove::<T, E>(&mut Recorder::new(&db, version, None, &mut log));
    let mut committed = Recorder::new(&db, version, None, &mut log);
    read::<T, E>(&mut committed, PHASES[0]);
    prove::<T, E>(&mut committed, PHASES[0]);

    let snapshot = db.start_snapshot_read_transaction();
    change::<T, E>(&mut Recorder::new(&db, version, None, &mut log), 1);
    let mut pinned = Recorder::new(&db, version, Some(&snapshot), &mut log);
    read::<T, E>(&mut pinned, PHASES[1]);
    drop(snapshot);

    let transaction = db.start_transaction();
    let mut inside = Recorder::new(&db, version, Some(&transaction), &mut log);
    change::<T, E>(&mut inside, 2);
    read::<T, E>(&mut inside, PHASES[2]);
    let mut outside = Recorder::new(&db, version, None, &mut log);
    let beside = path(&[OWNERS, b"c"]);
    outside.insert(&beside, b"beside", Element::new_item(b"beside".to_vec()));
    let mut inside = Recorder::new(&db, version, Some(&transaction), &mut log);
    read::<T, E>(&mut inside, PHASES[3]);
    db.rollback_transaction(&transaction).expect("a rollback");
    drop(transaction);

    let mut committed = Recorder::new(&db, version, None, &mut log);
    read::<T, E>(&mut committed, PHASES[4]);
    prove::<T, E>(&mut committed, PHASES[4]);
    db.flush().expect("a flush");
    log
}

/// Runs the history of the kind `T` on every grove version, on LigninDB
/// and where the RocksDB engine is compiled in on RocksDB too, and
/// compares every call. `has_kind` says which grove versions, by their
/// numbers, make the kind.
fn on_both_engines<T: Indexed>(has_kind: impl Fn(usize) -> bool) {
    for (number, version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let log = reads_and_proofs::<T, LigninEngine>(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = reads_and_proofs::<T, crate::RocksDbEngine>(dir.path(), version);
            assert_same_calls(&log.answers, &on_rocksdb.answers);
        }
        check(dir.path(), version);
        let of = |call: &str| tally(&log).get(call).copied().unwrap_or_default();
        eprintln!(
            "{} on grove version {number}: {} calls; run {:?}, query raw {:?}, prove {:?}, \
             verified {:?}",
            T::NAME,
            log.answers.len(),
            of("run"),
            of("query raw"),
            of("prove"),
            of("verified"),
        );
        // The verifier took every proof that was made, and came to the
        // committed root hash by it.
        let refused: Vec<_> = (log.answers.iter())
            .filter(|answer| [NOT_VERIFIED, ANOTHER_ROOT].contains(&kind_of(answer).0.as_str()))
            .collect();
        assert!(refused.is_empty(), "{refused:#?}");
        assert_eq!(of("verified").0, of("prove").1);
        if !has_kind(number) {
            continue;
        }
        // A version that makes the kind takes every write of the history
        // into its indexed trees: each insert is answered, and each
        // delete removes the leaf it names.
        for write in WRITES {
            let taken = |answer: &&Answer| match write {
                "delete a leaf" => answer.2 == "Ok(true)",
                _ => answer.2.starts_with("Ok"),
            };
            let made = log.answers.iter().filter(|answer| answer.0 == write);
            let (made, refused): (Vec<_>, Vec<_>) = made.partition(taken);
            assert!(
                !made.is_empty() && refused.is_empty(),
                "{write}: {refused:#?}"
            );
        }
        // The reads of an axis are of grove version 4. From there on, a
        // version that makes the kind answers each ranking of
        // [`RANKINGS`], of every axis of the two trees of
        // [`READ_ALONE`], in every round with the rows and the skip that
        // the children of the tree give, the rankings with an offset
        // among them, and proves each in both rounds of proofs.
        if number < 4 {
            continue;
        }
        let axes = T::axes();
        for phase in PHASES {
            for (ranking, k, offset, _) in RANKINGS {
                for (tree, children) in READ_ALONE {
                    let skipped = (offset as usize).min(children);
                    let rows = usize::from(k).min(children - skipped);
                    let expected = format!("{rows} rows, skipped Some({skipped})");
                    for axis in &axes {
                        let name = format!("{axis:?} of the {tree} tree: {ranking}");
                        let page = named(phase, PAGE, &name);
                        let noted = log.answers.iter().filter(|answer| answer.0 == page);
                        let noted: Vec<&str> = noted.map(|answer| answer.2.as_str()).collect();
                        assert_eq!(noted, [expected.as_str()], "{page}");
                    }
                }
            }
        }
        let proven = log.answers.iter().filter(|answer| {
            kind_of(answer) == ("prove".to_owned(), true)
                && (RANKINGS.iter()).any(|ranking| answer.0.ends_with(&format!(": {}", ranking.0)))
        });
        assert_eq!(proven.count(), 2 * 2 * axes.len() * RANKINGS.len());
    }
}

#[test]
fn the_axes_of_a_count_indexed_tree_are_read_and_proven_as_on_rocksdb() {
    on_both_engines::<Count>(|_| true);
}

#[test]
fn the_axes_of_a_sum_indexed_tree_are_read_and_proven_as_on_rocksdb() {
    on_both_engines::<Sum>(|_| true);
}

#[test]
fn the_axes_of_a_count_and_sum_indexed_tree_are_read_and_proven_as_on_rocksdb() {
    on_both_engines::<CountSum>(|_| true);
}

#[test]
fn the_axes_of_an_indexed_tree_with_two_of_three_axes_are_read_and_proven_as_on_rocksdb() {
    on_both_engines::<SumAndAverage>(|_| true);
}

#[test]
fn the_axes_of_a_big_sum_indexed_tree_are_read_and_proven_as_on_rocksdb() {
    on_both_engines::<BigSum>(|number| number >= 5);
}

#[test]
fn the_axes_of_a_count_and_big_sum_indexed_tree_are_read_and_proven_as_on_rocksdb() {
    on_both_engines::<CountBigSum>(|number| number >= 5);
}

/// A read of an axis keeps no version pinned once it has returned, through
/// a write transaction and through none: the cursor it points at the
/// secondary of its tree is dropped with the read.
#[test]
fn a_read_of_an_axis_keeps_no_version_pinned_once_it_has_returned() {
    let version = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<LigninEngine>(dir.path());
    let mut log = Log::default();
    grove::<CountSum, _>(&mut Recorder::new(&db, version, None, &mut log));
    let pins = || db.engine().database().stats().pins;
    assert_eq!(pins(), 0, "nothing is open");
    let transaction = db.start_transaction();
    change::<CountSum, _>(
        &mut Recorder::new(&db, version, Some(&transaction), &mut log),
        1,
    );
    let held = pins();
    for through in [None, Some(&transaction)] {
        let mut recorder = Recorder::new(&db, version, through, &mut log);
        read::<CountSum, _>(&mut recorder, "pins");
        assert_eq!(pins(), held);
    }
    prove::<CountSum, _>(&mut Recorder::new(&db, version, None, &mut log), "pins");
    assert_eq!(pins(), held);
    let answered =
        (log.answers.iter()).filter(|answer| kind_of(answer) == ("run".to_owned(), true));
    assert!(answered.count() > 40, "the reads were made");
    db.rollback_transaction(&transaction).expect("a rollback");
    drop(transaction);
    assert_eq!(pins(), 0, "nothing is open");
}

/// A move of a cursor that is refused inside a scan of an indexed tree
/// fails the operation on both engines alike: the history of
/// [`refused_scans_of_an_indexed_tree`](crate::tests::refused_scans::refused_scans_of_an_indexed_tree)
/// gives the same answers at the same costs.
#[test]
fn a_move_refused_inside_a_scan_of_an_indexed_tree_fails_the_operation_as_on_rocksdb() {
    use crate::tests::refused_scans::refused_scans_of_an_indexed_tree;

    let version = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let log = refused_scans_of_an_indexed_tree::<LigninEngine>(dir.path(), version);
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let on_rocksdb =
            refused_scans_of_an_indexed_tree::<crate::RocksDbEngine>(dir.path(), version);
        assert_same_calls(&log, &on_rocksdb);
    }
    check(dir.path(), version);
    assert!(log.len() > 40, "{} calls", log.len());
}
