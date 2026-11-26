//! How a page version becomes a block: a run or a base image, one link for
//! a commit or for several, and what is never written.

use super::*;

/// The versions of a page that no reader can ask for any more are not
/// written one by one: their records go into the run of the next version
/// that is written (`transactions.md` FL3).
#[test]
fn the_commits_of_a_page_fold_into_one_run_when_no_reader_is_between_them() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"k", inline(&noise(1, 1000)))]);
    db.wb().pass().expect("a pass");
    let before = db.stats();
    for round in 0..4u8 {
        db.commit(&[hash((1, 1), round)]);
    }
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.held, pass.waiting), (1, 3, 0));
    let after = db.stats();
    assert_eq!(db.shape(page(1)), "Bd");
    assert_eq!(db.chain(page(1))[1].version, db.latest());
    // One run with the four hashes.
    assert_eq!(after.delta_bytes - before.delta_bytes, 4 + 4 * 35);
    assert_eq!(after.runs_refused, 0);
    assert_eq!(db.pool.reclaim(&db.versions), 3);
    db.check_cold();
}

/// A version that a snapshot can ask for gets a link of its own, so that
/// the snapshot's page can be read from its segments.
#[test]
fn a_version_that_a_reader_can_ask_for_has_a_link_of_its_own() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((1, 1), 2)]);
    let pinned = db.commit(&[hash((1, 1), 3)]);
    db.commit(&[hash((1, 1), 4)]);
    db.commit(&[hash((1, 1), 5)]);
    db.versions.pin(pinned);

    let pass = db.wb().pass().expect("a pass");
    // Version 2 is folded into 3, which the snapshot reads. 4 and 5 each
    // have a reader that could come between them and the next.
    assert_eq!((pass.written, pass.held), (3, 1));
    assert_eq!(db.shape(page(1)), "Bddd");
    let versions: Vec<u64> = db
        .chain(page(1))
        .iter()
        .map(|link| link.version.get())
        .collect();
    assert_eq!(versions, [1, 3, 4, 5]);
    db.check_cold();
}

/// A page is freed while a change of an earlier commit to it is only in
/// the log and the pool (`page-table.md` T18; test T-PN7). The change is
/// never written back, a snapshot below the freeing version keeps reading
/// it, and a table checkpoint covers it all the same.
#[test]
fn nothing_older_than_a_free_link_is_written_back() {
    let db = Db::new();
    db.commit(&[
        insert((5, 1), b"k", inline(b"v")),
        insert((6, 1), b"k", inline(b"v")),
    ]);
    db.wb().pass().expect("a pass");
    let changed = db.commit(&[hash((5, 1), 9)]);
    db.versions.pin(changed);
    let freed = db.commit(&[Op::Free { page: page(5) }]);
    assert_eq!(db.shape(page(5)), "BF");

    let before = db.stats();
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (0, 0));
    assert_eq!(db.stats().stored_bytes, before.stored_bytes);
    assert_eq!(db.shape(page(5)), "BF");
    // The snapshot reads the change from the pool, which keeps it.
    db.check_warm();
    assert_eq!(db.pool.census().held, 1);

    let checkpointed = db.wb().checkpoint().expect("a checkpoint");
    assert!(matches!(checkpointed, Checkpointed::Installed { covers, .. } if covers == freed));

    db.versions.unpin();
    assert_eq!(db.pool.reclaim(&db.versions), 1);
    db.check_cold();
}

/// The id of a freed page names a new page: its first block is a base
/// image behind the free link, and the old chain goes when no reader can
/// ask for it.
#[test]
fn a_page_that_is_made_again_under_a_freed_id_starts_with_a_base_image() {
    let db = Db::new();
    db.commit(&[
        insert((5, 1), b"old", inline(b"v")),
        insert((6, 1), b"k", inline(b"v")),
    ]);
    db.commit(&[hash((5, 1), 1)]);
    db.wb().pass().expect("a pass");
    let old = db.latest();
    db.versions.pin(old);
    db.commit(&[Op::Free { page: page(5) }]);
    db.commit(&[insert((5, 3), b"new", inline(b"w"))]);
    db.commit(&[hash((5, 3), 2)]);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.waiting, 0);
    assert_eq!(db.shape(page(5)), "BFBd");
    db.check_cold();

    db.versions.unpin();
    db.commit(&[hash((6, 1), 1)]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((5, 3), 3)]);
    db.wb().pass().expect("a pass");
    // The chain of the page that was freed went with its free link.
    assert_eq!(db.shape(page(5)), "Bdd");
    db.check_cold();
}

