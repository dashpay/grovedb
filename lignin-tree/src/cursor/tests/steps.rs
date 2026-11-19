//! Background work that takes a cell out of a page on a cursor's way: the
//! handles into the page end, and the cursor goes on from its key.

use std::{
    collections::BTreeSet,
    ops::Bound,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering as Atomic},
};

use super::*;

/// The pages of the nodes above the one `cursor` stands on, and its own.
fn way_pages(cursor: &Cursor) -> Vec<PageId> {
    let above = cursor.way.iter().map(|above| above.node.page());
    above
        .chain(cursor.handle().map(|here| here.page()))
        .collect()
}

#[test]
fn a_cursor_goes_on_from_its_key_when_a_page_step_ends_a_handle_on_its_way() {
    let nodes = 900;
    for back in [false, true] {
        let tree = Tree::balanced(&keys(&mut Rng(50), nodes), 1);
        let (store, dropped) = planted_with(tree, Placement::Beside(6), 50, 4);
        let pool = roomy(store.pages.len());
        let loader = store.disk.loader(&pool);
        let guard = Guard;
        let reader = NodeReader::new(loader, &guard, Lease::take(), version(1));
        let mut buffers = LoadBuffers::new();
        let mut cursor = Cursor::open(&reader, store.root);
        let mut left = by_page(&dropped);
        let mut expected: Vec<_> = store.tree.records().into_iter().collect();
        if back {
            expected.reverse();
        }

        let mut rng = Rng(51);
        let (mut found, mut steps, mut on_the_way) = (Vec::new(), 0, 0);
        let (start, on) = if back { (LAST, PREV) } else { (FIRST, NEXT) };
        let mut step = start;
        while let Some(record) = go(&mut cursor, &reader, &mut buffers, step) {
            step = on;
            found.push(record);
            // Now and then a cell leaves a page of the way: the page the
            // cursor stands on, the page of the root, or one between.
            let way = way_pages(&cursor);
            let victim = match rng.below(8) {
                0 => way.last().copied(),
                1 => way.first().copied(),
                2 => Some(way[rng.below(way.len())]),
                // Or a page that is not on the way, which costs nothing.
                3 => store.pages.iter().copied().find(|page| !way.contains(page)),
                _ => None,
            };
            let slot = victim.and_then(|page| left.get_mut(&page)?.pop());
            if let (Some(page), Some(slot)) = (victim, slot) {
                take_out(loader, &pool, page, slot);
                steps += 1;
                on_the_way += usize::from(way.contains(&page));
                if way.last() == Some(&page) {
                    // The node is read again by its key, and the cursor
                    // has a new handle for it.
                    let before = cursor.handle().expect("positioned");
                    let refinds = cursor.stats().refinds;
                    let again = go(&mut cursor, &reader, &mut buffers, NODE);
                    assert_eq!(again.as_ref(), found.last());
                    let after = cursor.handle().expect("positioned");
                    assert_eq!(after.address(), before.address());
                    assert_eq!(after.departures(), before.departures() + 1);
                    assert_eq!(cursor.stats().refinds, refinds + 1);
                }
            }
        }
        assert_eq!(found, expected);

        let stats = cursor.stats();
        assert!(
            steps > 100 && on_the_way > 60,
            "{steps} steps, {on_the_way} on the way"
        );
        // A handle that ended is found when it is followed, at most once
        // for each step on the way, and each time is one walk from the
        // root. Nothing else walks from the root but the seek.
        assert!(stats.refinds > 30 && stats.refinds as usize <= on_the_way);
        assert_eq!(stats.descents, 1 + stats.refinds);
    }
}

