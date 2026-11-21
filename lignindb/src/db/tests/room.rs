//! OPS-08: a device that fills gives room back through the reserve, and
//! not without it (`docs/lignindb/disk-full-and-backup.md`, DF10 and DF11;
//! [`crate::space`]).
//!
//! The simulated device has a capacity in bytes. A database with a
//! reserve is written on it, with write-back after every commit and a
//! cleaner that is behind, until a commit is refused for room. What is
//! left free is taken by somebody else, so that the device has no free
//! byte. Then the order of [`crate::space`] is gone through: nothing comes
//! back while nothing is released, and with the reserve released
//! write-back and the cleaner give back what is dead, the session takes
//! the next commit, and the reserve is filled again.
//!
//! The cleaner is not a part of `Db` yet. Here it is the store's, over the
//! parts of the open database, with the passes its loop would make.

use std::path::Path;

use lignin_format::PageSize;
use lignin_io::{EntryKind, OpenMode, Vfs};
use lignin_store::{
    cleaner::{BothOwners, Cleaner, CleanerConfig, CleanerError, CleanerState},
    flush::FlushConfig,
    wal::SyncPolicy,
};
use lignin_tree::{flat::FlatOwners, value::TreeOwners};

use super::{full::Session, *};
use crate::{
    backup::tests::world::{batch, first, height, key, value, State},
    space::{out_of_space, Reserve},
    txn::tests::model::OwnedBatch,
};

const DIR: &str = "/db";

/// The room the device has beyond what the database takes when it is
/// given its capacity.
const HEADROOM: u64 = 512 << 10;

/// What a refused operation may leave or take away on a device with no
/// free byte: less than two pages, of files that are made and removed
/// again.
const SLACK: u64 = 8 << 10;

