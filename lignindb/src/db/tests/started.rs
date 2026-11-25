//! The first commit after an open, with write-back over a bound (PERF-07):
//! it makes no turn of write-back on its own thread, however late the
//! loop's thread comes to its first instruction.
//!
//! A committer that finds write-back over a bound while no loop is running
//! makes the turn itself (FL5, `WriteBack::wait_for_room`), and the loop
//! says that it runs on its own thread. The tests have that thread run
//! something of theirs before its loop.

use std::sync::mpsc;

use lignin_store::flush::FlushConfig;

use super::*;
use crate::db::BEFORE_THE_LOOP;

/// The thread of write-back is held before its loop, and the opening
/// thread commits as soon as it has its handle. An open that returned
/// while the thread is held would let that commit find pages owed over
/// the bound and no loop, and the commit would make the turn, with its
/// syncs. With the loop running, the commit waits for a turn of it or
/// finds that the loop has written what was owed, and syncs nothing
/// either way.
#[test]
fn the_first_commit_after_an_open_makes_no_turn_of_write_back() {
    let dir = temp();
    let vfs = Watched::default();
    // One page that is owed is over the bound.
    let flush = FlushConfig {
        max_unwritten: 0,
        idle: Duration::from_millis(5),
        ..FlushConfig::new(PageSize::MIN)
    };
    let options = Options {
        vfs: Arc::new(vfs.clone()),
        flush: Some(flush),
        ..options()
    };
    let path = dir.path().join("db");

    let (at_the_loop, reached) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (done, ended) = mpsc::channel();
    // The thread is left to itself where the test fails, so that a wait
    // that does not end fails the test and does not hang it.
    thread::spawn(move || {
        let held = move || {
            let _ = at_the_loop.send(());
            // Until the test lets go, or leaves.
            let _ = gone.recv();
        };
        BEFORE_THE_LOOP.with(|hook| *hook.borrow_mut() = Some(Box::new(held)));
        let db = Db::open(path, &options).expect("a new database");
        let me = thread::current().id();
        let before = vfs.syncs_by(me);
        // Values that are too long for their cells, so that a turn of
        // write-back has a value-log file to sync.
        plant(&db, numbered(1), Merk::sized(b'k', 6, 3_000));
        let synced = vfs.syncs_by(me) - before;
        let error = db.stats().background.last_error;
        db.close().expect("a close");
        let _ = done.send((synced, error));
    });
    let minute = Duration::from_secs(60);
    reached
        .recv_timeout(minute)
        .expect("the thread of write-back was started");
    // The thread of write-back is held before its loop. Where the open
    // has returned all the same, the commit comes now and finds no loop.
    let early = ended.recv_timeout(Duration::from_millis(300));
    drop(go);
    let (synced, error) = early
        .or_else(|_| ended.recv_timeout(3 * minute))
        .expect("the open, the commit and the close ended");
    assert_eq!(synced, 0, "the commit made a turn of write-back itself");
    assert_eq!(error, None);
}

/// A thread of write-back that ends before its loop is seen to run ends
/// the wait of the open, with an error.
#[test]
fn an_open_whose_loop_never_runs_is_refused() {
    let dir = temp();
    let (done, ended) = mpsc::channel();
    let path = dir.path().join("db");
    thread::spawn(move || {
        let ends = || panic!("the test has the thread of write-back end before its loop");
        BEFORE_THE_LOOP.with(|hook| *hook.borrow_mut() = Some(Box::new(ends)));
        let opened = Db::open(path, &options());
        let _ = done.send(opened.map(drop).map_err(|error| error.to_string()));
    });
    let opened = ended
        .recv_timeout(Duration::from_secs(180))
        .expect("the open ended");
    let error = opened.expect_err("an open with no loop of write-back");
    assert!(
        error.contains("the thread of write-back ended before its loop was seen to run"),
        "{error}"
    );
}
