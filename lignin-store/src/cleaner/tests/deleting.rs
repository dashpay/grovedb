//! The deletion of segments: retire first, reclaim later, and what holds
//! a segment back.

use super::*;

/// A segment with no live block that takes no more blocks is retired, and
/// goes once nothing needs it. The blocks that never had a link, or lost
/// it, are found dead with no rule of their own.
#[test]
fn a_segment_that_nothing_points_into_is_deleted() {
    let db = churned(13);
    let before = segment_ids(&db);
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let empty: Vec<FileId> = (cleaner.space().expect("the space").segments.iter())
        .filter(|segment| segment.live_blocks == 0 && !segment.has_writer)
        .map(|segment| segment.file)
        .collect();
    assert!(!empty.is_empty());
    let pass = cleaner.pass().expect("a pass");
    assert!(pass.retired >= empty.len(), "{pass:?}");
    assert_eq!(pass.retired, pass.dropped);
    assert_eq!(pass.waiting, 0);
    let after = segment_ids(&db);
    for file in &empty {
        assert!(
            !after.contains(file) && !on_disk(&db, *file),
            "segment {file}"
        );
    }
    assert!(after.len() < before.len());
    let stats = state.stats();
    assert_eq!(stats.segments_retired, stats.segments_dropped);
    assert!(stats.bytes_dropped > 0);
    db.check_cold();
    // The manifest names what is left, and the next session opens it.
    cleaner.finish().expect("closed");
    let db = db.reopen();
    assert_eq!(segment_ids(&db), after);
    db.check_cold();
}

