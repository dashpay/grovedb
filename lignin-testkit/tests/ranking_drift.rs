//! Whether the rankings of an indexed tree follow its entries when batches
//! write them, looked for in the streams of `wide-ranks`.
//!
//! On grove versions 1 to 4 the removal of an entry can leave its row in
//! the rankings (the corpus case `ranking-drift`). `wide-ranks` therefore
//! writes its entries through the methods of the trees on those versions
//! and repairs the rankings before it reads them
//! (`Features::indexed_batches_in_step`). Here the stream is generated as
//! for a version whose batches keep the rankings in step, applied to grove
//! version 4, and after every commit every ranking of every indexed tree is
//! compared with the entries of the tree:
//!
//! - **left behind**: a row for a key the tree does not hold;
//! - **missing**: a key of the tree with no row;
//! - **stale**: a row that ranks a key at another count or sum than its
//!   element has;
//! - **short**: a key that a read of the ranking's keys returns and a read
//!   of its entries does not.
//!
//! Rows are left behind in nearly every stream, and a read of the entries
//! then fails on the row of a removed key, or ranks every entry after it
//! one place late. None of the other three shows: no ranking was found to
//! lack the row of an entry.

use std::collections::BTreeMap;

use grovedb::{
    operations::proof::indexed_axis::AxisEntries, AxisKeys, AxisQuery, Element, GroveDb, PathQuery,
    PathQueryRun, Query,
};
use grovedb_merk::proofs::query::IndexAxis;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::workload::{
    adversarial::WideRanks, apply, BatchOp, Features, Op, Outcome, Read, Transactions, Write,
};

type Path = Vec<Vec<u8>>;

/// The axes named by the tags of `axes`, which are among `known`.
fn axes_by_tag(axes: &[(u8, Option<Vec<u8>>)], known: [IndexAxis; 3]) -> Vec<IndexAxis> {
    axes.iter()
        .map(|(tag, _)| {
            known
                .into_iter()
                .find(|axis| axis.tag() == *tag)
                .expect("an axis of the tree")
        })
        .collect()
}

/// The axes an indexed tree ranks its entries on. None for any other
/// element.
fn axes_of(element: &Element) -> Option<Vec<IndexAxis>> {
    use IndexAxis::{Avg, BigSum, Count, Sum};
    Some(match element {
        Element::ProvableSumIndexedTree(..) => vec![Sum],
        Element::ProvableCountIndexedTree(..) => vec![Count],
        Element::ProvableBigSumIndexedTree(..) => vec![BigSum],
        Element::ProvableCountProvableSumIndexedTree(.., axes, _) => {
            axes_by_tag(axes, [Count, Sum, Avg])
        }
        Element::ProvableCountProvableBigSumIndexedTree(.., axes, _) => {
            axes_by_tag(axes, [Count, Avg, BigSum])
        }
        _ => return None,
    })
}

/// The indexed trees a stream creates, each with its axes.
fn indexed_trees(ops: &[Op]) -> Vec<(Path, Vec<IndexAxis>)> {
    let mut trees: Vec<(Path, Vec<IndexAxis>)> = Vec::new();
    let mut see = |path: &Path, key: &[u8], element: &Element| {
        if let Some(axes) = axes_of(element) {
            let mut tree = path.clone();
            tree.push(key.to_vec());
            if !trees.iter().any(|(held, _)| *held == tree) {
                trees.push((tree, axes));
            }
        }
    };
    for op in ops {
        if let Op::Write(
            Write::Insert { path, key, element }
            | Write::IndexedInsert {
                path, key, element, ..
            },
        ) = op
        {
            see(path, key, element);
        }
        for batch_op in op.batch_ops() {
            if let BatchOp::InsertOnly { path, key, element }
            | BatchOp::InsertOrReplace { path, key, element }
            | BatchOp::Replace { path, key, element } = batch_op
            {
                see(path, key, element);
            }
        }
    }
    trees
}

/// How the rankings of a database differ from its trees.
#[derive(Debug, Default, PartialEq, Eq)]
struct Drift {
    left_behind: u64,
    missing: u64,
    stale: u64,
    short: u64,
    /// How many reads of the entries of a ranking failed.
    unreadable: u64,
}

