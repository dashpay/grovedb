//! Table checkpoints: what one covers, the order of its steps, and the
//! log files that go afterwards (`page-table.md` K1 to K15).

use lignin_format::wal::LogPlace;

use super::*;
use crate::table::CheckpointInfo;

fn small_log() -> LogOptions {
    LogOptions {
        target_file_len: 400,
        ..LogOptions::default()
    }
}

fn loaded(db: &Db) -> (PageTable, CheckpointInfo) {
    let manifest = db.manifest.lock().expect("the manifest");
    let named = manifest.manifest().expect("a manifest");
    let (table, info) = PageTable::load(&*db.vfs, &db.path, named).expect("the checkpoint loads");
    (table, info.expect("the manifest names a checkpoint"))
}

fn installed(checkpointed: Checkpointed) -> (CommitVersion, LogPlace, usize) {
    match checkpointed {
        Checkpointed::Installed {
            covers,
            place,
            log_files_dropped,
        } => (covers, place, log_files_dropped),
        other => panic!("no checkpoint was installed: {other:?}"),
    }
}

/// A checkpoint covers every commit that is written back, holds the table
/// and a sample of the value counts, and is named only with the log
/// durable behind it. The log files before it then go.
#[test]
fn a_table_checkpoint_covers_what_is_written_back_and_lets_the_log_go() {
    let db = Db::with_log(FlushConfig::new(Db::SIZE), small_log());
    assert_eq!(db.state.in_force(), Some((version(0), None)));
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", inline(b"v")),
    ]);
    for round in 0..6u8 {
        db.commit(&[hash((2, 1), round)]);
    }
    let files_before = db.log.files_named().expect("the files").len();
    assert!(files_before >= 1);
    assert!(!db.wb().checkpoint_due());

    let (covers, place, dropped_1) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    assert_eq!(db.state.in_force(), Some((covers, Some(place))));
    assert_eq!(db.stats().checkpoints, 1);
    // The checkpoint wrote everything back first.
    assert_eq!(db.state.unwritten(), 0);
    assert!(db.pending(page(1)).is_empty());

    // What the manifest names loads, and is the table and the counts.
    let (table, info) = loaded(&db);
    assert_eq!((info.covers(), info.sample().place), (covers, Some(place)));
    for id in [1, 2].map(page) {
        let mut chain = Vec::new();
        table.chain(id, &mut chain);
        assert_eq!(chain, db.chain(id));
    }
    let sampled: Vec<(u64, u64)> = info
        .sample()
        .counts
        .iter()
        .map(|count| (count.values, count.bytes))
        .collect();
    let counted: Vec<(u64, u64)> = db
        .values
        .counts()
        .iter()
        .map(|count| (count.values, count.bytes))
        .collect();
    assert_eq!(sampled, counted);
    assert_eq!(counted.iter().map(|count| count.0).sum::<u64>(), 1);
    // The log was synced after the copy, through the mark.
    let durable = db.log.durable();
    assert!(durable.place >= Some(place) && durable.version == covers);

    // A second checkpoint: its sample is taken after the first one's, it
    // covers no less, and the log files before it are taken out.
    for round in 0..12u8 {
        db.commit(&[hash((2, 1), 100 + round)]);
    }
    let oldest = |db: &Db| db.log.files_named().expect("the files")[0].id();
    let oldest_then = oldest(&db);
    let (covers_2, place_2, dropped) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert!(covers_2 > covers && place_2 > place);
    assert!(dropped > 0);
    assert!(oldest(&db) > oldest_then);
    assert_eq!(db.stats().log_files_dropped, (dropped_1 + dropped) as u64);
    db.check_cold();
}

/// A store with nothing in it can take a checkpoint too.
#[test]
fn an_empty_store_takes_a_checkpoint() {
    let db = Db::new();
    let (covers, _, dropped) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert_eq!((covers, dropped), (version(0), 0));
    let (table, info) = loaded(&db);
    assert_eq!((table.pages(), info.covers()), (0, version(0)));
}

/// The places of successive samples never go back (K15). A sample that
/// would is refused, and nothing is written for it.
#[test]
fn a_sample_from_before_the_last_one_is_refused() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    let (_, place, _) = installed(db.wb().checkpoint().expect("a checkpoint"));
    let ahead = LogPlace::new(place.file(), place.offset() + 10_000).expect("a place");
    db.state
        .checkpoints
        .lock()
        .expect("the checkpoints")
        .last_sample = Some(ahead);

    db.commit(&[hash((1, 1), 1)]);
    let error = db.wb().checkpoint().expect_err("the sample went back");
    assert!(
        matches!(error, FlushError::SampleWentBack { before, taken } if before == ahead && taken < ahead),
        "{error}"
    );
    assert_eq!(db.stats().checkpoints, 1);
    assert_eq!(loaded(&db).1.sample().place, Some(place));
}

