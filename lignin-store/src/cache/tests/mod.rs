mod basic;
mod errors;
mod flush;
mod model;
mod private;
mod races;
mod step;
mod threads;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use lignin_format::{CommitVersion, PageId};

use super::{
    frame::{flag, kind, NO_VERSION},
    pool::evictable,
    sync::{lock, read},
    *,
};
use crate::alloc::{Epoch, Pins};

const FRAME: usize = 64;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn owner(raw: u64) -> OwnerId {
    OwnerId::new(raw)
}

fn value(bytes: &[u8]) -> Arc<[u8]> {
    Arc::from(bytes)
}

/// A pool of `frames` small frames whose table has one part, so that every
/// page meets every other in it.
fn pool(frames: usize) -> BufferPool {
    BufferPool::with_frames(frames, FRAME, 1, frames * FRAME).expect("a small pool")
}

/// A pool of `frames` small frames with a table of four parts.
fn wide_pool(frames: usize) -> BufferPool {
    BufferPool::with_frames(frames, FRAME, 4, frames * FRAME).expect("a small pool")
}

/// What a test page holds: which page it is, the commit that bore it, and
/// how many page steps have changed it since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Content {
    page: u32,
    born: u64,
    revision: u32,
}

fn content(page: u32, born: u64, revision: u32) -> Content {
    Content {
        page,
        born,
        revision,
    }
}

impl Content {
    fn filler(self) -> u8 {
        (self.page as u8)
            .wrapping_mul(31)
            .wrapping_add((self.born as u8).wrapping_mul(7))
            .wrapping_add(self.revision as u8)
            .wrapping_add(1)
    }

    fn write(self, bytes: &mut [u8]) {
        bytes[..4].copy_from_slice(&self.page.to_le_bytes());
        bytes[4..12].copy_from_slice(&self.born.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.revision.to_le_bytes());
        bytes[16..].fill(self.filler());
    }

    /// Reads a page back, and checks that every byte of it is of one write.
    fn read(bytes: &[u8]) -> Self {
        let content = Content {
            page: u32::from_le_bytes(bytes[..4].try_into().expect("four bytes")),
            born: u64::from_le_bytes(bytes[4..12].try_into().expect("eight bytes")),
            revision: u32::from_le_bytes(bytes[12..16].try_into().expect("four bytes")),
        };
        let filler = content.filler();
        assert!(
            bytes[16..].iter().all(|&byte| byte == filler),
            "the page {content:?} is torn"
        );
        content
    }
}

/// The segments and the page table, as far as a load needs them: for each
/// page, what was written back, in the order of its chain.
#[derive(Debug, Default, Clone)]
struct Disk {
    chains: BTreeMap<u32, Vec<Link>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    Page(Content),
    Free(u64),
}

impl Link {
    fn version(self) -> u64 {
        match self {
            Link::Page(content) => content.born,
            Link::Free(version) => version,
        }
    }
}

impl Disk {
    /// Write-back of one page version. A page step's change to a version
    /// that is on disk already takes its place, as a delta of the same
    /// version does.
    fn write(&mut self, content: Content) {
        let chain = self.chains.entry(content.page).or_default();
        match chain.last_mut() {
            Some(Link::Page(last)) if last.born == content.born => *last = content,
            last => {
                assert!(
                    last.is_none_or(|last| last.version() < content.born),
                    "write-back went backwards on page {}",
                    content.page
                );
                chain.push(Link::Page(content));
            }
        }
    }

    fn free(&mut self, page: u32, version: u64) {
        self.chains
            .entry(page)
            .or_default()
            .push(Link::Free(version));
    }

    /// What the page table gives a reader at `at` (T5): the newest link at
    /// or before it, and the version of the link that follows.
    fn lookup(&self, page: u32, at: u64) -> Option<(Content, Option<u64>)> {
        let chain = self.chains.get(&page)?;
        let index = chain.iter().rposition(|link| link.version() <= at)?;
        let next = chain.get(index + 1).map(|link| link.version());
        match chain[index] {
            Link::Page(content) => Some((content, next)),
            Link::Free(_) => None,
        }
    }
}

