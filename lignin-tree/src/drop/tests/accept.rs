//! The acceptance test of the task: a subtree of a million elements is
//! dropped by a commit that does not depend on its size, all its cells are
//! removed, a page is freed when it is empty, and the out-of-line values
//! that the cells held are reported.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use lignin_format::PageSize;

use super::*;

/// What a run measured.
#[derive(Debug)]
struct Measured {
    elements: usize,
    nested: usize,
    tree_pages: usize,
    /// The drop: the write of the transaction, and its commit. The write
    /// is the one that was committed, and `unlinks` are the same write made
    /// and rolled back before it, in ascending order.
    unlink: Duration,
    unlinks: Vec<Duration>,
    commit: Duration,
    tree_pages_changed: usize,
    flat_pages_read: usize,
    flat_pages_written: usize,
    log_bytes: usize,
    /// The flat records the Merk had in the column of its own, and the
    /// pages of that column.
    flat_records: usize,
    column_pages: usize,
    /// The reclaiming.
    reclaim: Duration,
    done: Reclaimed,
    steps: usize,
    /// The freeing of the pages that the drop unlinked.
    unlinked: FlatReclaimed,
    freeing: Duration,
}

fn key(n: usize) -> Vec<u8> {
    format!("element-{n:08}").into_bytes()
}

/// Every tenth element has a value that is out of line.
fn value(n: usize) -> Vec<u8> {
    match n % 10 {
        0 => bytes(n as u64, 300),
        _ => bytes(n as u64, 16),
    }
}

fn record(n: usize) -> Vec<u8> {
    format!("record-{n:08}").into_bytes()
}

