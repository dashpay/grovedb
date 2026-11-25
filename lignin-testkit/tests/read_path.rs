//! Measurements of the read path on LigninDB through GroveDB's own calls
//! (PERF-05). Every test here is a measurement that a normal run leaves
//! out:
//!
//! ```text
//! TMPDIR=<a directory on a disk> cargo test --release -p lignin-testkit --test read_path -- \
//!     --ignored --nocapture --test-threads 1
//! ```
//!
//! | Setting | Default | Is |
//! | --- | --- | --- |
//! | `RP_STORE` | none | the `lignindb` directory that a `lignin-bench run --keep` of the reference workload left; a copy of it is read. With none, the measurements over the reference database are left out |
//! | `RP_SEED`, `RP_OWNERS` | 1, 10000 | the seed and the owners of that run |
//! | `RP_TREES` | 20000 | the trees of each grove of mostly empty trees |
//! | `RP_EVERY` | 1000 | one tree in so many of such a grove holds a record |
//! | `RP_GAPS` | 1000,10000,100000 | the numbers of emptied trees between two trees that hold a record, for the time of a step past them |
//!
//! # A step from no key
//!
//! A descending range that ends past the last key of its subtree seeks
//! to its end, where the cursor stands on no key, and steps back
//! (`QueryItem::RangeTo`). The step asks the engine whether a record of
//! another subtree lies beside this one in the column, which it answers
//! from the column's flat keyspace and, in the data column, from the
//! subtree directory: a walk to the nearest subtree with a stored record
//! (`docs/lignindb/node-store.md`, "Flat records"). The same range that
//! includes its end (`QueryItem::RangeToInclusive`) seeks to the last key
//! at or before the end and asks nothing. What is measured is the
//! difference of the two whole reads, in pages the pool is asked for and
//! in time. The two differ in more than the question: the first seeks to
//! its end and steps, the second makes one seek, and where the first
//! returns no row it does not read one. Where a record of another subtree
//! lies behind this one, both read the last row. Where none does, the
//! first reads no row, as it does on RocksDB, whose cursor then stands
//! nowhere and does not step (`docs/lignindb/flat-store.md`, "A step from
//! no key"): the measurement says how many of the subtrees it read gave
//! their row to the step. A read is timed around the test kit's
//! `workload::apply` of the read, which makes GroveDB's call.

#![cfg(grovedb_engine_seam)]

use std::{path::Path, time::Instant};

use grovedb::{
    engine::lignin::LigninEngine, Element, GroveDb, PathQuery, Query, QueryItem, RocksDbEngine,
    SizedQuery,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;
use lignin_testkit::workload::{apply, drive::Documents, Op, Read, StreamEngine, Write};

type Grove = GroveDb<LigninEngine>;

fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} is {value:?}, which is no value of its kind")),
        Err(_) => default,
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory");
    for entry in std::fs::read_dir(from).expect("a directory") {
        let entry = entry.expect("an entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a kind").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a copy");
        }
    }
}

/// The pages the pool was asked for since the database was opened: those
/// it held and those it read.
fn pages(db: &Grove) -> u64 {
    let store = db.engine().database().stats().store;
    store.cache_hits + store.page_reads
}

/// The last row of the subtree at `path`, by a descending range to a key
/// past every key: one that leaves its end out, which steps back from no
/// key, or one that includes it.
fn last_row(path: &[Vec<u8>], inclusive: bool) -> PathQuery {
    let end = vec![0xff; 48];
    let item = match inclusive {
        true => QueryItem::RangeToInclusive(..=end),
        false => QueryItem::RangeTo(..end),
    };
    let query = Query::new_single_query_item_with_direction(item, false);
    PathQuery::new(path.to_vec(), SizedQuery::new(query, Some(1), None))
}

/// What a read of the last row of one subtree asked of the pool and was
/// charged in seeks, by the range that steps from no key and by the one
/// that does not.
#[derive(Debug, Clone, Copy)]
struct Step {
    stepping: (u64, u64),
    seeking: (u64, u64),
    /// Whether the range that steps from no key read the last row.
    read_the_row: bool,
}

impl Step {
    /// The pages the read that steps asked for beyond those of the read
    /// that does not.
    fn pages(&self) -> i64 {
        self.stepping.0 as i64 - self.seeking.0 as i64
    }
}

