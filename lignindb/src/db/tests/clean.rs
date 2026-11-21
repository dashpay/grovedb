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

/// A session begins with three Merks that an earlier session wrote
/// before it was closed: their pages are in segments that are sealed,
/// and the values of the two that have values out of line are in files
/// of the value log that are closed. The one with no value out of line
/// is dropped first, and one of the other two by the first of the
/// commits that follow: blocks of sealed segments and values of closed
/// files are dead whatever the threads of this session do next. The
/// commits enter a Merk with values out of line and
/// drop the one before, again and again. What the drops leave is
/// reclaimed, and the cleaner of the session, on its thread, gives files
/// back: segments, and files of the value log. Whether it moved something
/// out of a file first or found the file dead as a whole is the layout's,
/// which the threads make between them; what the cleaner moves is the
/// next test's. Nothing a reader is given changes, the close ends the
/// cleaner and checks the store, and the next session reads the same.
#[test]
fn the_cleaner_of_a_session_gives_back_what_drops_left_dead() {
    const ROUNDS: u32 = 120;
    let dir = temp();
    let cleaning = Options {
        cleaner: Some(policy()),
        ..options()
    };
    let long = || Merk::sized(b'k', 12, 1500);
    let db = Db::open(dir.path(), &cleaning).expect("a new database");
    plant(&db, subtree(1), long());
    Writes {
        enter: vec![(numbered(0), None), (subtree(8), None)],
        merks: vec![
            (numbered(0), long()),
            (subtree(8), Merk::sized(b'w', 120, 30)),
        ],
        ..Writes::default()
    }
    .commit(&db);
    // The close writes every page back and places every value.
    db.close().expect("the first session ends");
    let db = Db::open(dir.path(), &cleaning).expect("the database is opened again");
    Writes {
        drop: vec![subtree(8)],
        ..Writes::default()
    }
    .commit(&db);
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
        reclaimed.merks == u64::from(ROUNDS) + 1 && reclaimed.waiting == 0
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
    let (stays, goes) = half_dead(&db);

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

    whole_of(&db, stays, goes);
    drop(db);
    let db = Db::open(dir.path(), &options()).expect("the database is opened again");
    whole_of(&db, stays, goes);
    db.close().expect("the session ends");
}

/// The two Merks of a pair in the layout of [`half_dead`]: one of several
/// pages, and one with values out of line.
fn wide() -> Merk {
    Merk::sized(b'w', 120, 30)
}

fn long() -> Merk {
    Merk::sized(b'k', 12, 1500)
}

/// Makes the layout that the cleaner has something to move in, in a
/// database whose background work is at rest: two pairs of Merks in one
/// segment and one file of the value log, of which the second pair is
/// dropped and reclaimed. Returns the pair that stays and the pair that
/// went.
fn half_dead(db: &Db) -> ([Subtree; 2], [Subtree; 2]) {
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
    .commit(db);
    written_back(db);
    Writes {
        drop: goes.to_vec(),
        ..Writes::default()
    }
    .commit(db);
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
    written_back(db);
    (stays, goes)
}

/// A reader finds every key and value of the pair that stays in the
/// layout of [`half_dead`], and nothing of the pair that went.
fn whole_of(db: &Db, stays: [Subtree; 2], goes: [Subtree; 2]) {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert_eq!(items_of(&operation, stays[0]), Some(wide().items()));
    assert_eq!(items_of(&operation, stays[1]), Some(long().items()));
    for gone in goes {
        assert_eq!(items_of(&operation, gone), None);
    }
}

/// A database on a simulated device, with its background work at rest,
/// in which the cleaner has moved one block and has more to move: the
/// layout of [`half_dead`], and passes that end after the first block
/// they moved. The cleaner's segment is open. Returns the device, the
/// database, the pair that stays and the pair that went, and the path of
/// the cleaner's segment.
struct Moving {
    fs: lignin_io::sim::SimVfs,
    options: Options,
    db: Db,
    stays: [Subtree; 2],
    goes: [Subtree; 2],
    segment: String,
}

impl Moving {
    fn new(seed: u64) -> Self {
        use std::collections::BTreeSet;

        use lignin_io::sim::{Event, OpKind, SimConfig, SimVfs};

        let fs = SimVfs::new(SimConfig {
            trace_ops: true,
            ..SimConfig::new(seed)
        });
        let options = Options {
            vfs: Arc::new(fs.clone()),
            cleaner: Some(CleanerConfig {
                max_pass_bytes: 1,
                ..policy()
            }),
            ..options()
        };
        let db = open_at_rest("/db", &options).expect("a new database");
        let (stays, goes) = half_dead(&db);
        let segments = || -> BTreeSet<String> {
            let appended = fs.trace().into_iter().filter_map(|event| match event {
                Event::Op(op) if op.kind == OpKind::Append && op.path.contains("/seg/") => {
                    Some(op.path)
                }
                _ => None,
            });
            appended.collect()
        };
        let mut turns = 0;
        let segment = loop {
            let before = segments();
            let moved = crate::db::with_cleaner(&db.shared, |cleaner, _| cleaner.pass());
            let moved = moved.expect("a pass on a device with room");
            if moved.blocks_moved > 0 {
                assert!(moved.more, "the pass left blocks to move: {moved:?}");
                let after = segments();
                let mut taken = after.difference(&before).cloned();
                let (Some(segment), None) = (taken.next(), taken.next()) else {
                    panic!("the pass appended to one new segment: {before:?} {after:?}");
                };
                break segment;
            }
            written_back(&db);
            turns += 1;
            assert!(turns < 20, "{moved:?}\n{}", db.stats());
        };
        Self {
            fs,
            options,
            db,
            stays,
            goes,
            segment,
        }
    }

