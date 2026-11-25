//! The peak memory of one operation, measured on either engine.
//!
//! GroveDB bounds a key at 255 bytes and bounds neither the size of a
//! value, nor how deep trees are in trees, nor how many operations a batch
//! holds: those are the application's to bound. The test here makes one
//! such operation at a size that the environment names, in a process of
//! its own, and prints how much the resident memory of the process grew
//! while the operation ran:
//!
//! ```text
//! LIGNIN_MEMORY_ENGINE=lignin LIGNIN_MEMORY_CASE=value:16777216 \
//!   cargo test -p grovedb --features lignin_storage --lib -- \
//!   --ignored --exact --nocapture \
//!   engine::lignin::tests::measured::the_peak_memory_of_one_operation
//! ```
//!
//! The cases: `key` (a key and every segment of its path at 255 bytes),
//! `value:<bytes>` (an item of that size put, read, replaced and deleted),
//! `depth:<trees>` (an item put under that many trees in trees),
//! `batch:<operations>` (one batch of that many items), and
//! `beside:<bytes>:<count>` (single small items put between `count` items
//! of `bytes` each, which are of one Merk).
//!
//! The last is the worst case of LigninDB made on purpose. A value of more
//! than a part of a page is stored out of line, and when a page of the tree
//! splits, the cells that move take their values with them: each is read
//! whole and held until the commit (`lignin-tree`, the move rule of
//! FMT-07). So one small write beside large values can hold many of them.
//!
//! What is measured is the growth of the resident set from just before the
//! operation to its peak during it, as the kernel counts it (`VmHWM` of
//! `/proc/self/status`, reset through `/proc/self/clear_refs`). It counts
//! the memory of both engines alike, RocksDB's own allocations among it.
//!
//! **Run it with `MALLOC_MMAP_THRESHOLD_=16384`.** The allocator of glibc
//! then takes every block of 16 KiB or more from the system and gives it
//! back when it is freed, so the resident set follows the large blocks
//! that are held. Without it the allocator keeps what an earlier operation
//! freed, a later operation is given that memory again, and its growth
//! reads as nothing: the cases with many operations in one process then
//! show the first of them only. Run under a memory limit, a case that
//! outgrows the limit is killed and prints nothing.
//!
//! `LIGNIN_MEMORY_DIR` names the directory of the database, which is then
//! left where it is: with it, a case that a memory limit had killed leaves
//! its database to `the_database_that_a_killed_operation_left`, which
//! opens it again, checks it, and counts what it holds.

use std::{collections::BTreeSet, fs, path::PathBuf};

use grovedb_version::version::GroveVersion;

use super::{
    nested::{check, open},
    LigninEngine,
};
use crate::{batch::QualifiedGroveDbOp, Element, Engine, GroveDb};

/// An engine with what it counts of its log, where it counts it.
trait Counted: Engine {
    /// The bytes of the groups that the commits of the session appended to
    /// the log so far, on an engine that counts them.
    fn logged(db: &GroveDb<Self>) -> Option<u64>;

    /// Every count the engine keeps of its session, as it shows them, on
    /// an engine that keeps such counts: where the bytes that the process
    /// wrote went (the log, write-back, the cleaner).
    fn counts(db: &GroveDb<Self>) -> Option<String>;
}

impl Counted for LigninEngine {
    fn logged(db: &GroveDb<Self>) -> Option<u64> {
        Some(db.engine().database().stats().commits.group_bytes)
    }

    fn counts(db: &GroveDb<Self>) -> Option<String> {
        let stats = db.engine().database().stats();
        Some(format!("{stats}\n{:?}\n{:?}", stats.store, stats.cleaner))
    }
}

#[cfg(feature = "rocksdb_storage")]
impl Counted for crate::RocksDbEngine {
    fn logged(_: &GroveDb<Self>) -> Option<u64> {
        None
    }

    fn counts(_: &GroveDb<Self>) -> Option<String> {
        None
    }
}

/// The bytes that the process has given to the system to write so far,
/// to any file and from any thread (`wchar` of `/proc/self/io`): the log
/// of either engine among them, and what a thread of the engine wrote in
/// the background meanwhile.
fn written() -> u64 {
    let io = fs::read_to_string("/proc/self/io").expect("the counts of the process");
    let line = io.lines().find(|line| line.starts_with("wchar:"));
    let bytes = line.and_then(|line| line.split_whitespace().nth(1));
    bytes.and_then(|bytes| bytes.parse().ok()).expect("a count")
}

