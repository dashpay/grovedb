//! The counts of `stats` on a store that commits, writes back and takes
//! checkpoints: each count moves by what was done, and by nothing else.

use std::time::Duration;

use lignin_format::PageId;
use lignin_io::sim::{Fault, FaultRule, OpSet};

use super::*;
use crate::{
    segment::{tests::sim::db_dir, FileNames},
    stats::{Holder, Stats, StoreStats},
};

fn read(db: &Db, stats: &Stats) -> StoreStats {
    stats.read(&db.wb())
}

/// A hash-only commit: one group, one node record, whose bytes are the run
/// write-back then writes as a block, less its page id.
#[test]
fn a_commit_is_counted_by_its_records_and_its_bytes() {
    let db = Db::new();
    let stats = Stats::new();
    assert_eq!(read(&db, &stats), StoreStats::default());

    db.commit(&[
        insert((1, 1), b"a", inline(b"value")),
        insert((1, 2), b"b", long(1, 900)),
    ]);
    db.commit_flat(page(9), &[(b"k", inline(b"flat"))], &BTreeMap::new());
    let first = read(&db, &stats);
    assert_eq!(first.log_groups, 2);
    assert_eq!(first.log.groups, 2);
    assert_eq!((first.log.changes, first.log.marks), (0, 0));
    assert_eq!(first.log_bytes, first.log.group_bytes);
    assert_eq!(first.flush.groups, 2);
    assert!(first.node_records >= 2, "{first:?}");
    assert!(
        first.node_record_bytes > 900,
        "the value travels in the group"
    );
    assert_eq!(first.other_records, 1);
    assert!(first.other_record_bytes > 0);
    assert_eq!(first.flush_bytes, 0, "nothing written back yet");

    db.wb().pass().expect("a pass");
    let passed = read(&db, &stats);
    assert_eq!(passed.flush.values_appended, 1);
    assert!(passed.flush.value_stored_bytes > passed.flush.value_bytes);
    assert_eq!(passed.log.changes, 1, "the value's page step");
    assert_eq!(
        passed.log_bytes,
        passed.log.group_bytes + passed.log.change_bytes
    );
    assert_eq!(
        passed.flush_bytes,
        passed.flush.stored_bytes + passed.flush.value_stored_bytes
    );
    assert_eq!(passed.log_groups, 2, "a page step is no commit");

    db.commit(&[hash((1, 1), 7)]);
    let before = read(&db, &stats);
    db.wb().pass().expect("a pass");
    let after = read(&db, &stats);
    assert_eq!(after.node_records - before.node_records, 0);
    assert_eq!(before.node_records - passed.node_records, 1);
    assert_eq!(
        before.node_record_bytes - passed.node_record_bytes,
        after.flush.delta_bytes - PageId::ENCODED_LEN as u64,
        "the record is the run that was written, less its page id"
    );

    let Checkpointed::Installed { .. } = db.wb().checkpoint().expect("a checkpoint") else {
        panic!("nothing waits");
    };
    let checkpointed = read(&db, &stats);
    assert_eq!(checkpointed.log.marks, 1);
    assert!(checkpointed.flush.checkpoint_bytes > 0);
    assert_eq!(
        checkpointed.flush_bytes,
        checkpointed.flush.stored_bytes
            + checkpointed.flush.value_stored_bytes
            + checkpointed.flush.checkpoint_bytes
    );
    assert_eq!(checkpointed.log_bytes, checkpointed.log.bytes());
    assert_eq!(checkpointed.watch.log_held, 0);
    assert!(checkpointed.watch.log_held_by().is_empty());
}

/// Reads of the pool are counted as hits and as reads from storage, and
/// the counts the layers above keep are read as they were counted.
#[test]
fn reads_hashes_lookups_and_cleaning_are_counted() {
    let db = Db::new();
    let stats = Stats::new();
    let at = db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    db.wb().pass().expect("a pass");
    let before = read(&db, &stats);
    for _ in 0..3 {
        drop(db.pool.get(page(1), at).expect("in the pool"));
    }
    assert!(db.pool.get(page(2), at).is_err());
    stats.count_hashes(5);
    stats.count_keyed_lookups(2);
    stats.count_cleaner_bytes(4096);
    let after = read(&db, &stats);
    assert_eq!(after.cache_hits - before.cache_hits, 3);
    assert_eq!(after.page_reads - before.page_reads, 1);
    assert_eq!(
        (after.hashes, after.keyed_lookups, after.cleaner_bytes),
        (5, 2, 4096)
    );
}

