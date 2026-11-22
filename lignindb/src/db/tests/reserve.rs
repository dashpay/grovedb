//! OPS-08: the reserve of a session
//! (`docs/lignindb/disk-full-and-backup.md`, DF13): what an open does with
//! it, which refusals release it, and when the session fills it again.
//!
//! `db::tests::room` goes through the order of DF11 with a session at
//! rest and measures how large a reserve has to be. Here each rule is
//! looked at by itself, with the session at rest and the turns of its
//! threads made by the test, then with one loop of the three running
//! alone, and at the end with a database that is opened with its three
//! threads, on a device that fills, at every size of that ladder.

use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

use lignin_io::{
    sim::{Event, Fault, FaultRule, OpKind, OpSet, SimConfig},
    OpenMode, SimVfs, Vfs,
};
use lignin_store::{
    cleaner::CleanerStats,
    flush::{Checkpointed, FlushConfig},
    segment::FileNames,
};

use super::{
    full::{sound, tick, Mode, Session},
    room::{cleaner_turn, rewrite, small, HEADROOM, LADDER, RESERVE},
    *,
};
use crate::{
    backup::tests::world::{batch, check_reads, commit, first, height, key, value, State},
    checkpoint::CheckpointOptions,
    db::{
        reserve::CleanerTurns,
        store::{Shared, Signal, Store},
        DbStats, ReserveStats, WriteError,
    },
    space::{out_of_space, Reserve},
    txn::tests::model::{self, OwnedBatch, OwnedSubtree},
};

const DIR: &str = "/db";

/// The reserve that the sessions at rest keep.
const KEPT: u64 = 48 << 10;

/// The idle time of the three loops where they run.
const IDLE: Duration = Duration::from_millis(5);
/// The longest wait after failed turns.
const MOST: Duration = Duration::from_millis(80);
/// How long the turns of the running threads are counted.
const WINDOW: Duration = Duration::from_millis(400);

/// A simulated device whose operations are recorded, and the options of a
/// database on it that keeps a reserve of `reserve` bytes.
fn device(seed: u64, reserve: u64) -> (SimVfs, Options) {
    let fs = SimVfs::new(SimConfig {
        trace_ops: true,
        ..SimConfig::new(seed)
    });
    let options = Options {
        vfs: Arc::new(fs.clone()),
        reserve,
        ..options()
    };
    (fs, options)
}

/// Commits the batch that `build` makes of `state`, and takes a commit
/// that was made into the state.
fn write(
    db: &Db,
    state: &mut State,
    build: impl FnOnce(&State) -> OwnedBatch,
) -> Result<Committed, WriteError> {
    let batch = build(state);
    let ended = commit(db, &batch, false);
    if ended.is_ok() {
        state.commit(&batch);
    }
    ended
}

/// A commit of a few bytes.
fn note(state: &State) -> OwnedBatch {
    batch(Vec::new(), vec![height(state.version())], Vec::new())
}

/// A database at rest on a new device, which keeps a reserve of [`KEPT`]
/// bytes, after one commit that was written back.
fn kept(seed: u64) -> (SimVfs, Options, Db, State) {
    kept_with(seed, |_| {})
}

/// As [`kept`], opened with the options that `adjust` leaves.
fn kept_with(seed: u64, adjust: impl FnOnce(&mut Options)) -> (SimVfs, Options, Db, State) {
    let (fs, mut options) = device(seed, KEPT);
    adjust(&mut options);
    let db = open_at_rest(DIR, &options).expect("a new database");
    let mut state = State::new();
    write(&db, &mut state, first).expect("the first commit");
    tick(&db).expect("a turn");
    (fs, options, db, state)
}

/// The bytes of the reserve in the directory of the database.
fn held(fs: &SimVfs) -> u64 {
    let reserve = Reserve::of(Arc::new(fs.clone()), Path::new(DIR));
    reserve.held().expect("the reserve")
}

/// The operations that the device was asked for on the two names of a
/// reserve, after its operation number `after`.
fn on_reserve(fs: &SimVfs, after: u64) -> Vec<(OpKind, String)> {
    let names = [Reserve::FILE_NAME, Reserve::UNFINISHED_FILE_NAME]
        .map(|name| Path::new(DIR).join(name).display().to_string());
    let on = |event: &Event| match event {
        Event::Op(op) if op.op > after => {
            let named =
                names.contains(&op.path) || op.to.as_ref().is_some_and(|to| names.contains(to));
            named.then(|| (op.kind, op.path.clone()))
        }
        _ => None,
    };
    fs.trace().iter().filter_map(on).collect()
}

/// A table checkpoint, installed.
fn checkpoint(db: &Db) {
    tick(db).expect("a turn");
    let installed = db.shared.write_back().checkpoint();
    assert!(
        matches!(installed, Ok(Checkpointed::Installed { .. })),
        "{installed:?}"
    );
}

/// One turn of the cleaner's loop in which no pass fails. Says whether
/// the session holds its reserve after it.
fn turn(db: &Db, turns: &mut CleanerTurns) -> bool {
    assert!(!cleaner_turn(db, turns), "a pass of the cleaner failed");
    db.stats().reserve.held
}

/// The counts of the cleaner that a turn at rest leaves as they were:
/// all but those of its passes, of its reads of the page table and of the
/// links it trimmed.
fn work(store: &Store) -> CleanerStats {
    CleanerStats {
        passes: 0,
        scans: 0,
        links_trimmed: 0,
        bytes_trimmed: 0,
        value_passes: 0,
        ..store.cleaner.stats()
    }
}

/// What a turn at rest may move, count by count: the cleaner's passes,
/// its reads of the page table and the links it trimmed. A failed pass
/// and every count of something that was copied, closed, retired, dropped
/// or asked for is work.
#[test]
fn a_turn_at_rest_moves_no_count_but_passes_scans_and_trims() {
    use crate::db::reserve::work;

    let base = CleanerStats::default();
    let at_rest = CleanerStats {
        passes: 3,
        scans: 2,
        links_trimmed: 9,
        bytes_trimmed: 900,
        value_passes: 3,
        ..base
    };
    assert_eq!(work(&at_rest), work(&base));
    let moved: [(&str, CleanerStats); 21] = [
        (
            "blocks_moved",
            CleanerStats {
                blocks_moved: 1,
                ..base
            },
        ),
        (
            "bytes_moved",
            CleanerStats {
                bytes_moved: 1,
                ..base
            },
        ),
        (
            "blocks_lost",
            CleanerStats {
                blocks_lost: 1,
                ..base
            },
        ),
        (
            "bytes_lost",
            CleanerStats {
                bytes_lost: 1,
                ..base
            },
        ),
        (
            "segments_retired",
            CleanerStats {
                segments_retired: 1,
                ..base
            },
        ),
        (
            "segments_dropped",
            CleanerStats {
                segments_dropped: 1,
                ..base
            },
        ),
        (
            "bytes_dropped",
            CleanerStats {
                bytes_dropped: 1,
                ..base
            },
        ),
        (
            "segments_unreadable",
            CleanerStats {
                segments_unreadable: 1,
                ..base
            },
        ),
        (
            "segments_gone",
            CleanerStats {
                segments_gone: 1,
                ..base
            },
        ),
        (
            "checkpoints",
            CleanerStats {
                checkpoints: 1,
                ..base
            },
        ),
        (
            "checkpoints_missed",
            CleanerStats {
                checkpoints_missed: 1,
                ..base
            },
        ),
        (
            "segments_sealed",
            CleanerStats {
                segments_sealed: 1,
                ..base
            },
        ),
        (
            "value_files_closed",
            CleanerStats {
                value_files_closed: 1,
                ..base
            },
        ),
        (
            "value_files_cleaned",
            CleanerStats {
                value_files_cleaned: 1,
                ..base
            },
        ),
        (
            "values_moved",
            CleanerStats {
                values_moved: 1,
                ..base
            },
        ),
        (
            "values_left",
            CleanerStats {
                values_left: 1,
                ..base
            },
        ),
        (
            "value_bytes_copied",
            CleanerStats {
                value_bytes_copied: 1,
                ..base
            },
        ),
        (
            "value_files_dropped",
            CleanerStats {
                value_files_dropped: 1,
                ..base
            },
        ),
        (
            "value_files_given_up",
            CleanerStats {
                value_files_given_up: 1,
                ..base
            },
        ),
        ("errors", CleanerStats { errors: 1, ..base }),
        (
            "segments_not_durable",
            CleanerStats {
                segments_not_durable: 1,
                ..base
            },
        ),
    ];
    for (count, moved) in moved {
        assert_ne!(work(&moved), work(&base), "{count}");
    }
}

