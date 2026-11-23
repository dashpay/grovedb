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
    Create, OpenMode, SimVfs, Vfs,
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
/// checkpoint, both after the refusal: neither is enough alone, in
/// either order.
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

/// What counts is counted from the refusal, and not from a turn of the
/// cleaner's loop after it: with a commit and a table checkpoint since
/// the refusal, the first turn that the cleaner's loop ends at rest fills
/// the reserve, also where it is the first turn after the refusal.
#[test]
fn the_first_turn_at_rest_after_a_commit_and_a_checkpoint_fills_the_reserve() {
    let (fs, db, mut state, mut turns) = released(37);
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    assert!(turn(&db, &mut turns), "the reserve is not filled again");
    assert_eq!(held(&fs), KEPT);
    let stats = db.stats().reserve;
    assert_eq!((stats.releases, stats.fills), (1, 2), "{stats:?}");
}

/// The same where a pass of the cleaner over the value log met the
/// refusal: the session is told of it after the turn has ended, and the
/// commit and the table checkpoint that follow count all the same.
#[test]
fn a_refusal_in_the_cleaners_pass_over_the_values_is_what_the_fill_counts_from() {
    let mut session = with_dead_values(38);
    let mut turns = {
        let db = session.db.as_ref().expect("the database is open");
        CleanerTurns::begun(&db.shared.store)
    };
    {
        let db = session.db.as_ref().expect("the database is open");
        let shared = &*db.shared;
        let store = &*shared.store;
        let mut tried = 0;
        crate::db::with_cleaner(shared, |cleaner, owners| loop {
            cleaner.pass().expect("a pass over the segments");
            // The device refuses every append while the pass over the
            // value log is made, and at no other time.
            let refused = FaultRule::new(Fault::Enospc).on(OpKind::Append);
            session.fs.add_rule(refused.torn_append(false).forever());
            let values = crate::db::end_turn(shared, cleaner, owners, &mut turns);
            session.fs.clear_rules();
            if let Err(error) = values {
                assert!(out_of_space(&error), "{error}");
                // The turn has ended, and the session is told now.
                assert!(db.stats().reserve.held);
                store.troubles.cleaner_failed(&error);
                break;
            }
            tried += 1;
            assert!(tried < 40, "the pass over the value log writes nothing");
        });
        let stats = db.stats().reserve;
        assert_eq!((stats.held, stats.releases), (false, 1), "{stats:?}");
    }
    // Write-back cuts an append that failed off the log.
    session.ticks("cut");
    let latest = session.told.latest;
    session.write("with room", rewrite, false);
    assert_eq!(session.told.latest, latest + 1);
    let db = session.db.as_ref().expect("the database is open");
    let store = &db.shared.store;
    checkpoint(db);
    let mut worked = 0;
    loop {
        let before = work(store);
        assert!(
            !cleaner_turn(db, &mut turns),
            "a pass of the cleaner failed"
        );
        let at_work = work(store) != before;
        assert_eq!(
            db.stats().reserve.held,
            !at_work,
            "after {worked} turns in which the cleaner worked"
        );
        if !at_work {
            break;
        }
        worked += 1;
        assert!(worked < 40, "the cleaner does not come to rest");
    }
    assert_eq!(held(&session.fs), KEPT);
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

/// A fill can fail and leave the whole reserve on the device. An open
/// finds the reserve of the session before, at its length, and has only
/// its name to make durable, which the device refuses. The session does
/// not hold its reserve, and the file is there: the next refusal for room
/// takes it off the device, as it takes a reserve that is held.
#[test]
fn what_a_fill_that_failed_left_on_the_device_goes_at_the_next_refusal_for_room() {
    let (fs, options, db, mut state) = kept(36);
    drop(db);
    let syncs_of_the_directory = |after: u64| {
        let synced = |event: &Event| matches!(event, Event::Op(op) if op.op > after && op.kind == OpKind::SyncDir);
        fs.trace().iter().filter(|event| synced(event)).count() as u64
    };
    // An open that has nothing to recover, and the syncs of the directory
    // that the next such open makes: the fill's is the last of them.
    drop(open_at_rest(DIR, &options).expect("the database opens"));
    let ops = fs.stats().ops;
    let db = open_at_rest(DIR, &options).expect("the database opens");
    let made = syncs_of_the_directory(ops);
    assert!(made > 0, "an open syncs its directory");
    assert_eq!(db.stats().reserve.fills, 1);
    drop(db);

    let failing = FaultRule::new(Fault::Eio).on(OpKind::SyncDir);
    fs.add_rule(failing.skip(made - 1));
    let db = open_at_rest(DIR, &options).expect("an open whose fill fails");
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.fills, stats.fills_failed, stats.releases),
        (false, 0, 1, 0),
        "{stats:?}"
    );
    assert_eq!(held(&fs), KEPT, "the fill left the reserve under its name");

    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, note).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(held(&fs), 0);
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (false, 1, KEPT),
        "{stats:?}"
    );
    check_reads(&db, &state, "after the release");
}

