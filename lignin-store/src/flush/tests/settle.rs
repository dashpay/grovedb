//! The settling of a manifest that a replacement left in doubt
//! ([`WriteBack::manifest`]): the manifest is opened again, and every
//! store is told of the one that is in force, whatever another store
//! answers.
//!
//! Each of the first three tests leaves a replacement of one store in
//! doubt where the directory is synced, so that the new manifest is in
//! force and that store has not heard of it, in a store with a segment
//! file that cannot be deleted: the segment store, which is told first,
//! ends with [`SegmentStoreError::NotDeleted`]. The store whose
//! replacement it was must hear of the manifest all the same, in that
//! settling: the next call finds nothing in doubt and tells nobody.

use std::collections::BTreeSet;

use lignin_format::{FileId, FileKind};
use lignin_io::sim::{Fault, FaultRule, OpKind};

use super::*;
use crate::segment::{FileNames, SegmentStoreError};

/// A store on the simulated file system in which every block is a base
/// image, with log files of `log_file_len` bytes.
fn store(log_file_len: u64) -> (SimVfs, Db) {
    let config = FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    };
    let log = LogOptions {
        target_file_len: log_file_len,
        ..LogOptions::default()
    };
    Db::on_sim(config, log)
}

/// A log that does not turn in these tests.
const ONE_LOG_FILE: u64 = 1 << 20;

/// The files of one kind that are on disk and that the manifest in force
/// does not name, by their names.
fn unnamed_on_disk(db: &Db, names: FileNames, kind: FileKind) -> BTreeSet<String> {
    let manifest = db.manifest.lock().expect("the manifest");
    let manifest = manifest.manifest().expect("a settled manifest");
    let named: BTreeSet<String> = match kind {
        FileKind::PageTable => (manifest.table_checkpoint().iter())
            .map(|table| names.file_name(table.file()))
            .collect(),
        _ => (manifest.files().iter())
            .filter(|file| file.kind() == kind)
            .map(|file| names.file_name(file.id()))
            .collect(),
    };
    let on_disk = db.vfs.read_dir(&names.dir(&db.path)).expect("a listing");
    (on_disk.into_iter())
        .map(|entry| entry.name.to_string_lossy().into_owned())
        .filter(|name| !named.contains(name))
        .collect()
}

/// Leaves the store with a segment that was dropped and that the manifest
/// no longer names, whose file cannot be deleted: every call that tells
/// the segment store of a manifest ends with
/// [`SegmentStoreError::NotDeleted`] until the rules are cleared. Returns
/// the segment.
fn a_segment_that_cannot_be_deleted(fs: &SimVfs, db: &Db) -> FileId {
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    let location = db.chain(page(1))[0].location().expect("a block");
    let segment = location.file;
    assert!(db
        .wb()
        .seal_segment(segment)
        .expect("the segment is sealed"));
    // The page gets its next image in another segment, and the first
    // holds no block that a link points to.
    db.commit(&[hash((1, 1), 1)]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "B");

    let path = FileNames::SEGMENT.path(&db.path, segment);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::RemoveFile)
            .at(&path)
            .forever(),
    );
    db.segments
        .drop_segment(segment)
        .expect("the segment is dropped");
    let committed = (db.segments).commit(&mut db.wb().manifest().expect("the manifest"));
    assert!(
        matches!(committed, Err(SegmentStoreError::NotDeleted { .. })),
        "{committed:?}"
    );
    segment
}

/// Fails the sync of the store's directory once, after `through` of them:
/// a replacement of the manifest is left in doubt with its rename made,
/// so the new manifest is in force.
fn leave_a_replacement_in_doubt(fs: &SimVfs, db: &Db, through: u64) {
    let rule = FaultRule::new(Fault::Eio).on(OpKind::SyncDir).at(&db.path);
    fs.add_rule(rule.skip(through));
}

fn in_doubt(db: &Db) -> bool {
    let manifest = db.manifest.lock().expect("the manifest");
    manifest.manifest().is_err()
}

/// Settles the manifest. The segment store answers that its file is not
/// deleted, the call ends with that, and nothing is in doubt afterwards.
fn settle_with_the_segment_stuck(db: &Db) {
    assert!(in_doubt(db));
    let settled = db.wb().manifest().map(drop);
    assert!(
        matches!(
            settled,
            Err(FlushError::Segments(SegmentStoreError::NotDeleted { .. }))
        ),
        "{settled:?}"
    );
    assert!(!in_doubt(db));
}

/// The session goes on once the file can be deleted: the segment store's
/// next replacement deletes it, a checkpoint covers everything, and every
/// version reads as the commits left it.
fn the_session_goes_on(fs: &SimVfs, db: &Db, stuck: FileId) {
    fs.clear_rules();
    db.commit(&[hash((1, 1), 200)]);
    let (covers, ..) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    (db.segments)
        .commit(&mut db.wb().manifest().expect("the manifest"))
        .expect("the segments are named");
    let stuck = FileNames::SEGMENT.file_name(stuck);
    let left = unnamed_on_disk(db, FileNames::SEGMENT, FileKind::Segment);
    assert!(!left.contains(&stuck), "{left:?}");
    db.check_cold();
}

