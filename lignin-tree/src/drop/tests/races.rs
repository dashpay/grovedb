//! Reclaiming beside a writer: a page that a write transaction has copied
//! is left alone, a record is taken out only as it was read, and steps and
//! commits that run at once leave the pages whole (`transactions.md`
//! T-CL1).

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use lignin_format::PageId;
use lignin_store::alloc::PageState;

use super::{
    super::step::{seen, EntryRemoved, Step},
    *,
};
use crate::directory::record::{dropped_key, entry_key};

fn key(n: usize) -> Vec<u8> {
    format!("k{n:05}").into_bytes()
}

fn value(n: usize) -> Vec<u8> {
    match n % 4 {
        0 => bytes(n as u64, 280),
        _ => bytes(n as u64, 20),
    }
}

/// Puts the keys `from..from + count` into `merk`, the middle ones first.
fn fill(grove: &mut Grove, merk: usize, count: usize, from: usize) -> Vec<usize> {
    let mut nodes = Vec::new();
    let mut spans = std::collections::VecDeque::from([(0, count)]);
    while let Some((low, high)) = spans.pop_front() {
        if low < high {
            let middle = low + (high - low) / 2;
            nodes.push(grove.put(merk, &key(from + middle), &value(from + middle)));
            spans.push_back((low, middle));
            spans.push_back((middle + 1, high));
        }
    }
    nodes
}

/// A grove whose root Merk holds `live` nodes and two hosts. The first
/// hosts a Merk of `inside` nodes with Merks nested in it, which the tests
/// drop, and the second a Merk of 40 nodes that stays.
struct Built {
    db: Db,
    grove: Grove,
    dropped: usize,
    kept: usize,
}

fn build(live: usize, inside: usize) -> Built {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    fill(&mut grove, 0, live, 0);
    let first = grove.put(0, b"host-a", b"a tree");
    let second = grove.put(0, b"host-b", b"a tree");
    let (dropped, kept) = (grove.host(first), grove.host(second));
    fill(&mut grove, kept, 40, 50_000);
    grove.flush(&mut txn);
    db.commit(txn);
    for chunk in 0..inside.div_ceil(50) {
        let mut txn = db.begin();
        let nodes = fill(
            &mut grove,
            dropped,
            50.min(inside - chunk * 50),
            1_000 + chunk * 50,
        );
        for &node in nodes.iter().step_by(17) {
            let nested = grove.host(node);
            fill(&mut grove, nested, 4, 0);
        }
        grove.flush(&mut txn);
        db.commit(txn);
    }
    db.write_back();
    grove.check(&db);
    Built {
        db,
        grove,
        dropped,
        kept,
    }
}

/// The pages that hold a cell that a drop left, and those among them that
/// hold a cell in use as well.
fn pages_left(grove: &Grove) -> (BTreeSet<PageId>, BTreeSet<PageId>) {
    let page = |node: &super::grove::Node| node.at.expect("a cell").page();
    let left: BTreeSet<PageId> = grove
        .nodes
        .iter()
        .filter(|node| node.left_behind)
        .map(page)
        .collect();
    let in_use: BTreeSet<PageId> = grove
        .nodes
        .iter()
        .filter(|node| node.alive)
        .map(page)
        .collect();
    let shared = left.intersection(&in_use).copied().collect();
    (left, shared)
}

/// Drops the Merk, and returns its record with the directory of the
/// version after.
fn drop_it(db: &Db, grove: &mut Grove, merk: usize) -> (Directory, Dropped) {
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    db.commit(txn);
    db.tick();
    let directory = grove.directory(db);
    let [dropped] = drops(db, &directory)[..] else {
        panic!("one Merk was dropped");
    };
    (directory, dropped)
}

