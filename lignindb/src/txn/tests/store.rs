//! A page store on disk with what a write transaction needs around it that
//! is not built yet: the open of the engine (TXN-07), reduced to making the
//! stores, the log and the first commit, and the check of the committed
//! tree pages (VER-07). The subtree directory is the real one
//! (`lignin_tree::directory`), and so is the commit (`crate::commit`).

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    },
};

use lignin_format::{
    manifest::Manifest,
    tree_page::{Hash, Side},
    wal::GroupWriter,
    CellAddress, CommitVersion, PageId, PageSize,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    alloc::{PageAllocator, PageState, Pins},
    cache::{BufferPool, OwnerId, PoolConfig},
    flush::{FlushConfig, FlushError, FlushState, Stores, WriteBack},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore},
    stats::Stats,
    table::{PageTable, TableCheckpoints},
    values::ValueStore,
    wal::{LogOptions, LogReader, LogStore},
};
use lignin_tree::{
    directory::Directory,
    drop::FlatDrops,
    flat::{Committed, FlatConfig, FlatPages, FlatTree, Private},
    fragment::Layout,
    hash::{HashRules, Refusal},
    node::{Lease, NodePage, NodeRef},
    value::InlinePolicy,
};

use crate::{
    commit::{CommitError, CommitState, Committer, Published},
    snapshot::{ReadPath, Reclaim, Versions},
    txn::{path::Store, Batch, Commits, KeySet, Keyspaces, Subtree, TxnError, WritePath, WriteTxn},
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
    _dir: Option<tempfile::TempDir>,
}

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

