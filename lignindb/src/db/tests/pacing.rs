//! STORE-06: what the background threads of a database do on a device that
//! stays full (`docs/lignindb/disk-full-and-backup.md`, DF12; D-204).
//!
//! The state is made by construction, with the background work at rest:
//! write-back has appended the values of a commit and a manifest covers
//! them, the log has refused the record of the first page step and takes
//! nothing until it is rolled, and then the device refuses every write
//! and every new name while its syncs work. The log has no file to turn
//! to, so each turn of write-back asks for one and fails. Before D-204 the
//! turn's own page step, refused by the log, woke the loop for the next
//! turn, and each turn asked for the file under a new id.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use lignin_format::{
    manifest::{LastFileIds, Manifest},
    FileId, PageSize,
};
use lignin_io::{
    sim::{Fault, FaultRule, OpKind, SimConfig},
    SimVfs, Vfs,
};
use lignin_store::{
    flush::FlushConfig,
    segment::{FileNames, ManifestStore, SegmentStoreError},
    wal::SyncPolicy,
};

use super::{
    full::{tick, Mode, Session},
    room::rewrite,
    schedules::{commit_behind, dropped_on, Behind},
    *,
};
use crate::{
    backup::tests::world::{check_reads, commit, first, State},
    db::{
        store::{Shared, Signal},
        Reopen, WriteError,
    },
};

const DIR: &str = "/db";
/// The idle time of write-back's thread and of the reclaimer's.
const IDLE: Duration = Duration::from_millis(5);
/// The longest wait after failed turns.
const MOST: Duration = Duration::from_millis(80);
/// How long the turns of the running threads are counted: 80 idle times.
const WINDOW: Duration = Duration::from_millis(400);

/// The ids of the log's files.
fn log_files(fs: &SimVfs) -> Vec<u32> {
    let entries = fs.read_dir(&FileNames::WAL.dir(Path::new(DIR)));
    let mut ids: Vec<u32> = (entries.expect("a listing").iter())
        .filter_map(|entry| FileNames::WAL.parse(&entry.name))
        .map(FileId::get)
        .collect();
    ids.sort_unstable();
    ids
}

/// The most turns that a loop of `config` can end in `window` while every
/// turn fails and moves nothing on: each is followed by its wait, and the
/// waits are shortest where the first turn of the window is the first that
/// failed.
fn most_turns(config: &FlushConfig, window: Duration) -> u64 {
    let mut waited = Duration::ZERO;
    let mut turns = 1;
    for failed in 1.. {
        waited += config.wait_after_failure(failed);
        if waited > window {
            break;
        }
        turns += 1;
    }
    turns
}

/// A session at rest in the state of the module's documentation, after
/// one commit that the full device refused.
fn stuck() -> Session {
    let mut session = Session::new(77, SyncPolicy::Never, true);
    session.options.flush = Some(FlushConfig {
        idle: IDLE,
        max_failed_wait: MOST,
        ..FlushConfig::new(PageSize::MIN)
    });
    session.options.idle = IDLE;
    assert!(session.open());
    session.write("first", first, false);
    session.ticks("first");
    session.write("values", rewrite, false);
    let db = session.db.as_ref().expect("the database is open");
    let store = &db.shared.store;
    assert!(!store.log.has_file_ready(), "the log has a file to turn to");
    let file = store.log.end().expect("the log has a file").file();
    let before = store.flush.stats();
    // The record of the first page step is refused, and nothing else is.
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(Path::new(DIR), file));
    session.fs.add_rule(record.torn_append(false));
    let _ = tick(db);
    let after = store.flush.stats();
    assert!(after.values_appended > before.values_appended);
    assert_eq!(
        after.values_placed, before.values_placed,
        "a value is placed"
    );
    assert!(
        store.log.is_due(),
        "the log takes nothing until it is rolled"
    );
    session.fs.add_rule(Mode::FullButSyncs.rule(0));
    let latest = session.told.latest;
    session.write("refused", rewrite, false);
    assert_eq!(session.told.latest, latest, "a commit on a full device");
    session
}

/// Ends the threads that a test started on a database at rest, and who
/// asks them for turns, whatever becomes of the test.
struct Halt<'a>(&'a Shared, &'a AtomicBool);

impl Drop for Halt<'_> {
    fn drop(&mut self) {
        self.1.store(false, Ordering::Release);
        self.0.stop_background();
    }
}

