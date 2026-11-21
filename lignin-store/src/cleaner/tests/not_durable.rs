//! A sync of the cleaner's segment that the file layer refused: write-back
//! is told, and the error keeps what the file layer said.

use lignin_io::sim::{Fault, FaultRule, OpSet, SimError, SimVfs};

use super::*;
use crate::{
    flush::FlushError,
    segment::{tests::sim::db_dir, FileNames, SegmentStoreError},
    wal::LogOptions,
};

/// A churned store on a simulated device in which the cleaner has moved
/// one block into a segment of its own and has more to move: a pass ends
/// after the first block it moved. The cleaner's segment is open, and no
/// sync of a segment has failed. Returns the device, the store, the
/// cleaner's state and its segment.
fn moving() -> (SimVfs, Db, CleanerState, FileId) {
    let (fs, db) = Db::on_sim(small_segments(), LogOptions::default());
    let db = churned_in(db, 13);
    let state = CleanerState::new(CleanerConfig {
        max_pass_bytes: 1,
        ..moving_only()
    });
    let before = segment_ids(&db);
    let pass = Cleaner::new(db.wb(), &state).pass();
    let pass = pass.expect("a pass on a device with room");
    assert!(pass.blocks_moved > 0 && pass.more, "{pass:?}");
    let after = segment_ids(&db);
    let mut made = after.iter().filter(|file| !before.contains(file));
    let (Some(file), None) = (made.next(), made.next()) else {
        panic!("the pass made one segment: {before:?} {after:?}");
    };
    let flush = db.wb().state();
    assert!(!flush.segments_not_durable());
    assert_eq!(flush.stats().segment_syncs_failed, 0);
    (fs, db, state, *file)
}

/// From here on the device refuses the operations `ops` on the segment
/// `file`, for room.
fn refuse(fs: &SimVfs, file: FileId, ops: OpSet) {
    let path = FileNames::SEGMENT.path(db_dir(), file);
    fs.add_rule(FaultRule::new(Fault::Enospc).on(ops).at(path).forever());
}

/// What the device said, if `source` is an operation that it refused for
/// room.
fn refused_for_room(source: &SegmentStoreError) -> bool {
    match source {
        SegmentStoreError::Io { source, .. } => SimError::injected(source) == Some(Fault::Enospc),
        _ => false,
    }
}

/// A pass is refused an append to the cleaner's segment and then the sync
/// of what the segment holds. Links point to copies that cannot be made
/// durable any more, so no table checkpoint is installed until the store
/// is opened again: write-back's state says so at once, as after a failed
/// sync of write-back's own segment, the sync is counted with those, and
/// a table checkpoint is answered with that and not with the segment's
/// error.
#[test]
fn a_refused_sync_of_the_cleaners_segment_is_told_to_write_back() {
    let (fs, db, state, file) = moving();
    refuse(&fs, file, OpSet::WRITES | OpSet::SYNCS);
    let refused = Cleaner::new(db.wb(), &state).pass();
    let refused = refused.expect_err("the append and the sync are refused");
    let CleanerError::SegmentNotDurable {
        file: failed,
        source,
    } = &refused
    else {
        panic!("{refused}");
    };
    assert_eq!(*failed, file);
    assert!(refused_for_room(source), "{refused}");
    fs.clear_rules();

    let flush = db.wb().state();
    assert!(flush.segments_not_durable());
    assert_eq!(flush.stats().segment_syncs_failed, 1);
    let again = db.wb().checkpoint();
    let again = again.expect_err("no table checkpoint is installed");
    assert!(matches!(again, FlushError::SegmentsNotDurable), "{again}");
}

/// The sealing of the cleaner's segment is refused its sync: the device
/// takes the bytes of the sealing and refuses every sync of the file. The
/// segment answers a second sync only with the fact that one failed, so
/// the error keeps what the device said of the first, and whoever reads
/// its cause finds that the device had no room. Write-back is told as
/// after any sync of the cleaner's segment that failed.
#[test]
fn a_sealing_that_is_refused_its_sync_keeps_what_the_device_said() {
    let (fs, db, state, file) = moving();
    refuse(&fs, file, OpSet::SYNCS);
    let refused = Cleaner::new(db.wb(), &state).finish();
    let refused = refused.expect_err("the sealing is refused its sync");
    let CleanerError::SegmentNotDurable {
        file: failed,
        source,
    } = &refused
    else {
        panic!("{refused}");
    };
    assert_eq!(*failed, file);
    assert!(refused_for_room(source), "{refused}");
    let flush = db.wb().state();
    assert!(flush.segments_not_durable());
    assert_eq!(flush.stats().segment_syncs_failed, 1);
}

/// A table checkpoint syncs every segment that is being written, the
/// cleaner's among them, and the cleaner is not told when that sync
/// fails. Its statistics count the segment all the same, right after the
/// checkpoint, by the segment store's record of it, and count it once:
/// also after a pass, which moves nothing in a store whose segments are
/// not durable, and after the cleaner has met the failure at its sealing.
#[test]
fn a_checkpoint_whose_sync_of_the_cleaners_segment_fails_is_counted_at_once_and_once() {
    let (fs, db, state, file) = moving();
    assert_eq!(state.stats().segments_not_durable, 0);
    refuse(&fs, file, OpSet::SYNCS);
    let refused = db.wb().checkpoint();
    let refused = refused.expect_err("the checkpoint syncs the cleaner's segment");
    assert!(db.wb().state().segments_not_durable(), "{refused}");
    assert_eq!(state.stats().segments_not_durable, 1, "{refused}");

    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass().expect("a pass that moves nothing");
    assert_eq!(pass.blocks_moved, 0, "{pass:?}");
    assert_eq!(state.stats().segments_not_durable, 1);
    let met = cleaner.finish();
    let met = met.expect_err("the sealing of a segment that failed");
    assert!(
        matches!(met, CleanerError::SegmentNotDurable { file: failed, .. } if failed == file),
        "{met}"
    );
    assert_eq!(state.stats().segments_not_durable, 1);
    let _ = cleaner.finish();
    assert_eq!(state.stats().segments_not_durable, 1);
}

/// The cleaner's own sealing is refused its sync: the segment is counted,
/// once. A segment that the cleaner seals with no failure is not, and its
/// record is given up with its writer.
#[test]
fn a_sealing_of_the_cleaners_segment_that_is_refused_its_sync_is_counted_once() {
    let (_fs, db, state, _) = moving();
    Cleaner::new(db.wb(), &state)
        .finish()
        .expect("a sealing on a device with room");
    assert_eq!(state.stats().segments_not_durable, 0);
    let kept = state.written.lock().expect("the records");
    assert!(kept.is_empty(), "a record of a sealed segment is kept");
    drop(kept);

    let (fs, db, state, file) = moving();
    refuse(&fs, file, OpSet::SYNCS);
    let cleaner = Cleaner::new(db.wb(), &state);
    cleaner
        .finish()
        .expect_err("the sealing is refused its sync");
    assert_eq!(state.stats().segments_not_durable, 1);
    let _ = cleaner.finish();
    assert_eq!(state.stats().segments_not_durable, 1);
}
