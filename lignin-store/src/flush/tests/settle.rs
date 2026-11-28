//! Steps that are all made whatever became of the one before, with the
//! first error returned afterwards: the settling of a manifest that a
//! replacement left in doubt ([`WriteBack::manifest`]), in which the
//! manifest is opened again and every store is told of the one that is in
//! force; the close of a session, in which every store names its files;
//! a checkpoint that is given up after a sync of the log failed; and the
//! cover of the values a pass appended before something ended its walk.
//!
//! Each of the first three tests leaves a replacement of one store in
//! doubt where the directory is synced, so that the new manifest is in
//! force and that store has not heard of it, in a store with a segment
//! file that cannot be deleted: the segment store, which is told first,
//! ends with [`SegmentStoreError::NotDeleted`]. The store whose
//! replacement it was must hear of the manifest all the same, in that
//! settling: the next call finds nothing in doubt and tells nobody.

use std::collections::BTreeSet;

use lignin_format::{
    manifest::{FileState, LiveFile},
    FileId, FileKind,
};
use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};

use super::{recover::the_store_recovers_the_same, *};
use crate::{
    segment::{tests::sim::failed_ops, FileNames, SegmentStoreError},
    wal::LogError,
};

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
    // The first replacement of this checkpoint names the log file it
    // turns to, and the installation is the second.
    leave_a_replacement_in_doubt(&fs, &db, 1);
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

/// The files of one kind that the manifest in force names.
fn named(db: &Db, kind: FileKind) -> Vec<LiveFile> {
    let manifest = db.manifest.lock().expect("the manifest");
    let manifest = manifest.manifest().expect("a settled manifest");
    let files = manifest.files().iter();
    files.filter(|file| file.kind() == kind).copied().collect()
}

/// A store with a segment that cannot be deleted, a value out of line in
/// a value-log file that write-back is filling, and a commit behind the
/// last manifest: what a close has to name. Returns the pages it wrote.
fn a_session_to_close(fs: &SimVfs, db: &Db) -> [PageId; 2] {
    a_segment_that_cannot_be_deleted(fs, db);
    db.commit(&[insert((3, 1), b"long", long(1, 900))]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((1, 1), 9)]);
    [page(1), page(3)]
}

/// Closes the session, which ends with the segment store's error.
fn close_with_the_segment_stuck(db: &Db) {
    let finished = db.wb().finish();
    assert!(
        matches!(
            finished,
            Err(FlushError::Segments(SegmentStoreError::NotDeleted { .. }))
        ),
        "{finished:?}"
    );
}

/// **A close has every store name its files, also when the segment store
/// could not delete one.** The segment store's replacement is the first,
/// and ends with [`SegmentStoreError::NotDeleted`]. The value store's and
/// the log's are made all the same: the manifest names the value-log file
/// closed and the log to its durable end, as [`WriteBack::finish`] says
/// it does.
#[test]
fn a_close_names_the_files_of_every_store_though_a_segment_was_not_deleted() {
    let (fs, db) = store(ONE_LOG_FILE);
    a_session_to_close(&fs, &db);
    close_with_the_segment_stuck(&db);

    let segments = db.segments.segments_named().expect("the segments");
    assert_eq!(named(&db, FileKind::Segment), segments);
    let values = db.values.files_named().expect("the value-log files");
    assert!(!values.is_empty());
    assert_eq!(
        named(&db, FileKind::ValueLog),
        values,
        "the value store did not name its files at the close"
    );
    let log = db.log.files_named().expect("the log's files");
    assert_eq!(
        named(&db, FileKind::Wal),
        log,
        "the log did not name its files at the close"
    );
}

/// **A close has every store name its files, also after a settling that a
/// store answered with an error.** The checkpoint of the close leaves the
/// manifest in doubt at its installation. The close settles it, the
/// segment store answers the settling with
/// [`SegmentStoreError::NotDeleted`], and every store has heard of the
/// manifest all the same: the three replacements are made from it.
#[test]
fn a_close_names_the_files_of_every_store_after_a_settling_that_ended_with_an_error() {
    let (fs, db) = store(ONE_LOG_FILE);
    a_session_to_close(&fs, &db);
    // The installation is the first replacement of the close.
    leave_a_replacement_in_doubt(&fs, &db, 0);
    let finished = db.wb().finish();
    assert!(
        matches!(finished, Err(FlushError::Table(_))),
        "the installation's error comes first: {finished:?}"
    );
    assert!(!in_doubt(&db));

    let values = db.values.files_named().expect("the value-log files");
    assert!(!values.is_empty());
    assert_eq!(
        named(&db, FileKind::ValueLog),
        values,
        "the value store did not name its files at the close"
    );
    let log = db.log.files_named().expect("the log's files");
    assert_eq!(
        named(&db, FileKind::Wal),
        log,
        "the log did not name its files at the close"
    );
    let segments = db.segments.segments_named().expect("the segments");
    assert_eq!(named(&db, FileKind::Segment), segments);
}