/// The resident set of the process now, and its peak since the last
/// [`forget_the_peak`], in kibibytes.
fn resident() -> (u64, u64) {
    let status = fs::read_to_string("/proc/self/status").expect("the status of the process");
    let field = |name: &str| -> u64 {
        let line = status.lines().find(|line| line.starts_with(name));
        let value = line.and_then(|line| line.split_whitespace().nth(1));
        value.and_then(|kb| kb.parse().ok()).expect("a size")
    };
    (field("VmRSS:"), field("VmHWM:"))
}

/// Has the kernel count the peak of the resident set from now.
fn forget_the_peak() {
    fs::write("/proc/self/clear_refs", "5").expect("the peak is reset");
}

/// Runs `operation` and prints how far the resident set grew while it ran.
fn measured<R>(
    case: &str,
    what: &str,
    input_bytes: usize,
    operation: impl FnOnce() -> R,
) -> (R, u64) {
    forget_the_peak();
    let (before, _) = resident();
    let returned = operation();
    let (after, peak) = resident();
    let growth = peak.saturating_sub(before);
    println!(
        "MEMORY case={case} operation={what} input_bytes={input_bytes} before_kb={before} \
         growth_kb={growth} after_kb={after}"
    );
    (returned, growth)
}

fn item(len: usize, fill: u8) -> Element {
    Element::new_item(vec![fill; len])
}

fn put<E: Engine>(db: &GroveDb<E>, at: &[&[u8]], key: &[u8], element: Element, v: &GroveVersion) {
    db.insert(at, key, element, None, None, v)
        .unwrap()
        .expect("an insert");
}

/// A key and every segment of its path at the 255 bytes GroveDB allows.
fn key<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) {
    let segments: Vec<Vec<u8>> = (0..8u8).map(|n| vec![n; 255]).collect();
    let mut at: Vec<&[u8]> = Vec::new();
    for segment in &segments {
        put(db, &at, segment, Element::empty_tree(), v);
        at.push(segment);
    }
    let long = [0xa5; 255];
    let bytes = 255 * 9;
    measured("key", "put", bytes, || put(db, &at, &long, item(1, 1), v));
    let read = measured("key", "get", bytes, || {
        db.get(at.as_slice(), &long, None, v).unwrap()
    });
    read.0.expect("the item");
}

/// An item of `len` bytes: put, read, replaced by another of its size, and
/// deleted.
fn value<E: Engine>(db: &GroveDb<E>, len: usize, v: &GroveVersion) {
    let case = format!("value:{len}");
    put(db, &[], b"tree", Element::empty_tree(), v);
    let at: [&[u8]; 1] = [b"tree"];
    for n in 0..16u8 {
        put(db, &at, &[b'k', n], item(40, n), v);
    }
    let large = item(len, 1);
    measured(&case, "put", len, || put(db, &at, b"large", large, v));
    let read = measured(&case, "get", len, || {
        db.get(&at, b"large", None, v).unwrap()
    });
    drop(read.0.expect("the item"));
    let other = item(len, 2);
    measured(&case, "replace", len, || put(db, &at, b"large", other, v));
    let deleted = measured(&case, "delete", len, || {
        db.delete(&at, b"large", None, None, v).unwrap()
    });
    deleted.0.expect("a delete");
}

/// An item put under `trees` trees in trees.
fn depth<E: Engine>(db: &GroveDb<E>, trees: usize, v: &GroveVersion) {
    let case = format!("depth:{trees}");
    let segments: Vec<Vec<u8>> = (0..trees).map(|n| format!("t{n}").into_bytes()).collect();
    let mut at: Vec<&[u8]> = Vec::new();
    for segment in &segments {
        put(db, &at, segment, Element::empty_tree(), v);
        at.push(segment);
    }
    let bytes = segments.iter().map(Vec::len).sum::<usize>() + 40;
    measured(&case, "put", bytes, || {
        put(db, &at, b"item", item(40, 1), v)
    });
    let read = measured(&case, "get", bytes, || {
        db.get(at.as_slice(), b"item", None, v).unwrap()
    });
    read.0.expect("the item");
}

