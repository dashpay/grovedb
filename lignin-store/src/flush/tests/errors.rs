//! What write-back does when something fails or is refused: the errors
//! say what, nothing committed is lost, and the call can be made again.
//! `faults.rs` fails every file operation in turn. These are the cases
//! that have a behavior of their own to show.

use std::error::Error as _;

use lignin_format::{FileId, FormatError};
use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};

use super::*;
use crate::{
    cache::{CacheError, StepRefusal},
    segment::{tests::sim::db_dir, BlockLocation, FileNames, SegmentStoreError},
    table::{LinkError, PageTableError},
    values::ValueStoreError,
    wal::LogError,
};

fn file_id(raw: u32) -> FileId {
    FileId::new(raw).expect("a file id")
}

/// Every error says which page, block or place it is about, and names the
/// error under it where there is one.
#[test]
fn every_error_says_what_failed() {
    let place = |offset| LogPlace::new(file_id(3), offset).expect("a place");
    let location = BlockLocation {
        file: file_id(7),
        offset: 4096,
        len: 64,
    };
    let dir = || PathBuf::from("/db");
    let cases: Vec<(FlushError, &str, bool)> = vec![
        (
            FlushError::FrameSize {
                page_size: 4096,
                frame_len: 8192,
            },
            "the store's pages are 4096 bytes and the pool's frames 8192",
            false,
        ),
        (
            SegmentStoreError::ManifestInDoubt { dir: dir() }.into(),
            "write-back: ",
            true,
        ),
        (
            PageTableError::Store(SegmentStoreError::ManifestInDoubt { dir: dir() }).into(),
            "write-back: ",
            true,
        ),
        (ValueStoreError::ReadOnly.into(), "write-back: ", true),
        (
            LogError::NoActiveFile.into(),
            "write-back: the log has no file that takes items",
            true,
        ),
        (CacheError::CapacityTooLarge.into(), "write-back: ", true),
        (
            FlushError::page(page(5))(FormatError::BadMagic),
            "page 5 cannot be written back: ",
            true,
        ),
        (
            FlushError::Link {
                page: page(5),
                location,
                source: LinkError::DeltaWithoutBase,
            },
            "the page table refused the block of page 5 at offset 4096 of segment 7: ",
            true,
        ),
        (
            FlushError::StepRefused {
                page: page(5),
                source: StepRefusal::NotCurrent,
            },
            "a page step on page 5 was refused after its record was written: ",
            true,
        ),
        (
            FlushError::PendingValue {
                page: page(5),
                born: version(9),
            },
            "a pending value of page 5 as born at version 9 has no bytes of its length",
            false,
        ),
        (
            FlushError::SampleWentBack {
                before: place(900),
                taken: place(400),
            },
            "a sample of the value counts was taken at offset 400 of log file 3, before the \
             sample at offset 900 of log file 3",
            false,
        ),
        (
            FlushError::Regroup {
                page: page(5),
                source: "no room".into(),
            },
            "the values of page 5 were not regrouped: no room",
            true,
        ),
        (
            FlushError::SegmentsNotDurable,
            "a sync of a segment failed in this session, and no table checkpoint is installed \
             until the store is opened again",
            false,
        ),
    ];
    for (error, text, has_source) in cases {
        let shown = error.to_string();
        assert!(shown.starts_with(text), "{shown}");
        assert_eq!(error.source().is_some(), has_source, "{shown}");
        if let Some(source) = error.source() {
            assert!(shown.ends_with(&source.to_string()), "{shown}");
        }
    }
}

/// Write-back is refused for a pool whose frames are not the store's
/// pages.
#[test]
fn a_pool_of_another_page_size_is_refused() {
    let db = Db::new();
    let other = PageSize::new(Db::SIZE.get() * 2).expect("a page size");
    let stores = db.wb().stores;
    let error = WriteBack::new(stores, &db.state, other).expect_err("the sizes differ");
    assert!(
        matches!(
            error,
            FlushError::FrameSize { page_size, frame_len }
                if page_size == other.get() && frame_len == Db::SIZE.get()
        ),
        "{error}"
    );
}

