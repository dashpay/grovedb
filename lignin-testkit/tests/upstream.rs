//! Models of what the workloads return that reach what GroveDB gained after
//! the first corpus: the indexed trees with a 128-bit sum and their axis,
//! and a limit of a query node on a layer that has subqueries.
//!
//! Each test applies a stream on every grove version and compares what
//! every read returns with a model that is told the writes and nothing
//! else.

use std::{
    collections::{BTreeMap, BTreeSet},
    thread,
};

use grovedb::{
    operations::{
        proof::{indexed_axis::AxisEntries, VerifiedPathQuery},
        QueryItemOrSumReturnType,
    },
    query_result_type::PathKeyElementTrio,
    AxisAggregateValue, AxisKeys, AxisProjection, AxisQuery, AxisTraversal, Element, GroveDb,
    PathQuery, PathQueryRun, PathQueryShape,
};
use grovedb_merk::proofs::query::{AggregateFold, IndexAxis};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::workload::{
    adversarial::{CappedLayers, WideRanks},
    BatchOp, Coverage, Features, IndexedTree, Op, Operation, Outcome, Read, StreamDigest,
    Transactions, Write,
};

const SEEDS: [u64; 4] = [1, 0x4C49_474E_494E, 2, 3];

/// GroveDB recurses once per level of nesting when it queries and proves.
const STACK_BYTES: usize = 64 << 20;

fn open() -> (tempfile::TempDir, GroveDb) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = GroveDb::open(dir.path()).expect("open");
    (dir, db)
}

fn on_every_grove_version(body: impl Fn(&'static GroveVersion) + Sync) {
    let body = &body;
    thread::scope(|scope| {
        for grove_version in GROVE_VERSIONS {
            thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, move || body(grove_version))
                .expect("spawn");
        }
    });
}

type Path = Vec<Vec<u8>>;

fn below(path: &[Vec<u8>], key: &[u8]) -> Path {
    let mut path = path.to_vec();
    path.push(key.to_vec());
    path
}

/// The trees a stream wrote, as the elements it wrote into them.
#[derive(Default)]
struct Trees {
    held: BTreeMap<Path, BTreeMap<Vec<u8>, Element>>,
}

impl Trees {
    fn put(&mut self, path: &[Vec<u8>], key: &[u8], element: &Element) {
        self.held
            .entry(path.to_vec())
            .or_default()
            .insert(key.to_vec(), element.clone());
    }

    /// What the tree at `path` holds, in the order of its keys.
    fn entries(&self, path: &[Vec<u8>]) -> impl Iterator<Item = (&Vec<u8>, &Element)> {
        self.held.get(path).into_iter().flatten()
    }

    /// Removes `key` and what is under it. Returns whether it was held.
    fn remove(&mut self, path: &[Vec<u8>], key: &[u8]) -> bool {
        let held = self
            .held
            .get_mut(path)
            .and_then(|tree| tree.remove(key))
            .is_some();
        let under = below(path, key);
        self.held.retain(|tree, _| !tree.starts_with(&under));
        held
    }

    /// Follows a write. For a delete through the methods of an indexed tree
    /// it returns whether the key was held, which is what GroveDB answers.
    fn record(&mut self, op: &Op) -> Option<bool> {
        match op {
            Op::Write(
                Write::Insert { path, key, element }
                | Write::IndexedInsert {
                    path, key, element, ..
                },
            ) => self.put(path, key, element),
            Op::Write(Write::Delete { path, key }) => {
                assert!(self.remove(path, key), "{op:?} deletes what is not held");
            }
            Op::Write(Write::IndexedDelete { path, key, .. }) => {
                return Some(self.remove(path, key));
            }
            Op::Write(Write::ReconcileIndexed { .. }) | Op::Commit | Op::Read(_) => {}
            Op::Batch(ops) => {
                for op in ops {
                    match op {
                        BatchOp::InsertOnly { path, key, element }
                        | BatchOp::InsertOrReplace { path, key, element }
                        | BatchOp::Replace { path, key, element } => self.put(path, key, element),
                        BatchOp::Delete { path, key } => {
                            assert!(self.remove(path, key), "{op:?} deletes what is not held");
                        }
                        other => panic!("the model does not follow {other:?}"),
                    }
                }
            }
            other => panic!("the model does not follow {other:?}"),
        }
        None
    }

    /// The count and the sum the tree at `path` keeps of what it holds.
    fn total(&self, path: &[Vec<u8>]) -> (u64, i128) {
        self.entries(path)
            .fold((0, 0), |(count, sum), (key, element)| {
                let (counted, summed) = self.adds(path, key, element);
                (count + counted, sum + summed)
            })
    }

    /// What an element adds to the count and to the sum of the tree it is
    /// in: an amount counts once and adds itself, a tree that keeps a count
    /// adds its count and its sum, and any other tree counts once and adds
    /// the sum it keeps.
    fn adds(&self, path: &[Vec<u8>], key: &[u8], element: &Element) -> (u64, i128) {
        let inside = || self.total(&below(path, key));
        match element {
            Element::SumItem(amount, _) | Element::ItemWithSumItem(_, amount, _) => {
                (1, i128::from(*amount))
            }
            Element::Tree(..) | Element::Item(..) | Element::Reference(..) => (1, 0),
            Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::ProvableSumIndexedTree(..)
            | Element::ProvableBigSumIndexedTree(..) => (1, inside().1),
            Element::CountSumTree(..)
            | Element::CountBigSumTree(..)
            | Element::ProvableCountProvableSumIndexedTree(..)
            | Element::ProvableCountProvableBigSumIndexedTree(..) => inside(),
            other => panic!("the model does not follow {other:?}"),
        }
    }

