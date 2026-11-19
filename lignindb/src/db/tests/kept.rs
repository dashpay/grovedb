//! The handles on the subtree directory that the engine keeps
//! ([`Directories`], D-103): every reader has the handle of its own
//! version, from before the version is published until no reader can ask
//! for it.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use lignin_format::CommitVersion;
use lignin_store::alloc::Pins;

use super::*;
use crate::snapshot::{Directories, LatestLease, Versions};

/// The order a commit keeps: the handle of its version is in the engine's
/// set before the version is published. A reader that begins between the
/// two pins the version before, and is given the handle of that version;
/// a reader that begins after the publication has the new one, with the
/// first reader's unchanged.
#[test]
fn a_reader_that_begins_between_the_handle_and_the_publication_has_its_own_version() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    plant(&db, subtree(1), Merk::sized(b'k', 20, 20));
    let store = &*db.shared.store;
    // Two versions of one database, and the handle the engine kept for
    // each: with the subtree in use, and with it dropped.
    let with = db.snapshot();
    let before = with.version();
    let writes = Writes {
        drop: vec![subtree(1)],
        ..Writes::default()
    };
    let after = writes.commit(&db).version;
    let without = db.snapshot();
    let handle = |version| store.directories.at(version).expect("a handle is kept");
    let (old, new) = (handle(before), handle(after));
    let opens = |directory: &lignin_tree::directory::Directory, at: CommitVersion| {
        let guard = db.shared.versions.guard();
        let pages = store.committed(&guard, at);
        let entry = directory
            .open(&pages, subtree(1).as_bytes())
            .expect("a lookup");
        entry.is_some()
    };
    assert!(opens(&old, before) && !opens(&new, after));

    // The same two steps, with a reader between them.
    let versions = Versions::new(before);
    let kept = Directories::new(before, old);
    kept.set(after, new);
    let between = versions.pin();
    assert_eq!(
        between.version(),
        before,
        "the next version is not published"
    );
    let taken = kept
        .at(between.version())
        .expect("the handle of its version");
    assert!(
        opens(&taken, between.version()),
        "a handle of the version before"
    );
    // An operation that pins nothing reads the latest version under its
    // guard, and has that version's handle as well.
    let guard = versions.guard();
    assert!(opens(
        &kept.at(guard.latest()).expect("a handle"),
        guard.latest()
    ));

    versions.publish(after).expect("the next version");
    let later = versions.pin();
    assert_eq!(later.version(), after);
    assert!(!opens(&kept.at(later.version()).expect("a handle"), after));
    // The first reader's version has its handle for as long as it is
    // pinned, and the operation's for as long as its guard is held.
    assert_eq!(kept.reclaim(&versions), 0);
    assert!(opens(
        &kept.at(between.version()).expect("still kept"),
        before
    ));
    drop(between);
    assert_eq!(kept.reclaim(&versions), 0, "the guard is from before");
    assert_eq!(kept.len(), 2);
    drop(guard);
    assert_eq!(kept.reclaim(&versions), 1);
    assert!(kept.at(before).is_none() && kept.at(after).is_some());
    assert_eq!(versions.oldest_pin(), after);
    drop((with, without));
}