/// A write transaction holds a private copy of a page whose value waits:
/// the log is held, and the reading says by what.
#[test]
fn the_log_held_says_what_holds_it() {
    let db = Db::new();
    let stats = Stats::new();
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    let open = stats.write_opened();
    let mut txn = db.begin();
    assert!(!txn.copy(page(1)), "the page exists");
    let pass = db.wb().pass().expect("a pass");
    assert_eq!(pass.blocked, 1);

    let reading = read(&db, &stats);
    let watch = &reading.watch;
    assert!(watch.log_held > 0);
    assert_eq!((watch.blocked_pages, watch.unwritten), (1, 1));
    assert!(watch.pending_value_bytes >= 900);
    assert_eq!(watch.open_writes, 1);
    let holders = watch.log_held_by();
    assert!(holders.contains(&Holder::BlockedPages { pages: 1 }));
    assert!(holders.contains(&Holder::Unwritten { pages: 1 }));
    assert!(holders
        .iter()
        .any(|holder| matches!(holder, Holder::OpenWrites { count: 1, .. })));
    let text = reading.to_string();
    assert!(
        text.contains("1 pages held back by write transactions"),
        "{text}"
    );
    assert!(text.contains("1 open write transactions"), "{text}");

    assert!(matches!(
        db.wb().checkpoint().expect("an attempt"),
        Checkpointed::PutOff { .. }
    ));
    let put_off = read(&db, &stats);
    assert_eq!(
        (
            put_off.watch.checkpoints_put_off,
            put_off.watch.checkpoints_failed
        ),
        (1, 0)
    );
    let text = put_off.to_string();
    assert!(text.contains("checkpoints put off: 1, failed: 0"), "{text}");

    txn.roll_back();
    drop(open);
    db.wb().place_ready().expect("placed");
    db.wb().pass().expect("a pass");
    let reading = read(&db, &stats);
    assert_eq!(
        (reading.watch.blocked_pages, reading.watch.open_writes),
        (0, 0)
    );
    assert!(reading.watch.log_held > 0);
    assert_eq!(reading.watch.log_held_by(), vec![Holder::NextCheckpoint]);
}

/// A page whose bytes cannot be written is counted for as long as it
/// cannot, and stops being counted when it can.
#[test]
fn a_page_that_cannot_be_written_is_counted() {
    let db = Db::new();
    let stats = Stats::new();
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", inline(b"v")),
    ]);
    super::errors::shorten_the_value(&db, page(1));
    for _ in 0..2 {
        db.wb().pass().expect_err("the bytes are not the value");
        let reading = read(&db, &stats);
        assert_eq!(reading.watch.failing_pages, 1);
        assert!(reading
            .watch
            .log_held_by()
            .contains(&Holder::FailingPages { pages: 1 }));
    }
    for attempt in 1..=2 {
        db.wb()
            .checkpoint()
            .expect_err("the page cannot be written");
        let reading = read(&db, &stats);
        assert_eq!(reading.watch.checkpoints_failed, attempt);
        assert_eq!(reading.watch.checkpoints_put_off, 0);
        assert_eq!(reading.flush.checkpoints, 0);
    }
}

/// A committer that finds write-back over its bounds waits, and the wait
/// is counted, with whether it ended stalled.
#[test]
fn waits_for_room_are_counted_with_their_length() {
    let config = FlushConfig {
        max_unwritten: 0,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    let stats = Stats::new();
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Free);
    assert_eq!(read(&db, &stats).watch.room_waits, 0, "within its bounds");

    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Free);
    let watch = read(&db, &stats).watch;
    assert_eq!((watch.room_waits, watch.room_stalls), (1, 0));

    db.commit(&[insert((2, 1), b"b", long(1, 900))]);
    let mut txn = db.begin();
    assert!(!txn.copy(page(2)), "the page exists");
    assert_eq!(db.wb().wait_for_room().expect("room"), Room::Stalled);
    let watch = read(&db, &stats).watch;
    assert_eq!((watch.room_waits, watch.room_stalls), (2, 1));
    assert!(watch.room_waited > Duration::ZERO);
    txn.roll_back();
}

/// A segment whose sync fails is counted, and the reading says that no
/// checkpoint is installed until the store is opened again.
#[test]
fn a_segment_sync_that_fails_is_counted() {
    let (fs, db) = Db::on_sim(FlushConfig::new(Db::SIZE), LogOptions::default());
    let stats = Stats::new();
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    db.wb().checkpoint().expect("a checkpoint");
    let first = db.chain(page(1))[0].location().expect("a block").file;
    db.commit(&[hash((1, 1), 1)]);
    assert!(!read(&db, &stats).watch.segments_not_durable);

    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(FileNames::SEGMENT.path(db_dir(), first)),
    );
    db.wb().checkpoint().expect_err("the sync fails");
    let reading = read(&db, &stats);
    assert_eq!(reading.watch.segment_syncs_failed, 1);
    assert_eq!(reading.watch.checkpoints_failed, 1);
    assert!(reading.watch.segments_not_durable);
    assert_eq!(reading.watch.log_held_by()[0], Holder::SegmentsNotDurable);
    assert!(reading.to_string().contains("segment syncs failed: 1"));
}

/// A page step of another layer, here the value log's cleaner, is
/// counted as a change of the log, with the bytes of its record.
#[test]
fn a_page_step_of_another_layer_is_counted_as_a_change() {
    let db = Db::new();
    let stats = Stats::new();
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    db.wb().pass().expect("a pass");
    let before = read(&db, &stats).log;
    db.move_value(page(1), slot(1), false);
    let after = read(&db, &stats).log;
    assert_eq!(after.changes - before.changes, 1);
    assert!(after.change_bytes > before.change_bytes);
    assert_eq!(after.groups, before.groups);
}