fn step(db: &Grove, path: &[Vec<u8>], version: &GroveVersion) -> Step {
    let read = |inclusive: bool| {
        let op = Op::Read(Read::Rows {
            query: Box::new(last_row(path, inclusive)),
            raw: true,
        });
        // Once unmeasured, so that both reads find their pages in the pool.
        apply(db, &op, None, version).value.expect("a read");
        let before = pages(db);
        let run = apply(db, &op, None, version);
        let asked = pages(db) - before;
        let rows = format!("{:?}", run.value.expect("a read"));
        ((asked, u64::from(run.cost.seek_count)), rows)
    };
    let (stepping, rows) = read(false);
    let (seeking, last) = read(true);
    assert!(last.contains("PathKeyElementTrioResultItem"), "{last}");
    let read_the_row = rows == last;
    assert!(
        read_the_row || !rows.contains("PathKeyElementTrioResultItem"),
        "the step read another row: {rows}"
    );
    Step {
        stepping,
        seeking,
        read_the_row,
    }
}

fn report(what: &str, steps: &[Step]) {
    let pages: Vec<i64> = steps.iter().map(Step::pages).collect();
    let sum = |of: fn(&Step) -> u64| steps.iter().map(of).sum::<u64>() as f64 / steps.len() as f64;
    println!(
        "{what}: {} subtrees, {} of them gave the step their last row; pages the stepping read \
         asks for beyond the other read: mean {:.2}, least {}, most {}; a read asks the pool \
         for {:.1} pages with the step and {:.1} without; seeks charged {:.2} and {:.2}",
        steps.len(),
        steps.iter().filter(|step| step.read_the_row).count(),
        pages.iter().sum::<i64>() as f64 / pages.len() as f64,
        pages.iter().min().expect("a subtree"),
        pages.iter().max().expect("a subtree"),
        sum(|step| step.stepping.0),
        sum(|step| step.seeking.0),
        sum(|step| step.stepping.1),
        sum(|step| step.seeking.1),
    );
}

/// A grove with `trees` trees under one tree, of which one in `every`
/// holds an item. With `emptied`, each of the others held an item that
/// was deleted, so that the subtree directory has an entry for it with
/// no root; without, nothing was ever written into them.
fn mostly_empty(
    dir: &Path,
    trees: u32,
    every: u32,
    emptied: bool,
    version: &GroveVersion,
) -> Grove {
    let db: Grove = GroveDb::open_with_engine(dir).expect("a new database");
    let write = |ops: &[Op]| {
        let transaction = db.start_transaction();
        for op in ops {
            let applied = apply(&db, op, Some(&transaction), version);
            applied.value.expect("a write");
        }
        let committed = db.commit_transaction(transaction);
        committed.value.expect("a commit");
    };
    let insert =
        |path: Vec<Vec<u8>>, key: Vec<u8>, element| Op::Write(Write::Insert { path, key, element });
    let top = b"trees".to_vec();
    write(&[insert(vec![], top.clone(), Element::empty_tree())]);
    let name = |number: u32| number.to_be_bytes().to_vec();
    for chunk in (0..trees).collect::<Vec<_>>().chunks(500) {
        let mut ops = Vec::new();
        for number in chunk {
            ops.push(insert(
                vec![top.clone()],
                name(*number),
                Element::empty_tree(),
            ));
            let inside = vec![top.clone(), name(*number)];
            let item = Element::new_item(vec![7; 16]);
            if number % every == 0 || emptied {
                ops.push(insert(inside.clone(), b"item".to_vec(), item));
            }
            if number % every != 0 && emptied {
                ops.push(Op::Write(Write::Delete {
                    path: inside,
                    key: b"item".to_vec(),
                }));
            }
        }
        write(&ops);
    }
    db
}

