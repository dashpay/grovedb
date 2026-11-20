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

/// A copy whose link was gone when it was to be switched was written all
/// the same, and counts against the throttle: a pass does not go on
/// copying because its copies come to nothing.
#[test]
fn a_copy_whose_link_is_gone_counts_against_the_throttle() {
    let db = std::sync::Arc::new(churned(13));
    db.versions.pin(db.latest());
    for round in 14..=16 {
        churn(&db, round);
    }
    // Fewer bytes than the smallest block: one copy to a pass.
    let state = CleanerState::new(CleanerConfig {
        max_pass_bytes: 40,
        ..moving_only()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let raced = db.clone();
    let mut copied = 0;
    let meanwhile = super::super::state::AtSwitch(Box::new(move |id, _| {
        copied += 1;
        assert_eq!(copied, 1, "a second block was copied in the pass");
        raced.versions.unpin();
        for byte in 51..=53 {
            raced.commit(&[hash((id.get(), 1), byte)]);
            raced.wb().pass().expect("a pass");
        }
    }));
    *state.at_switch.lock().expect("the hook") = Some(meanwhile);
    let pass = cleaner.pass().expect("a pass");
    *state.at_switch.lock().expect("the hook") = None;
    assert_eq!((pass.blocks_lost, pass.blocks_moved), (1, 0), "{pass:?}");
    assert!(pass.bytes_lost > 40 && pass.more, "{pass:?}");
    assert_eq!(state.stats().bytes_lost, pass.bytes_lost);
    // The passes after it finish the work.
    let done = settle(&cleaner);
    assert!(done.blocks_moved > 0, "{done:?}");
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

/// What the links of a chain are, without their places.
fn kinds(chain: &[Link]) -> Vec<(CommitVersion, char)> {
    let kind = |link: &Link| match link.kind {
        crate::table::LinkKind::Base(_) => 'B',
        crate::table::LinkKind::Delta(_) => 'd',
        crate::table::LinkKind::Free => 'F',
    };
    chain
        .iter()
        .map(|link| (link.version, kind(link)))
        .collect()
}

/// A page that a commit frees while one of its blocks is copied stays
/// freed. A move adds no link: it points the link the block had at the
/// copy, which is what the readers below the freeing version read by, and
/// the free link stays the newest of the chain (the owner's decision
/// D-116).
#[test]
fn a_page_that_is_freed_while_its_block_is_copied_stays_freed() {
    let db = std::sync::Arc::new(churned(13));
    let pinned = db.latest();
    db.versions.pin(pinned);
    for round in 14..=16 {
        churn(&db, round);
    }
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let freed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (seen, raced) = (freed.clone(), db.clone());
    let meanwhile = super::super::state::AtSwitch(Box::new(move |id, from| {
        if seen.lock().expect("seen").is_some() {
            return;
        }
        let before = raced.chain(id);
        raced.commit(&[Op::Free { page: id }]);
        raced.wb().pass().expect("a pass");
        *seen.lock().expect("seen") = Some((id, from, before));
    }));
    *state.at_switch.lock().expect("the hook") = Some(meanwhile);
    let pass = cleaner.pass().expect("a pass");
    *state.at_switch.lock().expect("the hook") = None;

    let (id, from, before) = (freed.lock().expect("seen").take()).expect("a block was copied");
    let after = db.chain(id);
    // The chain has what it had and the free link, which is its newest.
    let mut expected = kinds(&before);
    expected.push((db.latest(), 'F'));
    assert_eq!(kinds(&after), expected);
    assert_eq!(db.table.newest(id), Some(Link::free(db.latest())));
    // The block was moved, for the reader that is below the freeing
    // version: its link points at the copy.
    assert_eq!(pass.blocks_lost, 0, "{pass:?}");
    assert!(after.iter().all(|link| link.location() != Some(from)));
    let cold = crate::flush::tests::new_pool(db.size);
    let loader = crate::load::PageLoader::new(&cold, &db.table, &db.segments, db.size);
    let loader = loader.expect("a loader");
    assert!(loader.get(id, pinned, &mut Default::default()).is_ok());
    let gone = loader.get(id, db.latest(), &mut Default::default());
    assert!(matches!(gone, Err(crate::load::LoadError::NoPage { .. })));
    db.check_warm();
    db.check_cold();

    // With no reader below the freeing version, the chain goes whole.
    db.versions.unpin();
    settle(&cleaner);
    assert_eq!(db.shape(id), "");
    db.check_cold();
}

/// The same for a page that was made under the id of a freed page and
/// that no block holds yet: the commit that frees it again finds the free
/// link of the page before it and pushes no second one. A block of the
/// page before it, which a reader below that free link still reads, is
/// moved meanwhile, and the id is left with no link that says it is in
/// use (D-116).
#[test]
fn a_move_gives_no_link_to_an_id_that_is_freed_again_before_it_was_written_back() {
    let db = std::sync::Arc::new(Db::with(small_segments()));
    // The image of page 1 lies beside one of page 3 that dies.
    db.commit(&[
        insert((1, 1), b"a", inline(&noise(1, 120))),
        insert((3, 1), b"c", inline(&noise(3, 120))),
    ]);
    db.wb().pass().expect("a pass");
    for round in 1..=3 {
        db.commit(&[hash((3, 1), round)]);
        db.wb().pass().expect("a pass");
    }
    let pinned = db.latest();
    db.versions.pin(pinned);
    db.commit(&[Op::Free { page: page(1) }]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BF");
    // The id names another page, which only the pool and the log hold.
    db.commit(&[insert((1, 1), b"again", inline(&noise(8, 90)))]);
    let made = db.latest();

    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let hit = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (seen, raced) = (hit.clone(), db.clone());
    let meanwhile = super::super::state::AtSwitch(Box::new(move |id, from| {
        if id == page(1) && seen.lock().expect("seen").is_none() {
            raced.commit(&[Op::Free { page: id }]);
            *seen.lock().expect("seen") = Some(from);
        }
    }));
    *state.at_switch.lock().expect("the hook") = Some(meanwhile);
    let pass = cleaner.pass().expect("a pass");
    *state.at_switch.lock().expect("the hook") = None;
    let from = hit
        .lock()
        .expect("seen")
        .expect("the block of page 1 was copied");
    assert!(pass.blocks_moved >= 1, "{pass:?}");

    // The chain is what it was, in another place: an image and the free
    // link of the first page. Nothing says that the id is in use.
    let chain = db.chain(page(1));
    assert_eq!(kinds(&chain), [(version(1), 'B'), (version(5), 'F')]);
    assert_ne!(chain[0].location(), Some(from));
    // Write-back owes nothing of the page that was made and freed, and a
    // pass leaves the chain as it is.
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BF");
    assert_eq!(
        db.model
            .lock()
            .expect("the model")
            .at(page(1), made)
            .map(|at| at.0),
        Some(6)
    );
    db.check_warm();
    db.versions.unpin();
    settle(&cleaner);
    assert_eq!(db.shape(page(1)), "");
    db.check_warm();
}