/// Waits for `reached`, which the threads of a test come to by
/// themselves.
fn until(what: &str, mut reached: impl FnMut() -> bool) {
    let began = Instant::now();
    while !reached() {
        assert!(began.elapsed() < Duration::from_secs(60), "{what}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Ends the loops that a test started on a database at rest, whatever
/// becomes of the test.
struct Halt<'a>(&'a Shared);

impl Drop for Halt<'_> {
    fn drop(&mut self) {
        self.0.stop_background();
    }
}

#[test]
fn an_open_fills_the_reserve_where_the_directory_has_none_of_its_length() {
    let (fs, options, db, _) = kept(1);
    assert_eq!(held(&fs), KEPT);
    let filled = ReserveStats {
        len: KEPT,
        held: true,
        fills: 1,
        ..ReserveStats::default()
    };
    assert_eq!(db.stats().reserve, filled);
    let said = db.stats().to_string();
    assert!(said.contains("reserve: 49152 bytes, held"), "{said}");
    db.close().expect("a close");
    assert_eq!(held(&fs), KEPT, "a close keeps the reserve");

    // The next open finds it, and writes none of it again.
    let ops = fs.stats().ops;
    let db = open_at_rest(DIR, &options).expect("the database opens");
    assert_eq!(db.stats().reserve, filled);
    let written = on_reserve(&fs, ops);
    assert!(
        written.iter().all(|(kind, _)| !matches!(
            kind,
            OpKind::Append | OpKind::Write | OpKind::Rename | OpKind::SyncAll
        )),
        "{written:?}"
    );
    db.close().expect("a close");

    // An open that keeps another length writes the reserve anew.
    let longer = Options {
        reserve: 2 * KEPT,
        ..options.clone()
    };
    let db = open_at_rest(DIR, &longer).expect("the database opens");
    assert_eq!(held(&fs), 2 * KEPT);
    assert_eq!(
        (db.stats().reserve.len, db.stats().reserve.fills),
        (2 * KEPT, 1)
    );
    db.close().expect("a close");

    // An open that keeps none leaves the file as it is, and asks the
    // device for nothing on its names.
    let none = Options {
        reserve: 0,
        ..options
    };
    let ops = fs.stats().ops;
    let db = open_at_rest(DIR, &none).expect("the database opens");
    assert_eq!(db.stats().reserve, ReserveStats::default());
    assert!(!db.stats().to_string().contains("reserve:"));
    assert_eq!(
        db.foreign_entries().expect("a listing"),
        Vec::<std::ffi::OsString>::new()
    );
    db.close().expect("a close");
    assert_eq!(on_reserve(&fs, ops), []);
    assert_eq!(held(&fs), 2 * KEPT);
}

/// Gives a handle whose background work is at rest three threads that
/// have ended: its close, and its drop, then do what those of a session
/// do, where the handle as it is would end as a process that is killed.
fn with_ended_threads(db: &Db) {
    let ended = || thread::spawn(|| {});
    *crate::db::store::lock(&db.threads) = Some(crate::db::Threads {
        write_back: ended(),
        reclaim: ended(),
        cleaner: ended(),
    });
}

/// What is asked of a database on a device that is full.
#[derive(Debug, Clone, Copy)]
enum Call {
    Commit,
    Write,
    Wipe,
    Flush,
    Checkpoint,
    Close,
    Drop,
}

/// Each call of the facade that writes is refused by a device that is
/// full, and the session has released its reserve when the call returns.
#[test]
fn a_call_that_a_full_device_refuses_releases_the_reserve() {
    let calls = [
        Call::Commit,
        Call::Write,
        Call::Wipe,
        Call::Flush,
        Call::Checkpoint,
        Call::Close,
        Call::Drop,
    ];
    for (seed, call) in (10..).zip(calls) {
        let context = format!("{call:?}");
        let (fs, _, db, mut state) = kept(seed);
        // A commit whose pages wait: a close has something to write.
        write(&db, &mut state, rewrite).expect("a commit");
        // A flush is refused its sync, and the others a write or a name.
        let full = match call {
            Call::Flush => Mode::Full,
            _ => Mode::FullButSyncs,
        };
        fs.add_rule(full.rule(0).torn_append(false));
        let for_room = |error: &(dyn std::error::Error + 'static)| {
            assert!(out_of_space(error), "{context}: {error}");
        };
        let db = match call {
            Call::Commit | Call::Write => {
                let in_txn = matches!(call, Call::Commit);
                for_room(&commit(&db, &rewrite(&state), in_txn).expect_err("a commit"));
                Some(db)
            }
            Call::Wipe => {
                for_room(&db.wipe().expect_err("a wipe"));
                Some(db)
            }
            Call::Flush => {
                for_room(&db.flush().expect_err("a flush"));
                Some(db)
            }
            Call::Checkpoint => {
                let target = Path::new("/checkpoint");
                let made = db.checkpoint(target, CheckpointOptions::default());
                for_room(&made.expect_err("a checkpoint"));
                Some(db)
            }
            Call::Close => {
                with_ended_threads(&db);
                for_room(&db.close().expect_err("a close"));
                None
            }
            Call::Drop => {
                with_ended_threads(&db);
                drop(db);
                None
            }
        };
        assert_eq!(held(&fs), 0, "{context}");
        if let Some(db) = db {
            let stats = db.stats().reserve;
            assert_eq!(
                (stats.held, stats.releases, stats.bytes_released),
                (false, 1, KEPT),
                "{context}: {stats:?}"
            );
            assert_eq!(stats.last_error, None, "{context}");
        }
    }
}

/// A turn of write-back that the device refuses for room releases the
/// reserve when the session is told of it, as write-back's loop tells it.
#[test]
fn a_turn_of_write_back_that_a_full_device_refuses_releases_the_reserve() {
    let (fs, _, db, mut state) = kept(20);
    write(&db, &mut state, rewrite).expect("a commit");
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let error = tick(&db).expect_err("a turn on a full device");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(held(&fs), KEPT, "the session is not told yet");
    db.shared.store.troubles.write_back_failed(&error);
    assert_eq!(held(&fs), 0);
    let stats = db.stats();
    assert_eq!(
        (
            stats.reserve.held,
            stats.reserve.releases,
            stats.reserve.bytes_released
        ),
        (false, 1, KEPT)
    );
    assert_eq!(stats.background.write_back_errors, 1);
    check_reads(&db, &state, "after the release");
}

/// A session at rest with small files, which keeps a reserve of [`KEPT`]
/// bytes and has rewritten its values three times: the value log holds
/// what the cleaner closes and cleans.
fn with_dead_values(seed: u64) -> Session {
    with_dead_values_and(seed, |_| {})
}

/// As [`with_dead_values`], opened with the options that `adjust` leaves.
fn with_dead_values_and(seed: u64, adjust: impl FnOnce(&mut Options)) -> Session {
    let mut session = small(seed, true, KEPT);
    adjust(&mut session.options);
    assert!(session.open());
    session.write("w1", first, false);
    session.ticks("t1");
    for _ in 0..3 {
        session.write("w", rewrite, false);
        session.ticks("t");
    }
    session
}

/// A pass of the cleaner that the device refuses for room releases the
/// reserve.
#[test]
fn a_pass_of_the_cleaner_that_a_full_device_refuses_releases_the_reserve() {
    let session = with_dead_values(21);
    let db = session.db.as_ref().expect("the database is open");
    let mut turns = CleanerTurns::begun(&db.shared.store);
    session.fs.add_rule(Mode::FullButSyncs.rule(0));
    assert_eq!(held(&session.fs), KEPT);
    assert!(
        cleaner_turn(db, &mut turns),
        "no pass of the cleaner failed"
    );
    assert_eq!(held(&session.fs), 0);
    let stats = db.stats();
    assert_eq!((stats.reserve.held, stats.reserve.releases), (false, 1));
    assert!(stats.background.cleaner_errors >= 1, "{stats}");
}

/// A database at rest that keeps a reserve, in which a Merk with values
/// was dropped and a commit followed: the reclaimer has cells to take
/// out, in page steps whose records go to the log.
fn with_a_dropped_merk(seed: u64) -> (SimVfs, Db, State) {
    let (fs, _, db, mut state) = kept(seed);
    let dropped = |state: &State| {
        let more = vec![height(state.version())];
        batch(
            Vec::new(),
            more,
            vec![OwnedSubtree::Drop(model::subtree(2))],
        )
    };
    write(&db, &mut state, dropped).expect("the drop");
    // A page that the latest commit wrote is freed after the next one.
    write(&db, &mut state, note).expect("a commit");
    tick(&db).expect("a turn");
    (fs, db, state)
}

/// A call of the reclaimer that the device refuses for room releases the
/// reserve.
#[test]
fn a_call_of_the_reclaimer_that_a_full_device_refuses_releases_the_reserve() {
    let (fs, db, state) = with_a_dropped_merk(22);
    fs.add_rule(Mode::FullButSyncs.rule(0));
    assert_eq!(held(&fs), KEPT);
    let round = db.reclaim();
    assert!(round.failed, "{round:?}");
    assert_eq!(held(&fs), 0);
    let stats = db.stats();
    assert_eq!((stats.reserve.held, stats.reserve.releases), (false, 1));
    assert!(stats.background.reclaim_errors >= 1, "{stats}");
    check_reads(&db, &state, "after the release");
}

/// An error that a full device did not cause releases nothing: of a
/// commit, and of a turn of write-back.
#[test]
fn a_refusal_that_is_not_for_room_releases_nothing() {
    let (fs, _, db, mut state) = kept(23);
    write(&db, &mut state, rewrite).expect("a commit");
    let failing = FaultRule::new(Fault::Eio).on(OpSet::WRITES | OpSet::NAMES);
    fs.add_rule(failing.forever());
    let ops = fs.stats().ops;
    let error = tick(&db).expect_err("a turn on a device that fails");
    assert!(!out_of_space(&error), "{error}");
    db.shared.store.troubles.write_back_failed(&error);
    let error = commit(&db, &note(&state), false).expect_err("a commit");
    assert!(!out_of_space(&error), "{error}");
    assert_eq!(on_reserve(&fs, ops), []);
    fs.clear_rules();
    assert_eq!(held(&fs), KEPT);
    let stats = db.stats().reserve;
    assert_eq!((stats.held, stats.releases), (true, 0), "{stats:?}");
}

/// A session that has released its reserve asks the device for nothing
/// on the reserve's names at the refusals that follow, and a session that
/// keeps no reserve at none: a reserve that somebody else made in its
/// directory stays.
#[test]
fn only_a_reserve_that_the_session_holds_is_released() {
    let (fs, _, db, state) = kept(24);
    fs.add_rule(Mode::FullButSyncs.rule(0).torn_append(false));
    let refuse = |db: &Db, state: &State| {
        commit(db, &note(state), false).expect_err("a commit on a full device");
        if let Err(error) = tick(db) {
            db.shared.store.troubles.write_back_failed(&error);
        }
    };
    refuse(&db, &state);
    assert_eq!(held(&fs), 0);
    let ops = fs.stats().ops;
    for _ in 0..3 {
        refuse(&db, &state);
    }
    assert_eq!(on_reserve(&fs, ops), []);
    assert_eq!(db.stats().reserve.releases, 1);
    drop(db);

    // The same refusals in a session that keeps none.
    let (fs, options) = device(25, 0);
    let db = open_at_rest(DIR, &options).expect("a new database");
    let mut state = State::new();
    write(&db, &mut state, first).expect("the first commit");
    tick(&db).expect("a turn");
    let theirs = Reserve::of(options.vfs.clone(), Path::new(DIR));
    theirs.fill(KEPT).expect("room for a reserve");
    fs.add_rule(Mode::FullButSyncs.rule(0).torn_append(false));
    let ops = fs.stats().ops;
    for _ in 0..3 {
        refuse(&db, &state);
    }
    assert_eq!(on_reserve(&fs, ops), []);
    assert_eq!(held(&fs), KEPT);
    assert_eq!(db.stats().reserve, ReserveStats::default());
}

/// A session at rest that released its reserve at a commit which the
/// device refused once, on a device that has room again, with the turns
/// of its cleaner, which has made none.
fn released(seed: u64) -> (SimVfs, Db, State, CleanerTurns) {
    let (fs, _, db, mut state) = kept(seed);
    let turns = CleanerTurns::begun(&db.shared.store);
    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, note).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(held(&fs), 0);
    (fs, db, state, turns)
}

