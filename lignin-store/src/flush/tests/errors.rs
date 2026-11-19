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
    table::{LinkError, LogPlace, PageTableError},
    values::ValueStoreError,
    wal::LogError,
};

fn file_id(raw: u32) -> FileId {
    FileId::new(raw).expect("a file id")
}

fn installed(checkpointed: Checkpointed) -> CommitVersion {
    match checkpointed {
        Checkpointed::Installed { covers, .. } => covers,
        other => panic!("no checkpoint was installed: {other:?}"),
    }
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

/// The log fails the record of the page step that places a value, with
/// the append taken back or left torn in the file. Nothing is installed,
/// the value keeps its hold, and the log takes nothing until write-back
/// has rolled it (`transactions.md` CM9 item 5). Then the step is made.
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

        let pass = db.wb().pass().expect("a pass");
        assert_eq!(fs.stats().faults, 1);
        // An append that is left torn may have left the whole record: the
        // log then has it, and the step was made. Otherwise:
        if pass.placed == 0 {
            put_off += 1;
            assert_eq!((pass.written, pass.waiting), (0, 1), "{torn}");
            let stats = db.stats();
            assert_eq!((stats.values_appended, stats.values_placed), (1, 0));
            assert_eq!(db.pending(page(1)).len(), 1);
            assert_eq!(db.wb().place_ready().expect("put off again"), 0);
        } else {
            assert!(torn);
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
        let live: u64 = db.values.counts().iter().map(|count| count.values).sum();
        assert_eq!(live, 1);
        db.check_cold();
        db.reopen().check_cold();
    }
    assert!(put_off > 0);
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
        installed(db.wb().checkpoint().expect("a checkpoint")),
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
        installed(db.wb().checkpoint().expect("a checkpoint")),
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
    let live: u64 = db.values.counts().iter().map(|count| count.values).sum();
    assert_eq!(live, 2);
    db.check_cold();
    db.reopen().check_cold();
}

/// A pending value whose bytes are with its frame in another length than
/// its cell states is not written anywhere: the pass fails, and the page
/// stays as it is.
#[test]
fn a_pending_value_without_bytes_of_its_length_is_refused() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    let (_, pending) = db.pending(page(1))[0];
    {
        let step = db.pool.page_step(page(1));
        let old = step.current().expect("the page");
        let mut blank = db.pool.blank();
        blank.copy_from(&old);
        blank.attach(pending.id(), Arc::from(&b"too short"[..]));
        step.replace(&old, blank).expect("replaced");
    }
    let error = db.wb().pass().expect_err("the bytes are not the value");
    assert!(
        matches!(error, FlushError::PendingValue { page: id, born } if id == page(1) && born == version(1)),
        "{error}"
    );
    let stats = db.stats();
    assert_eq!((stats.values_appended, stats.base_blocks), (0, 0));
    assert_eq!(db.state.unwritten(), 1);
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