/// Loads the page a miss was for, if the disk has it. The disk is read
/// again for as long as the pool says that it changed meanwhile.
fn load_with<'p>(
    pool: &'p BufferPool,
    mut miss: Miss,
    lookup: impl Fn(u32, u64) -> Option<(Content, Option<u64>)>,
) -> Option<PageRef<'p>> {
    let (page, at) = (miss.page(), miss.at());
    let mut blank = pool.blank();
    loop {
        let (content, next) = lookup(page.get(), at.get())?;
        content.write(&mut blank);
        let loaded = pool
            .install_loaded(blank, miss, version(content.born), next.map(version))
            .expect("the disk gives what the miss asked for");
        blank = match loaded {
            Loaded::Page(loaded) => return Some(loaded),
            Loaded::Again(blank) => blank,
        };
        miss = match pool.get(page, at) {
            Ok(found) => return Some(found),
            Err(miss) => miss,
        };
    }
}

fn load<'p>(pool: &'p BufferPool, disk: &Disk, miss: Miss) -> Option<PageRef<'p>> {
    load_with(pool, miss, |page, at| disk.lookup(page, at))
}

/// Reads a page at a version, from the pool or else from the disk.
fn fetch<'p>(pool: &'p BufferPool, disk: &Disk, page: u32, at: u64) -> Option<PageRef<'p>> {
    match pool.get(self::page(page), version(at)) {
        Ok(found) => Some(found),
        Err(miss) => load(pool, disk, miss),
    }
}

/// The page of a load that nothing interfered with.
fn loaded(loaded: Result<Loaded<'_>, CacheError>) -> PageRef<'_> {
    match loaded.expect("a load") {
        Loaded::Page(page) => page,
        Loaded::Again(_) => panic!("the pool changed during the load"),
    }
}

/// Puts a page straight into the pool as a loaded page version that stands
/// for every version from `born` on, or up to `next`.
fn seed(pool: &BufferPool, content: Content, next: Option<u64>) -> PageRef<'_> {
    let miss = pool
        .get(page(content.page), version(content.born))
        .expect_err("the page is not in the pool yet");
    let mut blank = pool.blank();
    content.write(&mut blank);
    loaded(pool.install_loaded(blank, miss, version(content.born), next.map(version)))
}

/// One commit of one page: a private copy of the version at `at`, rewritten
/// and installed at `at + 1`.
fn commit<'p>(pool: &'p BufferPool, page: u32, at: u64) -> PageRef<'p> {
    let copy = pool
        .make_private(owner(at + 1), self::page(page), version(at))
        .expect("the page is in the pool");
    content(page, at + 1, 0).write(&mut copy.write().expect("a private copy"));
    pool.install(copy, version(at + 1)).expect("a commit")
}

/// The first commit of a page: a new private copy, installed at `born`.
fn create<'p>(pool: &'p BufferPool, page: u32, born: u64) -> PageRef<'p> {
    let copy = pool
        .new_private(owner(born), self::page(page))
        .expect("a new page");
    content(page, born, 0).write(&mut copy.write().expect("a private copy"));
    pool.install(copy, version(born)).expect("a commit")
}

/// Pins and guards as a test sets them.
#[derive(Debug, Default)]
struct TestPins {
    oldest: AtomicU64,
    epoch: AtomicU64,
    oldest_guard: AtomicU64,
    asked: AtomicU64,
}

impl TestPins {
    /// No pin below `oldest`, and no operation running.
    fn at(oldest: u64) -> Self {
        let pins = Self::default();
        pins.oldest.store(oldest, Ordering::SeqCst);
        pins.oldest_guard.store(u64::MAX, Ordering::SeqCst);
        pins
    }

    fn set_oldest(&self, oldest: u64) {
        self.oldest.store(oldest, Ordering::SeqCst);
    }

    /// An operation is running that began in `epoch`, or none with
    /// `u64::MAX`.
    fn set_oldest_guard(&self, epoch: u64) {
        self.oldest_guard.store(epoch, Ordering::SeqCst);
    }

    fn asked(&self) -> u64 {
        self.asked.load(Ordering::SeqCst)
    }
}