/// Builds a Merk of `elements` nodes below a host of the root Merk, with a
/// Merk of two nodes nested at every `nest`th of them, drops it, and
/// reclaims it. Each nested Merk has three flat records, in two columns.
/// With `own_flat`, the Merk has a flat record for every tenth element as
/// well, every tenth of them with a value out of line.
fn drop_a_subtree(
    elements: usize,
    nest: usize,
    size: PageSize,
    pool: usize,
    rounds: usize,
    own_flat: bool,
) -> Measured {
    let db = Db::with(size, pool);
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    for n in 0..40 {
        grove.put(0, &key(n * 37 % 40), b"an element of the root Merk");
    }
    let host = grove.put(0, b"the host", b"a tree");
    let merk = grove.host(host);
    grove.flush(&mut txn);
    db.commit(txn);

    // The elements, the middle ones first, so that the tree is as low as a
    // Merk keeps it.
    let mut spans = VecDeque::from([(0, elements)]);
    let mut nested = 0;
    let mut put = 0;
    let mut entered = Vec::new();
    while !spans.is_empty() {
        let mut txn = db.begin();
        for _ in 0..20_000 {
            let Some((low, high)) = spans.pop_front() else {
                break;
            };
            if low == high {
                continue;
            }
            let middle = low + (high - low) / 2;
            let node = grove.put(merk, &key(middle), &value(middle));
            put += 1;
            if middle % nest == 0 {
                let inner = grove.host(node);
                grove.put(inner, b"a", b"nested");
                grove.put(inner, b"b", b"nested");
                entered.push(inner);
                nested += 1;
            }
            spans.push_back((low, middle));
            spans.push_back((middle + 1, high));
        }
        grove.flush(&mut txn);
        // The Merks that the write entered get their records.
        for inner in entered.drain(..) {
            grove.put_flat(inner, 0, b"a", b"of a nested Merk");
            grove.put_flat(inner, 0, b"b", b"of a nested Merk");
            grove.put_flat(inner, 1, b"a", b"of a nested Merk");
        }
        grove.flush(&mut txn);
        db.commit(txn);
        db.write_back();
    }
    assert_eq!(put, elements);
    // A flat page keeps a value up to an eighth of its size.
    let flat_records = match own_flat {
        true => elements / 10,
        false => 0,
    };
    let long = db.config.max_inline() + 100;
    for chunk in 0..flat_records.div_ceil(20_000) {
        let mut txn = db.begin();
        for n in chunk * 20_000..flat_records.min(chunk * 20_000 + 20_000) {
            let held = match n % 10 {
                0 => bytes(n as u64, long),
                _ => bytes(n as u64, 16),
            };
            grove.put_flat(merk, 0, &record(n), &held);
        }
        grove.flush(&mut txn);
        db.commit(txn);
        db.write_back();
    }
    let flat_pointers = flat_records.div_ceil(10);
    let column_pages = scan_flat(&db, &grove.columns[0].tree).pages.len();
    db.trim_log();
    let below = grove.below(merk).len();
    assert_eq!(below, elements + 2 * nested);
    assert_eq!(db.cells(), grove.alive());
    let tree_pages = db.tree_frames().len();
    let pointers = db.tree_pointers().len();
    assert_eq!(pointers, elements.div_ceil(10));

    // The drop, made and given up a few times first: the same write on the
    // same pages, for a time that one slow moment of the machine does not
    // decide.
    let mut unlinks = Vec::new();
    for _ in 0..rounds {
        let saved = grove.clone();
        let mut txn = db.begin();
        grove.drop_merks(&mut txn, &[merk], false);
        unlinks.push(grove.unlinked_in.expect("a drop"));
        db.roll_back(txn);
        grove = saved;
    }
    unlinks.sort();

    let before = db.log.lock().expect("the log").len();
    let mut txn = db.begin();
    txn.count_from_here();
    let left = grove.drop_merks(&mut txn, &[merk], false);
    let unlink = grove.unlinked_in.expect("a drop");
    let tree_pages_changed = txn.tree.changed().len();
    let flat_pages_read = txn.flat.read.borrow().len();
    let flat_pages_written = txn.flat.written().count();
    let began = Instant::now();
    db.commit(txn);
    let commit = began.elapsed();
    let log_bytes = db.log.lock().expect("the log").len() - before;
    assert_eq!(left.len(), 1);
    // Every cell is where it was: the drop took none out.
    assert_eq!(db.cells(), grove.alive() + below);
    grove.check(&db);

    // The reclaiming, once no reader needs what was left.
    db.tick();
    let directory = grove.directory(&db);
    let [dropped] = drops(&db, &directory)[..] else {
        panic!("one Merk was dropped");
    };
    let mut wait = Wait::new(db.latest());
    assert!(wait.poll(&db.pins));
    let mut reclaim = Reclaim::new(dropped);
    let began = Instant::now();
    finish(&db, &directory, &mut reclaim, usize::MAX);
    let reclaim_time = began.elapsed();
    let done = reclaim.reclaimed().clone();
    assert!(reclaim.miscount().is_none());

    // All its cells are removed.
    assert_eq!(done.cells as usize, below);
    assert_eq!(db.cells(), grove.alive());
    let steps = db.reclaims();
    // The pages of its flat records that the drop unlinked are freed, and
    // the values their entries held are released.
    let began = Instant::now();
    let unlinked = free_unlinked(&db, &db.drops());
    let freeing = began.elapsed();
    assert!(unlinked.values as usize <= flat_pointers);
    if flat_records > 10_000 {
        assert!(unlinked.pages_freed as usize > column_pages * 9 / 10);
        assert!(unlinked.values as usize > flat_pointers * 9 / 10);
    }
    // A page is freed when it is empty: no page is left without a cell,
    // and the pages that are left hold the cells in use.
    check_reclaimed(&db, &grove);
    assert_eq!(
        db.tree_frames().len() as u64,
        tree_pages as u64 - done.pages_freed
    );
    // The out-of-line values the cells held are reported: every one of
    // them, in the log and to the counts of live values.
    assert_eq!(done.values as usize, pointers);
    assert_eq!(done.value_bytes, 300 * pointers as u64);
    let listed: usize = steps.iter().map(|(_, _, released, _)| released).sum();
    assert_eq!(listed, pointers);
    assert!(db.values.counts().iter().all(|count| count.values == 0));
    // The nested Merks were found through the directory, and their flat
    // records with them.
    assert_eq!(done.merks.len(), nested);
    assert_eq!(done.records as usize, 3 * nested + 1);
    assert_eq!(done.flat_records as usize, 3 * nested);

    Measured {
        elements,
        nested,
        tree_pages,
        unlink,
        unlinks,
        commit,
        tree_pages_changed,
        flat_pages_read,
        flat_pages_written,
        log_bytes,
        flat_records,
        column_pages,
        reclaim: reclaim_time,
        done,
        steps: steps.len(),
        unlinked,
        freeing,
    }
}