/// A page step that finds the log with no file that takes items is
/// refused with what had left the log so when the step asked: under the
/// lock on the counts, where no turn gives the log a file and no append
/// of another fails. Here the log is rolled as soon as the step has
/// given its locks back, and a page step of another is refused its
/// record in the file the log turned to. The pass fails with the refusal
/// that kept its own record out, and not with the later one.
#[test]
fn a_page_step_is_refused_with_what_had_left_the_log_without_a_file_when_it_asked() {
    use std::{cell::RefCell, rc::Rc};

    use crate::flush::place::AFTER_A_REFUSED_STEP;

    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    let db = Arc::new(db);
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    // The next record that is appended to the file the log ends in is
    // refused. Returns that file.
    let refuse_a_record = {
        let fs = fs.clone();
        move |db: &Db| {
            let file = db.log.end().expect("the log has a file").file();
            let path = FileNames::WAL.path(db_dir(), file);
            let record = FaultRule::new(Fault::Enospc).on(OpKind::Append).at(&path);
            fs.add_rule(record.torn_append(false));
            path
        }
    };
    // The page step of the value is refused its record: the value stays
    // ready, and the log has no file that takes items.
    let first = refuse_a_record(&db);
    let error = db.wb().pass().expect_err("the record of the step");
    assert!(
        matches!(error, FlushError::Log(LogError::TakenBack { .. })),
        "{error}"
    );
    assert!(db.log.is_due());

    let second = Rc::new(RefCell::new(None));
    AFTER_A_REFUSED_STEP.with(|hook| {
        let (db, second) = (db.clone(), second.clone());
        *hook.borrow_mut() = Some(Box::new(move || {
            assert!(db.wb().roll_log().expect("a roll beside the pass"));
            *second.borrow_mut() = Some(refuse_a_record(&db));
            let (slot, pending) = db.pending(page(1))[0];
            let file = FileId::new(1).expect("a file id");
            let to = ValuePointer::new(file, 4096, pending.value_len()).expect("a pointer");
            let step = ValueMove::in_tree(page(1), slot, ValuePlace::Pending(pending), to);
            let mut bytes = Vec::new();
            step.expect("a move").write(&mut bytes).expect("a record");
            let error = db.wb().append_change(&bytes);
            let error = error.expect_err("the record of the other step");
            assert!(matches!(error, LogError::TakenBack { .. }), "{error}");
            db.log.settle().expect("the log is settled");
        }));
    });
    let error = db
        .wb()
        .pass()
        .expect_err("the log has no file for the record");
    let FlushError::Log(LogError::NoActiveFileSince { path, .. }) = &error else {
        panic!("{error}");
    };
    assert_eq!(path, &first, "{error}");
    let second = second
        .borrow()
        .clone()
        .expect("the step gave its locks back");
    assert_ne!(first, second);
    let Some(LogError::NoActiveFileSince { path: now, .. }) = db.log.refusal() else {
        panic!("the log has kept no refusal");
    };
    assert_eq!(now, second, "what the log has kept is the later refusal");

    // A turn rolls the log, and the step is made.
    let tick = db.wb().tick().expect("a turn of the loop");
    assert!(tick.rolled);
    assert_eq!(db.pointers(page(1)).len(), 1, "the value is placed");
    db.check_cold();
}