/// A fill that failed and took its files away has left nothing: the
/// next refusal for room finds no file of the reserve, counts no release,
/// and the refusal after it asks the device for nothing on the reserve's
/// names.
#[test]
fn a_refusal_after_a_fill_that_left_nothing_counts_no_release() {
    let (fs, db, mut state, mut turns) = released(51);
    write(&db, &mut state, note).expect("a commit with room");
    checkpoint(&db);
    // Room for less than the reserve.
    fs.set_capacity(Some(fs.used_bytes() + KEPT - 1));
    assert!(!turn(&db, &mut turns));
    fs.set_capacity(None);
    let stats = db.stats().reserve;
    assert_eq!((stats.releases, stats.fills_failed), (1, 1), "{stats:?}");
    assert_eq!(held(&fs), 0);

    let refuse = |state: &mut State| {
        fs.add_rule(Mode::Once.rule(0).torn_append(false));
        let error = write(&db, state, note).expect_err("a commit that the device refuses");
        assert!(out_of_space(&error), "{error}");
    };
    let ops = fs.stats().ops;
    refuse(&mut state);
    assert!(!on_reserve(&fs, ops).is_empty(), "the session did not look");
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (false, 1, KEPT),
        "{stats:?}"
    );
    let ops = fs.stats().ops;
    refuse(&mut state);
    assert_eq!(on_reserve(&fs, ops), []);
}

/// A fill that was cut short can leave its bytes under the name that a
/// reserve is written under: here the device refuses the fill of an open
/// the name of the reserve, and then the removal of what the fill wrote.
/// The next refusal for room takes that file off the device, and counts
/// it as a release with its bytes.
#[test]
fn what_a_fill_that_was_cut_short_left_goes_at_the_next_refusal_for_room() {
    let (fs, options) = device(52, 0);
    let db = open_at_rest(DIR, &options).expect("a new database");
    let mut state = State::new();
    write(&db, &mut state, first).expect("the first commit");
    tick(&db).expect("a turn");
    db.close().expect("a close");

    let unfinished = Path::new(DIR).join(Reserve::UNFINISHED_FILE_NAME);
    let failing = |kind: OpKind| FaultRule::new(Fault::Eio).on(kind).at(&unfinished);
    fs.add_rule(failing(OpKind::Rename));
    fs.add_rule(failing(OpKind::RemoveFile));
    let keeping = Options {
        reserve: KEPT,
        ..options
    };
    let db = open_at_rest(DIR, &keeping).expect("an open whose fill fails");
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.fills, stats.fills_failed, stats.releases),
        (false, 0, 1, 0),
        "{stats:?}"
    );
    let left = || {
        fs.open(&unfinished, OpenMode::Read)
            .and_then(|file| file.len())
    };
    assert_eq!(left().expect("what the fill left"), KEPT);
    assert_eq!(held(&fs), 0);

    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, note).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(
        left().map_err(|error| error.kind()),
        Err(std::io::ErrorKind::NotFound)
    );
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (false, 1, KEPT),
        "{stats:?}"
    );
}