#[test]
#[ignore = "a measurement: minutes, a release build and a disk"]
fn the_pages_that_a_step_from_no_key_asks_for() {
    let version = GroveVersion::latest();
    let scratch = tempfile::tempdir().expect("a directory");

    if let Some(store) = std::env::var_os("RP_STORE") {
        let (seed, owners): (u64, u64) = (setting("RP_SEED", 1), setting("RP_OWNERS", 10_000));
        let documents = Documents {
            owners,
            ..Documents::reference()
        };
        let dir = scratch.path().join("reference");
        copy_dir(Path::new(&store), &dir);
        let db: Grove = GroveDb::open_with_engine(&dir).expect("the kept database opens");
        let primary = step(&db, &documents.primary_path(seed), version);
        report("reference, the documents' tree", &[primary]);
        let indexes: Vec<Step> = (0..owners.min(500))
            .map(|owner| step(&db, &documents.owner_references_path(seed, owner), version))
            .collect();
        report("reference, the references of an owner", &indexes);
    } else {
        println!("RP_STORE names no kept database: the reference database is left out");
    }

    let (trees, every): (u32, u32) = (setting("RP_TREES", 20_000), setting("RP_EVERY", 1_000));
    assert!(every > 0 && trees > 0, "RP_TREES and RP_EVERY are not zero");
    for emptied in [false, true] {
        let dir = scratch.path().join(format!("empty-{emptied}"));
        let db = mostly_empty(&dir, trees, every, emptied, version);
        let top = b"trees".to_vec();
        let steps: Vec<Step> = (0..trees)
            .step_by(every as usize)
            .map(|number| step(&db, &[top.clone(), number.to_be_bytes().to_vec()], version))
            .collect();
        let what = match emptied {
            false => "trees that nothing was written into",
            true => "trees that were emptied",
        };
        report(
            &format!("{trees} trees, one in {every} with a record, the others {what}"),
            &steps,
        );
        report(
            "the same grove, the tree that holds the trees",
            &[step(&db, &[top], version)],
        );
    }
}

/// The prefix of the subtree at `path`, by which its entry is ordered in
/// the subtree directory and its records in a column of RocksDB.
fn prefix(path: &[Vec<u8>]) -> [u8; 32] {
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    grovedb_storage::prefix::build_prefix(SubtreePath::from(segments.as_slice())).value
}

/// The names of `gap + 2` trees under the tree `top` whose prefixes follow
/// one another with the prefix of no other subtree of the grove between
/// them: the first, the `gap` between, and the last.
fn in_a_row(top: &[u8], gap: usize) -> Vec<Vec<u8>> {
    let name = |number: u32| number.to_be_bytes().to_vec();
    let mut named: Vec<([u8; 32], Vec<u8>)> = (0..(2 * gap + 64) as u32)
        .map(|number| (prefix(&[top.to_vec(), name(number)]), name(number)))
        .collect();
    named.sort();
    // The subtrees every such grove has beside these: its root, the tree
    // that holds the trees, and the one the engine keeps its records of
    // pending drops under.
    let others = [
        [0; 32],
        prefix(&[top.to_vec()]),
        *grovedb_storage::flat::pending_prefix_drops_namespace(),
    ];
    let clear = |from: usize| {
        let (first, last) = (named[from].0, named[from + gap + 1].0);
        others.iter().all(|other| *other < first || *other > last)
    };
    let from = (0..named.len() - gap - 1)
        .find(|from| clear(*from))
        .expect("a row of prefixes with no other subtree in it");
    named[from..from + gap + 2]
        .iter()
        .map(|(_, name)| name.clone())
        .collect()
}

/// A grove whose tree `trees` holds the trees of `names`: the first and
/// the last hold an item, and each of the others held one that was
/// deleted.
fn emptied_between<E: StreamEngine>(
    dir: &Path,
    names: &[Vec<u8>],
    version: &GroveVersion,
) -> GroveDb<E> {
    let db: GroveDb<E> = GroveDb::open_with_engine(dir).expect("a new database");
    let write = |ops: &[Op]| {
        let transaction = db.start_transaction();
        for op in ops {
            let applied = apply(&db, op, Some(&transaction), version);
            applied.value.expect("a write");
        }
        let committed = db.commit_transaction(transaction);
        committed.value.expect("a commit");
    };
    let insert =
        |path: Vec<Vec<u8>>, key: Vec<u8>, element| Op::Write(Write::Insert { path, key, element });
    let top = b"trees".to_vec();
    write(&[insert(vec![], top.clone(), Element::empty_tree())]);
    let (first, last) = (&names[0], &names[names.len() - 1]);
    for chunk in names.chunks(500) {
        let mut ops = Vec::new();
        for name in chunk {
            ops.push(insert(
                vec![top.clone()],
                name.clone(),
                Element::empty_tree(),
            ));
            let inside = vec![top.clone(), name.clone()];
            let item = Element::new_item(vec![7; 16]);
            ops.push(insert(inside.clone(), b"item".to_vec(), item));
            if name != first && name != last {
                ops.push(Op::Write(Write::Delete {
                    path: inside,
                    key: b"item".to_vec(),
                }));
            }
        }
        write(&ops);
    }
    db
}