/// The three background threads run on the full device, as a database
/// that is open has them, and somebody asks write-back and the reclaimer
/// for a turn all the time. Write-back's loop ends no more failed turns
/// in the window than its waits allow, the three threads together make a
/// few file operations for each of them, and write-back asks for one log
/// file throughout: no id is used up. The session has not ended, and
/// with room it takes the next commit: the log goes on in the file the
/// failed turns had asked for, with the file after it made ready.
#[test]
fn write_back_on_a_device_that_stays_full_waits_between_its_turns_and_uses_up_no_id() {
    let mut session = stuck();
    let files = log_files(&session.fs);
    let next = FileId::new(*files.last().expect("a log file") + 1).expect("an id");
    let asked_for = FileNames::WAL.path(Path::new(DIR), next);
    let asked_for = format!("cannot create {}", asked_for.display());
    {
        let db = session.db.as_ref().expect("the database is open");
        let shared = &*db.shared;
        let store = &*shared.store;
        let config = store.flush.config();
        let asking = AtomicBool::new(true);
        let (window, turns, ops) = thread::scope(|scope| {
            scope.spawn(|| crate::db::write_back(shared));
            scope.spawn(|| shared.reclaim_loop(IDLE));
            scope.spawn(|| crate::db::clean(shared));
            let _halt = Halt(shared, &asking);
            scope.spawn(|| {
                while asking.load(Ordering::Acquire) {
                    shared.write_back().wake();
                    store.signal.wake();
                    thread::yield_now();
                }
            });
            // The loop has made its first turn: a state it reaches alone.
            let failed = || store.flush.stats().errors;
            while failed() == 0 {
                thread::yield_now();
            }
            let began = Instant::now();
            let (turns, ops) = (failed(), session.fs.stats().ops);
            thread::sleep(WINDOW);
            let turns = failed() - turns;
            let ops = session.fs.stats().ops - ops;
            (began.elapsed(), turns, ops)
        });
        let most = most_turns(&config, window);
        let stats = db.stats();
        println!(
            "in {window:?}, with an idle time of {IDLE:?} and waits of {MOST:?} at most: {turns} \
             failed turns of write-back ({most} at most) and {ops} file operations of the three \
             threads; {} waits went on though a turn was asked for; {} passes of the cleaner \
             failed",
            store.flush.stats().turns_put_off,
            stats.background.cleaner_errors
        );
        assert!(
            turns <= most,
            "{turns} failed turns in {window:?}, where the waits allow {most}"
        );
        assert!(
            ops <= (turns + 1) * 8,
            "{ops} file operations in {turns} turns"
        );
        let last = db.stats().background.last_error.expect("a turn failed");
        assert!(last.contains(&asked_for), "{last}");
        assert_eq!(db.reopen(), None, "the session has ended");
    }
    assert_eq!(
        log_files(&session.fs),
        files,
        "a failed turn left a log file"
    );

    session.fs.clear_rules();
    let latest = session.told.latest;
    session.write("with room", rewrite, false);
    assert_eq!(session.told.latest, latest + 1, "a commit with room");
    let mut with_room = files;
    with_room.extend([next.get(), next.get() + 1]);
    assert_eq!(log_files(&session.fs), with_room);
    session.ticks("with room");
    session.check("with room");
}

/// The reclaimer's thread has the rule of write-back's loop: a round in
/// which a call failed is followed by a wait, whoever wakes the thread,
/// which is the idle time and twice as long after each further such round
/// in a row, up to the bound of the session's write-back. A round in
/// which nothing fails brings the idle time back.
#[test]
fn a_round_of_the_reclaimer_that_failed_is_followed_by_a_wait_that_grows_to_a_bound() {
    let fs = SimVfs::new(SimConfig::new(11));
    let (db, history) = dropped_on(fs.clone());
    // A page that the latest commit wrote is freed after the next one.
    commit_behind(&db, &history, Behind::Column);
    let shared = &*db.shared;
    let idle = Duration::from_millis(10);
    // The log refuses the record of every step.
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let mut failed_rounds = 0;
    let mut waits = Vec::new();
    for _ in 0..9 {
        let (round, wait) = shared.paced_round(idle, &mut failed_rounds);
        assert_eq!((round.finished, round.waiting), (0, 1), "{round:?}");
        waits.push(wait.map(|wait| wait.as_millis()));
    }
    let failed = db.stats().background;
    assert_eq!(
        waits,
        [10, 20, 40, 80, 160, 320, 640, 1000, 1000].map(Some),
        "{failed:?}"
    );
    assert!(failed.reclaim_errors >= 9, "{failed:?}");

    // With room write-back rolls the log, and the round fails no more.
    fs.clear_rules();
    tick(&db).expect("a turn with room");
    let (_, wait) = shared.paced_round(idle, &mut failed_rounds);
    assert_eq!((wait, failed_rounds), (None, 0));
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let (round, wait) = shared.paced_round(idle, &mut failed_rounds);
    if round.waiting > 0 {
        assert_eq!(wait, Some(idle), "{round:?}");
    }
}

