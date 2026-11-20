//! The log's files in a session with write-back: a table checkpoint turns
//! the log where it begins, so that the files before the turn can go when
//! it is installed, and makes the file it turns to where the log has none.

use lignin_format::{table::FIRST_SAMPLE_OFFSET, FileId};
use lignin_io::sim::{Fault, FaultRule, OpKind};

use super::*;
use crate::segment::FileNames;

/// The ids of the files the log has, as a manifest would name them.
fn log_files(db: &Db) -> Vec<FileId> {
    let named = db.log.files_named().expect("the log's files");
    named.iter().map(|file| file.id()).collect()
}

/// How many files `wal/` holds.
fn on_disk(db: &Db) -> usize {
    let dir = FileNames::WAL.dir(&db.path);
    db.vfs.read_dir(&dir).expect("a listing").len()
}

fn file_of_the_end(db: &Db) -> FileId {
    db.log.end().expect("an end").file()
}

#[test]
fn a_checkpoint_turns_the_log_and_drops_the_file_with_the_commits_it_covers() {
    let db = Db::new();
    let wb = db.wb();
    db.commit(&[insert((1, 1), b"a", inline(&[1; 40]))]);
    db.commit(&[insert((2, 1), b"b", inline(&[2; 40]))]);
    let first = file_of_the_end(&db);
    assert!(
        db.log.has_file_ready(),
        "the roll of a new store leaves one"
    );

    let (covers, place, dropped) = installed(wb.checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    // The log was turned before the sample was taken: the mark is the
    // first item of the next file, and the file with the two commits is
    // gone with the checkpoint.
    assert!(place.file() > first);
    assert_eq!(file_of_the_end(&db), place.file());
    assert_eq!(place.offset(), FIRST_SAMPLE_OFFSET);
    assert_eq!(dropped, 1);
    assert_eq!(log_files(&db), [place.file()]);
    assert_eq!(on_disk(&db), 1);
    // The turn took the file that was ready.
    assert!(!db.log.has_file_ready());

    // A checkpoint with no commit since the turn leaves the log in its
    // file, and makes no file.
    let (_, again, dropped) = installed(wb.checkpoint().expect("a checkpoint"));
    assert_eq!(again.file(), place.file());
    assert_eq!(dropped, 0);
    assert_eq!(on_disk(&db), 1);

    // What a recovery reads is the file the log turned to.
    db.commit(&[hash((1, 1), 7)]);
    let db = db.reopen();
    db.check_cold();
}

#[test]
fn a_checkpoint_makes_the_log_file_it_turns_to_where_none_is_ready() {
    let db = Db::new();
    let wb = db.wb();
    db.commit(&[insert((1, 1), b"a", inline(&[1; 40]))]);
    let (_, first, _) = installed(wb.checkpoint().expect("a checkpoint"));
    assert!(!db.log.has_file_ready());

    // A commit, and no file to turn to: the checkpoint has one made,
    // turns to it, and the file with the commit goes.
    db.commit(&[hash((1, 1), 3)]);
    let (covers, place, dropped) = installed(wb.checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    assert!(place.file() > first.file());
    assert_eq!(place.offset(), FIRST_SAMPLE_OFFSET);
    assert_eq!(dropped, 1);
    assert_eq!(log_files(&db), [place.file()]);
    assert_eq!(on_disk(&db), 1);
    assert!(!db.log.has_file_ready());
    db.check_cold();
}

#[test]
fn the_log_is_turned_once_for_a_checkpoint_that_is_put_off() {
    let db = Db::new();
    let wb = db.wb();
    db.commit(&[insert((1, 1), b"a", inline(&[1; 40]))]);
    db.commit(&[insert((2, 1), b"b", long(1, 900))]);
    let before = file_of_the_end(&db);

    // A write transaction keeps the value of page 2 from being placed, so
    // the page cannot be written back, and the checkpoint is put off.
    let mut txn = db.begin();
    txn.copy(page(2));
    let put_off = |checkpointed| assert_eq!(checkpointed, Checkpointed::PutOff { blocked: 1 });
    put_off(wb.checkpoint().expect("an attempt"));
    let turned = file_of_the_end(&db);
    assert!(turned > before);
    let files = on_disk(&db);

    // The next attempts find commits in the log's file, and neither turn
    // the log nor make a file for a turn: the log was turned for this
    // checkpoint.
    for byte in [6, 7] {
        db.commit(&[hash((1, 1), byte)]);
        put_off(wb.checkpoint().expect("an attempt"));
        assert_eq!(file_of_the_end(&db), turned);
        assert_eq!(on_disk(&db), files);
    }

    // Once the checkpoint is installed, the next one turns the log again.
    txn.roll_back();
    installed(wb.checkpoint().expect("a checkpoint"));
    assert_eq!(file_of_the_end(&db), turned);
    db.commit(&[hash((1, 1), 8)]);
    let (_, place, _) = installed(wb.checkpoint().expect("a checkpoint"));
    assert!(place.file() > turned);
}

/// **The checkpoint does not rest on its turn.** Where the file for the
/// turn cannot be created, or cannot be made durable with the manifest
/// that names it, the checkpoint is taken in the file the log is in, and
/// the next checkpoint turns the log.
#[test]
fn a_checkpoint_is_taken_where_the_log_is_when_no_file_can_be_made_for_its_turn() {
    for failed in [OpKind::Open, OpKind::SyncData] {
        let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
        let wb = db.wb();
        db.commit(&[insert((1, 1), b"a", inline(&[1; 40]))]);
        let (_, first, _) = installed(wb.checkpoint().expect("a checkpoint"));
        assert!(!db.log.has_file_ready());
        let files = on_disk(&db);

        // The next log file is the third, and it cannot be made.
        let next = FileNames::WAL.path(&db.path, FileId::new(3).expect("an id"));
        fs.add_rule(FaultRule::new(Fault::Eio).on(failed).at(&next));
        db.commit(&[hash((1, 1), 3)]);
        let (covers, place, dropped) = installed(wb.checkpoint().expect("a checkpoint"));
        assert_eq!(covers, db.latest(), "{failed}");
        assert_eq!(place.file(), first.file(), "{failed}");
        assert!(place > first);
        assert_eq!(dropped, 0);
        assert!(!db.log.has_file_ready());
        assert_eq!(on_disk(&db), files, "{failed}: the file is not left behind");

        // The file after it can be made, and the next checkpoint turns.
        db.commit(&[hash((1, 1), 4)]);
        let (_, place, dropped) = installed(wb.checkpoint().expect("a checkpoint"));
        assert_eq!(place.file(), FileId::new(4).expect("an id"), "{failed}");
        assert_eq!(dropped, 1);
        db.check_cold();
    }
}
