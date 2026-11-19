//! A page store on disk with what a commit, write-back and a cleaner do to
//! it, for the tests and the benchmark of snapshot reads. The tasks that
//! build those actors are not merged yet (TXN-05, STORE-06, STORE-08), so
//! each is a few calls on the store's own structures, in the order their
//! contracts give.
//!
//! A page is a tree page with one cell, whose value says which page it is
//! and which commit wrote it. [`Store::history`] is the model: the pages of
//! every version, with the commit that wrote each.

#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

use lignin_format::{
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellContent, CellValue, TreePage, TreePageMut},
    CommitVersion, PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    alloc::{Epoch, PageAllocator, Pins},
    cache::{BufferPool, OwnerId, PoolConfig},
    load::{LoadBuffers, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
};
use lignindb::snapshot::{ReadPath, Reclaim, Versions};

pub const SIZE: PageSize = PageSize::MIN;

pub fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

pub fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pages of one version: for each page id, the commit that wrote the
/// page as that version has it.
pub type Pages = BTreeMap<u32, u64>;

/// Writes into `frame` the page `id` as the commit `born` leaves it.
fn write_page(frame: &mut [u8], id: PageId, born: u64) {
    let mut value = [0u8; 96];
    value[..4].copy_from_slice(&id.get().to_le_bytes());
    value[4..12].copy_from_slice(&born.to_le_bytes());
    let mut written = TreePageMut::init(frame, SIZE, id, 0).expect("a page");
    written
        .insert(
            SlotId::MIN,
            &CellContent {
                key: b"page",
                feature: &[],
                value_hash: &[1; 32],
                kv_hash: &[2; 32],
                left: None,
                right: None,
                hosted_roots: &[],
                value: CellValue::Inline(&value),
                fragment_root: true,
            },
        )
        .expect("the cell fits");
}

/// The page id and the commit that a frame's page says it is of.
pub fn read_page(frame: &[u8]) -> (u32, u64) {
    let tree = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
    let cell = tree.cell(SlotId::MIN).expect("the cell");
    let CellValue::Inline(value) = cell.value() else {
        panic!("the value is inline");
    };
    let id = u32::from_le_bytes(value[..4].try_into().expect("four bytes"));
    let born = u64::from_le_bytes(value[4..12].try_into().expect("eight bytes"));
    (id, born)
}

/// What one commit does.
#[derive(Debug, Default, Clone)]
pub struct Changes {
    /// Pages of the latest version that it writes again.
    pub write: Vec<PageId>,
    /// How many pages it creates.
    pub create: usize,
    /// Pages of the latest version that it frees.
    pub free: Vec<PageId>,
}

impl Changes {
    /// A commit that creates `count` pages and does nothing else.
    pub fn create(count: usize) -> Self {
        Self {
            create: count,
            ..Self::default()
        }
    }
}

#[derive(Debug, Default)]
pub struct Counts {
    pub commits: AtomicU64,
    /// Pages created under an id that named another page before.
    pub ids_reused: AtomicU64,
    pub ids_released: AtomicU64,
    pub links_trimmed: AtomicU64,
    pub reclaims: AtomicU64,
    /// Times a reader said that a reclaim is due.
    pub wakes: AtomicU64,
}

pub struct Store {
    pub segments: SegmentStore,
    pub table: PageTable,
    pub pool: BufferPool,
    pub ids: PageAllocator,
    pub counts: Counts,
    /// The pages of each version, by its number, or of the latest version
    /// alone.
    history: Mutex<Vec<Arc<Pages>>>,
    keep_history: bool,
    writer: Mutex<SegmentWriter>,
    /// The cleaner's part: an oldest pin that was answered, with the epoch
    /// that was started after it. The chains are trimmed to it once every
    /// guard of an earlier epoch is gone (PN6).
    trims: Mutex<Vec<(CommitVersion, Epoch)>>,
    used: Mutex<Vec<u32>>,
    _dir: tempfile::TempDir,
}

impl Store {
    /// An empty store with a pool of `frames` frames, which keeps the pages
    /// of every version for [`Self::pages_at`].
    pub fn new(frames: usize) -> Arc<Self> {
        Self::build(frames, true)
    }

    /// As [`Self::new`], for a run of any length: only the pages of the
    /// latest version are kept.
    pub fn without_history(frames: usize) -> Arc<Self> {
        Self::build(frames, false)
    }

    fn build(frames: usize, keep_history: bool) -> Arc<Self> {
        let dir = tempfile::tempdir().expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest = ManifestStore::create(vfs, dir.path(), empty).expect("a manifest");
        let segments = SegmentStore::open(&manifest).expect("the segments");
        let writer = segments.create().expect("a segment");
        segments
            .commit(&mut manifest)
            .expect("the segment is named");
        let pool = BufferPool::new(PoolConfig {
            page_size: SIZE,
            capacity: frames * (SIZE.get() + 512),
        })
        .expect("a pool");
        Arc::new(Self {
            segments,
            table: PageTable::new(),
            pool,
            ids: PageAllocator::new(),
            counts: Counts::default(),
            history: Mutex::new(vec![Arc::new(Pages::new())]),
            writer: Mutex::new(writer),
            trims: Mutex::new(Vec::new()),
            keep_history,
            used: Mutex::new(Vec::new()),
            _dir: dir,
        })
    }

    /// The versions of this store, which ask it to reclaim.
    pub fn versions(self: &Arc<Self>) -> Versions {
        Versions::with_reclaim(CommitVersion::EMPTY, Box::new(Hook(self.clone())))
    }

