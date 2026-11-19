//! What recovery refuses: a directory that is not what a session of the
//! store leaves, whatever the crash. Each is reported with its own error,
//! and nothing is repaired.

use std::error::Error;

use lignin_format::wal::CarriedValue;

use super::*;
use crate::{
    segment::FileNames,
    table::PageTableError,
    values::{Miscount, ValueStoreError},
    wal::LogError,
};

/// The error a recovery of `dir` ends with.
fn refused(dir: &Dir) -> RecoveryError {
    match dir.recover() {
        Ok(store) => panic!("the store recovered: {:?}", store.report),
        Err(error) => error,
    }
}

/// A store with two pages that commit 1 made and write-back wrote, and a
/// session on it.
fn two_pages(dir: &Dir) -> Session<'_> {
    let mut session = Session::begin(dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", inline(b"two")),
    ]);
    session.write_back(1, 1);
    session.write_back(2, 1);
    session
}

/// Cuts the file `path` to `len` bytes, as a crash that loses its end.
fn cut(path: &Path, len: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("the file");
    file.set_len(len).expect("cut");
}

/// **LG29.** The item that ends at the place of the checkpoint's sample is
/// the mark of that checkpoint.
#[test]
fn a_sample_whose_place_is_not_the_end_of_its_mark_is_refused() {
    // No mark ends there: the place is the end of a group.
    let dir = Dir::new();
    let mut session = two_pages(&dir);
    let group = session.commit(&[hash((1, 1), 2)]);
    session.write_back(1, 2);
    let sample = session.sample_at(group.end, &[]);
    session.checkpoint_with(2, &sample, None);
    session.kill();
    let error = refused(&dir);
    assert!(
        matches!(error, RecoveryError::MarkMissing { place } if place == group.end),
        "{error}"
    );
    assert!(error.is_damage() && error.source().is_none());
    assert_eq!(
        error.to_string(),
        format!(
            "no checkpoint mark ends at offset {} of log file 1, the place of the table \
             checkpoint's sample",
            group.end.offset()
        )
    );

    // A mark ends there, of a checkpoint that covers another version.
    let dir = Dir::new();
    let mut session = two_pages(&dir);
    session.commit(&[hash((1, 1), 2)]);
    session.write_back(1, 2);
    let mark = session.store.log.append_mark(version(1)).expect("a mark");
    let sample = session.sample_at(mark.end, &[]);
    session.checkpoint_with(2, &sample, None);
    session.kill();
    let error = refused(&dir);
    assert!(
        matches!(
            error,
            RecoveryError::WrongMark { place, mark: of, covers }
                if place == mark.end && of == version(1) && covers == version(2)
        ),
        "{error}"
    );
    assert!(error.is_damage());
    assert!(error
        .to_string()
        .contains("covers commit 1, and the table checkpoint covers commit 2"));
}

/// **K4.** A checkpoint is named only once the log is durable through
/// what it covers. A log that ends before that commit is not the log of
/// the checkpoint.
#[test]
fn a_log_that_ends_before_what_the_checkpoint_covers_is_refused() {
    let dir = Dir::new();
    let mut session = two_pages(&dir);
    let second = session.commit(&[hash((1, 1), 2)]);
    session.write_back(1, 2);
    session.checkpoint(2);
    session.kill();
    // The manifest does not vouch for the group of commit 2, and the
    // crash takes it.
    cut(
        &FileNames::WAL.path(&dir.path, second.at.file()),
        second.at.offset(),
    );
    let error = refused(&dir);
    assert!(
        matches!(
            error,
            RecoveryError::LogBehindCheckpoint { covers, landed }
                if covers == version(2) && landed == version(1)
        ),
        "{error}"
    );
    assert!(error.is_damage());
    assert_eq!(
        error.to_string(),
        "the log ends at commit 1, and the table checkpoint covers commit 2"
    );
}