/// A write transaction has copied the page that the dropped root is in,
/// which holds cells in use as well. Nothing is taken out of that page, and
/// so nothing at all: the Merk keeps its record until its root can go.
/// Then the transaction commits a change to the page, or gives its copy
/// up, and everything is reclaimed.
#[test]
fn nothing_is_reclaimed_while_the_page_of_the_root_is_copied() {
    for commits in [true, false] {
        let Built {
            db,
            mut grove,
            dropped: merk,
            ..
        } = build(60, 300);
        let root = grove.merks[merk].root.expect("a root");
        let root_page = grove.nodes[root].at.expect("a cell").page();
        let (directory, dropped) = drop_it(&db, &mut grove, merk);
        let (_, shared) = pages_left(&grove);
        assert!(shared.contains(&root_page));
        let before = grove.clone();

        // The writer changes a node in use whose cell is in that page.
        let mut txn = db.begin();
        let in_page = |node: &&super::grove::Node| {
            node.alive && node.merk == 0 && node.at.expect("a cell").page() == root_page
        };
        let changed = grove
            .nodes
            .iter()
            .find(in_page)
            .expect("a node")
            .key
            .clone();
        grove.put(0, &changed, b"another value");
        grove.flush(&mut txn);
        assert!(txn.tree.touched().contains(&root_page));

        let reclaimer = db.reclaimer();
        let mut reclaim = Reclaim::new(dropped);
        let cells = db.cells();
        for _ in 0..3 {
            let progress = reclaimer.reclaim(&mut reclaim, &directory, &GUARD, usize::MAX);
            assert_eq!(progress.expect("a look"), Progress::Blocked(Busy::Copied));
        }
        // The records of the nested Merks are gone, and the Merk's own is
        // there: no cell was taken out, so the walk can begin from it.
        assert!(!reclaim.holds_cells() && reclaim.reclaimed().cells == 0);
        assert!(reclaim.reclaimed().records > 0);
        assert_eq!(drops(&db, &directory), [dropped]);
        assert_eq!(db.cells(), cells);

        match commits {
            true => drop(db.commit(txn)),
            false => {
                db.roll_back(txn);
                grove = before;
            }
        }
        finish(&db, &directory, &mut reclaim, usize::MAX);
        assert_eq!(reclaim.reclaimed().cells as usize, grove.left_behind());
        // No cell that was taken out is back, and the change is there.
        check_reclaimed(&db, &grove);
    }
}

/// A write transaction has copied a page in the middle of the dropped
/// subtree. The walk takes out what it can reach without that page, says
/// that it waits, and goes on when the copy is gone.
#[test]
fn the_walk_waits_at_a_page_that_is_copied_and_goes_on_afterwards() {
    for commits in [true, false] {
        let Built {
            db,
            mut grove,
            dropped: merk,
            ..
        } = build(40, 700);
        let (directory, dropped) = drop_it(&db, &mut grove, merk);
        let (left, shared) = pages_left(&grove);
        let own: Vec<PageId> = left.difference(&shared).copied().collect();
        assert!(own.len() > 4);
        let held = own[own.len() / 2];

        let mut txn = db.begin();
        txn.tree.copy(held);
        let reclaimer = db.reclaimer();
        let mut reclaim = Reclaim::new(dropped);
        let progress = reclaimer.reclaim(&mut reclaim, &directory, &GUARD, usize::MAX);
        assert_eq!(
            progress.expect("the steps"),
            Progress::Blocked(Busy::Copied)
        );
        assert!(reclaim.holds_cells() && reclaim.pending() > 0);
        let so_far = reclaim.reclaimed().cells;
        assert!(so_far > 0 && (so_far as usize) < grove.left_behind());
        // The page is as it was, and so is what hangs below it.
        assert!(db.current(held).is_some());
        db.check_pages();
        grove.check(&db);
        db.check_counts(&db.tree_pointers());
        let again = reclaimer.reclaim(&mut reclaim, &directory, &GUARD, usize::MAX);
        assert_eq!(again.expect("a look"), Progress::Blocked(Busy::Copied));
        assert_eq!(reclaim.reclaimed().cells, so_far);

        // A copy that was not changed is given up by its commit as well.
        match commits {
            true => drop(db.commit(txn)),
            false => db.roll_back(txn),
        }
        finish(&db, &directory, &mut reclaim, usize::MAX);
        assert_eq!(reclaim.reclaimed().cells as usize, grove.left_behind());
        check_reclaimed(&db, &grove);
        assert_eq!(db.state(held), PageState::Retired);
    }
}

