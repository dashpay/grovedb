//! Deferral: a turn of the background loop leaves the pages that changed
//! last for a later turn ([`FlushConfig::max_deferred`]), and what is kept
//! of a page that waits stays within one run.

use super::*;
use crate::flush::{
    state::{lock, Changes, Kept},
    write_back::{AFTER_THE_LOOK, PENDING_SCANS},
};

/// A store whose turns leave `leave` pages.
fn leaving(leave: usize) -> Db {
    Db::with(FlushConfig {
        max_deferred: leave,
        ..FlushConfig::new(Db::SIZE)
    })
}

/// A turn that finds more pages waiting than it may leave writes those
/// whose current version is the oldest. A pass that a caller makes leaves
/// none, and writes the versions that waited behind a page's current one
/// in the block of that one.
#[test]
fn a_turn_leaves_the_pages_that_changed_last() {
    let db = leaving(2);
    for id in 1..=3 {
        db.commit(&[insert((id, 1), b"a", inline(b"v"))]);
    }
    let tick = db.wb().tick_deferring().expect("a turn");
    let pass = tick.pass;
    assert_eq!((pass.written, pass.deferred, pass.waiting), (1, 2, 2));
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(
        (db.shape(page(2)), db.shape(page(3))),
        (String::new(), String::new())
    );
    assert!(tick.progressed());

    // Page 2 changes again. Its first version has no reader: it is kept
    // for the block of the second, and both pages are left.
    db.commit(&[hash((2, 1), 7)]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.held, pass.deferred), (0, 1, 2));
    assert_eq!(pass.waiting, 2, "one version of each page");

    // A fourth page: now page 3 is the one that has waited longest.
    db.commit(&[insert((4, 1), b"d", inline(b"v"))]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.held, pass.deferred), (1, 0, 2));
    assert_eq!(db.shape(page(3)), "B");
    assert_eq!(
        (db.shape(page(2)), db.shape(page(4))),
        (String::new(), String::new())
    );
    db.check_warm();

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.deferred, pass.waiting), (2, 0, 0));
    assert_eq!(
        (db.shape(page(2)), db.shape(page(4))),
        ("B".into(), "B".into())
    );
    let stats = db.stats();
    assert_eq!((stats.base_blocks, stats.delta_blocks), (4, 0));
    assert_eq!(stats.runs_refused, 0);
    db.check_warm();
    db.check_cold();
}

/// The commits that change a page while it is left are written as one run
/// behind its image. Each of the three changes a cell of its own, so the
/// page as it is read from the run shows that the run holds all three.
#[test]
fn the_commits_of_a_page_that_was_left_are_one_run() {
    let db = leaving(4);
    // Three cells that make the page's image longer than the run will be.
    db.commit(&[
        insert((1, 1), b"a", inline(&noise(1, 300))),
        insert((1, 2), b"b", inline(&noise(2, 300))),
        insert((1, 3), b"c", inline(&noise(3, 300))),
    ]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    for cell in 1..=3u16 {
        db.commit(&[hash((1, cell), 10 + cell as u8)]);
        let pass = db.wb().tick_deferring().expect("a turn").pass;
        assert_eq!((pass.written, pass.deferred), (0, 1));
    }
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.wb().state.unwritten(), 1);

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!(db.shape(page(1)), "Bd");
    let stats = db.stats();
    assert_eq!((stats.delta_blocks, stats.runs_refused), (1, 0));
    assert_eq!(stats.held_for_readers, 2, "the first two of the three");
    db.check_warm();
    db.check_cold();
}

/// A turn in which a table checkpoint is due leaves no page, and the
/// checkpoint covers every commit.
#[test]
fn a_turn_with_a_table_checkpoint_due_leaves_none() {
    let db = Db::with(FlushConfig {
        max_deferred: 8,
        checkpoint_log_bytes: 1,
        ..FlushConfig::new(Db::SIZE)
    });
    for id in 1..=3 {
        db.commit(&[insert((id, 1), b"a", inline(b"v"))]);
    }
    let tick = db.wb().tick_deferring().expect("a turn");
    assert_eq!((tick.pass.written, tick.pass.deferred), (3, 0));
    let (covers, _, _) = installed(tick.checkpoint.expect("one was due"));
    assert_eq!(covers, db.latest());
}