/// The two files of a reserve go each by itself. The session holds its
/// reserve, and a file is under the other name as well, as a fill that
/// was cut short leaves one. The device refuses the removal of the
/// reserve once. The first refusal for room takes the other file off the
/// device: that is a release with the bytes of that file, and the
/// session holds its reserve still. The next takes the reserve.
#[test]
fn a_release_takes_each_file_of_the_reserve_by_itself() {
    const LEFT: u64 = 5000;
    let (fs, _, db, mut state) = kept(54);
    let unfinished = Path::new(DIR).join(Reserve::UNFINISHED_FILE_NAME);
    let file = fs.open(&unfinished, OpenMode::Append(Create::New));
    let file = file.expect("a file of the other name");
    file.append(&vec![7; LEFT as usize]).expect("its bytes");
    drop(file);
    let whole = Path::new(DIR).join(Reserve::FILE_NAME);
    let failing = FaultRule::new(Fault::Eio).on(OpKind::RemoveFile);
    fs.add_rule(failing.at(whole));
    let refuse = |state: &mut State| {
        fs.add_rule(Mode::Once.rule(0).torn_append(false));
        let error = write(&db, state, note).expect_err("a commit that the device refuses");
        assert!(out_of_space(&error), "{error}");
    };

    refuse(&mut state);
    assert_eq!(held(&fs), KEPT, "the removal of the reserve was refused");
    let gone = fs.open(&unfinished, OpenMode::Read).map(drop);
    assert_eq!(
        gone.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::NotFound)
    );
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (true, 1, LEFT),
        "{stats:?}"
    );
    let said = stats.last_error.expect("what the removal said");
    assert!(said.starts_with("the release of the reserve: "), "{said}");

    refuse(&mut state);
    assert_eq!(held(&fs), 0);
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (false, 2, LEFT + KEPT),
        "{stats:?}"
    );
}

/// A release that the file layer refuses leaves the reserve as it is:
/// the session holds it still, says what the release said, and the next
/// refusal for room tries the release again.
#[test]
fn a_release_that_fails_is_tried_again_at_the_next_refusal_for_room() {
    let (fs, _, db, mut state) = kept(39);
    let file = Path::new(DIR).join(Reserve::FILE_NAME);
    let failing = FaultRule::new(Fault::Eio).on(OpKind::RemoveFile);
    fs.add_rule(failing.at(file));
    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, note).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(held(&fs), KEPT, "the release was refused");
    let stats = db.stats().reserve;
    assert_eq!((stats.held, stats.releases), (true, 0), "{stats:?}");
    let said = stats.last_error.expect("what the release said");
    assert!(said.starts_with("the release of the reserve: "), "{said}");

    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, note).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(held(&fs), 0);
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (false, 1, KEPT),
        "{stats:?}"
    );
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

/// The loops are told by whoever tells the session of the refusal, and
/// by no call of the facade after it: a turn of write-back that a
/// committer makes for its commit is told to the session where the
/// committer stands, and it may wait for write-back's loop next. Here a
/// turn of write-back that the device refuses for room is told to the
/// session as the commit path tells it, beside a cleaner's loop whose
/// idle wait is an hour: the loop comes to a second turn by itself.
#[test]
fn a_refusal_that_the_commit_path_tells_the_session_has_the_loops_take_their_next_turn() {
    let (fs, _, db, mut state) = kept_with(48, waits_of_an_hour);
    let shared = &*db.shared;
    let store = &*shared.store;
    let passes = || store.cleaner.stats().passes;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        until("the cleaner's loop makes a turn", || passes() >= 1);
        write(&db, &mut state, rewrite).expect("a commit");
        fs.add_rule(Mode::FullButSyncs.rule(0));
        let error = tick(&db).expect_err("a turn on a full device");
        assert!(out_of_space(&error), "{error}");
        fs.clear_rules();
        assert_eq!(
            passes(),
            1,
            "the cleaner's loop was woken by something else"
        );
        store.troubles.write_back_failed(&error);
        assert_eq!(db.stats().reserve.releases, 1);
        until("the cleaner's loop takes its next turn", || passes() >= 2);
    });
    assert_eq!(db.stats().background.cleaner_errors, 0);
}

/// The turn that a commit's thread makes for write-back, in a session
/// whose write-back has no loop and is over its bound after every
/// commit. The device has room for the group of a commit in the log and
/// none for what write-back then writes: the commit is published, the
/// turn that its thread makes after it is refused for room, and the
/// commit path tells the session (`CommitState::reporting`). The reserve
/// is released, and the cleaner's loop, whose idle wait is an hour, comes
/// to a second turn by itself.
#[test]
fn a_turn_that_a_commits_thread_makes_for_write_back_releases_the_reserve_and_tells_the_loops() {
    let (fs, _, db, mut state) = kept_with(49, |options| {
        waits_of_an_hour(options);
        options
            .flush
            .as_mut()
            .expect("a configuration")
            .max_unwritten = 0;
    });
    let shared = &*db.shared;
    let store = &*shared.store;
    let passes = || store.cleaner.stats().passes;
    thread::scope(|scope| {
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        until("the cleaner's loop makes a turn", || passes() >= 1);
        let failed = db.stats().background.write_back_errors;
        // A commit of rewrites is a group of 72 KiB, and write-back
        // writes as much for it.
        fs.set_capacity(Some(fs.used_bytes() + (80 << 10)));
        write(&db, &mut state, rewrite).expect("a commit that is published");
        let stats = db.stats();
        assert!(stats.background.write_back_errors > failed, "{stats}");
        assert_eq!(stats.reserve.releases, 1, "{stats}");
        until("the cleaner's loop takes its next turn", || passes() >= 2);
    });
}

