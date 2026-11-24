//! Pending values: where a value gets its pointer (`transactions.md` FL7,
//! PV7; test T-VL5).

use lignin_io::sim::{Fault, FaultRule, OpKind};

use super::*;
use crate::segment::{tests::sim::db_dir, FileNames};

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
    assert_eq!(db.live_values(), 0);
    assert_eq!(db.pool.attached_bytes(), 8000);
    db.check_warm();

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed, pass.waiting), (2, 3, 0));
    assert!(db.pending(page(1)).is_empty() && db.pending(page(2)).is_empty());
    assert_eq!(db.live_values(), 3);
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
    assert_eq!(db.live_values(), 0);
    assert_eq!(db.pending(page(1)).len(), 1);
    db.check_warm();

    // The value is listed for the committers, and for no pass.
    assert_eq!(db.state.listed(), (0, 1));

    txn.roll_back();
    assert_eq!(db.wb().place_ready().expect("placed"), 1);
    assert_eq!(db.state.listed(), (0, 0));
    assert_eq!(db.live_values(), 1);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed, pass.waiting), (1, 0, 0));
    assert_eq!(db.stats().values_appended, 1);
    db.check_cold();
}

/// A committer's call tries the values that a private copy held back, and
/// no other: a value that no copy holds back is a pass's. Here two values
/// are ready, the page of one is copied, and the pass that would place the
/// other is refused the record of its step by the log. The call of the
/// transaction's end places the first and leaves the second to a pass.
#[test]
fn a_committer_places_what_a_private_copy_held_back_and_nothing_else() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
    ]);
    let mut txn = db.begin();
    txn.copy(page(1));
    let file = db.log.end().expect("the log has a file").file();
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(db_dir(), file));
    fs.add_rule(record.torn_append(false));
    let error = db.wb().pass().expect_err("the log refused the record");
    assert!(matches!(error, FlushError::Log(_)), "{error}");
    fs.clear_rules();
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (2, 0));
    assert_eq!(db.state.listed(), (1, 1));

    // The log takes a record again, and the copy is gone.
    assert!(db.wb().roll_log().expect("the log is rolled"));
    txn.roll_back();
    assert_eq!(db.wb().place_ready().expect("placed"), 1);
    assert_eq!(db.state.listed(), (1, 0));
    assert!(db.pending(page(1)).is_empty());
    assert_eq!(db.pending(page(2)).len(), 1);

    // The other value is placed by the next pass, and nothing was appended
    // to the value log twice.
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.waiting), (1, 0));
    assert_eq!(db.state.listed(), (0, 0));
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (2, 2));
    assert_eq!(stats.values_given_up, 0);
    db.check_cold();
}

/// A value of the committers' list whose record the log refuses stays in
/// that list, whoever tried it, and the next call of a committer places
/// it. A copy holds the value back and a pass lists it, the copy goes, the
/// log refuses the record to the pass that tries the value next, and the
/// log is rolled: the call then makes the step, once.
#[test]
fn a_held_back_value_whose_record_the_log_refused_is_placed_by_the_next_committer() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    let mut txn = db.begin();
    txn.copy(page(1));
    db.wb().pass().expect("a pass");
    assert_eq!(db.state.listed(), (0, 1));
    txn.roll_back();

    let file = db.log.end().expect("the log has a file").file();
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(db_dir(), file));
    fs.add_rule(record.torn_append(false));
    let error = db.wb().pass().expect_err("the log refused the record");
    assert!(matches!(error, FlushError::Log(_)), "{error}");
    fs.clear_rules();
    assert_eq!(db.state.listed(), (0, 1));
    assert_eq!(db.stats().values_placed, 0);

    assert!(db.wb().roll_log().expect("the log is rolled"));
    assert_eq!(db.wb().place_ready().expect("placed"), 1);
    assert_eq!(db.state.listed(), (0, 0));
    let stats = db.stats();
    assert_eq!(
        (
            stats.values_appended,
            stats.values_placed,
            stats.values_given_up
        ),
        (1, 1, 0)
    );
    assert_eq!(db.live_values(), 1);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.waiting), (0, 0));
    db.check_cold();
}

