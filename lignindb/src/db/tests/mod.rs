//! Tests of the facade, through its public calls: databases in a temporary
//! directory, and on the simulated file system where a test cuts the power
//! or fails a file operation.

mod backup;
mod beside;
mod check;
mod checkpoints;
mod clean;
mod counted;
mod ended;
mod enters;
mod filling;
mod full;
mod guards;
mod kept;
mod listed;
mod moves;
mod open;
mod pacing;
mod power;
mod reads;
mod reclaim;
mod reopen;
mod room;
mod schedules;
mod stress;
mod wipe;
mod writes;

use std::{
    collections::{BTreeMap, HashMap},
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use lignin_io::{DirEntry, OpenMode, StdVfs, Vfs, VfsFile, VfsLock};

use lignin_format::{
    tree_page::{Hash, Side},
    CellAddress, PageSize,
};
use lignin_store::load::LoadBuffers;
use lignin_tree::{hash::Stale, node::NodeHandle};

use crate::{
    commit::{Committed, NoStaleHashes, Published},
    db::{OpenError, ReclaimStats},
    snapshot::Operation,
    txn::{
        Batch, Column, Entering, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite,
        RootPut, Subtree, SubtreeWrite, ValuePut,
    },
    Db, Options,
};

/// What a test does to a directory at a moment it chose.
type Change = Box<dyn FnOnce() + Send>;

/// What a test does at every moment of one kind.
type Again = Box<dyn FnMut() + Send>;

/// What a [`Watched`] file layer saw.
#[derive(Default)]
pub(super) struct Counts {
    reads: AtomicU64,
    read_bytes: AtomicU64,
    opens: AtomicU64,
    /// Calls that change a file or a directory, or make one durable.
    changes: AtomicU64,
    /// The same, for each thread that made one.
    changes_by: Mutex<HashMap<ThreadId, u64>>,
    /// Every sync that has ended, in that order: what was synced, the
    /// path as the caller named it, of the directory or of the file when
    /// it was opened, who made it and how long it took.
    synced: Mutex<Vec<(Synced, PathBuf, ThreadId, Duration)>>,
    /// The directory that paths with no root are in, if there is one.
    root: Option<PathBuf>,
    /// How many appends to a segment go through before one waits, if one
    /// is to wait, and how many have waited.
    holding: Mutex<Option<u64>>,
    released: Condvar,
    held: AtomicU64,
    /// What is run, once, when a lock is asked for, before it is taken,
    /// and whether only a lock that makes its file counts.
    at_lock: Mutex<Option<(bool, Change)>>,
    /// What is run, once, after a lock that makes no file found none.
    after_no_lock_file: Mutex<Option<Change>>,
    /// What is run, once, after a lock was taken.
    after_lock: Mutex<Option<Change>>,
    /// What is run, once, after a directory was listed.
    after_listing: Mutex<Option<Change>>,
    /// What is run, once, before a segment is read.
    at_segment_read: Mutex<Option<Change>>,
    /// What is run before every open of a log file below a directory, for
    /// reading, and that directory.
    at_log_open: Mutex<Option<(PathBuf, Again)>>,
}

impl fmt::Debug for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Counts")
            .field("changes", &self.changes)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
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
        let mut by = self.0.changes_by.lock().expect("the counts");
        *by.entry(thread::current().id()).or_default() += 1;
    }

    fn sync<T>(
        &self,
        what: Synced,
        path: &Path,
        sync: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        self.changed();
        let began = Instant::now();
        let synced = sync();
        let took = began.elapsed();
        let record = (what, path.to_path_buf(), thread::current().id(), took);
        self.0.synced.lock().expect("the list").push(record);
        synced
    }

    /// The counts since the last call: reads, the bytes they read, and
    /// files opened.
    pub(super) fn take(&self) -> (u64, u64, u64) {
        let take = |count: &AtomicU64| count.swap(0, Ordering::Relaxed);
        let counts = &self.0;
        (
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

    /// From here on `through` appends to a segment are made, and the next
    /// waits until [`Self::release`]. A block is one append, so a
    /// write-back pass is held with its next page in hand, the block not
    /// written and its link not pushed.
    pub(super) fn hold_segment_appends_after(&self, through: u64) {
        *self.0.holding.lock().expect("the gate") = Some(through);
    }

    /// Lets the append that waits go on, and holds none any more.
    pub(super) fn release(&self) {
        *self.0.holding.lock().expect("the gate") = None;
        self.0.released.notify_all();
    }

    /// Has `change` run when the next lock is asked for, with a file made
    /// or with none, before it is taken: a directory is changed between
    /// an open's look at it and its lock.
    pub(super) fn at_the_next_lock(&self, change: impl FnOnce() + Send + 'static) {
        *self.0.at_lock.lock().expect("the hook") = Some((false, Box::new(change)));
    }

    /// Has `change` run after the next lock that makes no file has found
    /// none, before its caller hears of it: what another open does between
    /// this one's ask for the lock and its next look.
    pub(super) fn after_the_next_lock_that_finds_no_file(
        &self,
        change: impl FnOnce() + Send + 'static,
    ) {
        *self.0.after_no_lock_file.lock().expect("the hook") = Some(Box::new(change));
    }

    /// Has `change` run after the next lock that is taken, with a file
    /// made or with none, before its caller has the guard: a directory is
    /// changed between an open's lock and its look under it.
    pub(super) fn after_the_next_lock_that_is_taken(&self, change: impl FnOnce() + Send + 'static) {
        *self.0.after_lock.lock().expect("the hook") = Some(Box::new(change));
    }

    /// Has `change` run after the next listing of a directory, before its
    /// caller has the entries: a directory is changed in the middle of an
    /// open's look at it, between the listing and the read of the
    /// manifest.
    pub(super) fn after_the_next_listing(&self, change: impl FnOnce() + Send + 'static) {
        *self.0.after_listing.lock().expect("the hook") = Some(Box::new(change));
    }

    /// Has `change` run before the next read of a segment, on the thread
    /// that makes it: an operation that loads a page waits with the
    /// location of its block in hand and the block not read.
    pub(super) fn before_the_next_read_of_a_segment(&self, change: impl FnOnce() + Send + 'static) {
        *self.0.at_segment_read.lock().expect("the hook") = Some(Box::new(change));
    }

    /// Has `change` run before every open, for reading, of a log file of
    /// the database in `dir`: between any two looks that an open of that
    /// database takes at its log.
    pub(super) fn before_every_read_of_a_log_file_in(
        &self,
        dir: &Path,
        change: impl FnMut() + Send + 'static,
    ) {
        *self.0.at_log_open.lock().expect("the hook") = Some((dir.join("wal"), Box::new(change)));
    }

    fn change_at_a_log_open(&self, path: &Path) {
        let hook = self.0.at_log_open.lock().expect("the hook").take();
        let Some((dir, mut change)) = hook else {
            return;
        };
        if path.starts_with(&dir) {
            // The hook is out of its place while it runs: what it does
            // opens files of its own.
            change();
        }
        *self.0.at_log_open.lock().expect("the hook") = Some((dir, change));
    }

    fn change_at_a_segment_read(&self) {
        let change = self.0.at_segment_read.lock().expect("the hook").take();
        if let Some(change) = change {
            change();
        }
    }

    fn change_after_the_lock(&self) {
        let change = self.0.after_lock.lock().expect("the hook").take();
        if let Some(change) = change {
            change();
        }
    }

    /// The same, at the next lock that makes its file if it is missing.
    pub(super) fn at_the_next_lock_that_makes(&self, change: impl FnOnce() + Send + 'static) {
        *self.0.at_lock.lock().expect("the hook") = Some((true, Box::new(change)));
    }

    /// How many appends have waited so far.
    pub(super) fn held(&self) -> u64 {
        self.0.held.load(Ordering::Acquire)
    }

    fn change_at_the_lock(&self, makes: bool) {
        let mut hook = self.0.at_lock.lock().expect("the hook");
        let change = match &*hook {
            Some((only_one_that_makes, _)) if makes || !only_one_that_makes => hook.take(),
            _ => None,
        };
        drop(hook);
        if let Some((_, change)) = change {
            change();
        }
    }

    fn wait_at_the_gate(&self) {
        let mut holding = self.0.holding.lock().expect("the gate");
        match *holding {
            None => return,
            Some(0) => {}
            Some(through) => {
                *holding = Some(through - 1);
                return;
            }
        }
        self.0.held.fetch_add(1, Ordering::AcqRel);
        while holding.is_some() {
            holding = self.0.released.wait(holding).expect("the gate");
        }
    }

    /// How many of those calls the calling thread has made: what a close
    /// writes itself, whatever a turn of the background work that was
    /// under way still writes beside it.
    pub(super) fn changes_here(&self) -> u64 {
        let by = self.0.changes_by.lock().expect("the counts");
        by.get(&thread::current().id()).copied().unwrap_or(0)
    }

    /// The directories that were synced so far, as the caller named them.
    pub(super) fn synced_dirs(&self) -> Vec<PathBuf> {
        let synced = self.0.synced.lock().expect("the list");
        let dirs = synced.iter().filter(|(what, ..)| *what == Synced::Dir);
        dirs.map(|(_, path, ..)| path.clone()).collect()
    }

    /// The syncs that the calling thread has made since the last call, in
    /// order, each with the time it took. Those of other threads are
    /// dropped.
    pub(super) fn take_synced(&self) -> Vec<(Synced, PathBuf, Duration)> {
        let synced = std::mem::take(&mut *self.0.synced.lock().expect("the list"));
        let here = thread::current().id();
        let mine = synced.into_iter().filter(|(_, _, by, _)| *by == here);
        mine.map(|(what, path, _, took)| (what, path, took))
            .collect()
    }
}

/// What a sync was of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Synced {
    /// The content of a file.
    Data,
    /// A file with its length and the rest of what the system keeps of it.
    All,
    /// The names in a directory.
    Dir,
}