/// The wait itself: whoever wakes the thread before it or during it, it
/// lasts as long as it was told to. Only the end of the session ends it.
#[test]
fn a_wake_does_not_end_the_reclaimers_wait_after_a_failed_round() {
    let signal = Signal::default();
    signal.attach(thread::current());
    signal.wake();
    let waking = AtomicBool::new(true);
    let wait = Duration::from_millis(60);
    let began = Instant::now();
    thread::scope(|scope| {
        scope.spawn(|| {
            while waking.load(Ordering::Acquire) {
                signal.wake();
                thread::yield_now();
            }
        });
        signal.sleep(wait);
        waking.store(false, Ordering::Release);
    });
    assert!(began.elapsed() >= wait, "{:?}", began.elapsed());
    // A thread that is told to end sleeps no more.
    signal.stop();
    signal.sleep(Duration::from_secs(3600));
}

/// The log's ids end at the largest one. The session whose log has asked
/// for a file after it says that it has ended and why, refuses its
/// commits, and goes on reading. The next open finds no id for the log
/// file it begins, and says so.
#[test]
fn a_log_whose_file_ids_are_used_up_ends_the_session_and_says_why() {
    let fs = SimVfs::new(SimConfig::new(78));
    let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
    let options = Options {
        vfs: vfs.clone(),
        log_file_len: Some(8 << 10),
        ..options()
    };
    let mut state = State::new();
    let write = |db: &Db, state: &mut State| {
        let batch = rewrite(state);
        let ended = commit(db, &batch, false);
        if ended.is_ok() {
            state.commit(&batch);
        }
        ended
    };
    let db = open_at_rest(DIR, &options).expect("a new database");
    write(&db, &mut state).expect("the first commit");
    tick(&db).expect("a turn");
    db.close().expect("a close");

    // The manifest says that every log file id but the last two is used.
    let mut manifest = ManifestStore::open(vfs.clone(), Path::new(DIR)).expect("the manifest");
    let replaced = manifest.update(|current| -> Result<Manifest, SegmentStoreError> {
        let last = LastFileIds {
            wal: FileId::new(FileId::MAX.get() - 2),
            ..current.last_file_ids()
        };
        let (table, files) = (current.table_checkpoint(), current.files().to_vec());
        Ok(Manifest::new(current.page_size(), last, table, files).expect("a manifest"))
    });
    replaced.expect("the manifest is replaced");
    drop(manifest);

    let db = open_at_rest(DIR, &options).expect("the database opens");
    let mut rounds = 0;
    while db.reopen().is_none() {
        let _ = write(&db, &mut state);
        let _ = tick(&db);
        rounds += 1;
        assert!(rounds < 200, "the log does not come to its last id");
    }
    assert_eq!(db.reopen(), Some(Reopen::LogFileIdsUsedUp));
    assert_eq!(db.stats().reopen, Some(Reopen::LogFileIdsUsedUp));
    let refused = write(&db, &mut state).expect_err("a commit of a session that has ended");
    assert!(
        matches!(refused, WriteError::Reopen(Reopen::LogFileIdsUsedUp)),
        "{refused}"
    );
    check_reads(&db, &state, "after the last id");
    assert_eq!(log_files(&fs).last(), Some(&FileId::MAX.get()));
    drop(db);

    let error = open_at_rest(DIR, &options)
        .err()
        .expect("an open with no id left");
    let mut said = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        said = format!("{said}: {cause}");
        source = cause.source();
    }
    assert!(said.contains("every log file id has been used"), "{said}");
}
