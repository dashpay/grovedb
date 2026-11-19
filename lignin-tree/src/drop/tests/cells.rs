//! What a drop writes, and what reclaiming takes out of the tree pages.

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::{tree_page::Side, CellAddress, PageId};
use lignin_store::{alloc::PageState, load::LoadBuffers};

use super::*;
use crate::node::{Lease, NodeReader};

fn key(n: usize) -> Vec<u8> {
    format!("key-{n:06}").into_bytes()
}

/// A value that stays in its cell, and one that does not.
fn value(n: usize) -> Vec<u8> {
    match n % 5 {
        0 => bytes(n as u64, 300),
        _ => bytes(n as u64, 24),
    }
}

/// Puts `count` nodes into `merk`, in an order that keeps the tree low.
fn fill(grove: &mut Grove, merk: usize, count: usize, from: usize) -> Vec<usize> {
    let mut order = Vec::with_capacity(count);
    let mut spans = vec![(0, count)];
    while let Some((low, high)) = spans.pop() {
        if low < high {
            let middle = low + (high - low) / 2;
            order.push(middle);
            spans.insert(0, (low, middle));
            spans.insert(0, (middle + 1, high));
        }
    }
    order
        .into_iter()
        .map(|n| grove.put(merk, &key(from + n), &value(from + n)))
        .collect()
}

/// A grove whose root Merk has `live` nodes and one more, `host`, which
/// hosts a Merk of `inside` nodes. Every tenth node of that Merk hosts a
/// Merk of three nodes, and the first of those a Merk of two in turn.
struct Built {
    db: Db,
    grove: Grove,
    host: usize,
    merk: usize,
}

fn build(live: usize, inside: usize) -> Built {
    build_nested(live, inside, 10)
}

/// The same, with a nested Merk at every `every`th node.
fn build_nested(live: usize, inside: usize, every: usize) -> Built {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    fill(&mut grove, 0, live, 0);
    let host = grove.put(0, b"host", b"a tree");
    let merk = grove.host(host);
    grove.flush(&mut txn);
    db.commit(txn);

    let mut from = 1_000;
    for chunk in 0..inside.div_ceil(40) {
        let mut txn = db.begin();
        let count = 40.min(inside - chunk * 40);
        let nodes: Vec<usize> = (0..count)
            .map(|n| grove.put(merk, &key(from + n * 7919 % 40), &value(from + n)))
            .collect();
        from += 40;
        let nested = if every > count {
            0
        } else {
            count.div_ceil(every)
        };
        for &node in nodes.iter().step_by(every.min(count)).take(nested) {
            let nested = grove.host(node);
            let inner = fill(&mut grove, nested, 3, 0);
            let deepest = grove.host(inner[0]);
            fill(&mut grove, deepest, 2, 0);
        }
        grove.flush(&mut txn);
        db.commit(txn);
    }
    db.write_back();
    grove.check(&db);
    Built {
        db,
        grove,
        host,
        merk,
    }
}

/// The pages a drop writes do not depend on what the Merk holds: the page
/// of the host, and the same pages of the directory.
#[test]
fn a_drop_writes_the_page_of_its_host_and_the_directory_whatever_the_merk_holds() {
    let mut written = Vec::new();
    for inside in [1, 40, 1_200] {
        // No Merk is nested here, so the directory holds the same records
        // whatever the Merk holds.
        let Built {
            db,
            mut grove,
            host,
            merk,
        } = build_nested(30, inside, usize::MAX);
        let below = grove.below(merk).len();
        assert!(below >= inside);
        let cells = db.cells();
        let before = db.log.lock().expect("the log").len();

        // The write holds the host and the nodes above it, as every write
        // of a Merk does, and changes no other page.
        let mut path = BTreeSet::new();
        let mut above = Some(host);
        while let Some(node) = above {
            path.insert(grove.nodes[node].at.expect("a cell").page());
            above = grove.nodes[node].parent;
        }
        let mut txn = db.begin();
        let left = grove.drop_merks(&mut txn, &[merk], false);
        let host_page = grove.nodes[host].at.expect("the host").page();
        let touched = txn.tree.touched();
        assert!(touched.contains(&host_page));
        assert!(
            touched.iter().all(|page| path.contains(page)),
            "{touched:?}"
        );
        let flat = txn.flat.written().count();
        db.commit(txn);
        let group = db.log.lock().expect("the log").len() - before;

        assert_eq!(left.len(), 1);
        assert!(left[0].root.is_some());
        // Nothing of the Merk was touched: its cells are where they were.
        assert_eq!(db.cells(), cells);
        assert_eq!(grove.left_behind(), below);
        grove.check(&db);
        assert!(group < 400, "{group} bytes in the log");
        written.push(flat);
    }
    // The same page of the directory.
    assert_eq!(written, [1, 1, 1]);
}