/// A committer's call does not wait until a pass has gone through its own
/// placing steps. The pass is stopped inside its walk over the values that
/// no copy holds back, where the log has refused the record of a step: it
/// holds the list of those values there. The call of a committer is made
/// at that moment, on a thread of its own, and returns while the pass is
/// still stopped.
#[test]
fn a_committer_does_not_wait_for_the_placing_steps_of_a_pass() {
    use std::{cell::Cell, rc::Rc, sync::mpsc, time::Duration};

    use crate::flush::place::AFTER_A_REFUSED_STEP;

    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    let db = Arc::new(db);
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    let file = db.log.end().expect("the log has a file").file();
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(db_dir(), file));
    fs.add_rule(record.torn_append(false));

    let called = Rc::new(Cell::new(None));
    AFTER_A_REFUSED_STEP.with(|hook| {
        let (db, called) = (db.clone(), called.clone());
        *hook.borrow_mut() = Some(Box::new(move || {
            assert!(
                db.state.placing.try_lock().is_err(),
                "the pass holds the list of its values"
            );
            let (done, ended) = mpsc::channel();
            let committer = db.clone();
            std::thread::spawn(move || {
                let _ = done.send(
                    committer
                        .wb()
                        .place_ready()
                        .map_err(|error| error.to_string()),
                );
            });
            let placed = ended
                .recv_timeout(Duration::from_secs(60))
                .expect("the call waited for the pass");
            called.set(Some(placed.expect("no step is tried")));
        }));
    });
    let error = db.wb().pass().expect_err("the log refused the record");
    assert!(matches!(error, FlushError::Log(_)), "{error}");
    assert_eq!(called.get(), Some(0), "the call was made inside the pass");
    assert_eq!(db.state.listed(), (1, 0));
}

/// A value that a private copy holds back stays listed for the committers
/// while the copy is there, whoever tries it, and is given up from that
/// list when its cell is gone.
#[test]
fn a_value_that_is_held_back_stays_listed_until_it_is_placed_or_given_up() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", long(1, 2000))]);
    let mut txn = db.begin();
    txn.copy(page(1));
    db.wb().pass().expect("a pass");
    assert_eq!(db.state.listed(), (0, 1));

    // A call beside the open transaction, as a write with no transaction
    // makes it, and another pass: the copy is there still.
    assert_eq!(db.wb().place_ready().expect("no step"), 0);
    assert_eq!(db.state.listed(), (0, 1));
    db.wb().pass().expect("a pass");
    assert_eq!(db.state.listed(), (0, 1));
    assert_eq!(db.stats().steps_put_off, 3);

    // The transaction removes the cell: the value is given up by the call
    // of its commit.
    txn.commit(&[Op::Remove {
        page: page(1),
        slot: slot(1),
    }]);
    assert_eq!(db.wb().place_ready().expect("no step"), 0);
    assert_eq!(db.state.listed(), (0, 0));
    let stats = db.stats();
    assert_eq!((stats.values_placed, stats.values_given_up), (0, 1));
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
    assert_eq!(db.live_values(), 1);
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
    assert_eq!(db.live_values(), 0);
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
    assert_eq!(db.live_values(), 1);

    db.commit(&[Op::Value {
        page: page(1),
        slot: slot(1),
        value: long(9, 2500),
    }]);
    assert_eq!(db.live_values(), 0);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.placed), (1, 1));
    assert_eq!(db.live_values(), 1);
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

/// The value-log file `raw` of the simulated store, by name and by path.
fn value_file(raw: u32) -> (String, std::path::PathBuf) {
    use crate::segment::{tests::sim::db_dir, FileNames};

    let id = lignin_format::FileId::new(raw).expect("a file id");
    let path = FileNames::VALUE_LOG.path(db_dir(), id);
    let name = path.file_name().expect("a name").to_string_lossy();
    (name.into_owned(), path)
}

/// What the manifest in force says of the value-log file `id`: its state
/// and the length of it that the manifest covers.
fn in_force(db: &Db, id: lignin_format::FileId) -> (lignin_format::manifest::FileState, u64) {
    use lignin_format::FileKind;

    let manifest = db.manifest.lock().expect("the manifest");
    let manifest = manifest.manifest().expect("a manifest in force");
    let mut files = manifest.files().iter();
    let file = files.find(|file| file.kind() == FileKind::ValueLog && file.id() == id);
    let file = file.expect("the manifest names the file");
    (file.state(), file.valid_len())
}