/// The log fails the record of the page step that places a value, with
/// the append taken back or left torn in the file. Nothing is installed,
/// the value keeps its hold, the pass returns what the log answered, and
/// the log takes nothing until write-back has rolled it
/// (`transactions.md` CM9 item 5). Then the step is made.
#[test]
fn a_page_step_whose_record_the_log_fails_is_made_once_the_log_is_rolled() {
    let mut put_off = 0;
    for torn in [false, true] {
        let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
        db.commit(&[insert((1, 1), b"a", long(1, 900))]);
        let log_file = db.log.end().expect("the log has a file").file();
        let path = FileNames::WAL.path(db_dir(), log_file);
        fs.add_rule(
            FaultRule::new(Fault::Eio)
                .on(OpKind::Append)
                .at(path)
                .torn_append(torn),
        );

        let (pass, passed) = db.wb().pass_through();
        assert_eq!(fs.stats().faults, 1);
        // An append that is left torn may have left the whole record: the
        // log then has it, and the step was made. Otherwise:
        if pass.placed == 0 {
            put_off += 1;
            let error = passed.expect_err("the record of the step was refused");
            let refused = matches!(
                error,
                FlushError::Log(LogError::TakenBack { .. } | LogError::NotAppended { .. })
            );
            assert!(refused, "{torn}: {error}");
            assert_eq!((pass.written, pass.waiting), (0, 1), "{torn}");
            let stats = db.stats();
            assert_eq!((stats.values_appended, stats.values_placed), (1, 0));
            assert_eq!(db.pending(page(1)).len(), 1);
            assert_eq!(db.wb().place_ready().expect("put off again"), 0);
        } else {
            assert!(torn);
            passed.expect("a pass");
        }
        // Either way the file takes no more items, of a page step or of
        // anyone, until the log is rolled.
        assert!(db.log.is_due());
        db.check_warm();

        let tick = db.wb().tick().expect("a turn of the loop");
        assert!(tick.rolled, "{torn}");
        assert_eq!(pass.placed + tick.pass.placed, 1);
        assert_eq!(
            (pass.written + tick.pass.written, tick.pass.waiting),
            (1, 0)
        );
        assert!(db.log.end().expect("the log has a file").file() > log_file);
        // The value was appended once, and is counted once.
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
        db.check_cold();
        db.reopen().check_cold();
    }
    assert!(put_off > 0);
}

/// The log fails the record of a page step and cannot find out what
/// became of it. The record may be in the log, so nothing is installed,
/// the value keeps its hold, and the call says that the log takes nothing
/// more in this session: every call after it says so too.
#[test]
fn a_page_step_whose_record_is_unsettled_is_reported_and_keeps_its_hold() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", inline(b"v")),
    ]);
    let log_file = db.log.end().expect("the log has a file").file();
    let path = FileNames::WAL.path(db_dir(), log_file);
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Append).at(&path));
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Len).at(&path));

    let unsettled = |result: Result<usize, FlushError>| {
        let error = result.expect_err("the log is unsettled");
        assert!(
            matches!(error, FlushError::Log(LogError::Unsettled { .. })),
            "{error}"
        );
    };
    unsettled(db.wb().pass().map(|pass| pass.placed));
    assert_eq!(fs.stats().faults, 2);
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.values_placed), (1, 0));
    assert_eq!(db.pending(page(1)).len(), 1);
    assert_eq!(db.state.placing.lock().expect("what is ready").len(), 1);
    // The page that had no value to place was written all the same.
    assert_eq!(db.shape(page(2)), "B");
    db.check_warm();

    unsettled(db.wb().place_ready());
    unsettled(db.wb().tick().map(|tick| tick.pass.placed));
    assert_eq!(db.state.placing.lock().expect("what is ready").len(), 1);
    assert_eq!(db.stats().values_given_up, 0);
}

/// A segment fails an append. What it holds stays, the page stays owed,
/// and the next pass writes it to another segment.
#[test]
fn a_segment_that_fails_an_append_is_left_for_another() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    db.commit(&[
        insert((1, 1), b"a", inline(b"v")),
        insert((2, 1), b"b", inline(b"w")),
    ]);
    db.wb().pass().expect("a pass");
    let first = db.chain(page(1))[0].location().expect("a block").file;
    db.commit(&[hash((1, 1), 1), hash((2, 1), 1)]);

    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Append)
            .at(FileNames::SEGMENT.path(db_dir(), first)),
    );
    let error = db.wb().pass().expect_err("the append fails");
    assert!(
        matches!(error, FlushError::Segments(SegmentStoreError::Io { .. })),
        "{error}"
    );
    assert_eq!(db.state.unwritten(), 2);
    db.check_warm();

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (2, 0));
    for id in [1, 2].map(page) {
        let chain = db.chain(id);
        assert_eq!(db.shape(id), "Bd");
        assert_eq!(chain[0].location().expect("a block").file, first);
        assert!(chain[1].location().expect("a block").file > first);
    }
    db.check_cold();
    // The segment that failed was synced, so a checkpoint can name it.
    assert_eq!(
        installed(db.wb().checkpoint().expect("a checkpoint")).0,
        db.latest()
    );
    db.reopen().check_cold();
}