struct WatchedFile {
    file: Box<dyn VfsFile>,
    watched: Watched,
    /// The path the file was opened by, as the caller named it.
    path: PathBuf,
    /// Whether the file is in the directory of the segments.
    segment: bool,
}

impl fmt::Debug for WatchedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.file.fmt(f)
    }
}

impl VfsFile for WatchedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if self.segment {
            self.watched.change_at_a_segment_read();
        }
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
        if self.segment {
            self.watched.wait_at_the_gate();
        }
        self.watched.changed();
        self.file.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        (self.watched).sync(Synced::Data, &self.path, || self.file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        (self.watched).sync(Synced::All, &self.path, || self.file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.watched.changed();
        self.file.set_len(len)
    }
}

/// A guard of the file layer below, which is asked about a path as that
/// layer has it.
#[derive(Debug)]
struct WatchedLock(Box<dyn VfsLock>, Watched);

impl VfsLock for WatchedLock {
    fn is_at(&self, path: &Path) -> io::Result<bool> {
        self.0.is_at(&self.1.at(path))
    }
}

impl Vfs for Watched {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.0.opens.fetch_add(1, Ordering::Relaxed);
        if !matches!(mode, OpenMode::Read) {
            self.changed();
        } else {
            self.change_at_a_log_open(path);
        }
        let file = StdVfs.open(&self.at(path), mode)?;
        let segment = path.components().any(|part| part.as_os_str() == "seg");
        Ok(Box::new(WatchedFile {
            file,
            watched: self.clone(),
            path: path.to_path_buf(),
            segment,
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
        let entries = StdVfs.read_dir(&self.at(path));
        let change = self.0.after_listing.lock().expect("the hook").take();
        if let Some(change) = change {
            change();
        }
        entries
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.sync(Synced::Dir, path, || StdVfs.sync_dir(&self.at(path)))
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.changed();
        self.change_at_the_lock(true);
        let lock = StdVfs.lock(&self.at(path))?;
        self.change_after_the_lock();
        Ok(Box::new(WatchedLock(lock, self.clone())))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.change_at_the_lock(false);
        let lock = StdVfs.lock_existing(&self.at(path));
        if matches!(&lock, Err(error) if error.kind() == io::ErrorKind::NotFound) {
            let change = self.0.after_no_lock_file.lock().expect("the hook").take();
            if let Some(change) = change {
                change();
            }
        }
        let lock = lock?;
        self.change_after_the_lock();
        Ok(Box::new(WatchedLock(lock, self.clone())))
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
    /// The keys of nodes that the Merk had before and has no more.
    removed: Vec<Vec<u8>>,
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
        Self {
            nodes,
            root,
            removed: Vec::new(),
        }
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

    /// The same Merk with one more node stated under `key`, which no link
    /// of its tree leads to: the engine keeps it as an unlinked record.
    pub(super) fn with_unlinked(mut self, key: &[u8], value: &[u8]) -> Self {
        self.nodes.push(Node {
            key: key.to_vec(),
            value: value.to_vec(),
            feature: vec![3, 1, 0, 0, 0, 0, 0, 0, 0],
            left: None,
            right: None,
            hosted: Vec::new(),
        });
        self
    }

    /// The same Merk, written over one that has a node under `key`,
    /// which this one has not.
    pub(super) fn without(mut self, key: &[u8]) -> Self {
        self.removed.push(key.to_vec());
        self
    }

    pub(super) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The keys and the values of the nodes, in key order.
    pub(super) fn items(&self) -> Items {
        let items = self.nodes.iter();
        let mut items: Items = items
            .map(|node| (node.key.clone(), node.value.clone()))
            .collect();
        items.sort();
        items
    }
}

/// The keys and the values of the nodes of a Merk, in key order.
pub(super) type Items = Vec<(Vec<u8>, Vec<u8>)>;

/// The nodes of the Merk of `subtree` as a reader walks them: each key
/// with the bytes of its value. `None` if the subtree is not entered.
pub(super) fn items_of(operation: &Operation<'_>, subtree: Subtree) -> Option<Items> {
    let opened = operation.open(subtree).expect("a lookup")?;
    let mut walk = opened.walk().expect("a lookup");
    let mut items = Items::new();
    walk.seek_to_first(operation).expect("a move");
    while let Some(key) = walk.key().map(<[u8]>::to_vec) {
        let mut children = crate::txn::ChildKeys::new();
        let value = walk.node_with_children(operation, &mut children, |view| {
            let view = view.expect("the node the cursor stands on");
            let mut value = lignin_tree::value::ValueBuf::new();
            view.value(&mut value).expect("a value").to_vec()
        });
        items.push((key, value.expect("a read")));
        walk.next(operation).expect("a move");
    }
    Some(items)
}

/// A subtree to enter, with the subtree and the key of the node that
/// hosts it, if one does.
pub(super) type Entered = (Subtree, Option<(Subtree, Vec<u8>)>);

/// What a reader finds of a subtree: the nodes of its Merk, and the record
/// under one key in each column that has one.
pub(super) type Seen = (usize, Vec<(Column, Vec<u8>)>);

/// A subtree to move to another prefix (`SubtreeWrite::Move`), at the
/// position 0 of its host.
#[derive(Debug, Clone)]
pub(super) struct Moved {
    pub(super) from: Subtree,
    pub(super) to: Subtree,
    /// The Merk the host is in, and the key of the host afterwards.
    pub(super) host: (Subtree, Vec<u8>),
    /// The read of the node that hosts the subtree before.
    pub(super) read: Option<NodeHandle>,
    /// The prefixes of the subtrees below it, before and afterwards.
    pub(super) below: Vec<(Subtree, Subtree)>,
}

/// What one batch of these tests writes.
#[derive(Debug, Clone, Default)]
pub(super) struct Writes {
    /// Subtrees to move.
    pub(super) moves: Vec<Moved>,
    /// Subtrees to enter, each with the node that hosts it, if one does.
    pub(super) enter: Vec<Entered>,
    /// Subtrees to enter if they are absent, each as for `enter`.
    pub(super) enter_if_absent: Vec<Entered>,
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
        let removed: Vec<Vec<&[u8]>> = (self.merks.iter())
            .map(|(_, merk)| merk.removed.iter().map(Vec::as_slice).collect())
            .collect();
        let merks: Vec<MerkWrite<'_>> = (self.merks.iter().zip(&nodes).zip(&removed))
            .map(|(((subtree, merk), nodes), removed)| MerkWrite {
                subtree: *subtree,
                nodes,
                removed,
                root: RootPut::Set(merk.root.as_deref()),
                anchored: false,
            })
            .collect();
        fn entering(entered: &Entered, if_absent: bool) -> SubtreeWrite<'_> {
            let (subtree, host) = entered;
            let entering = Entering {
                subtree: *subtree,
                tree_type: TREE_TYPE,
                host: host.as_ref().map(|(parent, key)| HostPut {
                    subtree: *parent,
                    key,
                    position: 0,
                }),
                if_absent,
            };
            entering.write()
        }
        let new = self.enter.iter().map(|entered| entering(entered, false));
        let absent = self.enter_if_absent.iter();
        let enters = new.chain(absent.map(|entered| entering(entered, true)));
        let drops = (self.drop.iter()).map(|subtree| SubtreeWrite::Drop { subtree: *subtree });
        let moves = self.moves.iter().map(|moved| SubtreeWrite::Move {
            from: moved.from,
            to: moved.to,
            host: HostPut {
                subtree: moved.host.0,
                key: &moved.host.1,
                position: 0,
            },
            read: moved.read,
            below: &moved.below,
        });
        let subtrees: Vec<SubtreeWrite<'_>> = drops.chain(moves).chain(enters).collect();
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
        let before = db.version();
        let published = match self.with(|batch| db.write(batch, &mut NoStaleHashes)) {
            Ok(Committed::Published(published)) => published,
            other => panic!("the writes are committed, and not {other:?}"),
        };
        assert!(published.version > before, "a commit takes a later version");
        assert!(published.miscounted.is_none(), "a value is miscounted");
        if !published.dropped.is_empty() {
            // The handle the commit hands on is the directory of the
            // version that was published: it leaves out what one that is
            // loaded from the pages of that version leaves out.
            // A round of the reclaiming takes entries out of the pages
            // of the latest version, so the comparison is made with the
            // reclaiming held, and left out where a round is running or
            // the caller is inside one. It is left out as well where a
            // commit came before the version was sampled: it was the
            // latest then, and the guard keeps its pages for the
            // comparison.
            let store = &*db.shared.store;
            let held = store.reclaiming.try_lock();
            let guard = db.shared.versions.guard();
            let poisoned = matches!(held, Err(std::sync::TryLockError::Poisoned(_)));
            if (held.is_ok() || poisoned) && guard.latest() == published.version {
                let pages = store.committed(&guard, published.version);
                let fresh = store.catalog().load_directory(&pages);
                let fresh = fresh.expect("the directory of the version");
                let mut entries = fresh.entries();
                while let Some((_, entry)) = entries.next(&pages).expect("an entry") {
                    assert_eq!(published.directory.is_live(&entry), fresh.is_live(&entry));
                }
            }
        }
        published
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

/// The physical invariant checker on the latest version of `db`, with what
/// a store keeps beside its pages: for a database whose background work
/// has ended and that nobody writes to.
pub(super) fn checked(db: &Db) -> crate::verify::Report {
    let at = db.shared.versions.latest();
    db.check(at, crate::verify::Scope::Still, None)
}

/// Waits until the reclaiming of `db` is as `done` says, and says what it
/// is and what the store holds if that takes more than a minute.
pub(super) fn reclaimed(db: &Db, what: &str, done: impl Fn(&ReclaimStats) -> bool) {
    stats_until(db, what, |stats| &stats.reclaimed, done);
}

/// Waits until a part of the counts of `db` is as `done` says, and says
/// what it is and what the store holds if that takes more than a minute.
pub(super) fn stats_until<T: std::fmt::Debug>(
    db: &Db,
    what: &str,
    part: impl Fn(&crate::db::DbStats) -> &T,
    done: impl Fn(&T) -> bool,
) {
    let began = Instant::now();
    loop {
        let stats = db.stats();
        if done(part(&stats)) {
            return;
        }
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "{what}: {:?}\n{stats}",
            part(&stats)
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

/// Ends the background threads of `db` and leaves the handle open: a
/// session in which nothing is written back and nothing is reclaimed but
/// what the test asks for, and one that ends, when the handle is dropped,
/// as a process that is killed ends.
///
/// A loop that is ended here may have made turns since the open: see
/// [`open_at_rest`] for a session in which none is made.
pub(super) fn halt_background(db: &Db) {
    let threads = crate::db::store::lock(&db.threads).take();
    let threads = threads.expect("the threads of the database");
    db.shared.stop_background();
    threads.reclaim.join().expect("the reclaimer ends");
    threads.cleaner.join().expect("the cleaner ends");
    threads.write_back.join().expect("write-back ends");
}

/// Opens the database in `dir` as [`Db::open`] does, and starts none of
/// its three threads: a session in which no thread but the test's own
/// writes back, reclaims or cleans, from the open on.
///
/// [`halt_background`] does not give that. A thread that [`Db::open`]
/// started can have made its first turn before it is told to end, and
/// whether it has is the schedule's to say. A turn of any of them shows
/// in what the session does next:
///
/// - The reclaimer's first round takes the allocator's step
///   (`Versions::reclaim`). An open has a page id that an earlier session
///   freed, and whose free link the page table still holds, as retired,
///   and that step frees it. So after that round the first commit of
///   the session can give a new page an id that it could not have given
///   it before.
/// - Write-back's first turn makes the file operations that the first
///   turn of the test would have made, and meets what the device refuses
///   of them in the test's place.
/// - The cleaner's first turn is a pass over the segments and one over
///   the value log, with the file operations of both.
///
/// This is [`Db::open`] up to the start of its threads, step for step, and
/// has to be kept so. The handle ends as one that [`halt_background`] was
/// called on does: when it is dropped, as a process that is killed.
pub(super) fn open_at_rest(dir: impl AsRef<Path>, options: &Options) -> Result<Db, OpenError> {
    let dir = dir.as_ref();
    let opened = crate::db::open::open_dir(dir, options)?;
    let (shared, directory, _) = crate::db::open::open_store(dir, options, opened.manifest)?;
    shared
        .adopt_drops(&directory)
        .map_err(OpenError::catalog("what earlier drops left"))?;
    Ok(Db {
        shared: Arc::new(shared),
        threads: Mutex::new(None),
        created: opened.created,
        opening: crate::db::Opening::default(),
        unchecked: std::sync::atomic::AtomicBool::new(false),
        hash_rules: Mutex::new(None),
        guard: opened.lock,
    })
}
