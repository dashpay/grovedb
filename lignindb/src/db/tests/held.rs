//! Commits and reads beside background work that is held in a sync
//! (PERF-07): whichever file the background work is making durable, and
//! for however long that takes, a commit under the default sync policy is
//! made and published, and a reader reads.
//!
//! Each test holds every sync of one kind, from the first that is asked
//! for until the test lets them go, so the thread that asked is inside
//! that sync, with the locks it takes for it, for as long as the commits
//! of the test take.
//!
//! The commits are few and small, and write-back stays within its bounds
//! under them, which each test asserts: a committer that finds write-back
//! over its bounds waits for a turn of it (FL5), and that turn can be the
//! one that is inside the sync.

use std::sync::mpsc;

use lignin_store::flush::FlushConfig;

use super::*;

/// The commits that are made while the sync is held.
const COMMITS: u32 = 6;

/// The nodes of the Merk that each commit writes.
const NODES: u32 = 6;

/// Lets the held syncs go when the test leaves, also by a panic.
struct Released<'a>(&'a Watched);

impl Drop for Released<'_> {
    fn drop(&mut self) {
        self.0.release_syncs();
    }
}

/// One commit: a new Merk whose values are too long for their cells, so
/// that write-back has values to place, pages to write and log to cover.
fn write(db: &Db, number: u32) -> Subtree {
    let subtree = numbered(number);
    plant(db, subtree, Merk::sized(b'k', NODES, 3_000));
    subtree
}

/// Commits until a sync of `of` is held, then makes [`COMMITS`] more and
/// reads what they wrote, with the sync still held.
fn commits_are_made_while_a_sync_is_held(of: SyncOf) {
    let dir = temp();
    let vfs = Watched::default();
    // A table checkpoint is due at a turn of write-back whenever a byte
    // was logged since the last one, so that its syncs come with the
    // first commits.
    let flush = FlushConfig {
        checkpoint_log_bytes: 1,
        idle: Duration::from_millis(5),
        ..FlushConfig::new(PageSize::MIN)
    };
    let options = Options {
        vfs: Arc::new(vfs.clone()),
        flush: Some(flush),
        ..options()
    };
    let db = Db::open(dir.path().join("db"), &options).expect("a new database");

    // Every commit of the test is made on a thread of its own, so that a
    // commit that waited for the sync would fail the test and not hang
    // it: the syncs are let go when this thread leaves.
    let (read, synced, before) = thread::scope(|threads| {
        let _release = Released(&vfs);
        vfs.hold_syncs(of);
        let (done, ended) = mpsc::channel();
        let (db, vfs) = (&db, &vfs);
        threads.spawn(move || {
            let me = thread::current().id();
            let mut number = 0;
            let mut next = || {
                number += 1;
                number
            };
            let began = Instant::now();
            while vfs.syncs_held() == 0 {
                assert!(
                    began.elapsed() < Duration::from_secs(60),
                    "the background work made no sync of {of:?}"
                );
                write(db, next());
                let waited = Instant::now();
                while vfs.syncs_held() == 0 && waited.elapsed() < Duration::from_millis(100) {
                    thread::sleep(Duration::from_millis(1));
                }
            }

            // Somebody is inside a sync of that kind now, and stays there.
            let before = db.version();
            let mut read = Vec::new();
            for _ in 0..COMMITS {
                let subtree = write(db, next());
                let snapshot = db.snapshot();
                let items = items_of(&snapshot.operation(), subtree);
                read.push(items.map_or(0, |items| items.len()));
            }
            let _ = done.send((read, vfs.syncs_by(me), before));
        });
        let ended = ended.recv_timeout(Duration::from_secs(180));
        ended.unwrap_or_else(|_| panic!("a commit waited for a sync of {of:?}"))
    });
    assert!(vfs.syncs_held() > 0);
    assert_eq!(db.version().get(), before.get() + u64::from(COMMITS));
    assert_eq!(
        read,
        vec![NODES as usize; COMMITS as usize],
        "each commit is read"
    );
    assert_eq!(synced, 0, "a commit made a sync");
    let stats = db.stats();
    assert_eq!(
        stats.store.flush.room_waits, 0,
        "write-back was over a bound"
    );
    assert_eq!(stats.background.last_error, None);
    db.close().expect("a close");
}

#[test]
fn commits_are_made_while_a_value_log_file_is_synced() {
    commits_are_made_while_a_sync_is_held(SyncOf::FileIn("val"));
}

#[test]
fn commits_are_made_while_a_segment_is_synced() {
    commits_are_made_while_a_sync_is_held(SyncOf::FileIn("seg"));
}

#[test]
fn commits_are_made_while_the_log_is_synced() {
    commits_are_made_while_a_sync_is_held(SyncOf::FileIn("wal"));
}

#[test]
fn commits_are_made_while_a_table_checkpoint_is_synced() {
    commits_are_made_while_a_sync_is_held(SyncOf::FileIn("table"));
}

#[test]
fn commits_are_made_while_a_manifest_is_synced() {
    commits_are_made_while_a_sync_is_held(SyncOf::Manifest);
}

#[test]
fn commits_are_made_while_the_names_of_a_directory_are_synced() {
    commits_are_made_while_a_sync_is_held(SyncOf::Names);
}