/// The same run at a size that every test run can afford.
#[test]
fn a_subtree_is_dropped_and_reclaimed() {
    let small = drop_a_subtree(300, 50, PageSize::MIN, 4 << 20, 2, true);
    let large = drop_a_subtree(12_000, 50, PageSize::MIN, 16 << 20, 2, true);
    // Forty times the elements, and the drop changes the same tree pages.
    // Of the directory it writes the leaf of each of the four records and
    // the branches above them, and the directory of the larger run is the
    // larger one: it holds forty times the Merks.
    assert!(large.tree_pages > 20 * small.tree_pages);
    assert!(large.tree_pages_changed <= small.tree_pages_changed + 1);
    // And of the flat columns the two ends of the Merk's records, the
    // branches above them and the record of the pages between.
    let (small_flat, large_flat) = (
        (small.flat_pages_read, small.flat_pages_written),
        (large.flat_pages_read, large.flat_pages_written),
    );
    // Each tree is a level or two higher in the larger run, and that is
    // all the difference.
    assert!(
        small_flat.1 <= 6 && large_flat.1 <= 14,
        "{small_flat:?} {large_flat:?}"
    );
    assert!(
        small_flat.0 <= 8 && large_flat.0 <= 20,
        "{small_flat:?} {large_flat:?}"
    );
    // What it logs is of those pages: the leaves it changed, not the
    // subtree.
    let (small_log, large_log) = (small.log_bytes, large.log_bytes);
    assert!(
        small_log < 2_048 && large_log < 16_384,
        "{small_log} {large_log}"
    );
    assert!(large.done.pages_freed > 100);
}