/// A handle goes by the three questions of a reclaimer: not while a pin
/// is on its version or an older one, and not while a guard is held that
/// was taken before that was found.
#[test]
fn a_kept_handle_goes_when_no_pin_and_no_guard_can_ask_for_it() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    let version = |raw| CommitVersion::new(raw).expect("a version");
    let handle = (db.shared.store.directories.at(db.version())).expect("a handle is kept");
    let versions = Versions::new(version(1));
    let kept = Directories::new(version(1), handle.clone());
    for raw in 2..=5 {
        kept.set(version(raw), handle.clone());
        versions.publish(version(raw)).expect("the next version");
    }
    assert_eq!(kept.len(), 5);
    // A guard that is held when the handles are found unpinned keeps them
    // all, and the next reclaim after it is gone takes them.
    let guard = versions.guard();
    assert_eq!(kept.reclaim(&versions), 0);
    drop(guard);
    assert_eq!(kept.reclaim(&versions), 4);
    assert!(kept.at(version(4)).is_none() && kept.at(version(5)).is_some());
    assert_eq!(
        kept.reclaim(&versions),
        0,
        "the handle of the latest version stays"
    );

    // A pin keeps its version's handle and every later one.
    let pin = versions.pin();
    for raw in 6..=8 {
        kept.set(version(raw), handle.clone());
        versions.publish(version(raw)).expect("the next version");
    }
    assert_eq!(kept.reclaim(&versions), 0);
    assert_eq!((kept.len(), pin.version()), (4, version(5)));
    drop(pin);
    assert_eq!(kept.reclaim(&versions), 3);

    // A handle that is set for a version that has one replaces it, and
    // what was kept for a later version goes: a commit that was not
    // published left it.
    kept.set(version(9), handle.clone());
    kept.set(version(8), handle);
    assert_eq!(kept.len(), 1);
    assert!(kept.at(version(9)).is_none() && !kept.is_empty());
    assert!(format!("{kept:?}").contains("newest"));
}

/// Readers beside commits that each enter one subtree and drop the one
/// before it, with the reclaimer taking out what the drops left: at every
/// version exactly one of the subtrees is in use, and every reader finds
/// that one and neither of its neighbours, whenever it first opens a
/// subtree. A reader with the handle of another version would find its
/// own subtree dropped, or the next one entered.
#[test]
fn readers_beside_commits_that_drop_have_the_handle_of_their_version() {
    const ROUNDS: u32 = 300;
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    // Version 2 + n has the subtree of round n.
    let first = Writes {
        enter: vec![(numbered(0), None)],
        records: vec![(Column::Aux, numbered(0), b"k".to_vec(), vec![0])],
        ..Writes::default()
    };
    let base = first.commit(&db).version.get();
    let done = AtomicBool::new(false);
    thread::scope(|threads| {
        let _ending = Ending(&done);
        let (db, done) = (&db, &done);
        let check = move |operation: &Operation<'_>, late: bool| {
            let round = (operation.at().get() - base) as u32;
            if late {
                // Commits land, and the reclaimer works, before the reader
                // first opens a subtree.
                thread::sleep(Duration::from_micros(300));
            }
            let found = |round: u32| seen(operation, numbered(round), b"k");
            let own = found(round);
            assert!(
                own.is_some(),
                "round {round}: its own subtree is not opened"
            );
            if round > 0 {
                assert_eq!(found(round - 1), None, "round {round}: the one before");
            }
            assert_eq!(found(round + 1), None, "round {round}: the one after");
        };
        let readers: Vec<_> = (0..4u32)
            .map(|reader| {
                threads.spawn(move || {
                    let mut lease = LatestLease::new();
                    let mut looks = 0u64;
                    while !done.load(Ordering::Acquire) {
                        let late = looks % 3 == 0;
                        match reader % 2 {
                            0 => check(&db.snapshot().operation(), late),
                            _ => check(&db.read(&mut lease), late),
                        }
                        looks += 1;
                    }
                    looks
                })
            })
            .collect();
        for round in 1..=ROUNDS {
            let writes = Writes {
                enter: vec![(numbered(round), None)],
                drop: vec![numbered(round - 1)],
                records: vec![(Column::Aux, numbered(round), b"k".to_vec(), vec![1])],
                ..Writes::default()
            };
            assert_eq!(writes.commit(db).version.get(), base + u64::from(round));
        }
        done.store(true, Ordering::Release);
        for reader in readers {
            assert!(reader.join().expect("a reader ends") > 0);
        }
    });
    wait_for("every drop is reclaimed", || {
        db.stats().reclaimed.merks == u64::from(ROUNDS)
    });
    // With every reader gone, one handle is left.
    wait_for("the handles of older versions go", || {
        db.reclaim();
        db.stats().directories == 1
    });
    assert_eq!(db.stats().background.reclaim_errors, 0);
}