/// **K1.** The log is kept from the group of the commit after the one the
/// checkpoint covers. A log that goes on from a later commit has lost
/// commits.
#[test]
fn a_log_that_begins_after_the_commit_it_has_to_begin_with_is_refused() {
    for with_a_group in [true, false] {
        let dir = Dir::new();
        let small = RecoveryOptions {
            log: LogOptions {
                target_file_len: 200,
                ..LogOptions::default()
            },
            ..options(dir.size)
        };
        let mut session = Session::begin_with(&dir, Model::default(), &small);
        session.commit(&[insert((1, 1), b"a", inline(&[7; 100]))]);
        session.commit(&[insert((1, 2), b"b", inline(&[8; 100]))]);
        session.turn();
        if with_a_group {
            session.commit(&[hash((1, 1), 3)]);
        }
        // The first file is dropped although no checkpoint covers its
        // commits.
        let store = &mut session.store;
        store.log.sync().expect("durable");
        let kept_from = store.log.end().expect("the end");
        // Write-back holds the whole log while no checkpoint is named,
        // which is what keeps the file. This session breaks that rule:
        // it gives the hold up.
        drop(std::mem::replace(
            &mut store.flush,
            FlushState::new(FlushConfig::new(dir.size)),
        ));
        let dropped = store.log.drop_before(kept_from).expect("dropped");
        assert_eq!(dropped.len(), 1);
        store.log.commit(&mut store.manifest).expect("named");
        session.kill();

        let error = refused(&dir);
        let found = version(if with_a_group { 3 } else { 2 });
        assert!(
            matches!(
                error,
                RecoveryError::CommitsMissing { covers, found: at }
                    if covers == CommitVersion::EMPTY && at == found
            ),
            "{error}"
        );
        assert!(error.is_damage());
        assert_eq!(
            error.to_string(),
            format!(
                "the log goes on from commit {found}, and the table checkpoint covers commit \
                 0: the commits between them are missing"
            )
        );
    }
}

/// **K4, FL4.** The log goes first: a link of a later version than the
/// one recovery lands on is a broken rule, and recovery discards nothing
/// to make it fit.
#[test]
fn a_link_above_the_version_recovery_lands_on_is_refused() {
    let dir = Dir::new();
    let mut session = two_pages(&dir);
    session.commit(&[hash((2, 1), 2)]);
    // A block of page 1 that is said to be of commit 5, and a checkpoint
    // that is told the log is durable through it.
    session.write_back(1, 5);
    let sample = session.sample(1);
    let synced = session.store.log.sync().expect("durable");
    let told = LogDurable {
        version: version(5),
        ..synced
    };
    session.checkpoint_with(1, &sample, Some(told));
    session.kill();

    let error = refused(&dir);
    assert!(
        matches!(
            error,
            RecoveryError::LinksAfterLanding { links: 1, landed } if landed == version(2)
        ),
        "{error}"
    );
    assert!(error.is_damage());
    assert_eq!(
        error.to_string(),
        "the table checkpoint holds 1 links of versions after 2, the last commit of the log"
    );
}

/// **K11.** A change that was dropped for want of an image is settled by
/// a later commit that frees the page or makes it again. If none does,
/// the checkpoint or the log is damaged.
#[test]
fn a_change_to_a_page_that_is_nowhere_is_refused() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", inline(b"two")),
        insert((4, 1), b"d", inline(b"four")),
    ]);
    // The checkpoint covers commit 1 and has only page 1.
    session.write_back(1, 1);
    session.checkpoint(1);
    session.commit(&[hash((2, 1), 2), hash((4, 1), 2)]);
    session.kill();

    let error = refused(&dir);
    assert!(
        matches!(
            error,
            RecoveryError::ChangesWithoutPage { page: of, pages: 2 } if of == page(2)
        ),
        "{error}"
    );
    assert!(error.is_damage());
    assert_eq!(
        error.to_string(),
        "the log changes 2 pages that the table checkpoint has no image of and that no later \
         commit freed or made again, page 2 among them"
    );
}

/// A logged change that does not apply to its page, as the checkpoint
/// and the changes before it give the page.
#[test]
fn a_change_that_does_not_apply_to_its_page_is_refused() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", inline(b"one"))]);
    // The block is the page of commit 1, and its link says commit 2.
    session.write_back(1, 2);
    session.commit(&[insert((1, 2), b"b", inline(b"two"))]);
    session.checkpoint(2);
    let third = session.commit(&[hash((1, 2), 3)]);
    session.kill();

    let error = refused(&dir);
    assert!(
        matches!(
            &error,
            RecoveryError::Change { page: of, at, .. } if *of == page(1) && *at == third.at
        ),
        "{error}"
    );
    assert!(error.is_damage() && error.source().is_some());
    assert!(error.to_string().starts_with(&format!(
        "the item at offset {} of log file 1 does not apply to page 1: ",
        third.at.offset()
    )));
}