impl Db {
    /// An empty store with a pool of `frames` frames, in a directory of its
    /// own. Its first commit makes the flat keyspaces and the directory.
    pub fn new(frames: usize) -> (Arc<Self>, Versions) {
        // In memory where there is such a place: the syncs of a store's
        // first files are most of what its tests would wait for.
        let memory = Path::new("/dev/shm");
        let dir = match memory.is_dir() {
            true => tempfile::tempdir_in(memory),
            false => tempfile::tempdir(),
        };
        let dir = dir.expect("a directory");
        let path = dir.path().to_path_buf();
        Self::on(
            Arc::new(StdVfs),
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
        ReadPath::new(versions, self.loader())
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

    /// Checks the committed tree pages at `at`, as the physical invariant
    /// checker will (`tree-layout.md` section 16, VER-07): the directory
    /// passes its own check, every Merk in use that has a root is a search
    /// tree of cells that are each reached once, its entry names the key of
    /// its root, every stored address names a cell, the key length in a link
    /// is its child's, the fragment-root marks are what the pointers imply,
    /// and no page holds a cell that nothing reaches but what a dropped Merk
    /// left. Returns the keys of every Merk that has a node, in order.
    pub fn check_trees(
        &self,
        versions: &Versions,
        at: CommitVersion,
    ) -> BTreeMap<Subtree, Vec<Vec<u8>>> {
        let guard = versions.guard();
        let pages = self.committed(&guard, at);
        let loader = self.loader();
        let lease = Lease::take();
        let mut buffers = LoadBuffers::new();
        let mut reached: HashMap<CellAddress, Subtree> = HashMap::new();
        // Every pointer a cell stores: from, to.
        let mut pointers: Vec<(CellAddress, CellAddress)> = Vec::new();
        let mut keys = BTreeMap::new();
        let directory = self.directory_at(&pages);
        let census = directory.check(&pages).expect("the directory is sound");
        *locked(&self.counts.census) = (census.live, census.left_over, census.dropped);
        let mut all = Vec::new();
        let mut entries = directory.entries();
        while let Some((prefix, entry)) = entries.next(&pages).expect("an entry") {
            if let Some(root) = entry.root().filter(|_| directory.is_live(&entry)) {
                all.push((Subtree::new(prefix), root.address, root.key.to_vec()));
            }
        }
        let root_of: HashMap<CellAddress, Subtree> = all
            .iter()
            .map(|(subtree, root, _)| (*root, *subtree))
            .collect();
        assert_eq!(root_of.len(), all.len(), "two Merks with one root");
        // What the dropped Merks left: their cells stay until they are
        // reclaimed, and nothing in use reaches them.
        let mut left: Vec<CellAddress> = Vec::new();
        let mut drops = directory.drops();
        while let Some(dropped) = drops.next(&pages).expect("a dropped Merk") {
            left.extend(dropped.root);
        }

        let live = (all.iter()).map(|(subtree, root, key)| (*subtree, *root, Some(key)));
        let gone = left
            .iter()
            .map(|root| (Subtree::new([0xdd; 32]), *root, None));
        for (subtree, root, root_key) in live.chain(gone) {
            let (subtree, root) = (&subtree, &root);
            let mut order = Vec::new();
            // A cell with the bounds its key lies between.
            let mut stack = vec![(*root, None::<Vec<u8>>, None::<Vec<u8>>, None::<u8>)];
            while let Some((address, low, high, key_len)) = stack.pop() {
                let frame = loader
                    .get(address.page(), at, &mut buffers)
                    .unwrap_or_else(|error| panic!("{address} of {subtree} at {at}: {error}"));
                let page = NodePage::open(&frame, lease).expect("a tree page");
                let node = page
                    .node(address.slot())
                    .unwrap_or_else(|| panic!("no cell at {address} of {subtree}"));
                assert!(
                    reached.insert(address, *subtree).is_none(),
                    "{address} is reached twice"
                );
                let key = node.key().to_vec();
                assert!(
                    low.as_ref().is_none_or(|low| *low < key),
                    "order in {subtree}"
                );
                assert!(
                    high.as_ref().is_none_or(|high| key < *high),
                    "order in {subtree}"
                );
                if let Some(len) = key_len {
                    assert_eq!(len as usize, key.len(), "the key length in a link");
                }
                if let Some(root_key) = root_key.filter(|_| address == *root) {
                    assert_eq!(&key, root_key, "the root key in the entry of {subtree}");
                }
                for (side, low, high) in [
                    (Side::Left, low.clone(), Some(key.clone())),
                    (Side::Right, Some(key.clone()), high.clone()),
                ] {
                    if let Some(link) = node.link(side) {
                        pointers.push((address, link.child));
                        stack.push((link.child, low, high, Some(link.child_key_len)));
                    }
                }
                for hosted in node.hosted_roots().flatten() {
                    match root_key {
                        Some(_) => assert!(
                            root_of.contains_key(&hosted),
                            "a host in {subtree} points at {hosted}, which is the root of no Merk"
                        ),
                        // What a dropped Merk left has the Merks below it.
                        None => stack.push((hosted, None, None, None)),
                    }
                    pointers.push((address, hosted));
                }
                order.push(key);
            }
            order.sort();
            if root_key.is_some() {
                keys.insert(*subtree, order);
            }
        }

        let hosted = pointers.iter().filter(|(_, to)| root_of.contains_key(to));
        self.counts
            .hosted
            .store(hosted.count() as u64, Ordering::Relaxed);
        // The marks, and the cells that nothing reaches and no dropped Merk
        // left.
        let named: BTreeSet<CellAddress> = pointers
            .iter()
            .filter(|(from, to)| from.page() == to.page())
            .map(|(_, to)| *to)
            .collect();
        // Every tree page the version has, reached or not: a page that was
        // freed or made later is not loaded at it, and a flat page is no
        // tree page.
        let visited: BTreeSet<PageId> = reached.keys().map(|address| address.page()).collect();
        let pages: Vec<PageId> = locked(&self.used).iter().copied().collect();
        let mut checked = BTreeSet::new();
        for id in pages {
            let Ok(frame) = loader.get(id, at, &mut buffers) else {
                continue;
            };
            let Ok(page) = NodePage::open(&frame, lease) else {
                continue;
            };
            checked.insert(id);
            for node in page.nodes() {
                let address = node.address();
                assert!(
                    reached.contains_key(&address),
                    "the cell at {address} is in no Merk at {at}"
                );
                assert_eq!(
                    node.is_fragment_root(),
                    !named.contains(&address),
                    "the fragment-root mark of {address}"
                );
            }
        }
        assert!(
            visited.is_subset(&checked),
            "a page that a Merk reaches is not among the pages of the store"
        );
        keys
    }

    /// The oldest pinned version, as a reclaimer is told.
    pub fn oldest_pin(&self, versions: &Versions) -> CommitVersion {
        versions.oldest_pin()
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