/// A page that was owed when the sample was taken has to be in the
/// checkpoint (K1). While a write transaction keeps its value from being
/// placed, no checkpoint is taken, and the one in force stays.
#[test]
fn a_checkpoint_is_put_off_while_a_page_it_needs_cannot_be_written() {
    let db = Db::new();
    db.commit(&[insert((1, 1), b"a", inline(b"v"))]);
    let (first, ..) = installed(db.wb().checkpoint().expect("a checkpoint"));

    db.commit(&[insert((2, 1), b"b", long(1, 900))]);
    db.commit(&[hash((1, 1), 1)]);
    let mut txn = db.begin();
    txn.copy(page(2));
    let put_off = db.wb().checkpoint().expect("no checkpoint");
    assert_eq!(put_off, Checkpointed::PutOff { blocked: 1 });
    assert_eq!(db.state.in_force().map(|named| named.0), Some(first));
    assert_eq!(loaded(&db).1.covers(), first);
    let stats = db.stats();
    assert_eq!((stats.checkpoints, stats.checkpoints_put_off), (1, 1));
    // The page that could be written was, and a reader finds both.
    assert_eq!(db.shape(page(1)), "Bd");
    db.check_warm();

    txn.roll_back();
    let (covers, ..) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert_eq!(covers, db.latest());
    db.check_cold();
}

/// A session ends with everything written back, a checkpoint that covers
/// it, and a log that is durable with a manifest that says so. The next
/// session opens on that, adopts the checkpoint, and goes on from it.
#[test]
fn the_next_session_goes_on_from_the_checkpoint_of_the_last() {
    let db = Db::with_log(FlushConfig::new(Db::SIZE), small_log());
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", inline(b"v")),
    ]);
    db.commit(&[hash((2, 1), 1)]);
    db.wb().pass().expect("a pass");
    db.commit(&[hash((2, 1), 2), insert((1, 2), b"c", long(2, 800))]);
    let latest = db.latest();

    let db = db.reopen();
    let (covers, place) = db.state.in_force().expect("a checkpoint was adopted");
    assert_eq!(covers, latest);
    assert!(place.is_some());
    assert_eq!(db.pool.census().published, 0);
    db.check_cold();
    assert_eq!(
        db.values
            .counts()
            .iter()
            .map(|count| count.values)
            .sum::<u64>(),
        2
    );

    // The session goes on: commits, write-back and a checkpoint that
    // covers more, with a sample from a later place.
    db.commit(&[hash((2, 1), 3), insert((3, 1), b"d", long(3, 700))]);
    db.commit(&[Op::Value {
        page: page(1),
        slot: slot(1),
        value: long(4, 600),
    }]);
    let (covers_2, place_2, _) = installed(db.wb().checkpoint().expect("a checkpoint"));
    assert_eq!(covers_2, db.latest());
    assert!(Some(place_2) > place);
    db.check_cold();
    let db = db.reopen();
    db.check_cold();
    assert_eq!(
        db.values
            .counts()
            .iter()
            .map(|count| count.values)
            .sum::<u64>(),
        3
    );
}

struct StopOnDrop<'s>(WriteBack<'s>);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// The background loop: commits are written back, values placed and
/// checkpoints taken with nobody calling for them, and a committer that
/// asks for room waits for the loop.
#[test]
fn the_background_loop_writes_back_and_takes_checkpoints() {
    let config = FlushConfig {
        max_unwritten: 4,
        checkpoint_log_bytes: 2000,
        idle: std::time::Duration::from_millis(5),
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with_log(config, small_log());
    let wb = db.wb();
    let errors = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|error| errors.lock().expect("the errors").push(error.to_string())));
        // The loop ends whatever becomes of this thread.
        let _stop = StopOnDrop(wb);
        for round in 0..60u32 {
            assert_eq!(wb.wait_for_room().expect("room"), Room::Free);
            let at = (1 + round % 7, 1 + (round / 7) as u16);
            db.commit(&[
                insert(
                    at,
                    &round.to_le_bytes(),
                    long(round as u8, 300 + round as usize),
                ),
                insert((8, 1 + round as u16), &round.to_le_bytes(), inline(b"v")),
            ]);
            wb.place_ready().expect("placed");
            wb.wake();
        }
        // The loop catches up by itself.
        let caught_up = |db: &Db| db.pool.oldest_unwritten().is_none();
        for _ in 0..2000 {
            if caught_up(&db) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });
    assert_eq!(*errors.lock().expect("the errors"), Vec::<String>::new());
    let stats = db.stats();
    assert_eq!(db.state.unwritten(), 0);
    assert_eq!(stats.values_placed, 60);
    assert!(stats.checkpoints >= 2, "{stats:?}");
    assert!(stats.log_files_dropped > 0, "{stats:?}");
    assert_eq!((stats.errors, stats.runs_refused), (0, 0));
    db.check_chains();
    db.check_cold();
    // And the store ends and opens again on what the loop left.
    db.reopen().check_cold();
}