    /// The element GroveDB must hold under `key`, without its root keys: a
    /// tree with the totals of what it holds, anything else as written.
    fn stored(&self, path: &[Vec<u8>], key: &[u8]) -> Option<Element> {
        let element = self.held.get(path)?.get(key)?.clone();
        let (count, sum) = self.total(&below(path, key));
        let narrow = || i64::try_from(sum).expect("a narrow total fits an i64");
        Some(match element {
            Element::SumTree(_, _, flags) => Element::SumTree(None, narrow(), flags),
            Element::BigSumTree(_, _, flags) => Element::BigSumTree(None, sum, flags),
            Element::CountSumTree(_, _, _, flags) => {
                Element::CountSumTree(None, count, narrow(), flags)
            }
            Element::CountBigSumTree(_, _, _, flags) => {
                Element::CountBigSumTree(None, count, sum, flags)
            }
            Element::ProvableSumIndexedTree(_, _, _, flags) => {
                Element::ProvableSumIndexedTree(None, None, narrow(), flags)
            }
            Element::ProvableBigSumIndexedTree(_, _, _, flags) => {
                Element::ProvableBigSumIndexedTree(None, None, sum, flags)
            }
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, flags) => {
                Element::ProvableCountProvableSumIndexedTree(None, count, narrow(), axes, flags)
            }
            Element::ProvableCountProvableBigSumIndexedTree(_, _, _, axes, flags) => {
                Element::ProvableCountProvableBigSumIndexedTree(None, count, sum, axes, flags)
            }
            other => other,
        })
    }

    /// `ranked`, keys of the tree at `path`, each with the element GroveDB
    /// must hold under it.
    fn with_elements(
        &self,
        path: &[Vec<u8>],
        ranked: Vec<(i128, Vec<u8>)>,
    ) -> Vec<(i128, Vec<u8>, Element)> {
        ranked
            .into_iter()
            .map(|(value, key)| {
                let element = self.stored(path, &key).expect("a held key");
                (value, key, element)
            })
            .collect()
    }

    /// The keys of the tree at `path` in the order of `axis`, lowest first,
    /// each with the value it is ranked by. Entries of one value are in the
    /// order of their keys.
    fn ranking(&self, path: &[Vec<u8>], axis: IndexAxis) -> Vec<(i128, Vec<u8>)> {
        let (key, parent) = path.split_last().expect("a tree under the root");
        let wide = matches!(
            self.held.get(parent).and_then(|tree| tree.get(key)),
            Some(
                Element::ProvableBigSumIndexedTree(..)
                    | Element::ProvableCountProvableBigSumIndexedTree(..)
            )
        );
        let mut ranking: Vec<(i128, Vec<u8>)> = self
            .entries(path)
            .map(|(key, element)| {
                let (count, sum) = self.adds(path, key, element);
                let value = match axis {
                    IndexAxis::Count => i128::from(count),
                    IndexAxis::Sum | IndexAxis::BigSum => sum,
                    IndexAxis::Avg => average(sum, count, wide),
                };
                (value, key.clone())
            })
            .collect();
        ranking.sort();
        ranking
    }
}

/// The scale of the fixed-point average an indexed tree ranks by.
const AVERAGE_SCALE: i128 = 10_000_000_000_000_000_000;

/// The average an indexed tree ranks an entry by: the sum over the count,
/// times [`AVERAGE_SCALE`], rounded down. The average of nothing is zero.
fn average(sum: i128, count: u64, wide: bool) -> i128 {
    if count == 0 {
        return 0;
    }
    let count = i128::from(count);
    if !wide {
        return (sum * AVERAGE_SCALE).div_euclid(count);
    }
    let whole = sum.div_euclid(count) * AVERAGE_SCALE;
    whole + sum.rem_euclid(count) * AVERAGE_SCALE / count
}

/// `element` without what GroveDB adds to a tree it stores: the root keys.
fn without_root_keys(element: Element) -> Element {
    let rootless = |axes: Vec<(u8, Option<Vec<u8>>)>| -> Vec<(u8, Option<Vec<u8>>)> {
        axes.into_iter().map(|(axis, _)| (axis, None)).collect()
    };
    match element {
        Element::Tree(_, flags) => Element::Tree(None, flags),
        Element::SumTree(_, sum, flags) => Element::SumTree(None, sum, flags),
        Element::BigSumTree(_, sum, flags) => Element::BigSumTree(None, sum, flags),
        Element::CountSumTree(_, count, sum, flags) => {
            Element::CountSumTree(None, count, sum, flags)
        }
        Element::CountBigSumTree(_, count, sum, flags) => {
            Element::CountBigSumTree(None, count, sum, flags)
        }
        Element::ProvableSumIndexedTree(_, _, sum, flags) => {
            Element::ProvableSumIndexedTree(None, None, sum, flags)
        }
        Element::ProvableBigSumIndexedTree(_, _, sum, flags) => {
            Element::ProvableBigSumIndexedTree(None, None, sum, flags)
        }
        Element::ProvableCountProvableSumIndexedTree(_, count, sum, axes, flags) => {
            Element::ProvableCountProvableSumIndexedTree(None, count, sum, rootless(axes), flags)
        }
        Element::ProvableCountProvableBigSumIndexedTree(_, count, sum, axes, flags) => {
            Element::ProvableCountProvableBigSumIndexedTree(None, count, sum, rootless(axes), flags)
        }
        other => other,
    }
}

/// The entries of an axis read as the value each is ranked by, its key and
/// its element.
fn axis_entries(entries: AxisEntries) -> (IndexAxis, Vec<(i128, Vec<u8>, Element)>) {
    fn widened<T: Into<i128>>(
        entries: Vec<grovedb::IndexedAxisEntry<T>>,
    ) -> Vec<(i128, Vec<u8>, Element)> {
        entries
            .into_iter()
            .map(|entry| {
                (
                    entry.ordering_value.into(),
                    entry.primary_key,
                    without_root_keys(entry.value),
                )
            })
            .collect()
    }
    match entries {
        AxisEntries::Count(entries) => (IndexAxis::Count, widened(entries)),
        AxisEntries::Sum(entries) => (IndexAxis::Sum, widened(entries)),
        AxisEntries::Avg(entries) => (IndexAxis::Avg, widened(entries)),
        AxisEntries::BigSum(entries) => (IndexAxis::BigSum, widened(entries)),
    }
}