/// A cell must not be given a pointer to a value that the value log does
/// not have: a step stores a pointer only under a manifest that covers
/// the value (FL7).
#[test]
fn a_pointer_to_a_value_that_is_not_there_is_refused() {
    // The file is there, and ends before the value.
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", long(1, 600))]);
    let (step, _) = session.place((1, 1));
    let beyond = ValuePointer::new(step.to().file(), 1 << 30, 600).expect("a pointer");
    let moved = ValueMove::in_tree(page(1), slot(1), ValuePlace::Placed(step.to()), beyond)
        .expect("a move");
    let mut record = Vec::new();
    moved.write(&mut record).expect("a record");
    let appended = session.store.log.append_change(&record).expect("appended");
    session.kill();
    let error = refused(&dir);
    assert!(
        matches!(
            &error,
            RecoveryError::ValueNotThere { pointer, at, why: Miscount::OutOfBounds }
                if *pointer == beyond && *at == appended.at
        ),
        "{error}"
    );
    assert!(error.is_damage() && error.source().is_none());
    assert_eq!(
        error.to_string(),
        format!(
            "the item at offset {} of log file 1 makes a cell hold a value at offset \
             1073741824 of value-log file 1: it ends past the end of its file",
            appended.at.offset()
        )
    );

    // The file is not there at all, and the counts say it has a live
    // value: the value log says so when it is asked for the store.
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", long(1, 600))]);
    let pending = *session
        .model
        .pending
        .keys()
        .next()
        .expect("a pending value");
    let pending = PendingValue::new(pending, 600).expect("a pending value");
    let nowhere = ValuePointer::new(lignin_format::FileId::new(9).expect("an id"), 20, 600)
        .expect("a pointer");
    let placed = ValueMove::in_tree(page(1), slot(1), ValuePlace::Pending(pending), nowhere)
        .expect("a step");
    let mut record = Vec::new();
    placed.write(&mut record).expect("a record");
    session.store.log.append_change(&record).expect("appended");
    session.kill();
    let error = refused(&dir);
    assert!(
        matches!(
            &error,
            RecoveryError::Values(ValueStoreError::LiveValuesInMissingFile { values: 1, .. })
        ),
        "{error}"
    );
    assert!(!error.is_damage() && error.source().is_some());
    assert!(error
        .to_string()
        .starts_with("the value log was not recovered: "));
}

/// **A release that no count included** leaves its file kept for good,
/// which is the value log's own answer to a count that is wrong. Recovery
/// goes on, and says how many there were.
#[test]
fn a_release_that_no_count_included_is_reported_and_its_file_is_kept() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", inline(b"one"))]);
    // A cell is given a pointer that no step counted, and is removed.
    let pointer = session.append_value(1, &[5; 400]);
    session.commit(&[insert((1, 2), b"b", Value::At(pointer))]);
    let counts = session.store.values.step().expect("the counts");
    let mut change = ValueChange::new();
    change.acquire(pointer);
    counts.apply(&change, version(2)).expect("counted here");
    session.commit(&[remove((1, 2))]);
    let model = session.kill();

    let store = dir.open();
    assert_eq!(store.report.miscounts, 1);
    let count = store.values.count(pointer.file()).expect("the file");
    assert!(count.miscounted, "{count:?}");
    assert_eq!(count.values, 0);
    let frame = frame_of(&store, page(1)).expect("the page");
    let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
    assert_eq!(Some(tree.to_image()), model.image(page(1)));
}

