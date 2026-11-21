//! The reopen of a database (the row's "reopen of the reference database
//! within 60 ms"): what a session that was closed leaves for the next one
//! to do, and how long that takes.
//!
//! `the_reopen_of_a_store_of_the_reference_size` is the measurement. It is
//! left out of a normal run and wants a release build:
//!
//! ```text
//! cargo test --release -p lignindb --lib -- --ignored --nocapture the_reopen_of_a_store
//! ```
//!
//! `LIGNINDB_REOPEN_DIR` names a directory for the store (about 1 GiB);
//! without it the store is in the temporary directory. On a file system in
//! memory a sync costs nothing, and on a disk the syncs are most of the
//! time: the test says how many there were and how long they took.

use std::{path::Path, sync::Arc, time::Instant};

use lignin_tree::directory::Directory;

use super::*;

/// What one reopen took, and what it was made of.
#[derive(Debug)]
struct Reopened {
    open_ms: f64,
    syncs: u64,
    sync_ms: f64,
    reads: u64,
    read_bytes: u64,
    files: u64,
    directory_ms: f64,
    close_ms: f64,
    /// The parts of the open, as the engine timed them.
    opening: crate::db::Opening,
    /// The syncs of the open, in order, by what was synced and its path
    /// in the database directory.
    synced: Vec<String>,
}

/// Opens the database in `dir`, checks `check` on it, closes it, and says
/// what the open took.
fn reopen(dir: &Path, options: &Options, vfs: &Watched, check: impl Fn(&Db)) -> Reopened {
    vfs.take();
    vfs.take_synced();
    let began = Instant::now();
    let db = Db::open(dir, options).expect("the database is opened again");
    let open_ms = began.elapsed().as_secs_f64() * 1e3;
    let (reads, read_bytes, files) = vfs.take();
    // The syncs of the open itself: those of this thread, which made it.
    // A turn of the threads it started is not the open's.
    let synced = vfs.take_synced();
    let syncs = synced.len() as u64;
    let sync_ms = (synced.iter().map(|(.., took)| took.as_secs_f64())).sum::<f64>() * 1e3;
    assert!(!db.created());
    let opening = db.stats().opening;
    let recovery = db.stats().recovery;
    assert_eq!(
        recovery.commits, 0,
        "a session that was closed left a commit to replay"
    );
    assert_eq!(recovery.pages_rebuilt, 0);
    check(&db);
    // The handle on the directory, loaded once more as the open loads it.
    let store = &*db.shared.store;
    let began = Instant::now();
    {
        let guard = db.shared.versions.guard();
        let pages = store.committed(&guard, guard.latest());
        let tree = store.catalog().directory;
        Directory::load(&pages, tree.root(), tree.config()).expect("the directory is loaded");
    }
    let directory_ms = began.elapsed().as_secs_f64() * 1e3;
    let began = Instant::now();
    db.close().expect("the session ends");
    Reopened {
        open_ms,
        syncs,
        sync_ms,
        reads,
        read_bytes,
        files,
        directory_ms,
        close_ms: began.elapsed().as_secs_f64() * 1e3,
        opening,
        synced: synced
            .iter()
            .map(|(what, path, took)| {
                let path = path.strip_prefix(dir).unwrap_or(path);
                let ms = took.as_secs_f64() * 1e3;
                format!("{what:?} {} ({ms:.2} ms)", path.display())
            })
            .collect(),
    }
}

/// A session that was closed leaves nothing to replay: the next one reads
/// its manifest, its table checkpoint and a log with no commit in it.
#[test]
fn a_database_that_was_closed_is_reopened_without_a_replay() {
    let dir = temp();
    let vfs = Watched::default();
    let options = Options {
        vfs: Arc::new(vfs.clone()),
        ..options()
    };
    let db = Db::open(dir.path(), &options).expect("a new database");
    for name in 0..40 {
        plant(&db, numbered(name), Merk::sized(b'k', 100, 400));
    }
    db.close().expect("the session ends");
    for _ in 0..3 {
        let reopened = reopen(dir.path(), &options, &vfs, |db| {
            let snapshot = db.snapshot();
            assert_eq!(
                seen(&snapshot.operation(), numbered(39), b"k"),
                Some((100, vec![]))
            );
        });
        println!("{reopened:?}");
        assert!(reopened.syncs > 0 && reopened.files > 0);
        assert!(!reopened.synced.is_empty());
    }
}