fn axis_keys(keys: AxisKeys) -> (IndexAxis, Vec<(i128, Vec<u8>)>) {
    fn widened<T: Into<i128>>(keys: Vec<(T, Vec<u8>)>) -> Vec<(i128, Vec<u8>)> {
        keys.into_iter()
            .map(|(value, key)| (value.into(), key))
            .collect()
    }
    match keys {
        AxisKeys::Count(keys) => (IndexAxis::Count, widened(keys)),
        AxisKeys::Sum(keys) => (IndexAxis::Sum, widened(keys)),
        AxisKeys::Avg(keys) => (IndexAxis::Avg, widened(keys)),
        AxisKeys::BigSum(keys) => (IndexAxis::BigSum, widened(keys)),
    }
}

/// What an axis read selects from a ranking: the entries in the order they
/// are walked, and how many an offset skipped.
fn selected(
    mut ranking: Vec<(i128, Vec<u8>)>,
    query: &AxisQuery,
) -> (Vec<(i128, Vec<u8>)>, Option<u64>) {
    if query.descending {
        ranking.reverse();
    }
    match &query.traversal {
        AxisTraversal::RankedPage { k, offset } => {
            let skipped = (*offset).min(ranking.len() as u64);
            let page = ranking
                .into_iter()
                .skip(skipped as usize)
                .take(usize::from(*k))
                .collect();
            (page, Some(skipped))
        }
        AxisTraversal::Bounded { lo, hi, limit } => {
            let band = ranking
                .into_iter()
                .filter(|(value, _)| lo <= value && value <= hi)
                .take(usize::from(*limit))
                .collect();
            (band, None)
        }
        other => panic!("{other:?} selects no entries"),
    }
}

/// What [`the_rankings_follow_what_was_written`] checked, by the kind of
/// read.
#[derive(Debug, Default, PartialEq, Eq)]
struct RanksChecked {
    elements: u64,
    values_or_sums: u64,
    rows: u64,
    pages: u64,
    key_pages: u64,
    bands: u64,
    ranks: u64,
    aggregates: u64,
    branched: u64,
    deletes_of_nothing: u64,
    /// How many trees were read with a total no `i64` holds.
    totals_past_an_i64: u64,
    /// How many entries were read at a rank whose value no `i64` holds.
    ranked_past_an_i64: u64,
}