/// The acceptance test. It builds a subtree of a million elements twice,
/// which takes a minute or two and about two gigabytes, so it is run by
/// hand:
///
/// ```text
/// cargo test --release -p lignin-tree --lib -- --ignored --nocapture \
///     a_subtree_of_a_million_elements_is_dropped_within_a_millisecond
/// ```
///
/// The second time the subtree has a hundred thousand flat records of its
/// own as well, which the drop takes out of their column: two leaves, a
/// branch and the record of the pages between. That run is measured and
/// held to a looser bound. Its drop writes six flat pages where the first
/// writes four, and on 32 KiB pages a write to a flat page is most of what
/// a drop costs.
#[test]
#[ignore = "builds a subtree of a million elements"]
fn a_subtree_of_a_million_elements_is_dropped_within_a_millisecond() {
    let elements = std::env::var("DROP_ELEMENTS")
        .ok()
        .and_then(|elements| elements.parse().ok())
        .unwrap_or(1_000_000);
    let one = drop_a_subtree(1, 1_000, PageSize::DEFAULT, 64 << 20, 20, false);
    let run = drop_a_subtree(elements, 1_000, PageSize::DEFAULT, 1 << 30, 20, false);
    let flat = drop_a_subtree(elements, 1_000, PageSize::DEFAULT, 1 << 30, 20, true);
    for (name, run) in [
        ("one element", &one),
        ("the subtree", &run),
        ("the subtree with flat records of its own", &flat),
    ] {
        println!(
            "{name}: {} elements and {} nested Merks in {} tree pages",
            run.elements, run.nested, run.tree_pages
        );
        println!(
            "  with {} flat records of its own in a column of {} pages, and three for each \
             nested Merk",
            run.flat_records, run.column_pages
        );
        println!(
            "  drop: write {:?}, commit {:?}, total {:?}; {} tree pages changed, {} flat \
             pages read and {} written (the directory, the columns, the unlinked pages), \
             {} bytes in the log",
            run.unlink,
            run.commit,
            run.unlink + run.commit,
            run.tree_pages_changed,
            run.flat_pages_read,
            run.flat_pages_written,
            run.log_bytes
        );
        println!(
            "  the write, made {} times and rolled back: fastest {:?}, median {:?}, slowest {:?}",
            run.unlinks.len(),
            run.unlinks[0],
            run.unlinks[run.unlinks.len() / 2],
            run.unlinks[run.unlinks.len() - 1]
        );
        println!(
            "  reclaim: {:?} in {} page steps; {} cells of {} bytes, {} pages freed, {} \
             written again, {} records, {} value pointers of {} bytes",
            run.reclaim,
            run.steps,
            run.done.cells,
            run.done.cell_bytes,
            run.done.pages_freed,
            run.done.pages_written,
            run.done.records,
            run.done.values,
            run.done.value_bytes
        );
        println!(
            "  of the flat records of the nested Merks: {} taken out of {} leaves; the \
             unlinked pages: {:?} for {} pages freed, {} value pointers of {} bytes",
            run.done.flat_records,
            run.done.flat_leaves,
            run.freeing,
            run.unlinked.pages_freed,
            run.unlinked.values,
            run.unlinked.value_bytes
        );
    }
    // The commit is made once. Of the write, the median of its rounds
    // stands for it, so that one slow moment of a shared machine does not
    // fail the test.
    let median = |run: &Measured| run.unlinks[run.unlinks.len() / 2];
    assert!(median(&run) + run.commit < Duration::from_millis(1));
    assert!(median(&flat) + flat.commit < Duration::from_millis(2));
    for run in [&run, &flat] {
        assert!(run.tree_pages_changed <= one.tree_pages_changed + 1);
        assert!(run.flat_pages_read <= 28 && run.flat_pages_written <= 18);
    }
}

/// The share of their bytes that the tree pages of the latest version use.
fn fill(db: &Db) -> (usize, f64) {
    let frames = db.tree_frames();
    let used: usize = frames
        .iter()
        .map(|(_, frame)| tree(frame).used_bytes())
        .sum();
    (
        frames.len(),
        used as f64 / (frames.len() * db.size.get()) as f64,
    )
}

/// What reclaiming leaves behind, for whoever measures the space on disk:
/// it takes cells out and merges nothing (`tree-layout.md` D3), so a page
/// that held cells of a dropped Merk beside others is as much emptier as
/// those cells were.
///
/// Two thousand subtrees of one to three elements, which lie in the pages
/// of their hosts, and every third of them dropped: a page is freed only
/// where every subtree in it went.
#[test]
fn reclaiming_leaves_the_shared_pages_emptier_and_merges_nothing() {
    let db = Db::with(PageSize::MIN, 16 << 20);
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    grove.flush(&mut txn);
    db.commit(txn);
    let mut merks = Vec::new();
    for chunk in 0..40 {
        let mut txn = db.begin();
        for n in chunk * 50..chunk * 50 + 50 {
            let host = grove.put(0, &key(n * 7919 % 2_000), b"a tree");
            let merk = grove.host(host);
            for element in 0..1 + n % 3 {
                grove.put(merk, &key(element), &bytes(n as u64, 40));
            }
            merks.push(merk);
        }
        grove.flush(&mut txn);
        db.commit(txn);
    }
    db.write_back();
    let before = fill(&db);

    let dropped: Vec<usize> = merks.iter().copied().step_by(3).collect();
    for chunk in dropped.chunks(100) {
        let mut txn = db.begin();
        grove.drop_merks(&mut txn, chunk, false);
        db.commit(txn);
    }
    let left = grove.left_behind();
    db.tick();
    let done = reclaim_all(&db, &grove);
    let after = fill(&db);
    check_reclaimed(&db, &grove);

    let cells: u64 = done.iter().map(|done| done.cells).sum();
    let freed: u64 = done.iter().map(|done| done.pages_freed).sum();
    let written: u64 = done.iter().map(|done| done.pages_written).sum();
    assert_eq!(cells as usize, left);
    println!(
        "tiny subtrees: {} pages {:.1}% full, then {} cells of {} subtrees reclaimed in {written} \
         page writes with {freed} pages freed, and {} pages {:.1}% full",
        before.0,
        before.1 * 100.0,
        cells,
        dropped.len(),
        after.0,
        after.1 * 100.0
    );
    // The subtrees are spread over the pages of their hosts, so hardly a
    // page loses all it holds, and what is left is that much emptier.
    assert!(freed <= 2 && written >= dropped.len() as u64 / 2);
    assert!(after.1 < before.1 && after.0 as u64 + freed == before.0 as u64);
}

