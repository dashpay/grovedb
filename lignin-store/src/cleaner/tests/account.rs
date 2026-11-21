//! The accounting and the trimming: what is live, what a pin keeps, and
//! which chains a pass shortens.

use super::*;

/// The accounting agrees with a walk over every segment and a read of
/// every chain, block for block, while pages are written again and again.
#[test]
fn a_block_is_live_while_a_link_points_to_it() {
    let db = churned(13);
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    assert!(
        space.segments.len() > 3,
        "{} segments",
        space.segments.len()
    );
    // Write-back trimmed what it superseded, so most of what the sealed
    // segments hold is dead, and nothing is pinned.
    assert!(space.dead_bytes() > space.live_bytes(), "{space:?}");
    assert_eq!(space.pinned_bytes(), 0);
    assert_eq!(space.block_bytes(), space.live_bytes() + space.dead_bytes());
    let share = space.dead_bytes() as f64 * 100.0 / space.block_bytes() as f64;
    assert!((space.dead_percent() - share).abs() < 1e-9);
    assert_eq!(space.oldest_pin, Some(db.latest()));
    // The read changed nothing.
    assert_eq!(state.stats(), CleanerStats::default());
    assert_eq!(state.space(), Space::default());
}

/// A store with no block has no dead space, and a share of nothing is 0.
#[test]
fn a_store_with_no_block_has_no_dead_space() {
    let db = Db::with(small_segments());
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    assert!(space.segments.is_empty());
    assert_eq!((space.block_bytes(), space.dead_percent()), (0, 0.0));
    assert!(space.segment(FileId::MIN).is_none());
    let pass = cleaner.pass().expect("a pass");
    assert!(pass.scanned && !pass.progressed(), "{pass:?}");
}

/// The footer of a sealed segment is no block: a segment whose blocks all
/// have their links has no dead byte, sealed or not.
#[test]
fn headers_and_footers_are_not_counted() {
    let db = Db::with(small_segments());
    for id in 1..=6 {
        db.commit(&[insert((id, 1), b"key", inline(&noise(u64::from(id), 200)))]);
    }
    db.wb().pass().expect("a pass");
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    let sealed = space
        .segments
        .iter()
        .filter(|segment| segment.state == FileState::Sealed)
        .count();
    assert!(sealed >= 1 && sealed < space.segments.len(), "{space:?}");
    assert_eq!(space.dead_bytes(), 0);
    for segment in &space.segments {
        let extent = db.segments.extents();
        let end = extent
            .iter()
            .find(|extent| extent.file == segment.file)
            .expect("the segment")
            .end;
        let sealed = segment.state == FileState::Sealed;
        assert_eq!(end - 20 > segment.block_bytes, sealed, "{segment:?}");
        assert_eq!(segment.has_writer, !sealed);
    }
    // Nothing is dead, so a pass moves nothing and retires nothing.
    let pass = cleaner.pass().expect("a pass");
    assert_eq!((pass.blocks_moved, pass.retired), (0, 0));
}

/// A version pin keeps the links that readers at it are given, and the
/// accounting says how many bytes that is (PN7). They are live: the
/// cleaner moves them like any other block and trims nothing a pinned
/// reader needs.
#[test]
fn what_a_pin_keeps_is_live_and_is_reported() {
    let db = Db::with(small_segments());
    db.commit(&[insert((1, 1), b"a", inline(&noise(1, 150)))]);
    db.wb().pass().expect("a pass");
    db.versions.pin(db.latest());
    for round in 1..=6 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
    }
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    // The page has two base images and runs between them. A reader at the
    // latest version needs the newest image alone.
    assert_eq!(db.shape(page(1)), "BddBddB");
    let newest = db
        .chain(page(1))
        .last()
        .and_then(Link::location)
        .expect("an image");
    assert_eq!(
        space.pinned_bytes(),
        space.live_bytes() - u64::from(newest.len)
    );
    assert_eq!(space.oldest_pin, Some(version(1)));

    let done = settle(&cleaner);
    assert_eq!(done.links_trimmed, 0, "{done:?}");
    assert_eq!(db.shape(page(1)), "BddBddB");
    db.check_cold();

    // Once the pin is gone, the links go, and with them what they kept.
    db.versions.unpin();
    let done = settle(&cleaner);
    assert_eq!(done.links_trimmed, 6, "{done:?}");
    assert_eq!(db.shape(page(1)), "B");
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    assert_eq!(space.pinned_bytes(), 0);
    db.check_cold();
}