/// A turn leaves no more than half of the page versions that may wait,
/// and no more than a quarter of the pool's frames.
#[test]
fn what_a_turn_leaves_is_bounded_by_the_pool_and_by_what_may_wait() {
    let db = Db::with(FlushConfig {
        max_deferred: usize::MAX,
        max_unwritten: 8,
        ..FlushConfig::new(Db::SIZE)
    });
    assert_eq!(db.wb().deferral(), 4);
    for id in 1..=6 {
        db.commit(&[insert((id, 1), b"a", inline(b"v"))]);
    }
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred), (2, 4));
    assert!(db.wb().has_room());

    let roomy = Db::with(FlushConfig {
        max_deferred: usize::MAX,
        max_unwritten: usize::MAX,
        ..FlushConfig::new(Db::SIZE)
    });
    assert!(roomy.pool.frames() >= 4);
    assert_eq!(roomy.wb().deferral(), roomy.pool.frames() / 4);
    assert_eq!(leaving(0).wb().deferral(), 0);
    assert_eq!(leaving(3).wb().deferral(), 3);
}

/// What is kept of a page stays within one run: once the records of the
/// commits that wait are longer, they are let go, and the page is written
/// as an image. Nothing is refused for it, and the next commit is a run
/// again.
#[test]
fn a_page_whose_commits_outgrow_a_run_is_written_as_an_image() {
    let db = leaving(4);
    let cells: Vec<Op> = (1..=10u16)
        .map(|slot| insert((1, slot), &slot.to_be_bytes(), inline(b"v")))
        .collect();
    db.commit(&cells);
    db.wb().pass().expect("a pass");
    assert_eq!(db.stats().base_blocks, 1);

    // Each commit is ten hash records. Together they pass a page.
    let record = db.stats().node_record_bytes;
    db.commit(&(1..=10).map(|slot| hash((1, slot), 1)).collect::<Vec<_>>());
    let of_one = (db.stats().node_record_bytes - record) as usize;
    let commits = max_run_len(Db::SIZE) / of_one + 2;
    for round in 0..commits {
        let byte = (round % 250) as u8 + 2;
        db.commit(
            &(1..=10)
                .map(|slot| hash((1, slot), byte))
                .collect::<Vec<_>>(),
        );
        let pass = db.wb().tick_deferring().expect("a turn").pass;
        assert_eq!((pass.written, pass.deferred), (0, 1));
    }
    // The records were let go: it is not their length at the write that
    // makes the block an image.
    assert!(lock(&db.state.kept).changes(page(1)).is_empty());
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.written, 1);
    let stats = db.stats();
    assert_eq!((stats.base_blocks, stats.delta_blocks), (2, 0));
    assert_eq!((stats.consolidations, stats.runs_refused), (0, 0));
    db.check_warm();
    db.check_cold();

    db.commit(&[hash((1, 1), 1)]);
    db.wb().pass().expect("a pass");
    let stats = db.stats();
    assert_eq!((stats.base_blocks, stats.delta_blocks), (2, 1));
    assert_eq!(stats.runs_refused, 0);
    db.check_cold();
}