impl Drift {
    fn add(&mut self, other: &Drift) {
        self.left_behind += other.left_behind;
        self.missing += other.missing;
        self.stale += other.stale;
        self.short += other.short;
        self.unreadable += other.unreadable;
    }

    /// One for each kind of difference there is any of.
    fn shown(&self) -> Drift {
        Drift {
            left_behind: u64::from(self.left_behind > 0),
            missing: u64::from(self.missing > 0),
            stale: u64::from(self.stale > 0),
            short: u64::from(self.short > 0),
            unreadable: u64::from(self.unreadable > 0),
        }
    }
}

fn committed(db: &GroveDb, read: Read, grove_version: &GroveVersion) -> Result<Outcome, String> {
    apply(db, &Op::Read(read), None, grove_version)
        .unwrap()
        .map_err(|error| format!("{error:?}"))
}

/// Compares every ranking of `trees` with the entries of its tree, in
/// committed state.
fn drift(db: &GroveDb, trees: &[(Path, Vec<IndexAxis>)], grove_version: &GroveVersion) -> Drift {
    let mut drift = Drift::default();
    for (tree, axes) in trees {
        let rows = Read::Rows {
            query: Box::new(PathQuery::new_unsized(
                tree.clone(),
                Query::new_range_full(),
            )),
            raw: true,
        };
        let held: BTreeMap<Vec<u8>, Element> = match committed(db, rows, grove_version) {
            Ok(Outcome::Run(PathQueryRun::Elements { elements, .. })) => elements
                .to_path_key_elements()
                .into_iter()
                .map(|(_, key, element)| (key, element))
                .collect(),
            other => panic!("the rows of {tree:?}: {other:?}"),
        };
        for axis in axes {
            let page = AxisQuery::top_k(*axis, u16::MAX, 0, false);
            let keys = Read::PathQuery {
                query: Box::new(PathQuery::new_axis(tree.clone(), page.clone().keys_only())),
                proven: false,
            };
            let ranked: BTreeMap<Vec<u8>, i128> = match committed(db, keys, grove_version) {
                Ok(Outcome::Run(PathQueryRun::AxisKeys { keys, .. })) => match keys {
                    AxisKeys::Count(keys) => keys.into_iter().map(|(v, k)| (k, v.into())).collect(),
                    AxisKeys::Sum(keys) => keys.into_iter().map(|(v, k)| (k, v.into())).collect(),
                    AxisKeys::Avg(keys) | AxisKeys::BigSum(keys) => {
                        keys.into_iter().map(|(v, k)| (k, v)).collect()
                    }
                },
                other => panic!("the keys of {tree:?} on {axis:?}: {other:?}"),
            };
            drift.left_behind +=
                ranked.keys().filter(|key| !held.contains_key(*key)).count() as u64;
            drift.missing += held.keys().filter(|key| !ranked.contains_key(*key)).count() as u64;
            drift.stale += held
                .iter()
                .filter(|(key, element)| {
                    let expected: i128 = match axis {
                        IndexAxis::Count => element.count_value_or_default().into(),
                        IndexAxis::Sum | IndexAxis::BigSum => element.big_sum_value_or_default(),
                        // The average is worked out from the two.
                        IndexAxis::Avg => return false,
                    };
                    ranked.get(*key).is_some_and(|ranked| *ranked != expected)
                })
                .count() as u64;
            let entries = Read::PathQuery {
                query: Box::new(PathQuery::new_axis(tree.clone(), page)),
                proven: false,
            };
            match committed(db, entries, grove_version) {
                Ok(Outcome::Run(PathQueryRun::AxisEntries { entries, .. })) => {
                    let read: Vec<Vec<u8>> = match entries {
                        AxisEntries::Count(e) => e.into_iter().map(|e| e.primary_key).collect(),
                        AxisEntries::Sum(e) => e.into_iter().map(|e| e.primary_key).collect(),
                        AxisEntries::Avg(e) => e.into_iter().map(|e| e.primary_key).collect(),
                        AxisEntries::BigSum(e) => e.into_iter().map(|e| e.primary_key).collect(),
                    };
                    drift.short += held
                        .keys()
                        .filter(|key| ranked.contains_key(*key) && !read.contains(key))
                        .count() as u64;
                }
                _ => drift.unreadable += 1,
            }
        }
    }
    drift
}

