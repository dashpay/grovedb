//! The indexed trees on the LigninDB engine: an element that hosts its
//! primary Merk and one secondary Merk for each of its axes.
//!
//! Each of the five kinds has a history, written once for any engine and
//! run on LigninDB and on RocksDB on every grove version, with every
//! answer, every `OperationCost` and the root hash after every write
//! compared (`append_only`). The tests after them are of what the engine
//! itself does with such an element: which subtrees it enters, where each
//! hangs, and what it refuses.

use grovedb_costs::CostResult;
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    proofs::query::AggregateFold,
    store::lignin::{LigninEntry, LigninNodeStore, LigninSubtree},
    TreeType,
};
use grovedb_storage::prefix::{build_prefix, secondary_prefix_for};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, on_both_engines, Answer, History},
    Lignin,
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
    fn leaf(n: usize) -> Element;

    /// An empty tree that the kind takes as a child.
    fn branch() -> Element;

    /// The `n`th element put into such a child.
    fn in_branch(n: usize) -> Element;
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

    fn leaf(n: usize) -> Element {
        Element::new_sum_item(sum(n))
    }

    fn branch() -> Element {
        Element::empty_sum_tree()
    }

    fn in_branch(n: usize) -> Element {
        Element::new_sum_item(sum(n + 50))
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

    fn in_branch(n: usize) -> Element {
        Element::new_sum_item(sum(n + 50))
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

    fn in_branch(n: usize) -> Element {
        CountSum::in_branch(n)
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

    fn leaf(n: usize) -> Element {
        Element::new_sum_item(sum(n))
    }

    fn branch() -> Element {
        Element::empty_big_sum_tree()
    }

    fn in_branch(n: usize) -> Element {
        Element::new_sum_item(sum(n + 50))
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

    fn leaf(n: usize) -> Element {
        Element::new_sum_item(sum(n))
    }

    fn branch() -> Element {
        Element::empty_count_big_sum_tree()
    }

    fn in_branch(n: usize) -> Element {
        Element::new_sum_item(sum(n + 50))
    }
}

fn owned(path: &[&[u8]]) -> Vec<Vec<u8>> {
    path.iter().map(|segment| segment.to_vec()).collect()
}

/// Every read of the axes of the indexed tree at `path`, with no
/// transaction: the ranking from both ends with and without an offset,
/// bands of values in ascending order, the rank of a key that is there
/// and of one that is not, and the two folds over a band.
fn axis_reads<T: Indexed, E: Engine>(h: &mut History<'_, E>, path: &[&[u8]]) {
    let (db, v) = (h.db, h.v);
    let mut run = |name: &str, query: PathQuery| {
        let pairs = QueryResultType::QueryKeyElementPairResultType;
        let answer = db.run_path_query(&query, true, true, true, pairs, None, v);
        h.read(name, answer);
    };
    for axis in T::axes() {
        let at = || owned(path);
        run("top", PathQuery::new_axis_top_k(at(), axis, 5, 0, true));
        run("top", PathQuery::new_axis_top_k(at(), axis, 3, 2, false));
        run("top", PathQuery::new_axis_top_k(at(), axis, 4, 40, true));
        run(
            "band",
            PathQuery::new_axis_bounded(at(), axis, -30, 30, 6, false),
        );
        run(
            "band",
            PathQuery::new_axis_bounded(at(), axis, 0, i128::from(i64::MAX), 3, false),
        );
        let rank = |key: &[u8]| PathQuery::new_axis_rank_of_key(at(), axis, key.to_vec(), true);
        run("rank", rank(b"leaf02"));
        run("rank", rank(b"missing"));
        for fold in [AggregateFold::Population, AggregateFold::Total] {
            let query = PathQuery::new_axis_aggregate_over_value_range(at(), axis, -20, 60, fold);
            run("fold", query);
        }
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
    h.read("element", db.get(ROOT, b"idx", None, v));
    h.read("empty", db.is_empty_tree(top, None, v));
    axis_reads::<T, E>(&mut h, top);

    // Children that are no trees, one by one.
    for n in 0..9 {
        let inserted = T::insert(db, top, &leaf_key(n), T::leaf(n), None, v);
        h.write("insert", inserted);
        if n % 4 == 0 {
            h.read("element", db.get(ROOT, b"idx", None, v));
            h.read("child", db.get(top, &leaf_key(n), None, v));
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
    h.read("element", db.get(ROOT, b"idx", None, v));
    h.read("child", db.get(top, b"b1", None, v));
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
    h.read("element", db.get(ROOT, b"idx", Some(&tx), v));
    h.read("element", db.get(ROOT, b"idx", None, v));
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
    h.read("child", db.get(top, b"never", None, v));

    // A tree that one batch makes and fills.
    let fresh: &[&[u8]] = &[b"fresh"];
    let batch = vec![
        put(ROOT, b"fresh", T::empty()),
        put(fresh, b"one", T::leaf(50)),
        put(fresh, b"two", T::leaf(51)),
    ];
    h.write("fresh batch", db.apply_batch(batch, None, None, v));
    h.read("element", db.get(ROOT, b"fresh", None, v));
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
    h.read("element", db.get(ROOT, b"unused", None, v));
    h.answers
}

/// What a history must have done on the grove version `number`: on a
/// version that has the kind, every tree is made, every insert through
/// the kind's entry point and every write below the primary is taken, and
/// each batch is applied.
fn told(name: &'static str, has_kind: impl Fn(usize) -> bool) -> impl Fn(&[Answer], usize) {
    move |answers, number| {
        let count = |call: &str| {
            let ok = |(named, _, answer): &&Answer| named == call && answer.starts_with("Ok(");
            answers.iter().filter(ok).count()
        };
        let seen = [
            count("made"),
            count("insert"),
            count("below"),
            count("batch"),
            count("fresh batch"),
            count("repair"),
        ];
        match has_kind(number) {
            true => assert_eq!(seen, [3, 19, 8, 3, 1, 1], "{name} on version {number}"),
            false => assert_eq!(seen[..2], [0, 0], "{name} on version {number}"),
        }
    }
}

#[test]
fn a_count_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Count, _>, told(Count::NAME, |_| true));
}

#[test]
fn a_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<Sum, _>, told(Sum::NAME, |_| true));
}

#[test]
fn a_count_and_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(history::<CountSum, _>, told(CountSum::NAME, |_| true));
}

#[test]
fn an_indexed_tree_with_two_of_three_axes_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(
        history::<SumAndAverage, _>,
        told(SumAndAverage::NAME, |_| true)
    );
}

#[test]
fn a_big_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(
        history::<BigSum, _>,
        told(BigSum::NAME, |number| number >= 5)
    );
}

#[test]
fn a_count_and_big_sum_indexed_tree_answers_on_lignindb_as_on_rocksdb() {
    on_both_engines!(
        history::<CountBigSum, _>,
        told(CountBigSum::NAME, |number| number >= 5)
    );
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
