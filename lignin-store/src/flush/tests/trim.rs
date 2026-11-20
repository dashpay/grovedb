//! When a pass trims the chain of a page it gave a base image: once no
//! version pin and no running operation reads below that image
//! (`transactions.md` RD4, PN5, PN6).

use super::*;

/// A store in which every block is a base image, as every block of a flat
/// page is, with page 1 committed and written back. Returns the version
/// of that commit.
fn written() -> (Db, CommitVersion) {
    let db = Db::with(FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    });
    let first = db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    (db, first)
}

/// What a reader at `at` is given of page 1 when the pool does not hold
/// the page: what the page table and the segments give.
fn cold(db: &Db, at: CommitVersion) -> Result<Vec<Cell>, LoadError> {
    let pool = new_pool(db.size);
    let frame = db.loader(&pool).get(page(1), at, &mut LoadBuffers::new())?;
    Ok(db.cells(page(1), &frame, None))
}

/// Checks that such a reader is given page 1 as the commits left it at
/// `at`.
fn reads(db: &Db, at: CommitVersion) {
    let found = cold(db, at).unwrap_or_else(|error| panic!("page 1 at version {at}: {error}"));
    let model = db.model.lock().expect("the model");
    let (_, expected) = model.at(page(1), at).expect("a version of page 1");
    assert!(
        same(&found, expected),
        "page 1 at version {at}: {found:?}, not {expected:?}"
    );
}

/// How many pages wait for a trim, and how many answers about the oldest
/// pin wait for their operations.
fn waiting(db: &Db) -> (usize, usize) {
    let files = db.state.files.lock().expect("the files");
    (files.untrimmed.len(), files.trim_bound.waiting())
}

/// The window: an operation holds a guard and no pin, and reads the
/// latest version of its beginning. A commit supersedes a page that the
/// operation has not looked up yet, and a pass writes the new image and
/// trims. The link of the operation's version must still be there when it
/// looks the page up.
#[test]
fn an_operation_that_pins_nothing_reads_its_version_after_a_pass_wrote_a_newer_image() {
    let (db, read_at) = written();
    db.versions.hold_guard();
    assert_eq!(db.latest(), read_at);

    let newer = db.commit(&[hash((1, 1), 7)]);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));

    reads(&db, read_at);
    reads(&db, newer);
    assert_eq!(db.shape(page(1)), "BB");
    assert_eq!(db.dead_blocks(), 0);
    db.versions.release_guard();
}

/// The first control: the same operation and the same commit with no pass
/// between them.
#[test]
fn an_operation_that_pins_nothing_reads_its_version_after_a_commit() {
    let (db, read_at) = written();
    db.versions.hold_guard();
    db.commit(&[hash((1, 1), 7)]);
    reads(&db, read_at);
    assert_eq!(db.shape(page(1)), "B");
    db.versions.release_guard();
}

/// The second control: a reader with a version pin, which a trim has
/// always waited for.
#[test]
fn a_snapshot_reads_its_version_after_a_pass_wrote_a_newer_image() {
    let (db, read_at) = written();
    assert_eq!(db.versions.pin_latest(), read_at);
    let newer = db.commit(&[hash((1, 1), 7)]);
    db.wb().pass().expect("a pass");
    reads(&db, read_at);
    reads(&db, newer);
    assert_eq!(db.shape(page(1)), "BB");

    db.versions.unpin();
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(waiting(&db), (0, 0));
}

/// With no operation running, nothing waits: the pass that writes the
/// image trims, and the version before it has no link any more.
#[test]
fn the_pass_that_wrote_the_image_trims_when_no_operation_runs() {
    let (db, read_at) = written();
    let newer = db.commit(&[hash((1, 1), 7)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.chain(page(1))[0].version, newer);
    assert_eq!(db.dead_blocks(), 1);
    assert_eq!(waiting(&db), (0, 0));
    assert!(
        matches!(cold(&db, read_at), Err(LoadError::NoPage { .. })),
        "the version before the image was trimmed"
    );
    db.check_cold();
}

/// A trim that waited is made by the first pass after the guard is
/// released, also by a pass that has nothing to write: nothing is left
/// untrimmed for good.
#[test]
fn a_trim_that_waited_for_a_guard_is_made_by_a_later_pass() {
    let (db, read_at) = written();
    db.versions.hold_guard();
    let newer = db.commit(&[hash((1, 1), 7)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BB");
    assert_eq!(waiting(&db), (1, 1));

    // Another pass under the guard asks nothing new, and trims nothing.
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.written, 0);
    assert_eq!(db.shape(page(1)), "BB");
    assert_eq!(waiting(&db), (1, 1));
    reads(&db, read_at);

    db.versions.release_guard();
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.written, 0);
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.chain(page(1))[0].version, newer);
    assert_eq!(db.dead_blocks(), 1);
    assert_eq!(waiting(&db), (0, 0));
    assert!(matches!(cold(&db, read_at), Err(LoadError::NoPage { .. })));
    db.check_cold();
    db.check_chains();
}

