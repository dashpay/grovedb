//! The indexed trees on the LigninDB engine: an element that hosts its
//! primary Merk and one secondary Merk for each of its axes.
//!
//! Each of the five kinds has a history, written once for any engine and
//! run on LigninDB and on RocksDB on every grove version, with every
//! answer, every `OperationCost` and the root hash after every write
//! compared (`append_only`). The tests after them are of what the engine
//! itself does with such an element: which subtrees it enters, where each
//! hangs, and what it refuses.

use grovedb_costs::{CostResult, OperationCost};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    proofs::{query::AggregateFold, Query},
    store::lignin::{LigninEntry, LigninHost, LigninNodeStore, LigninSubtree},
    TreeType,
};
use grovedb_storage::{
    prefix::{build_prefix, secondary_prefix_for},
    Error as StorageError, FlatStore,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, on_both_engines, sound, taken, verified, Answer, History},
    Lignin, LigninEngine,
};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::Engine,
    operations::proof::indexed_axis::AxisEntries,
    query_result_type::{IndexedAxisEntry, QueryResultType},
    Element, Error, GroveDb, PathQuery, PathQueryRun, TransactionArg,
};

const ROOT: &[&[u8]] = &[];

/// One kind of indexed tree, as its history uses it.
pub(super) trait Indexed {
    /// What the history calls the tree.
    const NAME: &'static str;

    /// The element of an empty tree.
    fn empty() -> Element;

    /// The axes the tree keeps, in the order its element has them.
    fn axes() -> Vec<IndexAxis>;

    /// Inserts `child` under `key` through the entry point of the kind.
    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Deletes the child `key` through the entry point of the kind.
    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error>;

    /// The `n`th child that is no tree.
    fn leaf(n: usize) -> Element {
        Element::new_sum_item(sum(n))
    }

    /// An empty tree that the kind takes as a child.
    fn branch() -> Element;

    /// The `n`th element put into such a child.
    fn in_branch(n: usize) -> Element {
        Element::new_sum_item(sum(n + 50))
    }
}

fn sum(n: usize) -> i64 {
    (n as i64 * 37) % 101 - 40
}

pub(super) struct Count;

impl Indexed for Count {
    const NAME: &'static str = "count";

    fn empty() -> Element {
        Element::empty_provable_count_indexed_tree()
    }

    fn axes() -> Vec<IndexAxis> {
        vec![IndexAxis::Count]
    }

    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error> {
        db.insert_into_count_indexed_tree(path, key, child, through, v)
    }

    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error> {
        db.delete_from_count_indexed_tree(path, key, through, v)
    }

    fn leaf(n: usize) -> Element {
        Element::new_item(vec![n as u8; 4 + n])
    }

    fn branch() -> Element {
        Element::empty_count_tree()
    }

    fn in_branch(n: usize) -> Element {
        Element::new_item(vec![n as u8 ^ 0x11; 3 + n])
    }
}

pub(super) struct Sum;

impl Indexed for Sum {
    const NAME: &'static str = "sum";

    fn empty() -> Element {
        Element::empty_provable_sum_indexed_tree()
    }

    fn axes() -> Vec<IndexAxis> {
        vec![IndexAxis::Sum]
    }

    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error> {
        db.insert_into_provable_sum_indexed_tree(path, key, child, through, v)
    }

    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error> {
        db.delete_from_provable_sum_indexed_tree(path, key, through, v)
    }

    fn branch() -> Element {
        Element::empty_sum_tree()
    }
}

pub(super) struct CountSum;

impl Indexed for CountSum {
    const NAME: &'static str = "count and sum";

    fn empty() -> Element {
        let axes = Self::axes().into_iter().map(|axis| (axis.tag(), None));
        Element::empty_provable_count_provable_sum_indexed_tree(axes.collect()).expect("axes")
    }

    fn axes() -> Vec<IndexAxis> {
        vec![IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
    }

    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error> {
        db.insert_into_provable_count_provable_sum_indexed_tree(path, key, child, through, v)
    }

    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error> {
        db.delete_from_provable_count_provable_sum_indexed_tree(path, key, through, v)
    }

    fn leaf(n: usize) -> Element {
        Element::new_item_with_sum_item(vec![n as u8; 4 + n], sum(n))
    }

    fn branch() -> Element {
        Element::empty_count_sum_tree()
    }
}

/// A tree with a count and a sum that keeps two of the three axes: its
/// second Merk is the one of the sum, and its third the one of the
/// average.
pub(super) struct SumAndAverage;

impl Indexed for SumAndAverage {
    const NAME: &'static str = "sum and average";

    fn empty() -> Element {
        let axes = Self::axes().into_iter().map(|axis| (axis.tag(), None));
        Element::empty_provable_count_provable_sum_indexed_tree(axes.collect()).expect("axes")
    }

    fn axes() -> Vec<IndexAxis> {
        vec![IndexAxis::Sum, IndexAxis::Avg]
    }

    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error> {
        CountSum::insert(db, path, key, child, through, v)
    }

    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error> {
        CountSum::delete(db, path, key, through, v)
    }

    fn leaf(n: usize) -> Element {
        CountSum::leaf(n)
    }

    fn branch() -> Element {
        CountSum::branch()
    }
}

pub(super) struct BigSum;

impl Indexed for BigSum {
    const NAME: &'static str = "big sum";

    fn empty() -> Element {
        Element::empty_provable_big_sum_indexed_tree()
    }

    fn axes() -> Vec<IndexAxis> {
        vec![IndexAxis::BigSum]
    }

    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error> {
        db.insert_into_provable_big_sum_indexed_tree(path, key, child, through, v)
    }

    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error> {
        db.delete_from_provable_big_sum_indexed_tree(path, key, through, v)
    }

    fn branch() -> Element {
        Element::empty_big_sum_tree()
    }
}

pub(super) struct CountBigSum;

impl Indexed for CountBigSum {
    const NAME: &'static str = "count and big sum";

    fn empty() -> Element {
        let axes = Self::axes().into_iter().map(|axis| (axis.tag(), None));
        Element::empty_provable_count_provable_big_sum_indexed_tree(axes.collect()).expect("axes")
    }

    fn axes() -> Vec<IndexAxis> {
        vec![IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]
    }

    fn insert<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        child: Element,
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<(), Error> {
        db.insert_into_provable_count_provable_big_sum_indexed_tree(path, key, child, through, v)
    }

    fn delete<'db, E: Engine>(
        db: &'db GroveDb<E>,
        path: &[&[u8]],
        key: &[u8],
        through: TransactionArg<'db, '_, E>,
        v: &GroveVersion,
    ) -> CostResult<bool, Error> {
        db.delete_from_provable_count_provable_big_sum_indexed_tree(path, key, through, v)
    }

    fn branch() -> Element {
        Element::empty_count_big_sum_tree()
    }
}

fn owned(path: &[&[u8]]) -> Vec<Vec<u8>> {
    path.iter().map(|segment| segment.to_vec()).collect()
}

/// Every read of the axes of the indexed tree at `path`, with no
/// transaction: the ranking from both ends with and without an offset,
/// bands of values in ascending order, the rank of a key that is there
/// and of one that is not, the two folds over a band, and the proofs of
/// a ranking and of a rank.
fn axis_reads<T: Indexed, E: Engine>(h: &mut History<'_, E>, path: &[&[u8]]) {
    axis_reads_of::<T, E>(h, path, "");
}

/// The bytes of the element in each row that a read of an axis gave.
fn row_bytes(run: &PathQueryRun, v: &GroveVersion) -> Vec<Vec<u8>> {
    fn of<T>(rows: &[IndexedAxisEntry<T>], v: &GroveVersion) -> Vec<Vec<u8>> {
        let bytes = |row: &IndexedAxisEntry<T>| row.value.serialize(v).expect("an element");
        rows.iter().map(bytes).collect()
    }
    match run {
        PathQueryRun::AxisEntries { entries, .. } => match entries {
            AxisEntries::Count(rows) => of(rows, v),
            AxisEntries::Sum(rows) => of(rows, v),
            AxisEntries::Avg(rows) | AxisEntries::BigSum(rows) => of(rows, v),
        },
        _ => Vec::new(),
    }
}