/// The two trees of `wide-ranks` keep the totals of what the stream wrote
/// into them, and every reader returns what the model holds: the elements,
/// the entries as stored, and the rankings.
#[test]
fn the_rankings_follow_what_was_written() {
    on_every_grove_version(|grove_version| {
        let version = grove_version.protocol_version;
        let features = Features::for_grove_version(grove_version);
        let workload = WideRanks {
            features,
            ..WideRanks::default()
        };
        for seed in SEEDS {
            let (_dir, db) = open();
            let mut transactions = Transactions::new(&db);
            let mut trees = Trees::default();
            let mut checked = RanksChecked::default();
            for (index, op) in workload.stream(seed).enumerate() {
                let context =
                    format!("wide-ranks on grove v{version}, seed {seed}: op {index}\n{op:?}");
                let outcome = transactions
                    .apply(&op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{context}\nrefused: {error:?}"));
                if let Some(held) = trees.record(&op) {
                    let Outcome::Flag(deleted) = outcome else {
                        panic!("{context}\nreturned {outcome:?}");
                    };
                    assert_eq!(deleted, held, "{context}");
                    checked.deletes_of_nothing += u64::from(!held);
                    continue;
                }
                let Op::Read(read) = &op else {
                    continue;
                };
                check_rank_read(&context, &trees, read, outcome, &mut checked);
            }
            let rounds = u64::from(workload.rounds);
            let ranked = if features.read_modes { rounds } else { 0 };
            assert_eq!(
                checked,
                RanksChecked {
                    elements: 2 * rounds,
                    values_or_sums: rounds,
                    rows: 4 * rounds,
                    pages: 2 * ranked,
                    key_pages: 2 * ranked,
                    bands: 2 * ranked,
                    ranks: 2 * ranked,
                    aggregates: 2 * ranked,
                    branched: ranked,
                    deletes_of_nothing: 2 * (rounds - 1),
                    totals_past_an_i64: checked.totals_past_an_i64,
                    ranked_past_an_i64: checked.ranked_past_an_i64,
                },
                "wide-ranks on grove v{version}, seed {seed}"
            );
            assert_eq!(
                (
                    checked.totals_past_an_i64 > 0,
                    checked.ranked_past_an_i64 > 0
                ),
                (features.big_sum_tree_twins, features.big_sum_axis),
                "wide-ranks on grove v{version}, seed {seed}: {checked:?}"
            );
        }
    });
}

fn past_an_i64(value: i128) -> bool {
    i64::try_from(value).is_err()
}

fn check_rank_read(
    context: &str,
    trees: &Trees,
    read: &Read,
    outcome: Outcome,
    checked: &mut RanksChecked,
) {
    match (read, outcome) {
        (Read::Get { path, key }, Outcome::Element(element)) => {
            let total = match &element {
                Element::ProvableBigSumIndexedTree(_, _, sum, _)
                | Element::ProvableCountProvableBigSumIndexedTree(_, _, sum, ..) => *sum,
                _ => 0,
            };
            checked.totals_past_an_i64 += u64::from(past_an_i64(total));
            assert_eq!(
                Some(without_root_keys(element)),
                trees.stored(path, key),
                "{context}"
            );
            checked.elements += 1;
        }
        (
            Read::ItemValues {
                query,
                or_sums: true,
            },
            Outcome::ItemValuesOrSums { values, .. },
        ) => {
            // The values come in the order of the keys.
            let mut keys: Vec<&Vec<u8>> = query
                .query
                .query
                .items
                .iter()
                .map(|item| match item {
                    grovedb::QueryItem::Key(key) => key,
                    other => panic!("{context}\n{other:?} is not a key"),
                })
                .collect();
            keys.sort_unstable();
            let expected: Vec<_> = keys
                .into_iter()
                .map(
                    |key| match trees.stored(&query.path, key).expect("a held tree") {
                        Element::ProvableSumIndexedTree(_, _, sum, _) => {
                            QueryItemOrSumReturnType::SumValue(sum)
                        }
                        Element::ProvableBigSumIndexedTree(_, _, sum, _) => {
                            QueryItemOrSumReturnType::BigSumValue(sum)
                        }
                        Element::ProvableCountProvableSumIndexedTree(_, count, sum, ..) => {
                            QueryItemOrSumReturnType::CountSumValue(count, sum)
                        }
                        Element::ProvableCountProvableBigSumIndexedTree(_, count, sum, ..) => {
                            QueryItemOrSumReturnType::CountBigSumValue(count, sum)
                        }
                        other => panic!("{context}\nreads {other:?} as a number"),
                    },
                )
                .collect();
            assert_eq!(values, expected, "{context}");
            checked.values_or_sums += 1;
        }
        (Read::PathQuery { query, .. }, Outcome::Run(run)) => {
            check_rank_query(context, trees, query, run, checked)
        }
        (read, outcome) => panic!("{context}\n{read:?} returned {outcome:?}"),
    }
}

/// The rows a query of `wide-ranks` selects: the keys of one tree in a
/// range, up to a limit, either in the tree the query starts in or in the
/// one tree it names there.
fn rank_rows(trees: &Trees, query: &PathQuery) -> Vec<PathKeyElementTrio> {
    let selects = &query.query.query;
    let (path, items) = match selects.default_subquery_branch.subquery.as_deref() {
        None => (query.path.clone(), &selects.items),
        Some(subquery) => {
            let [grovedb::QueryItem::Key(key)] = selects.items.as_slice() else {
                panic!("{query:?} does not name one tree");
            };
            (below(&query.path, key), &subquery.items)
        }
    };
    trees
        .entries(&path)
        .filter(|(key, _)| items.iter().any(|item| item.contains(key)))
        .take(query.query.limit.map_or(usize::MAX, usize::from))
        .map(|(key, _)| {
            let element = trees.stored(&path, key).expect("a held key");
            (path.clone(), key.clone(), element)
        })
        .collect()
}

fn check_rank_query(
    context: &str,
    trees: &Trees,
    query: &PathQuery,
    run: PathQueryRun,
    checked: &mut RanksChecked,
) {
    let shape = query
        .classify()
        .unwrap_or_else(|error| panic!("{context}\nno shape: {error:?}"));
    let mut count_past = |axis: IndexAxis, values: &[i128]| {
        if matches!(axis, IndexAxis::Sum | IndexAxis::BigSum) {
            checked.ranked_past_an_i64 +=
                values.iter().filter(|value| past_an_i64(**value)).count() as u64;
        }
    };
    match (shape, run) {
        (PathQueryShape::KeySelection, PathQueryRun::Elements { elements, .. }) => {
            let returned: Vec<_> = elements
                .to_path_key_elements()
                .into_iter()
                .map(|(path, key, element)| (path, key, without_root_keys(element)))
                .collect();
            assert_eq!(returned, rank_rows(trees, query), "{context}");
            checked.rows += 1;
        }
        (PathQueryShape::AxisRead { axis }, PathQueryRun::AxisEntries { entries, skipped }) => {
            assert_eq!(axis.projection, AxisProjection::Entries, "{context}");
            let (expected, expected_skipped) =
                selected(trees.ranking(&query.path, axis.axis), axis);
            let expected = trees.with_elements(&query.path, expected);
            let (read_axis, entries) = axis_entries(entries);
            assert_eq!(read_axis, axis.axis, "{context}");
            assert_eq!(entries, expected, "{context}");
            assert_eq!(skipped, expected_skipped, "{context}");
            let values: Vec<i128> = entries.iter().map(|entry| entry.0).collect();
            count_past(axis.axis, &values);
            match axis.traversal {
                AxisTraversal::RankedPage { .. } => checked.pages += 1,
                _ => checked.bands += 1,
            }
        }
        (PathQueryShape::AxisRead { axis }, PathQueryRun::AxisKeys { keys, skipped }) => {
            assert_eq!(axis.projection, AxisProjection::Keys, "{context}");
            let (expected, expected_skipped) =
                selected(trees.ranking(&query.path, axis.axis), axis);
            let (read_axis, keys) = axis_keys(keys);
            assert_eq!(read_axis, axis.axis, "{context}");
            assert_eq!(keys, expected, "{context}");
            assert_eq!(skipped, expected_skipped, "{context}");
            let values: Vec<i128> = keys.iter().map(|key| key.0).collect();
            count_past(axis.axis, &values);
            checked.key_pages += 1;
        }
        (PathQueryShape::AxisRead { axis }, PathQueryRun::AxisRank(rank)) => {
            let AxisTraversal::RankOfKey { key } = &axis.traversal else {
                panic!("{context}\nreturned a rank");
            };
            let mut ranking = trees.ranking(&query.path, axis.axis);
            if axis.descending {
                ranking.reverse();
            }
            let expected = ranking.iter().position(|(_, held)| held == key);
            assert_eq!(Some(rank), expected.map(|rank| rank as u64), "{context}");
            checked.ranks += 1;
        }
        (PathQueryShape::AxisRead { axis }, PathQueryRun::AxisAggregate(value)) => {
            let AxisTraversal::AggregateOverValueRange { lo, hi, fold } = &axis.traversal else {
                panic!("{context}\nreturned an aggregate");
            };
            let band: Vec<i128> = trees
                .ranking(&query.path, axis.axis)
                .into_iter()
                .map(|(value, _)| value)
                .filter(|value| lo <= value && value <= hi)
                .collect();
            let expected = match fold {
                AggregateFold::Population => AxisAggregateValue::Population(band.len() as u64),
                AggregateFold::Total => AxisAggregateValue::Total(
                    i64::try_from(band.iter().sum::<i128>()).expect("a narrow total"),
                ),
            };
            assert_eq!(format!("{value:?}"), format!("{expected:?}"), "{context}");
            checked.aggregates += 1;
        }
        (
            PathQueryShape::BranchedAxisRead { axis, .. },
            PathQueryRun::BranchedAxisEntries(branches),
        ) => {
            let outer = &query.query.query;
            let inner = outer
                .default_subquery_branch
                .subquery_path
                .clone()
                .expect("a branched read names the tree in each branch");
            let mut expected = Vec::new();
            for item in &outer.items {
                let grovedb::QueryItem::Key(branch) = item else {
                    panic!("{context}\n{item:?} is not a key");
                };
                let mut tree = below(&query.path, branch);
                let held = trees.held.contains_key(&tree);
                tree.extend(inner.iter().cloned());
                let page = held.then(|| {
                    let (page, _) = selected(trees.ranking(&tree, axis.axis), axis);
                    trees.with_elements(&tree, page)
                });
                expected.push((branch.clone(), page));
            }
            let branches: Vec<_> = branches
                .into_iter()
                .map(|(branch, entries)| {
                    let entries = entries.map(|entries| {
                        let (read_axis, entries) = axis_entries(entries);
                        assert_eq!(read_axis, axis.axis, "{context}");
                        entries
                    });
                    (branch, entries)
                })
                .collect();
            assert_eq!(branches, expected, "{context}");
            for (_, page) in &branches {
                let values: Vec<i128> = page.iter().flatten().map(|entry| entry.0).collect();
                count_past(axis.axis, &values);
            }
            checked.branched += 1;
        }
        (shape, run) => panic!("{context}\na query of shape {shape:?} returned {run:?}"),
    }
}

/// The keys of the tree at `path` that `query` selects, in the order it
/// walks them, each with the subquery it descends into the key with.
fn walked<'a>(
    trees: &'a Trees,
    query: &'a grovedb::Query,
    path: &[Vec<u8>],
) -> Vec<(&'a Vec<u8>, Option<&'a grovedb::Query>)> {
    let mut keys: Vec<_> = trees
        .entries(path)
        .filter(|(key, _)| query.items.iter().any(|item| item.contains(key)))
        .map(|(key, element)| {
            let branch = query
                .conditional_subquery_branches
                .iter()
                .flatten()
                .find(|(item, _)| item.contains(key))
                .map_or(&query.default_subquery_branch, |(_, branch)| branch);
            assert!(
                branch.subquery_path.is_none(),
                "{query:?} has a subquery path"
            );
            let subquery = branch.subquery.as_deref();
            (key, subquery.filter(|_| element.is_any_tree()))
        })
        .collect();
    if !query.left_to_right {
        keys.reverse();
    }
    keys
}

