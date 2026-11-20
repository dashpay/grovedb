//! A page store on disk with what a write transaction needs around it that
//! is not built yet: the open of the engine (TXN-07), reduced to making the
//! stores, the log and the first commit. The subtree directory is the real
//! one (`lignin_tree::directory`), and so is the commit (`crate::commit`).
//!
//! A store is checked by the physical invariant checker (`crate::verify`)
//! when it is dropped, in a debug build: `Db::verify` is the check, and
//! the end of this file says when it is left out.

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    },
};

use lignin_format::{
    manifest::Manifest,
    tree_page::{Hash, Side},
    wal::GroupWriter,
    CellAddress, CommitVersion, PageId, PageSize,
};
use lignin_io::Vfs;
use lignin_store::{
    alloc::{PageAllocator, PageState, Pins},
    cache::{BufferPool, OwnerId, PoolConfig},
    flush::{FlushConfig, FlushError, FlushState, Stores, WriteBack},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore},
    stats::Stats,
    table::{PageTable, TableCheckpoints},
    values::{OwnersError, ValueStore},
    wal::{LogOptions, LogReader, LogStore},
};
use lignin_tree::{
    directory::Directory,
    drop::{self, Busy, FlatDrop, FlatDrops, FlatReclaimed, Progress, Reclaimed, Reclaimer, Wait},
    flat::{Committed, FlatConfig, FlatPages, FlatTree, Private, StepLog},
    fragment::Layout,
    hash::{HashRules, Refusal},
    node::{Lease, NodePage, NodeRef},
    value::InlinePolicy,
};

use crate::{
    commit::{CommitError, CommitState, Committer, Published},
    snapshot::{ReadPath, Reclaim, Versions},
    txn::{path::Store, Batch, Commits, KeySet, Keyspaces, Subtree, TxnError, WritePath, WriteTxn},
    verify::{self, Finding, Grove, Options, Place, Report, Rule, Scope},
};

pub const SIZE: PageSize = PageSize::MIN;

pub fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The subtrees that every store of the tests has entered from its first
/// commit on, as Merks that stand alone: those whose 32 bytes are all one
/// byte below this. A test that enters a subtree itself names another.
pub const ENTERED: u8 = 200;

/// The tree type of the Merks a store is made with.
pub const TREE_TYPE: &[u8] = b"merk";

/// Mixes `bytes` into `seed`: what the hashes and the tree shapes of the
/// tests' model are made of.
pub fn mix(seed: u64, bytes: &[u8]) -> u64 {
    let mut state = seed ^ 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        state = (state ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    state ^ (state >> 29)
}

/// The hash of `parts`, as the model of the tests makes its hashes.
pub fn hash_of(parts: &[&[u8]]) -> Hash {
    let mut hash = [0; 32];
    let mut state = 0x1234_5678;
    for part in parts {
        state = mix(state, part).wrapping_add(part.len() as u64);
    }
    for (index, chunk) in hash.chunks_mut(8).enumerate() {
        state = mix(state.wrapping_add(index as u64), &state.to_le_bytes());
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    hash
}

/// The hash rules of these tests, which are those the model makes its
/// hashes by, with a count of the calls.
pub struct Rules<'a>(pub &'a AtomicU64);

impl HashRules for Rules<'_> {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        _: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(hash_of(&[value]))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(hash_of(&[key, value_hash]))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.0.fetch_add(1, Ordering::Relaxed);
        let none = [0; 32];
        let child = |side| node.link(side).map_or(&none, |link| link.hash);
        Ok(hash_of(&[
            node.kv_hash(),
            child(Side::Left),
            child(Side::Right),
        ]))
    }
}

#[derive(Debug, Default)]
pub struct Counts {
    pub commits: AtomicU64,
    pub conflicts: AtomicU64,
    /// Pages created under an id that named another page before.
    pub ids_reused: AtomicU64,
    pub ids_released: AtomicU64,
    pub pages_freed: AtomicU64,
    /// Pages freed that no segment ever held, under an id that was freed
    /// before.
    pub freed_unwritten: AtomicU64,
    /// The pointers to hosted roots that the last check of the tree pages
    /// followed.
    pub hosted: AtomicU64,
    /// The tree pages that commits attached values to.
    pub value_pages: AtomicU64,
    /// The nodes with a stale hash that commits evaluated, and the calls
    /// of a hash rule that took.
    pub stale_nodes: AtomicU64,
    pub rule_calls: AtomicU64,
    /// The Merks that commits dropped or cleared and that left something.
    pub dropped: AtomicU64,
    /// The private copies of tree pages that commits were handed, and
    /// those among them that no write had changed.
    pub tree_copies: AtomicU64,
    pub tree_copies_unchanged: AtomicU64,
    /// The page steps of the reclaimers of this store whose records the
    /// log took.
    pub steps: AtomicU64,
    /// The Merks in use, the entries below a dropped Merk, and the dropped
    /// Merks, as the last check of the tree pages found them.
    pub census: Mutex<(u64, u64, u64)>,
}