/// [`axis_reads`], with `of` after the name of each call.
fn axis_reads_of<T: Indexed, E: Engine>(h: &mut History<'_, E>, path: &[&[u8]], of: &str) {
    let (db, v) = (h.db, h.v);
    let mut run = |name: &str, axis: IndexAxis, query: PathQuery| {
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        let answer = db.run_path_query(&query, true, true, true, pairs, None, v);
        // A row with the bytes of its element: the text of an element
        // leaves out what two elements can differ in.
        let answer = answer.map_ok(|run| {
            let bytes = row_bytes(&run, v);
            (run, bytes)
        });
        h.read(format!("{name} {axis:?}{of}"), answer);
    };
    for axis in T::axes() {
        let at = || owned(path);
        run(
            "top",
            axis,
            PathQuery::new_axis_top_k(at(), axis, 5, 0, true),
        );
        run(
            "top",
            axis,
            PathQuery::new_axis_top_k(at(), axis, 3, 2, false),
        );
        run(
            "top",
            axis,
            PathQuery::new_axis_top_k(at(), axis, 4, 40, true),
        );
        let band = |lo, hi, limit| PathQuery::new_axis_bounded(at(), axis, lo, hi, limit, false);
        run("band", axis, band(-30, 30, 6));
        run("band", axis, band(0, i128::from(i64::MAX), 3));
        // An average is a fixed-point number: a band of small values
        // holds none, and this one holds every one.
        if axis == IndexAxis::Avg {
            run("whole band", axis, band(i128::MIN, i128::MAX, 4));
        }
        let rank = |key: &[u8]| PathQuery::new_axis_rank_of_key(at(), axis, key.to_vec(), true);
        run("rank", axis, rank(b"leaf02"));
        run("rank missing", axis, rank(b"missing"));
        for fold in [AggregateFold::Population, AggregateFold::Total] {
            let query = PathQuery::new_axis_aggregate_over_value_range(at(), axis, -20, 60, fold);
            run(&format!("fold {fold:?}"), axis, query);
        }
    }
    // Every child of the primary with its bytes: what an axis read gives
    // of an element is its text.
    let children = PathQuery::new_unsized(owned(path), Query::new_range_full());
    let pairs = QueryResultType::QueryKeyElementPairResultType;
    h.elements(
        format!("children{of}"),
        db.query_raw(&children, true, true, true, pairs, None, v),
    );
    for axis in T::axes() {
        let ranking = PathQuery::new_axis_top_k(owned(path), axis, 4, 1, true);
        h.read("proof", db.prove_query(&ranking, None, v));
        let rank = PathQuery::new_axis_rank_of_key(owned(path), axis, b"leaf03".to_vec(), false);
        h.read("proof", db.prove_query(&rank, None, v));
    }
}

/// The history of one kind of indexed tree on the engine `E`.
///
/// A tree of the kind at the root and one under a tree; children that are
/// no trees, inserted through the kind's entry point one by one; children
/// that are trees, filled through the generic insert below the primary, so
/// that each write below moves a row of every axis; a child replaced and
/// children deleted, at the primary and below it; the generic insert and
/// delete at the primary, which GroveDB refuses; batches at the primary,
/// below it, across two indexed trees and beside an ordinary item, in a
/// transaction that commits and in one that is rolled back, and one whose
/// last operation fails; a tree that one batch makes and fills; the repair
/// of the secondaries; and the reads of every axis after each stage.
fn history<T: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let leaf_key = |n: usize| format!("leaf{n:02}").into_bytes();
    let top: &[&[u8]] = &[b"idx"];
    let nested: &[&[u8]] = &[b"docs", b"idx"];
    let docs: &[&[u8]] = &[b"docs"];
    let in_b0: &[&[u8]] = &[b"idx", b"b0"];
    let in_b1: &[&[u8]] = &[b"idx", b"b1"];
    let nowhere: &[&[u8]] = &[b"nothing", b"here"];

    h.write("made", db.insert(ROOT, b"idx", T::empty(), None, None, v));
    h.write(
        "tree",
        db.insert(ROOT, b"docs", Element::empty_tree(), None, None, v),
    );
    h.write("made", db.insert(docs, b"idx", T::empty(), None, None, v));
    h.element("element", db.get(ROOT, b"idx", None, v));
    h.read("empty", db.is_empty_tree(top, None, v));
    axis_reads::<T, E>(&mut h, top);

    // Children that are no trees, one by one.
    for n in 0..9 {
        let inserted = T::insert(db, top, &leaf_key(n), T::leaf(n), None, v);
        h.write("insert", inserted);
        if n % 4 == 0 {
            h.element("element", db.get(ROOT, b"idx", None, v));
            h.element("child", db.get(top, &leaf_key(n), None, v));
        }
    }
    axis_reads::<T, E>(&mut h, top);
    for n in 0..3 {
        let inserted = T::insert(db, nested, &leaf_key(n), T::leaf(n + 20), None, v);
        h.write("insert", inserted);
    }

    // Children that are trees, filled from below the primary.
    for (branch, filled) in [(b"b0".as_slice(), 1), (b"b1", 4), (b"b2", 2)] {
        h.write("insert", T::insert(db, top, branch, T::branch(), None, v));
        for n in 0..filled {
            let below: &[&[u8]] = &[b"idx", branch];
            let key = format!("in{n}").into_bytes();
            h.write(
                "below",
                db.insert(below, &key, T::in_branch(n), None, None, v),
            );
        }
    }
    h.element("element", db.get(ROOT, b"idx", None, v));
    h.element("child", db.get(top, b"b1", None, v));
    axis_reads::<T, E>(&mut h, top);

    // A child replaced, and children deleted at the primary and below it.
    h.write(
        "insert",
        T::insert(db, top, &leaf_key(2), T::leaf(30), None, v),
    );
    h.write("delete", T::delete(db, top, &leaf_key(5), None, v));
    h.write("delete", T::delete(db, top, b"missing", None, v));
    h.write("below", db.delete(in_b1, b"in2", None, None, v));
    axis_reads::<T, E>(&mut h, top);

    // The generic insert and delete at the primary.
    h.write(
        "generic",
        db.insert(top, b"generic", T::leaf(40), None, None, v),
    );
    h.write("generic", db.delete(top, &leaf_key(0), None, None, v));

    // Batches.
    let put = |path: &[&[u8]], key: &[u8], element: Element| {
        QualifiedGroveDbOp::insert_or_replace_op(owned(path), key.to_vec(), element)
    };
    let batch = vec![
        put(top, b"batch0", T::leaf(41)),
        put(top, b"batch1", T::leaf(42)),
        QualifiedGroveDbOp::delete_op(owned(top), leaf_key(7)),
    ];
    h.write("batch", db.apply_batch(batch, None, None, v));
    let batch = vec![
        put(top, b"batch2", T::leaf(43)),
        put(nested, b"batch3", T::leaf(44)),
        put(in_b0, b"in9", T::in_branch(9)),
        put(docs, b"item", Element::new_item(b"beside".to_vec())),
    ];
    h.write("batch", db.apply_batch(batch, None, None, v));
    axis_reads::<T, E>(&mut h, top);
    axis_reads::<T, E>(&mut h, nested);

    let tx = db.start_transaction();
    let batch = vec![
        put(top, b"batch4", T::leaf(45)),
        put(top, b"batch5", T::leaf(46)),
    ];
    let applied = db.apply_batch(batch, None, Some(&tx), v);
    h.write_in("batch", applied, Some(&tx));
    let inserted = T::insert(db, top, b"in tx", T::leaf(47), Some(&tx), v);
    h.write_in("insert", inserted, Some(&tx));
    h.element("element", db.get(ROOT, b"idx", Some(&tx), v));
    h.element("element", db.get(ROOT, b"idx", None, v));
    h.write("commit", db.commit_transaction(tx));
    axis_reads::<T, E>(&mut h, top);

    let tx = db.start_transaction();
    let inserted = T::insert(db, top, b"undone", T::leaf(48), Some(&tx), v);
    h.write_in("insert", inserted, Some(&tx));
    let deleted = T::delete(db, top, &leaf_key(1), Some(&tx), v);
    h.write_in("delete", deleted, Some(&tx));
    h.plain("rollback", db.rollback_transaction(&tx));
    drop(tx);
    h.read("root", db.root_hash(None, v));

    let failing = vec![
        put(top, b"never", T::leaf(49)),
        put(nowhere, b"key", Element::new_item(vec![1])),
    ];
    h.write("failing batch", db.apply_batch(failing, None, None, v));
    h.element("child", db.get(top, b"never", None, v));

    // A tree that one batch makes and fills.
    let fresh: &[&[u8]] = &[b"fresh"];
    let batch = vec![
        put(ROOT, b"fresh", T::empty()),
        put(fresh, b"one", T::leaf(50)),
        put(fresh, b"two", T::leaf(51)),
    ];
    h.write("fresh batch", db.apply_batch(batch, None, None, v));
    h.element("element", db.get(ROOT, b"fresh", None, v));
    axis_reads::<T, E>(&mut h, fresh);
    h.write(
        "insert",
        T::insert(db, fresh, b"three", T::leaf(52), None, v),
    );
    axis_reads::<T, E>(&mut h, fresh);

    // The repair of the secondaries, which finds nothing to repair.
    h.write(
        "repair",
        db.reconcile_indexed_tree_secondaries(top, None, v),
    );
    axis_reads::<T, E>(&mut h, top);

    // An indexed tree that nothing was written into is deleted.
    h.write(
        "made",
        db.insert(ROOT, b"unused", T::empty(), None, None, v),
    );
    h.write("delete", db.delete(ROOT, b"unused", None, None, v));
    h.element("element", db.get(ROOT, b"unused", None, v));
    verified(&mut h);
    h.answers
}