/// A call that room was made which comes before the reclaimer's thread
/// can be woken is not lost: the idle wait that the thread comes to
/// first ends at once. A thread whose wait goes on is let go.
#[test]
fn a_call_that_room_was_made_before_the_reclaimers_thread_can_be_woken_ends_its_idle_wait() {
    let signal = Signal::default();
    let ended = std::sync::atomic::AtomicBool::new(false);
    signal.room_made();
    let in_time = thread::scope(|scope| {
        scope.spawn(|| {
            signal.attach(thread::current());
            signal.idle(HOUR);
            ended.store(true, Ordering::Release);
        });
        let began = Instant::now();
        while !ended.load(Ordering::Acquire) && began.elapsed() < Duration::from_secs(30) {
            thread::sleep(Duration::from_millis(1));
        }
        let in_time = ended.load(Ordering::Acquire);
        // A wake is lost on a thread that has not named itself yet.
        while !ended.load(Ordering::Acquire) {
            signal.wake();
            thread::sleep(Duration::from_millis(1));
        }
        in_time
    });
    assert!(in_time, "the idle wait went on");
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
        // The thread that met the refusal releases the reserve. A commit
        // can be answered before a thread of the background work that met
        // one has told the session (DF4), and the counts of the reserve
        // are read one by one.
        until("the session releases its reserve", || {
            released(&session) == (false, 1, 1, 0)
        });
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
    /// What the workload came to after the release.
    came: Came,
    /// How often the session released its reserve and filled it, and
    /// the fills that failed.
    releases: u64,
    fills: u64,
    fills_failed: u64,
}

/// What a workload came to after the session had released its reserve.
struct Came {
    /// How often somebody else made room: 0 where the session came to
    /// hold its reserve again with the room that it had.
    helped: u64,
    /// The commits that the session took before it held its reserve
    /// again, and those that it refused.
    taken: u64,
    refused: u64,
}

impl Came {
    fn by(&self, itself: &'static str) -> &'static str {
        match self.helped {
            0 => itself,
            _ => "once somebody had made room",
        }
    }
}

/// The writer of the two runs with threads, from the moment at which the
/// session has released its reserve.
trait Writer {
    fn db(&self) -> &Db;
    /// Makes a commit, the first after the release if `first`, and says
    /// whether it was taken.
    fn commit(&mut self, first: bool) -> bool;
    /// Makes the turns of write-back that its loop would make, for a
    /// session whose threads have ended.
    fn write_back(&mut self);
    /// Somebody makes room: the device is given [`RESERVE`] bytes more.
    fn make_room(&mut self);
}

/// What a writer looks at to say whether the session needs a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    /// How often something put a fill of the session's reserve off: an
    /// error that a full device caused, and a fill that failed.
    put_off: u64,
    /// The session's count of the table checkpoints that it installed.
    checkpoints: u64,
}

impl Seen {
    /// Reads the two counts, the first before the second. Each of what
    /// the first counts is counted once the session has read how far it
    /// has come (DF13's two marks). So a commit that begins after this
    /// read is published after the version that the session read, and
    /// the count of table checkpoints that is read here is no less than
    /// the one that the session read.
    fn now(db: &Db) -> Self {
        let reserve = db.stats().reserve;
        let put_off = reserve.refusals + reserve.fills_failed;
        let checkpoints = db.shared.store.flush.stats().checkpoints;
        Self {
            put_off,
            checkpoints,
        }
    }
}