/// What the streams of some seeds showed: how many streams there were, and
/// how many of them showed each kind of difference.
#[derive(Debug, Default, PartialEq, Eq)]
struct Shown {
    streams: u64,
    drift: Drift,
}

/// Applies the writes of `wide-ranks`, generated for batches that write
/// entries, to a database of `grove_version`, with the repairs of the
/// stream or without them, and compares the rankings after every commit.
fn hunt(
    seeds: std::ops::RangeInclusive<u64>,
    grove_version: &GroveVersion,
    repairs: bool,
) -> Shown {
    let features = Features {
        indexed_batches_in_step: true,
        ..Features::for_grove_version(grove_version)
    };
    let mut shown = Shown::default();
    for seed in seeds {
        for (rounds, entries) in [(5, 4), (8, 6), (12, 9)] {
            let workload = WideRanks {
                rounds,
                entries,
                features,
            };
            let ops: Vec<Op> = workload
                .stream(seed)
                .filter(|op| match op {
                    Op::Read(_) => false,
                    Op::Write(Write::ReconcileIndexed { .. }) => repairs,
                    _ => true,
                })
                .collect();
            assert!(
                indexed_trees(&ops).len() >= 2,
                "the two trees of the workload"
            );
            let dir = tempfile::tempdir().expect("temp dir");
            let db = GroveDb::open(dir.path()).expect("open");
            let mut transactions = Transactions::new(&db);
            let mut of_stream = Drift::default();
            for (index, op) in ops.iter().enumerate() {
                transactions
                    .apply(op, grove_version)
                    .unwrap()
                    .unwrap_or_else(|error| panic!("seed {seed}: op {index} {op:?}: {error:?}"));
                if matches!(op, Op::Commit) {
                    // The trees created so far: all of them are committed.
                    let trees = indexed_trees(&ops[..=index]);
                    of_stream.add(&drift(&db, &trees, grove_version));
                }
            }
            shown.streams += 1;
            shown.drift.add(&of_stream.shown());
        }
    }
    shown
}

fn grove_version(number: u32) -> &'static GroveVersion {
    GROVE_VERSIONS
        .iter()
        .find(|grove_version| grove_version.protocol_version == number)
        .expect("a grove version of that number")
}

/// On grove version 5 batches leave every ranking in step with its tree.
/// On grove version 4 they leave rows of removed entries behind, with the
/// repairs of the stream and without them, and nothing else: no ranking
/// lacks a row, ranks a key at another value or returns fewer entries than
/// keys.
#[test]
fn batches_leave_rows_behind_on_grove_version_4_and_no_row_out() {
    for repairs in [false, true] {
        let latest = hunt(1..=4, grove_version(5), repairs);
        assert_eq!(
            latest,
            Shown {
                streams: 12,
                ..Shown::default()
            },
            "grove version 5, repairs: {repairs}"
        );
        let fourth = hunt(1..=4, grove_version(4), repairs);
        println!("grove version 4, repairs: {repairs}: {fourth:?}");
        let Drift {
            left_behind,
            missing,
            stale,
            short,
            unreadable,
        } = fourth.drift;
        assert!(left_behind >= 6 && unreadable > 0, "{fourth:?}");
        assert_eq!((missing, stale, short), (0, 0, 0), "{fourth:?}");
    }
}

/// The same over many seeds: `DRIFT_SEEDS` of them, 400 unless it is set,
/// each at three sizes, which takes half an hour.
#[test]
#[ignore = "half an hour"]
fn no_ranking_lacks_a_row_over_many_seeds() {
    let seeds: u64 = std::env::var("DRIFT_SEEDS")
        .ok()
        .map_or(400, |seeds| seeds.parse().expect("a number of seeds"));
    for repairs in [false, true] {
        let shown = hunt(1..=seeds, grove_version(4), repairs);
        println!("grove version 4, repairs: {repairs}: {shown:?}");
        let Drift {
            missing,
            stale,
            short,
            ..
        } = shown.drift;
        assert_eq!((missing, stale, short), (0, 0, 0));
    }
}
