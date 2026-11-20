//! The cleaner, started by the engine (D-142): its policy comes from the
//! options, it runs on a thread of the session beside the commits, and a
//! close ends it and closes its files.

use lignin_store::cleaner::{CleanerConfig, CleanerStats};

use super::*;
use crate::db::Background;

/// A cleaner for a store of these tests: a file is closed for it once it
/// holds 4 KiB, and it sleeps 5 ms between two passes.
fn policy() -> CleanerConfig {
    CleanerConfig {
        min_segment_len: 4 << 10,
        idle: Duration::from_millis(5),
        ..CleanerConfig::new()
    }
}

/// Waits until the cleaner's counts are as `done` says.
fn cleaned(db: &Db, what: &str, done: impl Fn(&CleanerStats) -> bool) {
    stats_until(db, what, |stats| &stats.cleaner, done);
}

/// The policy that the options give is the session's, and with none given
/// it is the store's default.
#[test]
fn the_policy_of_the_cleaner_comes_from_the_options() {
    let dir = temp();
    let given = Options {
        cleaner: Some(policy()),
        ..options()
    };
    let db = Db::open(dir.path(), &given).expect("a new database");
    assert_eq!(db.shared.store.cleaner.config(), policy());
    db.close().expect("the session ends");
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    assert_eq!(db.shared.store.cleaner.config(), CleanerConfig::new());
}

/// Commits enter a Merk with values out of line and drop the one before,
/// again and again, beside a Merk that stays. What the drops leave is
/// reclaimed, the blocks and values of freed pages are dead, and the
/// cleaner of the session, on its thread, gives files back: segments, and
/// files of the value log. Whether it moved something out of a file
/// first or found the file dead as a whole is the layout's, which the
/// threads make between them; what the cleaner moves is the next test's.
/// Nothing a reader is given changes, the close ends the cleaner and
/// checks the store, and the next session reads the same.
#[test]
fn the_cleaner_of_a_session_gives_back_what_drops_left_dead() {
    const ROUNDS: u32 = 120;
    let dir = temp();
    let cleaning = Options {
        cleaner: Some(policy()),
        ..options()
    };
    let db = Db::open(dir.path(), &cleaning).expect("a new database");
    let long = || Merk::sized(b'k', 12, 1500);
    plant(&db, subtree(1), long());
    plant(&db, numbered(0), long());
    for round in 1..=ROUNDS {
        Writes {
            drop: vec![numbered(round - 1)],
            enter: vec![(numbered(round), None)],
            merks: vec![(numbered(round), long())],
            ..Writes::default()
        }
        .commit(&db);
    }
    reclaimed(&db, "every drop is reclaimed", |reclaimed| {
        reclaimed.merks == u64::from(ROUNDS) && reclaimed.waiting == 0
    });
    cleaned(&db, "segments are given back", |cleaner| {
        cleaner.segments_dropped > 0
    });
    cleaned(&db, "files of the value log are given back", |cleaner| {
        cleaner.value_files_dropped > 0
    });
    let stats = db.stats();
    println!("{:?}\n{stats}", stats.cleaner);
    assert_eq!(stats.background, Background::default());
    assert_eq!(stats.cleaner.errors, 0);
    assert!(stats.to_string().contains("cleaner: "));

    let whole = |db: &Db| {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        for kept in [subtree(1), numbered(ROUNDS)] {
            assert_eq!(seen(&operation, kept, b"k"), Some((12, vec![])));
        }
        assert_eq!(seen(&operation, numbered(ROUNDS - 1), b"k"), None);
    };
    whole(&db);
    db.close().expect("the session ends");
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    whole(&db);
}

/// Turns of write-back on the calling thread, with the view its own
/// thread has, until no page version and no value waits.
fn written_back(db: &Db) {
    crate::db::with_page_values(&db.shared, |write_back| {
        for _ in 0..50 {
            write_back.tick().expect("a turn of write-back");
            let watch = db.stats().store.watch;
            if watch.unwritten == 0 && watch.pending_value_bytes == 0 {
                return;
            }
        }
        panic!("write-back stands still: {}", db.stats());
    });
}

/// The cleaner moves what lives beside what is dead, in a layout that is
/// made for it, with the background threads ended and each turn made
/// here as the cleaner's thread makes it. One commit writes two pairs
/// of Merks, each a Merk of several pages and one with values out of
/// line, and write-back puts the pages of all four into one segment and
/// the values of both pairs into one file of the value log. One pair is
/// dropped and reclaimed: half of what those files hold is dead and half
/// lives. The cleaner moves the blocks and the values that live, and the
/// pointers of the Merk that stays are rewritten by the owners the engine
/// gives it. A reader finds every key and value of the pair that stays as
/// it was written, in this session and in the next, which recovers a
/// session that ended with no close.
#[test]
fn the_cleaner_moves_what_lives_beside_what_a_drop_left_dead() {
    let dir = temp();
    let cleaning = Options {
        cleaner: Some(policy()),
        ..options()
    };
    let db = Db::open(dir.path(), &cleaning).expect("a new database");
    halt_background(&db);
    let wide = || Merk::sized(b'w', 120, 30);
    let long = || Merk::sized(b'k', 12, 1500);
    let (stays, goes) = ([subtree(1), subtree(3)], [subtree(2), subtree(4)]);
    Writes {
        enter: (stays.iter().chain(&goes))
            .map(|subtree| (*subtree, None))
            .collect(),
        merks: vec![
            (stays[0], wide()),
            (stays[1], long()),
            (goes[0], wide()),
            (goes[1], long()),
        ],
        ..Writes::default()
    }
    .commit(&db);
    written_back(&db);
    Writes {
        drop: goes.to_vec(),
        ..Writes::default()
    }
    .commit(&db);
    for _ in 0..1_000 {
        if db.reclaim().waiting == 0 {
            break;
        }
    }
    let stats = db.stats();
    assert_eq!(
        (stats.reclaimed.merks, stats.reclaimed.waiting),
        (2, 0),
        "{stats}"
    );
    written_back(&db);

    let mut turns = 0;
    loop {
        crate::db::with_cleaner(&db.shared, |cleaner, owners| {
            cleaner.pass().expect("a pass over the segments");
            let values = cleaner.pass_values(owners);
            values.expect("a pass over the value log");
        });
        written_back(&db);
        let cleaner = db.stats().cleaner;
        if cleaner.blocks_moved > 0 && cleaner.values_moved > 0 {
            break;
        }
        turns += 1;
        assert!(turns < 20, "{cleaner:?}\n{}", db.stats());
    }
    let stats = db.stats();
    println!("{:?}\n{stats}", stats.cleaner);
    assert_eq!(stats.cleaner.errors, 0, "{stats}");
    assert_eq!(stats.background, Background::default());

    let whole = |db: &Db| {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        assert_eq!(items_of(&operation, stays[0]), Some(wide().items()));
        assert_eq!(items_of(&operation, stays[1]), Some(long().items()));
        for gone in goes {
            assert_eq!(items_of(&operation, gone), None);
        }
    };
    whole(&db);
    drop(db);
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    whole(&db);
    db.close().expect("the session ends");
}