/// What a history must have done on the grove version `number`: on a
/// version that has the kind, every tree is made, every insert through
/// the kind's entry point and every write below the primary is taken, and
/// each batch is applied. The grove's own check finds nothing wrong, and
/// every axis is proven where the grove version proves one.
fn told<T: Indexed>(has_kind: impl Fn(usize) -> bool) -> impl Fn(&[Answer], usize) {
    let (name, axes) = (T::NAME, T::axes());
    move |answers, number| {
        let count = |call: &str| taken(answers, call);
        let seen = [
            count("made"),
            count("insert"),
            count("below"),
            count("batch"),
            count("fresh batch"),
            count("repair"),
        ];
        assert!(sound(answers), "{name} on version {number}");
        // The reads of an axis are of grove version 4. A history reads
        // every axis at ten stages. Each read of a ranking and of a band
        // is answered, 17 of the 30 rankings and 18 of the 20 bands with
        // rows; the rank of a key is answered at the seven stages that
        // have the key; the population of a band is answered for every
        // axis but the average, and its total for the axes with a 64-bit
        // value. The folds of the other axes are refused before a read.
        if has_kind(number) && number >= 4 {
            let rows = |call: &str| with_rows(answers, call);
            for axis in &axes {
                let of = |call: &str| format!("{call} {axis:?}");
                let (average, wide) = (*axis == IndexAxis::Avg, *axis == IndexAxis::BigSum);
                let seen = [
                    count(&of("top")),
                    rows(&of("top")),
                    count(&of("band")),
                    rows(&of("band")),
                    count(&of("rank")),
                    count(&of("fold Population")),
                    count(&of("fold Total")),
                ];
                // A band of raw values holds no average, which is a
                // fixed-point number.
                let expected = [
                    30,
                    17,
                    20,
                    if average { 0 } else { 18 },
                    7,
                    if average { 0 } else { 10 },
                    if average || wide { 0 } else { 10 },
                ];
                assert_eq!(seen, expected, "{name}, {axis:?}, on version {number}");
                // A rank that is answered is a rank, at the seven stages
                // that have the key, and the key that no stage has is
                // told as a key that is not there.
                let ranks = [
                    opening(answers, &of("rank"), RANK),
                    opening(answers, &of("rank missing"), NO_RANK),
                ];
                assert_eq!(ranks, [7, 10], "{name}, {axis:?}, on version {number}");
                // A fold that is not answered is refused as a fold the
                // axis does not have, and by no other error.
                for fold in [of("fold Population"), of("fold Total")] {
                    let refused = refused_folds(answers, &fold);
                    assert_eq!(
                        count(&fold) + refused,
                        10,
                        "{name}, {fold}, on version {number}"
                    );
                }
                // Every average is in the whole band, at each of the
                // nine stages that have a child.
                if average {
                    let whole = [count(&of("whole band")), rows(&of("whole band"))];
                    assert_eq!(whole, [10, 9], "{name} on version {number}");
                }
            }
            assert_eq!(count("children"), 10, "{name} on version {number}");
        }
        // The proofs of an axis are of grove version 4.
        if has_kind(number) && number >= 4 {
            assert_eq!(
                count("proof"),
                16 * axes.len(),
                "{name} on version {number}"
            );
        }
        match has_kind(number) {
            true => assert_eq!(seen, [3, 19, 8, 3, 1, 1], "{name} on version {number}"),
            false => assert_eq!(seen[..2], [0, 0], "{name} on version {number}"),
        }
    }
}

#[test]
fn a_count_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Count, _>, told::<Count>(|_| true));
}

#[test]
fn a_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Sum, _>, told::<Sum>(|_| true));
}

#[test]
fn a_count_and_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<CountSum, _>, told::<CountSum>(|_| true));
}

#[test]
fn an_indexed_tree_with_two_of_three_axes_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<SumAndAverage, _>, told::<SumAndAverage>(|_| true));
}

#[test]
fn a_big_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<BigSum, _>, told::<BigSum>(|number| number >= 5));
}

#[test]
fn a_count_and_big_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(
        history::<CountBigSum, _>,
        told::<CountBigSum>(|number| number >= 5)
    );
}

/// An indexed tree of the kind in the primary of another: the inner tree
/// is a child of the outer, and each write into it moves its rows in the
/// secondaries of both.
fn nested<T: Indexed, E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let leaf_key = |n: usize| format!("leaf{n:02}").into_bytes();
    let top: &[&[u8]] = &[b"idx"];
    let inner: &[&[u8]] = &[b"idx", b"inner"];
    h.write("made", db.insert(ROOT, b"idx", T::empty(), None, None, v));
    h.write("nested", T::insert(db, top, b"inner", T::empty(), None, v));
    for n in 0..4 {
        let inserted = T::insert(db, inner, &leaf_key(n), T::leaf(n), None, v);
        h.write("insert", inserted);
    }
    h.write(
        "insert",
        T::insert(db, top, &leaf_key(0), T::leaf(9), None, v),
    );
    h.write("delete", T::delete(db, inner, &leaf_key(1), None, v));
    h.element("element", db.get(ROOT, b"idx", None, v));
    h.element("child", db.get(top, b"inner", None, v));
    axis_reads_of::<T, E>(&mut h, top, " outer");
    axis_reads_of::<T, E>(&mut h, inner, " inner");
    verified(&mut h);
    h.answers
}

/// How many of `answers` are of a call named `call` that gave rows.
fn with_rows(answers: &[Answer], call: &str) -> usize {
    let rows = |(named, _, answer): &&Answer| {
        named == call && answer.starts_with("Ok(") && answer.contains("primary_key")
    };
    answers.iter().filter(rows).count()
}

/// How many of `answers` are of a call named `call` whose answer opens
/// with `with`.
fn opening(answers: &[Answer], call: &str, with: &str) -> usize {
    let opens = |(named, _, answer): &&Answer| named == call && answer.starts_with(with);
    answers.iter().filter(opens).count()
}

/// The text that the rank of a key opens with, and the rank of a key the
/// tree does not hold.
const RANK: &str = "Ok((AxisRank(";
const NO_RANK: &str = "Err(PathKeyNotFound(";

/// The text that the refusal of a fold opens with: a fold that the axis
/// does not have is refused where the query is validated, before a read.
const FOLD_REFUSED: &str = "Err(InvalidQuery(";

/// How many of `answers` are of a call named `call` that was refused as
/// a fold the axis does not have.
fn refused_folds(answers: &[Answer], call: &str) -> usize {
    let refused = |(named, _, answer): &&Answer| named == call && answer.starts_with(FOLD_REFUSED);
    answers.iter().filter(refused).count()
}

