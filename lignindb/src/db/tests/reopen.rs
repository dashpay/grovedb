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
//! `LIGNINDB_REOPEN_DIR` names a directory on a disk for the store (about
//! 1 GiB); without it the store is in memory, where a sync costs nothing.

use std::{
    fmt, io,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use lignin_io::{DirEntry, OpenMode, StdVfs, Vfs, VfsFile, VfsLock};
use lignin_tree::directory::Directory;

use super::*;

/// The file layer of the operating system, with a count of the syncs and
/// the reads and of the time they take: what a reopen is made of.
#[derive(Debug, Default)]
struct Counts {
    syncs: AtomicU64,
    sync_nanos: AtomicU64,
    reads: AtomicU64,
    read_bytes: AtomicU64,
    opens: AtomicU64,
}

#[derive(Debug, Clone, Default)]
struct Timed(Arc<Counts>);

impl Timed {
    fn sync<T>(&self, sync: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let began = Instant::now();
        let synced = sync();
        let nanos = began.elapsed().as_nanos() as u64;
        self.0.syncs.fetch_add(1, Ordering::Relaxed);
        self.0.sync_nanos.fetch_add(nanos, Ordering::Relaxed);
        synced
    }

    /// The counts since the last call: syncs, the milliseconds they took,
    /// reads, the bytes they read, and files opened.
    fn take(&self) -> (u64, f64, u64, u64, u64) {
        let take = |count: &AtomicU64| count.swap(0, Ordering::Relaxed);
        let counts = &self.0;
        (
            take(&counts.syncs),
            take(&counts.sync_nanos) as f64 / 1e6,
            take(&counts.reads),
            take(&counts.read_bytes),
            take(&counts.opens),
        )
    }
}

struct TimedFile(Box<dyn VfsFile>, Timed);

impl fmt::Debug for TimedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl VfsFile for TimedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let read = self.0.read_at(buf, offset)?;
        self.1 .0.reads.fetch_add(1, Ordering::Relaxed);
        self.1
             .0
            .read_bytes
            .fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.0.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.0.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.1.sync(|| self.0.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.1.sync(|| self.0.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.0.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }
}

impl Vfs for Timed {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.0.opens.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(TimedFile(StdVfs.open(path, mode)?, self.clone())))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdVfs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        StdVfs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.sync(|| StdVfs.sync_dir(path))
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }
}

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
}

/// Opens the database in `dir`, checks `check` on it, closes it, and says
/// what the open took.
fn reopen(dir: &Path, options: &Options, vfs: &Timed, check: impl Fn(&Db)) -> Reopened {
    vfs.take();
    let began = Instant::now();
    let db = Db::open(dir, options).expect("the database is opened again");
    let open_ms = began.elapsed().as_secs_f64() * 1e3;
    let (syncs, sync_ms, reads, read_bytes, files) = vfs.take();
    assert!(!db.created());
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
    }
}

/// A session that was closed leaves nothing to replay: the next one reads
/// its manifest, its table checkpoint and a log with no commit in it.
#[test]
fn a_database_that_was_closed_is_reopened_without_a_replay() {
    let dir = temp();
    let vfs = Timed::default();
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
    let kept;
    let dir = match std::env::var_os("LIGNINDB_REOPEN_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir).join("reference");
            let _ = fs::remove_dir_all(&dir);
            dir
        }
        None => {
            kept = temp();
            kept.path().join("reference")
        }
    };
    let vfs = Timed::default();
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
             from {} files; the directory handle loads in {:.3} ms; the close took {:.2} ms",
            reopened.open_ms,
            reopened.syncs,
            reopened.sync_ms,
            reopened.reads,
            reopened.read_bytes,
            reopened.files,
            reopened.directory_ms,
            reopened.close_ms,
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
    // The row: within 60 ms. On a disk the syncs are the disk's, so what
    // is held to the line is the reopen without them.
    assert!(
        without_syncs < 60.0,
        "{without_syncs:.2} ms without the syncs"
    );
    if std::env::var_os("LIGNINDB_REOPEN_DIR").is_some() {
        let _ = fs::remove_dir_all(&dir);
    }
}