/// What reclaiming leaves in a flat column: the records of a Merk that was
/// nested in a dropped one go a leaf at a time, and no leaf leaves its
/// tree (`flat`, choice 12). A keyspace of five thousand records between
/// those of two Merks that stay: every leaf it had to itself stays, with
/// no entry. The dropped Merk's own records went by the lazy range delete,
/// and their pages were freed.
#[test]
fn reclaiming_flat_records_leaves_their_leaves_with_no_entry() {
    let db = Db::with(PageSize::MIN, 16 << 20);
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    let hosts: Vec<usize> = (0..3).map(|n| grove.put(0, &key(n), b"a tree")).collect();
    let (first, outer, last) = (
        grove.host(hosts[0]),
        grove.host(hosts[1]),
        grove.host(hosts[2]),
    );
    let host = grove.put(outer, b"a host", b"a tree");
    // The nested Merk is entered between the two that stay, so its records
    // lie between theirs.
    grove.flush(&mut txn);
    db.commit(txn);
    let mut txn = db.begin();
    let middle = grove.merks[first].prefix;
    let mut between = middle;
    between[31] = 0xFF;
    assert!(between > middle && between < grove.merks[outer].prefix);
    let nested = grove.host_under(host, between);
    grove.put(nested, b"a node", b"of the nested Merk");
    grove.flush(&mut txn);
    db.commit(txn);
    let records: usize = 5_000;
    for (merk, count) in [
        (first, 200),
        (outer, records),
        (last, 200),
        (nested, records),
    ] {
        for chunk in 0..count.div_ceil(1_000) {
            let mut txn = db.begin();
            for n in chunk * 1_000..count.min(chunk * 1_000 + 1_000) {
                grove.put_flat(merk, 0, &record(n), &bytes(n as u64, 24));
            }
            grove.flush(&mut txn);
            db.commit(txn);
        }
    }
    db.write_back();
    let column = grove.columns[0].tree;
    let before = scan_flat(&db, &column);
    assert_eq!(before.empty_leaves, 0);

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[outer], false);
    db.commit(txn);
    db.tick();
    let dropped = scan_flat(&db, &column);
    let done = reclaim_all(&db, &grove);
    let after = scan_flat(&db, &column);
    check_reclaimed(&db, &grove);

    println!(
        "a column of {} leaves; after the drop of a Merk with {records} records of its own, \
         {} leaves; the {records} records of the Merk nested in it were taken out of {} \
         leaves, and {} leaves of {} stay with no entry",
        before.leaves, dropped.leaves, done[0].flat_leaves, after.empty_leaves, after.leaves
    );
    // The drop took the pages of its own records out of the tree.
    assert!(dropped.leaves < before.leaves);
    // Reclaiming took no page out: the leaves that held nothing else are
    // empty, and still there.
    assert_eq!(done[0].flat_records, records as u64);
    assert_eq!(after.leaves, dropped.leaves);
    assert_eq!(after.pages, dropped.pages);
    assert!(after.empty_leaves as u64 + 2 >= done[0].flat_leaves);
    assert!(after.empty_leaves > 10);
}