/// The same holds with Merks nested in the dropped one: none of them is
/// read or written by the drop.
#[test]
fn a_drop_reads_and_writes_nothing_of_the_merks_nested_in_it() {
    let Built {
        db,
        mut grove,
        host,
        merk,
    } = build(30, 400);
    let nested = grove.merks_below(merk);
    assert!(nested.len() >= 20);
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    let host_page = grove.nodes[host].at.expect("the host").page();
    assert_eq!(txn.tree.touched(), [host_page]);
    // One record of each kind is written, each in one leaf, and the
    // branches above them: no more than that for any number of Merks.
    assert!(txn.flat.written().count() <= 8);
    db.commit(txn);
    let directory = grove.directory(&db);
    let pages = db.reader();
    for merk in nested {
        let prefix = grove.merks[merk].prefix;
        assert!(directory.open(&pages, &prefix).expect("a lookup").is_none());
        assert!(directory
            .stored(&pages, &prefix)
            .expect("a lookup")
            .is_some());
    }
}

/// Reclaiming takes out every cell the dropped Merk left, with those of
/// the Merks nested in it, frees every page that keeps no cell, reports
/// every value pointer the cells held, and leaves the cells in use alone.
#[test]
fn reclaiming_takes_out_every_cell_and_frees_the_pages_that_are_empty() {
    let Built {
        db,
        mut grove,
        merk,
        ..
    } = build(60, 900);
    let nested: BTreeSet<_> = grove
        .merks_below(merk)
        .into_iter()
        .map(|merk| grove.merks[merk].prefix)
        .collect();
    let pages_before: BTreeSet<PageId> =
        db.tree_frames().into_iter().map(|(page, _)| page).collect();
    let pointers_before = db.tree_pointers().len();
    assert!(nested.len() > 40 && pointers_before > 100);

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    db.commit(txn);
    let left = grove.left_behind();
    assert_eq!(db.cells(), grove.alive() + left);
    // The counts still hold what the cells that were left hold.
    db.check_counts(&db.tree_pointers());

    db.tick();
    let done = reclaim_all(&db, &grove);
    let [done] = &done[..] else {
        panic!("one Merk was dropped");
    };
    assert_eq!(done.cells as usize, left);
    assert_eq!(done.records as usize, 3 * nested.len() + 1);
    let named: BTreeSet<_> = done.merks.iter().map(|(prefix, _)| *prefix).collect();
    assert_eq!(named, nested);
    assert_eq!(done.merks.len(), nested.len());
    assert_eq!(
        done.values as usize,
        pointers_before - db.tree_pointers().len()
    );
    assert!(done.values > 100 && done.value_bytes == done.values * 300);
    assert!(done.cell_bytes > done.cells * 100);

    check_reclaimed(&db, &grove);
    let pages_after: BTreeSet<PageId> =
        db.tree_frames().into_iter().map(|(page, _)| page).collect();
    let freed: Vec<PageId> = pages_before.difference(&pages_after).copied().collect();
    assert_eq!(freed.len() as u64, done.pages_freed);
    assert!(done.pages_freed > 5 && done.pages_written >= 1);
    for &page in &freed {
        assert_eq!(db.state(page), PageState::Retired);
        assert!(db.current(page).is_none());
    }

    // The log has one step for each page that was freed, and the steps
    // list the pointers that were released.
    let steps = db.reclaims();
    let freed_by_step: Vec<PageId> = steps
        .iter()
        .filter(|(_, freed, ..)| freed.is_some())
        .map(|(page, ..)| *page)
        .collect();
    assert_eq!(
        freed_by_step.iter().collect::<BTreeSet<_>>().len(),
        freed.len()
    );
    assert_eq!(freed_by_step.len(), freed.len());
    let released: usize = steps.iter().map(|(_, _, released, _)| released).sum();
    assert_eq!(released as u64, done.values);
    // A step that frees its page lists no cell, and every other one does.
    assert!(steps
        .iter()
        .all(|(_, freed, _, listed)| freed.is_some() != *listed));

    // The ids go back once no reader can reach the pages.
    let released: BTreeSet<PageId> = db.reclaim_ids().into_iter().collect();
    assert!(freed.iter().all(|page| released.contains(page)));
}

