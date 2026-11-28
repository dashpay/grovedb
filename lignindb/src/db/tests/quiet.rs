//! What a session leaves alone once its open has returned (GROVE-08-F1,
//! D-300, D-301).
//!
//! A caller may remove the directory of a database under its handle
//! (P-18), and a file that a thread of the session makes there meanwhile
//! can make that removal fail. Two things are done about it, and each
//! session of these tests begins with no commit asked of it:
//!
//! - What the open's recovery replayed is owed to the segments. The open
//!   makes turns of write-back for it itself, on its own thread, before
//!   it starts the threads of the background work. What those turns do
//!   not write is left to write-back's thread.
//! - The cleaner's thread and the reclaimer's make no turn until the
//!   session is asked for its first commit. What an earlier session left
//!   them stays until then, or until a caller asks for it (`Db::clean`,
//!   `Db::reclaim`).

use std::sync::atomic::Ordering::Relaxed;

use super::{
    full::Mode,
    reserve::{device, held, DIR, KEPT},
    *,
};
use lignin_io::sim::{Fault, FaultRule, OpKind};
use lignin_store::flush::FlushConfig;

use crate::{
    checkpoint::CheckpointOptions,
    commit::CommitError,
    db::{DbError, Reopen, WriteError},
    space::{out_of_space, Reserve},
};

/// A checkpoint at `target` of a new database at `live`, both on `vfs`,
/// that holds everything it holds in its log: its source has none of its
/// threads, so it has written nothing back. One of its Merks has values
/// of `value_len` bytes, and one that it had was dropped and not
/// reclaimed.
fn unwritten_checkpoint(vfs: Arc<dyn Vfs>, live: &Path, target: &Path, value_len: usize) {
    let options = Options { vfs, ..options() };
    let live = open_at_rest(live, &options).expect("a new database");
    plant(&live, subtree(1), Merk::sized(b'k', 10, value_len));
    plant(&live, subtree(2), Merk::sized(b'l', 10, 20));
    plant(&live, subtree(3), Merk::sized(b'm', 40, 20));
    let writes = Writes {
        drop: vec![subtree(3)],
        ..Writes::default()
    };
    writes.commit(&live);
    live.checkpoint(target, CheckpointOptions::default())
        .unwrap_or_else(|error| panic!("the checkpoint: {error}"));
}

/// What the reclaimer has done in a session that has only been told of
/// the one Merk that an earlier session dropped.
fn waiting() -> ReclaimStats {
    ReclaimStats {
        waiting: 1,
        ..ReclaimStats::default()
    }
}

/// Waits until the cleaner's thread and the reclaimer's have both come
/// to wait for the first commit of the session, and until write-back's
/// loop has ended two turns from now on. Each is a state that the thread
/// reaches by itself.
fn at_rest(db: &Db) {
    let store = &db.shared.store;
    wait_for("the two threads that wait for a commit", || {
        store.signal.waited.load(Relaxed) == 2
    });
    let turns = store.flush.turns();
    wait_for("two turns of write-back's loop", || {
        store.flush.turns() >= turns + 2
    });
}