/// The rows a query of `capped-layers` returns on a grove version that has
/// limits of query nodes, and what the limits did on the way.
///
/// A row is charged to every limit in force. A tree below a key that gives
/// no row and charges nothing is charged to the limit of the whole query,
/// as a row would be, and to no limit of a query node.
#[derive(Default)]
struct Walk {
    rows: Vec<PathKeyElementTrio>,
    /// The rows still owed to each limit in force: the one of the whole
    /// query first, where there is one, then one for each query node with
    /// a limit on the way down.
    budgets: Vec<usize>,
    /// Whether the first budget is the limit of the whole query.
    whole: bool,
    /// How many layers with subqueries a limit of a query node ended with
    /// keys left to walk.
    layers_cut: u64,
}

impl Walk {
    /// Walks `query` at `path` and returns how much it charged to the
    /// limit of the whole query.
    fn walk(&mut self, trees: &Trees, query: &grovedb::Query, path: &[Vec<u8>]) -> usize {
        if let Some(limit) = query.limit {
            self.budgets.push(usize::from(limit));
        }
        let mut charged = 0;
        for (key, subquery) in walked(trees, query, path) {
            if self.budgets.contains(&0) {
                let of_nodes = &self.budgets[usize::from(self.whole)..];
                self.layers_cut += u64::from(query.has_subquery() && of_nodes.contains(&0));
                break;
            }
            match subquery {
                Some(subquery) => {
                    let inside = self.walk(trees, subquery, &below(path, key));
                    if inside == 0 && self.whole {
                        self.budgets[0] -= 1;
                        charged += 1;
                    }
                    charged += inside;
                }
                None => {
                    let element = trees.stored(path, key).expect("a held key");
                    self.rows.push((path.to_vec(), key.clone(), element));
                    for budget in &mut self.budgets {
                        *budget -= 1;
                    }
                    charged += 1;
                }
            }
        }
        if query.limit.is_some() {
            self.budgets.pop();
        }
        charged
    }
}

/// The rows a query of `capped-layers` returns on grove versions 1 to 3,
/// which have a limit of the whole query only.
///
/// The walk below a key starts with what is left of the limit, and the
/// layer above is then charged for the rows that came back, or for one row
/// if none did. What the walk below charged for trees that gave no row is
/// not carried up.
fn legacy_rows(
    trees: &Trees,
    query: &grovedb::Query,
    path: &[Vec<u8>],
    mut limit: Option<usize>,
) -> Vec<PathKeyElementTrio> {
    assert!(query.limit.is_none(), "{query:?} has a limit of its own");
    let mut rows = Vec::new();
    for (key, subquery) in walked(trees, query, path) {
        if limit == Some(0) {
            break;
        }
        let found = match subquery {
            Some(subquery) => legacy_rows(trees, subquery, &below(path, key), limit),
            None => {
                let element = trees.stored(path, key).expect("a held key");
                vec![(path.to_vec(), key.clone(), element)]
            }
        };
        if let Some(limit) = &mut limit {
            *limit -= found.len().max(1);
        }
        rows.extend(found);
    }
    rows
}