/// The records of a page are let go when they pass the bound, and kept
/// again for the commits after the next block of the page. The versions
/// that were let go have no records until a block holds them.
#[test]
fn records_are_let_go_past_the_bound_and_kept_again_after_a_block() {
    let one = page(1);
    let kept_of = |kept: &Kept| -> Vec<(u64, usize)> {
        (kept.changes(one).iter())
            .map(|changes| (changes.born, changes.records.len()))
            .collect()
    };
    let mut kept = Kept::default();
    kept.committed(one, 1, &[1; 60], 100);
    kept.stepped(one, 1, &[1; 40], 100);
    assert_eq!(kept_of(&kept), [(1, 100)], "at the bound, and kept");
    kept.committed(one, 2, &[2; 1], 100);
    assert_eq!(kept_of(&kept), [], "past it");
    // Nothing is kept until a block of the page is written.
    kept.committed(one, 3, &[3; 10], 100);
    kept.stepped(one, 3, &[9; 4], 100);
    assert_eq!(kept_of(&kept), []);

    // A block through version 2: the commits after it are kept again, and
    // version 3, which was let go, has no records, whatever steps it.
    kept.wrote(one, &[], 2);
    kept.committed(one, 4, &[4; 10], 100);
    kept.stepped(one, 3, &[9; 4], 100);
    kept.stepped(one, 4, &[9; 4], 100);
    assert_eq!(kept_of(&kept), [(4, 14)]);

    // A block through version 4 holds everything.
    let written: Vec<Changes> = kept.changes(one);
    kept.wrote(one, &written, 4);
    kept.committed(one, 5, &[5; 10], 100);
    kept.stepped(one, 5, &[9; 4], 100);
    assert_eq!(kept_of(&kept), [(5, 14)]);

    // A step alone can pass the bound, and a page that is freed or made
    // anew is one of which nothing was let go.
    kept.stepped(one, 5, &[9; 90], 100);
    assert_eq!(kept_of(&kept), []);
    kept.forget(one);
    kept.committed(one, 6, &[6; 10], 100);
    assert_eq!(kept_of(&kept), [(6, 10)]);
    kept.committed(one, 7, &[7; 100], 100);
    assert_eq!(kept_of(&kept), []);
    kept.freed(one, 7);
    kept.committed(one, 8, &[8; 10], 100);
    assert_eq!(kept_of(&kept), [], "version 7 was let go and is not freed");
    kept.freed(one, 9);
    kept.committed(one, 9, &[9; 10], 100);
    assert_eq!(kept_of(&kept), [(9, 10)]);
}

/// Commits that each change more pages than a turn may leave: every turn
/// writes down to that many, so what waits after a turn is bounded by the
/// setting, whatever a commit changes, and takes that many frames.
#[test]
fn commits_of_more_pages_than_a_turn_leaves_are_written_down_to_that() {
    let db = leaving(3);
    for round in 0..4u8 {
        let ops: Vec<Op> = (1..=8u32)
            .map(|id| match round {
                0 => insert((id, 1), b"a", inline(&noise(u64::from(id), 300))),
                _ => hash((id, 1), round),
            })
            .collect();
        db.commit(&ops);
        let pass = db.wb().tick_deferring().expect("a turn").pass;
        assert_eq!((pass.written, pass.deferred, pass.waiting), (5, 3, 3));
        // The versions that were left by the turn before are kept for the
        // block of their successors, and take no frame that waits.
        assert_eq!(pass.held, if round == 0 { 0 } else { 3 });
        assert_eq!(db.pool.census().unwritten, 3);
        assert_eq!(db.wb().state.unwritten(), 3);
    }
    db.check_warm();
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (3, 0));
    assert_eq!(db.pool.census().unwritten, 0);
    assert_eq!(db.stats().runs_refused, 0);
    db.check_cold();
}

/// A version of a page that a pin can reach gets its block though the
/// page is left: the reader of that version is given it from a segment,
/// the current version waits, and the chain is in the order of the
/// versions. The version that no reader can ask for goes into the block
/// of the one that is written last.
#[test]
fn a_version_that_a_pin_reaches_gets_its_block_while_the_page_is_left() {
    let db = leaving(4);
    db.commit(&[
        insert((1, 1), b"a", inline(&noise(1, 300))),
        insert((1, 2), b"b", inline(&noise(2, 300))),
    ]);
    db.wb().pass().expect("a pass");
    let second = db.commit(&[hash((1, 1), 2)]);
    db.versions.pin(second);
    let third = db.commit(&[hash((1, 1), 3)]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.held, pass.deferred), (1, 0, 1));
    assert_eq!(db.shape(page(1)), "Bd");
    assert_eq!(db.chain(page(1))[1].version, second);
    // The pinned version as its block gives it, through an empty pool.
    {
        let cold = new_pool(Db::SIZE);
        let mut buffers = LoadBuffers::new();
        let frame = (db.loader(&cold))
            .get(page(1), second, &mut buffers)
            .expect("the pinned version");
        assert_eq!(frame.born(), second);
        let model = db.model.lock().expect("the model");
        let (_, cells) = model.at(page(1), second).expect("the page at the pin");
        let found = db.cells(page(1), &frame, None);
        assert!(same(&found, cells), "{found:?}, not {cells:?}");
    }

    // The pin goes, and the third version is superseded: it is kept for
    // the block of the fourth.
    db.versions.unpin();
    let fourth = db.commit(&[hash((1, 2), 4)]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.held, pass.deferred), (0, 1, 1));
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!(db.shape(page(1)), "Bdd");
    let versions: Vec<CommitVersion> = (db.chain(page(1)).iter())
        .map(|link| link.version)
        .collect();
    assert_eq!(versions[1..], [second, fourth]);
    assert!(third < fourth);
    assert_eq!(db.stats().runs_refused, 0);
    db.check_warm();
    db.check_cold();
}

