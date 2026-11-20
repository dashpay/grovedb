//! Tests of the facade, through its public calls: databases in a temporary
//! directory, and on the simulated file system where a test cuts the power
//! or fails a file operation.

mod kept;
mod open;
mod reclaim;
mod reopen;
mod wipe;
mod writes;

use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use lignin_io::{DirEntry, OpenMode, StdVfs, Vfs, VfsFile, VfsLock};

use lignin_format::{
    tree_page::{Hash, Side},
    CellAddress, PageSize,
};
use lignin_store::load::LoadBuffers;
use lignin_tree::hash::Stale;

use crate::{
    commit::{Committed, NoStaleHashes, Published},
    db::ReclaimStats,
    snapshot::Operation,
    txn::{
        Batch, Column, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, Subtree,
        SubtreeWrite, ValuePut,
    },
    Db, Options,
};

/// What a [`Watched`] file layer saw.
#[derive(Debug, Default)]
pub(super) struct Counts {
    syncs: AtomicU64,
    sync_nanos: AtomicU64,
    reads: AtomicU64,
    read_bytes: AtomicU64,
    opens: AtomicU64,
    /// Calls that change a file or a directory, or make one durable.
    changes: AtomicU64,
    /// The directories that were synced, as the caller named them.
    synced_dirs: Mutex<Vec<PathBuf>>,
    /// The directory that paths with no root are in, if there is one.
    root: Option<PathBuf>,
}

/// The file layer of the operating system, with a count of what is asked
/// of it: the syncs and the time they take, the reads, and every call
/// that changes something.
#[derive(Debug, Clone, Default)]
pub(super) struct Watched(Arc<Counts>);

impl Watched {
    /// A file layer whose relative paths are in `root`, as they would be
    /// for a process whose working directory that is.
    pub(super) fn rooted(root: &Path) -> Self {
        Self(Arc::new(Counts {
            root: Some(root.to_path_buf()),
            ..Counts::default()
        }))
    }

    fn at(&self, path: &Path) -> PathBuf {
        match &self.0.root {
            Some(root) if path.is_relative() => root.join(path),
            _ => path.to_path_buf(),
        }
    }

    fn changed(&self) {
        self.0.changes.fetch_add(1, Ordering::Relaxed);
    }

    fn sync<T>(&self, sync: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.changed();
        let began = Instant::now();
        let synced = sync();
        let nanos = began.elapsed().as_nanos() as u64;
        self.0.syncs.fetch_add(1, Ordering::Relaxed);
        self.0.sync_nanos.fetch_add(nanos, Ordering::Relaxed);
        synced
    }

    /// The counts since the last call: syncs, the milliseconds they took,
    /// reads, the bytes they read, and files opened.
    pub(super) fn take(&self) -> (u64, f64, u64, u64, u64) {
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

    /// How many calls so far wrote, removed, renamed, made or synced
    /// something, or opened a file to write it.
    pub(super) fn changes(&self) -> u64 {
        self.0.changes.load(Ordering::Relaxed)
    }

    /// The directories that were synced so far, as the caller named them.
    pub(super) fn synced_dirs(&self) -> Vec<PathBuf> {
        self.0.synced_dirs.lock().expect("the list").clone()
    }
}

struct WatchedFile {
    file: Box<dyn VfsFile>,
    watched: Watched,
}

impl fmt::Debug for WatchedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.file.fmt(f)
    }
}

impl VfsFile for WatchedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let read = self.file.read_at(buf, offset)?;
        let counts = &self.watched.0;
        counts.reads.fetch_add(1, Ordering::Relaxed);
        counts.read_bytes.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.watched.changed();
        self.file.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.watched.changed();
        self.file.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.watched.sync(|| self.file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.watched.sync(|| self.file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.watched.changed();
        self.file.set_len(len)
    }
}