/// A write transaction copies a page after cells were taken out of it, and
/// commits a change to it: its copy was made of the page as the step left
/// it, so no cell comes back.
#[test]
fn a_copy_made_after_a_step_does_not_bring_a_cell_back() {
    let Built {
        db,
        mut grove,
        dropped: merk,
        ..
    } = build(60, 300);
    let root = grove.merks[merk].root.expect("a root");
    let root_page = grove.nodes[root].at.expect("a cell").page();
    let (directory, dropped) = drop_it(&db, &mut grove, merk);
    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(dropped);
    // The steps up to the first that takes cells out: that of the root.
    while reclaim.reclaimed().cells == 0 {
        reclaimer
            .reclaim(&mut reclaim, &directory, &GUARD, 1)
            .expect("a step");
    }
    assert!(reclaim.holds_cells());

    let mut txn = db.begin();
    let in_page = |node: &&super::grove::Node| {
        node.alive && node.merk == 0 && node.at.expect("a cell").page() == root_page
    };
    let changed = grove
        .nodes
        .iter()
        .find(in_page)
        .expect("a node")
        .key
        .clone();
    grove.put(0, &changed, &bytes(9, 300));
    // New nodes take the slots that the step freed.
    fill(&mut grove, 0, 30, 80_000);
    grove.flush(&mut txn);
    db.commit(txn);

    finish(&db, &directory, &mut reclaim, usize::MAX);
    assert_eq!(reclaim.reclaimed().cells as usize, grove.left_behind());
    check_reclaimed(&db, &grove);
}

/// A reclaimer runs between the plan of a write and its changes: after the
/// write has read the page that it changes, which holds cells that a drop
/// left. The write has its own copy of every page it read, so the step is
/// refused, and the write changes the page it planned on.
#[test]
fn a_step_between_the_plan_of_a_write_and_its_changes_is_refused() {
    use std::cell::RefCell;

    let Built {
        db,
        mut grove,
        dropped: merk,
        ..
    } = build(60, 300);
    let root = grove.merks[merk].root.expect("a root");
    let root_page = grove.nodes[root].at.expect("a cell").page();
    let (directory, dropped) = drop_it(&db, &mut grove, merk);
    let reclaim = RefCell::new(Reclaim::new(dropped));
    let seen_blocked = RefCell::new(0);

    let mut txn = db.begin();
    txn.tree.after_read = Some(Box::new(|page| {
        if page != root_page {
            return;
        }
        let progress =
            db.reclaimer()
                .reclaim(&mut reclaim.borrow_mut(), &directory, &GUARD, usize::MAX);
        assert_eq!(progress.expect("a look"), Progress::Blocked(Busy::Copied));
        *seen_blocked.borrow_mut() += 1;
    }));
    let in_page = |node: &&super::grove::Node| {
        node.alive && node.merk == 0 && node.at.expect("a cell").page() == root_page
    };
    let changed = grove
        .nodes
        .iter()
        .find(in_page)
        .expect("a node")
        .key
        .clone();
    grove.put(0, &changed, &bytes(3, 300));
    fill(&mut grove, 0, 20, 70_000);
    grove.flush(&mut txn);
    txn.tree.after_read = None;
    assert!(*seen_blocked.borrow() > 0);
    assert_eq!(reclaim.borrow().reclaimed().cells, 0);
    db.commit(txn);

    let mut reclaim = reclaim.into_inner();
    finish(&db, &directory, &mut reclaim, usize::MAX);
    assert_eq!(reclaim.reclaimed().cells as usize, grove.left_behind());
    check_reclaimed(&db, &grove);
}