/// What `writer` does once the session has released its reserve, until
/// the session has made a fill of its reserve, and the cleaner has
/// dropped a file of the value log, that `before` does not count:
/// `before` is what the session had counted when none of its threads
/// could have done either yet.
///
/// The writer looks at the session, and the run is over where both
/// counts have moved, also where the writer has made no commit yet.
/// **Else the writer commits when the session needs a commit.** A fill
/// waits for both of the session's marks to move on after the last
/// refusal for room (DF13), so that is:
///
/// - where it has made no commit yet;
/// - again where its last commit was refused;
/// - where something has put a fill off since the writer last looked
///   before a commit ([`Seen::put_off`]);
/// - where the count of table checkpoints is the one that the writer
///   read before its last commit, write-back has nothing to write, and
///   no table checkpoint is due: write-back's loop tries none until more
///   is logged.
///
/// Between two looks at the session it waits [`PAUSE`]. Whenever
/// [`IN_VAIN`] commits in a row were refused, somebody makes room.
///
/// How often the writer commits, and whether somebody has to make room,
/// is the schedule's to say. What the run asserts is what holds in every
/// schedule: it ends, within a minute, with the two counts moved.
fn goes_on(writer: &mut dyn Writer, context: &str, before: &DbStats) -> Came {
    let began = Instant::now();
    let mut came = Came {
        helped: 0,
        taken: 0,
        refused: 0,
    };
    // What the writer read before its last commit, or nothing where a
    // commit is due whatever it reads.
    let mut seen: Option<Seen> = None;
    let mut in_a_row = 0;
    loop {
        let now = Seen::now(writer.db());
        let stats = writer.db().stats();
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "{context}: the session does not fill its reserve again, with {} commits taken and \
             {} refused: {stats}",
            came.taken,
            came.refused
        );
        let filled = stats.reserve.fills > before.reserve.fills;
        let out = stats.cleaner.value_files_dropped > before.cleaner.value_files_dropped;
        if filled && out {
            return came;
        }
        let needed = match seen {
            None => true,
            Some(seen) if seen.put_off != now.put_off => true,
            Some(seen) => {
                let watch = &stats.store.watch;
                seen.checkpoints == now.checkpoints
                    && watch.unwritten == 0
                    && watch.pending_value_bytes == 0
                    && !writer.db().shared.write_back().checkpoint_due()
            }
        };
        if needed {
            seen = Some(now);
            match writer.commit(came.taken == 0) {
                true => {
                    came.taken += 1;
                    in_a_row = 0;
                }
                false => {
                    came.refused += 1;
                    in_a_row += 1;
                    seen = None;
                    if in_a_row % IN_VAIN == 0 {
                        // The device stays full: somebody makes room.
                        writer.make_room();
                        came.helped += 1;
                    }
                }
            }
        }
        thread::sleep(PAUSE);
    }
}

/// Has a session whose threads have ended hold its reserve, where it
/// does not: a thread can have told the session of a refusal after the
/// fill that the workload saw, as it ended or before, and that released
/// the reserve once more. The session then fills it by the same rule,
/// with its turns made here. Each round
/// makes a commit, whatever became of the one before, since a fill waits
/// for a commit that follows the last refusal; then write-back's turns;
/// then turns of the cleaner, until one fills the reserve.
fn holds_its_reserve_at_rest(writer: &mut dyn Writer, context: &str) {
    let mut turns = CleanerTurns::begun(&writer.db().shared.store);
    for _ in 0..8 {
        if writer.db().stats().reserve.held {
            return;
        }
        writer.commit(false);
        writer.write_back();
        for _ in 0..40 {
            let db = writer.db();
            if !cleaner_turn(db, &mut turns) && db.stats().reserve.held {
                return;
            }
        }
    }
    panic!(
        "{context}: the session at rest does not fill its reserve: {}",
        writer.db().stats()
    );
}

/// [`holds_its_reserve_at_rest`] by itself: a session at rest with the
/// small files of the two runs, which released its reserve at a commit
/// that the device refused once, holds it again after the turns that
/// are made there. The device refuses the first commit that is made
/// there as well: the next round makes another.
#[test]
fn a_session_at_rest_that_released_its_reserve_fills_it_by_its_turns() {
    let (fs, options) = {
        let session = small(53, true, KEPT);
        (session.fs.clone(), session.options.clone())
    };
    let db = open_at_rest(DIR, &options).expect("a new database");
    let mut state = State::new();
    write(&db, &mut state, first).expect("the first commit");
    ticks(&db);
    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let error = write(&db, &mut state, rewrite).expect_err("a commit that the device refuses");
    assert!(out_of_space(&error), "{error}");
    assert_eq!(held(&fs), 0);
    ticks(&db);
    fs.add_rule(Mode::Once.rule(0).torn_append(false));
    let mut writer = BesideTheLoops {
        db: &db,
        state: &mut state,
        fs: &fs,
        capacity: 0,
    };
    holds_its_reserve_at_rest(&mut writer, "at rest");
    assert_eq!(held(&fs), KEPT);
    let stats = db.stats().reserve;
    assert_eq!(
        (stats.refusals, stats.releases, stats.fills),
        (2, 1, 2),
        "{stats:?}"
    );
    check_reads(&db, &state, "with the reserve filled again");
}