    /// From here on the device refuses the operations `ops` on the
    /// cleaner's segment, for room.
    fn refuse_the_segment(&self, ops: lignin_io::sim::OpSet) {
        use lignin_io::sim::{Fault, FaultRule};

        let rule = FaultRule::new(Fault::Enospc).on(ops);
        self.fs.add_rule(rule.at(&self.segment).forever());
    }

    /// Closes the session as the close of a session does: the handle,
    /// whose background work was at rest, is given threads to end.
    fn close(
        self,
    ) -> (
        Result<(), crate::db::DbError>,
        Options,
        [Subtree; 2],
        [Subtree; 2],
    ) {
        let ended = || thread::spawn(|| {});
        *crate::db::store::lock(&self.db.threads) = Some(crate::db::Threads {
            write_back: ended(),
            reclaim: ended(),
            cleaner: ended(),
        });
        (self.db.close(), self.options, self.stays, self.goes)
    }
}

/// A sync of the cleaner's segment that the device refused ends the
/// session, as a sync of write-back's segment does
/// (`docs/lignindb/disk-full-and-backup.md`, DF8). The cleaner's thread
/// is at rest and each pass is made here as that thread makes it. The
/// device refuses every write and every sync of the cleaner's segment,
/// for room: the next pass is refused its append, and the sync of what
/// the segment holds. The device has room again right after.
///
/// The copies in that segment cannot be made durable any more, and links
/// point to them, so no table checkpoint is installed until the store is
/// opened again. The session says that it has ended to whoever asks next,
/// before write-back or a close has met the segment. Nothing that was
/// published is lost: readers read on, and the next session has it all.
#[test]
fn a_sync_of_a_segment_that_the_cleaner_was_refused_ends_the_session() {
    use lignin_io::sim::OpSet;
    use lignin_store::{cleaner::CleanerError, flush::FlushError};

    use crate::{
        db::{DbError, Reopen},
        space::out_of_space,
    };

    let moving = Moving::new(47);
    moving.refuse_the_segment(OpSet::WRITES | OpSet::SYNCS);
    let db = &moving.db;
    let refused = crate::db::with_cleaner(&db.shared, |cleaner, _| cleaner.pass());
    let refused = refused.expect_err("the append and the sync are refused");
    assert!(
        matches!(refused, CleanerError::SegmentNotDurable { .. }),
        "{refused}"
    );
    assert!(out_of_space(&refused), "{refused}");
    moving.fs.clear_rules();

    assert_eq!(db.reopen(), Some(Reopen::SegmentsNotDurable));
    assert_eq!(db.stats().reopen, Some(Reopen::SegmentsNotDurable));
    // With room, a table checkpoint is answered with what stays failed.
    let again = db.shared.write_back().checkpoint();
    let again = again.expect_err("no table checkpoint is installed");
    assert!(matches!(again, FlushError::SegmentsNotDurable), "{again}");
    whole_of(db, moving.stays, moving.goes);
    let flushed = db.flush().expect("the log is synced");
    assert_eq!(flushed.version, db.version());

    // The close says the same, as the close of a session that
    // write-back's segment ended does.
    let (closed, options, stays, goes) = moving.close();
    let closed = closed.expect_err("the close of a session that has ended");
    assert!(
        matches!(closed, DbError::Flush(FlushError::SegmentsNotDurable)),
        "{closed}"
    );
    let db = open_at_rest("/db", &options).expect("the database is opened again");
    assert_eq!(db.reopen(), None);
    whole_of(&db, stays, goes);
}

/// The device fills while nothing of a session is being written, and
/// the close is the first to meet it: the cleaner seals the segment it
/// was moving blocks into, and the device refuses that. On one device
/// here it refuses every write and every sync of the segment, so the
/// sealing is refused its append and then the sync of what the segment
/// holds. On the other it refuses the syncs alone, so the sealing's own
/// sync is the one that fails. Write-back, which closes its files after the cleaner,
/// then installs no table checkpoint and says only that. The session had
/// not ended when the close began, so the close fails for room (DF8): it
/// answers with the cleaner's error, which has the device's refusal as
/// its cause. The next session has everything that was published.
#[test]
fn a_close_that_cannot_make_the_cleaners_segment_durable_fails_for_room() {
    use lignin_io::sim::OpSet;
    use lignin_store::cleaner::CleanerError;

    use crate::{db::DbError, space::out_of_space};

    for refused in [OpSet::WRITES | OpSet::SYNCS, OpSet::SYNCS] {
        let moving = Moving::new(48);
        moving.refuse_the_segment(refused);
        assert_eq!(moving.db.reopen(), None);
        let fs = moving.fs.clone();
        let (closed, options, stays, goes) = moving.close();
        let closed = closed.expect_err("the cleaner's segment is not sealed");
        assert!(
            matches!(
                closed,
                DbError::Clean(CleanerError::SegmentNotDurable { .. })
            ),
            "{refused:?}: {closed}"
        );
        assert!(out_of_space(&closed), "{refused:?}: {closed}");
        fs.clear_rules();
        let db = open_at_rest("/db", &options).expect("the database is opened again");
        assert_eq!(db.reopen(), None);
        whole_of(&db, stays, goes);
    }
}