/// A write transaction holds a copy of a leaf of the directory, because it
/// enters a Merk. No record of that leaf is taken out until it has
/// committed, and its Merk is there afterwards.
#[test]
fn a_record_waits_while_its_leaf_is_copied() {
    let Built {
        db,
        mut grove,
        dropped: merk,
        kept,
    } = build(30, 150);
    let (directory, dropped) = drop_it(&db, &mut grove, merk);

    let mut txn = db.begin();
    let host = grove.merks[kept].root.expect("a root");
    let entered = grove.host(host);
    fill(&mut grove, entered, 6, 0);
    grove.flush(&mut txn);
    assert!(txn.flat.written().count() > 0);

    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(dropped);
    let progress = reclaimer.reclaim(&mut reclaim, &directory, &GUARD, usize::MAX);
    assert_eq!(progress.expect("a look"), Progress::Blocked(Busy::Copied));
    assert!(!reclaim.holds_cells() && reclaim.reclaimed().cells == 0);
    db.commit(txn);

    // The directory of the reclaimer is the one from before the commit: it
    // has the dropped Merk, which is all it is asked for.
    finish(&db, &directory, &mut reclaim, usize::MAX);
    check_reclaimed(&db, &grove);
    let directory = grove.directory(&db);
    let prefix = grove.merks[entered].prefix;
    assert!(directory
        .open(&db.reader(), &prefix)
        .expect("a lookup")
        .is_some());
}

/// A record is taken out only as it was read. A Merk that is entered again
/// under the prefix of a left-over entry, between the reclaimer's look and
/// its step, keeps its entry.
#[test]
fn a_record_that_is_another_one_by_now_is_not_taken_out() {
    let Built {
        db,
        mut grove,
        dropped: merk,
        kept,
    } = build(30, 150);
    let nested = grove.merks_below(merk)[0];
    let prefix = grove.merks[nested].prefix;
    let (directory, dropped) = drop_it(&db, &mut grove, merk);
    let tree = *directory.tree();
    let key = entry_key(&prefix);
    let looked = seen(&db.reader(), &tree, &key)
        .expect("a look")
        .expect("the left-over entry");

    let mut txn = db.begin();
    let host = grove.merks[kept].root.expect("a root");
    let again = grove.host_under(host, prefix);
    fill(&mut grove, again, 3, 0);
    grove.flush(&mut txn);
    db.commit(txn);

    let reclaimer = db.reclaimer();
    let step = reclaimer
        .remove_entry(&tree, &key, &looked)
        .expect("a step");
    assert!(matches!(step, Step::Made(EntryRemoved::Changed)));
    let now = grove.directory(&db);
    assert!(now.open(&db.reader(), &prefix).expect("a lookup").is_some());

    // The record of the drop is taken out once, as it was read.
    let own = dropped_key(dropped.id);
    let looked = seen(&db.reader(), &tree, &own)
        .expect("a look")
        .expect("the record");
    let mut reclaim = Reclaim::new(dropped);
    finish(&db, &now, &mut reclaim, usize::MAX);
    let step = reclaimer
        .remove_entry(&tree, &own, &looked)
        .expect("a step");
    assert!(matches!(step, Step::Made(EntryRemoved::Absent)));
    assert!(seen(&db.reader(), &tree, &own).expect("a look").is_none());
    check_reclaimed(&db, &grove);

    // A second reclaiming of the same drop finds no record, and takes no
    // cell.
    let mut second = Reclaim::new(dropped);
    let refused = reclaimer.reclaim(&mut second, &now, &GUARD, usize::MAX);
    assert!(matches!(
        refused,
        Err(DropError::Directory(
            crate::directory::DirectoryError::NotDropped { .. }
        ))
    ));
    check_reclaimed(&db, &grove);
}

/// A key that the tree keeps in a bucket is not a record that a step takes
/// out.
#[test]
fn an_entry_of_a_bucket_is_not_taken_out_by_a_step() {
    let Built { db, grove, .. } = build(5, 0);
    let tree = *grove.directory.tree();
    let long = vec![7; db.config.key_limit() + 1];
    let looked = seen(&db.reader(), &tree, &[1])
        .expect("a look")
        .expect("the header");
    let refused = db.reclaimer().remove_entry(&tree, &long, &looked);
    assert!(matches!(refused, Err(DropError::Record(_))));
}