/// A page that a turn left with nothing to do for it is not looked at by
/// the turns after it: a turn in a store at rest takes no page in hand,
/// neither to write it nor to look for its pending values. A commit that
/// changes the page has it looked at again.
#[test]
fn a_turn_does_not_look_again_at_a_page_that_rests() {
    let db = leaving(4);
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    db.commit(&[insert((2, 1), b"b", inline(b"w"))]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred, pass.waiting), (0, 2, 2));
    assert_eq!(lock(&db.state.files).rested.len(), 2);

    // A look at a page would run this: no turn at rest makes one.
    let looked = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = |looked: &std::rc::Rc<std::cell::Cell<u32>>| {
        let looked = looked.clone();
        AFTER_THE_LOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |_| looked.set(looked.get() + 1)));
        });
    };
    count(&looked);
    let scanned = PENDING_SCANS.with(|scans| scans.get());
    for _ in 0..3 {
        let tick = db.wb().tick_deferring().expect("a turn");
        assert_eq!((tick.pass.deferred, tick.pass.waiting), (2, 2));
        assert!(!tick.progressed());
    }
    assert_eq!(looked.get(), 0);
    assert_eq!(PENDING_SCANS.with(|scans| scans.get()), scanned);

    // Page 1 changes: it is looked at, and its first version is kept for
    // the block of the second. Page 2 rests on. The turn after that finds
    // one version of page 1 waiting, and from then on it rests too.
    db.commit(&[hash((1, 1), 9)]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.held, pass.deferred, pass.waiting), (1, 2, 2));
    assert_eq!(looked.get(), 1);
    assert_eq!(PENDING_SCANS.with(|scans| scans.get()), scanned + 1);
    let rests = || -> Vec<PageId> {
        let mut rests: Vec<PageId> = lock(&db.state.files).rested.keys().copied().collect();
        rests.sort_unstable();
        rests
    };
    assert_eq!(rests(), [page(2)]);
    count(&looked);
    db.wb().tick_deferring().expect("a turn");
    assert_eq!((looked.get(), rests()), (2, vec![page(1), page(2)]));

    // A pass that leaves none writes both, and nothing rests.
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.deferred, pass.waiting), (2, 0, 0));
    assert!(lock(&db.state.files).rested.is_empty());
    db.check_warm();
    db.check_cold();
}

/// With more pages waiting than a turn leaves, a flat page is left before
/// a tree page that changed after it: its block would be its whole image.
/// The images of flat pages are counted apart.
#[test]
fn a_flat_page_is_left_before_a_tree_page() {
    let db = leaving(1);
    let flat = page(9);
    db.commit_flat(flat, &[(b"k", inline(b"v"))], &BTreeMap::new());
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred, pass.waiting), (1, 1, 1));
    assert_eq!(
        (db.shape(page(1)), db.shape(flat)),
        ("B".into(), String::new())
    );
    assert_eq!(db.stats().flat_blocks, 0);
    assert_eq!(
        lock(&db.state.files)
            .rested
            .get(&flat)
            .map(|rests| rests.flat),
        Some(true)
    );

    // Two flat pages and one may be left: the one that changed last.
    let other = page(8);
    db.commit_flat(other, &[(b"m", inline(b"w"))], &BTreeMap::new());
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred), (1, 1));
    assert_eq!(
        (db.shape(flat), db.shape(other)),
        ("B".into(), String::new())
    );
    let stats = db.stats();
    assert_eq!((stats.base_blocks, stats.flat_blocks), (2, 1));
    assert!(stats.flat_stored_bytes > 0 && stats.flat_stored_bytes < stats.stored_bytes);

    db.wb().pass().expect("a pass");
    assert_eq!(db.stats().flat_blocks, 2);
    db.check_cold();
}