/// A sync of a segment fails when a checkpoint is to be installed. Its
/// blocks cannot be made durable any more, so no checkpoint is installed
/// in this session, and the log keeps everything. Readers and commits go
/// on, and so does write-back, in another segment.
#[test]
fn after_a_failed_sync_of_a_segment_no_checkpoint_is_installed() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    assert_eq!(
        installed(db.wb().checkpoint().expect("a checkpoint")).0,
        version(1)
    );
    let first = db.chain(page(1))[0].location().expect("a block").file;
    db.commit(&[hash((1, 1), 1)]);

    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(FileNames::SEGMENT.path(db_dir(), first)),
    );
    let error = db.wb().checkpoint().expect_err("the sync fails");
    assert!(!matches!(error, FlushError::SegmentsNotDurable), "{error}");
    for _ in 0..2 {
        let error = db.wb().checkpoint().expect_err("no checkpoint any more");
        assert!(matches!(error, FlushError::SegmentsNotDurable), "{error}");
    }
    assert_eq!(db.state.in_force().map(|named| named.0), Some(version(1)));
    assert_eq!(db.stats().checkpoints, 1);

    // The blocks of the segment that failed are still read.
    db.check_cold();
    db.commit(&[hash((1, 1), 2), insert((2, 1), b"b", inline(b"w"))]);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (2, 0));
    assert!(db.chain(page(2))[0].location().expect("a block").file > first);
    db.check_cold();
    // And the log has every commit since the checkpoint in force.
    db.wb().sync_log().expect("the log syncs");
    assert_eq!(db.log.durable().version, db.latest());
}

/// A value-log file fails its sync. The values it took are given up, with
/// their holds, and are appended to another file by the next pass.
#[test]
fn a_value_log_file_that_cannot_be_synced_is_left_for_another() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    db.wb().pass().expect("a pass");
    let first = db.pointer(page(1), slot(1)).file();
    db.commit(&[insert((1, 2), b"b", long(2, 800))]);

    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(FileNames::VALUE_LOG.path(db_dir(), first)),
    );
    let error = db.wb().pass().expect_err("the sync fails");
    assert!(matches!(error, FlushError::Values(_)), "{error}");
    assert_eq!(db.pending(page(1)).len(), 1);
    db.check_warm();

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.written, pass.waiting), (1, 1, 0));
    assert!(db.pointer(page(1), slot(2)).file() > first);
    assert_eq!(db.stats().values_appended, 3);
    assert_eq!(db.live_values(), 2);
    db.check_cold();
    db.reopen().check_cold();
}

/// Puts other bytes, of another length, in place of those that are with
/// the frame of `id` for its first pending value.
pub(super) fn shorten_the_value(db: &Db, id: PageId) {
    let (_, pending) = db.pending(id)[0];
    let step = db.pool.page_step(id);
    let old = step.current().expect("the page");
    let mut blank = db.pool.blank();
    blank.copy_from(&old);
    blank.attach(pending.id(), Arc::from(&b"too short"[..]));
    step.replace(&old, blank).expect("replaced");
}