/// A pending value that the log names and does not carry cannot be given
/// its bytes: nothing keeps the file the name leads to (LG24), and no
/// session of this version writes such a group.
#[test]
fn a_pending_value_that_the_log_names_is_refused() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((2, 1), b"a", inline(b"one"))]);
    let source = session.append_value(2, &[9; 500]);
    let store = &session.store;
    let at = store.log.next_group().expect("a place");
    let mut bytes = Vec::new();
    let mut group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
    let carried = CarriedValue::new(page(3), page(2), source);
    let pending = group.carried(&carried).expect("a carried value");
    group
        .tree_run_with(page(3), Some(0), |run| {
            let cell = CellContent {
                key: b"carried",
                feature: &[],
                value_hash: &[1; 32],
                kv_hash: &[2; 32],
                left: None,
                right: None,
                hosted_roots: &[],
                value: CellValue::Pending(pending),
                fragment_root: true,
            }
            .encode()?;
            run.push(&Delta::Insert {
                slot: slot(1),
                cell: TreeCell::decode(&cell, V1)?,
            })
        })
        .expect("a run");
    group.finish();
    store.log.append_group(&at, &bytes).expect("appended");
    session.kill();

    let error = refused(&dir);
    assert!(
        matches!(error, RecoveryError::NamedValue { page: of } if of == page(3)),
        "{error}"
    );
    assert!(error.is_damage());
    assert_eq!(
        error.to_string(),
        "a pending value of page 3 is named by the log and not carried in it"
    );
}

/// Files that are damaged or gone are the errors of the parts that read
/// them, with the part named.
#[test]
fn a_file_that_is_damaged_or_gone_is_the_error_of_the_part_that_reads_it() {
    let build = |dir: &Dir| {
        let mut session = two_pages(dir);
        session.commit(&[insert((3, 1), b"c", long(1, 500))]);
        session.place((3, 1));
        session.checkpoint(1);
        // The manifest vouches for the log up to here.
        let store = &mut session.store;
        store.log.commit(&mut store.manifest).expect("named");
        let second = session.commit(&[hash((1, 1), 2)]);
        session.kill();
        second
    };
    let file_of = |dir: &Dir, names: &FileNames| {
        let mut files: Vec<_> = std::fs::read_dir(names.dir(&dir.path))
            .expect("the directory")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        files.sort();
        files.swap_remove(0)
    };

    // A block of a page that replay has to change.
    let dir = Dir::new();
    let second = build(&dir);
    let segment = file_of(&dir, &FileNames::SEGMENT);
    let mut bytes = std::fs::read(&segment).expect("the segment");
    bytes[40] ^= 0x10;
    std::fs::write(&segment, bytes).expect("written");
    let error = refused(&dir);
    assert!(
        matches!(&error, RecoveryError::Page { page: of, at, .. } if *of == page(1) && *at == second.at),
        "{error}"
    );
    assert!(!error.is_damage() && error.source().is_some());
    assert!(error.to_string().starts_with(&format!(
        "page 1, which the item at offset {} of log file 1 changes, could not be read: ",
        second.at.offset()
    )));

    // The table checkpoint.
    let dir = Dir::new();
    build(&dir);
    let checkpoint = file_of(&dir, &FileNames::PAGE_TABLE);
    let len = std::fs::metadata(&checkpoint).expect("the file").len();
    cut(&checkpoint, len - 3);
    let error = refused(&dir);
    assert!(
        matches!(
            &error,
            RecoveryError::Table(PageTableError::FileTooShort { .. })
        ),
        "{error}"
    );
    assert!(error
        .to_string()
        .starts_with("the table checkpoint did not load: "));
    assert!(error.source().is_some());

    // A segment.
    let dir = Dir::new();
    build(&dir);
    std::fs::remove_file(file_of(&dir, &FileNames::SEGMENT)).expect("removed");
    let error = refused(&dir);
    assert!(matches!(&error, RecoveryError::Segments(_)), "{error}");
    assert!(error.to_string().starts_with("recovery could not go on: "));
    assert!(error.source().is_some());

    // A value-log file.
    let dir = Dir::new();
    build(&dir);
    std::fs::remove_file(file_of(&dir, &FileNames::VALUE_LOG)).expect("removed");
    let error = refused(&dir);
    assert!(matches!(&error, RecoveryError::Values(_)), "{error}");
    assert!(error.source().is_some());

    // The log, below the length the manifest vouches for.
    let dir = Dir::new();
    build(&dir);
    let log = file_of(&dir, &FileNames::WAL);
    let mut bytes = std::fs::read(&log).expect("the log");
    bytes[60] ^= 0x01;
    std::fs::write(&log, bytes).expect("written");
    let error = refused(&dir);
    assert!(
        matches!(&error, RecoveryError::Log(LogError::Damaged { .. })),
        "{error}"
    );
    assert!(error.to_string().starts_with("the log was not recovered: "));
    assert!(!error.is_damage() && error.source().is_some());
}