    pub fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, SIZE).expect("a loader")
    }

    /// The read path of this store under its `versions`.
    pub fn reads<'s>(&'s self, versions: &'s Versions) -> ReadPath<'s> {
        ReadPath::new(versions, self.loader())
    }

    /// The pages of `version`. A store without history has those of the
    /// latest version only.
    pub fn pages_at(&self, version: CommitVersion) -> Arc<Pages> {
        let history = locked(&self.history);
        let at = if self.keep_history {
            version.get() as usize
        } else {
            0
        };
        history[at].clone()
    }

    /// One commit, from its install to its publication (CM3 steps 7 and
    /// 8), and the reclaim that follows it. One thread commits.
    pub fn commit(&self, versions: &Versions, changes: &Changes) -> CommitVersion {
        let guard = versions.guard();
        let latest = guard.latest();
        let commit = latest.next().expect("a next version");
        let owner = OwnerId::new(commit.get());
        let mut pages = Pages::clone(&self.pages_at(latest));
        let mut buffers = LoadBuffers::new();
        let mut copies = Vec::new();

        for &id in &changes.write {
            let copy = match self.pool.make_private(owner, id, latest) {
                Ok(copy) => copy,
                Err(_) => {
                    let loaded = self
                        .loader()
                        .get(id, latest, &mut buffers)
                        .expect("it loads");
                    let copy = self.pool.make_private(owner, id, latest);
                    drop(loaded);
                    copy.expect("the page is in the pool")
                }
            };
            write_page(&mut copy.write().expect("no other latch"), id, commit.get());
            pages.insert(id.get(), commit.get());
            copies.push(copy);
        }
        let mut created = Vec::new();
        for _ in 0..changes.create {
            let id = self.ids.allocate().expect("an id");
            let copy = self.pool.new_private(owner, id).expect("a new copy");
            write_page(&mut copy.write().expect("no other latch"), id, commit.get());
            pages.insert(id.get(), commit.get());
            copies.push(copy);
            created.push(id);
        }
        for id in &changes.free {
            pages.remove(&id.get());
        }
        let mut history = locked(&self.history);
        if !self.keep_history {
            history.clear();
        }
        history.push(Arc::new(pages));
        drop(history);

        for copy in &copies {
            drop(self.pool.install(copy, commit).expect("it installs"));
        }
        drop(copies);
        drop(guard);
        let mut used = locked(&self.used);
        for id in created {
            self.ids.publish(id).expect("it is private");
            if used.contains(&id.get()) {
                self.counts.ids_reused.fetch_add(1, Ordering::Relaxed);
            } else {
                used.push(id.get());
            }
        }
        drop(used);
        for &id in &changes.free {
            self.table
                .push(id, Link::free(commit))
                .expect("a free link");
            self.pool.free(id, commit).expect("it is freed");
            self.ids.free(id, commit).expect("it is live");
        }
        versions.publish(commit).expect("the next version");
        self.counts.commits.fetch_add(1, Ordering::Relaxed);
        versions.reclaim();
        commit
    }

    /// Write-back: every page version that no segment holds gets a block
    /// with its image and a link, oldest commit first.
    pub fn write_back(&self) {
        let mut writer = locked(&self.writer);
        let mut versions = Vec::new();
        self.pool.unwritten_versions(&mut versions);
        let mut image = Vec::new();
        for born in versions {
            let mut pages = Vec::new();
            self.pool.unwritten_pages(born, &mut pages);
            for id in pages {
                let frame = self.pool.get_version(id, born).expect("it is unwritten");
                image.clear();
                TreePage::from_validated(&frame, FRAME_FORMAT)
                    .expect("a tree page")
                    .write_image(&mut image);
                let block = writer
                    .append(id, BlockContent::Base, Compression::Lz4, &image)
                    .expect("a block");
                self.table
                    .push(id, Link::base(born, block))
                    .expect("a link");
                self.pool.mark_written(&frame).expect("it was owed");
            }
        }
    }
}

struct Hook(Arc<Store>);

/// What a reclaim does to the store: the page id allocator and the buffer
/// pool take their steps, and then the cleaner's part, which trims the
/// page table's chains.
impl Reclaim for Hook {
    fn reclaim(&self, pins: &Versions) {
        let store = &self.0;
        store.counts.reclaims.fetch_add(1, Ordering::Relaxed);
        let mut released = Vec::new();
        store.ids.reclaim(pins, &mut released);
        store
            .counts
            .ids_released
            .fetch_add(released.len() as u64, Ordering::Relaxed);
        store.pool.reclaim(pins);

        let oldest_pin = pins.oldest_pin();
        let epoch = pins.next_epoch();
        let oldest_guard = pins.oldest_guard();
        let mut trims = locked(&store.trims);
        trims.push((oldest_pin, epoch));
        let due = trims
            .iter()
            .filter(|(_, epoch)| *epoch <= oldest_guard)
            .map(|(oldest_pin, _)| *oldest_pin)
            .max();
        trims.retain(|(_, epoch)| *epoch > oldest_guard);
        drop(trims);
        let (Some(oldest), Some(last)) = (due, store.table.high_water()) else {
            return;
        };
        let mut removed = Vec::new();
        for raw in 1..=last.get() {
            store.table.trim(page(raw), oldest, &mut removed);
        }
        store
            .counts
            .links_trimmed
            .fetch_add(removed.len() as u64, Ordering::Relaxed);
    }

    /// The store has no background work: the reclaim after the next commit
    /// takes what the reader held back.
    fn wake(&self) {
        self.0.counts.wakes.fetch_add(1, Ordering::Relaxed);
    }
}

/// Numbers drawn from a seed.
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

    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}