/// A writer commits changes to the Merks in use, whose cells share pages
/// with what a drop left, while the reclaimer takes its steps on another
/// thread. Afterwards every cell that was left is gone, every cell in use
/// is there, every page was freed once, and the counts agree.
#[test]
fn a_writer_and_the_reclaimer_at_once_leave_the_pages_whole() {
    let (mut beside, mut waited) = (0, 0);
    for round in 0..4 {
        let Built {
            db,
            mut grove,
            dropped: merk,
            kept,
        } = build(60, 500 + round * 150);
        let (directory, dropped) = drop_it(&db, &mut grove, merk);
        let left = grove.left_behind();
        let finished = AtomicBool::new(false);

        thread::scope(|scope| {
            let reclaiming = scope.spawn(|| {
                let reclaimer = db.reclaimer();
                let mut reclaim = Reclaim::new(dropped);
                let mut waits = 0u32;
                loop {
                    match reclaimer.reclaim(&mut reclaim, &directory, &GUARD, 1) {
                        Ok(Progress::Done) => break,
                        Ok(Progress::More) => {}
                        Ok(Progress::Blocked(_)) => {
                            waits += 1;
                            thread::yield_now();
                        }
                        Err(error) => panic!("{error}"),
                    }
                }
                finished.store(true, Ordering::SeqCst);
                (reclaim.reclaimed().clone(), waits)
            });

            // The writer goes on until the reclaimer is done, and a while
            // longer: a page that waits for a commit gets one.
            let mut n = 0;
            let mut after = 0;
            while after < 3 {
                match finished.load(Ordering::SeqCst) {
                    true => after += 1,
                    false => beside += 1,
                }
                let mut txn = db.begin();
                for step in 0..4 {
                    let at = (n * 7 + step * 13) % 60;
                    grove.put(0, &key(at), &value(n + step));
                    grove.put(kept, &key(60_000 + n * 4 + step), &value(n + step + 1));
                }
                grove.put(0, &key(90_000 + n), &value(n));
                grove.flush(&mut txn);
                db.commit(txn);
                if n % 5 == 0 {
                    db.write_back();
                }
                n += 1;
                assert!(n < 100_000, "the reclaiming does not end");
            }
            let (done, waits) = reclaiming.join().expect("the reclaimer");
            assert_eq!(done.cells as usize, left);
            waited += waits;
        });

        db.write_back();
        check_reclaimed(&db, &grove);
        // One step freed each page that was freed.
        let freed: Vec<PageId> = db
            .reclaims()
            .into_iter()
            .filter_map(|(page, freed, ..)| freed.map(|_| page))
            .collect();
        assert_eq!(freed.iter().collect::<BTreeSet<_>>().len(), freed.len());
        assert!(freed
            .iter()
            .all(|page| db.state(*page) == PageState::Retired));
    }
    // The two did run at once: commits were made while steps were, and
    // steps were refused for the writer's copies.
    eprintln!("{beside} commits beside the reclaimer, {waited} steps that waited");
    assert!(beside > 0);
}

/// Two reclaimers take out what two drops left, whose cells share pages,
/// at the same time.
#[test]
fn two_reclaimers_of_two_merks_work_at_once() {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    fill(&mut grove, 0, 20, 0);
    let hosts: Vec<usize> = (0..2)
        .map(|n| grove.put(0, format!("host-{n}").as_bytes(), b"a tree"))
        .collect();
    let merks: Vec<usize> = hosts.iter().map(|host| grove.host(*host)).collect();
    grove.flush(&mut txn);
    db.commit(txn);
    // The two grow in turns, so their cells lie in the same pages.
    for chunk in 0..12 {
        let mut txn = db.begin();
        for &merk in &merks {
            fill(&mut grove, merk, 25, 1_000 + chunk * 25);
        }
        grove.flush(&mut txn);
        db.commit(txn);
    }
    db.write_back();

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &merks, false);
    db.commit(txn);
    db.tick();
    let directory = grove.directory(&db);
    let dropped = drops(&db, &directory);
    assert_eq!(dropped.len(), 2);

    let cells: u64 = thread::scope(|scope| {
        let work = |dropped: Dropped| {
            let (db, directory) = (&db, &directory);
            scope.spawn(move || {
                let mut reclaim = Reclaim::new(dropped);
                finish(db, directory, &mut reclaim, 1);
                reclaim.reclaimed().cells
            })
        };
        let both = [work(dropped[0]), work(dropped[1])];
        both.into_iter()
            .map(|done| done.join().expect("a reclaimer"))
            .sum()
    });
    assert_eq!(cells as usize, grove.left_behind());
    check_reclaimed(&db, &grove);
}