/// The next session opens to the same state whatever a close could name.
/// Here the value store's and the log's replacements are both attempted,
/// behind the segment store's answer, and fail before they write
/// anything, so the manifest names the value-log file as one that is
/// still written and the log short of its end, as a session that is
/// killed leaves them. The store's recovery lands on the last commit,
/// with every page as the commits left it and every count of live values
/// right.
#[test]
fn the_next_session_recovers_from_a_close_that_named_neither_the_values_nor_the_log() {
    let (fs, db) = store(ONE_LOG_FILE);
    let ids = a_session_to_close(&fs, &db);
    // The close's checkpoint is kept from turning the log, as it is when
    // an attempt before it turned the log and was put off. It then takes
    // its sample in the file the commits are in and takes no log file
    // out, so the last manifest that names the log's files is the one
    // from before the session's commits.
    (db.state.checkpoints.lock().expect("the checkpoints")).turned = true;
    // The installation and the segment store's replacement are made, and
    // no manifest is written after them.
    let temporary = db.path.join(ManifestStore::TEMP_FILE_NAME);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Open)
            .at(&temporary)
            .skip(2)
            .forever(),
    );
    close_with_the_segment_stuck(&db);
    let refused = (failed_ops(&fs).iter())
        .filter(|op| op.kind == OpKind::Open && op.path == temporary.to_string_lossy())
        .count();
    assert_eq!(refused, 2, "one replacement each of the values and the log");
    assert!(!in_doubt(&db));
    let values = named(&db, FileKind::ValueLog);
    assert!(
        values.iter().any(|file| file.state() == FileState::Open),
        "the value-log file is named as still written: {values:?}"
    );
    assert_ne!(values, db.values.files_named().expect("the files"));
    let log = db.log.files_named().expect("the log's files");
    assert_ne!(named(&db, FileKind::Wal), log, "the log is named short");

    let latest = db.latest();
    let model = db.model.lock().expect("the model").clone();
    let (vfs, path) = (db.vfs.clone(), db.path.clone());
    drop(db);
    fs.clear_rules();
    let found = recover::recover(vfs.clone(), &path, &ids);
    assert_eq!(found.landed, latest);
    assert!(found.checkpoint && found.covers == latest);
    for id in ids {
        let (_, expected) = model.at(id, latest).expect("a version of the page");
        let cells = found.pages[&id].as_ref().expect("the page is recovered");
        assert!(same(cells, expected), "page {id}: {cells:?}");
    }
    the_store_recovers_the_same(vfs, &path, &ids, &found);
}

/// Fails every sync of the file the log ends in, and returns nothing: the
/// next sync of the log fails.
fn fail_the_syncs_of_the_log(fs: &SimVfs, db: &Db) {
    let file = db.log.end().expect("the log has a file").file();
    let path = FileNames::WAL.path(&db.path, file);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(&path)
            .forever(),
    );
}

fn table_files(db: &Db) -> usize {
    let dir = FileNames::PAGE_TABLE.dir(&db.path);
    db.vfs.read_dir(&dir).expect("a listing").len()
}

/// A store with a checkpoint in force, a commit behind it that is
/// written back, a manifest that a replacement left in doubt, and a log
/// whose syncs fail: the next checkpoint is written and then given up.
/// Returns the checkpoint in force and how many files `table/` holds.
fn a_checkpoint_to_give_up(
    fs: &SimVfs,
    db: &Db,
) -> (Option<(CommitVersion, Option<LogPlace>)>, usize) {
    a_segment_that_cannot_be_deleted(fs, db);
    installed(db.wb().checkpoint().expect("a checkpoint"));
    let (in_force, before) = (db.state.in_force(), table_files(db));
    db.commit(&[hash((1, 1), 2)]);
    db.wb().pass().expect("a pass");

    // A replacement that is no installation is left in doubt: nothing
    // settles it before the checkpoint has synced the log. The segment
    // that write-back fills is sealed, so that the segment store has
    // something to name.
    let filled = db.chain(page(1))[0].location().expect("a block").file;
    assert!(db.wb().seal_segment(filled).expect("the segment is sealed"));
    leave_a_replacement_in_doubt(fs, db, 0);
    (db.segments)
        .commit(&mut db.wb().manifest().expect("the manifest"))
        .expect_err("the replacement fails");
    assert!(in_doubt(db));
    fail_the_syncs_of_the_log(fs, db);
    (in_force, before)
}

