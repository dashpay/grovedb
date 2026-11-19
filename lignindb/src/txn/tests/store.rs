//! A page store on disk with what a write transaction needs around it that
//! is not built yet: the commit (TXN-05), reduced to the steps that install
//! a transaction's frames and publish its version, the subtree directory
//! (TREE-05), reduced to one flat tree of root addresses, and the check of
//! the committed tree pages (VER-07).

#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    },
};

use lignin_format::{
    manifest::Manifest, tree_page::Side, CellAddress, CommitVersion, PageId, PageSize,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    alloc::{PageAllocator, PageState, Pins},
    cache::{BufferPool, OwnerId, PoolConfig},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore},
    table::{Link, LinkError, PageTable},
    values::ValueStore,
};
use lignin_tree::{
    flat::{Committed, FlatConfig, FlatPages, FlatTree, Private},
    fragment::Layout,
    node::{Lease, NodePage},
};

use crate::{
    snapshot::{ReadPath, Reclaim, Versions},
    txn::{
        path::Store, txn::Sealed, Commits, Directory, DirectoryError, FlatWriter, Keyspaces,
        Subtree, TxnError, WritePath, WriteTxn,
    },
};

pub const SIZE: PageSize = PageSize::MIN;

pub fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The subtree directory of the tests: one flat tree that holds, under the
/// 32 bytes of a subtree, the address of the root cell of its Merk.
#[derive(Debug)]
pub struct Roots {
    tree: OnceLock<FlatTree>,
    /// How many entries were written.
    pub writes: AtomicU64,
    /// How many times a root that went along with its host was looked for.
    pub moves: AtomicU64,
    /// How many more writes of an entry succeed before one fails;
    /// `u64::MAX` for all of them.
    pub writes_left: AtomicU64,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            tree: OnceLock::new(),
            writes: AtomicU64::new(0),
            moves: AtomicU64::new(0),
            writes_left: AtomicU64::new(u64::MAX),
        }
    }
}

impl Roots {
    pub fn tree(&self) -> FlatTree {
        *self
            .tree
            .get()
            .expect("the directory is made with the store")
    }

    fn read<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, DirectoryError> {
        let mut value = Vec::new();
        if !self.tree().get(pages, subtree.as_bytes(), &mut value)? {
            return Ok(None);
        }
        let bytes: [u8; CellAddress::ENCODED_LEN] =
            value.as_slice().try_into().map_err(|_| "a root address")?;
        Ok(CellAddress::decode(bytes)?)
    }

    /// Every Merk that has a root at the version `pages` reads, with its
    /// root.
    pub fn all(&self, pages: &Committed<'_>) -> Vec<(Subtree, CellAddress)> {
        let mut found = Vec::new();
        let mut cursor = self.tree().cursor();
        cursor.seek_to_first(pages).expect("the directory is read");
        while let Some(key) = cursor.key() {
            let subtree = Subtree::new(key.try_into().expect("32 bytes"));
            let mut value = Vec::new();
            assert!(cursor.value(pages, &mut value).expect("a value"));
            let bytes = value.as_slice().try_into().expect("a root address");
            let root = CellAddress::decode(bytes).expect("an address");
            found.push((subtree, root.expect("a root")));
            cursor.next(pages).expect("the directory is read");
        }
        found
    }
}

impl Directory for Roots {
    fn committed_root(
        &self,
        pages: &Committed<'_>,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, DirectoryError> {
        self.read(pages, subtree)
    }

    fn root(
        &self,
        pages: &FlatWriter<'_>,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, DirectoryError> {
        self.read(pages, subtree)
    }