/// On every grove version the inner tree is made and every write into it
/// is taken. From grove version 4 on, which has the reads of an axis,
/// each axis of the outer and of the inner tree is read: its three
/// rankings and two bands are answered, a ranking and a band of each with
/// rows, the whole band of the averages with rows, the rank of a key the
/// inner tree holds, and the folds each axis has.
fn nested_told<T: Indexed>() -> impl Fn(&[Answer], usize) {
    move |answers, number| {
        assert!(sound(answers), "version {number}");
        let count = |call: &str| taken(answers, call);
        let seen = [
            count("made"),
            count("nested"),
            count("insert"),
            count("delete"),
        ];
        assert_eq!(seen, [1, 1, 5, 1], "version {number}");
        if number < 4 {
            return;
        }
        for tree in ["outer", "inner"] {
            for axis in T::axes() {
                let of = |call: &str| format!("{call} {axis:?} {tree}");
                let average = axis == IndexAxis::Avg;
                let folds = [of("fold Population"), of("fold Total")];
                let seen = [
                    count(&of("top")),
                    count(&of("band")),
                    count(&folds[0]) + refused_folds(answers, &folds[0]),
                    count(&folds[1]) + refused_folds(answers, &folds[1]),
                    count(&folds[0]),
                    count(&folds[1]),
                ];
                let has = usize::from(!average);
                assert_eq!(
                    seen,
                    [3, 2, 1, 1, has, has],
                    "{axis:?} of {tree}, version {number}"
                );
                assert!(with_rows(answers, &of("top")) >= 1, "{axis:?} of {tree}");
                let band = if average { "whole band" } else { "band" };
                assert!(with_rows(answers, &of(band)) >= 1, "{axis:?} of {tree}");
            }
            assert_eq!(count(&format!("children {tree}")), 1, "version {number}");
        }
        // The inner tree holds `leaf02`, the outer does not, and neither
        // holds the other key. Each is asked once.
        for axis in T::axes() {
            let of = |call: &str, tree: &str, with: &str| {
                opening(answers, &format!("{call} {axis:?} {tree}"), with)
            };
            let seen = [
                of("rank", "inner", RANK),
                of("rank", "outer", NO_RANK),
                of("rank missing", "inner", NO_RANK),
                of("rank missing", "outer", NO_RANK),
            ];
            assert_eq!(seen, [1, 1, 1, 1], "{axis:?}, version {number}");
            let asked = [of("rank", "inner", ""), of("rank", "outer", "")];
            assert_eq!(asked, [1, 1], "{axis:?}, version {number}");
        }
    }
}

#[test]
fn a_sum_indexed_tree_in_a_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(nested::<Sum, _>, nested_told::<Sum>());
}

#[test]
fn a_count_and_sum_indexed_tree_in_another_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(nested::<CountSum, _>, nested_told::<CountSum>());
}

pub(super) fn entry(db: &Lignin, prefix: [u8; 32]) -> Option<LigninEntry> {
    let tx = db.start_transaction();
    let view = tx.write_transaction().expect("a write transaction");
    let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(prefix));
    handle.entry().expect("a read of the directory")
}

/// The first write into an indexed tree enters all of its Merks, each at
/// its position under the element and with its tree type, and no write
/// before it enters any.
#[test]
fn the_merks_of_an_indexed_tree_are_entered_together_under_their_element() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        db.insert(ROOT, b"idx", SumAndAverage::empty(), None, None, v)
            .unwrap()
            .expect("an indexed tree");
        let top: &[&[u8]] = &[b"idx"];
        let primary = build_prefix(top.into()).value;
        let secondary = |axis: IndexAxis| secondary_prefix_for(&primary, axis.tag()).value;
        let hosted = [
            primary,
            secondary(IndexAxis::Sum),
            secondary(IndexAxis::Avg),
        ];
        for prefix in hosted {
            assert_eq!(entry(&db, prefix), None, "before the first write");
        }

        SumAndAverage::insert(&db, top, b"one", SumAndAverage::leaf(1), None, v)
            .unwrap()
            .expect("a child");
        let kept = TreeType::ProvableCountProvableSumTree;
        let tree_types = [TreeType::ProvableCountProvableSumIndexedTree, kept, kept];
        for (position, (prefix, tree_type)) in hosted.into_iter().zip(tree_types).enumerate() {
            let entry = entry(&db, prefix).expect("an entry");
            assert_eq!(entry.tree_type().expect("a tree type"), tree_type);
            let host = entry.host.expect("a host");
            assert_eq!(
                (
                    host.subtree,
                    host.key.as_slice(),
                    usize::from(host.position)
                ),
                ([0; 32], b"idx".as_slice(), position)
            );
            assert!(
                entry.root_key.is_some(),
                "the Merk at {position} has a node"
            );
        }
        // The axis this element does not keep is no subtree of it.
        assert_eq!(entry(&db, secondary(IndexAxis::Count)), None);
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// The Merks of an indexed tree leave the directory with its element:
/// after the delete of the tree with the children it holds, and after a
/// flat drop of it, none of them is entered, and the key takes an indexed
/// tree of another kind.
#[test]
fn the_merks_of_an_indexed_tree_leave_with_its_element() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        for (name, flat) in [(b"deleted", false), (b"dropped", true)] {
            let top: &[&[u8]] = &[name];
            db.insert(ROOT, name, SumAndAverage::empty(), None, None, v)
                .unwrap()
                .expect("an indexed tree");
            for (child, sum) in [(b"one", 1), (b"two", 2)] {
                SumAndAverage::insert(&db, top, child, SumAndAverage::leaf(sum), None, v)
                    .unwrap()
                    .expect("a child");
            }
            let primary = build_prefix(top.into()).value;
            let secondary = |axis: IndexAxis| secondary_prefix_for(&primary, axis.tag()).value;
            let hosted = [
                primary,
                secondary(IndexAxis::Sum),
                secondary(IndexAxis::Avg),
            ];
            for prefix in hosted {
                assert!(entry(&db, prefix).is_some(), "entered with the first child");
            }
            let left = match flat {
                true => {
                    db.drop_flat_subtree(ROOT, name, crate::BackwardsReferences::DontCheck, None, v)
                }
                false => {
                    let allowed = crate::operations::delete::DeleteOptions {
                        allow_deleting_non_empty_trees: true,
                        deleting_non_empty_trees_returns_error: false,
                        ..Default::default()
                    };
                    db.delete(ROOT, name, Some(allowed), None, v)
                }
            };
            left.unwrap().expect("the tree leaves");
            for prefix in hosted {
                assert_eq!(entry(&db, prefix), None, "after the element went");
            }
            // The key again, as an indexed tree with other axes.
            db.insert(ROOT, name, CountSum::empty(), None, None, v)
                .unwrap()
                .expect("an indexed tree of another kind");
            CountSum::insert(&db, top, b"one", CountSum::leaf(3), None, v)
                .unwrap()
                .expect("a child");
            let count = entry(&db, secondary(IndexAxis::Count)).expect("an entry");
            assert_eq!(count.host.expect("a host").position, 1);
        }
        let pending =
            grovedb_storage::flat::PendingPrefixDrops::pending_prefix_drop_records(db.engine());
        assert_eq!(
            pending.expect("the records"),
            [],
            "the drop drained its record"
        );
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// A handle that is opened beside the subtree of an element writes a
/// subtree that the element hosts. Under a tree of one Merk, and under a
/// key that holds no element, its write is refused with its batch, and
/// nothing is entered. With no batch, and at the root, which no element
/// hosts, the handle names no element.
#[test]
fn a_write_beside_the_subtree_of_an_element_that_hosts_no_other_is_refused() {
    let v = GroveVersion::latest();
    let (_dir, engine) = super::engine();
    let db = GroveDb::from_engine(engine);
    db.insert(ROOT, b"tree", Element::empty_tree(), None, None, v)
        .unwrap()
        .expect("a tree");
    let root_before = db.root_hash(None, v).unwrap().expect("a root");
    let engine = db.engine();
    let tree: &[&[u8]] = &[b"tree"];
    let absent: &[&[u8]] = &[b"absent"];
    for path in [tree, absent] {
        let own = build_prefix(path.into()).value;
        let beside = secondary_prefix_for(&own, IndexAxis::Count.tag()).value;
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let opened = engine.subtree_beside(path.into(), beside, Some(&batch), &tx);
        assert_eq!(opened.cost, OperationCost::default());
        let handle = opened.unwrap();
        let host = handle.subtree().host.clone().expect("the element");
        assert_eq!(
            (host.subtree, host.key.as_slice(), host.hosted.as_slice()),
            ([0; 32], path[0], [own].as_slice())
        );
        (handle.put(b"k", b"v", None).unwrap()).expect("staging");
        drop(handle);
        let refused = engine.apply_batch(batch, &tx).unwrap();
        let refused = refused.expect_err("a subtree its element does not host");
        assert!(
            matches!(&refused, StorageError::NotSupported(words) if words.contains("beside")),
            "{refused}"
        );
        for prefix in [own, beside] {
            let read = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
            assert_eq!(read.entry().expect("the directory answers"), None);
            assert_eq!(read.get(b"k").unwrap().expect("a read"), None);
        }
        (engine.commit_transaction(tx).unwrap()).expect("a commit of nothing");

        let tx = engine.start_read_transaction();
        let read_only = engine.subtree_beside(path.into(), beside, None, &tx);
        assert_eq!(read_only.unwrap().subtree(), &LigninSubtree::alone(beside));
    }
    assert_eq!(db.root_hash(None, v).unwrap().expect("a root"), root_before);

    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let at_root = engine.subtree_beside(ROOT.into(), [9; 32], Some(&batch), &tx);
    assert_eq!(at_root.unwrap().subtree(), &LigninSubtree::alone([9; 32]));
}