/// The writer of [`fills_its_device`].
struct OnItsDevice<'a> {
    session: &'a mut Session,
    capacity: u64,
}

impl Writer for OnItsDevice<'_> {
    fn commit(&mut self, first: bool) -> bool {
        let latest = self.session.told.latest;
        match first {
            true => self.session.write("out", take_out, false),
            false => self.session.write("on", rewrite, false),
        }
        self.session.told.latest == latest + 1
    }

    fn db(&self) -> &Db {
        self.session.db.as_ref().expect("the session goes on")
    }

    fn write_back(&mut self) {
        self.session.ticks("t");
    }

    fn make_room(&mut self) {
        self.capacity += RESERVE;
        (self.session.fs).set_capacity(Some(self.capacity));
    }
}

/// A database that is opened with its three threads fills its device
/// with values that are live, until its writer sees that the device has
/// refused the session something for room: its own commit is refused,
/// or the session has released its reserve, which a thread of the
/// background work does where it is the one that is refused. The writer
/// looks after each commit, so one more commit of the filling can be
/// taken after the release. The run waits for the release. Then the
/// workload goes on as [`goes_on`] says: what it commits takes those
/// values out again until such a commit is taken, and rewrites others
/// after that. Where the session has made its fill and its cleaner has
/// dropped a file by then, the writer commits nothing, and the values
/// stay. It comes through with the room that the session has, or once
/// somebody has made room: what the session is told after the release,
/// and with that the commits of the writer, is the schedule's to say.
fn fills_its_device(seed: u64, reserve: u64) -> Filled {
    let context = format!("a reserve of {reserve} bytes");
    let mut session = threaded(seed, reserve);
    // The device has had room for everything: the fill of the open is
    // the only one.
    let opened = stats(&session);
    assert_eq!(opened.reserve.fills, 1, "{context}");
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
    // The session met a refusal for room at that commit, or in a turn
    // of its background work, and the thread that met it releases the
    // reserve. A commit can be answered before a thread of the
    // background work that met one has told the session (DF4).
    until("the session releases its reserve", || released(&session));
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None, "{context}");
    session.check(&context);

    let came = {
        let mut writer = OnItsDevice {
            session: &mut session,
            capacity,
        };
        goes_on(&mut writer, &context, &opened)
    };
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None, "{context}");
    // The threads end, each after the turn that it is in. Where the
    // reserve is out then, the session at rest fills it by its turns: a
    // thread can have told the session of a refusal after the fill that
    // the workload saw, and released the reserve once more.
    session.rest();
    {
        let mut writer = OnItsDevice {
            session: &mut session,
            capacity,
        };
        holds_its_reserve_at_rest(&mut writer, &context);
    }
    let end = stats(&session).reserve;
    assert!(
        end.held && end.releases >= 1 && end.fills >= 2,
        "{context}: {end:?}"
    );
    assert_eq!(held(&session.fs), reserve, "{context}: {end:?}");
    session.check(&context);
    assert!(session.close(), "{context}: a close with room");
    Filled {
        commits,
        came,
        releases: end.releases,
        fills: end.fills,
        fills_failed: end.fills_failed,
    }
}

