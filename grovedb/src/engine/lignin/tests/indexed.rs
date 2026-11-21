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
    store::lignin::{LigninEntry, LigninNodeStore, LigninSubtree},
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
    batch::QualifiedGroveDbOp, engine::Engine, query_result_type::QueryResultType, Element, Error,
    GroveDb, PathQuery, TransactionArg,
};

const ROOT: &[&[u8]] = &[];

/// One kind of indexed tree, as its history uses it.
trait Indexed {
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

struct Count;

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

struct Sum;

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

struct CountSum;

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
struct SumAndAverage;

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

struct BigSum;

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

struct CountBigSum;

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
    let (db, v) = (h.db, h.v);
    let mut run = |name: &str, axis: IndexAxis, query: PathQuery| {
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        let answer = db.run_path_query(&query, true, true, true, pairs, None, v);
        h.read(format!("{name} {axis:?}"), answer);
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
        let rank = |key: &[u8]| PathQuery::new_axis_rank_of_key(at(), axis, key.to_vec(), true);
        run("rank", axis, rank(b"leaf02"));
        run("rank", axis, rank(b"missing"));
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
        "children",
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
            let rows = |call: &str| {
                let with = |(named, _, answer): &&Answer| {
                    named == call && answer.starts_with("Ok(") && answer.contains("primary_key")
                };
                answers.iter().filter(with).count()
            };
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
    axis_reads::<T, E>(&mut h, top);
    axis_reads::<T, E>(&mut h, inner);
    verified(&mut h);
    h.answers
}

/// On a grove version from `since` on, the inner tree is made and every
/// write into it is taken.
fn nested_told(since: usize) -> impl Fn(&[Answer], usize) {
    move |answers, number| {
        assert!(sound(answers), "version {number}");
        let count = |call: &str| taken(answers, call);
        let seen = [
            count("made"),
            count("nested"),
            count("insert"),
            count("delete"),
        ];
        if number >= since {
            assert_eq!(seen, [1, 1, 5, 1], "version {number}");
        }
    }
}

#[test]
fn a_sum_indexed_tree_in_a_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(nested::<Sum, _>, nested_told(1));
}

#[test]
fn a_count_and_sum_indexed_tree_in_another_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(nested::<CountSum, _>, nested_told(1));
}

fn entry(db: &Lignin, prefix: [u8; 32]) -> Option<LigninEntry> {
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

/// A subtree is not dropped when its element goes (GROVE-05). An indexed
/// tree that is made under a key whose earlier element left subtrees
/// entered finds them there. Where they hang as the new element names
/// them, it is written. Where they do not, the write is refused and
/// changes nothing: with other axes, and after a tree of one Merk.
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
        let before = db.root_hash(None, v).unwrap().expect("a root");
        let written = CountSum::insert(&db, at, b"child", child(), None, v).unwrap();
        let after = db.root_hash(None, v).unwrap().expect("a root");
        (written, before == after)
    };
    let all = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg];
    let two = [IndexAxis::Sum, IndexAxis::Avg];

    let (written, _) = replaced(b"same", with_axes(&all), with_axes(&all));
    written.expect("the subtrees hang where the element names them");
    // The axes it keeps are the first of those the earlier tree kept:
    // each hangs where the new element names it.
    let leading = [IndexAxis::Count, IndexAxis::Sum];
    let (written, _) = replaced(b"leading", with_axes(&all), with_axes(&leading));
    written.expect("the subtrees it hosts hang where the element names them");

    let refusals = [
        replaced(b"fewer", with_axes(&all), with_axes(&two)),
        replaced(b"more", with_axes(&two), with_axes(&all)),
        replaced(
            b"other",
            with_axes(&[IndexAxis::Count]),
            with_axes(&[IndexAxis::Sum]),
        ),
        replaced(b"plain", Element::empty_count_sum_tree(), with_axes(&two)),
    ];
    for (written, unchanged) in refusals {
        let refused = written.expect_err("subtrees that hang elsewhere");
        assert!(
            matches!(&refused, Error::NotSupported(words) if words.contains("other places")),
            "{refused}"
        );
        assert!(unchanged, "a refused write changed the root hash");
    }

    // A write into the primary alone, by a batch that states no element,
    // is held to the same: the entry of the primary is the earlier
    // tree's, and says nothing of the element that is there now.
    let engine = db.engine();
    let tx = engine.start_transaction();
    let batch = <LigninEngine as Engine>::Batch::default();
    let plain: &[&[u8]] = &[b"plain"];
    let primary = engine.subtree(plain.into(), Some(&batch), &tx).unwrap();
    (primary.aux().put(b"k", b"v", None).unwrap()).expect("staging");
    drop(primary);
    let refused = engine.apply_batch(batch, &tx).unwrap();
    let refused = refused.expect_err("a primary whose secondaries are not entered");
    assert!(
        matches!(&refused, StorageError::NotSupported(words) if words.contains("other places")),
        "{refused}"
    );
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

    // The right handle alone writes.
    let batch = <LigninEngine as Engine>::Batch::default();
    stage(&batch, idx, b"x");
    (engine.apply_batch(batch, &tx).unwrap()).expect("a record of the secondary");
}