/// The RocksDB engine opens such a handle as the handle of its prefix,
/// at no cost: what is written through it is read by prefix.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn on_rocksdb_a_handle_beside_a_subtree_is_the_handle_of_its_prefix() {
    let dir = tempfile::tempdir().expect("a directory");
    let engine = crate::RocksDbEngine::open(dir.path()).expect("an engine");
    let tree: &[&[u8]] = &[b"tree"];
    let own = build_prefix(tree.into()).value;
    let beside = secondary_prefix_for(&own, IndexAxis::Sum.tag()).value;
    let tx = engine.start_transaction();
    let batch = <crate::RocksDbEngine as Engine>::Batch::default();
    let opened = engine.subtree_beside(tree.into(), beside, Some(&batch), &tx);
    assert_eq!(opened.cost, OperationCost::default());
    let handle = opened.unwrap();
    (handle.put(b"k", b"v", None).unwrap()).expect("staging");
    drop(handle);
    (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
    let by_prefix = engine.subtree_by_prefix(beside, None, &tx).unwrap();
    let read = by_prefix.get(b"k").unwrap().expect("a read");
    assert_eq!(read.as_deref(), Some(b"v".as_slice()));
    let primary = engine.subtree_by_prefix(own, None, &tx).unwrap();
    assert_eq!(primary.get(b"k").unwrap().expect("a read"), None);
}

