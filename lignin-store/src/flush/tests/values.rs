//! Pending values: where a value gets its pointer (`transactions.md` FL7,
//! PV7; test T-VL5).

use super::*;

fn live_values(db: &Db) -> u64 {
    db.values.counts().iter().map(|count| count.values).sum()
}

/// A commit that writes values too long for their cells appends to the log
/// only. Write-back then appends each value to the value log, has a
/// manifest cover it, gives its cell the pointer and counts it, once.
#[test]
fn a_pending_value_is_placed_and_counted_once() {
    let db = Db::new();
    let named_before = db.values.files_named().expect("the files");
    db.commit(&[
        insert((1, 1), b"a", long(1, 2000)),
        insert((1, 2), b"b", long(2, 5000)),
        insert((2, 1), b"c", long(3, 1000)),
        insert((2, 2), b"d", inline(b"short")),
    ]);
    // The commit wrote no value-log file and replaced no manifest (CM7).
    assert_eq!(db.values.files_named().expect("the files"), named_before);
    assert_eq!(db.pending(page(1)).len(), 2);
    assert_eq!(live_values(&db), 0);
    assert_eq!(db.pool.attached_bytes(), 8000);
    db.check_warm();

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed, pass.waiting), (2, 3, 0));
    assert!(db.pending(page(1)).is_empty() && db.pending(page(2)).is_empty());
    assert_eq!(live_values(&db), 3);
    assert_eq!(db.pool.attached_bytes(), 0);
    for pointer in db.pointers(page(1)).into_iter().chain(db.pointers(page(2))) {
        assert!(db.values.covers(pointer), "{pointer:?} is covered");
    }
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (3, 3));
    assert_eq!(stats.value_bytes, 8000);
    db.check_warm();
    db.check_cold();

    // A second pass has nothing to do.
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass, Pass::default());
    assert_eq!(db.stats(), stats);
}

/// A value is not placed while a write transaction holds a private copy of
/// its page (PV7, check 3). It is appended and covered once, and its step
/// is made when the copy is gone: here the transaction rolls back.
#[test]
fn a_value_waits_for_a_private_copy_of_its_page_and_is_appended_once() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", long(1, 2000))]);
    let mut txn = db.begin();
    txn.copy(page(1));

    for _ in 0..3 {
        let pass = db.wb().pass().expect("a pass");
        assert_eq!((pass.written, pass.placed, pass.waiting), (0, 0, 1));
    }
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (1, 0));
    assert_eq!(stats.steps_put_off, 3);
    assert_eq!(live_values(&db), 0);
    assert_eq!(db.pending(page(1)).len(), 1);
    db.check_warm();

    txn.roll_back();
    assert_eq!(db.wb().place_ready().expect("placed"), 1);
    assert_eq!(live_values(&db), 1);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed, pass.waiting), (1, 0, 0));
    assert_eq!(db.stats().values_appended, 1);
    db.check_cold();
}

/// The transaction that held the copy commits. Its commit carries the
/// value on, still pending, and the call a committer makes once it is
/// published places it: in the page version that commit installed.
#[test]
fn a_value_is_placed_right_after_the_commit_that_carried_it_on() {
    let db = Db::new();
    db.commit(&[
        insert((1, 1), b"a", long(1, 2000)),
        insert((1, 2), b"b", inline(b"v")),
    ]);
    let mut txn = db.begin();
    txn.copy(page(1));
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.waiting), (0, 1));

    let second = txn.commit(&[hash((1, 2), 7)]);
    assert_eq!(db.pending(page(1)).len(), 1);
    assert_eq!(db.wb().place_ready().expect("placed"), 1);
    assert!(db.pending(page(1)).is_empty());
    assert_eq!(live_values(&db), 1);
    assert_eq!(db.frame(page(1), second).expect("the page").born(), second);

    // The version that was superseded with the value pending is never
    // written: it is kept for the readers that could still ask for it, and
    // the page's first block is the image of the version after it.
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.held, pass.waiting), (1, 1, 0));
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(db.chain(page(1))[0].version, second);
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (1, 1));
    assert_eq!(stats.held_for_readers, 1);
    db.check_cold();
}

/// A snapshot of a page version that was superseded with a pending value
/// keeps reading it, from the pool, until it ends (FL7; the choice of
/// `transactions.md` section 18, open point 3: it is kept in memory).
#[test]
fn a_snapshot_keeps_reading_a_version_that_was_superseded_with_a_pending_value() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    let mut txn = db.begin();
    txn.copy(page(1));
    let first = db.commit(&[insert((2, 1), b"b", long(4, 3000))]);
    txn.commit(&[hash((1, 1), 1)]);

    // A second transaction keeps page 2 from being placed, and supersedes
    // it with its value pending, while a snapshot reads the first version.
    let mut txn = db.begin();
    txn.copy(page(2));
    db.versions.pin(first);
    assert_eq!(db.wb().pass().expect("a pass").waiting, 1);
    let second = txn.commit(&[insert((2, 2), b"c", inline(b"w"))]);
    db.wb().place_ready().expect("placed");
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.held, pass.waiting), (1, 0));
    assert_eq!(db.shape(page(2)), "B");
    assert_eq!(db.chain(page(2))[0].version, second);
    assert_eq!(db.pool.census().held, 1);

    // The snapshot's version of page 2 is in no segment and still reads,
    // with its value, from the frame.
    db.check_warm();
    let old = db.frame(page(2), first).expect("the page of the snapshot");
    assert_eq!(old.born(), first);
    assert_eq!(old.attachments().count(), 1);
    drop(old);

    // When the snapshot ends, the pool lets the version go.
    db.versions.unpin();
    assert_eq!(db.pool.reclaim(&db.versions), 1);
    assert_eq!(db.pool.census().held, 0);
    db.check_cold();
}