/// The open of a checkpoint that was never opened writes back what its
/// recovery replayed before it returns, on the thread that opens: values
/// that are kept in their cells, and values that go to the value log.
/// The session is then asked for no commit. Its cleaner and its reclaimer wait,
/// write-back's loop makes two turns, and no thread of the session has
/// written, removed, renamed, made or synced anything since the open
/// returned: the directory is byte for byte what the open left, with the
/// Merk that its source dropped not reclaimed.
///
/// Then the directory is removed under the handle, at the first attempt,
/// and the handle closes, by `close` or by dropping it, with nothing made
/// again.
#[test]
fn a_session_that_is_asked_for_no_commit_writes_nothing_after_its_open() {
    for (value_len, explicit) in [(20, true), (4000, false)] {
        let dir = temp();
        let target = dir.path().join("checkpoint");
        let os: Arc<dyn Vfs> = Arc::new(StdVfs);
        unwritten_checkpoint(os, &dir.path().join("live"), &target, value_len);

        let watched = Watched::default();
        let options = Options {
            vfs: Arc::new(watched.clone()),
            ..options()
        };
        let opened = Db::open(&target, &options).unwrap_or_else(|error| panic!("{error}"));
        // Every change up to here, by every thread, and those of this
        // thread among them: all of them.
        let (changes, here) = (watched.changes(), watched.changes_here());
        let left = image(&target);
        let store = &opened.shared.store;
        let stats = opened.stats();
        assert!(stats.recovery.commits >= 4, "{:?}", stats.recovery);
        assert_eq!(store.pool.oldest_unwritten(), None, "pages are owed");
        let written = store.flush.stats();
        assert!(written.base_blocks > 0, "{written:?}");
        assert_eq!(written.values_placed > 0, value_len > 20, "{written:?}");
        assert_eq!(stats.background.write_back_errors, 0);

        at_rest(&opened);
        assert_eq!(
            (watched.changes(), watched.changes_here()),
            (changes, here),
            "something was changed after the open returned"
        );
        assert_eq!(changes, here, "a thread of the background work wrote");
        assert_eq!(image(&target), left);
        let stats = opened.stats();
        assert_eq!(
            (stats.cleaner.passes, stats.cleaner.value_passes),
            (0, 0),
            "the cleaner made a pass"
        );
        assert_eq!(store.signal.rounds.load(Relaxed), 0, "the reclaimer");
        assert_eq!(stats.reclaimed, waiting(), "{:?}", stats.reclaimed);

        fs::remove_dir_all(&target).expect("the directory is removed");
        match explicit {
            true => {
                let error = opened.close().expect_err("nothing could be made durable");
                assert!(matches!(&error, DbError::Removed { dir } if *dir == target));
            }
            false => drop(opened),
        }
        assert!(!target.exists(), "the directory was made again");
    }
}

/// What the two threads wait with is done when it is asked for, and when
/// the session is asked for its first commit. A call of `Db::reclaim` and
/// one of `Db::clean` each make their turn on the calling thread, in a
/// session with no commit, and the threads go on waiting. After a commit
/// both threads make their turns: the Merk that the earlier session
/// dropped is reclaimed, and the cleaner passes over the segments and the
/// value log. So they do after a commit that the engine refused, and
/// after a wipe. The mark that lets the threads go is not set by the two
/// calls, and is set by each of the three commits.
#[test]
fn the_cleaner_and_the_reclaimer_begin_at_the_first_commit_or_when_asked() {
    for way in [
        "the calls",
        "a commit",
        "a commit that is refused",
        "a wipe",
    ] {
        let asked = way == "the calls";
        let dir = temp();
        let target = dir.path().join("checkpoint");
        let os: Arc<dyn Vfs> = Arc::new(StdVfs);
        unwritten_checkpoint(os, &dir.path().join("live"), &target, 4000);
        let opened = Db::open(&target, &options()).unwrap_or_else(|error| panic!("{error}"));
        let store = &opened.shared.store;
        at_rest(&opened);
        assert_eq!(opened.stats().reclaimed, waiting());
        assert!(!store.signal.was_asked(), "{way}: before anything is asked");
        if asked {
            while opened.reclaim().more {}
            assert!(!store.signal.was_asked(), "a round let the threads go");
            let (segments, _) = opened.clean().expect("a turn of the cleaner");
            assert!(!store.signal.was_asked(), "a turn let the threads go");
            assert!(segments.scanned, "{segments:?}");
            let stats = opened.stats();
            assert_eq!(stats.reclaimed.merks, 1, "{:?}", stats.reclaimed);
            assert_eq!((stats.cleaner.passes, stats.cleaner.value_passes), (1, 1));
            assert_eq!(store.signal.rounds.load(Relaxed), 0, "the reclaimer");
        } else {
            if way == "a commit" {
                plant(&opened, subtree(4), Merk::sized(b'n', 4, 20));
            } else if way == "a wipe" {
                opened.wipe().expect("the wipe");
            } else {
                // A record of a subtree that was never entered.
                let unentered = Writes {
                    records: vec![(Column::Aux, subtree(9), b"a".to_vec(), b"x".to_vec())],
                    ..Writes::default()
                };
                let refused = unentered.with(|batch| opened.write(batch, &mut NoStaleHashes));
                assert!(
                    matches!(&refused, Err(WriteError::Commit(CommitError::Refused(_)))),
                    "{refused:?}"
                );
            }
            assert!(store.signal.was_asked(), "{way}");
            // The wipe drops two Merks more.
            reclaimed(&opened, "the Merk that was dropped", |reclaimed| {
                reclaimed.merks >= 1 && (way == "a wipe" || reclaimed.merks == 1)
            });
            stats_until(
                &opened,
                "a turn of the cleaner",
                |stats| &stats.cleaner,
                |cleaner| cleaner.passes >= 1 && cleaner.value_passes >= 1,
            );
            assert!(store.signal.rounds.load(Relaxed) >= 1);
        }
        assert_eq!(opened.stats().background, Default::default(), "{way}");
        opened.close().expect("the session ends");
    }
}