/// The fill after a release waits for a commit and for a table
/// checkpoint, both after the first turn of the cleaner that followed the
/// refusal: neither is enough alone, in either order.
#[test]
fn the_reserve_is_filled_again_once_a_commit_and_a_table_checkpoint_have_followed() {
    for commit_first in [true, false] {
        let (fs, db, mut state, mut turns) = released(30 + u64::from(commit_first));
        let ops = fs.stats().ops;
        // With neither, turn after turn.
        for _ in 0..3 {
            assert!(!turn(&db, &mut turns));
        }
        match commit_first {
            true => drop(write(&db, &mut state, note).expect("a commit with room")),
            false => checkpoint(&db),
        }
        for _ in 0..3 {
            assert!(!turn(&db, &mut turns), "one of the two was enough");
        }
        assert_eq!(on_reserve(&fs, ops), [], "a fill was tried");
        match commit_first {
            true => checkpoint(&db),
            false => drop(write(&db, &mut state, note).expect("a commit with room")),
        }
        assert!(turn(&db, &mut turns), "the reserve is not filled again");
        assert_eq!(held(&fs), KEPT);
        let stats = db.stats().reserve;
        assert_eq!(
            (stats.releases, stats.fills, stats.fills_failed),
            (1, 2, 0),
            "{stats:?}"
        );
        check_reads(&db, &state, "with the reserve filled again");
    }
}