/// A commit that writes every value of 24 keys again: what it wrote the
/// last time is dead from then on.
fn rewrite(state: &State) -> OwnedBatch {
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

/// The passes that the cleaner's loop makes in one turn: one over the
/// segments, one over the value log. Says how many segments and value-log
/// files went.
fn clean(db: &Db, state: &CleanerState) -> Result<usize, CleanerError> {
    let shared = &*db.shared;
    let store = &*shared.store;
    let cleaner = Cleaner::new(shared.write_back(), state).with_stats(&store.stats);
    let pass = cleaner.pass()?;
    let steps = shared.steps();
    let owners = BothOwners(
        TreeOwners::new(store.loader(), &store.pool, store.page_size, &steps),
        FlatOwners::new(store.loader(), &store.pool, store.config(), &steps),
    );
    let values = cleaner.pass_values(&owners)?;
    Ok(pass.dropped + values.dropped.len())
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
    /// The commits that were taken before the device was full.
    filled_by: u64,
    /// The bytes the files of the database took when it was full.
    full: u64,
    /// The bytes that write-back gave back once the reserve was released.
    write_back_gave: u64,
    /// The bytes that the cleaner gave back after it.
    cleaner_gave: u64,
    /// Whether the session took a commit then.
    resumed: bool,
    /// Whether the reserve could be filled again.
    refilled: bool,
}

/// One run with a reserve of `reserve` bytes.
fn run(reserve: u64) -> Found {
    let mut session = Session::new(41, SyncPolicy::Never, true);
    session.options.flush = Some(FlushConfig {
        checkpoint_log_bytes: 64 << 10,
        segment_len: 256 << 10,
        ..FlushConfig::new(PageSize::MIN)
    });
    session.options.log_file_len = Some(96 << 10);
    assert!(session.open());
    let cleaning = CleanerState::new(CleanerConfig {
        segment_len: 256 << 10,
        min_segment_len: 16 << 10,
        ..CleanerConfig::new()
    });
    let room = Reserve::of(session.options.vfs.clone(), Path::new(DIR));
    let other = Reserve::of(session.options.vfs.clone(), Path::new("/"));
    let used = |session: &Session| session.fs.used_bytes();

    session.write("w1", first, false);
    session.ticks("t1");
    room.fill(reserve).expect("room for the reserve");
    assert_eq!(room.held().expect("the reserve"), reserve);
    let capacity = used(&session) + HEADROOM;
    session.fs.set_capacity(Some(capacity));

    // The database is written until a commit is refused for room. The
    // cleaner is behind: what the rewrites leave dead stays.
    let mut filled_by = 0;
    loop {
        let before = session.told.latest;
        session.write("fill", rewrite, false);
        if session.told.latest == before {
            break;
        }
        filled_by += 1;
        session.ticks("t");
        assert!(filled_by < 400, "the device does not fill");
    }
    // Whatever is left free is somebody else's from here on.
    let free = capacity.saturating_sub(used(&session));
    other.fill(free).expect("the rest of the device");
    let full = used(&session);
    assert!(full >= capacity, "{full} of {capacity} bytes");
    let files = full - reserve - free;
    println!(
        "full with a reserve of {} KiB: {}",
        reserve >> 10,
        sizes(&session)
    );

    // DF10: with no free byte nothing comes back, whatever is tried, and
    // nothing is lost: the session reads on, and has not ended.
    for _ in 0..3 {
        session.ticks("full");
        let db = session.db.as_ref().expect("the session goes on");
        if let Err(error) = clean(db, &cleaning) {
            assert!(out_of_space(&error), "{error}");
        }
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

    // DF11, step 2: the reserve is released.
    assert_eq!(room.release().expect("a removal needs no room"), reserve);
    // Step 3: write-back's turns. Step 4: the cleaner's passes, each with
    // the turn of write-back that its loop is given.
    let before = used(&session);
    for _ in 0..4 {
        session.ticks("back");
    }
    let after_write_back = used(&session);
    for _ in 0..12 {
        let db = session.db.as_ref().expect("the session goes on");
        if let Err(error) = clean(db, &cleaning) {
            assert!(out_of_space(&error), "{error}");
        }
        session.ticks("cleaned");
    }
    let after_cleaner = used(&session);
    println!("after the cleaner: {}", sizes(&session));
    // Step 5: the session takes the next commit, if there is room for it.
    let db = session.db.as_ref().expect("the session goes on");
    assert_eq!(db.reopen(), None, "a full device that refused no sync");
    let published = session.told.latest;
    session.write("resumed", rewrite, false);
    let resumed = session.told.latest == published + 1;
    session.ticks("t");
    // Step 6: the reserve is filled again, if there is room for it.
    let refilled = resumed && reserve > 0 && room.fill(reserve).is_ok();
    assert!(!refilled || room.held().expect("the reserve") == reserve);

    session.check("at the end");
    session.check_cut("at the end");
    Found {
        filled_by,
        full: files,
        write_back_gave: before.saturating_sub(after_write_back),
        cleaner_gave: after_write_back.saturating_sub(after_cleaner),
        resumed,
        refilled,
    }
}

/// The reserve that the runs of these tests keep: one log file and one
/// segment of the cleaner, as the runs configure them, and a table
/// checkpoint and a manifest beside them
/// (`docs/lignindb/disk-full-and-backup.md`, section 5.3).
const RESERVE: u64 = (96 << 10) + (256 << 10) + (32 << 10);

/// One line of what a run found.
fn said(reserve: u64, found: &Found) -> String {
    format!(
        "a reserve of {:5} KiB: full after {} commits with {} KiB of files; once it was \
         released write-back gave back {} KiB and the cleaner {} KiB; the session {} the next \
         commit, and the reserve {} filled again",
        reserve >> 10,
        found.filled_by,
        found.full >> 10,
        found.write_back_gave >> 10,
        found.cleaner_gave >> 10,
        match found.resumed {
            true => "took",
            false => "was refused",
        },
        match found.refilled {
            true => "was",
            false => "was not",
        },
    )
}

#[test]
#[ignore = "a value-log file that write-back gave up is not closed until a reopen (D-209): STORE-06-F3 removes this"]
fn a_full_device_gives_room_back_through_the_reserve_and_not_without_it() {
    let without = run(0);
    println!("{}", said(0, &without));
    assert!(without.filled_by > 2, "{without:?}");
    assert!(
        without.write_back_gave + without.cleaner_gave < SLACK,
        "room came back with nothing released: {without:?}"
    );
    assert!(!without.resumed && !without.refilled, "{without:?}");

    let with = run(RESERVE);
    println!("{}", said(RESERVE, &with));
    assert!(with.resumed && with.refilled, "{with:?}");
    assert!(
        with.write_back_gave + with.cleaner_gave > SLACK,
        "nothing that was dead came back: {with:?}"
    );
}

/// How much room is a little (section 5.3): the smallest reserve of a
/// ladder with which the session resumes and the reserve is filled again,
/// on the database of these runs. Every reserve above it does too.
#[test]
#[ignore = "a value-log file that write-back gave up is not closed until a reopen (D-209): STORE-06-F3 removes this"]
fn how_much_room_the_engine_needs_to_give_room_back() {
    let ladder = [1u64, 4, 16, 32, 64, 128, 256, 384, 512, 1024].map(|kib| kib << 10);
    let mut least = None;
    for reserve in ladder {
        let found = run(reserve);
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