/// A value whose cell is gone before its step is given up: nothing is
/// counted for it, and its bytes in the value log are dead space.
#[test]
fn a_value_whose_cell_goes_before_its_step_is_given_up() {
    let db = Db::new();
    db.commit(&[
        insert((1, 1), b"a", long(1, 2000)),
        insert((1, 2), b"b", inline(b"v")),
    ]);
    let mut txn = db.begin();
    txn.copy(page(1));
    db.wb().pass().expect("a pass");
    assert_eq!(db.stats().values_appended, 1);

    txn.commit(&[Op::Remove {
        page: page(1),
        slot: slot(1),
    }]);
    assert_eq!(db.wb().place_ready().expect("no step"), 0);
    let stats = db.stats();
    assert_eq!((stats.values_placed, stats.values_given_up), (0, 1));
    assert_eq!(live_values(&db), 0);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.waiting), (0, 0));
    db.check_cold();
}

/// A commit replaces a value that write-back has placed: its group
/// releases the pointer, and the new value is placed in its turn.
#[test]
fn a_placed_value_that_is_replaced_is_released_and_the_new_one_placed() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", long(1, 2000))]);
    db.wb().pass().expect("a pass");
    let old = db.pointers(page(1));
    assert_eq!(live_values(&db), 1);

    db.commit(&[Op::Value {
        page: page(1),
        slot: slot(1),
        value: long(9, 2500),
    }]);
    assert_eq!(live_values(&db), 0);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed), (1, 1));
    assert_eq!(live_values(&db), 1);
    assert_ne!(db.pointers(page(1)), old);
    assert_eq!(db.shape(page(1)), "Bd");
    db.check_cold();
}

/// Where a frame does not have the bytes of a pending value, the log has
/// them, and the pending value says where (`log-format.md` LG21).
#[test]
fn the_bytes_of_a_pending_value_come_from_the_log_if_its_frame_has_none() {
    let db = Db::new();
    let mut txn = db.begin();
    txn.values_in_log_only = true;
    txn.commit(&[insert((1, 1), b"a", long(5, 4000))]);
    assert_eq!(db.pool.attached_bytes(), 0);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed), (1, 1));
    db.check_cold();
}

/// Bytes that a frame holds for no cell are taken off it, in a step that
/// changes nothing the page says, before the page is written.
#[test]
fn bytes_attached_for_no_cell_are_taken_off_before_the_page_is_written() {
    let db = Db::new();
    let mut txn = db.begin();
    txn.attach_stray(page(1), [9; 8], b"left behind");
    txn.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    assert_eq!(db.pool.attached_bytes(), 11);

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!(db.pool.attached_bytes(), 0);
    db.check_cold();

    // While a transaction holds a copy, the step waits, like any other.
    let mut txn = db.begin();
    txn.attach_stray(page(3), [8; 8], b"more");
    txn.commit(&[insert((3, 1), b"a", inline(b"v"))]);
    let mut copier = db.begin();
    copier.copy(page(3));
    assert_eq!(db.wb().pass().expect("a pass").waiting, 1);
    // The copy shares the bytes, and they are counted for each frame.
    assert_eq!(db.pool.attached_bytes(), 8);
    copier.roll_back();
    assert_eq!(db.wb().pass().expect("a pass").waiting, 0);
    assert_eq!(db.pool.attached_bytes(), 0);
    db.check_cold();
}

/// Backpressure: a committer waits while write-back is over its bounds,
/// and is never left waiting for write-back that its own copies hold back
/// (FL5).
#[test]
fn a_committer_waits_for_room_and_never_for_its_own_copies() {
    let config = FlushConfig {
        max_unwritten: 1,
        max_pending_bytes: 100,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    assert!(db.wb().has_room());
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Free);

    db.commit(&[
        insert((1, 1), b"a", inline(b"v")),
        insert((2, 1), b"b", inline(b"v")),
    ]);
    assert_eq!(db.state.unwritten(), 2);
    assert!(!db.wb().has_room());
    // No background loop runs, so the caller makes the pass itself.
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Free);
    assert_eq!(db.state.unwritten(), 0);
    assert_eq!(db.stats().base_blocks, 2);

    // Pending values count too, by their bytes.
    db.commit(&[insert((3, 1), b"c", long(1, 2000))]);
    assert!(!db.wb().has_room());
    let mut txn = db.begin();
    txn.copy(page(3));
    // The value cannot be placed while this transaction holds its copy, so
    // its commit is not made to wait for that.
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Stalled);
    txn.commit(&[hash((3, 1), 1)]);
    db.wb().place_ready().expect("placed");
    // The version that was superseded with the value pending is kept for
    // its readers, with the value's bytes, until whoever keeps the pins
    // asks the pool to reclaim. Waiting does not help with that either.
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Stalled);
    assert_eq!(db.pool.reclaim(&db.versions), 1);
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Free);
    db.check_cold();
}