/// One batch of `operations` items of 100 bytes under keys of 32, in one
/// tree.
fn batch<E: Engine>(db: &GroveDb<E>, operations: usize, v: &GroveVersion) {
    let case = format!("batch:{operations}");
    put(db, &[], b"tree", Element::empty_tree(), v);
    let ops: Vec<QualifiedGroveDbOp> = (0..operations as u64)
        .map(|n| {
            let mut key = n.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes().to_vec();
            key.resize(32, 7);
            QualifiedGroveDbOp::insert_or_replace_op(vec![b"tree".to_vec()], key, item(100, 3))
        })
        .collect();
    let applied = measured(&case, "apply", operations * 132, || {
        db.apply_batch(ops, None, None, v).unwrap()
    });
    applied.0.expect("the batch");
}

/// One small put beside large items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Put {
    /// How far the resident set grew while it ran, in kibibytes.
    growth_kb: u64,
    /// The bytes its commit appended to the log, where the engine counts
    /// them.
    logged: u64,
    /// The bytes the process gave to the system to write while it ran.
    written: u64,
    /// The resident set before it, in kibibytes.
    before_kb: u64,
    /// Which of the puts it was.
    number: usize,
}

/// `count` items of `len` bytes in one Merk, and then 256 items of 8
/// bytes put one by one between them: the growth of each of those puts,
/// the largest first, with what the put of the largest growth appended to
/// the log, and what the puts append and write at their median and
/// together.
fn beside<E: Counted>(db: &GroveDb<E>, len: usize, count: usize, v: &GroveVersion) {
    let case = format!("beside:{len}:{count}");
    put(db, &[], b"tree", Element::empty_tree(), v);
    let at: [&[u8]; 1] = [b"tree"];
    for n in 0..count {
        put(
            db,
            &at,
            &(n as u32 * 16).to_be_bytes(),
            item(len, n as u8),
            v,
        );
    }
    for line in E::counts(db).unwrap_or_default().lines() {
        println!("COUNTS before the small puts: {line}");
    }
    let mut puts = Vec::new();
    for number in 0..256usize {
        // Between the large items, at another place each time.
        let key = ((number * 7919 % (count * 16)) as u32 | 1).to_be_bytes();
        let small = item(8, 9);
        println!("PUT case={case} number={number} begins");
        forget_the_peak();
        let (before_kb, _) = resident();
        let (logged, wrote) = (E::logged(db), written());
        put(db, &at, &key, small, v);
        puts.push(Put {
            growth_kb: resident().1.saturating_sub(before_kb),
            logged: (E::logged(db).zip(logged)).map_or(0, |(now, then)| now - then),
            written: written() - wrote,
            before_kb,
            number,
        });
    }
    for line in E::counts(db).unwrap_or_default().lines() {
        println!("COUNTS after the small puts: {line}");
    }
    puts.sort_unstable_by(|one, other| other.cmp(one));
    let large_kb = (len / 1024) as u64;
    let over = (puts.iter())
        .filter(|put| put.growth_kb >= large_kb.max(1024))
        .count();
    let median = |of: fn(&Put) -> u64| {
        let mut all: Vec<u64> = puts.iter().map(of).collect();
        all.sort_unstable();
        all[all.len() / 2]
    };
    let most = puts[0];
    println!(
        "MEMORY case={case} operation=small-put input_bytes=12 growth_kb={} at_put={} \
         before_kb={} logged_bytes={} written_bytes={} \
         puts_that_grew_by_a_large_item_or_more={over} large_item_kb={large_kb} \
         the_eight_largest_kb={:?} most_logged_bytes={} most_written_bytes={} \
         median_growth_kb={} median_logged_bytes={} median_written_bytes={} \
         all_logged_bytes={} all_written_bytes={}",
        most.growth_kb,
        most.number,
        most.before_kb,
        most.logged,
        most.written,
        (puts.iter().take(8))
            .map(|put| put.growth_kb)
            .collect::<Vec<_>>(),
        puts.iter().map(|put| put.logged).max().unwrap_or(0),
        puts.iter().map(|put| put.written).max().unwrap_or(0),
        median(|put| put.growth_kb),
        median(|put| put.logged),
        median(|put| put.written),
        puts.iter().map(|put| put.logged).sum::<u64>(),
        puts.iter().map(|put| put.written).sum::<u64>(),
    );
}

