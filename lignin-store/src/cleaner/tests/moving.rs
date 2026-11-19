//! The moving of live blocks: what a reader is given does not change, a
//! link that is gone is not brought back, and the throttle.

use super::*;

/// Every version a reader can ask for reads the same before and after
/// the cleaner has moved every block it can, from segments alone, with a
/// pin on an old version among them (CL2, T-PN2). The chains have the
/// same links, with the same versions and kinds, in other places.
#[test]
fn every_reader_reads_what_it_read_before_its_blocks_were_moved() {
    let db = churned(13);
    db.commit(&[insert((7, 1), b"long", long(3, 700))]);
    db.wb().pass().expect("a pass");
    let pinned = db.latest();
    db.versions.pin(pinned);
    for round in 14..=22 {
        churn(&db, round);
    }
    db.commit(&[hash((7, 1), 9)]);
    db.wb().pass().expect("a pass");
    db.check_cold();
    let shapes = |db: &Db| [1, 2, 3, 4, 5, 6, 7].map(|id| db.shape(page(id)));
    let versions = |db: &Db| -> Vec<(PageId, CommitVersion)> {
        let links = links_of(db);
        links.iter().map(|(id, link)| (*id, link.version)).collect()
    };
    let (shapes_before, versions_before) = (shapes(&db), versions(&db));
    let places_before: Vec<BlockLocation> = (links_of(&db).iter())
        .filter_map(|(_, link)| link.location())
        .collect();

    let state = CleanerState::new(eager());
    let stats = crate::stats::Stats::new();
    let cleaner = Cleaner::new(db.wb(), &state).with_stats(&stats);
    let done = settle(&cleaner);
    assert!(done.blocks_moved >= 3, "{done:?}");
    assert!(done.dropped >= 2, "{done:?}");
    assert_eq!(done.waiting, 0);
    assert_eq!(done.blocks_lost, 0);

    assert_eq!(shapes(&db), shapes_before);
    assert_eq!(versions(&db), versions_before);
    let places_after: Vec<BlockLocation> = (links_of(&db).iter())
        .filter_map(|(_, link)| link.location())
        .collect();
    assert_ne!(places_after, places_before);
    assert_eq!(db.versions.oldest_pin(), pinned);
    db.check_warm();
    db.check_cold();

    // Nothing dead is left but in the segments that are still written.
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    for segment in &space.segments {
        assert!(
            segment.has_writer || segment.dead_bytes() == 0,
            "{segment:?}"
        );
    }
    assert_eq!(stats.cleaner_bytes(), state.stats().bytes_moved);
    assert_eq!(state.stats().bytes_moved, done.bytes_moved);
    assert_eq!(cleaner.state().stats().blocks_moved, done.blocks_moved);

    // And the next session reads the same.
    db.versions.unpin();
    cleaner.finish().expect("the cleaner's files are closed");
    let db = db.reopen();
    db.check_cold();
}

/// A block keeps its kind and the way it is stored when it is moved, and
/// its payload byte for byte.
#[test]
fn a_copy_is_a_block_of_the_same_kind_with_the_same_payload() {
    let db = churned(4);
    db.versions.pin(db.latest());
    for round in 5..=7 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
    }
    let read = |db: &Db| -> BTreeMap<(PageId, u64, usize), (String, Vec<u8>)> {
        let mut blocks = BTreeMap::new();
        let mut buf = Vec::new();
        let mut nth = BTreeMap::new();
        for (id, link) in links_of(db) {
            let location = link.location().expect("a block");
            let block = (db.segments.read(location, db.size.get(), &mut buf)).expect("a block");
            let kind = format!("{:?} {:?}", block.content(), block.compression());
            let at: &mut usize = nth.entry((id, link.version.get())).or_default();
            let payload = block.payload().expect("a payload").into_owned();
            blocks.insert((id, link.version.get(), *at), (kind, payload));
            *at += 1;
        }
        blocks
    };
    let before = read(&db);
    assert!(before.values().any(|(kind, _)| kind == "Delta Raw"));
    assert!(before.values().any(|(kind, _)| kind == "Base Lz4"));
    let state = CleanerState::new(eager());
    let done = settle(&Cleaner::new(db.wb(), &state));
    assert!(done.blocks_moved > 0);
    assert_eq!(read(&db), before);
}