/// What counts is what follows the last refusal for room: a commit and a
/// table checkpoint that came before a later refusal fill nothing.
#[test]
fn a_refusal_for_room_puts_the_fill_off() {
    let (fs, db, mut state, mut turns) = released(32);
    assert!(!turn(&db, &mut turns));
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    // The device refuses the session once more, before the cleaner's
    // next turn.
    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, note).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    let ops = fs.stats().ops;
    for _ in 0..3 {
        assert!(!turn(&db, &mut turns));
    }
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    assert_eq!(on_reserve(&fs, ops), [], "a fill was tried");
    assert!(turn(&db, &mut turns));
    assert_eq!(db.stats().reserve.releases, 1);
}

/// The fill waits for a turn in which the cleaner was at rest: while a
/// turn moves one of the cleaner's counts of work, nothing is filled, and
/// the first turn that moves none fills the reserve.
#[test]
fn the_reserve_is_filled_again_in_a_turn_in_which_the_cleaner_was_at_rest() {
    let mut session = with_dead_values(33);
    let mut turns = {
        let db = session.db.as_ref().expect("the database is open");
        CleanerTurns::begun(&db.shared.store)
    };
    session.fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let latest = session.told.latest;
    session.write("refused", rewrite, false);
    assert_eq!(
        session.told.latest, latest,
        "a commit that the device refuses"
    );
    assert_eq!(held(&session.fs), 0);
    // Write-back cuts the failed append off the log, which takes the
    // records of the cleaner's page steps again.
    session.ticks("cut");
    {
        // The first turn notes how far the session has come, whatever
        // the cleaner does in it.
        let db = session.db.as_ref().expect("the database is open");
        cleaner_turn(db, &mut turns);
        assert!(!db.stats().reserve.held);
    }
    session.write("with room", rewrite, false);
    assert_eq!(session.told.latest, latest + 1);
    let db = session.db.as_ref().expect("the database is open");
    let store = &db.shared.store;
    checkpoint(db);
    let (mut worked, mut rested) = (0, 0);
    while rested == 0 {
        let before = work(store);
        let failed = cleaner_turn(db, &mut turns);
        assert!(!failed, "a pass of the cleaner failed");
        let at_work = work(store) != before;
        assert_eq!(
            db.stats().reserve.held,
            !at_work,
            "after {worked} turns in which the cleaner worked"
        );
        match at_work {
            true => worked += 1,
            false => rested += 1,
        }
        assert!(worked < 40, "the cleaner does not come to rest");
    }
    assert!(worked > 0, "the cleaner had nothing to do");
    assert_eq!(held(&session.fs), KEPT);
}

/// A fill that the device refuses leaves no reserve, and the next is
/// tried after a further commit and a further table checkpoint, and not
/// before.
#[test]
fn a_fill_that_fails_is_tried_again_after_a_further_commit_and_checkpoint() {
    let (fs, db, mut state, mut turns) = released(34);
    assert!(!turn(&db, &mut turns));
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    // Room for less than the reserve.
    fs.set_capacity(Some(fs.used_bytes() + KEPT - 1));
    let ops = fs.stats().ops;
    assert!(!turn(&db, &mut turns));
    let stats = db.stats().reserve;
    assert_eq!((stats.fills, stats.fills_failed), (1, 1), "{stats:?}");
    let said = stats.last_error.expect("what the fill said");
    assert!(said.starts_with("the fill of the reserve: "), "{said}");
    assert!(!on_reserve(&fs, ops).is_empty(), "no fill was tried");
    assert_eq!(held(&fs), 0);
    let unfinished = Path::new(DIR).join(Reserve::UNFINISHED_FILE_NAME);
    let left = fs.open(&unfinished, OpenMode::Read).map(drop);
    assert_eq!(
        left.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::NotFound)
    );

    // No further fill is tried by itself, nor after a commit alone.
    let ops = fs.stats().ops;
    for _ in 0..3 {
        assert!(!turn(&db, &mut turns));
    }
    write(&db, &mut state, note).expect("a commit with room");
    for _ in 0..3 {
        assert!(!turn(&db, &mut turns));
    }
    assert_eq!(on_reserve(&fs, ops), []);
    checkpoint(&db);
    assert!(!turn(&db, &mut turns));
    assert_eq!(db.stats().reserve.fills_failed, 2);

    // With room the same rule fills it.
    fs.set_capacity(None);
    assert!(!turn(&db, &mut turns));
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    assert!(turn(&db, &mut turns));
    assert_eq!(held(&fs), KEPT);
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.releases, stats.fills, stats.fills_failed),
        (1, 2, 2),
        "{stats:?}"
    );
    check_reads(&db, &state, "with the reserve filled again");
}

/// An open that cannot fill the reserve is made all the same, and the
/// session fills it by the rule it has after a release.
#[test]
fn an_open_that_cannot_fill_the_reserve_is_made_and_the_session_fills_it_later() {
    let (fs, options) = device(35, 0);
    let db = open_at_rest(DIR, &options).expect("a new database");
    let mut state = State::new();
    write(&db, &mut state, first).expect("the first commit");
    tick(&db).expect("a turn");
    db.close().expect("a close");

    // The device has room for the open, and none for the reserve.
    let unfinished = Path::new(DIR).join(Reserve::UNFINISHED_FILE_NAME);
    let no_room = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(unfinished);
    fs.add_rule(no_room.torn_append(false));
    let keeping = Options {
        reserve: KEPT,
        ..options
    };
    let db = open_at_rest(DIR, &keeping).expect("an open with no room for the reserve");
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.len, stats.held, stats.fills, stats.fills_failed),
        (KEPT, false, 0, 1),
        "{stats:?}"
    );
    assert_eq!(held(&fs), 0);
    check_reads(&db, &state, "with no reserve");

    let mut turns = CleanerTurns::begun(&db.shared.store);
    assert!(!turn(&db, &mut turns));
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    assert!(turn(&db, &mut turns));
    assert_eq!(held(&fs), KEPT);
}