/// The acceptance of the reserve (DF11, DF13), at every size of the
/// ladder: on a device that fills while a workload runs through a
/// database with its three threads, the session releases its reserve
/// once the device has refused it something for room; it comes to a
/// fill of its reserve, and its cleaner to a dropped file of the value
/// log, within a minute, with the room that it had or once somebody had
/// made room; and with its threads ended it holds its reserve, after
/// the turns of a session at rest where the reserve was out by then.
#[test]
fn a_device_that_fills_under_a_database_with_its_threads_gets_its_room_back() {
    for (seed, reserve) in (90..).zip(LADDER) {
        let filled = fills_its_device(seed, reserve);
        println!(
            "a reserve of {:5} KiB: full after {} commits; the session came through {}, with {} \
             commits that it took and {} that it refused; it released its reserve {} times and \
             filled it {} times, and {} fills failed",
            reserve >> 10,
            filled.commits,
            filled.came.by("with the room that it had"),
            filled.came.taken,
            filled.came.refused,
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
    /// What the workload came to after the release.
    came: Came,
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
/// [`goes_on`] has somebody make room.
const IN_VAIN: u64 = 40;

/// How long such a workload waits between two looks at the session.
const PAUSE: Duration = Duration::from_millis(10);

/// The writer of [`gives_room_back`].
struct BesideTheLoops<'a> {
    db: &'a Db,
    state: &'a mut State,
    fs: &'a SimVfs,
    capacity: u64,
}

impl Writer for BesideTheLoops<'_> {
    fn commit(&mut self, _first: bool) -> bool {
        write(self.db, self.state, rewrite).is_ok()
    }

    fn db(&self) -> &Db {
        self.db
    }

    fn write_back(&mut self) {
        ticks(self.db);
    }

    fn make_room(&mut self) {
        self.capacity += RESERVE;
        self.fs.set_capacity(Some(self.capacity));
    }
}

/// A session that keeps a reserve of `reserve` bytes fills its device
/// with a cleaner that is behind, as in `db::tests::room`: the session
/// is at rest, and its cleaner makes no turn. It has released its
/// reserve at the first refusal, and the reserve is all the room there
/// is. From then on the three loops of the session run, as the threads
/// of a database run them, and the workload goes on beside them as
/// [`goes_on`] says. It comes through with the room of the reserve
/// alone, or once somebody has made room.
fn gives_room_back(seed: u64, reserve: u64) -> Gave {
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
    // No loop runs yet.
    let at_rest = db.stats();
    let came = thread::scope(|scope| {
        scope.spawn(|| crate::db::write_back(shared));
        scope.spawn(|| shared.reclaim_loop(IDLE));
        scope.spawn(|| crate::db::clean(shared));
        let _halt = Halt(shared);
        let mut writer = BesideTheLoops {
            db: &db,
            state: &mut state,
            fs: &fs,
            capacity,
        };
        goes_on(&mut writer, &context, &at_rest)
    });
    // The loops have ended, each after the turn that it was in.
    {
        let mut writer = BesideTheLoops {
            db: &db,
            state: &mut state,
            fs: &fs,
            capacity,
        };
        holds_its_reserve_at_rest(&mut writer, &context);
    }
    let end = db.stats();
    assert!(
        end.reserve.held && end.reserve.releases >= 1 && end.reserve.fills >= 2,
        "{context}: {end}"
    );
    assert_eq!(held(&fs), reserve, "{context}");
    assert_eq!(db.reopen(), None, "{context}");
    check_reads(&db, &state, &context);
    sound(&db, &context);
    let after = fs.used_bytes() - reserve - other.held().expect("their file");
    Gave {
        came,
        full: files,
        after,
        releases: end.reserve.releases,
        fills_failed: end.reserve.fills_failed,
    }
}

/// DF11 and DF13 with the loops of a database running, at every size of
/// the ladder: the reserve was released at the first refusal for room,
/// with the session at rest. With its loops running the session comes to
/// a fill of its reserve, and its cleaner to a dropped file of the value
/// log, within a minute; and with its loops ended it holds its reserve,
/// after the turns of a session at rest where the reserve was out by
/// then. Whether it comes there with the room of the reserve alone is
/// the schedule's to say, since the commits of the workload take the
/// room of the reserve as write-back and the cleaner do: what each size
/// came to is printed. `db::tests::room` measures two fixed orders of
/// turns, with a session at rest.
#[test]
fn a_session_whose_loops_run_gives_room_back_and_fills_its_reserve_again() {
    for (seed, reserve) in (70..).zip(LADDER) {
        let gave = gives_room_back(seed, reserve);
        println!(
            "a reserve of {:5} KiB: the session filled its reserve again {}, after {} commits \
             that it took and {} that it refused; its files went from {} KiB to {} KiB; it \
             released its reserve {} times, and {} of its fills failed",
            reserve >> 10,
            gave.came.by("with the room of the reserve alone"),
            gave.came.taken,
            gave.came.refused,
            gave.full >> 10,
            gave.after >> 10,
            gave.releases,
            gave.fills_failed
        );
    }
}