/// A store on the simulator, behind the file layer that knows its
/// handles apart, whose value-log files are full after one value of 900
/// bytes.
fn with_small_value_files() -> (lignin_io::sim::SimVfs, std::sync::Arc<handles::Handles>, Db) {
    use crate::segment::tests::sim::{db_dir, shared, sim_fs};

    let fs = sim_fs(lignin_io::sim::SimConfig::new(5));
    let vfs = handles::Handles::over(shared(&fs));
    let config = FlushConfig::new(Db::SIZE);
    let mut db = Db::create(vfs.clone(), db_dir(), config, LogOptions::default());
    let values = {
        let manifest = db.manifest.lock().expect("the manifest");
        crate::values::ValueStore::open(&manifest, &[]).expect("the value log")
    };
    db.values = values.with_target_file_len(700);
    (fs, vfs, db)
}

/// A pass fills one value-log file and goes on in the next, and the sync
/// that was to cover the values of the pass is refused for the second
/// file. The value in the first file is covered all the same: that file
/// was synced and closed when it was full, and the manifest that named
/// the second file names it at its whole length. So that value keeps its
/// place there and is given its pointer in the same pass, and only the
/// value in the second file is given up and appended again.
#[test]
fn a_refused_covering_sync_keeps_the_values_that_a_manifest_covers_in_an_earlier_file() {
    use lignin_format::{manifest::FileState, FileId};
    use lignin_io::sim::{Fault, FaultRule, OpKind};

    let (fs, vfs, db) = with_small_value_files();
    let file = |raw| FileId::new(raw).expect("a file id");
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
    ]);
    // The second file is synced once when it is made. The sync after
    // that is the one that covers the values of the pass.
    let (second, path) = value_file(2);
    let sync = FaultRule::new(Fault::Enospc).on(OpKind::SyncData).at(&path);
    fs.add_rule(sync.skip(1));
    let error = db.wb().pass().expect_err("the sync of the second file");
    assert!(error.to_string().contains(&second), "{error}");

    let stats = db.stats();
    assert_eq!(
        (stats.values_appended, stats.values_placed),
        (2, 1),
        "{stats:?}"
    );
    assert_eq!((stats.values_given_up, stats.value_files_given_up), (1, 1));
    let first = db.pointers(page(1));
    assert_eq!(first.len(), 1, "the value in the first file has its place");
    assert_eq!(first[0].file(), file(1));
    assert!(db.values.covers(first[0]));
    assert_eq!(db.pointers(page(2)).len(), 0);
    let (state, covered) = in_force(&db, file(1));
    assert_eq!(state, FileState::Closed);
    assert!(
        first[0].offset() + first[0].extent_len(lignin_format::FormatVersion::CURRENT) <= covered,
        "{first:?} in {covered} bytes"
    );
    // The first file took its header and one value, each with its sync:
    // when the file was made, and when it was full. Nothing was done to
    // it since.
    let (name, _) = value_file(1);
    let ops = |seen: Vec<handles::Seen>| -> Vec<(&'static str, bool)> {
        seen.iter().map(|seen| (seen.op, seen.ok)).collect()
    };
    let filled = [("append", true), ("sync", true)].repeat(2);
    assert_eq!(ops(vfs.seen(&name)), filled);

    // The value of the second file goes to a third.
    db.wb().pass().expect("a pass");
    assert_eq!(db.pointers(page(1)), first);
    let other = db.pointers(page(2));
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].file(), file(3));
    assert_eq!(db.stats().values_appended, 3);
    assert_eq!(ops(vfs.seen(&name)), filled);
    db.check_cold();
}