/// The Merks nested in a dropped one are taken out of the directory the
/// innermost first, each after every Merk below it.
#[test]
fn the_nested_merks_go_the_innermost_first() {
    let Built {
        db,
        mut grove,
        merk,
        ..
    } = build(10, 120);
    let depth_of = |grove: &Grove, mut merk: usize| {
        let mut depth = 0;
        while let Some((host, _)) = grove.merks[merk].host {
            merk = grove.nodes[host].merk;
            depth += 1;
        }
        depth
    };
    let depths: BTreeMap<_, _> = grove
        .merks_below(merk)
        .into_iter()
        .map(|nested| (grove.merks[nested].prefix, depth_of(&grove, nested)))
        .collect();
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    db.commit(txn);
    db.tick();
    let done = reclaim_all(&db, &grove);
    let order: Vec<usize> = done[0]
        .merks
        .iter()
        .map(|(prefix, _)| depths[prefix])
        .collect();
    assert_eq!(order.len(), depths.len());
    // A Merk two levels down goes before the Merk it was in.
    let mut deepest_left = order.iter().filter(|depth| **depth == 3).count();
    assert!(deepest_left > 0);
    for pair in order.chunks(2) {
        assert_eq!(pair, [3, 2]);
        deepest_left -= 1;
    }
    assert_eq!(deepest_left, 0);
    check_reclaimed(&db, &grove);
}

/// A drop that deletes the host as well, and a host that is the only node
/// of its page: the page holds nothing in use afterwards, and is freed.
#[test]
fn a_dropped_merk_whose_host_is_deleted_is_reclaimed() {
    let Built {
        db,
        mut grove,
        host,
        merk,
    } = build(40, 200);
    let mut txn = db.begin();
    let left = grove.drop_with_host(&mut txn, host);
    assert_eq!(left.len(), 1);
    db.commit(txn);
    assert!(!grove.merks[merk].alive);
    db.tick();
    let done = reclaim_all(&db, &grove);
    assert_eq!(done[0].cells as usize, grove.left_behind());
    check_reclaimed(&db, &grove);
}

/// A clear is a drop that keeps the host: the Merk is there, with no node,
/// what it held is reclaimed, and what is put into it afterwards stays.
#[test]
fn a_cleared_merk_is_reclaimed_and_keeps_what_it_is_given_afterwards() {
    let Built {
        db,
        mut grove,
        merk,
        ..
    } = build(40, 300);
    let mut txn = db.begin();
    let left = grove.drop_merks(&mut txn, &[merk], true);
    assert_eq!(left.len(), 1);
    db.commit(txn);
    assert!(grove.merks[merk].alive && grove.merks[merk].root.is_none());
    let was_left = grove.left_behind();

    // The Merk is written again before anything is reclaimed.
    let mut txn = db.begin();
    let again = fill(&mut grove, merk, 25, 5_000);
    let nested = grove.host(again[3]);
    fill(&mut grove, nested, 4, 0);
    grove.flush(&mut txn);
    db.commit(txn);
    grove.check(&db);

    let done = reclaim_all(&db, &grove);
    assert_eq!(done[0].cells as usize, was_left);
    check_reclaimed(&db, &grove);

    // A Merk with no node has nothing to clear.
    let mut txn = db.begin();
    let empty = grove.host(again[5]);
    grove.flush(&mut txn);
    assert!(grove.drop_merks(&mut txn, &[empty], true).is_empty());
    db.commit(txn);
    check_reclaimed(&db, &grove);
}