/// A pending value whose bytes are with its frame in another length than
/// its cell states is not written anywhere. The pass reports it and goes
/// on: the page stays as it is, and the other pages are written.
#[test]
fn a_pending_value_without_bytes_of_its_length_is_refused() {
    let db = Db::new();
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 800)),
        insert((3, 1), b"c", inline(b"v")),
    ]);
    shorten_the_value(&db, page(1));
    for _ in 0..2 {
        let error = db.wb().pass().expect_err("the bytes are not the value");
        assert!(
            matches!(error, FlushError::PendingValue { page: id, born } if id == page(1) && born == version(1)),
            "{error}"
        );
        let stats = db.stats();
        assert_eq!(
            (
                stats.values_appended,
                stats.values_placed,
                stats.base_blocks
            ),
            (1, 1, 2)
        );
        assert_eq!(db.state.unwritten(), 1);
        assert_eq!(db.pending(page(1)).len(), 1);
    }
    // No checkpoint can cover the commit of that page.
    let error = db
        .wb()
        .checkpoint()
        .expect_err("the page cannot be written");
    assert!(matches!(error, FlushError::PendingValue { .. }), "{error}");
    assert_eq!(db.stats().checkpoints, 0);
}

/// A frame whose bytes are no page cannot be written back. The pass says
/// which page it is, leaves it, and writes the pages behind it.
#[test]
fn a_frame_that_is_no_page_is_left_and_the_pages_behind_it_are_written() {
    let db = Db::new();
    db.commit(&[
        insert((1, 1), b"a", inline(b"v")),
        insert((2, 1), b"b", inline(b"w")),
        insert((3, 1), b"c", long(1, 700)),
    ]);
    {
        let step = db.pool.page_step(page(1));
        let old = step.current().expect("the page");
        let mut blank = db.pool.blank();
        blank.fill(0);
        step.replace(&old, blank).expect("replaced");
    }
    let error = db.wb().pass().expect_err("the frame is no page");
    assert!(
        matches!(error, FlushError::Page { page: id, .. } if id == page(1)),
        "{error}"
    );
    let stats = db.stats();
    assert_eq!((stats.base_blocks, stats.values_placed), (2, 1));
    assert_eq!(db.state.unwritten(), 1);
    assert_eq!(
        (db.shape(page(2)), db.shape(page(3))),
        ("B".into(), "B".into())
    );
}

/// A commit frees a page while write-back writes its block. The page table
/// takes no link behind the free link (`page-table.md` T18): the block is
/// dead space, and nothing of the page is owed any more.
#[test]
fn a_page_that_is_freed_while_its_block_is_written_gets_no_link() {
    /// Stands where the tree layer does, between the look at the chain
    /// and the block, and has a commit free the page there.
    struct Frees<'d>(&'d Db);
    impl PageValues for Frees<'_> {
        fn scan_order(&self, _: &[u8], _: &mut Vec<SlotId>) {}

        fn regroup(
            &self,
            id: PageId,
            _: &mut ValueWriter,
            _: &mut dyn FnMut(&mut ValueWriter) -> Result<(), FlushError>,
        ) -> Result<u64, RegroupError> {
            self.0.commit(&[Op::Free { page: id }]);
            Ok(0)
        }
    }

    let db = Db::with(FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    });
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((1, 1), 1)]);

    let pass = db.wb_with(Some(&Frees(&db))).pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (0, 0));
    assert_eq!(db.latest(), version(3));
    let stats = db.stats();
    assert_eq!((stats.links_refused, stats.base_blocks), (1, 1));
    let chain = db.chain(page(1));
    assert_eq!(
        chain.last().map(|link| (link.kind, link.version)),
        Some((LinkKind::Free, version(3)))
    );
    assert_eq!(chain.len(), 2);
    db.check_cold();
}

/// A turn of the loop that fails is counted and reported, and the loop
/// goes on: the next turn does what the failed one could not.
#[test]
fn the_loop_reports_a_turn_that_fails_and_goes_on() {
    let config = FlushConfig {
        idle: std::time::Duration::from_millis(2),
        ..FlushConfig::new(Db::SIZE)
    };
    let (fs, db) = Db::on_sim(config, LogOptions::default());
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    let first = db.chain(page(1))[0].location().expect("a block").file;
    db.commit(&[hash((1, 1), 1)]);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Append)
            .at(FileNames::SEGMENT.path(db_dir(), first)),
    );

    let wb = db.wb();
    let errors = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| errors.lock().expect("the errors").push(error.to_string())));
        for _ in 0..5000 {
            if db.pool.oldest_unwritten().is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        wb.stop();
    });
    let errors = errors.into_inner().expect("the errors");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(db.stats().errors, 1);
    assert_eq!(db.state.unwritten(), 0);
    db.check_cold();
}