/// A step that takes a cell out of the page the cursor stands on, before
/// a move in each direction and before each kind of seek.
#[test]
fn every_move_goes_on_after_the_page_it_stands_on_lost_a_cell() {
    let tree = Tree::balanced(&numbered(60), 1);
    let (store, dropped) = planted_with(tree, Placement::Beside(3), 52, 12);
    let pool = roomy(store.pages.len());
    let loader = store.disk.loader(&pool);
    let guard = Guard;
    let reader = NodeReader::new(loader, &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut cursor = Cursor::open(&reader, store.root);
    let key = numbered_key;
    let mut left = by_page(&dropped);
    let mut lose_a_cell = |cursor: &Cursor| {
        let page = cursor.handle().expect("positioned").page();
        let slot = left.get_mut(&page).and_then(Vec::pop).expect("a cell");
        take_out(loader, &pool, page, slot);
    };

    for at in [0u32, 17, 30, 31, 59] {
        let moved = cursor.seek(&reader, &mut buffers, &key(at));
        assert_eq!(arrived(&cursor, moved).expect("a node").0, key(at));

        lose_a_cell(&cursor);
        let after = go(&mut cursor, &reader, &mut buffers, NEXT);
        assert_eq!(after.map(|(key, _)| key), (at < 59).then(|| key(at + 1)));

        let moved = cursor.seek_for_prev(&reader, &mut buffers, &key(at));
        assert_eq!(arrived(&cursor, moved).expect("a node").0, key(at));
        lose_a_cell(&cursor);
        let before = go(&mut cursor, &reader, &mut buffers, PREV);
        assert_eq!(before.map(|(key, _)| key), at.checked_sub(1).map(key));

        // A seek does not follow the handle the cursor has.
        if cursor.valid() {
            lose_a_cell(&cursor);
        }
        let refinds = cursor.stats().refinds;
        let moved = cursor.seek_to_last(&reader, &mut buffers);
        assert_eq!(arrived(&cursor, moved).expect("a node").0, key(59));
        assert_eq!(cursor.stats().refinds, refinds);
    }
    assert_eq!(cursor.stats().refinds, 10);
}

/// Readers scan and seek on their own threads while background work takes
/// cells out of every page of their tree. Every scan gives exactly the
/// tree.
#[test]
fn scans_and_seeks_are_exact_while_page_steps_take_cells_out() {
    let nodes = 1_500;
    let tree = Tree::balanced(&keys(&mut Rng(60), nodes), 1);
    let (store, mut dropped) = planted_with(tree, Placement::Beside(8), 60, 10);
    let pool = roomy(store.pages.len());
    let loader = store.disk.loader(&pool);
    let records: Vec<_> = store.tree.records().into_iter().collect();
    let keys: BTreeSet<&[u8]> = records.iter().map(|(key, _)| key.as_slice()).collect();
    let mut rng = Rng(61);
    rng.shuffle(&mut dropped);

    let done = AtomicBool::new(false);
    let rounds = AtomicUsize::new(0);
    let refinds = AtomicUsize::new(0);
    // The readers that have ended, so that a reader that fails does not
    // leave the background work waiting for it.
    let gone = AtomicUsize::new(0);
    struct Leaving<'a>(&'a AtomicUsize);
    impl Drop for Leaving<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Atomic::SeqCst);
        }
    }
    std::thread::scope(|scope| {
        // Background work: a few steps, and then it lets a reader finish a
        // round, so that the steps are spread over the readers' scans.
        scope.spawn(|| {
            for batch in dropped.chunks(8) {
                let seen = rounds.load(Atomic::SeqCst);
                for at in batch {
                    take_out(loader, &pool, at.page(), at.slot());
                }
                while rounds.load(Atomic::SeqCst) == seen && gone.load(Atomic::SeqCst) < 3 {
                    std::thread::yield_now();
                }
            }
            done.store(true, Atomic::SeqCst);
        });
        for seed in 0..3 {
            let (records, keys, root) = (&records, &keys, store.root);
            let (done, rounds, refinds, pool) = (&done, &rounds, &refinds, &pool);
            let leaving = Leaving(&gone);
            scope.spawn(move || {
                let _leaving = leaving;
                let guard = Guard;
                let lease = Lease::take();
                let mut buffers = LoadBuffers::new();
                let mut rng = Rng(70 + seed);
                let opened = NodeReader::new(loader, &guard, lease, version(1));
                let mut cursor = Cursor::open(&opened, root);
                let mut last = false;
                while !last {
                    last = done.load(Atomic::SeqCst);
                    let reader = NodeReader::new(loader, &guard, lease, version(1));
                    // Forward, a node at a time.
                    let mut at = 0;
                    let mut moved = cursor.seek_to_first(&reader, &mut buffers);
                    while let Some(record) = arrived(&cursor, moved) {
                        assert_eq!(record, records[at]);
                        at += 1;
                        moved = cursor.next(&reader, &mut buffers);
                    }
                    assert_eq!(at, records.len());
                    // Backward.
                    let mut moved = cursor.seek_to_last(&reader, &mut buffers);
                    while let Some(record) = arrived(&cursor, moved) {
                        at -= 1;
                        assert_eq!(record, records[at]);
                        moved = cursor.prev(&reader, &mut buffers);
                    }
                    assert_eq!(at, 0);
                    // Seeks, each with a step after it.
                    for _ in 0..40 {
                        let probe = super::keys(&mut rng, 1).pop().expect("a key");
                        let moved = cursor.seek(&reader, &mut buffers, &probe);
                        let found = arrived(&cursor, moved).map(|(key, _)| key);
                        let from = (Bound::Included(probe.as_slice()), Bound::Unbounded);
                        let expected = keys.range::<[u8], _>(from).next();
                        assert_eq!(found.as_deref(), expected.copied());
                        let Some(found) = found else {
                            continue;
                        };
                        let moved = cursor.prev(&reader, &mut buffers);
                        let before = arrived(&cursor, moved).map(|(key, _)| key);
                        let to = (Bound::Unbounded, Bound::Excluded(found.as_slice()));
                        let expected = keys.range::<[u8], _>(to).next_back();
                        assert_eq!(before.as_deref(), expected.copied());
                    }
                    rounds.fetch_add(1, Atomic::SeqCst);
                }
                refinds.fetch_add(cursor.stats().refinds as usize, Atomic::SeqCst);
                assert_eq!(pool.stats().overflows, 0);
            });
        }
    });
    // The steps met the scans.
    let refinds = refinds.load(Atomic::SeqCst);
    assert!(refinds > 0);
    println!(
        "{} page steps under {} rounds of three readers: {refinds} re-finds",
        dropped.len(),
        rounds.load(Atomic::SeqCst)
    );
}