/// A Merk that is entered again under the prefix of a dropped one, and one
/// under the prefix of a Merk that was nested in it, start empty and keep
/// their records while what the drop left is reclaimed.
#[test]
fn a_merk_entered_again_under_a_dropped_prefix_keeps_its_records() {
    let Built {
        db,
        mut grove,
        host,
        merk,
    } = build(20, 160);
    let prefix = grove.merks[merk].prefix;
    let nested = grove.merks_below(merk)[0];
    let nested_prefix = grove.merks[nested].prefix;

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    db.commit(txn);

    let mut txn = db.begin();
    let again = grove.host_again(host, 0, prefix);
    let nodes = fill(&mut grove, again, 12, 9_000);
    let inner = grove.host_under(nodes[2], nested_prefix);
    fill(&mut grove, inner, 5, 0);
    grove.flush(&mut txn);
    db.commit(txn);
    grove.check(&db);

    let left = grove.left_behind();
    let done = reclaim_all(&db, &grove);
    assert_eq!(done[0].cells as usize, left);
    check_reclaimed(&db, &grove);
    let directory = grove.directory(&db);
    let pages = db.reader();
    for merk in [again, inner] {
        let prefix = grove.merks[merk].prefix;
        let entry = directory.open(&pages, &prefix).expect("a lookup");
        let root = grove.merks[merk].root.and_then(|root| grove.nodes[root].at);
        assert_eq!(
            entry.expect("the Merk").root().map(|root| root.address),
            root
        );
    }
}

/// Reclaiming that is taken on one page step at a time comes to the same
/// end, and says what is left at every point.
#[test]
fn reclaiming_step_by_step_comes_to_the_same_end() {
    let Built {
        db,
        mut grove,
        merk,
        ..
    } = build(50, 400);
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    db.commit(txn);
    db.tick();

    let directory = grove.directory(&db);
    let [dropped] = drops(&db, &directory)[..] else {
        panic!("one Merk was dropped");
    };
    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(dropped);
    assert_eq!(reclaim.dropped(), dropped);
    assert_eq!(
        reclaimer
            .reclaim(&mut reclaim, &directory, &GUARD, 0)
            .expect("no step"),
        Progress::More
    );
    let mut calls = 0;
    let mut held_cells = false;
    loop {
        let cells = db.cells();
        let progress = reclaimer
            .reclaim(&mut reclaim, &directory, &GUARD, 1)
            .expect("a step");
        calls += 1;
        // Every state between two steps is one of whole pages.
        db.check_pages();
        grove.check(&db);
        db.check_counts(&db.tree_pointers());
        if reclaim.holds_cells() {
            held_cells = true;
            assert!(reclaim.pending() > 0);
            assert!(drops(&db, &directory).is_empty());
        }
        match progress {
            Progress::Done => break,
            Progress::More => assert!(db.cells() <= cells),
            Progress::Blocked(busy) => panic!("nothing holds a page: {busy:?}"),
        }
    }
    assert!(held_cells && calls > 20);
    assert!(reclaim.is_done() && !reclaim.holds_cells() && reclaim.pending() == 0);
    assert_eq!(reclaim.reclaimed().cells as usize, grove.left_behind());
    assert_eq!(
        reclaimer
            .reclaim(&mut reclaim, &directory, &GUARD, 1)
            .expect("done"),
        Progress::Done
    );
    check_reclaimed(&db, &grove);
}