/// A committer that waits for room while the loop ends makes the pass
/// itself.
#[test]
fn a_committer_whose_loop_has_ended_makes_the_pass_itself() {
    let config = FlushConfig {
        max_unwritten: 0,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    let wb = db.wb();
    assert!(!wb.has_room());
    // A loop is said to run, and makes no pass.
    db.state.running.store(true, Ordering::Release);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            db.state.running.store(false, Ordering::Release);
        });
        assert_eq!(wb.wait_for_room().expect("room"), Room::Free);
    });
    assert_eq!(db.state.unwritten(), 0);
    db.check_cold();
}

/// A loop that is stopped before it begins ends at once, and the next one
/// runs.
#[test]
fn a_loop_that_is_stopped_before_it_begins_ends_at_once() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    let wb = db.wb();
    wb.stop();
    wb.run(|error| panic!("{error}"));
    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| panic!("{error}")));
        while db.pool.oldest_unwritten().is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        wb.stop();
    });
    db.check_cold();
}

/// The log cannot be synced any more, so every turn of the loop fails
/// when it rolls the log. The turn makes its pass all the same: pages are
/// written, and a committer that waits for room is not left waiting.
#[test]
fn a_turn_that_cannot_roll_the_log_still_makes_its_pass() {
    let config = FlushConfig {
        max_unwritten: 0,
        idle: std::time::Duration::from_millis(2),
        ..FlushConfig::new(Db::SIZE)
    };
    let log = LogOptions {
        target_file_len: 300,
        ..LogOptions::default()
    };
    let (fs, db) = Db::on_sim(config, log);
    db.commit(&[insert((1, 1), b"a", inline(&noise(1, 400)))]);
    assert!(db.log.is_due());
    let file = db.log.end().expect("the log has a file").file();
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(FileNames::WAL.path(db_dir(), file))
            .forever(),
    );

    let wb = db.wb();
    let errors = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| errors.lock().expect("the errors").push(error.to_string())));
        let _stop = StopOnDrop(wb);
        while !db.state.running.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        for round in 0..5u8 {
            assert_eq!(wb.wait_for_room().expect("room"), Room::Free);
            db.commit(&[hash((1, 1), round)]);
        }
        assert_eq!(wb.wait_for_room().expect("room"), Room::Free);
    });
    assert_eq!(db.state.unwritten(), 0);
    assert!(!errors.lock().expect("the errors").is_empty());
    assert!(db.stats().errors > 0);
    db.check_cold();
    // No checkpoint is installed while the log cannot be made durable.
    let error = db.wb().checkpoint().expect_err("the log is not durable");
    assert!(
        matches!(error, FlushError::Log(LogError::SyncFailed { .. })),
        "{error}"
    );
}

/// With the loop running, a page that cannot be written fails every turn.
/// The turns write the other pages all the same, and a committer that
/// waits for room is told that waiting does not help.
#[test]
fn a_page_that_cannot_be_written_holds_up_no_other_page_and_no_committer() {
    let config = FlushConfig {
        max_unwritten: 0,
        idle: std::time::Duration::from_millis(2),
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", inline(b"v")),
    ]);
    shorten_the_value(&db, page(1));

    let wb = db.wb();
    let errors = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| errors.lock().expect("the errors").push(error.to_string())));
        let _stop = StopOnDrop(wb);
        while !db.state.running.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert_eq!(wb.wait_for_room().expect("room"), Room::Stalled);
        db.commit(&[hash((2, 1), 1)]);
        assert_eq!(wb.wait_for_room().expect("room"), Room::Stalled);
    });
    assert_eq!(db.state.unwritten(), 1);
    assert_eq!(db.shape(page(2)), "Bd");
    let errors = errors.into_inner().expect("the errors");
    assert!(!errors.is_empty());
    assert!(
        errors
            .iter()
            .all(|error| error.starts_with("a pending value of page 1")),
        "{errors:?}"
    );
}