/// Write-back's loop, running alone on a device that is full, releases
/// the reserve at its first turn that the device refuses, with nobody
/// who calls the database.
#[test]
fn write_backs_loop_releases_the_reserve_by_itself() {
    let (fs, _, db, mut state) = kept(40);
    write(&db, &mut state, rewrite).expect("a commit");
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::write_back(shared));
        let _halt = Halt(shared);
        until("write-back's loop releases the reserve", || {
            db.stats().reserve.releases == 1
        });
    });
    assert_eq!(held(&fs), 0);
    assert!(db.stats().background.write_back_errors >= 1);
    check_reads(&db, &state, "after the release");
}

/// The cleaner's loop, running alone, does the same.
#[test]
fn the_cleaners_loop_releases_the_reserve_by_itself() {
    let session = with_dead_values(41);
    let db = session.db.as_ref().expect("the database is open");
    session.fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        until("the cleaner's loop releases the reserve", || {
            db.stats().reserve.releases == 1
        });
    });
    assert_eq!(held(&session.fs), 0);
    assert!(db.stats().background.cleaner_errors >= 1);
}

/// The reclaimer's thread, running alone, does the same.
#[test]
fn the_reclaimers_thread_releases_the_reserve_by_itself() {
    let (fs, db, state) = with_a_dropped_merk(42);
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    thread::scope(|scope| {
        scope.spawn(|| shared.reclaim_loop(IDLE));
        let _halt = Halt(shared);
        until("the reclaimer's thread releases the reserve", || {
            db.stats().reserve.releases == 1
        });
    });
    assert_eq!(held(&fs), 0);
    assert!(db.stats().background.reclaim_errors >= 1);
    check_reads(&db, &state, "after the release");
}

/// The idle time of a loop that is to take no second turn by itself in
/// the time of a test: its wait after a failed turn is as long.
const HOUR: Duration = Duration::from_secs(3600);

/// Write-back's loop and the cleaner's wait for an hour.
fn waits_of_an_hour(options: &mut Options) {
    let page_size = options.page_size.expect("the tests name their page size");
    let flush = options.flush.get_or_insert(FlushConfig::new(page_size));
    flush.idle = HOUR;
    flush.max_failed_wait = HOUR;
    options.cleaner.get_or_insert_default().idle = HOUR;
}

/// DF12's exception: the release of the reserve ends the wait after a
/// failed turn. Write-back's loop and the cleaner's run on a device that
/// is full, and each would wait an hour after a turn that failed. The
/// first turn that is refused for room, of either, releases the reserve,
/// and each of the two loops then comes to its second turn by itself:
/// the one that released it, whose wait had not begun, and the other,
/// which was in its turn or in its wait.
#[test]
fn the_release_of_the_reserve_ends_the_waits_of_write_backs_loop_and_of_the_cleaners() {
    let mut session = with_dead_values_and(43, waits_of_an_hour);
    // Pages and values that wait for write-back.
    session.write("w", rewrite, false);
    let db = session.db.as_ref().expect("the database is open");
    session.fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    let store = &*shared.store;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::write_back(shared));
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        until("write-back's loop takes a second turn", || {
            store.flush.stats().errors >= 2
        });
        until("the cleaner's loop takes a second turn", || {
            store.cleaner.stats().passes >= 2
        });
    });
    let stats = db.stats().reserve;
    assert_eq!((stats.held, stats.releases), (false, 1), "{stats:?}");
    assert_eq!(held(&session.fs), 0);
}

/// Write-back's loop alone: the turn that is refused for room releases
/// the reserve, and the loop comes to its second turn by itself, where
/// its wait after a failed turn is an hour.
#[test]
fn write_backs_loop_takes_its_next_turn_at_once_after_it_released_the_reserve() {
    let (fs, _, db, mut state) = kept_with(46, waits_of_an_hour);
    write(&db, &mut state, rewrite).expect("a commit");
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::write_back(shared));
        let _halt = Halt(shared);
        until("write-back's loop takes a second turn", || {
            shared.store.flush.stats().errors >= 2
        });
    });
    assert_eq!(db.stats().reserve.releases, 1);
    assert_eq!(held(&fs), 0);
}

/// The cleaner's loop alone, in the same way.
#[test]
fn the_cleaners_loop_takes_its_next_turn_at_once_after_it_released_the_reserve() {
    let session = with_dead_values_and(47, waits_of_an_hour);
    let db = session.db.as_ref().expect("the database is open");
    session.fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        until("the cleaner's loop takes a second turn", || {
            shared.store.cleaner.stats().passes >= 2
        });
    });
    let stats = db.stats();
    assert_eq!(stats.reserve.releases, 1);
    assert!(stats.background.cleaner_errors >= 1, "{stats}");
}

/// The same for the reclaimer's thread: the round that is refused for
/// room releases the reserve, and the thread comes to its second round
/// by itself, where its wait after a failed round is an hour.
#[test]
fn the_release_of_the_reserve_ends_the_wait_of_the_reclaimers_thread() {
    let (fs, db, _) = with_a_dropped_merk(44);
    fs.add_rule(Mode::FullButSyncs.rule(0));
    let shared = &*db.shared;
    let signal = &shared.store.signal;
    thread::scope(|scope| {
        scope.spawn(|| shared.reclaim_loop(HOUR));
        let _halt = Halt(shared);
        until("the reclaimer's thread makes a second round", || {
            signal.rounds.load(Ordering::Relaxed) >= 2
        });
    });
    let stats = db.stats();
    assert_eq!((stats.reserve.held, stats.reserve.releases), (false, 1));
    assert!(stats.background.reclaim_errors >= 1, "{stats}");
}

/// A call of the facade that releases the reserve tells the loops as
/// well. The cleaner's loop has nothing to clean, so no turn of it fails,
/// and it sleeps for an hour after each: nothing of a commit wakes it.
/// The device refuses the appends to the log's file and nothing else. A
/// commit that is refused so releases the reserve, and the cleaner's
/// loop comes to its next turn by itself.
#[test]
fn a_call_that_releases_the_reserve_has_the_cleaners_loop_take_its_next_turn() {
    let (fs, _, db, state) = kept_with(45, waits_of_an_hour);
    let shared = &*db.shared;
    let store = &*shared.store;
    let passes = || store.cleaner.stats().passes;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        until("the cleaner's loop makes a turn", || passes() >= 1);
        let file = store.log.end().expect("the log has a file").file();
        let log = FileNames::WAL.path(Path::new(DIR), file);
        let refused = FaultRule::new(Fault::Enospc).on(OpKind::Append).at(log);
        fs.add_rule(refused.torn_append(false).forever());
        let error = commit(&db, &note(&state), false).expect_err("a commit on a full device");
        assert!(out_of_space(&error), "{error}");
        assert_eq!(db.stats().reserve.releases, 1);
        until("the cleaner's loop takes its next turn", || passes() >= 2);
    });
    assert_eq!(db.stats().background.cleaner_errors, 0);
}