/// `options` with a loop of write-back that leaves no page for a later
/// turn (`FlushConfig::max_deferred`): for the tests that wait for the
/// loop to have written what an open left.
fn leaving_no_page(mut options: Options) -> Options {
    let size = options.page_size.unwrap_or(PageSize::DEFAULT);
    let flush = options.flush.unwrap_or(FlushConfig::new(size));
    options.flush = Some(FlushConfig {
        max_deferred: 0,
        ..flush
    });
    options
}

/// A turn of the open that fails does not fail the open, and the thread
/// that opens makes no turn after it. The first file that the session
/// opens to write a segment is refused, once: it is the opening thread
/// that is refused, before the open returns, and the failed turn is
/// counted once. What that turn did not write is owed still, and
/// write-back's thread writes it: the second segment file is asked for by
/// another thread than the one that opened, and no third is. The session
/// takes writes.
#[test]
fn a_turn_of_the_open_that_fails_leaves_its_work_to_the_loop() {
    let dir = temp();
    let target = dir.path().join("checkpoint");
    let os: Arc<dyn Vfs> = Arc::new(StdVfs);
    unwritten_checkpoint(os, &dir.path().join("live"), &target, 20);
    let watched = Watched::default();
    watched.refuse_the_next_segment_of(&target);
    let options = leaving_no_page(Options {
        vfs: Arc::new(watched.clone()),
        ..options()
    });
    let opened = Db::open(&target, &options).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(opened.reopen(), None::<Reopen>);
    let store = &opened.shared.store;
    wait_for("the loop writes what the open could not", || {
        store.pool.oldest_unwritten().is_none()
    });
    let here = thread::current().id();
    let asked = watched.segments_asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], (here, true), "the open's turn was not refused");
    assert!(asked[1].0 != here && !asked[1].1, "{asked:?}");
    let background = opened.stats().background;
    assert_eq!(background.write_back_errors, 1, "{background:?}");
    plant(&opened, subtree(5), Merk::sized(b'o', 4, 20));
    opened.close().expect("the session ends");
}