/// Has the next look that a pass of this thread takes at a page counted.
fn next_look() -> std::rc::Rc<std::cell::Cell<u32>> {
    let looked = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = looked.clone();
    AFTER_THE_LOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |_| count.set(count.get() + 1)));
    });
    looked
}

/// A page step gives a page that rests no new version and nothing to do
/// for it: the page rests on, the turn after the step does not look at
/// it, and the block that is written at last holds the step with the
/// commit.
#[test]
fn a_page_step_leaves_a_page_at_rest_and_reaches_its_block() {
    let db = leaving(4);
    db.commit(&[
        insert((1, 1), b"a", long(1, 2000)),
        insert((1, 2), b"b", inline(&noise(2, 300))),
    ]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    db.commit(&[hash((1, 2), 7)]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred), (0, 1));
    let rested = || lock(&db.state.files).rested.get(&page(1)).copied();
    let rests = rested().expect("the page rests");

    // The value of the first cell is moved: a page step, and no commit.
    let before = db.pointer(page(1), slot(1));
    db.move_value(page(1), slot(1), false);
    assert_ne!(db.pointer(page(1), slot(1)), before);
    let looked = next_look();
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred, pass.waiting), (0, 1, 1));
    assert_eq!((looked.get(), rested()), (0, Some(rests)));

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!((looked.get(), rested()), (1, None));
    assert_eq!(db.shape(page(1)), "Bd");
    assert_eq!(db.stats().runs_refused, 0);
    db.check_warm();
    db.check_cold();
    // The page as its blocks give it holds the pointer the step gave.
    let moved = db.pointer(page(1), slot(1));
    let cold = new_pool(Db::SIZE);
    let mut buffers = LoadBuffers::new();
    let frame = (db.loader(&cold))
        .get(page(1), db.latest(), &mut buffers)
        .expect("the page from its blocks");
    let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
    let cell = tree.cell(slot(1)).expect("the cell");
    assert_eq!(cell.value_pointer(), Some(moved));
    assert_ne!(moved, before);
}

/// A page that is left has its pending value placed in the turn that
/// leaves it, and rests from that turn on.
#[test]
fn a_page_that_is_left_has_its_pending_value_placed() {
    let db = leaving(4);
    db.commit(&[insert((1, 1), b"a", long(1, 2000))]);
    assert_eq!(db.pending(page(1)).len(), 1);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!(
        (pass.written, pass.placed, pass.blocked, pass.deferred),
        (0, 1, 0, 1)
    );
    assert!(db.pending(page(1)).is_empty());
    assert_eq!(db.pool.attached_bytes(), 0);
    assert!(lock(&db.state.files).rested.contains_key(&page(1)));
    assert_eq!(db.shape(page(1)), "");

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed, pass.waiting), (1, 0, 0));
    assert_eq!(db.shape(page(1)), "B");
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (1, 1));
    assert_eq!(stats.runs_refused, 0);
    db.check_warm();
    db.check_cold();
}

/// A page that rests and is freed waits no more: it rests no longer and
/// no block of it is written. A page that is made under its id afterwards
/// rests with a version of its own.
#[test]
fn a_page_that_rests_and_is_freed_is_forgotten() {
    let db = leaving(4);
    let first = db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    db.commit(&[insert((2, 1), b"b", inline(b"w"))]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.deferred, pass.waiting), (2, 2));
    let rested = |id| lock(&db.state.files).rested.get(&page(id)).copied();
    assert_eq!(rested(1).map(|rests| rests.born), Some(first));

    db.commit(&[Op::Free { page: page(1) }]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred, pass.waiting), (0, 1, 1));
    assert!(rested(1).is_none() && rested(2).is_some());
    // The page never had a block, so it has no chain for a free link.
    assert_eq!(db.shape(page(1)), "");

    let again = db.commit(&[insert((1, 1), b"c", inline(b"x"))]);
    let pass = db.wb().tick_deferring().expect("a turn").pass;
    assert_eq!((pass.written, pass.deferred, pass.waiting), (0, 2, 2));
    assert_eq!(rested(1).map(|rests| rests.born), Some(again));
    assert!(first < again);

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (2, 0));
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.stats().base_blocks, 2);
    db.check_warm();
    db.check_cold();
}