/// The sync that is to close a value-log file that is full is refused,
/// when the pass asks for the next file. The file is given up there and
/// counted so: it is closed at the length that was durable before, which
/// the manifest names, and the value in it is given up and goes to
/// another file with the value that was to follow it.
#[test]
fn a_refused_sync_at_the_turn_to_the_next_value_log_file_gives_the_full_file_up() {
    use lignin_format::{manifest::FileState, FileId};
    use lignin_io::{
        sim::{Fault, FaultRule, OpKind},
        OpenMode,
    };

    let (fs, vfs, db) = with_small_value_files();
    let file = |raw| FileId::new(raw).expect("a file id");
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
    ]);
    // The file is synced once when it is made. The sync after that is
    // the one that is to close it.
    let (name, path) = value_file(1);
    let sync = FaultRule::new(Fault::Enospc).on(OpKind::SyncData).at(&path);
    fs.add_rule(sync.skip(1));
    let error = db.wb().pass().expect_err("the sync of the full file");
    assert!(error.to_string().contains(&name), "{error}");

    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (1, 0));
    assert_eq!((stats.values_given_up, stats.value_files_given_up), (1, 1));
    let seen = vfs.seen(&name);
    let ops: Vec<(&str, bool)> = seen.iter().map(|seen| (seen.op, seen.ok)).collect();
    // The header with its sync, the value, and the sync that was refused.
    let expected = [
        ("append", true),
        ("sync", true),
        ("append", true),
        ("sync", false),
    ];
    assert_eq!(ops, expected);
    let (state, covered) = in_force(&db, file(1));
    assert_eq!(state, FileState::Closed);
    let on_device = db.vfs.open(&path, OpenMode::Read);
    let on_device = on_device.and_then(|file| file.len()).expect("a length");
    assert!(covered + 900 <= on_device, "{covered} of {on_device} bytes");
    let waiting = crate::flush::state::lock(&db.state.placing).len();
    assert_eq!(waiting, 0, "a value of the file waits for its page step");

    db.wb().pass().expect("a pass");
    db.values.sync().expect("a sync of the value log");
    assert_eq!(vfs.seen(&name), seen, "the file was appended to or synced");
    for id in 1..=2 {
        let pointers = db.pointers(page(id));
        assert_eq!(pointers.len(), 1, "page {id}");
        assert_ne!(pointers[0].file(), file(1), "page {id}");
        assert!(db.values.covers(pointers[0]), "page {id}");
    }
    assert_eq!(db.stats().value_files_given_up, 1);
    db.check_cold();
}

/// The handle that made every append to a value-log file, of which
/// `seen` is what its handles did.
fn appender(seen: &[handles::Seen]) -> u64 {
    let mut appended = seen.iter().filter(|seen| seen.op == "append");
    let handle = appended.next().expect("an append").handle;
    assert!(appended.all(|seen| seen.handle == handle), "{seen:?}");
    handle
}

/// D-209. An append to the value-log file that write-back fills is
/// refused, with a value of the same pass in the file already. The file
/// is given up: what it holds is synced through the handle that appended
/// it, and the file is closed where it ends. So the value before the
/// refused one is covered and placed, and the file is one that the
/// cleaner of the value log takes, in this session. The other value goes
/// to another file.
#[test]
fn a_value_log_file_that_is_given_up_after_a_refused_append_is_closed_in_the_session() {
    use lignin_format::{manifest::FileState, FileId};
    use lignin_io::{
        sim::{Fault, FaultRule, OpKind, SimConfig},
        OpenMode,
    };

    use crate::segment::tests::sim::{db_dir, shared, sim_fs};

    let fs = sim_fs(SimConfig::new(5));
    let vfs = handles::Handles::over(shared(&fs));
    let config = FlushConfig::new(Db::SIZE);
    let db = Db::create(vfs.clone(), db_dir(), config, LogOptions::default());
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
    ]);
    let file = |raw| FileId::new(raw).expect("a file id");
    let (name, path) = value_file(1);
    // The file's header and the first value are appended, and the second
    // value is refused.
    let refused = FaultRule::new(Fault::Enospc).on(OpKind::Append).at(&path);
    fs.add_rule(refused.skip(2).torn_append(false));
    let error = db.wb().pass().expect_err("an append to the value log");
    assert!(error.to_string().contains("00000001.val"), "{error}");

    let files = |pointers: Vec<ValuePointer>| -> Vec<FileId> {
        pointers.iter().map(|pointer| pointer.file()).collect()
    };
    assert_eq!(files(db.pointers(page(1))), [file(1)], "the first value");
    let stats = db.stats();
    assert_eq!((stats.values_given_up, stats.value_files_given_up), (0, 1));
    let closed: Vec<FileId> = (db.values.cleanable(0).iter())
        .map(|cleanable| cleanable.file)
        .collect();
    assert_eq!(
        closed,
        [file(1)],
        "the file that was given up is not closed"
    );

    // The sync of the giving up is the one after the refused append, and
    // the handle that appended made it.
    let seen = vfs.seen(&name);
    let at = seen.iter().position(|seen| seen.op == "append" && !seen.ok);
    let at = at.expect("the refused append");
    let sync = seen.get(at + 1).expect("a sync after the refused append");
    assert_eq!((sync.op, sync.ok), ("sync", true), "{seen:?}");
    assert_eq!(sync.handle, appender(&seen), "{seen:?}");
    assert_eq!(seen.len(), at + 2, "{seen:?}");

    // The next pass puts the other value into another file, and the
    // manifest it writes names the first one closed at its whole length.
    db.wb().pass().expect("a pass");
    assert_eq!(files(db.pointers(page(2))), [file(2)], "the second value");
    let named = db.values.files_named().expect("the files");
    let first = named.iter().find(|named| named.id() == file(1));
    let first = first.expect("the file is named");
    assert_eq!(first.state(), FileState::Closed);
    let on_device = db.vfs.open(&path, OpenMode::Read);
    let on_device = on_device.and_then(|file| file.len()).expect("a length");
    assert_eq!(first.valid_len(), on_device);
    db.check_cold();
}