/// A run that would be as long as the image of its page is not written:
/// the image is, and a reader has one block to read.
#[test]
fn a_run_as_long_as_the_image_of_its_page_is_written_as_the_image() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    for round in 0..4u8 {
        db.commit(&[hash((1, 1), round)]);
    }
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.held), (1, 3));
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.stats().consolidations, 0);
    db.check_cold();
}

/// A link that the table would refuse is not written: a change that was
/// owed when the table already says the page was freed after it.
#[test]
fn a_version_below_a_free_link_that_the_pool_still_lists_is_skipped() {
    let db = Db::new();
    db.commit(&[insert((5, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    let changed = db.commit(&[hash((5, 1), 9)]);
    // The free link is pushed first, and the pool is told after it
    // (`cache`, "What the callers do"). A pass in between sees this.
    db.table
        .push(page(5), Link::free(version(changed.get() + 1)))
        .expect("a free link");
    let before = db.stats();
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (0, 1));
    assert_eq!(db.stats().stored_bytes, before.stored_bytes);
}

/// The value log's cleaner moves a value of a page that is in a segment.
/// Its record comes through write-back, which then writes the step as a
/// run of one record with the version of the link before it, checked
/// against the page as the blocks of its chain give it.
#[test]
fn a_page_step_of_other_background_work_is_written_as_its_record() {
    let db = Db::new();
    let born = db.commit(&[
        insert((1, 1), b"a", long(1, 2000)),
        insert((1, 2), b"b", inline(b"v")),
    ]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    let before = (db.stats(), db.pointers(page(1)), db.versions.guards());

    db.move_value(page(1), slot(1), false);
    assert_ne!(db.pointers(page(1)), before.1);
    assert_eq!(db.pool.oldest_unwritten(), Some(born));
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    let after = db.stats();
    assert_eq!(db.shape(page(1)), "Bd");
    assert_eq!(db.chain(page(1))[1].version, born);
    assert_eq!(after.delta_bytes - before.0.delta_bytes, 4 + 19);
    assert_eq!(after.runs_refused, 0);
    // The page under the run was read from its blocks, under a guard:
    // one chain, of its base image alone.
    assert_eq!(db.versions.guards(), before.2 + 1);
    assert_eq!(
        (
            after.chains_read - before.0.chains_read,
            after.chain_blocks_read - before.0.chain_blocks_read
        ),
        (1, 1)
    );
    db.check_cold();

    // The same on a page version that no segment holds yet: the record
    // follows the commit's in one run.
    db.commit(&[hash((1, 2), 7)]);
    db.move_value(page(1), slot(1), false);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.written, 1);
    assert_eq!(db.shape(page(1)), "Bdd");
    assert_eq!(db.stats().delta_bytes - after.delta_bytes, 4 + 35 + 19);
    assert_eq!(db.stats().runs_refused, 0);
    db.check_cold();
}

/// A page step whose record did not come through write-back loses
/// nothing: write-back finds that what it kept does not lead to the frame,
/// and writes the page as a base image.
#[test]
fn a_page_step_that_write_back_was_not_told_of_makes_a_base_image() {
    let db = Db::new();
    db.commit(&[
        insert((1, 1), b"a", long(1, 2000)),
        insert((1, 2), b"b", inline(b"v")),
    ]);
    db.wb().pass().expect("a pass");

    // On a page that is in a segment: nothing is kept, so no run is tried.
    db.move_value(page(1), slot(1), true);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.written, 1);
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.stats().runs_refused, 0);
    db.check_cold();

    // On a page version whose commit write-back knows: the run of the
    // commit alone does not give the frame, and is refused.
    db.commit(&[hash((1, 2), 7)]);
    db.move_value(page(1), slot(1), true);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.written, 1);
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.stats().runs_refused, 1);
    db.check_cold();
}