/// A segment that waits for a running operation takes the room of the
/// copies as one that waits for a table checkpoint does: while it waits,
/// no more is moved than may wait at once.
#[test]
fn what_waits_for_an_operation_counts_against_the_room_for_copies() {
    let db = churned(13);
    // The segments that hold nothing live go first, with no operation
    // running: a cleaner that moves nothing deletes them.
    let emptied = CleanerState::new(CleanerConfig {
        target_dead_percent: 100,
        ..moving_only()
    });
    let swept = settle(&Cleaner::new(db.wb(), &emptied));
    assert!(swept.dropped > 0 && swept.blocks_moved == 0, "{swept:?}");

    let state = CleanerState::new(CleanerConfig {
        checkpoint_dead_percent: 10,
        ..moving_only()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let before = cleaner.space().expect("the space");
    let largest = (before.segments.iter().map(|segment| segment.block_bytes))
        .max()
        .expect("a segment");
    assert_eq!(before.retired_bytes(), 0);
    assert!(before.dead_bytes() > before.block_bytes() / 4, "{before:?}");

    db.versions.hold_guard();
    let done = settle(&cleaner);
    assert!(done.blocks_moved > 0 && done.retired > 0, "{done:?}");
    assert_eq!(done.dropped, 0, "an operation is still running");
    let waiting = cleaner.space().expect("the space");
    let room = (waiting.block_bytes() / 10).max(largest);
    assert!(
        waiting.retired_bytes() <= room,
        "{} bytes wait, and there is room for {room}",
        waiting.retired_bytes()
    );
    // There was more to move: what is dead is not all in what waits.
    assert!(
        waiting.dead_bytes() > 2 * waiting.retired_bytes(),
        "{waiting:?}"
    );
    for _ in 0..3 {
        let pass = cleaner.pass().expect("a pass");
        assert_eq!((pass.blocks_moved, pass.dropped), (0, 0), "{pass:?}");
    }

    // The operation ends: what waited goes, and the rest is moved.
    db.versions.release_guard();
    let rest = settle(&cleaner);
    assert!(
        rest.blocks_moved > 0 && rest.dropped > done.retired,
        "{rest:?}"
    );
    let space = cleaner.space().expect("the space");
    assert_eq!(space.retired_bytes(), 0);
    db.check_cold();
}

/// **Test T-PN5.** A segment is not deleted while an operation that was
/// running when it was retired is running: the operation may have read a
/// location in it (PN5, PN6). A pin alone holds nothing back here.
#[test]
fn a_segment_waits_for_the_operations_that_were_running_when_it_was_retired() {
    let db = churned(13);
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    // An operation reads the location of a block, and is then held up.
    let (id, read) = movable(&db, &cleaner.space().expect("the space"));
    db.versions.hold_guard();
    let mut buf = Vec::new();
    let bytes = (db.segments.read(read, db.size.get(), &mut buf))
        .map(|block| block.payload().expect("a payload").into_owned())
        .expect("the block");

    // The cleaner moves every block it can, and retires the segments.
    let done = settle(&cleaner);
    assert!(done.blocks_moved > 0 && done.retired > 0, "{done:?}");
    assert_eq!(done.dropped, 0, "an operation is still running");
    assert_eq!(done.waiting, done.retired);
    assert!(
        db.chain(id)
            .iter()
            .all(|link| link.location() != Some(read)),
        "the block was not moved"
    );
    let space = cleaner.space().expect("the space");
    assert!(space.segment(read.file).expect("the segment").retired);
    assert!(space.retired_bytes() > 0);
    assert_eq!(space.retired_bytes(), {
        let retired = space.segments.iter().filter(|segment| segment.retired);
        retired.map(|segment| segment.block_bytes).sum::<u64>()
    });
    // The operation reads what it read before, from where it was.
    let again = (db.segments.read(read, db.size.get(), &mut buf))
        .map(|block| block.payload().expect("a payload").into_owned())
        .expect("the block is still there");
    assert_eq!(again, bytes);
    assert!(on_disk(&db, read.file));
    // More passes change nothing, and neither does a pin.
    db.versions.pin(db.latest());
    assert_eq!(settle(&cleaner).dropped, 0);
    db.versions.unpin();

    // The operation ends, and the segments go.
    db.versions.release_guard();
    let done = settle(&cleaner);
    assert!(done.dropped > 0 && done.waiting == 0, "{done:?}");
    assert!(!on_disk(&db, read.file));
    assert!(db.segments.read(read, db.size.get(), &mut buf).is_err());
    db.check_cold();
}

/// A segment that the table checkpoint in force points into is not
/// deleted: a recovery reads it (`page-table.md` K6). It goes once a
/// checkpoint that does not point into it is in force, and the cleaner
/// asks for one when enough waits.
#[test]
fn a_segment_the_checkpoint_in_force_points_into_waits_for_the_next_checkpoint() {
    let db = churned(13);
    let (covers, ..) = crate::flush::tests::installed(db.wb().checkpoint().expect("one"));
    assert_eq!(covers, db.latest());
    let in_checkpoint = db.state.checkpoint_segments().expect("a checkpoint");
    assert!(!in_checkpoint.is_empty());
    for round in 14..=16 {
        churn(&db, round);
    }

    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let done = settle(&cleaner);
    assert!(done.retired > 0, "{done:?}");
    // What the checkpoint points into stays, with its file.
    let left = segment_ids(&db);
    for file in &in_checkpoint {
        assert!(left.contains(file) && on_disk(&db, *file), "segment {file}");
    }
    assert!(done.waiting >= 1, "{done:?}");
    assert_eq!(state.stats().checkpoints, 0);
    // A crash now recovers from that checkpoint, and reads those blocks.
    let recovered = crate::flush::tests::recover::recover(db.vfs.clone(), &db.path, &[page(1)]);
    assert!(recovered.checkpoint);

    // The eager cleaner asks for the checkpoint, and the segments go.
    let eager_state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &eager_state);
    let pass = cleaner.pass().expect("a pass");
    assert!(
        pass.checkpoints >= 1 && pass.checkpoint.is_none(),
        "{pass:?}"
    );
    assert_eq!(eager_state.stats().checkpoints, pass.checkpoints);
    settle(&cleaner);
    let now = db.state.checkpoint_segments().expect("a checkpoint");
    let left = segment_ids(&db);
    for file in &in_checkpoint {
        assert_eq!(left.contains(file), now.contains(file), "segment {file}");
        assert_eq!(on_disk(&db, *file), now.contains(file), "segment {file}");
    }
    assert!(in_checkpoint.iter().any(|file| !left.contains(file)));
    db.check_cold();
    cleaner.finish().expect("closed");
    db.reopen().check_cold();
}

/// Before write-back was told which checkpoint the manifest names, the
/// cleaner deletes nothing: it does not know what a recovery reads.
#[test]
fn nothing_is_deleted_before_the_checkpoint_in_force_is_known() {
    let db = churned(13);
    let untold = crate::flush::FlushState::new(small_segments());
    let stores = db.wb().stores();
    let write_back = crate::flush::WriteBack::new(stores, &untold, db.size).expect("a view");
    assert_eq!(untold.checkpoint_segments(), None);
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(write_back, &state);
    let pass = cleaner.pass().expect("a pass");
    assert!(pass.retired > 0 && pass.dropped == 0, "{pass:?}");
    assert_eq!(pass.waiting, pass.retired);
    db.check_cold();
}

/// **Test T-CK3, the cleaner's part.** While a checkpoint of the database
/// that is being created holds the files, no segment is taken out of the
/// store and none is deleted (CK3). Afterwards they go.
#[test]
fn nothing_is_deleted_while_a_checkpoint_being_created_holds_the_files() {
    let db = churned(13);
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let before = segment_ids(&db);
    // The sampling step: under the manifest's lock.
    let hold = {
        let _manifest = db.manifest.lock().expect("the manifest");
        db.segments.hold_files()
    };
    let done = settle(&cleaner);
    assert!(done.retired > 0 && done.dropped == 0, "{done:?}");
    for file in &before {
        assert!(on_disk(&db, *file), "segment {file}");
    }
    assert!(segment_ids(&db).len() >= before.len());
    // A second hold, and the first one given back: still held.
    let second = db.segments.hold_files();
    drop(hold);
    assert_eq!(settle(&cleaner).dropped, 0);
    drop(second);
    let done = settle(&cleaner);
    assert!(done.dropped > 0 && done.waiting == 0, "{done:?}");
    db.check_cold();
}

/// A segment that was dropped just before a hold was taken keeps its file
/// until the hold is given back: the checkpoint sampled a manifest that
/// may still name it.
#[test]
fn a_dropped_segment_is_not_deleted_under_a_hold() {
    let db = churned(13);
    let state = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.space().expect("the space");
    let taken = |segment: &&SegmentSpace| !segment.has_writer;
    let file = (space.segments.iter().filter(taken))
        .find(|segment| segment.live_blocks == 0)
        .map(|segment| segment.file)
        .expect("an empty segment");
    let other = (space.segments.iter().filter(taken))
        .find(|segment| segment.file != file)
        .map(|segment| segment.file)
        .expect("another segment");
    db.segments.drop_segment(file).expect("dropped");
    let hold = db.segments.hold_files();
    {
        let mut manifest = db.manifest.lock().expect("the manifest");
        db.segments.commit(&mut manifest).expect("a manifest");
    }
    assert!(on_disk(&db, file), "the file went under a hold");
    assert!(matches!(
        db.segments.drop_segment(other),
        Err(crate::segment::SegmentStoreError::SegmentHeld { file }) if file == other
    ));
    drop(hold);
    {
        let mut manifest = db.manifest.lock().expect("the manifest");
        db.segments.commit(&mut manifest).expect("a manifest");
    }
    assert!(!on_disk(&db, file));
    // The cleaner finds the store as it is, and goes on.
    settle(&cleaner);
    db.check_cold();
}

/// The segment that write-back fills is not cleaned while it is written.
/// When what is dead in it is too much, the cleaner has it sealed, and
/// then cleans it.
#[test]
fn the_segment_being_filled_is_sealed_when_too_much_of_it_is_dead() {
    let config = FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    db.commit(&[insert((1, 1), b"a", inline(&noise(1, 150)))]);
    db.wb().pass().expect("a pass");
    for round in 1..=8 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().pass().expect("a pass");
    }
    assert_eq!(segment_ids(&db).len(), 1, "one segment, never full");

    let patient = CleanerState::new(moving_only());
    let cleaner = Cleaner::new(db.wb(), &patient);
    let pass = cleaner.pass().expect("a pass");
    assert!(pass.sealed.is_empty() && pass.blocks_moved == 0, "{pass:?}");
    let space = cleaner.space().expect("the space");
    assert!(space.dead_percent() > 80.0, "{}", space.dead_percent());
    assert!(space.segments[0].has_writer);

    // A segment that is too small to be worth sealing is left as well.
    let large = CleanerState::new(CleanerConfig {
        min_segment_len: 1 << 20,
        ..eager()
    });
    let pass = Cleaner::new(db.wb(), &large).pass().expect("a pass");
    assert!(pass.sealed.is_empty(), "{pass:?}");

    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass().expect("a pass");
    assert_eq!(pass.sealed, [FileId::MIN], "{pass:?}");
    assert_eq!(state.stats().segments_sealed, 1);
    let done = settle(&cleaner);
    assert_eq!((done.blocks_moved, done.dropped), (1, 1), "{done:?}");
    let space = cleaner.space().expect("the space");
    assert_eq!(space.dead_bytes(), 0, "{space:?}");
    assert_eq!(space.segments.len(), 1);
    db.check_cold();
    // Write-back goes on in a new segment.
    db.commit(&[hash((1, 1), 77)]);
    db.wb().pass().expect("a pass");
    assert_eq!(segment_ids(&db).len(), 2);
    db.check_cold();
}