/// What a query of `capped-layers` returns: the rows, and how many layers
/// with subqueries a limit of a query node ended early.
fn layer_rows(
    trees: &Trees,
    query: &PathQuery,
    features: Features,
) -> (Vec<PathKeyElementTrio>, u64) {
    assert!(query.query.offset.is_none(), "{query:?} has an offset");
    let limit = query.query.limit.map(usize::from);
    if !features.instance_limits {
        return (
            legacy_rows(trees, &query.query.query, &query.path, limit),
            0,
        );
    }
    let mut walk = Walk {
        budgets: limit.into_iter().collect(),
        whole: limit.is_some(),
        ..Walk::default()
    };
    walk.walk(trees, &query.query.query, &query.path);
    (walk.rows, walk.layers_cut)
}

/// How many pages a round of `capped-layers` reads.
const CAPPED_PAGES_PER_ROUND: u64 = 7;

/// Every page of `capped-layers` holds the rows the index holds in the
/// order the query walks them, up to its limits, on every grove version.
#[test]
fn the_pages_hold_what_the_index_holds() {
    on_every_grove_version(|grove_version| {
        let version = grove_version.protocol_version;
        let features = Features::for_grove_version(grove_version);
        let workload = CappedLayers {
            features,
            ..CappedLayers::default()
        };
        for seed in SEEDS {
            let (_dir, db) = open();
            let mut transactions = Transactions::new(&db);
            let mut trees = Trees::default();
            let (mut pages, mut layers_cut) = (0u64, 0u64);
            for (index, op) in workload.stream(seed).enumerate() {
                let context =
                    format!("capped-layers on grove v{version}, seed {seed}: op {index}\n{op:?}");
                let outcome = transactions
                    .apply(&op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("{context}\nrefused: {error:?}"));
                trees.record(&op);
                let Op::Read(read) = &op else {
                    continue;
                };
                let (
                    Read::PathQuery { query, .. } | Read::Rows { query, raw: true },
                    Outcome::Run(PathQueryRun::Elements { elements, .. }),
                ) = (read, outcome)
                else {
                    panic!("{context}\nis no query for rows");
                };
                let returned: Vec<_> = elements
                    .to_path_key_elements()
                    .into_iter()
                    .map(|(path, key, element)| (path, key, without_root_keys(element)))
                    .collect();
                let (expected, cut) = layer_rows(&trees, query, features);
                assert_eq!(returned, expected, "{context}");
                pages += 1;
                layers_cut += cut;
            }
            assert_eq!(
                pages,
                CAPPED_PAGES_PER_ROUND * u64::from(workload.rounds),
                "capped-layers on grove v{version}, seed {seed}"
            );
            assert_eq!(
                layers_cut > 0,
                features.instance_limits,
                "capped-layers on grove v{version}, seed {seed}: {layers_cut} layers cut"
            );
        }
    });
}

/// The groups of the operations this file's workloads were added for.
const LATER_GROUPS: [&str; 3] = [
    "indexed trees with a 128-bit sum",
    "limits over subqueries",
    "trees a batch creates and fills",
];

fn path(segments: &[&[u8]]) -> Path {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

/// A stream with one operation of each kind added for the indexed trees
/// with a 128-bit sum, and one read of each form of [`LATER_GROUPS`].
fn one_of_each_later_kind() -> Vec<Op> {
    let (top, ranked, both, index) = (
        path(&[b"top"]),
        path(&[b"top", b"ranked"]),
        path(&[b"top", b"both"]),
        path(&[b"top", b"index"]),
    );
    let insert_only = |at: &Path, key: &[u8], element: Element| BatchOp::InsertOnly {
        path: at.clone(),
        key: key.to_vec(),
        element,
    };
    let insert = |tree: &Path, key: &[u8], element: Element, methods: IndexedTree| {
        Op::Write(Write::IndexedInsert {
            path: tree.clone(),
            key: key.to_vec(),
            element,
            tree: methods,
        })
    };
    let delete = |tree: &Path, key: &[u8], methods: IndexedTree| {
        Op::Write(Write::IndexedDelete {
            path: tree.clone(),
            key: key.to_vec(),
            tree: methods,
        })
    };
    let read = |query: PathQuery| {
        Op::Read(Read::PathQuery {
            query: Box::new(query),
            proven: true,
        })
    };
    let amount = Element::new_sum_item;
    let entry = |sum: i64| Element::new_item_with_sum_item(b"entry".to_vec(), sum);
    let page = || {
        let mut keys = grovedb::Query::new_range_full();
        keys.set_subquery(grovedb::Query::new_range_full());
        keys.limit = Some(1);
        keys
    };
    let mut under = grovedb::Query::new_single_key(b"index".to_vec());
    under.limit = Some(1);
    under.set_subquery({
        let mut keys = grovedb::Query::new_range_full();
        keys.set_subquery(grovedb::Query::new_range_full());
        keys
    });
    let mut merged = grovedb::Query::new();
    merged.insert_key(b"index".to_vec());
    merged.add_conditional_subquery(
        grovedb::QueryItem::Key(b"index".to_vec()),
        None,
        Some(page()),
    );
    let sums = IndexAxis::BigSum;
    vec![
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"top".to_vec(),
            element: Element::empty_tree(),
        }),
        Op::Batch(vec![
            insert_only(
                &top,
                b"ranked",
                Element::empty_provable_big_sum_indexed_tree(),
            ),
            insert_only(
                &top,
                b"both",
                Element::ProvableCountProvableBigSumIndexedTree(
                    None,
                    0,
                    0,
                    [IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]
                        .map(|axis| (axis.tag(), None))
                        .to_vec(),
                    None,
                ),
            ),
            insert_only(&top, b"index", Element::empty_tree()),
            insert_only(&index, b"k1", Element::empty_tree()),
            insert_only(
                &path(&[b"top", b"index", b"k1"]),
                b"d",
                Element::new_item(vec![1]),
            ),
            insert_only(&index, b"k2", Element::empty_tree()),
            insert_only(
                &path(&[b"top", b"index", b"k2"]),
                b"d",
                Element::new_item(vec![2]),
            ),
        ]),
        insert(&ranked, b"a", amount(i64::MAX), IndexedTree::BigSum),
        insert(&ranked, b"b", amount(i64::MAX), IndexedTree::BigSum),
        insert(&ranked, b"c", amount(-5), IndexedTree::BigSum),
        insert(&both, b"a", entry(i64::MAX), IndexedTree::CountBigSum),
        insert(&both, b"b", entry(i64::MAX), IndexedTree::CountBigSum),
        delete(&ranked, b"c", IndexedTree::BigSum),
        delete(&both, b"b", IndexedTree::CountBigSum),
        Op::Commit,
        read(PathQuery::new_axis_top_k(ranked.clone(), sums, 2, 0, true)),
        read(PathQuery::new_axis_bounded(
            ranked.clone(),
            sums,
            i128::MIN,
            i128::MAX,
            5,
            false,
        )),
        read(PathQuery::new_axis_rank_of_key(
            ranked.clone(),
            sums,
            b"a".to_vec(),
            true,
        )),
        read(PathQuery::new_axis_aggregate_over_value_range(
            ranked.clone(),
            sums,
            0,
            i128::MAX,
            AggregateFold::Population,
        )),
        read(PathQuery::new_branched_axis(
            Vec::new(),
            vec![b"top".to_vec()],
            vec![b"ranked".to_vec()],
            AxisQuery::top_k(sums, 1, 0, true),
        )),
        read(PathQuery::new_unsized(index, page())),
        read(PathQuery::new_unsized(top.clone(), under)),
        read(PathQuery::new_unsized(top, merged)),
    ]
}

