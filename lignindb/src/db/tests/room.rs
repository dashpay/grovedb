//! OPS-08: a device that fills gives room back through the reserve of the
//! session, and not without it (`docs/lignindb/disk-full-and-backup.md`,
//! DF10, DF11 and DF13; [`crate::space`]).
//!
//! The simulated device has a capacity in bytes. A session that keeps a
//! reserve ([`Options::reserve`]) writes on it until the device first
//! refuses it something for room, with a cleaner that is behind: it has
//! made no turn. The session has released its reserve by then, and what
//! is free beyond the reserve's bytes is taken by somebody else, so that
//! the reserve is all the room there is. Then the order of DF11 is gone
//! through, with the turns that the threads of a database make: with the
//! room of the reserve write-back and the cleaner give back what is dead,
//! the session takes the next commit, and it fills its reserve again. A
//! session that keeps no reserve gets nothing back.
//!
//! The session of these runs has none of its threads, and the run makes
//! their turns in one order, so that what a reserve of a given size is
//! enough for is the same in every run. `db::tests::reserve` has a session
//! whose threads run.

use std::path::Path;

use lignin_format::PageSize;
use lignin_io::{EntryKind, OpenMode, Vfs};
use lignin_store::{cleaner::CleanerConfig, flush::FlushConfig, wal::SyncPolicy};

use super::{full::Session, *};
use crate::{
    backup::tests::world::{batch, first, height, key, value, State},
    db::reserve::CleanerTurns,
    space::{out_of_space, Reserve},
    txn::tests::model::OwnedBatch,
};

pub(super) const DIR: &str = "/db";

/// The room the device has beyond what the database takes when it is
/// given its capacity.
pub(super) const HEADROOM: u64 = 512 << 10;

/// What a refused operation may leave or take away on a device with no
/// free byte: less than two pages, of files that are made and removed
/// again.
const SLACK: u64 = 8 << 10;

/// A commit that writes every value of 24 keys again: what it wrote the
/// last time is dead from then on.
pub(super) fn rewrite(state: &State) -> OwnedBatch {
    if !state.has(1) {
        return first(state);
    }
    let revision = (state.version() % 251) as u8;
    let one = state.merk(1, |items| {
        for number in 100..124 {
            items.insert(key(b'a', number), value(number, revision, 3000));
        }
    });
    batch(vec![one], vec![height(state.version())], Vec::new())
}

/// A session of these tests, not opened yet: small files, so that a
/// device of a megabyte fills and empties, and a reserve of `reserve`
/// bytes.
pub(super) fn small(seed: u64, halted: bool, reserve: u64) -> Session {
    let mut session = Session::new(seed, SyncPolicy::Never, halted);
    session.options.flush = Some(FlushConfig {
        checkpoint_log_bytes: 64 << 10,
        segment_len: 256 << 10,
        ..FlushConfig::new(PageSize::MIN)
    });
    session.options.log_file_len = Some(96 << 10);
    session.options.cleaner = Some(CleanerConfig {
        segment_len: 256 << 10,
        min_segment_len: 16 << 10,
        ..CleanerConfig::new()
    });
    session.options.reserve = reserve;
    session
}

/// One turn of the cleaner's loop, as the thread of a database makes it:
/// the pass over the segments, the pass over the value log, the look at
/// the turn for the reserve, and each pass that failed told to the
/// session. Says whether a pass failed.
pub(super) fn cleaner_turn(db: &Db, turns: &mut CleanerTurns) -> bool {
    let shared = &*db.shared;
    let store = &*shared.store;
    crate::db::with_cleaner(shared, |cleaner, owners| {
        let passed = cleaner.pass().map(drop);
        if let Err(error) = &passed {
            assert!(out_of_space(error), "{error}");
            store.troubles.cleaner_failed(error);
        }
        let values = crate::db::end_turn(shared, cleaner, owners, turns).map(drop);
        if let Err(error) = &values {
            assert!(out_of_space(error), "{error}");
            store.troubles.cleaner_failed(error);
        }
        passed.is_err() || values.is_err()
    })
}

/// The bytes that the files of each directory of the store hold, and those
/// of the files beside them, for the measurement of section 5.3.
fn sizes(session: &Session) -> String {
    let fs = &session.fs;
    let len = |path: &Path| {
        let file = fs.open(path, OpenMode::Read).expect("a file");
        file.len().expect("its length")
    };
    let mut said = Vec::new();
    let mut beside = 0;
    for entry in fs.read_dir(Path::new(DIR)).expect("the directory") {
        let path = Path::new(DIR).join(&entry.name);
        match entry.kind {
            EntryKind::Dir => {
                let files = fs.read_dir(&path).expect("a directory of the store");
                let bytes: u64 = files.iter().map(|file| len(&path.join(&file.name))).sum();
                let name = entry.name.to_string_lossy().into_owned();
                said.push(format!(
                    "{name} {} KiB in {} files",
                    bytes >> 10,
                    files.len()
                ));
            }
            _ => beside += len(&path),
        }
    }
    said.push(format!("beside them {} KiB", beside >> 10));
    said.join(", ")
}