fn run<E: Counted>(case: &str) {
    let v = GroveVersion::latest();
    // A directory of the run's own, which goes with it, or the one that
    // is named, which stays.
    let own = tempfile::tempdir().expect("a directory");
    let dir = std::env::var_os("LIGNIN_MEMORY_DIR").map_or(own.path().to_path_buf(), PathBuf::from);
    let db = open::<E>(&dir);
    let number = |text: &str| -> usize { text.parse().expect("a number") };
    match case.split(':').collect::<Vec<_>>().as_slice() {
        ["key"] => key(&db, v),
        ["value", len] => value(&db, number(len), v),
        ["depth", trees] => depth(&db, number(trees), v),
        ["batch", operations] => batch(&db, number(operations), v),
        ["beside", len, count] => beside(&db, number(len), number(count), v),
        _ => panic!("no case {case}"),
    }
}

/// What a process left that was killed while it made a small put of the
/// case `beside` on LigninDB, in the directory `LIGNIN_MEMORY_DIR`: the
/// physical checker finds the store sound, the database opens, GroveDB's
/// own verification finds nothing, every large item is read whole, and
/// the small items that are there are counted. `LIGNIN_MEMORY_CASE` is the
/// case the killed process ran.
#[test]
#[ignore = "a check of what a killed measurement left"]
fn the_database_that_a_killed_operation_left() {
    let v = GroveVersion::latest();
    let dir = PathBuf::from(std::env::var_os("LIGNIN_MEMORY_DIR").expect("LIGNIN_MEMORY_DIR"));
    let case = std::env::var("LIGNIN_MEMORY_CASE").expect("LIGNIN_MEMORY_CASE");
    let number = |text: &str| -> usize { text.parse().expect("a number") };
    let (len, count) = match case.split(':').collect::<Vec<_>>().as_slice() {
        ["beside", len, count] => (number(len), number(count)),
        _ => panic!("no case {case}"),
    };
    check(&dir, v);
    let db = open::<LigninEngine>(&dir);
    let issues = db
        .verify_grovedb(None, true, false, v)
        .expect("a verification");
    assert!(issues.is_empty(), "{} issues", issues.len());
    let at: [&[u8]; 1] = [b"tree"];
    for n in 0..count {
        let key = (n as u32 * 16).to_be_bytes();
        let read = db.get(&at, &key, None, v).unwrap().expect("a large item");
        assert_eq!(read, item(len, n as u8), "the large item {n}");
    }
    // The small puts are made in order, each a commit: the small items
    // that are there are those of the first puts, with none left out and
    // none of a later put. Two puts can be of one key, so the items can
    // be those of more than one count of puts.
    let key_of = |number: usize| ((number * 7919 % (count * 16)) as u32 | 1).to_be_bytes();
    let there: BTreeSet<[u8; 4]> = (0..256)
        .map(key_of)
        .filter(|key| db.get(&at, key, None, v).unwrap().is_ok())
        .collect();
    let made: Vec<usize> = (0..=256)
        .filter(|made| (0..*made).map(key_of).collect::<BTreeSet<_>>() == there)
        .collect();
    println!(
        "LEFT case={case} sound=true issues=0 large_items={count} small_items={} \
         small_puts_made={:?}..={:?}",
        there.len(),
        made.first(),
        made.last()
    );
    assert!(
        !made.is_empty(),
        "the small items are not those of the first puts"
    );
}

/// One operation of the case that `LIGNIN_MEMORY_CASE` names, on the
/// engine that `LIGNIN_MEMORY_ENGINE` names (`lignin` or `rocksdb`).
#[test]
#[ignore = "a measurement: one case in a process of its own"]
fn the_peak_memory_of_one_operation() {
    let case = std::env::var("LIGNIN_MEMORY_CASE").expect("LIGNIN_MEMORY_CASE");
    let engine = std::env::var("LIGNIN_MEMORY_ENGINE").expect("LIGNIN_MEMORY_ENGINE");
    println!("MEMORY engine={engine}");
    match engine.as_str() {
        "lignin" => run::<LigninEngine>(&case),
        #[cfg(feature = "rocksdb_storage")]
        "rocksdb" => run::<crate::RocksDbEngine>(&case),
        _ => panic!("no engine {engine}"),
    }
}