const PINNED_LATER_KINDS: &str = "43c7cc3640aa743236003466b6192288969066856431f05cc0c33ea194bdc722";

/// The canonical encoding of the kinds added for the indexed trees with a
/// 128-bit sum: the bytes are pinned, and a write to one key encodes
/// differently for each type of indexed tree. With the reads, they
/// are every operation of [`LATER_GROUPS`], in a stream the latest grove
/// version accepts.
#[test]
fn the_later_kinds_have_a_pinned_encoding() {
    let ops = one_of_each_later_kind();
    let mut encodings = BTreeSet::new();
    for op in &ops {
        let mut bytes = Vec::new();
        op.encode(&mut bytes);
        assert!(encodings.insert(bytes), "two operations encode as {op:?}");
    }
    let digest = StreamDigest::of(&ops);
    assert_eq!(digest.ops(), ops.len() as u64);
    assert_eq!(digest.hex(), PINNED_LATER_KINDS);

    // The same write to the same key differs by the type of the tree.
    let methods = [
        IndexedTree::Count,
        IndexedTree::Sum,
        IndexedTree::CountSum,
        IndexedTree::BigSum,
        IndexedTree::CountBigSum,
    ];
    let (tree, key) = (path(&[b"top", b"ranked"]), b"a".to_vec());
    let mut of_one_key = BTreeSet::new();
    for methods in methods {
        let insert = Op::Write(Write::IndexedInsert {
            path: tree.clone(),
            key: key.clone(),
            element: Element::new_sum_item(1),
            tree: methods,
        });
        let delete = Op::Write(Write::IndexedDelete {
            path: tree.clone(),
            key: key.clone(),
            tree: methods,
        });
        for op in [insert, delete] {
            assert!(
                of_one_key.insert(StreamDigest::of([&op]).hex()),
                "{op:?} encodes as another type of tree does"
            );
        }
    }

    let coverage = Coverage::of(&ops);
    let later = |operations: &[Operation]| -> Vec<Operation> {
        operations
            .iter()
            .copied()
            .filter(|operation| LATER_GROUPS.contains(&operation.group()))
            .collect()
    };
    let produced: Vec<Operation> = coverage.operations().iter().copied().collect();
    assert_eq!(later(&produced), later(Operation::ALL));
    assert_eq!(later(Operation::ALL).len(), 13);

    let (_dir, db) = open();
    let mut transactions = Transactions::new(&db);
    for op in &ops {
        transactions
            .apply(op, GroveVersion::latest())
            .unwrap()
            .unwrap_or_else(|error| panic!("{op:?}\nrefused: {error:?}"));
    }
}

/// What [`a_page_is_proven_by_fewer_keys_from_grove_version_5`] saw, over
/// the pages of one stream.
#[derive(Debug, Default)]
struct PageProofs {
    pages: u64,
    /// How many pages the newer grove version proves in fewer bytes.
    shorter: u64,
    older_bytes: usize,
    newer_bytes: usize,
}