/// A turn of the open that takes a table checkpoint and nothing else can
/// fill the log's file with the record of that checkpoint, and the open
/// then makes the turn that takes the log to its next file. The
/// checkpoint here is of a source whose table checkpoint covers all its
/// pages and whose log holds one commit after it that changed no page, a
/// delete of a key that is not there. It is opened with a table
/// checkpoint due after one logged byte and a log file that is full with
/// its first item. When the open returns no turn of the log is due, and
/// from then until the test closes the session, which is asked for no
/// commit, no thread of it changes anything.
#[test]
fn the_open_turns_the_log_that_its_checkpoint_filled() {
    let dir = temp();
    let live = open_at_rest(dir.path().join("live"), &options()).expect("a new database");
    plant(&live, subtree(1), Merk::sized(b'k', 10, 20));
    let covering = CheckpointOptions {
        catch_up_above: Some(0),
    };
    live.checkpoint(&dir.path().join("covered"), covering)
        .unwrap_or_else(|error| panic!("the checkpoint that covers: {error}"));
    let absent = [RecordWrite::delete(Column::Aux, subtree(1), b"absent")];
    let batch = Batch {
        records: &absent,
        ..Batch::default()
    };
    live.write(&batch, &mut NoStaleHashes)
        .unwrap_or_else(|error| panic!("the delete: {error}"));
    let target = dir.path().join("checkpoint");
    let as_it_is = CheckpointOptions {
        catch_up_above: None,
    };
    live.checkpoint(&target, as_it_is)
        .unwrap_or_else(|error| panic!("the checkpoint: {error}"));

    let watched = Watched::default();
    let options = Options {
        vfs: Arc::new(watched.clone()),
        flush: Some(FlushConfig {
            checkpoint_log_bytes: 1,
            ..FlushConfig::new(PageSize::MIN)
        }),
        log_file_len: Some(1),
        // No sync of the log by an interval in the time that is watched.
        sync: lignin_store::wal::SyncPolicy::Never,
        ..options()
    };
    let opened = Db::open(&target, &options).unwrap_or_else(|error| panic!("{error}"));
    let (changes, here) = (watched.changes(), watched.changes_here());
    let store = &opened.shared.store;
    let written = store.flush.stats();
    assert_eq!(written.base_blocks + written.delta_blocks, 0, "{written:?}");
    assert_eq!(written.checkpoints, 1, "{written:?}");
    assert!(!store.log.is_due(), "a turn of the log is due");
    assert_eq!(opened.stats().background.write_back_errors, 0);
    at_rest(&opened);
    assert_eq!(
        (watched.changes(), watched.changes_here()),
        (changes, here),
        "something was changed after the open returned"
    );
    assert_eq!(changes, here, "a thread of the background work wrote");
    opened.close().expect("the session ends");
}

/// Every entry below `dir` on `vfs`, by its path.
fn entries(vfs: &dyn Vfs, dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in vfs.read_dir(dir).expect("a listing") {
        let path = dir.join(&entry.name);
        if entry.kind == lignin_io::EntryKind::Dir {
            found.extend(entries(vfs, &path));
        }
        found.push(path);
    }
    found.sort();
    found
}