/// A Merk with no node leaves a record and nothing else, and reclaiming
/// takes the record.
#[test]
fn a_dropped_merk_with_no_node_leaves_only_its_record() {
    let Built {
        db,
        mut grove,
        host,
        ..
    } = build(10, 0);
    let mut txn = db.begin();
    let left = grove.drop_with_host(&mut txn, host);
    assert_eq!(left.len(), 1);
    assert!(left[0].root.is_none());
    db.commit(txn);
    let done = reclaim_all(&db, &grove);
    let none = Reclaimed {
        records: 1,
        ..Reclaimed::default()
    };
    assert_eq!(done, [none]);
    check_reclaimed(&db, &grove);
}

/// Several Merks dropped by one write, among them two whose cells share
/// pages, are reclaimed one after the other.
#[test]
fn merks_dropped_together_are_reclaimed_one_by_one() {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    fill(&mut grove, 0, 30, 0);
    let hosts: Vec<usize> = (0..6)
        .map(|n| grove.put(0, format!("host-{n}").as_bytes(), b"a tree"))
        .collect();
    let merks: Vec<usize> = hosts.iter().map(|host| grove.host(*host)).collect();
    for (n, &merk) in merks.iter().enumerate() {
        fill(&mut grove, merk, 3 + n * 9, 100 * n);
    }
    grove.flush(&mut txn);
    db.commit(txn);
    db.write_back();

    let mut txn = db.begin();
    let left = grove.drop_merks(&mut txn, &[merks[0], merks[2], merks[5]], false);
    assert_eq!(left.len(), 3);
    db.commit(txn);
    db.tick();
    let done = reclaim_all(&db, &grove);
    assert_eq!(done.len(), 3);
    let cells: u64 = done.iter().map(|done| done.cells).sum();
    assert_eq!(cells as usize, grove.left_behind());
    check_reclaimed(&db, &grove);
}

/// What a drop left waits for the readers of the versions before it, and
/// those readers read all of it for as long as they are held.
#[test]
fn what_a_drop_left_waits_for_the_readers_from_before() {
    use std::sync::atomic::Ordering;

    let Built {
        db,
        mut grove,
        merk,
        ..
    } = build(20, 150);
    let before = db.latest();
    let addresses: Vec<(CellAddress, Vec<u8>)> = grove
        .below(merk)
        .into_iter()
        .map(|node| {
            (
                grove.nodes[node].at.expect("a cell"),
                grove.nodes[node].key.clone(),
            )
        })
        .collect();
    // A snapshot from before the drop, and an operation that runs.
    db.pins.pinned.store(before.get(), Ordering::SeqCst);
    db.pins.guard.store(0, Ordering::SeqCst);

    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    let dropped_at = db.commit(txn);
    db.tick();

    let mut wait = Wait::new(db.latest());
    assert_eq!(wait.seen(), db.latest());
    assert!(!wait.poll(&db.pins) && !wait.poll(&db.pins));
    let read_all = |at| {
        let reader = NodeReader::new(db.loader(), &GUARD, Lease::take(), at);
        let mut buffers = LoadBuffers::new();
        for (address, key) in &addresses {
            let node = reader.node(*address, &mut buffers).expect("the cell");
            assert_eq!(node.node().key(), key);
            let _ = node.node().link(Side::Left);
        }
    };
    read_all(before);

    // A pin on the drop's own version does not hold it back: the wait is
    // for the version its record was read at.
    db.pins.pinned.store(dropped_at.get(), Ordering::SeqCst);
    assert!(!wait.poll(&db.pins));
    db.pins.pinned.store(u64::MAX, Ordering::SeqCst);
    // The pin is gone, and the operation that ran then still runs.
    assert!(!wait.poll(&db.pins));
    assert!(!wait.poll(&db.pins));
    db.pins.guard.store(u64::MAX, Ordering::SeqCst);
    assert!(wait.poll(&db.pins) && wait.poll(&db.pins));

    let done = reclaim_all(&db, &grove);
    assert_eq!(done[0].cells as usize, addresses.len());
    check_reclaimed(&db, &grove);
}