/// What one run found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Found {
    /// The commits that were taken before the device first refused.
    filled_by: u64,
    /// Whether the first refusal was of a commit: it was of a turn of
    /// write-back otherwise.
    commit_refused: bool,
    /// The bytes the files of the database took when it was full.
    full: u64,
    /// The bytes that write-back gave back with the room of the reserve.
    write_back_gave: u64,
    /// The bytes that the cleaner gave back after it.
    cleaner_gave: u64,
    /// In the order in which the writer comes first: whether the commit
    /// that it made before write-back's turns was taken.
    first_taken: Option<bool>,
    /// Whether the session took a commit then.
    resumed: bool,
    /// Whether the session filled its reserve again.
    refilled: bool,
}

/// One run of a session that keeps a reserve of `reserve` bytes. Where
/// `writer_first` is set, the session is asked for a commit as soon as
/// it has released its reserve, before write-back and the cleaner have
/// had a turn: the order of DF11 is gone through after that.
fn run(reserve: u64, writer_first: bool) -> Found {
    let mut session = small(41, true, reserve);
    assert!(session.open());
    let room = Reserve::of(session.options.vfs.clone(), Path::new(DIR));
    let other = Reserve::of(session.options.vfs.clone(), Path::new("/"));
    let used = |session: &Session| session.fs.used_bytes();
    let kept = |session: &Session| {
        let db = session.db.as_ref().expect("the session goes on");
        db.stats().reserve
    };
    // The open filled the reserve.
    assert_eq!(room.held().expect("the reserve"), reserve);
    assert_eq!(
        (kept(&session).held, kept(&session).fills),
        (reserve > 0, u64::from(reserve > 0))
    );

    session.write("w1", first, false);
    session.ticks("t1");
    let capacity = used(&session) + HEADROOM;
    session.fs.set_capacity(Some(capacity));
    // The cleaner is behind from here on: it makes no turn while the
    // device fills, and what the rewrites leave dead stays.
    let mut turns = {
        let db = session.db.as_ref().expect("the database is open");
        CleanerTurns::begun(&db.shared.store)
    };

    // The database is written until the device first refuses the session
    // something for room: a commit, or the turn of write-back after one.
    let mut filled_by = 0;
    let commit_refused = loop {
        let before = session.told.latest;
        session.write("fill", rewrite, false);
        if session.told.latest == before {
            break true;
        }
        filled_by += 1;
        if session.ticks("t") {
            break false;
        }
        assert!(filled_by < 400, "the device does not fill");
    };
    // DF13: the session released its reserve at that refusal, and it has
    // made no operation on a file for it otherwise.
    let stats = kept(&session);
    assert_eq!(
        (stats.held, stats.releases, stats.bytes_released),
        (false, u64::from(reserve > 0), reserve),
        "{stats:?}"
    );
    assert_eq!(room.held().expect("no reserve"), 0);
    // What is free beyond the bytes of the reserve is somebody else's
    // from here on: the reserve is all the room there is.
    let free = capacity.saturating_sub(used(&session));
    assert!(
        free >= reserve,
        "{free} bytes free of a reserve of {reserve}"
    );
    let files = used(&session);
    other.fill(free - reserve).expect("the rest of the device");
    println!(
        "full with a reserve of {} KiB: {}",
        reserve >> 10,
        sizes(&session)
    );

    if reserve == 0 {
        // DF10: with no free byte nothing comes back, whatever is tried,
        // and nothing is lost: the session reads on, and has not ended.
        for _ in 0..3 {
            session.ticks("full");
            let db = session.db.as_ref().expect("the session goes on");
            cleaner_turn(db, &mut turns);
            let before = session.told.latest;
            session.write("refused", rewrite, false);
            assert_eq!(session.told.latest, before, "a commit on a full device");
            assert!(
                used(&session) + SLACK >= capacity,
                "room came back with nothing released"
            );
        }
        let db = session.db.as_ref().expect("the session goes on");
        assert_eq!(db.reopen(), None);
        session.check("with no free byte");
    }

    let first_taken = writer_first.then(|| {
        let published = session.told.latest;
        session.write("first", rewrite, false);
        session.told.latest == published + 1
    });

    // DF11, step 3: write-back's turns. Step 4: the cleaner's turns, each
    // with the turn of write-back that follows it. What they give back
    // is counted without the reserve, which the session can have filled
    // again by then where its writer came first.
    let files_now =
        |session: &Session| used(session) - room.held().expect("the bytes of the reserve");
    let before = files_now(&session);
    for _ in 0..4 {
        session.ticks("back");
    }
    let after_write_back = files_now(&session);
    for _ in 0..12 {
        let db = session.db.as_ref().expect("the session goes on");
        cleaner_turn(db, &mut turns);
        session.ticks("cleaned");
    }
    let after_cleaner = files_now(&session);
    println!("after the cleaner: {}", sizes(&session));
    // Step 5: the session takes the next commit, if there is room for it.
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None, "a full device that refused no sync");
    let published = session.told.latest;
    session.write("resumed", rewrite, false);
    let resumed = session.told.latest == published + 1;
    // Step 6: the session fills its reserve again, once a commit was
    // published and a table checkpoint was installed and the cleaner is
    // at rest, if there is room for it.
    for _ in 0..12 {
        session.ticks("t");
        let db = session.db.as_ref().expect("the session goes on");
        cleaner_turn(db, &mut turns);
    }
    let stats = kept(&session);
    let refilled = stats.held;
    assert_eq!(
        room.held().expect("the reserve"),
        match refilled {
            true => reserve,
            false => 0,
        }
    );
    assert_eq!(stats.releases, u64::from(reserve > 0), "{stats:?}");
    assert_eq!(stats.fills, u64::from(reserve > 0) + u64::from(refilled));
    assert!(resumed || !refilled, "{stats:?}");

    session.check("at the end");
    session.check_cut("at the end");
    Found {
        filled_by,
        commit_refused,
        full: files,
        write_back_gave: before.saturating_sub(after_write_back),
        cleaner_gave: after_write_back.saturating_sub(after_cleaner),
        first_taken,
        resumed,
        refilled,
    }
}

