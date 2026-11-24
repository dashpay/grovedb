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

use std::fs;

use grovedb_version::version::GroveVersion;

use super::{nested::open, LigninEngine};
use crate::{batch::QualifiedGroveDbOp, Element, Engine, GroveDb};

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

/// `count` items of `len` bytes in one Merk, and then 256 items of 8
/// bytes put one by one between them: the growth of each of those puts,
/// the largest first.
fn beside<E: Engine>(db: &GroveDb<E>, len: usize, count: usize, v: &GroveVersion) {
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
    let mut growths = Vec::new();
    for n in 0..256usize {
        // Between the large items, at another place each time.
        let key = ((n * 7919 % (count * 16)) as u32 | 1).to_be_bytes();
        let small = item(8, 9);
        forget_the_peak();
        let (before, _) = resident();
        put(db, &at, &key, small, v);
        growths.push((resident().1.saturating_sub(before), n));
    }
    growths.sort_unstable_by(|one, other| other.cmp(one));
    let large_kb = (len / 1024) as u64;
    let over = growths
        .iter()
        .filter(|(kb, _)| *kb >= large_kb.max(1024))
        .count();
    println!(
        "MEMORY case={case} operation=small-put input_bytes=12 growth_kb={} at_put={} \
         puts_that_grew_by_a_large_item_or_more={over} large_item_kb={large_kb} \
         the_eight_largest_kb={:?}",
        growths[0].0,
        growths[0].1,
        growths
            .iter()
            .take(8)
            .map(|(kb, _)| *kb)
            .collect::<Vec<_>>()
    );
}

fn run<E: Engine>(case: &str) {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<E>(dir.path());
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