/// A chain that an earlier session left longer than this session's
/// thresholds allow is consolidated: a base image is added right behind
/// its newest link, built from the blocks.
#[test]
fn a_chain_that_is_over_the_thresholds_is_consolidated() {
    let db = Db::new();
    db.versions.pin(version(0));
    db.commit(&[
        insert((1, 1), b"k", inline(b"v")),
        insert((2, 1), b"k", inline(b"v")),
    ]);
    for round in 0..5u8 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
    }
    db.commit(&[hash((2, 1), 1)]);
    db.wb().pass().expect("a pass");
    assert_eq!(
        (db.shape(page(1)), db.shape(page(2))),
        ("Bddddd".into(), "Bd".into())
    );

    // Another state, with a threshold of two runs, for the same stores.
    let strict = FlushState::new(FlushConfig {
        max_chain_deltas: 2,
        ..FlushConfig::new(Db::SIZE)
    });
    let wb = WriteBack::new(
        Stores {
            pool: &db.pool,
            table: &db.table,
            segments: &db.segments,
            values: &db.values,
            log: &db.log,
            checkpoints: &db.checkpoints,
            manifest: &db.manifest,
            versions: &db.versions,
            page_values: None,
        },
        &strict,
        db.size,
    )
    .expect("write-back");
    assert_eq!(wb.consolidate_long_chains().expect("consolidated"), 1);
    assert_eq!(
        (db.shape(page(1)), db.shape(page(2))),
        ("BdddddB".into(), "Bd".into())
    );
    let chain = db.chain(page(1));
    assert_eq!(chain[6].version, chain[5].version);
    assert_eq!(strict.stats().consolidations, 1);
    db.check_cold();

    // A chain that ends in a base image, and a page that has none, need
    // nothing.
    assert!(!wb.consolidate(page(1)).expect("nothing to do"));
    assert!(!wb.consolidate(page(9)).expect("nothing to do"));
    assert_eq!(wb.consolidate_long_chains().expect("nothing to do"), 0);
    // And a chain of a page that is changed afterwards goes on from the
    // new image.
    db.versions.unpin();
    db.commit(&[hash((1, 1), 77)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "Bd");
    db.check_cold();
}

/// A page is freed, and the pool drops what it held of it, after
/// write-back has looked at the page and before it keeps an old version of
/// it for readers. The frame in write-back's hand is no page version any
/// more, and nothing is owed of the page: write-back passes over it with
/// no error, links nothing of it, and the pass goes on. So it is for a
/// page that never had a link, which gets no free link, and for one that
/// had its base image written.
#[test]
fn a_page_that_is_freed_and_dropped_after_the_look_is_passed_over() {
    use std::{cell::Cell, rc::Rc};

    for (linked, shape) in [(false, ""), (true, "BF")] {
        let db = Rc::new(Db::new());
        db.commit(&[
            insert((1, 1), b"a", inline(b"v")),
            insert((2, 1), b"b", inline(b"w")),
        ]);
        if linked {
            db.wb().pass().expect("a pass");
            db.commit(&[hash((1, 1), 5)]);
        }
        // Two versions of page 1 are owed, with no reader between them:
        // the older is one that write-back keeps for readers and does not
        // write.
        db.commit(&[hash((1, 1), 7)]);

        let (store, ran) = (db.clone(), Rc::new(Cell::new(None)));
        let told = ran.clone();
        crate::flush::write_back::AFTER_THE_LOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |looked_at| {
                let freed = store.commit(&[Op::Free { page: looked_at }]);
                // No reader can ask for the page: the pool drops both
                // frames, which write-back still has pinned.
                let dropped = (0..2)
                    .map(|_| store.pool.reclaim(&store.versions))
                    .sum::<usize>();
                told.set(Some((looked_at, freed, dropped)));
            }));
        });
        let before = db.stats();
        let pass = db.wb().pass().expect("a pass with no error");
        let (looked_at, freed, dropped) = ran.get().expect("the hook ran");
        assert_eq!((looked_at, dropped), (page(1), 2), "linked {linked}");
        assert_eq!(pass.held, 0, "linked {linked}: {pass:?}");
        assert_eq!(db.stats().held_for_readers, before.held_for_readers);

        // Nothing of the freed page was linked after the look.
        assert_eq!(db.shape(page(1)), shape, "linked {linked}");
        if let Some(last) = db.chain(page(1)).last() {
            assert_eq!(last.version, freed);
        }
        // The other page is written, by this pass or by the next.
        db.wb().pass().expect("a pass");
        assert!(db.shape(page(2)).starts_with('B'), "linked {linked}");
        db.check_cold();
    }
}