/// **A checkpoint that is given up because the log could not be made
/// durable ends with that error, and its file is deleted**, also when the
/// settling of a manifest in doubt ends with an error of a store: here
/// the segment store answers [`SegmentStoreError::NotDeleted`], and the
/// manifest is settled all the same.
#[test]
fn a_checkpoint_given_up_for_the_log_ends_with_the_logs_error_and_is_deleted() {
    let (fs, db) = store(ONE_LOG_FILE);
    let (in_force, before) = a_checkpoint_to_give_up(&fs, &db);
    let written = db.stats().checkpoint_bytes;

    let error = db.wb().checkpoint().expect_err("the log is not durable");
    assert!(
        matches!(error, FlushError::Log(LogError::SyncFailed { .. })),
        "{error}"
    );
    assert!(!in_doubt(&db));
    assert_eq!(db.state.in_force(), in_force);
    assert!(db.stats().checkpoint_bytes > written, "one was written");
    assert_eq!(table_files(&db), before, "the checkpoint's file is deleted");
}

/// Where the manifest cannot be opened again, the checkpoint that is
/// given up ends with the log's error too, and its file stays, named by
/// no manifest: the next session removes it.
#[test]
fn a_checkpoint_given_up_under_a_manifest_in_doubt_stays_until_the_next_session() {
    let (fs, db) = store(ONE_LOG_FILE);
    let ids = [page(1)];
    let (in_force, before) = a_checkpoint_to_give_up(&fs, &db);
    let manifest = db.path.join(ManifestStore::FILE_NAME);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Open)
            .at(&manifest)
            .forever(),
    );

    let error = db.wb().checkpoint().expect_err("the log is not durable");
    assert!(
        matches!(error, FlushError::Log(LogError::SyncFailed { .. })),
        "{error}"
    );
    assert!(in_doubt(&db));
    assert_eq!(db.state.in_force(), in_force);
    assert_eq!(table_files(&db), before + 1, "the checkpoint's file stays");

    // The next session has the checkpoint of before and no other file.
    let latest = db.latest();
    let (vfs, path) = (db.vfs.clone(), db.path.clone());
    drop(db);
    fs.clear_rules();
    let found = recover::recover(vfs.clone(), &path, &ids);
    assert_eq!(found.landed, latest);
    the_store_recovers_the_same(vfs.clone(), &path, &ids, &found);
    let left = vfs.read_dir(&FileNames::PAGE_TABLE.dir(&path));
    assert_eq!(left.expect("a listing").len(), before);
}

/// The control: with a manifest that can be had, the checkpoint that was
/// written is deleted at once, and the error is the log's.
#[test]
fn a_checkpoint_given_up_for_the_log_is_deleted_when_the_manifest_can_be_had() {
    let (fs, db) = store(ONE_LOG_FILE);
    db.commit(&[insert((1, 1), b"k", inline(b"v"))]);
    installed(db.wb().checkpoint().expect("a checkpoint"));
    let (in_force, before) = (db.state.in_force(), table_files(&db));
    db.commit(&[hash((1, 1), 2)]);
    fail_the_syncs_of_the_log(&fs, &db);
    let written = db.stats().checkpoint_bytes;

    let error = db.wb().checkpoint().expect_err("the log is not durable");
    assert!(
        matches!(error, FlushError::Log(LogError::SyncFailed { .. })),
        "{error}"
    );
    assert_eq!(db.state.in_force(), in_force);
    assert!(db.stats().checkpoint_bytes > written, "one was written");
    assert_eq!(table_files(&db), before, "the checkpoint's file is deleted");
}

/// **What ended a pass's walk over the pending values is returned, also
/// when the cover of what it had appended fails too.** Two pages have a
/// value out of line each, and a value-log file is full after one. The
/// first value is appended. The file for the second cannot be made, which
/// ends the walk, and then the manifest that covers the first cannot be
/// replaced. The pass ends with the error of the file that could not be
/// made.
#[test]
fn what_ended_the_walk_over_pending_values_is_returned_though_the_cover_fails() {
    let (fs, mut db) = store(ONE_LOG_FILE);
    let values = {
        let manifest = db.manifest.lock().expect("the manifest");
        crate::values::ValueStore::open(&manifest, &[]).expect("the value log")
    };
    db.values = values.with_target_file_len(700);
    db.commit(&[insert((9, 1), b"first", long(9, 900))]);
    db.wb().pass().expect("a pass");
    let first = db.pointer(page(9), slot(1)).file();
    let file = |after: u32| FileId::new(first.get() + after).expect("a file id");

    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
    ]);
    let unmade = FileNames::VALUE_LOG.path(&db.path, file(2));
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Open)
            .at(&unmade)
            .forever(),
    );
    // The file of the first value is named by the first replacement of
    // the pass, and the cover of that value is the second.
    leave_a_replacement_in_doubt(&fs, &db, 1);

    let error = db.wb().pass().expect_err("the pass fails");
    let said = error.to_string();
    assert!(
        said.contains(&FileNames::VALUE_LOG.file_name(file(2))),
        "the pass ended with: {said}"
    );
    assert_eq!(fs.stats().faults, 2, "both steps failed");
}