impl Pins for TestPins {
    fn oldest_pin(&self) -> CommitVersion {
        self.asked.fetch_add(1, Ordering::SeqCst);
        version(self.oldest.load(Ordering::SeqCst))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.epoch.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn oldest_guard(&self) -> Epoch {
        let current = self.epoch.load(Ordering::SeqCst);
        Epoch::new(self.oldest_guard.load(Ordering::SeqCst).min(current))
    }
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

/// Checks what must hold of a pool whenever no call into it is in progress
/// and the caller holds `loose` frames that are not in the pool's table: a
/// [`Blank`], or a frame that was replaced or discarded and is still
/// pinned.
///
/// - Of one page there is at most one frame for each born version, at most
///   one current version, and the versions each frame stands for do not
///   overlap.
/// - An owner has at most one private copy of a page.
/// - A page version is unwritten, held or neither, and the lists of
///   unwritten and held page versions say the same as the frames.
/// - A held page version is superseded.
/// - Every frame beside the fixed ones is in use: it cannot be evicted.
/// - Every fixed frame is in the table, on the free list, or loose.
/// - What the pool reports as memory is the fixed part, the frames beside
///   it and the attached values, and the frames beside it are the ones in
///   the table and, at most, the loose ones.
fn check(pool: &BufferPool, loose: usize) {
    let mut unwritten = BTreeSet::new();
    let mut held = BTreeSet::new();
    let mut linked_fixed = 0;
    let mut overflow = 0;
    for bucket in pool.buckets.iter() {
        let chain = read(&bucket.chain);
        let mut versions: BTreeMap<u32, Vec<(u64, u64)>> = BTreeMap::new();
        let mut owners = BTreeSet::new();
        for entry in chain.iter() {
            let frame = entry.frame();
            let page = frame.page_raw();
            assert_ne!(page, 0, "a linked frame holds a page");
            if frame.is_overflow() {
                overflow += 1;
                assert!(
                    frame.pins() > 0 || !evictable(frame),
                    "a frame beside the fixed ones is idle: {frame:?}"
                );
            } else {
                linked_fixed += 1;
            }
            match frame.kind() {
                kind::PUBLISHED => {
                    let (born, superseded) = (frame.born(), frame.superseded());
                    assert!(born < superseded, "{frame:?} stands for no version");
                    versions.entry(page).or_default().push((born, superseded));
                    let flags = frame.flags();
                    assert!(
                        flags & flag::UNWRITTEN == 0 || flags & flag::HELD == 0,
                        "{frame:?} is unwritten and held"
                    );
                    if flags & flag::UNWRITTEN != 0 {
                        assert!(unwritten.insert((born, page)));
                    }
                    if flags & flag::HELD != 0 {
                        assert_ne!(superseded, NO_VERSION, "{frame:?} is held and current");
                        assert!(held.insert((born, page)));
                    }
                }
                kind::PRIVATE => {
                    assert!(
                        owners.insert((page, frame.owner())),
                        "an owner has two copies of page {page}"
                    );
                }
                other => panic!("a linked frame is of kind {other}: {frame:?}"),
            }
        }
        for (page, mut versions) in versions {
            versions.sort_unstable();
            for pair in versions.windows(2) {
                assert!(
                    pair[0].1 <= pair[1].0,
                    "two frames of page {page} stand for one version: {pair:?}"
                );
            }
        }
    }

    let lists = lock(&pool.unwritten);
    let listed: BTreeSet<(u64, u32)> = lists
        .versions
        .iter()
        .flat_map(|(&born, pages)| pages.iter().map(move |page| (born, page.get())))
        .collect();
    let listed_count: usize = lists.versions.values().map(Vec::len).sum();
    assert_eq!(listed.len(), listed_count, "a page is listed twice");
    assert!(lists.versions.values().all(|pages| !pages.is_empty()));
    assert_eq!(
        listed, unwritten,
        "the unwritten lists and the frames differ"
    );
    let listed_held: BTreeSet<(u64, u32)> = lists
        .held
        .iter()
        .map(|held| (held.born, held.page.get()))
        .collect();
    assert_eq!(listed_held.len(), lists.held.len(), "a page is held twice");
    assert_eq!(listed_held, held, "the held list and the frames differ");
    drop(lists);

    let free = lock(&pool.free);
    assert!(free
        .iter()
        .all(|frame| frame.header().kind() == kind::FREE && !frame.header().is_overflow()));
    let distinct: BTreeSet<*const _> = free
        .iter()
        .map(|frame| std::ptr::from_ref(frame.header()))
        .collect();
    assert_eq!(distinct.len(), free.len(), "a frame is free twice");
    let fixed = linked_fixed + free.len();
    let beside = pool.overflow_frames();
    drop(free);

    assert!(fixed <= pool.frames() && beside >= overflow);
    assert_eq!(
        (pool.frames() - fixed) + (beside - overflow),
        loose,
        "a frame is neither in the table, nor free, nor held by the caller"
    );
    assert_eq!(
        pool.memory(),
        pool.fixed_memory() + pool.overflow_frames() * pool.overflow_cost() + pool.attached_bytes()
    );
    let census = pool.census();
    assert_eq!(census.overflow, overflow);
    assert_eq!(census.overflow, census.overflow_unevictable);
}