/// **Test T-PN4.** A block that is superseded and trimmed while it is
/// being copied is not brought back: the switch of its link is refused,
/// and the copy is dead (CL4).
#[test]
fn a_block_that_loses_its_link_while_it_is_copied_is_not_brought_back() {
    let db = std::sync::Arc::new(churned(13));
    db.versions.pin(db.latest());
    for round in 14..=16 {
        churn(&db, round);
    }
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    // While the first block is copied, the pin goes, commits write its
    // page again, and write-back gives the page a new image and trims
    // what was before it. The hook is called on this thread, while the
    // pass holds no lock of the store but the cleaner's own.
    let lost = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (seen, raced) = (lost.clone(), db.clone());
    let meanwhile = super::super::state::AtSwitch(Box::new(move |id, from| {
        if seen.lock().expect("seen").is_some() {
            return;
        }
        raced.versions.unpin();
        for byte in 51..=53 {
            raced.commit(&[hash((id.get(), 1), byte)]);
            raced.wb().pass().expect("a pass");
        }
        assert!(
            !(raced.chain(id).iter()).any(|link| link.location() == Some(from)),
            "the block still has its link"
        );
        *seen.lock().expect("seen") = Some((id, from));
    }));
    *state.at_switch.lock().expect("the hook") = Some(meanwhile);
    let pass = cleaner.pass().expect("a pass");
    *state.at_switch.lock().expect("the hook") = None;
    let (id, from) = lost.lock().expect("seen").expect("a block was copied");
    assert!(pass.blocks_lost >= 1, "{pass:?}");
    assert_eq!(state.stats().blocks_lost, pass.blocks_lost);
    assert!(state.stats().bytes_lost > 0);
    // No link points at the old block or at a copy of it: the page is what
    // its newest commits made it, in the one block write-back gave it.
    let chain = db.chain(id);
    assert_eq!(chain.len(), 1, "{chain:?}");
    assert!(chain.iter().all(|link| link.location() != Some(from)));
    assert_eq!(chain[0].version, db.latest());
    db.check_warm();
    db.check_cold();
    check_space(&db, &cleaner.space().expect("the space"));
    // The copy that nobody points to is dead space, and goes like any.
    settle(&cleaner);
    db.check_cold();
}

/// The throttle: a pass moves no more bytes than it may, says that more
/// is to do, and the passes after it finish.
#[test]
fn a_pass_moves_no_more_than_the_throttle_allows() {
    let db = churned(13);
    db.versions.pin(db.latest());
    for round in 14..=18 {
        churn(&db, round);
    }
    // Fewer bytes than the smallest block: one block to a pass.
    let config = CleanerConfig {
        max_pass_bytes: 40,
        ..moving_only()
    };
    let state = CleanerState::new(config);
    let cleaner = Cleaner::new(db.wb(), &state);
    let to_move = settle_count(&db);
    let mut passes = 0;
    loop {
        let pass = cleaner.pass().expect("a pass");
        passes += 1;
        // The bound is looked at before a block is moved, so a pass that
        // may move anything moves one block.
        assert!(pass.blocks_moved <= 1, "{pass:?}");
        if !pass.more {
            break;
        }
        assert!(pass.progressed());
        assert!(passes < 200, "the passes do not end");
    }
    assert!(to_move >= 4, "{to_move} blocks to move");
    assert_eq!(state.stats().blocks_moved, to_move);
    assert!(passes >= to_move, "{passes} passes for {to_move} blocks");
    db.check_cold();
    assert_eq!(state.stats().passes, passes);
}

/// The target: with dead space below it nothing is moved, and above it
/// the segments with the largest share of dead bytes go first, as many as
/// it takes.
#[test]
fn blocks_are_moved_while_the_dead_space_is_above_the_target() {
    let db = churned(13);
    let measure = CleanerState::new(moving_only());
    let space = Cleaner::new(db.wb(), &measure).space().expect("the space");
    let dead = space.dead_percent();
    assert!(dead > 40.0 && dead < 95.0, "{dead}");

    let lenient = CleanerState::new(CleanerConfig {
        target_dead_percent: 96,
        ..moving_only()
    });
    let pass = Cleaner::new(db.wb(), &lenient).pass().expect("a pass");
    assert_eq!((pass.blocks_moved, pass.blocks_lost), (0, 0), "{pass:?}");

    let half = CleanerState::new(CleanerConfig {
        target_dead_percent: 40,
        ..moving_only()
    });
    let cleaner = Cleaner::new(db.wb(), &half);
    settle(&cleaner);
    let after = cleaner.space().expect("the space");
    check_space(&db, &after);
    assert!(after.dead_percent() <= 40.0, "{}", describe(&db, &after));
    assert!(
        after.dead_bytes() > 0,
        "more was moved than the target asks"
    );
    db.check_cold();
}

/// How many blocks an eager cleaner with no throttle moves in a store
/// that is made as `db` was: the same store, made again.
fn settle_count(db: &Db) -> u64 {
    let again = churned(13);
    again.versions.pin(again.latest());
    for round in 14..=18 {
        churn(&again, round);
    }
    assert_eq!(links_of(&again).len(), links_of(db).len());
    let state = CleanerState::new(moving_only());
    settle(&Cleaner::new(again.wb(), &state)).blocks_moved
}