/// D-246. The sync of a value-log file at its giving up is refused. The
/// file held a value that an earlier sync had made durable and a manifest
/// covered, and two that were appended since.
///
/// - The file is closed at the length the earlier sync covered, and the
///   manifest names it so. Nothing is appended to it and no sync of it is
///   made for the rest of the session, whoever syncs the value log: a
///   sync that succeeded later would say nothing of the bytes the refused
///   one was for.
/// - The two values behind that length are given up, with their holds,
///   and no manifest covers them: no cell points at them. They are
///   appended to another file with the value that was refused. The value
///   before that length keeps its place and is read.
/// - The session goes on: a table checkpoint is installed.
#[test]
fn a_value_log_file_whose_sync_at_its_giving_up_is_refused_is_closed_where_a_sync_had_covered_it() {
    use lignin_format::{manifest::FileState, FileId};
    use lignin_io::{
        sim::{Fault, FaultRule, OpKind, SimConfig},
        OpenMode,
    };

    use crate::segment::tests::sim::{db_dir, shared, sim_fs};

    let fs = sim_fs(SimConfig::new(5));
    let vfs = handles::Handles::over(shared(&fs));
    let config = FlushConfig::new(Db::SIZE);
    let db = Db::create(vfs.clone(), db_dir(), config, LogOptions::default());
    let file = |raw| FileId::new(raw).expect("a file id");
    let (name, path) = value_file(1);
    let named = |db: &Db| {
        let named = db.values.files_named().expect("the files");
        let first = named.iter().find(|named| named.id() == file(1));
        let first = first.expect("the file is named");
        (first.state(), first.valid_len())
    };
    let on_device = |db: &Db| {
        let handle = db.vfs.open(&path, OpenMode::Read);
        handle.and_then(|file| file.len()).expect("a length")
    };

    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    db.wb().pass().expect("a pass");
    let first = db.pointers(page(1));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].file(), file(1));
    let (state, covered) = named(&db);
    assert_eq!(state, FileState::Open);
    assert_eq!(covered, on_device(&db), "the first value is durable");

    db.commit(&[
        insert((2, 1), b"b", long(2, 900)),
        insert((3, 1), b"c", long(3, 900)),
        insert((4, 1), b"d", long(4, 900)),
    ]);
    // Two more values are appended, the third is refused, and so is
    // every sync of the file from here on.
    let append = FaultRule::new(Fault::Enospc).on(OpKind::Append).at(&path);
    fs.add_rule(append.skip(2).torn_append(false));
    let sync = FaultRule::new(Fault::Enospc).on(OpKind::SyncData).at(&path);
    fs.add_rule(sync.forever());
    let error = db.wb().pass().expect_err("an append to the value log");
    assert!(error.to_string().contains("00000001.val"), "{error}");
    fs.clear_rules();

    let stats = db.stats();
    assert_eq!((stats.values_given_up, stats.value_files_given_up), (2, 1));
    assert_eq!(named(&db), (FileState::Closed, covered));
    // The manifest in force, which the pass installed, covers the file
    // to that length and no further.
    assert_eq!(in_force(&db, file(1)), (FileState::Closed, covered));
    assert!(on_device(&db) >= covered + 1800, "two values lie behind it");
    // The refused sync was made through the handle that appended, and is
    // the last thing a handle did to the file.
    let seen = vfs.seen(&name);
    let refused = seen.last().expect("a sync");
    assert_eq!((refused.op, refused.ok), ("sync", false), "{seen:?}");
    assert_eq!(refused.handle, appender(&seen), "{seen:?}");

    // The rest of the session: the values are placed elsewhere, the
    // value log is synced as a whole, and a table checkpoint is taken.
    db.wb().pass().expect("a pass");
    db.values.sync().expect("a sync of the value log");
    let installed = db.wb().checkpoint().expect("a table checkpoint");
    assert!(
        matches!(installed, Checkpointed::Installed { .. }),
        "{installed:?}"
    );
    db.values.sync().expect("a sync of the value log");
    assert_eq!(vfs.seen(&name), seen, "the file was appended to or synced");
    assert_eq!(named(&db), (FileState::Closed, covered));
    assert_eq!(in_force(&db, file(1)), (FileState::Closed, covered));

    assert_eq!(db.pointers(page(1)), first);
    for id in 1..=4 {
        let pointers = db.pointers(page(id));
        assert_eq!(pointers.len(), 1, "page {id}");
        let pointer = pointers[0];
        assert!(db.values.covers(pointer), "page {id}: {pointer:?}");
        assert_eq!(pointer.file() == file(1), id == 1, "page {id}");
    }
    assert_eq!(db.stats().values_given_up, 2);
    db.check_cold();
}