/// What the reclaimer's thread does with a wait of an hour where it is
/// told that room was made, before the wait if `before` and during it
/// otherwise: whether the wait ended within a minute, and how long the
/// wait of `after` lasted that the thread began next, after an idle wait
/// in between if `idle`. A thread whose wait goes on is let go.
fn told_that_room_was_made(before: bool, idle: bool, after: Duration) -> (bool, Duration) {
    let signal = Signal::default();
    let ended = std::sync::atomic::AtomicBool::new(false);
    let lasted = Mutex::new(Duration::ZERO);
    if before {
        signal.room_made();
    }
    let in_time = thread::scope(|scope| {
        scope.spawn(|| {
            signal.attach(thread::current());
            signal.sleep(HOUR);
            ended.store(true, Ordering::Release);
            if idle {
                // The call comes before the idle wait, which ends at
                // once for it and uses it up.
                signal.room_made();
                signal.idle(HOUR);
            }
            let began = Instant::now();
            signal.sleep(after);
            *lasted.lock().expect("the time") = began.elapsed();
        });
        if !before {
            thread::sleep(Duration::from_millis(20));
            signal.room_made();
        }
        let began = Instant::now();
        while !ended.load(Ordering::Acquire) && began.elapsed() < Duration::from_secs(60) {
            thread::sleep(Duration::from_millis(1));
        }
        let in_time = ended.load(Ordering::Acquire);
        if !in_time {
            signal.stop();
        }
        in_time
    });
    let lasted = *lasted.lock().expect("the time");
    (in_time, lasted)
}

/// The wait of the reclaimer's thread itself: told that room was made,
/// before the wait or during it, it ends at once, and the wait after it
/// lasts as long as it was told to, also where an idle wait used a
/// later call up.
#[test]
fn a_call_that_room_was_made_ends_one_wait_of_the_reclaimers_thread() {
    let wait = Duration::from_millis(60);
    for (before, idle) in [(true, false), (false, false), (true, true)] {
        let (ended, lasted) = told_that_room_was_made(before, idle, wait);
        assert!(ended, "the wait went on: {before} {idle}");
        assert!(lasted >= wait, "{lasted:?}: {before} {idle}");
    }
}

/// A session with small files and short idle times that keeps a reserve
/// of `reserve` bytes, opened with its three threads, after its first
/// commit.
fn threaded(seed: u64, reserve: u64) -> Session {
    let mut session = small(seed, false, reserve);
    let flush = session.options.flush.as_mut().expect("a configuration");
    flush.idle = IDLE;
    flush.max_failed_wait = MOST;
    session.options.cleaner.as_mut().expect("a policy").idle = IDLE;
    session.options.idle = IDLE;
    assert!(session.open());
    session.write("w1", first, false);
    assert_eq!(held(&session.fs), reserve);
    session
}

fn stats(session: &Session) -> DbStats {
    session.db.as_ref().expect("the session goes on").stats()
}

/// Commits rewrites, one at a time, until the session holds its reserve:
/// each is a commit that makes a table checkpoint due, and between two of
/// them the threads have time to come to rest.
fn until_filled(session: &mut Session) -> u64 {
    let mut commits = 0;
    while !stats(session).reserve.held {
        session.write("on", rewrite, false);
        commits += 1;
        assert!(commits < 400, "the session does not fill its reserve again");
        let began = Instant::now();
        while !stats(session).reserve.held && began.elapsed() < Duration::from_millis(250) {
            thread::sleep(Duration::from_millis(1));
        }
    }
    commits
}

/// The most turns that a loop of `config` can end in `window` while every
/// turn fails and moves nothing on.
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

/// DF13 and DF12 on a device that stays full, under a database with its
/// three threads, at every size of the ladder: the device has no free
/// byte from one moment on. The session releases its reserve once, at the
/// first refusal for room, whichever thread meets it, and from then on it
/// makes no fill and no release however many turns fail; write-back's
/// failed turns are those its waits allow; the session has not ended and
/// reads on. With room again it takes the next commit and fills its
/// reserve again.
#[test]
fn on_a_device_that_stays_full_the_reserve_is_released_once_and_the_turns_wait() {
    for (seed, reserve) in (50..).zip(LADDER) {
        let context = format!("a reserve of {reserve} bytes");
        let mut session = threaded(seed, reserve);
        session.fs.set_capacity(Some(0));
        let latest = session.told.latest;
        session.write("refused", rewrite, false);
        assert_eq!(
            session.told.latest, latest,
            "{context}: a commit on a full device"
        );
        let released = |session: &Session| {
            let reserve = stats(session).reserve;
            (
                reserve.held,
                reserve.releases,
                reserve.fills,
                reserve.fills_failed,
            )
        };
        assert_eq!(released(&session), (false, 1, 1, 0), "{context}");
        assert_eq!(held(&session.fs), 0, "{context}");

        let (window, turns, most) = {
            let db = session.db.as_ref().expect("the session goes on");
            let store = &db.shared.store;
            let failed = || store.flush.stats().errors;
            // The log has an append to cut off, and no room for the file
            // that takes the cut: every turn fails. What a failed turn
            // can still move on without a write, the first has.
            let full_since = failed();
            until("write-back's loop fails two turns", || {
                failed() >= full_since + 2
            });
            let began = Instant::now();
            let before = failed();
            thread::sleep(WINDOW);
            let turns = failed() - before;
            let window = began.elapsed();
            (window, turns, most_turns(&store.flush.config(), window))
        };
        assert!(
            turns <= most,
            "{context}: {turns} failed turns in {window:?}, where the waits allow {most}"
        );
        assert_eq!(released(&session), (false, 1, 1, 0), "{context}");
        assert_eq!(held(&session.fs), 0, "{context}");
        let db = session.db.as_ref().expect("the session goes on");
        assert_eq!(db.reopen(), None, "{context}");
        session.check(&context);

        session.fs.set_capacity(None);
        session.write("with room", rewrite, false);
        assert_eq!(
            session.told.latest,
            latest + 1,
            "{context}: a commit with room"
        );
        let commits = until_filled(&mut session);
        assert_eq!(held(&session.fs), reserve, "{context}");
        let end = stats(&session).reserve;
        assert_eq!((end.releases, end.fills), (1, 2), "{context}: {end:?}");
        println!(
            "{context}: {turns} failed turns of write-back in {window:?} ({most} at most); \
             filled again after {commits} more commits, {} fills failed",
            end.fills_failed
        );
        session.check(&context);
        assert!(session.close(), "{context}: a close with room");
    }
}

/// A commit that adds 24 values of 3000 bytes under new keys: nothing is
/// dead after it.
fn add(state: &State) -> OwnedBatch {
    let round = state.version() as u32;
    let one = state.merk(1, |items| {
        for number in 0..24 {
            let key = key(b'a', 1000 + 24 * round + number);
            items.insert(key, value(number, 1, 3000));
        }
    });
    batch(vec![one], vec![height(state.version())], Vec::new())
}

/// A commit that takes every value out that [`add`] put in.
fn take_out(state: &State) -> OwnedBatch {
    let from = key(b'a', 1000);
    let one = state.merk(1, |items| items.retain(|key, _| *key < from));
    batch(vec![one], vec![height(state.version())], Vec::new())
}