/// A batch that writes a secondary and not its element: a flat record of
/// the secondary's subtree, and a node of its Merk. No part of the group
/// states the element, so what it hosts is read from the transaction, and
/// the secondary's Merk is written with its host left as it is.
#[test]
fn a_secondary_that_is_written_without_its_element_is_one_of_those_the_element_hosts() {
    use grovedb_merk::{
        element::costs::ElementCostExtensions, store::NodeStore, Merk, Op, TreeFeatureType,
    };

    use crate::operations::indexed_tree::indexed_element_axes;

    let v = GroveVersion::latest();
    let (_dir, engine) = super::engine();
    let db = GroveDb::from_engine(engine);
    let top: &[&[u8]] = &[b"idx"];
    db.insert(ROOT, b"idx", Sum::empty(), None, None, v)
        .unwrap()
        .expect("an indexed tree");
    Sum::insert(&db, top, b"one", Sum::leaf(1), None, v)
        .unwrap()
        .expect("a child");
    let element = db.get(ROOT, b"idx", None, v).unwrap().expect("the tree");
    let axes = indexed_element_axes(&element).expect("an indexed tree");
    let (_, root_key) = axes.into_iter().next().expect("an axis");

    let engine = db.engine();
    let own = build_prefix(top.into()).value;
    let beside = secondary_prefix_for(&own, IndexAxis::Sum.tag()).value;
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let handle = engine
        .subtree_beside(top.into(), beside, Some(&batch), &tx)
        .unwrap();
    (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    let mut merk = Merk::open_layered_with_root_key(
        handle,
        root_key,
        TreeType::ProvableCountProvableSumTree,
        Some(&Element::value_defined_cost_for_serialized_value),
        v,
    )
    .unwrap()
    .expect("the secondary");
    let row = Element::new_item(b"row".to_vec())
        .serialize(v)
        .expect("the bytes of an element");
    let put = Op::Put(row, TreeFeatureType::ProvableCountedSummedMerkNode(1, 0));
    (merk.apply::<_, Vec<u8>>(&[(b"a row".to_vec(), put)], &[], None, v))
        .unwrap()
        .expect("a write of the secondary");
    drop(merk);
    (engine.apply_batch(batch, &tx).unwrap()).expect("a batch that writes the secondary alone");

    let written = engine.subtree_by_prefix(beside, None, &tx).unwrap();
    assert!(written.has_node(b"a row").unwrap().expect("a read"));
    let record = written.aux().get(b"k").unwrap().expect("a read");
    assert_eq!(record.as_deref(), Some(b"v".as_slice()));
    let entry = written.entry().expect("the directory answers");
    let host = entry.expect("entered").host.expect("a host");
    assert_eq!((host.key.as_slice(), host.position), (b"idx".as_slice(), 1));
    drop(written);
    engine.rollback_transaction(&tx).expect("a rollback");
}

/// A subtree leaves the directory with its element, so an indexed tree
/// that is made under a key whose earlier tree was deleted finds nothing
/// entered there and is written, whatever the earlier tree was. What
/// still leaves subtrees where an element does not name them is a
/// conversion, which writes the element with other axes and moves no
/// subtree (MERK-09): it is refused and changes nothing.
#[test]
fn an_indexed_tree_over_subtrees_that_an_earlier_element_left_elsewhere_is_refused() {
    let v = GroveVersion::latest();
    let (_dir, engine) = super::engine();
    let db = GroveDb::from_engine(engine);
    let with_axes = |axes: &[IndexAxis]| {
        let axes = axes.iter().map(|axis| (axis.tag(), None)).collect();
        Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes")
    };
    // Makes `first` under `key`, writes a child into it and takes the
    // child out again, deletes the tree, makes `second` in its place and
    // writes a child into that.
    let replaced = |key: &[u8], first: Element, second: Element| {
        let at: &[&[u8]] = &[key];
        let child = || CountSum::leaf(3);
        db.insert(ROOT, key, first.clone(), None, None, v)
            .unwrap()
            .expect("the first tree");
        match first.is_indexed_tree() {
            true => (CountSum::insert(&db, at, b"child", child(), None, v).unwrap())
                .expect("a child of the first tree"),
            false => (db.insert(at, b"child", child(), None, None, v).unwrap())
                .expect("a child of the first tree"),
        }
        match first.is_indexed_tree() {
            true => {
                let deleted = CountSum::delete(&db, at, b"child", None, v).unwrap();
                assert!(deleted.expect("a delete of the child"));
            }
            false => (db.delete(at, b"child", None, None, v).unwrap()).expect("a delete"),
        }
        (db.delete(ROOT, key, None, None, v).unwrap()).expect("a delete of the emptied tree");
        db.insert(ROOT, key, second, None, None, v)
            .unwrap()
            .expect("the second tree");
        CountSum::insert(&db, at, b"child", child(), None, v).unwrap()
    };
    let all = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
    let two = [IndexAxis::Sum, IndexAxis::Avg];
    let one = [IndexAxis::Count];
    let other = [IndexAxis::Sum];
    let plain = Element::empty_count_sum_tree;

    // After a delete nothing is entered under the key, whatever the
    // tree was that held it.
    let written = [
        replaced(b"same", with_axes(&all), with_axes(&all)),
        replaced(b"leading", with_axes(&all), with_axes(&[all[0], all[1]])),
        replaced(b"fewer", with_axes(&all), with_axes(&two)),
        replaced(b"more", with_axes(&two), with_axes(&all)),
        replaced(b"other", with_axes(&one), with_axes(&other)),
        replaced(b"plain", plain(), with_axes(&two)),
    ];
    for written in written {
        written.expect("nothing is entered under the key of a deleted tree");
    }

    // The conversion of an indexed tree to its 128-bit counterpart puts
    // its axes in another order, and its secondaries stay where they are
    // entered.
    let before = db.root_hash(None, v).unwrap().expect("a root");
    let converted = db.convert_to_big_sum_tree(ROOT, b"same", None, v).unwrap();
    let refused = converted.expect_err("subtrees that hang elsewhere");
    assert!(
        matches!(&refused, Error::NotSupported(words) if words.contains("other places")),
        "{refused}"
    );
    let after = db.root_hash(None, v).unwrap().expect("a root");
    assert_eq!(after, before, "a refused write changed the root hash");

    // A record of the primary alone, by a batch that states no element:
    // the Merks of the tree hang where its element names them, and the
    // record is written.
    let engine = db.engine();
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let same: &[&[u8]] = &[b"same"];
    let primary = engine.subtree(same.into(), Some(&batch), &tx).unwrap();
    (primary.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    drop(primary);
    (engine.apply_batch(batch, &tx).unwrap()).expect("a record of an entered subtree");
}

/// A subtree hangs on one element. Handles that name two elements for one
/// subtree are refused with their group, whether they staged into one
/// batch or into two.
#[test]
fn handles_that_name_two_elements_for_one_subtree_are_refused() {
    let v = GroveVersion::latest();
    let (_dir, engine) = super::engine();
    let db = GroveDb::from_engine(engine);
    let (idx, tree): (&[&[u8]], &[&[u8]]) = (&[b"idx"], &[b"tree"]);
    db.insert(ROOT, b"idx", Sum::empty(), None, None, v)
        .unwrap()
        .expect("an indexed tree");
    Sum::insert(&db, idx, b"one", Sum::leaf(1), None, v)
        .unwrap()
        .expect("a child");
    db.insert(ROOT, b"tree", Element::empty_tree(), None, None, v)
        .unwrap()
        .expect("a tree");
    let engine = db.engine();
    let own = build_prefix(idx.into()).value;
    let secondary = secondary_prefix_for(&own, IndexAxis::Sum.tag()).value;
    let tx = engine.start_transaction();
    // A record of the secondary, staged through a handle beside the tree
    // at `path`.
    let stage = |batch: &<LigninEngine as Engine>::Batch, path: &[&[u8]], key: &[u8]| {
        let handle = engine.subtree_beside(path.into(), secondary, Some(batch), &tx);
        (handle.unwrap().aux().put(key, b"v", None).unwrap()).expect("staging");
    };
    let two_elements = |refused: StorageError| {
        assert!(
            matches!(&refused, StorageError::NotSupported(words) if words.contains("two elements")),
            "{refused}"
        );
    };

    let batch = <LigninEngine as Engine>::Batch::default();
    stage(&batch, idx, b"x");
    stage(&batch, tree, b"y");
    two_elements((engine.apply_batch(batch, &tx).unwrap()).expect_err("one batch"));

    for wrong_first in [false, true] {
        let mut group = <LigninEngine as Engine>::WriteGroup::default();
        for part in [wrong_first, !wrong_first] {
            let batch = <LigninEngine as Engine>::Batch::default();
            match part {
                true => stage(&batch, tree, b"y"),
                false => stage(&batch, idx, b"x"),
            }
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        }
        let applied = engine.apply_write_group(group, &tx).unwrap();
        two_elements(applied.map(|_| ()).expect_err("two batches"));
    }
    let read = engine.subtree_by_prefix(secondary, None, &tx).unwrap();
    for key in [b"x", b"y"] {
        assert_eq!(read.aux().get(key).unwrap().expect("a read"), None);
    }

    // A handle beside the tree with the prefix of the secondary, in a
    // batch that writes the primary through its own handle: whichever of
    // the two subtrees the engine comes to first, the secondary is no
    // subtree of the tree.
    let batch = <LigninEngine as Engine>::Batch::default();
    let primary = engine.subtree(idx.into(), Some(&batch), &tx).unwrap();
    (primary.aux().put(b"p", b"v", None).unwrap()).expect("staging");
    drop(primary);
    stage(&batch, tree, b"y");
    let refused = (engine.apply_batch(batch, &tx).unwrap()).expect_err("the wrong element");
    assert!(
        matches!(&refused, StorageError::NotSupported(words) if words.contains("beside")),
        "{refused}"
    );
    let read = engine.subtree_by_prefix(own, None, &tx).unwrap();
    assert_eq!(read.aux().get(b"p").unwrap().expect("a read"), None);

    // Handles that name the element of the indexed tree, and disagree
    // about which subtree is the one of its own path: in one batch, and
    // in two batches of a group in either order.
    let misnamed = |batch: &<LigninEngine as Engine>::Batch, prefix, own| {
        let subtree = LigninSubtree {
            prefix,
            host: Some(LigninHost {
                subtree: [0; 32],
                key: b"idx".to_vec(),
                hosted: vec![own],
            }),
        };
        let view = tx.write_transaction().expect("a write transaction");
        let handle = LigninNodeStore::deferred(view, subtree, Some(batch));
        (handle.aux().put(b"z", b"v", None).unwrap()).expect("staging");
    };
    let wrong = |batch: &<LigninEngine as Engine>::Batch| misnamed(batch, secondary, secondary);
    let batch = <LigninEngine as Engine>::Batch::default();
    stage(&batch, idx, b"x");
    wrong(&batch);
    two_elements((engine.apply_batch(batch, &tx).unwrap()).expect_err("one batch"));
    for wrong_first in [false, true] {
        let mut group = <LigninEngine as Engine>::WriteGroup::default();
        for part in [wrong_first, !wrong_first] {
            let batch = <LigninEngine as Engine>::Batch::default();
            match part {
                true => wrong(&batch),
                false => stage(&batch, idx, b"x"),
            }
            (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
        }
        let applied = engine.apply_write_group(group, &tx).unwrap();
        two_elements(applied.map(|_| ()).expect_err("two batches"));
    }
    let read = engine.subtree_by_prefix(secondary, None, &tx).unwrap();
    for key in [b"x", b"z"] {
        assert_eq!(read.aux().get(key).unwrap().expect("a read"), None);
    }
    drop(read);
    // So are handles of two subtrees: one beside the tree, and one of
    // another prefix that names the tree's element with another subtree
    // of its own path.
    let (elsewhere, beside_it) = ([5; 32], [6; 32]);
    let batch = <LigninEngine as Engine>::Batch::default();
    stage(&batch, idx, b"x");
    misnamed(&batch, beside_it, elsewhere);
    two_elements((engine.apply_batch(batch, &tx).unwrap()).expect_err("two subtrees"));
    let read = engine.subtree_by_prefix(beside_it, None, &tx).unwrap();
    assert_eq!(read.entry().expect("the directory answers"), None);
    drop(read);

    // The right handle alone writes.
    let batch = <LigninEngine as Engine>::Batch::default();
    stage(&batch, idx, b"x");
    (engine.apply_batch(batch, &tx).unwrap()).expect("a record of the secondary");
}

/// A write into the primary through its path handle and a write into a
/// secondary through a handle by prefix, in one batch, as the first
/// writes into the tree: the secondary is owed an enter by both, and is
/// entered once, as the element names it.
#[test]
fn a_secondary_that_a_handle_by_prefix_writes_beside_a_named_write_is_entered_once() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        db.insert(ROOT, b"idx", Sum::empty(), None, None, v)
            .unwrap()
            .expect("an indexed tree");
        let engine = db.engine();
        let top: &[&[u8]] = &[b"idx"];
        let primary = build_prefix(top.into()).value;
        let secondary = secondary_prefix_for(&primary, IndexAxis::Sum.tag()).value;
        let tx = engine.start_transaction();
        let batch = <LigninEngine as Engine>::Batch::default();
        let by_path = engine.subtree(top.into(), Some(&batch), &tx).unwrap();
        (by_path.aux().put(b"p", b"v", None).unwrap()).expect("staging");
        let by_prefix = engine
            .subtree_by_prefix(secondary, Some(&batch), &tx)
            .unwrap();
        assert_eq!(by_prefix.subtree(), &LigninSubtree::alone(secondary));
        (by_prefix.aux().put(b"s", b"v", None).unwrap()).expect("staging");
        drop((by_path, by_prefix));
        (engine.apply_batch(batch, &tx).unwrap()).expect("both records");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");

        for (position, (prefix, key)) in
            [(primary, b"p"), (secondary, b"s")].into_iter().enumerate()
        {
            let entry = entry(&db, prefix).expect("an entry");
            let host = entry.host.expect("a host");
            assert_eq!(
                (host.key.as_slice(), usize::from(host.position)),
                (b"idx".as_slice(), position)
            );
            let tx = engine.start_read_transaction();
            let read = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
            let record = read.aux().get(key).unwrap().expect("a read");
            assert_eq!(record.as_deref(), Some(b"v".as_slice()));
        }
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// What another writer does inside the look of
/// [`a_writer_inside_the_look_of_an_indexed_tree_is_found_or_refuses_the_group`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum Inside {
    /// The first write into the tree, through a handle that names its
    /// element: both Merks are entered under the element.
    Named,
    /// The first write into the tree, into the Merk that the group
    /// writes by prefix, by its prefix alone: that Merk is entered under
    /// no element, and the other is not entered.
    ByPrefix,
    /// The tree's Merks were entered under its element before the look,
    /// after the group's handles were opened. A wipe, the tree inserted
    /// again, and a write into each Merk by its prefix alone: both are
    /// entered under no element.
    Replaced,
}

/// A group writes the two Merks of an indexed tree, one through a handle
/// that names the element and the other through a handle by prefix. Its
/// look reads the entry of the second twice, once for each handle, and
/// another writer's commits are published between the two reads, by a
/// hook inside the look, in both orders of the two handles.
///
/// The look goes by its last read of an entry. Where the other writer
/// entered both Merks under the element, the group is applied once and
/// both writers' records are there. Where the last read finds a Merk
/// under no element, whether nothing was entered before or both Merks
/// hung under the element at the first read, the look refuses the group:
/// nothing is applied, and the transaction, which stays open, holds none
/// of the group's records and no entry of its making.
#[test]
fn a_writer_inside_the_look_of_an_indexed_tree_is_found_or_refuses_the_group() {
    use std::{cell::Cell, rc::Rc, sync::Arc};

    use crate::engine::lignin::{AFTER_ENTRY_READS, REFUSED};

    let v = GroveVersion::latest();
    let cases = [Inside::Named, Inside::ByPrefix, Inside::Replaced];
    for (named_first, inside) in cases
        .into_iter()
        .flat_map(|inside| [(true, inside), (false, inside)])
    {
        let case = format!("named first: {named_first}, {inside:?}");
        let dir = tempfile::tempdir().expect("a directory");
        {
            let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
            let tree = move |db: &Lignin| {
                db.insert(ROOT, b"idx", Sum::empty(), None, None, v)
                    .unwrap()
                    .expect("an indexed tree");
            };
            tree(&db);
            let engine = db.engine();
            let top: &[&[u8]] = &[b"idx"];
            let primary = build_prefix(top.into()).value;
            let secondary = secondary_prefix_for(&primary, IndexAxis::Sum.tag()).value;
            // One committed record in each of `into`: through a handle
            // that names the element where there is one subtree and
            // `names` is set, and by prefix otherwise.
            let put = |db: &Lignin, into: &[[u8; 32]], names: bool, key: &[u8]| {
                let engine = db.engine();
                let tx = engine.start_transaction();
                let batch = <LigninEngine as Engine>::Batch::default();
                let top: &[&[u8]] = &[b"idx"];
                for prefix in into {
                    let handle = match names {
                        true => engine.subtree(top.into(), Some(&batch), &tx),
                        false => engine.subtree_by_prefix(*prefix, Some(&batch), &tx),
                    };
                    let handle = handle.unwrap();
                    (handle.aux().put(key, b"v", None).unwrap()).expect("staging");
                }
                (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
                (engine.commit_transaction(tx).unwrap()).expect("a commit");
            };
            // The subtrees are looked at in the order of their prefixes:
            // the handle that names the element is of the one that comes
            // first, or of the other.
            let (low, high) = (primary.min(secondary), primary.max(secondary));
            let (named, by_prefix) = match named_first {
                true => (low, high),
                false => (high, low),
            };
            let tx = engine.start_transaction();
            let batch = <LigninEngine as Engine>::Batch::default();
            let handle = match named == primary {
                true => engine.subtree(top.into(), Some(&batch), &tx),
                false => engine.subtree_beside(top.into(), secondary, Some(&batch), &tx),
            };
            let handle = handle.unwrap();
            (handle.aux().put(b"n", b"v", None).unwrap()).expect("staging");
            drop(handle);
            let handle = engine
                .subtree_by_prefix(by_prefix, Some(&batch), &tx)
                .unwrap();
            (handle.aux().put(b"p", b"v", None).unwrap()).expect("staging");
            drop(handle);
            // The group's handles were opened while nothing was entered:
            // the one by prefix names no element.
            if inside == Inside::Replaced {
                put(&db, &[primary], true, b"z");
            }

            // The named write reads the entry of each Merk of the tree,
            // after one read of its own subtree's where that is the one
            // of the element's path (an entry of one Merk would stand
            // for the element), and the write by prefix reads the entry
            // of its own: the other writer comes after the first read of
            // that entry.
            let reads = match named_first {
                true => 2 + usize::from(named == primary),
                false => 1,
            };
            let published = Rc::new(Cell::new(false));
            let other = {
                let (db, published) = (db.clone(), published.clone());
                Box::new(move || {
                    match inside {
                        Inside::Named => put(&db, &[primary], true, b"o"),
                        Inside::ByPrefix => put(&db, &[by_prefix], false, b"o"),
                        Inside::Replaced => {
                            db.wipe().expect("a wipe");
                            tree(&db);
                            put(&db, &[primary, secondary], false, b"o");
                        }
                    }
                    published.set(true);
                })
            };
            REFUSED.take();
            AFTER_ENTRY_READS.set(Some((reads, other)));
            let applied = engine.apply_batch(batch, &tx).unwrap();
            assert!(
                published.get(),
                "{case}: the other writer came inside the look"
            );
            assert!(AFTER_ENTRY_READS.take().is_none(), "{case}");
            // No application was refused by the transaction: a group is
            // taken as its look left it, or refused by its look.
            assert_eq!(REFUSED.take().len(), 0, "{case}");
            // Whether a record is there, and the entry of a subtree, as a
            // transaction shows them.
            let read = |tx: &<LigninEngine as Engine>::Transaction<'_>, prefix, key: &[u8]| {
                let handle = engine.subtree_by_prefix(prefix, None, tx).unwrap();
                handle.aux().get(key).unwrap().expect("a read").is_some()
            };
            let entry_in = |tx: &<LigninEngine as Engine>::Transaction<'_>, prefix| {
                let handle = engine.subtree_by_prefix(prefix, None, tx).unwrap();
                handle.entry().expect("the directory answers")
            };
            match inside {
                Inside::Named => {
                    applied.expect("both records");
                    (engine.commit_transaction(tx).unwrap()).expect("a commit");
                    let reader = engine.start_read_transaction();
                    let written = [(named, b"n"), (by_prefix, b"p"), (primary, b"o")];
                    for (prefix, key) in written {
                        assert!(read(&reader, prefix, key), "{case}");
                    }
                    for (position, prefix) in [primary, secondary].into_iter().enumerate() {
                        let host = entry_in(&reader, prefix).expect("an entry").host;
                        let host = host.expect("a host");
                        assert_eq!(
                            (host.key.as_slice(), usize::from(host.position)),
                            (b"idx".as_slice(), position),
                            "{case}"
                        );
                    }
                }
                Inside::ByPrefix | Inside::Replaced => {
                    let refused = applied.map(|_| ()).expect_err("a Merk under no element");
                    assert!(
                        matches!(refused, StorageError::NotSupported(_)),
                        "{case}: {refused}"
                    );
                    // The refused transaction, which stays open, and a
                    // reader beside it.
                    let reader = engine.start_read_transaction();
                    for (through, which) in [(&tx, "the transaction"), (&reader, "a reader")] {
                        let case = format!("{case}, {which}");
                        assert!(read(through, by_prefix, b"o"), "{case}");
                        assert!(!read(through, by_prefix, b"p"), "{case}");
                        assert!(!read(through, named, b"n"), "{case}");
                        assert!(!read(through, primary, b"z"), "{case}");
                        let alone = entry_in(through, by_prefix).expect("an entry");
                        assert_eq!(alone.host, None, "{case}");
                        let beside = entry_in(through, named);
                        match inside {
                            Inside::ByPrefix => assert_eq!(beside, None, "{case}"),
                            _ => assert_eq!(beside.expect("an entry").host, None, "{case}"),
                        }
                    }
                    drop(tx);
                }
            }
            db.flush().expect("a flush");
        }
        check(dir.path(), v);
    }
}

/// The delete of an indexed tree whose look is stale: another writer's
/// delete of the tree is published after the look. The removals that the
/// delete staged for the secondaries, through handles by prefix, are
/// then of subtrees that are not entered: they are not handed over, and
/// nothing is entered for them.
#[test]
fn a_delete_of_an_indexed_tree_that_another_commit_took_enters_nothing() {
    use std::sync::Arc;

    use super::super::AFTER_A_LOOK;

    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database"));
        let top: &[&[u8]] = &[b"top"];
        db.insert(ROOT, b"top", SumAndAverage::empty(), None, None, v)
            .unwrap()
            .expect("an indexed tree");
        for (child, sum) in [(b"one", 1), (b"two", 2)] {
            SumAndAverage::insert(&*db, top, child, SumAndAverage::leaf(sum), None, v)
                .unwrap()
                .expect("a child");
        }
        let primary = build_prefix(top.into()).value;
        let secondary = |axis: IndexAxis| secondary_prefix_for(&primary, axis.tag()).value;
        let hosted = [
            primary,
            secondary(IndexAxis::Sum),
            secondary(IndexAxis::Avg),
        ];
        for prefix in hosted {
            assert!(entry(&db, prefix).is_some(), "entered with the first child");
        }
        let allowed = || crate::operations::delete::DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        };
        let other = {
            let db = db.clone();
            Box::new(move || {
                (db.delete(ROOT, b"top", Some(allowed()), None, v).unwrap())
                    .expect("the other delete");
            })
        };
        let tx = db.start_transaction();
        AFTER_A_LOOK.set([super::apart(other)].into_iter().collect());
        let deleted = db.delete(ROOT, b"top", Some(allowed()), Some(&tx), v);
        assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
        deleted
            .unwrap()
            .expect("the delete is applied with no drop");
        let engine = db.engine();
        for prefix in hosted {
            let handle = engine.subtree_by_prefix(prefix, None, &tx).unwrap();
            let entered = handle.entry().expect("the directory answers");
            assert_eq!(entered, None, "in the transaction of the second delete");
        }
        // Whether the second delete commits is the conflict rule's to
        // say; the directory is the same either way.
        let _ = db.commit_transaction(tx).unwrap();
        for prefix in hosted {
            assert_eq!(entry(&db, prefix), None);
        }
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// A child of an indexed tree that is a tree with an element and a tree
/// in it is written over with an empty tree of its kind, through the
/// entry point of the indexed tree. GroveDB removes the records of the
/// old child in one batch and writes the new element in the next, and
/// nothing of the old child is read under the new. Gives every call with
/// its cost and the root hash after it.
fn written_over_a_child<E: Engine>(dir: &std::path::Path) -> Vec<(String, OperationCost, String)> {
    let v = GroveVersion::latest();
    let db = GroveDb::<E>::open_with_engine(dir).expect("a database");
    let mut calls = Vec::new();
    let mut call = |name: &str, cost: OperationCost, answer: String| {
        let root = db.root_hash(None, v).unwrap().expect("a root");
        calls.push((
            name.to_owned(),
            cost,
            format!("{answer} {}", hex::encode(root)),
        ));
    };
    let top: &[&[u8]] = &[b"top"];
    let child: &[&[u8]] = &[b"top", b"a"];
    let inner: &[&[u8]] = &[b"top", b"a", b"in"];
    let made = db.insert(ROOT, b"top", CountSum::empty(), None, None, v);
    call("made", made.cost.clone(), format!("{:?}", made.value));
    let branch = CountSum::insert(&db, top, b"a", CountSum::branch(), None, v);
    call(
        "a child",
        branch.cost.clone(),
        format!("{:?}", branch.value),
    );
    let filled = db.insert(child, b"x", CountSum::in_branch(0), None, None, v);
    call(
        "an element",
        filled.cost.clone(),
        format!("{:?}", filled.value),
    );
    let tree = db.insert(child, b"in", Element::empty_tree(), None, None, v);
    call(
        "a tree in it",
        tree.cost.clone(),
        format!("{:?}", tree.value),
    );
    let deep = db.insert(inner, b"y", Element::new_item(vec![7; 5]), None, None, v);
    call(
        "an element of that",
        deep.cost.clone(),
        format!("{:?}", deep.value),
    );

    let over = CountSum::insert(&db, top, b"a", CountSum::branch(), None, v);
    call(
        "written over",
        over.cost.clone(),
        format!("{:?}", over.value),
    );
    let gone = db.get(child, b"x", None, v);
    call(
        "the old element",
        gone.cost.clone(),
        format!("{}", gone.value.is_ok()),
    );
    let gone = db.has_raw(inner, b"y", None, v);
    call(
        "the old tree's element",
        gone.cost.clone(),
        format!("{:?}", gone.value),
    );
    let empty = db.is_empty_tree(child, None, v);
    call("empty", empty.cost.clone(), format!("{:?}", empty.value));
    let again = db.insert(child, b"z", CountSum::in_branch(1), None, None, v);
    call(
        "an element of the new child",
        again.cost.clone(),
        format!("{:?}", again.value),
    );
    let issues = db.verify_grovedb(None, true, false, v);
    call(
        "verified",
        OperationCost::default(),
        format!("{:?}", issues.map(|issues| issues.len())),
    );
    db.flush().expect("a flush");
    calls
}

/// A tree written over a tree whose records GroveDB removed in the batch
/// before, which left the Merk of the old child with no root and did not
/// write its element: the element's node names the Merk again, the
/// removals are charged as on RocksDB, the subtree of the old child
/// holds nothing afterwards, and the tree that was in it has left the
/// directory. Every answer, cost and root hash is RocksDB's.
#[test]
fn a_child_of_an_indexed_tree_that_is_written_over_is_swept_as_on_rocksdb() {
    let dir = tempfile::tempdir().expect("a directory");
    let on_lignin = written_over_a_child::<LigninEngine>(dir.path());
    let said = |call: &str| {
        let found = on_lignin.iter().find(|(name, ..)| name == call);
        found.map(|(.., said)| said.clone()).expect("the call")
    };
    assert!(
        said("written over").starts_with("Ok(())"),
        "{}",
        said("written over")
    );
    assert!(said("the old element").starts_with("false"));
    assert!(said("the old tree's element").starts_with("Ok(false)"));
    assert!(said("empty").starts_with("Ok(true)"));
    assert!(said("verified").starts_with("Ok(0)"));
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        let inner = build_prefix([b"top".as_slice(), b"a", b"in"].as_slice().into()).value;
        assert_eq!(entry(&db, inner), None, "the tree in the old child");
        db.flush().expect("a flush");
    }
    check(dir.path(), GroveVersion::latest());
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let on_rocksdb = written_over_a_child::<crate::RocksDbEngine>(dir.path());
        assert_eq!(on_lignin, on_rocksdb);
    }
}