impl Vfs for Watched {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.0.opens.fetch_add(1, Ordering::Relaxed);
        if !matches!(mode, OpenMode::Read) {
            self.changed();
        }
        let file = StdVfs.open(&self.at(path), mode)?;
        Ok(Box::new(WatchedFile {
            file,
            watched: self.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.changed();
        StdVfs.rename(&self.at(from), &self.at(to))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.changed();
        StdVfs.hard_link(&self.at(original), &self.at(link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.changed();
        StdVfs.remove_file(&self.at(path))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.changed();
        StdVfs.create_dir(&self.at(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.changed();
        StdVfs.remove_dir(&self.at(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(&self.at(path))
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let mut synced = self.0.synced_dirs.lock().expect("the list");
        synced.push(path.to_path_buf());
        drop(synced);
        self.sync(|| StdVfs.sync_dir(&self.at(path)))
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(&self.at(path))
    }
}

/// The tree type the Merks of these tests are entered with.
pub(super) const TREE_TYPE: &[u8] = b"merk";

/// The subtree whose 32 bytes are all `name`.
pub(super) fn subtree(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

/// The subtree whose bytes are `number`, for tests that enter many.
pub(super) fn numbered(number: u32) -> Subtree {
    let mut prefix = [0xa5; 32];
    prefix[..4].copy_from_slice(&number.to_be_bytes());
    Subtree::new(prefix)
}

/// Options for a small database: pages of 8 KiB and a pool of 16 MiB.
pub(super) fn options() -> Options {
    Options {
        page_size: Some(PageSize::MIN),
        cache_size: 16 << 20,
        idle: Duration::from_millis(20),
        ..Options::default()
    }
}

/// A directory for a database, in the temporary directory: `TMPDIR` says
/// where that is.
pub(super) fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a directory")
}

/// Every file below `dir` with its bytes, and every directory, by its path
/// from `dir`: what "byte for byte as it was" is compared by.
pub(super) fn image(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(at).expect("a directory") {
            let path = entry.expect("an entry").path();
            let name = path
                .strip_prefix(root)
                .expect("below the root")
                .to_path_buf();
            if path.is_dir() {
                out.insert(name, None);
                walk(root, &path, out);
            } else {
                out.insert(name, Some(fs::read(&path).expect("a file")));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// One node of a Merk of these tests, as it is stated to the engine.
#[derive(Debug, Clone)]
struct Node {
    key: Vec<u8>,
    value: Vec<u8>,
    feature: Vec<u8>,
    left: Option<Link>,
    right: Option<Link>,
    hosted: Vec<Subtree>,
}

#[derive(Debug, Clone)]
struct Link {
    key: Vec<u8>,
    heights: (u8, u8),
    aggregate: Vec<u8>,
}

/// A whole Merk, stated node by node: a balanced search tree of its keys.
/// The engine knows no Merk rule, so the hashes are any bytes.
#[derive(Debug, Clone, Default)]
pub(super) struct Merk {
    nodes: Vec<Node>,
    root: Option<Vec<u8>>,
}

const HASH: Hash = [0x5a; 32];

impl Merk {
    /// The Merk of `items`, by key.
    pub(super) fn of(items: &BTreeMap<Vec<u8>, Vec<u8>>) -> Self {
        fn build(items: &[(&Vec<u8>, &Vec<u8>)], out: &mut Vec<Node>) -> Option<(Link, u8)> {
            if items.is_empty() {
                return None;
            }
            let top = items.len() / 2;
            let (key, value) = items[top];
            let left = build(&items[..top], out);
            let right = build(&items[top + 1..], out);
            let heights = (
                left.as_ref().map_or(0, |(_, height)| *height),
                right.as_ref().map_or(0, |(_, height)| *height),
            );
            let mut feature = vec![3];
            feature.extend_from_slice(&(items.len() as u64).to_le_bytes());
            out.push(Node {
                key: key.clone(),
                value: value.clone(),
                feature: feature.clone(),
                left: left.map(|(link, _)| link),
                right: right.map(|(link, _)| link),
                hosted: Vec::new(),
            });
            let link = Link {
                key: key.clone(),
                heights,
                aggregate: feature,
            };
            Some((link, heights.0.max(heights.1) + 1))
        }
        let items: Vec<_> = items.iter().collect();
        let mut nodes = Vec::new();
        let root = build(&items, &mut nodes).map(|(link, _)| link.key);
        Self { nodes, root }
    }

    /// A Merk of `count` nodes whose keys begin with `lead` and whose
    /// values are `value_len` bytes long.
    pub(super) fn sized(lead: u8, count: u32, value_len: usize) -> Self {
        let items = (0..count).map(|number| {
            let mut key = vec![lead];
            key.extend_from_slice(&number.to_be_bytes());
            (key, vec![lead ^ number as u8; value_len])
        });
        Self::of(&items.collect())
    }

    /// The same Merk, whose node under `key` hosts `hosted`.
    pub(super) fn hosting(mut self, key: &[u8], hosted: Subtree) -> Self {
        let node = self.nodes.iter_mut().find(|node| node.key == key);
        node.expect("the host is a node of the Merk")
            .hosted
            .push(hosted);
        self
    }

    pub(super) fn len(&self) -> usize {
        self.nodes.len()
    }
}

/// A subtree to enter, with the subtree and the key of the node that
/// hosts it, if one does.
pub(super) type Entered = (Subtree, Option<(Subtree, Vec<u8>)>);

/// What a reader finds of a subtree: the nodes of its Merk, and the record
/// under one key in each column that has one.
pub(super) type Seen = (usize, Vec<(Column, Vec<u8>)>);

/// What one batch of these tests writes.
#[derive(Debug, Clone, Default)]
pub(super) struct Writes {
    /// Subtrees to enter, each with the node that hosts it, if one does.
    pub(super) enter: Vec<Entered>,
    /// Subtrees to drop.
    pub(super) drop: Vec<Subtree>,
    /// Whole Merks to write.
    pub(super) merks: Vec<(Subtree, Merk)>,
    /// Flat records to put.
    pub(super) records: Vec<(Column, Subtree, Vec<u8>, Vec<u8>)>,
}

impl Writes {
    /// Calls `run` with the batch that states these writes.
    pub(super) fn with<R>(&self, run: impl FnOnce(&Batch<'_>) -> R) -> R {
        fn link(link: &Option<Link>) -> Option<LinkPut<'_>> {
            link.as_ref().map(|link| LinkPut {
                key: &link.key,
                hash: &HASH,
                heights: link.heights,
                aggregate: &link.aggregate,
            })
        }
        fn put(node: &Node) -> NodePut<'_> {
            NodePut {
                key: &node.key,
                feature: &node.feature,
                value_hash: &HASH,
                kv_hash: &HASH,
                left: link(&node.left),
                right: link(&node.right),
                hosted: Hosted::Set(&node.hosted),
                value: ValuePut::Value(&node.value),
                stale: Stale::NONE,
                at: None,
            }
        }
        let nodes: Vec<Vec<NodePut<'_>>> = (self.merks.iter())
            .map(|(_, merk)| merk.nodes.iter().map(put).collect())
            .collect();
        let merks: Vec<MerkWrite<'_>> = (self.merks.iter().zip(&nodes))
            .map(|((subtree, merk), nodes)| MerkWrite {
                subtree: *subtree,
                nodes,
                removed: &[],
                root: RootPut::Set(merk.root.as_deref()),
                anchored: false,
            })
            .collect();
        let enters = self
            .enter
            .iter()
            .map(|(subtree, host)| SubtreeWrite::Enter {
                subtree: *subtree,
                tree_type: TREE_TYPE,
                host: host.as_ref().map(|(parent, key)| HostPut {
                    subtree: *parent,
                    key,
                    position: 0,
                }),
            });
        let drops = (self.drop.iter()).map(|subtree| SubtreeWrite::Drop { subtree: *subtree });
        let subtrees: Vec<SubtreeWrite<'_>> = drops.chain(enters).collect();
        let records: Vec<RecordWrite<'_>> = (self.records.iter())
            .map(|(column, subtree, key, value)| RecordWrite::put(*column, *subtree, key, value))
            .collect();
        run(&Batch {
            records: &records,
            merks: &merks,
            subtrees: &subtrees,
        })
    }

    /// Commits these writes with no transaction, and returns the commit.
    pub(super) fn commit(&self, db: &Db) -> Box<Published> {
        match self.with(|batch| db.write(batch, &mut NoStaleHashes)) {
            Ok(Committed::Published(published)) => published,
            other => panic!("the writes are committed, and not {other:?}"),
        }
    }
}

/// Enters `subtree` with no host and writes `merk` into it, in one commit.
pub(super) fn plant(db: &Db, subtree: Subtree, merk: Merk) -> Box<Published> {
    let writes = Writes {
        enter: vec![(subtree, None)],
        merks: vec![(subtree, merk)],
        ..Writes::default()
    };
    writes.commit(db)
}

/// How many nodes the Merk whose root cell is at `root` has, as
/// `operation` reads it.
fn nodes(operation: &Operation<'_>, root: Option<CellAddress>) -> usize {
    let reader = operation.nodes();
    let mut buffers = LoadBuffers::new();
    let mut count = 0;
    let mut stack: Vec<CellAddress> = root.into_iter().collect();
    while let Some(address) = stack.pop() {
        let pinned = reader.node(address, &mut buffers).expect("a node");
        let node = pinned.node();
        count += 1;
        for side in [Side::Left, Side::Right] {
            stack.extend(node.link(side).map(|link| link.child));
        }
    }
    count
}

/// What a reader finds of `subtree`: `None` if it is not entered, and
/// otherwise the nodes of its Merk and the record under `key` in each
/// column that has one.
pub(super) fn seen(operation: &Operation<'_>, subtree: Subtree, key: &[u8]) -> Option<Seen> {
    let opened = operation.open(subtree).expect("a lookup")?;
    let count = nodes(operation, opened.root());
    let mut records = Vec::new();
    for column in Column::ALL {
        let mut value = Vec::new();
        // The data column of a subtree with a Merk holds its nodes.
        if column == Column::Data && opened.root().is_some() {
            continue;
        }
        if opened.get(column, key, &mut value).expect("a read") {
            records.push((column, value));
        }
    }
    Some((count, records))
}

/// Waits until `done` says so, for at most a minute: the background work
/// of a database on a machine that is busy.
pub(super) fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let began = Instant::now();
    while !done() {
        assert!(began.elapsed() < Duration::from_secs(60), "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Waits until the reclaiming of `db` is as `done` says, and says what it
/// is and what the store holds if that takes more than a minute.
pub(super) fn reclaimed(db: &Db, what: &str, done: impl Fn(&ReclaimStats) -> bool) {
    let began = Instant::now();
    loop {
        let stats = db.stats();
        if done(&stats.reclaimed) {
            return;
        }
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "{what}: {:?}\n{stats}",
            stats.reclaimed
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Tells the reader threads of a test to end when it goes: also when the
/// thread that holds it panics, so that a failed assertion ends the test
/// and leaves no reader running.
pub(super) struct Ending<'a>(pub(super) &'a std::sync::atomic::AtomicBool);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Ends the two background threads of `db` and leaves the handle open: a
/// session in which nothing is written back and nothing is reclaimed but
/// what the test asks for, and one that ends, when the handle is dropped,
/// as a process that is killed ends.
pub(super) fn halt_background(db: &Db) {
    let threads = crate::db::store::lock(&db.threads).take();
    let threads = threads.expect("the threads of the database");
    db.shared.store.signal.stop();
    db.shared.write_back().stop();
    threads.reclaim.join().expect("the reclaimer ends");
    threads.write_back.join().expect("write-back ends");
}