/// What a session with its three threads did on a device that it filled.
struct Filled {
    /// The commits that were taken before the device refused the session
    /// something for room.
    commits: u64,
    /// Whether the session came through with the room that it had:
    /// somebody else made room otherwise.
    by_itself: bool,
    /// The commits that the session took from then until the room of
    /// the values was back and its reserve was full, and those that it
    /// refused.
    taken: u64,
    refused: u64,
    /// How often the session released its reserve and filled it, and
    /// the fills that failed.
    releases: u64,
    fills: u64,
    fills_failed: u64,
}

/// A database that is opened with its three threads fills its device
/// with values that are live, until the device refuses it something for
/// room: a commit, or the turn of write-back after one. The session has
/// released its reserve there, whichever thread met the refusal. The
/// workload goes on: its next commit takes those values out again, and
/// the commits after it rewrite others, with [`PAUSE`] after a commit
/// that was refused and [`BETWEEN`] after one that was taken. It goes on
/// until the values are out, the cleaner has dropped a file of the value
/// log, and the session holds its reserve.
///
/// Where `must` says so, the session has to come there with the room
/// that it has: its reserve, and what the refusal left free. Else
/// somebody makes room once [`IN_VAIN`] commits in a row were refused:
/// the device is given [`RESERVE`] bytes more.
///
/// What is counted is what the threads leave no choice in. The reserve
/// can be full again before the workload has made a commit of its own,
/// where the last commit of the filling was published after the refusal
/// that released it: so the releases and the fills are counted, which
/// only go up, and not what the session holds at a moment in between.
fn fills_its_device(seed: u64, reserve: u64, must: bool) -> Filled {
    let context = format!("a reserve of {reserve} bytes");
    let mut session = threaded(seed, reserve);
    let capacity = session.fs.used_bytes() + HEADROOM;
    session.fs.set_capacity(Some(capacity));
    let released = |session: &Session| stats(session).reserve.releases > 0;
    let mut commits = 0;
    loop {
        let latest = session.told.latest;
        session.write("more", add, false);
        if session.told.latest == latest {
            break;
        }
        commits += 1;
        assert!(commits < 400, "{context}: the device does not fill");
        // The workload waits for write-back between two commits, as a
        // node does between two blocks: until nothing waits to be
        // written, or the device has refused write-back the room for it.
        until("write-back catches up or is refused", || {
            let stats = stats(&session);
            let watch = &stats.store.watch;
            let caught_up = watch.unwritten == 0 && watch.pending_value_bytes == 0;
            caught_up || stats.reserve.releases > 0
        });
        if released(&session) {
            break;
        }
    }
    // The session met its first refusal for room at that commit, or in a
    // turn of its background work, and released its reserve there.
    assert!(
        released(&session),
        "{context}: {:?}",
        stats(&session).reserve
    );
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None, "{context}");
    session.check(&context);

    let began = Instant::now();
    let (mut taken, mut refused, mut in_a_row) = (0, 0, 0);
    let mut by_itself = true;
    let done = |session: &Session, taken: u64| {
        let stats = stats(session);
        taken > 0 && stats.reserve.held && stats.cleaner.value_files_dropped > 0
    };
    while !done(&session, taken) {
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "{context}: the session does not come to hold its reserve with the values out, with \
             {taken} commits taken and {refused} refused: {}",
            stats(&session)
        );
        let latest = session.told.latest;
        match taken {
            0 => session.write("out", take_out, false),
            _ => session.write("on", rewrite, false),
        }
        if session.told.latest == latest + 1 {
            taken += 1;
            in_a_row = 0;
            let waiting = Instant::now();
            while !done(&session, taken) && waiting.elapsed() < BETWEEN {
                thread::sleep(Duration::from_millis(1));
            }
            continue;
        }
        refused += 1;
        in_a_row += 1;
        if !must && by_itself && in_a_row == IN_VAIN {
            // The device stays full: somebody makes room.
            session.fs.set_capacity(Some(capacity + RESERVE));
            by_itself = false;
        }
        thread::sleep(PAUSE);
    }
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None, "{context}");
    session.check(&context);
    // The threads come to rest, and nothing takes a commit any more:
    // what the session holds then is what it has on its device.
    session.rest();
    let end = stats(&session).reserve;
    assert!(end.releases >= 1 && end.fills >= 2, "{context}: {end:?}");
    assert_eq!(
        held(&session.fs),
        match end.held {
            true => reserve,
            false => 0,
        },
        "{context}: {end:?}"
    );
    session.check(&context);
    assert!(session.close(), "{context}: a close with room");
    Filled {
        commits,
        by_itself,
        taken,
        refused,
        releases: end.releases,
        fills: end.fills,
        fills_failed: end.fills_failed,
    }
}

/// The acceptance of the reserve (DF11, DF13), at every size of the
/// ladder: on a device that fills while a workload runs through a
/// database with its three threads, the session releases its reserve at
/// the first refusal for room, the cleaner gives room back, commits go
/// on, and the reserve is full again: it was filled once more at least
/// than the open filled it. From [`ENOUGH`] bytes up the
/// session comes there with the room that it has. Below, that room is
/// the reserve and what the refusal left free, which is less than a
/// commit's 72 KiB: it is enough, or somebody makes room.
#[test]
fn a_device_that_fills_under_a_database_with_its_threads_gets_its_room_back() {
    for (seed, reserve) in (90..).zip(LADDER) {
        let filled = fills_its_device(seed, reserve, reserve >= ENOUGH);
        println!(
            "a reserve of {:5} KiB: full after {} commits; the session came through {}, with {} \
             commits that it took and {} that it refused; it released its reserve {} times and \
             filled it {} times, and {} fills failed",
            reserve >> 10,
            filled.commits,
            match filled.by_itself {
                true => "with the room that it had",
                false => "once somebody had made room",
            },
            filled.taken,
            filled.refused,
            filled.releases,
            filled.fills,
            filled.fills_failed
        );
    }
}

/// The turns of write-back that its loop would make until one moves
/// nothing on or fails, with a failure told to the session as the loop
/// tells it. Says whether one failed.
fn ticks(db: &Db) -> bool {
    for _ in 0..8 {
        match tick(db) {
            Ok(tick) if tick.progressed() => {}
            Ok(_) => return false,
            Err(error) => {
                db.shared.store.troubles.write_back_failed(&error);
                return true;
            }
        }
    }
    false
}

/// What a session did whose device filled while its cleaner was behind.
struct Gave {
    /// Whether the session came to hold its reserve again with the room
    /// of that reserve alone: somebody else made room otherwise.
    by_the_reserve: bool,
    /// The commits that the session took before it held its reserve
    /// again, and those that it refused.
    taken: u64,
    refused: u64,
    /// The bytes that the files of the database took when the device
    /// filled, and when the session had filled its reserve again.
    full: u64,
    after: u64,
    /// How often the session released its reserve, and how many of its
    /// fills failed.
    releases: u64,
    fills_failed: u64,
}

/// How many commits in a row the device refuses before the workload of
/// [`gives_room_back`] or of [`fills_its_device`] has somebody make room,
/// where the session need not get by with its reserve.
const IN_VAIN: u64 = 40;

