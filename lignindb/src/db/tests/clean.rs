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

/// Waits until the cleaner's counts are as `done` says, and says what
/// they are if that takes more than a minute.
fn cleaned(db: &Db, what: &str, done: impl Fn(&CleanerStats) -> bool) {
    let began = Instant::now();
    loop {
        let stats = db.stats();
        if done(&stats.cleaner) {
            return;
        }
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "{what}: {:?}\n{stats}",
            stats.cleaner
        );
        std::thread::sleep(Duration::from_millis(5));
    }
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
/// cleaner of the session moves what is live out of the files they are
/// in and gives the files back: segments, and files of the value log.
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
        cleaner.blocks_moved > 0 && cleaner.segments_dropped > 0
    });
    cleaned(&db, "files of the value log are given back", |cleaner| {
        cleaner.values_moved > 0 && cleaner.value_files_dropped > 0
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