/// A pass that finds the store as the pass before left it reads nothing,
/// and a change wakes it.
#[test]
fn a_pass_over_a_store_that_has_not_changed_reads_nothing() {
    let db = churned(13);
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    settle(&cleaner);
    let before = state.stats();
    for _ in 0..5 {
        let pass = cleaner.pass().expect("a pass");
        assert!(!pass.scanned && !pass.progressed(), "{pass:?}");
    }
    assert_eq!(state.stats().scans, before.scans);
    assert_eq!(state.stats().passes, before.passes + 5);

    db.commit(&[hash((1, 1), 99)]);
    db.wb().pass().expect("a pass");
    assert!(cleaner.pass().expect("a pass").scanned);
    settle(&cleaner);
    // A pin on the latest version holds nothing back, and is no change.
    // A pin that falls behind is one, and so is its going: what it kept
    // can be trimmed.
    db.versions.pin(db.latest());
    assert!(!cleaner.pass().expect("a pass").scanned);
    db.commit(&[hash((1, 1), 98)]);
    db.wb().pass().expect("a pass");
    settle(&cleaner);
    let scans = state.stats().scans;
    assert!(!cleaner.pass().expect("a pass").scanned);
    db.versions.unpin();
    assert!(cleaner.pass().expect("a pass").scanned);
    assert_eq!(state.stats().scans, scans + 1);
    db.check_cold();
}