/// The median time of `repeats` reads of the last row of the subtree at
/// `path`, in microseconds, timed around the test kit's `apply` of the
/// read, and what each of them returned, as it prints.
fn timed<E: StreamEngine>(
    db: &GroveDb<E>,
    path: &[Vec<u8>],
    inclusive: bool,
    repeats: usize,
    version: &GroveVersion,
) -> (f64, String) {
    let op = Op::Read(Read::Rows {
        query: Box::new(last_row(path, inclusive)),
        raw: true,
    });
    let read = || {
        let began = Instant::now();
        let outcome = apply(db, &op, None, version);
        let took = began.elapsed().as_secs_f64() * 1e6;
        (took, format!("{:?}", outcome.value.expect("a read")))
    };
    let (_, rows) = read();
    let mut micros: Vec<f64> = (0..repeats)
        .map(|_| {
            let (took, again) = read();
            assert_eq!(again, rows);
            took
        })
        .collect();
    micros.sort_by(f64::total_cmp);
    (micros[micros.len() / 2], rows)
}

/// The cost of one step from no key against the trees it passes: the time
/// of the read that steps and of the read that does not, on each engine,
/// for each number of emptied trees between two that hold a record. The
/// read is of the tree in front of the emptied ones, whose step looks
/// behind it, and then of the tree behind them, which has nothing behind
/// it but what the grove's other subtrees hold.
#[test]
#[ignore = "a measurement: minutes, a release build and a disk"]
fn the_time_of_a_step_from_no_key_past_emptied_trees() {
    let version = GroveVersion::latest();
    let gaps = std::env::var("RP_GAPS").unwrap_or_else(|_| "1000,10000,100000".to_string());
    let gaps: Vec<usize> = gaps
        .split(',')
        .map(str::trim)
        .filter(|gap| !gap.is_empty())
        .map(|gap| gap.parse().expect("RP_GAPS is numbers"))
        .collect();
    assert!(!gaps.is_empty(), "RP_GAPS names a number of emptied trees");
    fn measure<E: StreamEngine>(
        engine: &str,
        gap: usize,
        names: &[Vec<u8>],
        pages: impl Fn(&GroveDb<E>) -> Option<u64>,
        version: &GroveVersion,
    ) {
        let scratch = tempfile::tempdir().expect("a directory");
        let began = Instant::now();
        let db: GroveDb<E> = emptied_between(scratch.path(), names, version);
        let built = began.elapsed().as_secs_f64();
        let top = b"trees".to_vec();
        let repeats = (2_000_000 / gap.max(1)).clamp(15, 500);
        for (which, name) in [("in front", &names[0]), ("behind", &names[names.len() - 1])] {
            let path = [top.clone(), name.clone()];
            let (stepping, rows) = timed(&db, &path, false, repeats, version);
            let (seeking, last_row_read) = timed(&db, &path, true, repeats, version);
            // The read that does not step returns the last row. The one
            // that steps returns that row, or none.
            let row = "PathKeyElementTrioResultItem";
            assert!(last_row_read.contains(row), "{last_row_read}");
            let read_the_row = rows == last_row_read;
            assert!(read_the_row || !rows.contains(row), "another row: {rows}");
            let asked = pages(&db).map(|before| {
                let op = Op::Read(Read::Rows {
                    query: Box::new(last_row(&path, false)),
                    raw: true,
                });
                apply(&db, &op, None, version).value.expect("a read");
                pages(&db).expect("counted") - before
            });
            println!(
                "{engine}: {gap} emptied trees, the tree {which}: the read that steps {stepping:.1} us, \
                 the read that does not {seeking:.1} us, the two differ by {:.1} us; the stepping \
                 read returned {}; pages the pool was asked for by the stepping read: {}; grove \
                 built in {built:.0} s; median of {repeats} reads",
                stepping - seeking,
                if read_the_row { "the last row" } else { "no row" },
                asked.map_or("not counted".to_string(), |asked| asked.to_string()),
            );
        }
    }
    for gap in gaps {
        let names = in_a_row(b"trees", gap);
        measure::<LigninEngine>(
            "lignindb",
            gap,
            &names,
            |db| {
                let store = db.engine().database().stats().store;
                Some(store.cache_hits + store.page_reads)
            },
            version,
        );
        measure::<RocksDbEngine>("rocksdb", gap, &names, |_| None, version);
    }
}