/// How a commit of the test store ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The write set was empty: no version was taken (CV2).
    Empty,
    /// A commit outside the transaction wrote a key after the transaction
    /// first wrote it (CF2).
    Conflict,
    /// The commit was published as this version.
    Committed(CommitVersion),
}

pub struct Db {
    pub segments: SegmentStore,
    pub table: PageTable,
    pub pool: BufferPool,
    pub ids: PageAllocator,
    pub values: ValueStore,
    pub commits: Commits,
    pub counts: Counts,
    /// The log, which every commit appends its group to.
    pub log: LogStore,
    pub manifest: Mutex<ManifestStore>,
    pub checkpoints: TableCheckpoints,
    /// What write-back keeps of the store. No loop runs it: a test makes
    /// its turns, and so does a committer that has to wait for room.
    pub flush: FlushState,
    /// What the commits of the store share, and the errors of write-back
    /// that they were told.
    pub committing: CommitState,
    pub flush_errors: Arc<Mutex<Vec<String>>>,
    /// The counts of the store, which a write transaction is counted in
    /// while it has a write.
    pub stats: Stats,
    /// The files of the store, and the directory they are in.
    pub vfs: Arc<dyn Vfs>,
    pub dir: PathBuf,
    keyspaces: OnceLock<Keyspaces>,
    directory: OnceLock<FlatTree>,
    flat_drops: OnceLock<FlatDrops>,
    used: Mutex<BTreeSet<PageId>>,
    /// Whether the store is left out of the check that its drop makes.
    unchecked: AtomicBool,
    /// The rules that the check of its drop recomputes the hashes by, where
    /// they are not those of the model of these tests.
    hash_rules: Mutex<Option<Box<dyn HashRules + Send>>>,
    /// Whether the check of its drop recomputes no hash.
    unhashed: AtomicBool,
    _dir: Option<tempfile::TempDir>,
}

/// What a reclaimer of this store asks for the latest version, and what
/// takes the records of its page steps: they are appended to the log
/// through write-back. The engine's part of `lignin_tree::drop` is
/// TXN-07's to build.
pub struct Steps<'s> {
    db: &'s Db,
    versions: &'s Versions,
}