/// **The table checkpoints are told.** An installation is left in doubt,
/// in force: the checkpoint of before is a file that no manifest names,
/// and the settling deletes it.
#[test]
fn a_settling_tells_the_table_checkpoints_though_a_segment_was_not_deleted() {
    let (fs, db) = store(ONE_LOG_FILE);
    let stuck = a_segment_that_cannot_be_deleted(&fs, &db);
    installed(db.wb().checkpoint().expect("a checkpoint"));
    let table = |db: &Db| unnamed_on_disk(db, FileNames::PAGE_TABLE, FileKind::PageTable);
    assert_eq!(table(&db), BTreeSet::new());

    db.commit(&[hash((1, 1), 2)]);
    leave_a_replacement_in_doubt(&fs, &db, 0);
    db.wb().checkpoint().expect_err("the installation fails");

    settle_with_the_segment_stuck(&db);
    assert_eq!(
        table(&db),
        BTreeSet::new(),
        "the table checkpoints were not told of the manifest in force"
    );
    the_session_goes_on(&fs, &db, stuck);
}

/// **The log is told.** A checkpoint is installed, and the replacement
/// that takes the log files before it out of the manifest is left in
/// doubt, in force: those files are named by no manifest, and the
/// settling deletes them.
#[test]
fn a_settling_tells_the_log_though_a_segment_was_not_deleted() {
    let (fs, db) = store(400);
    let stuck = a_segment_that_cannot_be_deleted(&fs, &db);
    installed(db.wb().checkpoint().expect("a checkpoint"));
    for round in 0..12u8 {
        db.commit(&[hash((1, 1), 10 + round)]);
        db.wb().roll_log().expect("the log is rolled");
    }
    db.wb().pass().expect("a pass");
    let log = |db: &Db| unnamed_on_disk(db, FileNames::WAL, FileKind::Wal);
    assert_eq!(log(&db), BTreeSet::new());
    let in_force = db.state.in_force();

    // The installation is the first replacement of this checkpoint, and
    // the log's is the second.
    leave_a_replacement_in_doubt(&fs, &db, 1);
    db.wb()
        .checkpoint()
        .expect_err("the log's replacement fails");
    assert_ne!(db.state.in_force(), in_force, "the checkpoint is in force");

    settle_with_the_segment_stuck(&db);
    assert_eq!(
        log(&db),
        BTreeSet::new(),
        "the log was not told of the manifest in force"
    );
    the_session_goes_on(&fs, &db, stuck);
}

/// **The value store is told.** A value-log file that holds no live value
/// is dropped, and the replacement that takes it out of the manifest is
/// left in doubt, in force: the file is named by no manifest, and the
/// settling deletes it.
#[test]
fn a_settling_tells_the_value_store_though_a_segment_was_not_deleted() {
    let (fs, db) = store(ONE_LOG_FILE);
    let stuck = a_segment_that_cannot_be_deleted(&fs, &db);
    db.commit(&[insert((3, 1), b"long", long(1, 900))]);
    db.wb().pass().expect("a pass");
    let file = db.pointer(page(3), slot(1)).file();
    // The value is replaced by one in its cell, and the file holds none.
    db.commit(&[Op::Value {
        page: page(3),
        slot: slot(1),
        value: inline(b"short"),
    }]);
    db.wb().pass().expect("a pass");
    assert_eq!(db.live_values(), 0);
    assert!(db.wb().close_value_file(file).expect("the file is closed"));
    db.wb().sync_log().expect("the log is durable");
    let retired = db.values.retire(db.versions.oldest_pin());
    let dropped = db.values.drop_empty(retired).expect("the file is dropped");
    assert_eq!(dropped, [file]);
    let values = |db: &Db| unnamed_on_disk(db, FileNames::VALUE_LOG, FileKind::ValueLog);
    assert_eq!(values(&db), BTreeSet::new());

    leave_a_replacement_in_doubt(&fs, &db, 0);
    (db.values)
        .commit(&mut db.wb().manifest().expect("the manifest"))
        .expect_err("the value store's replacement fails");

    settle_with_the_segment_stuck(&db);
    assert_eq!(
        values(&db),
        BTreeSet::new(),
        "the value store was not told of the manifest in force"
    );
    the_session_goes_on(&fs, &db, stuck);
}

/// Every store can hear of the same manifest twice, as it does when a
/// settling follows a replacement of its own that it has heard of
/// already: nothing changes the second time.
#[test]
fn every_store_hears_the_same_manifest_twice() {
    let (_fs, db) = store(400);
    db.commit(&[
        insert((1, 1), b"k", inline(b"v")),
        insert((3, 1), b"long", long(1, 900)),
    ]);
    for round in 0..6u8 {
        db.commit(&[hash((1, 1), round)]);
        db.wb().roll_log().expect("the log is rolled");
    }
    installed(db.wb().checkpoint().expect("a checkpoint"));
    let listing = |db: &Db| {
        let names = [
            FileNames::SEGMENT,
            FileNames::VALUE_LOG,
            FileNames::WAL,
            FileNames::PAGE_TABLE,
        ];
        names.map(|names| {
            db.vfs
                .read_dir(&names.dir(&db.path))
                .expect("a listing")
                .len()
        })
    };
    let before = listing(&db);
    for _ in 0..2 {
        let manifest = db.wb().manifest().expect("the manifest");
        db.segments.manifest_replaced(&manifest).expect("segments");
        db.values.manifest_replaced(&manifest).expect("values");
        db.log.manifest_replaced(&manifest).expect("the log");
        (db.checkpoints.manifest_replaced(&manifest)).expect("the table checkpoints");
    }
    assert_eq!(listing(&db), before);

    db.commit(&[hash((1, 1), 100), insert((3, 2), b"more", long(2, 900))]);
    let (covers, ..) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    db.check_cold();
}