/// The chain of a freed page goes whole once no reader is below the
/// freeing version, and not before (`page-table.md` T11). Write-back does
/// not trim it: the page gets no base image any more.
#[test]
fn the_chain_of_a_freed_page_goes_once_no_reader_can_ask_for_the_page() {
    let db = Db::with(small_segments());
    db.commit(&[
        insert((1, 1), b"a", inline(&noise(1, 150))),
        insert((2, 1), b"b", inline(&noise(2, 150))),
    ]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((1, 1), 1)]);
    db.wb().pass().expect("a pass");
    let before = db.latest();
    db.versions.pin(before);
    db.commit(&[Op::Free { page: page(1) }]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BdF");

    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass().expect("a pass");
    assert_eq!(pass.links_trimmed, 0, "a reader at {before} reads the page");
    assert_eq!(db.shape(page(1)), "BdF");
    assert_eq!(db.table.trimmed_free(), None);
    db.check_warm();

    db.versions.unpin();
    let pass = cleaner.pass().expect("a pass");
    assert_eq!(pass.links_trimmed, 2, "{pass:?}");
    assert_eq!(db.shape(page(1)), "");
    assert_eq!(db.table.trimmed_free(), Some(db.latest()));
    assert_eq!(state.stats().links_trimmed, 2);
    assert!(state.stats().bytes_trimmed > 0);
    // Write-back counts them as dead, as it counts its own trims.
    let dead: u64 = (db.state.dead_space().values())
        .map(|dead| dead.blocks)
        .sum();
    assert_eq!(dead, 2);
    check_space(&db, &cleaner.space().expect("the space"));
    db.check_warm();
    db.check_cold();

    // The id is used again, and the new page is a page like any other.
    db.commit(&[insert((1, 1), b"again", inline(&noise(7, 90)))]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    settle(&cleaner);
    db.check_cold();
}

/// A free link under the base image of a page that was made again under
/// its id goes with what is older, and the newer page stays.
#[test]
fn a_free_link_under_a_newer_page_is_trimmed_with_what_is_older() {
    let db = Db::with(small_segments());
    db.commit(&[insert((1, 1), b"a", inline(&noise(1, 150)))]);
    db.wb().pass().expect("a pass");
    db.versions.pin(db.latest());
    db.commit(&[Op::Free { page: page(1) }]);
    db.commit(&[insert((1, 1), b"b", inline(&noise(2, 150)))]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BFB");
    db.versions.unpin();
    // Write-back's own trim comes with its next pass, for the page it
    // gave an image. The cleaner's pass comes first here.
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass().expect("a pass");
    assert_eq!(pass.links_trimmed, 1, "{pass:?}");
    assert_eq!(db.shape(page(1)), "B");
    db.check_cold();
}

/// The links that an earlier session left under a newer base image are
/// trimmed: write-back remembers the chains it has to trim for a session
/// only.
#[test]
fn chains_that_an_earlier_session_left_are_trimmed() {
    let db = Db::with(small_segments());
    db.commit(&[insert((1, 1), b"a", inline(&noise(1, 150)))]);
    db.wb().pass().expect("a pass");
    db.versions.pin(db.latest());
    for round in 1..=3 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
    }
    assert_eq!(db.shape(page(1)), "BddB");
    // The session ends with its reader, and the next one has none.
    let db = db.reopen();
    assert_eq!(db.shape(page(1)), "BddB");
    db.wb().pass().expect("a pass");
    assert_eq!(
        db.shape(page(1)),
        "BddB",
        "write-back has nothing to trim by"
    );

    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass().expect("a pass");
    assert_eq!(pass.links_trimmed, 3, "{pass:?}");
    assert_eq!(db.shape(page(1)), "B");
    db.check_cold();
}

/// What a trim at a version removes of a chain, link by link.
#[test]
fn a_trim_is_foretold_as_the_table_makes_it() {
    use crate::table::PageTable;
    let at = |offset: u64| BlockLocation {
        file: FileId::MIN,
        offset,
        len: 40,
    };
    let chains: [&[Link]; 7] = [
        &[],
        &[Link::base(version(1), at(20))],
        &[
            Link::base(version(1), at(20)),
            Link::delta(version(2), at(60)),
        ],
        &[
            Link::base(version(1), at(20)),
            Link::delta(version(2), at(60)),
            Link::base(version(2), at(100)),
            Link::delta(version(4), at(140)),
        ],
        &[Link::base(version(1), at(20)), Link::free(version(3))],
        &[
            Link::base(version(1), at(20)),
            Link::free(version(3)),
            Link::base(version(5), at(60)),
        ],
        &[Link::free(version(2))],
    ];
    for chain in chains {
        for oldest in (0..=6).map(version) {
            let table = PageTable::new();
            for link in chain {
                table.push(page(9), *link).expect("a chain");
            }
            let mut removed = Vec::new();
            table.trim(page(9), oldest, &mut removed);
            let mut left = Vec::new();
            table.chain(page(9), &mut left);
            let foretold = super::super::space::trimmable(chain, oldest);
            assert_eq!(
                &chain[foretold..],
                &left[..],
                "{chain:?} trimmed at {oldest}"
            );
        }
    }
}

#[test]
#[ignore = "prints the fixture"]
fn print_the_fixture() {
    let db = churned(13);
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    println!("{}", describe(&db, &space));
    println!("{:?}", settle(&cleaner));
    let space = cleaner.space().expect("the space");
    println!("{}", describe(&db, &space));
}

/// A chain is trimmed only once the operations that were running when the
/// oldest pin was found to allow it have ended (PN6). An operation that
/// holds no pin reads the latest version of its beginning, and has not
/// looked up every page it will read.
#[test]
fn a_trim_waits_for_the_operations_that_were_running() {
    let db = Db::with(small_segments());
    db.commit(&[insert((1, 1), b"a", inline(&noise(1, 150)))]);
    db.wb().pass().expect("a pass");
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    assert_eq!(cleaner.pass().expect("a pass").links_trimmed, 0);

    // An operation begins and reads the latest version. Then a commit
    // frees the page, and nothing is pinned.
    let reads_at = db.latest();
    db.versions.hold_guard();
    db.commit(&[Op::Free { page: page(1) }]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.versions.oldest_pin(), db.latest());
    for _ in 0..6 {
        let pass = cleaner.pass().expect("a pass");
        assert_eq!(pass.links_trimmed, 0, "an operation reads at {reads_at}");
        assert!(pass.scanned, "the cleaner waits, and looks again");
    }
    assert_eq!(db.shape(page(1)), "BF");
    // The operation looks the page up at its version, with nothing of it
    // in memory, and finds it.
    let cold = crate::flush::tests::new_pool(db.size);
    let loader = crate::load::PageLoader::new(&cold, &db.table, &db.segments, db.size);
    let found = (loader.expect("a loader")).get(page(1), reads_at, &mut Default::default());
    assert_eq!(found.expect("the page at its version").born(), reads_at);

    db.versions.release_guard();
    let pass = cleaner.pass().expect("a pass");
    assert_eq!(pass.links_trimmed, 1, "{pass:?}");
    assert_eq!(db.shape(page(1)), "");
    // An answer about the oldest pin that is lower than one before it
    // allows nothing more, and starts no epoch.
    let epoch = db.versions.next_epoch();
    db.versions.pin(version(1));
    cleaner.pass().expect("a pass");
    db.versions.unpin();
    cleaner.pass().expect("a pass");
    assert_eq!(db.versions.next_epoch().get(), epoch.get() + 1);
}