/// The release of a reserve starts neither thread (D-309): the gate
/// wins. A session that keeps a reserve is opened on what an earlier
/// session left in its log, with a Merk that was dropped and not
/// reclaimed, and is asked for no commit. A call that a full device
/// refuses, a flush or the creation of a checkpoint, releases the
/// reserve, and the reclaimer's thread is woken as the thread of a
/// session that runs is. Then the device has room again. The mark that
/// the wait of the two threads looks at is not set; write-back's loop
/// ends two turns, and no pass of the cleaner and no round of the
/// reclaimer has been made; the reserve is not filled; and the directory
/// has no entry that it did not have before the call, and the reserve's
/// file no more.
///
/// A commit that is asked afterwards, whatever it is answered, starts
/// both threads.
#[test]
fn the_release_of_a_reserve_starts_no_cleaning_where_no_commit_was_asked() {
    for (seed, call) in [(91, "a flush"), (92, "the creation of a checkpoint")] {
        let (fs, options) = device(seed, KEPT);
        let dir = Path::new(DIR);
        {
            // It ends as a process that is killed: nothing is written
            // back, and nothing of the drop is reclaimed.
            let earlier = open_at_rest(dir, &options).expect("a new database");
            plant(&earlier, subtree(1), Merk::sized(b'k', 10, 4000));
            plant(&earlier, subtree(3), Merk::sized(b'm', 40, 20));
            let writes = Writes {
                drop: vec![subtree(3)],
                ..Writes::default()
            };
            writes.commit(&earlier);
        }
        let db = Db::open(dir, &options).unwrap_or_else(|error| panic!("{call}: {error}"));
        let store = &db.shared.store;
        at_rest(&db);
        assert!(db.stats().reserve.held, "{call}");
        assert_eq!(held(&fs), KEPT, "{call}");
        let before = entries(&fs, dir);
        let woken = store.signal.wakes.load(Relaxed);

        let full = match call {
            "a flush" => Mode::Full,
            _ => Mode::FullButSyncs,
        };
        fs.add_rule(full.rule(0).torn_append(false));
        let refused: Box<dyn std::error::Error> = match call {
            "a flush" => Box::new(db.flush().expect_err("a flush")),
            _ => {
                let made = db.checkpoint(Path::new("/checkpoint"), CheckpointOptions::default());
                Box::new(made.expect_err("a checkpoint"))
            }
        };
        assert!(out_of_space(&*refused), "{call}: {refused}");
        fs.clear_rules();

        let reserve = db.stats().reserve;
        assert_eq!(
            (reserve.held, reserve.releases, reserve.bytes_released),
            (false, 1, KEPT),
            "{call}: {reserve:?}"
        );
        assert_eq!(held(&fs), 0, "{call}");
        assert!(store.signal.wakes.load(Relaxed) > woken, "{call}: not told");
        assert!(!store.signal.was_asked(), "{call}: the release let them go");
        at_rest(&db);
        let stats = db.stats();
        assert_eq!(
            (stats.cleaner.passes, stats.cleaner.value_passes),
            (0, 0),
            "{call}: the cleaner made a pass"
        );
        assert_eq!(store.signal.rounds.load(Relaxed), 0, "{call}");
        assert_eq!(stats.reclaimed, waiting(), "{call}");
        assert_eq!(
            (stats.reserve.held, stats.reserve.fills),
            (false, 1),
            "{call}: {:?}",
            stats.reserve
        );
        let after = entries(&fs, dir);
        let gained: Vec<_> = after.iter().filter(|path| !before.contains(path)).collect();
        assert!(gained.is_empty(), "{call}: {gained:?}");
        assert!(!after.contains(&dir.join(Reserve::FILE_NAME)), "{call}");

        // Whatever this is answered: the session of the flush has ended
        // for writes.
        let asked = Writes {
            records: vec![(Column::Aux, subtree(1), b"a".to_vec(), b"x".to_vec())],
            ..Writes::default()
        };
        let _ = asked.with(|batch| db.write(batch, &mut NoStaleHashes));
        assert!(store.signal.was_asked(), "{call}");
        wait_for("a round of the reclaimer and a pass of the cleaner", || {
            store.signal.rounds.load(Relaxed) >= 1 && db.stats().cleaner.passes >= 1
        });
    }
}

/// The open makes its turns of write-back before it fills the reserve
/// (DF9), so a turn of the open that the device refuses for room
/// releases no reserve. The directory has the reserve of an earlier
/// session, which ended as a process that is killed, and the first
/// segment file that this open asks for is refused for room, once. The
/// open succeeds. The session was told of one refusal for room and of one
/// failed turn of write-back, it has released nothing, and it holds its
/// reserve, which is whole on the device.
#[test]
fn a_turn_of_the_open_that_is_refused_for_room_releases_no_reserve() {
    let (fs, options) = device(93, KEPT);
    let options = leaving_no_page(options);
    let dir = Path::new(DIR);
    {
        let earlier = open_at_rest(dir, &options).expect("a new database");
        plant(&earlier, subtree(1), Merk::sized(b'k', 10, 20));
    }
    assert_eq!(held(&fs), KEPT);
    fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .on(OpKind::Open)
            .at(dir.join("seg/00000001.seg"))
            .times(1),
    );
    let db = Db::open(dir, &options).unwrap_or_else(|error| panic!("{error}"));
    let stats = db.stats();
    assert_eq!(
        stats.background.write_back_errors, 1,
        "{:?}",
        stats.background
    );
    let reserve = stats.reserve;
    assert_eq!(
        (reserve.refusals, reserve.releases, reserve.held),
        (1, 0, true),
        "{reserve:?}"
    );
    assert_eq!(held(&fs), KEPT);
    let store = &db.shared.store;
    wait_for("the loop writes what the open could not", || {
        store.pool.oldest_unwritten().is_none()
    });
    assert_eq!(db.stats().reserve.releases, 0);
}