/// The reserve that the runs of these tests keep: one log file and one
/// segment of the cleaner, as the runs configure them, and a table
/// checkpoint and a manifest beside them
/// (`docs/lignindb/disk-full-and-backup.md`, section 5.3).
pub(super) const RESERVE: u64 = (96 << 10) + (256 << 10) + (32 << 10);

/// One line of what a run found.
fn said(reserve: u64, found: &Found) -> String {
    format!(
        "a reserve of {:5} KiB: the device refused {} after {} commits, with {} KiB of files; \
         {}with the room of the reserve write-back gave back {} KiB and the cleaner {} KiB; the \
         session {} the next commit, and {} its reserve again",
        reserve >> 10,
        match found.commit_refused {
            true => "a commit",
            false => "a turn of write-back",
        },
        found.filled_by,
        found.full >> 10,
        match found.first_taken {
            None => "",
            Some(true) => "the writer's commit before their turns was taken; ",
            Some(false) => "the writer's commit before their turns was refused; ",
        },
        found.write_back_gave >> 10,
        found.cleaner_gave >> 10,
        match found.resumed {
            true => "took",
            false => "was refused",
        },
        match found.refilled {
            true => "filled",
            false => "did not fill",
        },
    )
}

#[test]
fn a_full_device_gives_room_back_through_the_reserve_and_not_without_it() {
    let without = run(0, false);
    println!("{}", said(0, &without));
    assert!(without.filled_by > 2, "{without:?}");
    assert!(
        without.write_back_gave + without.cleaner_gave < SLACK,
        "room came back with nothing released: {without:?}"
    );
    assert!(!without.resumed && !without.refilled, "{without:?}");

    let with = run(RESERVE, false);
    println!("{}", said(RESERVE, &with));
    assert!(with.resumed && with.refilled, "{with:?}");
    assert!(
        with.write_back_gave + with.cleaner_gave > SLACK,
        "nothing that was dead came back: {with:?}"
    );
}

/// How much room is a little (section 5.3): the smallest reserve of a
/// ladder with which the session resumes and fills its reserve again, on
/// the database of these runs. Every reserve above it does too.
#[test]
fn how_much_room_the_engine_needs_to_give_room_back() {
    let mut least = None;
    for reserve in LADDER {
        let found = run(reserve, false);
        println!("{}", said(reserve, &found));
        if found.resumed && found.refilled {
            least.get_or_insert(reserve);
        }
        assert!(
            least.is_none() || (found.resumed && found.refilled),
            "a reserve of {reserve} bytes does not do what a smaller one did: {found:?}"
        );
    }
    let least = least.expect("no reserve of the ladder is enough");
    println!("the least reserve that is enough: {} KiB", least >> 10);
    assert!(least <= RESERVE, "{least} bytes");
}

/// The order in which the writer comes first (section 5.3): the session
/// is asked for a commit of 72 KiB as soon as it has released its
/// reserve, and that commit takes its share of the room before
/// write-back and the cleaner have had a turn. The reserve that section
/// 5.3 sizes for this database is enough in this order, and so is every
/// larger one of the ladder. A smaller one is enough or not by what the
/// commit leaves: what each came to is printed, and is the same in every
/// run.
#[test]
fn the_reserve_of_a_log_file_and_a_segment_is_enough_where_the_writer_comes_first() {
    for reserve in LADDER {
        let found = run(reserve, true);
        println!("the writer first, {}", said(reserve, &found));
        if reserve >= RESERVE {
            assert_eq!(found.first_taken, Some(true), "{found:?}");
            assert!(found.resumed && found.refilled, "{found:?}");
        }
    }
}

/// The sizes of a reserve that are tried, in bytes.
pub(super) const LADDER: [u64; 10] = [
    1 << 10,
    4 << 10,
    16 << 10,
    32 << 10,
    64 << 10,
    128 << 10,
    256 << 10,
    384 << 10,
    512 << 10,
    1024 << 10,
];