    fn set_root(
        &self,
        pages: &mut FlatWriter<'_>,
        subtree: &Subtree,
        root: Option<CellAddress>,
    ) -> Result<(), DirectoryError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        match self.writes_left.load(Ordering::Relaxed) {
            u64::MAX => {}
            0 => {
                self.writes_left.store(u64::MAX, Ordering::Relaxed);
                return Err("the directory was told to fail".into());
            }
            left => self.writes_left.store(left - 1, Ordering::Relaxed),
        }
        match root {
            Some(root) => self.tree().put(pages, subtree.as_bytes(), &root.encode())?,
            None => {
                self.tree().delete(pages, subtree.as_bytes())?;
            }
        }
        Ok(())
    }

    fn moved(
        &self,
        pages: &mut FlatWriter<'_>,
        from: CellAddress,
        to: CellAddress,
    ) -> Result<(), DirectoryError> {
        self.moves.fetch_add(1, Ordering::Relaxed);
        let mut cursor = self.tree().cursor();
        cursor.seek_to_first(&*pages)?;
        let mut value = Vec::new();
        while let Some(key) = cursor.key() {
            value.clear();
            cursor.value(&*pages, &mut value)?;
            if value == from.encode() {
                let key = key.to_vec();
                self.tree().put(pages, &key, &to.encode())?;
                return Ok(());
            }
            cursor.next(&*pages)?;
        }
        Ok(())
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
    pub roots: Roots,
    pub counts: Counts,
    keyspaces: OnceLock<Keyspaces>,
    commit_lock: Mutex<()>,
    used: Mutex<BTreeSet<PageId>>,
    _dir: tempfile::TempDir,
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
    /// An empty store with a pool of `frames` frames. Its first commit
    /// makes the flat keyspaces and the directory.
    pub fn new(frames: usize) -> (Arc<Self>, Versions) {
        let dir = tempfile::tempdir().expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
        let manifest = ManifestStore::create(vfs, dir.path(), empty).expect("a manifest");
        let segments = SegmentStore::open(&manifest).expect("the segments");
        let values = ValueStore::open(&manifest, &[]).expect("the value log");
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
            roots: Roots::default(),
            counts: Counts::default(),
            keyspaces: OnceLock::new(),
            commit_lock: Mutex::new(()),
            used: Mutex::new(BTreeSet::new()),
            _dir: dir,
        });
        let versions = Versions::with_reclaim(CommitVersion::EMPTY, Box::new(Hook(db.clone())));
        db.make_keyspaces(&versions);
        (db, versions)
    }

    pub fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, SIZE).expect("a loader")
    }

    pub fn config(&self) -> FlatConfig {
        FlatConfig::new(SIZE)
    }

    /// The first commit: five empty flat trees.
    fn make_keyspaces(&self, versions: &Versions) {
        let base = Committed::new(self.loader(), &self.values, CommitVersion::EMPTY);
        let mut pages = Private::new(base, &self.pool, OwnerId::new(u64::MAX), &self.ids);
        let mut make = || FlatTree::create(&mut pages, self.config()).expect("a tree");
        let keyspaces = Keyspaces {
            data: make(),
            aux: make(),
            roots: make(),
            meta: make(),
        };
        let directory = make();
        self.keyspaces.set(keyspaces).expect("made once");
        self.roots.tree.set(directory).expect("made once");
        let commit = version(1);
        for (copy, created) in pages.into_parts().written {
            assert!(created);
            let page = copy.page();
            drop(self.pool.install(&copy, commit).expect("it installs"));
            self.ids.publish(page).expect("it is private");
            locked(&self.used).insert(page);
        }
        self.commits.record(commit, Default::default());
        versions.publish(commit).expect("the first version");
    }

    pub fn keyspaces(&self) -> Keyspaces {
        *self.keyspaces.get().expect("made with the store")
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
            directory: &self.roots,
        };
        WritePath::new(store, self.keyspaces(), Layout::proposed(SIZE))
    }

    /// The read path of this store under its `versions`.
    pub fn reads<'s>(&'s self, versions: &'s Versions) -> ReadPath<'s> {
        ReadPath::new(versions, self.loader())
    }

    /// The committed flat pages at `at`.
    pub fn committed(&self, at: CommitVersion) -> Committed<'_> {
        Committed::new(self.loader(), &self.values, at)
    }

    /// Commits `txn`: the conflict rule, the rebase, the install and the
    /// publication (CM3 steps 1 to 4, 7 and 8). It writes no log.
    pub fn commit(&self, versions: &Versions, txn: &WriteTxn<'_>) -> Result<Outcome, TxnError> {
        let lock = locked(&self.commit_lock);
        let parts = match txn.seal()? {
            Sealed::Empty => return Ok(Outcome::Empty),
            Sealed::Conflict => {
                self.counts.conflicts.fetch_add(1, Ordering::Relaxed);
                return Ok(Outcome::Conflict);
            }
            Sealed::Ready(parts) => *parts,
        };
        assert_eq!(parts.base, versions.latest(), "the frames are rebased");
        let commit = parts.base.next().expect("a next version");
        let mut created = Vec::new();
        let mut freed = Vec::new();
        let mut unused = Vec::new();

        let tree = parts.tree.into_parts();
        for (page, copy) in tree.written {
            if copy.is_unchanged() {
                self.pool.discard(&copy.frame).expect("it is discarded");
                continue;
            }
            drop(self.pool.install(&copy.frame, commit).expect("it installs"));
            if copy.created {
                created.push(page);
            }
        }
        for (page, copy) in tree.freed {
            self.pool.discard(&copy).expect("it is discarded");
            freed.push(page);
        }
        unused.extend(tree.unused);

        let flat = parts.flat.into_parts();
        for (copy, made) in flat.written {
            let page = copy.page();
            drop(self.pool.install(&copy, commit).expect("it installs"));
            if made {
                created.push(page);
            }
        }
        for copy in flat.freed {
            self.pool.discard(&copy).expect("it is discarded");
            freed.push(copy.page());
        }
        unused.extend(flat.unused);

        let mut used = locked(&self.used);
        for page in created {
            self.ids.publish(page).expect("it is private");
            if !used.insert(page) {
                self.counts.ids_reused.fetch_add(1, Ordering::Relaxed);
            }
        }
        drop(used);
        for page in freed {
            // A page that was made under an id that had been freed, and
            // that is freed again before write-back gave it a link, has
            // the earlier free link as its newest: the table takes no
            // second one.
            match self.table.push(page, Link::free(commit)) {
                Ok(()) => {}
                Err(LinkError::AlreadyFree) => {
                    self.counts.freed_unwritten.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => panic!("the free link of {page}: {error}"),
            }
            self.pool.free(page, commit).expect("it is freed");
            self.ids.free(page, commit).expect("it is live");
            self.counts.pages_freed.fetch_add(1, Ordering::Relaxed);
        }
        for page in unused {
            self.ids.release(page).expect("it is private");
        }
        // The key set is recorded before the version is published, so that
        // whoever reads the version finds it (CM3 step 8).
        self.commits.record(commit, parts.keys);
        versions.publish(commit).expect("the next version");
        drop(parts.pin);
        drop(lock);
        self.counts.commits.fetch_add(1, Ordering::Relaxed);
        versions.reclaim();
        Ok(Outcome::Committed(commit))
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
    /// checker will (`tree-layout.md` section 16, VER-07): every Merk of the
    /// directory is a search tree of cells that are each reached once, every
    /// stored address names a cell, the key length in a link is its child's,
    /// the fragment-root marks are what the pointers imply, and no page
    /// holds a cell that nothing reaches. Returns the keys of every Merk, in
    /// order.
    pub fn check_trees(&self, at: CommitVersion) -> BTreeMap<Subtree, Vec<Vec<u8>>> {
        let pages = self.committed(at);
        let loader = self.loader();
        let lease = Lease::take();
        let mut buffers = LoadBuffers::new();
        let mut reached: HashMap<CellAddress, Subtree> = HashMap::new();
        // Every pointer a cell stores: from, to.
        let mut pointers: Vec<(CellAddress, CellAddress)> = Vec::new();
        let mut keys = BTreeMap::new();
        let all = self.roots.all(&pages);
        let root_of: HashMap<CellAddress, Subtree> = all
            .iter()
            .map(|(subtree, root)| (*root, *subtree))
            .collect();
        assert_eq!(root_of.len(), all.len(), "two Merks with one root");

        for (subtree, root) in &all {
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
                    assert!(
                        root_of.contains_key(&hosted),
                        "a host in {subtree} points at {hosted}, which is the root of no Merk"
                    );
                    pointers.push((address, hosted));
                }
                order.push(key);
            }
            order.sort();
            keys.insert(*subtree, order);
        }

        let hosted = pointers.iter().filter(|(_, to)| root_of.contains_key(to));
        self.counts
            .hosted
            .store(hosted.count() as u64, Ordering::Relaxed);
        // The marks, and the cells nothing reaches.
        let named: BTreeSet<CellAddress> = pointers
            .iter()
            .filter(|(from, to)| from.page() == to.page())
            .map(|(_, to)| *to)
            .collect();
        let visited: BTreeSet<PageId> = reached.keys().map(|address| address.page()).collect();
        for id in visited {
            let frame = loader.get(id, at, &mut buffers).expect("it was read");
            let page = NodePage::open(&frame, lease).expect("a tree page");
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