/// D-246. The sync that covers the values of a pass is refused, once.
/// The file is given up there: it is closed at the length the sync
/// before had covered, and no second sync of it is asked for, which the
/// device would have answered with success and which would have said
/// nothing of the bytes the refused one was for. The values of the pass
/// are in no cell and go to another file.
#[test]
fn a_value_log_file_whose_covering_sync_is_refused_is_closed_and_not_synced_again() {
    use lignin_format::{manifest::FileState, FileId};
    use lignin_io::sim::{Fault, FaultRule, OpKind, SimConfig};

    use crate::segment::tests::sim::{db_dir, shared, sim_fs};

    let fs = sim_fs(SimConfig::new(5));
    let vfs = handles::Handles::over(shared(&fs));
    let config = FlushConfig::new(Db::SIZE);
    let db = Db::create(vfs.clone(), db_dir(), config, LogOptions::default());
    let file = |raw| FileId::new(raw).expect("a file id");
    let (name, path) = value_file(1);
    let named = |db: &Db| {
        let named = db.values.files_named().expect("the files");
        let first = named.iter().find(|named| named.id() == file(1));
        let first = first.expect("the file is named");
        (first.state(), first.valid_len())
    };

    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    db.wb().pass().expect("a pass");
    let first = db.pointers(page(1));
    let (state, covered) = named(&db);
    assert_eq!(state, FileState::Open);
    let before = vfs.seen(&name).len();

    db.commit(&[
        insert((2, 1), b"b", long(2, 900)),
        insert((3, 1), b"c", long(3, 900)),
    ]);
    let sync = FaultRule::new(Fault::Enospc).on(OpKind::SyncData).at(&path);
    fs.add_rule(sync);
    let error = db.wb().pass().expect_err("the sync of the value log");
    assert!(error.to_string().contains("00000001.val"), "{error}");

    let seen = vfs.seen(&name);
    let since: Vec<(&str, bool)> = (seen[before..].iter())
        .map(|seen| (seen.op, seen.ok))
        .collect();
    assert_eq!(since, [("append", true), ("append", true), ("sync", false)]);
    assert_eq!(named(&db), (FileState::Closed, covered));
    // No manifest was written since the one that covered the first
    // value: the one in force covers the file to that length.
    assert_eq!(in_force(&db, file(1)), (FileState::Open, covered));
    let stats = db.stats();
    assert_eq!((stats.values_given_up, stats.value_files_given_up), (2, 1));

    db.wb().pass().expect("a pass");
    db.values.sync().expect("a sync of the value log");
    assert_eq!(vfs.seen(&name), seen, "the file was appended to or synced");
    assert_eq!(in_force(&db, file(1)), (FileState::Closed, covered));
    assert_eq!(db.pointers(page(1)), first);
    for id in 2..=3 {
        let pointers = db.pointers(page(id));
        assert_eq!(pointers.len(), 1, "page {id}");
        assert_eq!(pointers[0].file(), file(2), "page {id}");
        assert!(db.values.covers(pointers[0]), "page {id}");
    }
    db.check_cold();
}