/// The trim waits for the operations that were running when the pass
/// asked, and for no other: an operation that begins afterwards reads a
/// version at or after the image.
#[test]
fn an_operation_that_begins_after_the_pass_asked_does_not_hold_the_trim_back() {
    let (db, _) = written();
    db.versions.hold_guard();
    let newer = db.commit(&[hash((1, 1), 7)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BB");

    // The first operation ends and another begins: it reads the latest
    // version of now.
    db.versions.release_guard();
    db.versions.hold_guard();
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    reads(&db, newer);
    assert_eq!(waiting(&db), (0, 0));
    db.versions.release_guard();
}

/// A guard that is never released stalls these trims as a pin does: every
/// version from the one its operation reads is kept, write-back keeps a
/// bounded number of answers, and backpressure counts none of it. Two
/// passes after it is released nothing waits any more.
#[test]
fn a_guard_that_is_not_released_holds_the_trims_and_nothing_else() {
    let (db, read_at) = written();
    db.versions.hold_guard();
    const ROUNDS: u8 = 12;
    for round in 0..ROUNDS {
        db.commit(&[hash((1, 1), round)]);
        let pass = db.wb().pass().expect("a pass");
        assert_eq!((pass.written, pass.waiting), (1, 0));
        assert!(db.wb().has_room());
    }
    assert_eq!(db.shape(page(1)), "B".repeat(1 + usize::from(ROUNDS)));
    assert_eq!(waiting(&db), (1, trim_bound::MAX_BOUNDS));
    assert_eq!(db.dead_blocks(), 0);
    for at in (read_at.get()..=db.latest().get()).map(version) {
        reads(&db, at);
    }

    db.versions.release_guard();
    db.wb().pass().expect("a pass");
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.dead_blocks(), u64::from(ROUNDS));
    assert_eq!(waiting(&db), (0, 0));
    db.check_cold();
}

/// Operations that follow one another without a gap, so that one is
/// running whenever a pass asks, and a page that gets an image in every
/// pass, as a flat page that every commit changes does. The page never
/// has an image within the version of the last answer when a pass ends,
/// and it is trimmed all the same: below the image that the operation of
/// that moment reads, one pass late.
#[test]
fn a_page_that_gets_an_image_in_every_pass_is_trimmed_under_operations_without_a_gap() {
    let (db, _) = written();
    for round in 0..12u8 {
        db.versions.hold_guard();
        let read_at = db.latest();
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
        reads(&db, read_at);
        reads(&db, db.latest());
        let chain = db.chain(page(1));
        assert_eq!(db.shape(page(1)), "BB", "in round {round}");
        assert_eq!(chain[0].version, read_at);
        assert_eq!(db.dead_blocks(), u64::from(round));
        db.versions.release_guard();
    }
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(waiting(&db), (0, 0));
    db.check_cold();
}

/// A page that waits is trimmed each time the version advances that
/// chains may be trimmed at, and not only once that version has reached
/// its newest image. Page 1 has images of four versions. While a
/// snapshot reads the first, all of them stay. Once the oldest snapshot
/// reads the second, the first has no reader and goes, though the page
/// still waits for the newest.
#[test]
fn a_page_that_waits_is_trimmed_each_time_the_version_advances() {
    let db = Db::with(FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    });
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.versions.hold_guard();
    db.wb().pass().expect("a pass");
    // A snapshot of a version in which page 1 is as the first commit left
    // it. Every version of the page after it gets an image, since the
    // snapshot is below the commit that superseded it.
    let pinned = db.commit(&[insert((2, 1), b"k", inline(b"v"))]);
    db.versions.pin(pinned);
    let second = db.commit(&[hash((1, 1), 3)]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((1, 1), 4)]);
    let newest = db.commit(&[hash((1, 1), 5)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BBBB");

    // The operation ends, and chains are trimmed at the snapshot's
    // version: it reads the first image.
    db.versions.release_guard();
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "BBBB");
    assert_eq!(waiting(&db), (1, 0));
    reads(&db, pinned);

    // That snapshot goes, and the oldest one left reads the second image.
    db.versions.pin(second);
    for _ in 0..2 {
        db.wb().pass().expect("a pass");
        assert_eq!(db.shape(page(1)), "BBB");
        assert_eq!(db.chain(page(1))[0].version, second);
        assert_eq!(db.dead_blocks(), 1);
        assert_eq!(waiting(&db), (1, 0));
    }
    for at in (second.get()..=newest.get()).map(version) {
        reads(&db, at);
    }

    db.versions.unpin();
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.chain(page(1))[0].version, newest);
    assert_eq!(db.dead_blocks(), 3);
    assert_eq!(waiting(&db), (0, 0));
    db.check_cold();
}