impl StepLog for Steps<'_> {
    fn latest(&self) -> CommitVersion {
        self.versions.latest()
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        // Through write-back, as the engine's reclaimer will (TXN-07): the
        // record goes to the log, and write-back knows of the step.
        self.db.write_back(self.versions).append_change(record)?;
        self.db.counts.steps.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// What a sweep of everything the committed drops left took out.
#[derive(Debug, Default)]
pub struct Swept {
    /// What was reclaimed of each dropped or cleared Merk.
    pub merks: Vec<Reclaimed>,
    /// The records of unlinked flat pages that were taken out, and what
    /// freeing their pages did.
    pub flat_drops: Vec<FlatDrop>,
    pub flat: FlatReclaimed,
    /// Why something is left, if something is.
    pub blocked: Option<Busy>,
    /// Whether nothing was begun, because a reader from before a drop is
    /// still there.
    pub waiting: bool,
}

/// The guard of a check that runs when nothing else does.
struct AtRest;

impl lignin_tree::OperationGuard for AtRest {}

struct Hook(Arc<Db>);

/// What a reclaim does to the store: the page id allocator and the buffer
/// pool take their steps.
impl Reclaim for Hook {
    fn reclaim(&self, pins: &Versions) {
        let mut released = Vec::new();
        self.0.ids.reclaim(pins, &mut released);
        self.0
            .counts
            .ids_released
            .fetch_add(released.len() as u64, Ordering::Relaxed);
        self.0.pool.reclaim(pins);
    }
}

#[path = "no_sync.rs"]
mod no_sync;

impl Db {
    /// An empty store with a pool of `frames` frames, in a directory of its
    /// own. Its first commit makes the flat keyspaces and the directory.
    pub fn new(frames: usize) -> (Arc<Self>, Versions) {
        // With syncs that do nothing: on a disk they are most of what the
        // tests of this store would wait for, and a test of what a crash
        // leaves names its file system itself (`Db::on`).
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().to_path_buf();
        Self::on(
            Arc::new(no_sync::NoSync),
            &path,
            frames,
            LogOptions::default(),
            FlushConfig::new(SIZE),
            Some(dir),
        )
    }

    /// An empty store in the directory `dir` of `vfs`, which is there and
    /// holds nothing, with a log that is written as `options` say and a
    /// write-back that works as `flush` says.
    pub fn on(
        vfs: Arc<dyn Vfs>,
        dir: &Path,
        frames: usize,
        options: LogOptions,
        flush: FlushConfig,
        keep: Option<tempfile::TempDir>,
    ) -> (Arc<Self>, Versions) {
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
        let manifest = ManifestStore::create(vfs.clone(), dir, empty).expect("a manifest");
        let segments = SegmentStore::open(&manifest).expect("the segments");
        let values = ValueStore::open(&manifest, &[]).expect("the value log");
        let checkpoints = TableCheckpoints::open(&manifest).expect("the table checkpoints");
        // A session reads the log to its end and opens it there.
        let named = manifest.manifest().expect("the manifest");
        let end = (LogReader::new(vfs.clone(), dir, named).into_end()).expect("the log is read");
        let log = LogStore::open(&manifest, &end, options).expect("the log");
        let flush_errors = Arc::new(Mutex::new(Vec::new()));
        let told = flush_errors.clone();
        let report = move |error: &FlushError| locked(&told).push(error.to_string());
        let pool = BufferPool::new(PoolConfig {
            page_size: SIZE,
            capacity: frames * (SIZE.get() + 512),
        })
        .expect("a pool");
        let db = Arc::new(Self {
            segments,
            table: PageTable::new(),
            pool,
            ids: PageAllocator::new(),
            values,
            commits: Commits::new(CommitVersion::EMPTY),
            counts: Counts::default(),
            log,
            manifest: Mutex::new(manifest),
            checkpoints,
            flush: FlushState::new(flush),
            committing: CommitState::reporting(report),
            flush_errors,
            stats: Stats::new(),
            vfs,
            dir: dir.to_path_buf(),
            keyspaces: OnceLock::new(),
            directory: OnceLock::new(),
            flat_drops: OnceLock::new(),
            used: Mutex::new(BTreeSet::new()),
            // A store on files that a test may fail or cut is checked by
            // the test that knows what it did to them.
            unchecked: AtomicBool::new(keep.is_none()),
            hash_rules: Mutex::new(None),
            unhashed: AtomicBool::new(false),
            _dir: keep,
        });
        let versions = Versions::with_reclaim(CommitVersion::EMPTY, Box::new(Hook(db.clone())));
        {
            // The manifest names no table checkpoint, and the log turns to
            // a file of this session before its first append.
            let write_back = db.write_back(&versions);
            write_back.adopt(None).expect("the whole log is kept");
            assert!(write_back.roll_log().expect("the log has a file"));
        }
        db.make_keyspaces(&versions);
        (db, versions)
    }

    /// The write-back of this store under its `versions`.
    pub fn write_back<'s>(&'s self, versions: &'s Versions) -> WriteBack<'s> {
        let stores = Stores {
            pool: &self.pool,
            table: &self.table,
            segments: &self.segments,
            values: &self.values,
            log: &self.log,
            checkpoints: &self.checkpoints,
            manifest: &self.manifest,
            versions,
            page_values: None,
        };
        WriteBack::new(stores, &self.flush, SIZE).expect("a pool of the store's page size")
    }

    pub fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, SIZE).expect("a loader")
    }

    pub fn config(&self) -> FlatConfig {
        FlatConfig::new(SIZE)
    }

    /// The first commit: four empty flat trees, and the subtree directory
    /// with the Merks every test may write ([`ENTERED`]). It is made by
    /// hand, as the engine's create will make it (TXN-07): a write path
    /// needs the keyspaces to be there.
    fn make_keyspaces(&self, versions: &Versions) {
        let guard = versions.guard();
        let base = self.committed(&guard, CommitVersion::EMPTY);
        let mut pages = Private::new(base, &self.pool, OwnerId::new(u64::MAX), &self.ids);
        let mut make = || FlatTree::create(&mut pages, self.config()).expect("a tree");
        let keyspaces = Keyspaces {
            data: make(),
            aux: make(),
            roots: make(),
            meta: make(),
        };
        let directory = Directory::create(&mut pages, self.config()).expect("a directory");
        for name in 0..ENTERED {
            (directory.add(&mut pages, &[name; 32], TREE_TYPE, None)).expect("a Merk is entered");
        }
        self.keyspaces.set(keyspaces).expect("made once");
        self.directory.set(*directory.tree()).expect("made once");
        // The keyspace of the pages that drops unlink, made last: the
        // pages before it have the ids they had.
        let drops = FlatDrops::create(&mut pages, self.config()).expect("a keyspace");
        self.flat_drops.set(drops).expect("made once");

        let lock = self.committing.lock();
        let counts = self.values.step().expect("the counts");
        let at = self.log.next_group().expect("a place");
        let commit = at.version();
        assert_eq!(commit, version(1));
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(commit, at.address(), &mut bytes).expect("a group");
        pages.log_values(&mut group).expect("no value");
        for page in pages.written().collect::<Vec<_>>() {
            assert!(pages.log_page(page, &mut group).expect("a run"));
        }
        group.finish();
        let appended = self.log.append_group(&at, &bytes).expect("it is appended");
        (self.write_back(versions))
            .committed(commit, appended.at, &bytes)
            .expect("write-back is told");
        for (copy, created) in pages.into_parts().written {
            assert!(created);
            let page = copy.page();
            drop(self.pool.install(&copy, commit).expect("it installs"));
            self.ids.publish(page).expect("it is private");
            locked(&self.used).insert(page);
        }
        drop(counts);
        self.commits.record(commit, Default::default());
        versions.publish(commit).expect("the first version");
        drop(lock);
    }

    /// Commits by hand what `write` does to the flat pages of the latest
    /// version, as the first commit of a store is made: for a test that
    /// writes what no write transaction would. Nothing else commits
    /// meanwhile, and `write` gives no value out of line.
    pub fn commit_flat(
        &self,
        versions: &Versions,
        write: impl FnOnce(&mut crate::txn::FlatWriter<'_, '_>),
    ) -> CommitVersion {
        let guard = versions.guard();
        let base = self.committed(&guard, versions.latest());
        let mut pages = Private::new(base, &self.pool, WritePath::next_owner(), &self.ids);
        write(&mut pages);

        let lock = self.committing.lock();
        let counts = self.values.step().expect("the counts");
        let at = self.log.next_group().expect("a place");
        let commit = at.version();
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(commit, at.address(), &mut bytes).expect("a group");
        pages.log_values(&mut group).expect("no value");
        let mut changed = BTreeSet::new();
        let written: BTreeSet<PageId> = pages.written().collect();
        let freed: BTreeSet<PageId> = pages.freed().collect();
        for page in written.union(&freed) {
            if freed.contains(page) {
                group.freed(*page).expect("a freed page");
            } else if pages.log_page(*page, &mut group).expect("a run") {
                changed.insert(*page);
            }
        }
        assert!(pages.released().is_empty(), "a value was released");
        group.finish();
        let appended = self.log.append_group(&at, &bytes).expect("it is appended");
        (self.write_back(versions))
            .committed(commit, appended.at, &bytes)
            .expect("write-back is told");
        let parts = pages.into_parts();
        for (copy, created) in parts.written {
            let page = copy.page();
            if !changed.contains(&page) {
                self.pool.discard(&copy).expect("it is discarded");
                continue;
            }
            drop(self.pool.install(&copy, commit).expect("it installs"));
            if created {
                self.ids.publish(page).expect("it is private");
                locked(&self.used).insert(page);
            }
        }
        for copy in parts.freed {
            let page = copy.page();
            self.pool.discard(&copy).expect("it is discarded");
            match self
                .table
                .push(page, lignin_store::table::Link::free(commit))
            {
                Ok(()) | Err(lignin_store::table::LinkError::AlreadyFree) => {}
                Err(error) => panic!("the free link of {page}: {error}"),
            }
            self.pool.free(page, commit).expect("it is freed");
            self.ids.free(page, commit).expect("it is live");
        }
        for page in parts.unused {
            self.ids.release(page).expect("it is private");
        }
        drop(counts);
        self.commits.record(commit, Default::default());
        versions.publish(commit).expect("the next version");
        drop(lock);
        commit
    }

    pub fn keyspaces(&self) -> Keyspaces {
        *self.keyspaces.get().expect("made with the store")
    }

    /// The keyspace of the subtree directory.
    pub fn directory(&self) -> FlatTree {
        *self.directory.get().expect("made with the store")
    }

    /// The keyspace of the pages that drops unlinked and that wait to be
    /// freed.
    pub fn flat_drops(&self) -> FlatDrops {
        *self.flat_drops.get().expect("made with the store")
    }

    /// A reclaimer of what the drops of this store left.
    pub fn reclaimer<'s>(&'s self, versions: &'s Versions) -> Reclaimer<'s, Steps<'s>> {
        let steps = Steps { db: self, versions };
        Reclaimer::new(
            self.loader(),
            &self.pool,
            &self.table,
            &self.ids,
            &self.values,
            SIZE,
            steps,
        )
    }

    /// What the drops and the clears that are committed left and that is
    /// not reclaimed, and the records of unlinked flat pages, as the latest
    /// version has them, with the wait for them.
    pub fn left(&self, versions: &Versions) -> (Vec<drop::Reclaim>, Vec<FlatDrop>, Wait) {
        let reclaimer = self.reclaimer(versions);
        let guard = versions.guard();
        let latest = guard.latest();
        let pages = self.committed(&guard, latest);
        let mut merks = Vec::new();
        let mut drops = self.directory_at(&pages).drops();
        while let Some(dropped) = drops.next(&pages).expect("a dropped Merk") {
            merks.push(drop::Reclaim::new(dropped));
        }
        let flat = reclaimer.flat_drops(&self.flat_drops(), &guard);
        (
            merks,
            flat.expect("the records are read"),
            Wait::new(latest),
        )
    }

    /// Takes the reclaiming of `reclaim` on by at most `steps` page steps,
    /// in an operation of its own, with the directory of the latest
    /// version.
    pub fn reclaim(
        &self,
        versions: &Versions,
        reclaim: &mut drop::Reclaim,
        steps: usize,
    ) -> Progress {
        let reclaimer = self.reclaimer(versions);
        let guard = versions.guard();
        let directory = self.directory_at(&self.committed(&guard, guard.latest()));
        let columns = self.keyspaces().columns();
        (reclaimer.reclaim(reclaim, &directory, &columns, &guard, steps)).expect("a step")
    }

    /// Reclaims everything the committed drops left, as background work
    /// does: it reads the records at the latest version, waits until no
    /// reader of before can reach what they name, and then takes it out in
    /// page steps. What a step is refused for is left, and said.
    pub fn sweep(&self, versions: &Versions) -> Swept {
        let (merks, flat_drops, mut wait) = self.left(versions);
        let mut swept = Swept::default();
        if !wait.poll(versions) {
            swept.waiting = true;
            return swept;
        }
        for mut reclaim in merks {
            loop {
                match self.reclaim(versions, &mut reclaim, 64) {
                    Progress::Done => break,
                    Progress::More => {}
                    Progress::Blocked(busy) => {
                        swept.blocked = Some(busy);
                        break;
                    }
                }
            }
            swept.merks.push(reclaim.reclaimed().clone());
        }
        let (reclaimer, drops) = (self.reclaimer(versions), self.flat_drops());
        for drop in flat_drops {
            loop {
                let guard = versions.guard();
                let made = reclaimer.reclaim_flat(&drops, &drop, &guard, 256, &mut swept.flat);
                match made.expect("a step") {
                    Progress::Done => {
                        swept.flat_drops.push(drop);
                        break;
                    }
                    Progress::More => {}
                    Progress::Blocked(busy) => {
                        swept.blocked = Some(busy);
                        break;
                    }
                }
            }
        }
        swept
    }

    /// The subtree directory as the committed version `at` has it.
    pub fn directory_at<P: FlatPages>(&self, pages: &P) -> Directory {
        let tree = self.directory();
        Directory::load(pages, tree.root(), tree.config()).expect("the directory is loaded")
    }

    /// The root cell of every Merk in use that has a node, as the directory
    /// of the committed version `at` has it.
    pub fn roots(&self, versions: &Versions, at: CommitVersion) -> BTreeMap<Subtree, CellAddress> {
        let guard = versions.guard();
        let pages = self.committed(&guard, at);
        let directory = self.directory_at(&pages);
        let mut roots = BTreeMap::new();
        let mut entries = directory.entries();
        while let Some((prefix, entry)) = entries.next(&pages).expect("an entry") {
            if let Some(root) = entry.root().filter(|_| directory.is_live(&entry)) {
                roots.insert(Subtree::new(prefix), root.address);
            }
        }
        roots
    }

    /// The write path of this store under its `versions`.
    pub fn path<'s>(&'s self, versions: &'s Versions) -> WritePath<'s> {
        let store = Store {
            versions,
            loader: self.loader(),
            pool: &self.pool,
            ids: &self.ids,
            values: &self.values,
            commits: &self.commits,
            stats: &self.stats,
            directory: self.directory(),
            flat_drops: self.flat_drops(),
        };
        WritePath::new(
            store,
            self.keyspaces(),
            Layout::proposed(SIZE),
            InlinePolicy::DEFAULT,
        )
    }

    /// The read path of this store under its `versions`.
    pub fn reads<'s>(&'s self, versions: &'s Versions) -> ReadPath<'s> {
        let (keyspaces, directory) = (self.keyspaces(), self.directory());
        ReadPath::new(versions, self.loader()).with_records(&self.values, keyspaces, directory)
    }

    /// The committed flat pages at `at`, for the operation that holds
    /// `guard`.
    pub fn committed<'r, G: lignin_tree::OperationGuard>(
        &'r self,
        guard: &'r G,
        at: CommitVersion,
    ) -> Committed<'r, 'r, G> {
        Committed::new(self.loader(), &self.values, guard, at)
    }

    /// The commit path of this store under its `versions`.
    pub fn committer<'s>(&'s self, versions: &'s Versions) -> Committer<'s> {
        Committer::new(
            self.path(versions),
            self.write_back(versions),
            &self.table,
            &self.log,
            &self.committing,
        )
    }

    /// Commits `txn` (`crate::commit`), with the hash rules of these
    /// tests, and counts what the commit did.
    pub fn commit(&self, versions: &Versions, txn: &WriteTxn<'_>) -> Result<Outcome, TxnError> {
        let committer = self.committer(versions);
        let stale = txn.stats().stale_nodes as u64;
        match self.commit_as(&committer, txn, None) {
            Ok(None) => Ok(Outcome::Empty),
            Ok(Some(published)) => {
                assert_eq!(published.evaluated as u64, stale);
                Ok(Outcome::Committed(published.version))
            }
            Err(CommitError::Conflict) => Ok(Outcome::Conflict),
            Err(CommitError::ScopesOpen) => Err(TxnError::ScopeOpen),
            Err(CommitError::NotFinalized) => Err(TxnError::NotFinalized),
            // The commit was refused, and the transaction is as it was.
            Err(CommitError::Refused(error)) => Err(error),
            Err(error) => panic!("the commit: {error}"),
        }
    }

    /// Commits `txn` through `committer`, a commit path of this store, with
    /// `also` in the key set of the commit, and counts what the commit
    /// did: `None` for a commit that took no version.
    pub fn commit_as(
        &self,
        committer: &Committer<'_>,
        txn: &WriteTxn<'_>,
        also: Option<&KeySet>,
    ) -> Result<Option<Box<Published>>, CommitError> {
        self.counted(committer, |rules| {
            committer.commit_covering(txn, rules, also)
        })
    }

    /// Writes `batch` through `committer` with no transaction (RD7), and
    /// counts what its commit did.
    pub fn write_as(
        &self,
        committer: &Committer<'_>,
        batch: &Batch<'_>,
    ) -> Result<Option<Box<Published>>, CommitError> {
        self.counted(committer, |rules| committer.write(batch, rules))
    }

    /// Runs `commit` with the hash rules of these tests, and counts what
    /// the commit did.
    fn counted(
        &self,
        committer: &Committer<'_>,
        commit: impl FnOnce(&mut Rules<'_>) -> Result<crate::commit::Committed, CommitError>,
    ) -> Result<Option<Box<Published>>, CommitError> {
        let versions = committer.path().versions();
        let before = versions.latest();
        let committed = commit(&mut Rules(&self.counts.rule_calls));
        match committed {
            Ok(crate::commit::Committed::Empty) => Ok(None),
            Ok(crate::commit::Committed::Published(published)) => {
                self.count(versions, before, &published);
                Ok(Some(published))
            }
            Err(CommitError::NotDurable { published, source }) => {
                self.count(versions, before, &published);
                Err(CommitError::NotDurable { published, source })
            }
            Err(error) => {
                if matches!(error, CommitError::Conflict) {
                    self.counts.conflicts.fetch_add(1, Ordering::Relaxed);
                }
                Err(error)
            }
        }
    }

    /// Adds what the commit `published` did to the counts of the store.
    fn count(&self, versions: &Versions, before: CommitVersion, published: &Published) {
        let counts = &self.counts;
        let add = |count: &AtomicU64, by: usize| count.fetch_add(by as u64, Ordering::Relaxed);
        assert!(published.version > before, "a commit takes a later version");
        assert!(published.miscounted.is_none(), "a value is miscounted");
        let mut used = locked(&self.used);
        for page in &published.created {
            if !used.insert(*page) {
                add(&counts.ids_reused, 1);
            }
        }
        drop(used);
        add(&counts.pages_freed, published.freed.len());
        add(&counts.freed_unwritten, published.freed_unlinked);
        add(&counts.value_pages, published.value_pages);
        add(&counts.stale_nodes, published.evaluated);
        add(&counts.dropped, published.dropped.len());
        add(&counts.tree_copies, published.tree_copies);
        add(&counts.tree_copies_unchanged, published.unchanged_tree);
        add(&counts.commits, 1);
        if !published.dropped.is_empty() {
            // The handle the commit hands on is the directory of the
            // version that was published: it leaves out what the freshly
            // loaded one leaves out.
            let guard = versions.guard();
            let pages = self.committed(&guard, published.version);
            let fresh = self.directory_at(&pages);
            let mut entries = fresh.entries();
            while let Some((_, entry)) = entries.next(&pages).expect("an entry") {
                assert_eq!(published.directory.is_live(&entry), fresh.is_live(&entry));
            }
        }
    }

    /// How many page ids are in each state.
    pub fn id_states(&self) -> BTreeMap<String, usize> {
        let mut states = BTreeMap::new();
        let (mut from, mut out) = (PageId::MIN, Vec::new());
        loop {
            out.clear();
            let next = self.ids.scan(from, &mut out);
            for (_, state) in &out {
                *states.entry(format!("{state:?}")).or_default() += 1;
            }
            match next {
                Some(next) => from = next,
                None => return states,
            }
        }
    }

    /// The ids that an open transaction holds.
    pub fn private_ids(&self) -> usize {
        *self
            .id_states()
            .get(&format!("{:?}", PageState::Private))
            .unwrap_or(&0)
    }

    /// Checks the committed version `at` with the physical invariant
    /// checker (`crate::verify`), as a reader of that version: the pages,
    /// the Merks from their roots, the directory against the cells, the
    /// flat keyspaces and the values, with the hashes recomputed by the
    /// rules of the store ([`Self::check_hashes_by`]). The version is sound
    /// ([`Self::unsound`]) and has no cell that nothing reaches. Returns the
    /// keys of every Merk in use that has a node, in order.
    ///
    /// The caller runs no reclaimer meanwhile. What is of the latest moment
    /// and of no version, the page ids, the counts of live values and the
    /// page table, is checked where the store is still: when it is dropped,
    /// and by [`Self::verify_still`].
    pub fn check_trees(
        &self,
        versions: &Versions,
        at: CommitVersion,
    ) -> BTreeMap<Subtree, Vec<Vec<u8>>> {
        let report = self.verify_by_own_rules(at, Scope::Reader);
        assert!(
            self.unsound(&report).is_empty() && report.more_findings == 0,
            "version {at} is not sound:\n{report}"
        );
        assert_eq!(
            report.unreached.cells, 0,
            "a cell that nothing reaches at {at}:\n{report}"
        );
        let census = &report.census;
        *locked(&self.counts.census) =
            (census.merks, census.left_over_entries, census.dropped_merks);
        // The pointers of the hosts: those the directory names, and those
        // of the Merks that stand alone ([`Self::unsound`]).
        let alone = (report.findings.iter()).filter(|finding| finding.rule == Rule::HostUnnamed);
        let hosted = census.hosted_roots + alone.count() as u64;
        self.counts.hosted.store(hosted, Ordering::Relaxed);

        let (loader, lease, mut buffers) = (self.loader(), Lease::take(), LoadBuffers::new());
        let mut keys = BTreeMap::new();
        for (subtree, root) in self.roots(versions, at) {
            let (mut order, mut next) = (Vec::new(), vec![root]);
            while let Some(address) = next.pop() {
                let frame = (loader.get(address.page(), at, &mut buffers)).expect("a page");
                let page = NodePage::open(&frame, lease).expect("a tree page");
                let node = page.node(address.slot()).expect("a cell");
                order.push(node.key().to_vec());
                let links = [Side::Left, Side::Right].map(|side| node.link(side));
                next.extend(links.into_iter().flatten().map(|link| link.child));
            }
            order.sort();
            keys.insert(subtree, order);
        }
        keys
    }

    /// The oldest pinned version, as a reclaimer is told.
    pub fn oldest_pin(&self, versions: &Versions) -> CommitVersion {
        versions.oldest_pin()
    }

    /// Runs the physical invariant checker (`crate::verify`) on the
    /// version `at` of this store, with `rules` to recompute the hashes by,
    /// or none of them with `None`.
    ///
    /// With [`Scope::Still`] the caller vouches that nothing writes to the
    /// store meanwhile and that `at` is its latest version.
    pub fn verify(
        &self,
        at: CommitVersion,
        scope: Scope,
        rules: Option<&mut dyn HashRules>,
    ) -> Report {
        self.verify_with(at, scope, rules, Some(self.flat_drops()))
    }

    /// As [`Self::verify`], with the keyspace of the pages that lazy range
    /// deletes left as the store is said to keep it: `None` is a store
    /// that does not name it to the checker.
    pub fn verify_with(
        &self,
        at: CommitVersion,
        scope: Scope,
        rules: Option<&mut dyn HashRules>,
        flat_drops: Option<FlatDrops>,
    ) -> Report {
        let columns = self.keyspaces().columns();
        let store = verify::Store {
            loader: self.loader(),
            table: &self.table,
            segments: &self.segments,
            values: &self.values,
            page_size: SIZE,
            ids: Some(&self.ids),
            grove: Grove::Directory {
                tree: self.directory(),
                columns: &columns,
            },
            flat_drops,
            flat: &[],
            values_counted: true,
        };
        verify::check(&store, &AtRest, Options::new(at, scope), rules)
    }

    /// Runs the checker on the latest version of this store, which is
    /// still, with the hashes recomputed by the rules of the store.
    pub fn verify_still(&self) -> Report {
        self.verify_by_own_rules(self.commits.newest(), Scope::Still)
    }

    /// Runs the checker with the rules that the hashes of this store's
    /// nodes are made by: those that were given ([`Self::check_hashes_by`]),
    /// or none ([`Self::unhashed`]). With none given, the tests of this
    /// crate use the rules of their model ([`Rules`]), and the tests of
    /// another crate, whose nodes have the hashes of their own rules, have
    /// no hash recomputed.
    fn verify_by_own_rules(&self, at: CommitVersion, scope: Scope) -> Report {
        let calls = AtomicU64::new(0);
        let mut model = Rules(&calls);
        let mut given = locked(&self.hash_rules);
        let rules: Option<&mut dyn HashRules> = match given.as_mut() {
            _ if self.unhashed.load(Ordering::Relaxed) => None,
            Some(rules) => Some(rules.as_mut()),
            None if cfg!(test) => Some(&mut model),
            None => None,
        };
        self.verify(at, scope, rules)
    }

    /// Leaves the store out of the check that its drop makes: for a test
    /// that damages its store, or leaves it in the middle of something.
    pub fn unchecked(&self) {
        self.unchecked.store(true, Ordering::Relaxed);
    }

    /// The rules that the check at the drop of this store recomputes the
    /// hashes by: for a store whose nodes were not written with the hashes
    /// of the model of these tests ([`Rules`]).
    pub fn check_hashes_by(&self, rules: Box<dyn HashRules + Send>) {
        *locked(&self.hash_rules) = Some(rules);
    }

    /// Has the check at the drop of this store recompute no hash: for a
    /// test that states hashes which no rule makes.
    pub fn unhashed(&self) {
        self.unhashed.store(true, Ordering::Relaxed);
    }

    /// The findings of `report` that say this store is unsound.
    ///
    /// One finding is the fixture's own doing and is left out. The Merks
    /// that every store of the tests has from its first commit stand alone
    /// ([`ENTERED`]): their entries name no host, and tests let a node host
    /// one of them all the same, which an engine never does
    /// ([`Rule::HostUnnamed`]).
    pub fn unsound<'r>(&self, report: &'r Report) -> Vec<&'r Finding> {
        let stands_alone = |finding: &Finding| match finding.place {
            Place::Merk(prefix) if finding.rule == Rule::HostUnnamed => {
                prefix[0] < ENTERED && prefix == [prefix[0]; 32]
            }
            _ => false,
        };
        (report.findings.iter())
            .filter(|finding| !stands_alone(finding))
            .collect()
    }
}

/// The physical invariant checker runs after every test, in a debug build:
/// a store that is dropped is still, and is checked at its latest version.
///
/// It is left out for a store that a test took out ([`Db::unchecked`]), for
/// one on files that the test chose ([`Db::on`] with no directory to keep),
/// and where the test is failing already. The hashes are recomputed by the
/// rules of the store ([`Db::check_hashes_by`], [`Db::unhashed`]). What
/// counts as unsound is [`Db::unsound`].
#[cfg(debug_assertions)]
impl Drop for Db {
    fn drop(&mut self) {
        if std::thread::panicking() || self.unchecked.load(Ordering::Relaxed) {
            return;
        }
        if self.keyspaces.get().is_none() || self.directory.get().is_none() {
            return;
        }
        let at = self.commits.newest();
        let report = self.verify_by_own_rules(at, Scope::Still);
        assert!(
            self.unsound(&report).is_empty() && report.more_findings == 0,
            "the store of a test is left unsound at version {at}:\n{report}"
        );
    }
}

/// Numbers drawn from a seed.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}