/// What a re-find does when no node has the cursor's key: it goes to the
/// node beside where the key would be. A caller that keeps its version
/// pinned and gives the root that version has never sees this, so the test
/// gives the cursor another tree while it stands on a key.
#[test]
fn a_re_find_that_misses_its_key_goes_on_from_where_the_key_would_be() {
    let (evens, odds): (Vec<_>, Vec<_>) = numbered(40)
        .into_iter()
        .partition(|key| key.last().is_some_and(|last| last % 2 == 0));
    let (store, dropped) = planted_with(Tree::balanced(&evens, 1), Placement::Beside(3), 90, 6);
    let mut store = store;
    let other = Tree::balanced(&odds, 2);
    let (cells, other_root) =
        Layout::starting_at(1_000).place(&other, Placement::Beside(3), &mut Rng(91));
    store.disk.plant(1, &cells);
    let pool = roomy(store.pages.len() + cells.len());
    let loader = store.disk.loader(&pool);
    let guard = Guard;
    let reader = NodeReader::new(loader, &guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();
    let mut left = by_page(&dropped);

    let cases: [(Move, Option<u32>); 3] = [(NEXT, Some(21)), (PREV, Some(19)), (NODE, None)];
    for (step, comes_to) in cases {
        let mut cursor = Cursor::open(&reader, store.root);
        let moved = cursor.seek(&reader, &mut buffers, &numbered_key(20));
        assert_eq!(arrived(&cursor, moved).expect("a node").0, numbered_key(20));
        let page = cursor.handle().expect("positioned").page();
        let slot = left.get_mut(&page).and_then(Vec::pop).expect("a cell");
        take_out(loader, &pool, page, slot);

        cursor.root = other_root;
        let found = go(&mut cursor, &reader, &mut buffers, step);
        let expected = comes_to.map(|key| (numbered_key(key), value_of(2, &numbered_key(key))));
        assert_eq!(found, expected);
        assert_eq!(cursor.stats().refinds, 1);
    }
}