/// How long such a workload waits after a commit that was refused. While
/// a commit is being made its transaction holds a copy of a page, which
/// write-back does not write and takes no table checkpoint over.
const PAUSE: Duration = Duration::from_millis(10);

/// How long such a workload waits after a commit that was taken, unless
/// the session holds its reserve before: it commits four times a second.
const BETWEEN: Duration = Duration::from_millis(250);

/// A session that keeps a reserve of `reserve` bytes fills its device
/// with a cleaner that is behind, as in `db::tests::room`: the session
/// is at rest, and its cleaner makes no turn. It has released its
/// reserve at the first refusal, and the reserve is all the room there
/// is. From then on the three loops of the session run, as the threads
/// of a database run them, and the workload goes on beside them until
/// the session holds its reserve again: it commits, waits [`PAUSE`]
/// after a commit that was refused and [`BETWEEN`] after one that was
/// taken.
///
/// Where `must` says so, the session has to come there with the room of
/// its reserve alone. Else somebody makes room once [`IN_VAIN`] commits
/// in a row were refused: the device is given [`RESERVE`] bytes more.
fn gives_room_back(seed: u64, reserve: u64, must: bool) -> Gave {
    let context = format!("a reserve of {reserve} bytes");
    let (fs, options) = {
        let mut session = small(seed, true, reserve);
        let flush = session.options.flush.as_mut().expect("a configuration");
        flush.idle = IDLE;
        flush.max_failed_wait = MOST;
        session.options.cleaner.as_mut().expect("a policy").idle = IDLE;
        session.options.idle = IDLE;
        (session.fs.clone(), session.options.clone())
    };
    let db = open_at_rest(DIR, &options).expect("a new database");
    let mut state = State::new();
    write(&db, &mut state, first).expect("the first commit");
    ticks(&db);
    let capacity = fs.used_bytes() + HEADROOM;
    fs.set_capacity(Some(capacity));
    for commits in 0.. {
        if write(&db, &mut state, rewrite).is_err() || ticks(&db) {
            break;
        }
        assert!(commits < 400, "{context}: the device does not fill");
    }
    let full = db.stats().reserve;
    assert_eq!(
        (full.held, full.releases),
        (false, 1),
        "{context}: {full:?}"
    );
    // What is free beyond the bytes of the reserve is somebody else's.
    let free = capacity - fs.used_bytes();
    let files = fs.used_bytes();
    let other = Reserve::of(options.vfs.clone(), Path::new("/"));
    other.fill(free - reserve).expect("the rest of the device");

    let shared = &*db.shared;
    let (taken, refused, by_the_reserve) = thread::scope(|scope| {
        scope.spawn(|| crate::db::write_back(shared));
        scope.spawn(|| shared.reclaim_loop(IDLE));
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        let began = Instant::now();
        // The session holds its reserve again, and the cleaner has
        // dropped a file of the dead values.
        let held = || {
            let stats = db.stats();
            stats.reserve.held && stats.cleaner.value_files_dropped > 0
        };
        let (mut taken, mut refused, mut in_a_row) = (0, 0, 0);
        let mut by_the_reserve = true;
        while !held() {
            assert!(
                began.elapsed() < Duration::from_secs(60),
                "{context}: the session does not fill its reserve again, with {taken} commits \
                 taken and {refused} refused: {}",
                db.stats()
            );
            if write(&db, &mut state, rewrite).is_ok() {
                taken += 1;
                in_a_row = 0;
                let waiting = Instant::now();
                while !held() && waiting.elapsed() < BETWEEN {
                    thread::sleep(Duration::from_millis(1));
                }
                continue;
            }
            refused += 1;
            in_a_row += 1;
            if !must && by_the_reserve && in_a_row == IN_VAIN {
                // The device stays full: somebody makes room.
                fs.set_capacity(Some(capacity + RESERVE));
                by_the_reserve = false;
            }
            thread::sleep(PAUSE);
        }
        (taken, refused, by_the_reserve)
    });
    // The loops have ended, and the session is at rest again. The fill
    // waited for a commit, and the loops ran beside none but the
    // workload's. A turn of a loop can have released the reserve again
    // since the workload saw it held: what the session says it holds is
    // what its directory has.
    let end = db.stats();
    assert!(taken > 0, "{context}: a fill with no commit before it");
    assert!(
        end.reserve.releases >= 1 && end.reserve.fills >= 2,
        "{context}: {end}"
    );
    let on_the_device = match end.reserve.held {
        true => reserve,
        false => 0,
    };
    assert_eq!(held(&fs), on_the_device, "{context}");
    assert_eq!(db.reopen(), None, "{context}");
    check_reads(&db, &state, &context);
    sound(&db, &context);
    let after = fs.used_bytes() - on_the_device - other.held().expect("their file");
    Gave {
        by_the_reserve,
        taken,
        refused,
        full: files,
        after,
        releases: end.reserve.releases,
        fills_failed: end.reserve.fills_failed,
    }
}

/// The reserve with which the session of [`gives_room_back`] has to get
/// by, whatever the order in which its loops and its workload take their
/// turns: the one that section 5.3 of the document sizes for this
/// database, a log file and a segment at their configured lengths and a
/// little beside them. It is six times the least reserve that was enough
/// in the one order of `db::tests::room`.
///
/// What has to be written before the room of the dead values comes back
/// is 160 KiB at most: the values and pages that wait for write-back
/// (40 KiB), and the copies of everything that is live in the value log
/// (109 KiB) and in the segments (15 KiB). The workload takes 72 KiB of
/// the room for each commit that it makes meanwhile, and it makes one in
/// each [`BETWEEN`], in which each loop has made a turn: its waits after
/// failed turns are [`MOST`] at most.
const ENOUGH: u64 = RESERVE;

/// DF11 and DF13 with the loops of a database running, at every size of
/// the ladder: the reserve is released at the first refusal for room,
/// and with a reserve of [`ENOUGH`] bytes or more the cleaner gives room
/// back, the session takes commits again and fills its reserve again,
/// with nobody who makes room. With a smaller one either that happens,
/// or the device stays full until somebody makes room, and then the
/// same follows: which of the two is the schedule's to say, since the
/// commits of the workload take the room of the reserve as write-back
/// and the cleaner do.
#[test]
fn a_session_whose_loops_run_gives_room_back_and_fills_its_reserve_again() {
    for (seed, reserve) in (70..).zip(LADDER) {
        let gave = gives_room_back(seed, reserve, reserve >= ENOUGH);
        println!(
            "a reserve of {:5} KiB: the session filled its reserve again {}, after {} commits \
             that it took and {} that it refused; its files went from {} KiB to {} KiB; it \
             released its reserve {} times, and {} of its fills failed",
            reserve >> 10,
            match gave.by_the_reserve {
                true => "with the room of the reserve alone",
                false => "once somebody had made room",
            },
            gave.taken,
            gave.refused,
            gave.full >> 10,
            gave.after >> 10,
            gave.releases,
            gave.fills_failed
        );
        assert!(gave.after < gave.full, "nothing came back");
    }
}