/// The measurement of the row: a store of the reference load's size, built
/// through commits of the facade, closed, and opened again.
///
/// The reference workload is one million documents of 400 bytes with one
/// reference each, in a grove of 20,007 Merks and 2,020,007 nodes
/// (TREE-10's reference load). GroveDB does not run on the engine yet, so
/// the store here has that many nodes in that many Merks, written as
/// whole Merks: 2,000 of 500 documents with values of 441 bytes, out of
/// line, and 18,007 of 56 or 57 references of 40 bytes.
#[test]
#[ignore = "builds a store of one million documents; run it in release mode"]
fn the_reopen_of_a_store_of_the_reference_size() {
    const DOCUMENT_MERKS: u32 = 2_000;
    const DOCUMENTS: u32 = 500;
    const INDEX_MERKS: u32 = 18_007;
    // A directory of this run's own, in the one that is named or in the
    // temporary directory: it goes with `kept`, and nothing else does.
    let kept = match std::env::var_os("LIGNINDB_REOPEN_DIR") {
        Some(parent) => tempfile::tempdir_in(parent).expect("a directory for the store"),
        None => temp(),
    };
    let dir = kept.path().join("reference");
    let vfs = Watched::default();
    let options = Options {
        vfs: Arc::new(vfs.clone()),
        ..Options::default()
    };
    let began = Instant::now();
    let db = Db::open(&dir, &options).expect("a new database");
    let mut nodes = 0u64;
    // A block is four Merks of documents and 36 of references, as one
    // commit: 500 commits and seven more for the last references.
    let mut index = 0;
    let mut block = 0;
    while block * 4 < DOCUMENT_MERKS || index < INDEX_MERKS {
        let mut writes = Writes::default();
        for merk in (block * 4..(block * 4 + 4)).filter(|merk| *merk < DOCUMENT_MERKS) {
            let subtree = numbered(merk);
            writes.enter.push((subtree, None));
            let documents = Merk::sized(b'd', DOCUMENTS, 441);
            nodes += documents.len() as u64;
            writes.merks.push((subtree, documents));
        }
        for _ in 0..36 {
            if index == INDEX_MERKS {
                break;
            }
            let subtree = numbered(1_000_000 + index);
            writes.enter.push((subtree, None));
            // 1,020,007 nodes in 18,007 Merks.
            let references = Merk::sized(b'r', 56 + u32::from(index < 11_615), 40);
            nodes += references.len() as u64;
            writes.merks.push((subtree, references));
            index += 1;
        }
        writes.commit(&db);
        block += 1;
    }
    assert_eq!(nodes, 2_020_007);
    let loaded = began.elapsed();
    let stats = db.stats();
    println!(
        "built {nodes} nodes in {} Merks by {} commits in {loaded:.1?}: {} bytes logged, \
         {} bytes written back",
        DOCUMENT_MERKS + INDEX_MERKS,
        stats.commits.published,
        stats.store.log_bytes,
        stats.store.flush_bytes,
    );
    let began = Instant::now();
    db.close().expect("the session ends");
    println!("the first close took {:.1?}", began.elapsed());
    let size = |name: &str| -> (usize, u64) {
        let files = fs::read_dir(dir.join(name)).expect("a directory");
        let lens = files.map(|file| file.expect("a file").metadata().expect("its length").len());
        let lens: Vec<u64> = lens.collect();
        (lens.len(), lens.iter().sum())
    };
    for name in ["wal", "seg", "val", "table"] {
        let (files, bytes) = size(name);
        println!("{name}: {files} files, {bytes} bytes");
    }

    let check = |db: &Db| {
        let snapshot = db.snapshot();
        let operation = snapshot.operation();
        let last = numbered(DOCUMENT_MERKS - 1);
        assert_eq!(
            seen(&operation, last, b"k"),
            Some((DOCUMENTS as usize, vec![]))
        );
        let last = numbered(1_000_000 + INDEX_MERKS - 1);
        assert_eq!(seen(&operation, last, b"k"), Some((56, vec![])));
    };
    let mut opens = Vec::new();
    for round in 0..10 {
        let reopened = reopen(&dir, &options, &vfs, check);
        println!(
            "reopen {round}: {:.2} ms, of which {} syncs took {:.2} ms; {} reads of {} bytes \
             from {} files; the directory handle loads in {:.3} ms; the close took {:.2} ms\n  {}",
            reopened.open_ms,
            reopened.syncs,
            reopened.sync_ms,
            reopened.reads,
            reopened.read_bytes,
            reopened.files,
            reopened.directory_ms,
            reopened.close_ms,
            reopened.opening,
        );
        opens.push(reopened);
    }
    opens.sort_by(|a, b| a.open_ms.total_cmp(&b.open_ms));
    let median = &opens[opens.len() / 2];
    let without_syncs = median.open_ms - median.sync_ms;
    println!(
        "median reopen {:.2} ms ({:.2} ms without its {} syncs), fastest {:.2} ms, slowest \
         {:.2} ms",
        median.open_ms,
        without_syncs,
        median.syncs,
        opens[0].open_ms,
        opens[opens.len() - 1].open_ms,
    );
    println!("the syncs of the median reopen, in order:");
    for sync in &median.synced {
        println!("  {sync}");
    }
    // What the recovery's time is made of, as far as an option says: the
    // same reopen with a sixteenth of the buffer pool.
    let small = Options {
        cache_size: options.cache_size / 16,
        ..options.clone()
    };
    let mut with_small: Vec<f64> = (0..5)
        .map(|_| reopen(&dir, &small, &vfs, check).open_ms)
        .collect();
    with_small.sort_by(f64::total_cmp);
    println!(
        "with a pool of {} MiB and not {}: median {:.2} ms",
        small.cache_size >> 20,
        options.cache_size >> 20,
        with_small[2]
    );
    // The same reopen with nothing between the engine and the operating
    // system's files. A list of syncs reaches the file layer as one list
    // only this way (`Vfs::sync_together`), and where a sync ends in a
    // flush of the whole device that is what a reopen waits for.
    let plain = Options {
        vfs: Arc::new(StdVfs),
        ..options.clone()
    };
    let mut direct: Vec<f64> = (0..10)
        .map(|_| {
            let began = Instant::now();
            let db = Db::open(&dir, &plain).expect("the database is opened again");
            let took = began.elapsed().as_secs_f64() * 1e3;
            check(&db);
            db.close().expect("the session ends");
            took
        })
        .collect();
    direct.sort_by(f64::total_cmp);
    println!(
        "with no file layer over the operating system's: median {:.2} ms, fastest {:.2} ms, \
         slowest {:.2} ms",
        direct[direct.len() / 2],
        direct[0],
        direct[direct.len() - 1]
    );
    // The row: a reopen within 60 ms, all of it. The syncs of a reopen
    // are listed above, so on a disk whose sync takes several milliseconds
    // this measures the disk: the parts above say which it is.
    assert!(
        median.open_ms < 60.0,
        "the median reopen took {:.2} ms, {without_syncs:.2} ms of it not in a sync",
        median.open_ms
    );
}