/// The streams of `capped-layers` are the same for grove versions 4 and 5,
/// and so are the rows of every page. The proofs are not: grove version 4
/// proves the layer a limit of a query node is in force on with every key
/// in range, and grove version 5 ends it where the limit was spent. A
/// proof of grove version 5 that is shorter is one grove version 4 does
/// not accept, and grove version 5 accepts every proof of grove version 4.
#[test]
fn a_page_is_proven_by_fewer_keys_from_grove_version_5() {
    let (older, newer) = (&GROVE_VERSIONS[3], &GROVE_VERSIONS[4]);
    assert_eq!((older.protocol_version, newer.protocol_version), (4, 5));
    let stream = |grove_version: &GroveVersion, seed: u64| {
        let workload = CappedLayers {
            features: Features::for_grove_version(grove_version),
            ..CappedLayers::default()
        };
        workload.stream(seed)
    };
    // The rows a proof verifies to and their root, or nothing if it does
    // not verify.
    let verified = |proof: &[u8], query: &PathQuery, grove_version: &GroveVersion| {
        match GroveDb::verify_path_query(proof, query, grove_version) {
            Ok(VerifiedPathQuery::Elements {
                root_hash,
                elements,
            }) => Some((root_hash, elements)),
            Ok(other) => panic!("{query:?} verified to {other:?}"),
            Err(_) => None,
        }
    };
    on_a_big_stack(|| {
        for seed in SEEDS {
            assert_eq!(
                StreamDigest::of(&stream(older, seed).collect::<Vec<_>>()).hex(),
                StreamDigest::of(&stream(newer, seed).collect::<Vec<_>>()).hex(),
                "seed {seed}"
            );
            let (_older_dir, older_db) = open();
            let (_newer_dir, newer_db) = open();
            let mut older_transactions = Transactions::new(&older_db);
            let mut newer_transactions = Transactions::new(&newer_db);
            let mut proofs = PageProofs::default();
            for (index, op) in stream(newer, seed).enumerate() {
                let context = format!("capped-layers, seed {seed}: op {index}\n{op:?}");
                let before = older_transactions
                    .apply(&op, older)
                    .unwrap()
                    .expect("applies");
                let after = newer_transactions
                    .apply(&op, newer)
                    .unwrap()
                    .expect("applies");
                let Op::Read(Read::PathQuery { query, .. }) = &op else {
                    continue;
                };
                assert_eq!(format!("{before:?}"), format!("{after:?}"), "{context}");
                let old_proof = older_db
                    .prove_query(query, None, older)
                    .unwrap()
                    .expect("proof");
                let new_proof = newer_db
                    .prove_query(query, None, newer)
                    .unwrap()
                    .expect("proof");
                let (old_root, old_rows) =
                    verified(&old_proof, query, older).expect("its own version verifies");
                let (new_root, new_rows) =
                    verified(&new_proof, query, newer).expect("its own version verifies");
                assert_eq!(old_root, new_root, "{context}");
                assert_eq!(old_rows, new_rows, "{context}");
                assert_eq!(
                    verified(&old_proof, query, newer).expect("the older proof verifies"),
                    (new_root, new_rows),
                    "{context}"
                );
                assert!(new_proof.len() <= old_proof.len(), "{context}");
                if new_proof.len() < old_proof.len() {
                    assert!(verified(&new_proof, query, older).is_none(), "{context}");
                    proofs.shorter += 1;
                } else {
                    assert_eq!(new_proof, old_proof, "{context}");
                }
                proofs.pages += 1;
                proofs.older_bytes += old_proof.len();
                proofs.newer_bytes += new_proof.len();
            }
            println!("seed {seed}: {proofs:?}");
            assert_eq!(
                proofs.pages,
                CAPPED_PAGES_PER_ROUND * u64::from(CappedLayers::default().rounds)
            );
            assert!(2 * proofs.shorter > proofs.pages, "seed {seed}: {proofs:?}");
        }
    });
}

fn on_a_big_stack(body: impl FnOnce() + Send) {
    thread::scope(|scope| {
        thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, body)
            .expect("spawn");
    });
}

/// `workload::apply` sets its savepoints through `GroveDb`'s own methods.
/// It has a fallback for the engine the corpus is pinned to, which sets
/// them on the transaction, and a method of `GroveDb` is preferred to it.
/// Here the fallback is not in scope, so this builds only while `GroveDb`
/// has the two methods.
#[test]
fn grovedb_sets_savepoints_itself() {
    let (_dir, db) = open();
    let grove_version = GroveVersion::latest();
    let transaction = db.start_transaction();
    let root = |transaction| {
        db.root_hash(Some(transaction), grove_version)
            .unwrap()
            .expect("root hash")
    };
    let before = root(&transaction);
    db.set_savepoint(&transaction).expect("a savepoint");
    db.insert(
        &[] as &[&[u8]],
        b"key",
        Element::empty_tree(),
        None,
        Some(&transaction),
        grove_version,
    )
    .unwrap()
    .expect("insert");
    assert_ne!(root(&transaction), before);
    db.rollback_to_savepoint(&transaction).expect("back to it");
    assert_eq!(root(&transaction), before);
}

/// The proofs of `legacy-limit-over-empty-subtrees`, which the corpus
/// records as verified or not, against the rows the reads return: on grove
/// versions 1 and 2 they do not verify, on grove version 3 the proof of
/// the second page verifies to other rows than the read returns, and from
/// grove version 4 read and proof agree.
#[test]
fn a_limit_over_keys_without_documents_is_proven_as_it_is_read_from_grove_version_4() {
    let case = lignin_testkit::golden::legacy::cases()
        .into_iter()
        .find(|case| case.name == "limit-over-empty-subtrees")
        .expect("the case");
    let mut agreed = Vec::new();
    for grove_version in GROVE_VERSIONS {
        let (_dir, db) = open();
        let mut transactions = Transactions::new(&db);
        let mut pages = Vec::new();
        for op in &case.ops {
            let outcome = transactions
                .apply(op, grove_version)
                .unwrap()
                .expect("applies");
            let (Op::Read(Read::PathQuery { query, .. }), Outcome::Run(run)) = (op, outcome) else {
                continue;
            };
            let PathQueryRun::Elements { elements, .. } = run else {
                panic!("{query:?} returned {run:?}");
            };
            let read: Vec<_> = elements
                .to_path_key_elements()
                .into_iter()
                .map(|(path, key, element)| (path, key, Some(element)))
                .collect();
            let proof = db
                .prove_query(query, None, grove_version)
                .unwrap()
                .expect("proof");
            pages.push(
                match GroveDb::verify_path_query(&proof, query, grove_version) {
                    Ok(VerifiedPathQuery::Elements { elements, .. }) => Some(elements == read),
                    Ok(other) => panic!("{query:?} verified to {other:?}"),
                    Err(_) => None,
                },
            );
        }
        agreed.push(pages);
    }
    println!("{agreed:?}");
    assert_eq!(
        agreed,
        [
            vec![None, None],
            vec![None, None],
            vec![Some(true), Some(false)],
            vec![Some(true), Some(true)],
            vec![Some(true), Some(true)],
        ]
    );
}
