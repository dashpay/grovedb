//! The buffer pool's two promises about memory, measured at the allocator:
//! the read path allocates nothing, and what the pool has allocated never
//! exceeds its configured size plus the frames that are pinned.
//!
//! The test binary runs under an allocator that counts, for the calling
//! thread, every allocation and the bytes that are allocated now.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

use lignin_format::{CommitVersion, PageId, PageSize};
use lignin_store::cache::{BufferPool, Loaded, OwnerId, PageRef, PoolConfig};

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<i64> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn count(allocations: u64, bytes: i64) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + allocations));
        let _ = BYTES.try_with(|count| count.set(count.get() + bytes));
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count(1, layout.size() as i64);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count(1, layout.size() as i64);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count(1, new_size as i64 - layout.size() as i64);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::count(0, -(layout.size() as i64));
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let before = ALLOCATIONS.get();
    let result = work();
    (result, ALLOCATIONS.get() - before)
}

fn allocated() -> i64 {
    BYTES.get()
}

const PAGE_SIZE: usize = 8_192;
const FRAMES: usize = 8;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn pool() -> BufferPool {
    let config = PoolConfig {
        page_size: PageSize::new(PAGE_SIZE).expect("a page size"),
        capacity: FRAMES * PAGE_SIZE + 4_096,
    };
    let pool = BufferPool::new(config).expect("holds frames");
    assert_eq!(pool.frames(), FRAMES);
    pool
}

/// Reads page `raw` at version 1, loading it as a page whose every byte is
/// its number if the pool does not have it.
fn fetch(pool: &BufferPool, raw: u32) -> PageRef<'_> {
    let miss = match pool.get(page(raw), version(1)) {
        Ok(found) => return found,
        Err(miss) => miss,
    };
    let mut blank = pool.blank();
    blank.fill(raw as u8);
    match pool
        .install_loaded(blank, miss, version(1), None)
        .expect("a load")
    {
        Loaded::Page(loaded) => loaded,
        Loaded::Again(_) => panic!("nothing else changes the pool"),
    }
}

fn is_page(found: &PageRef<'_>, raw: u32) -> bool {
    found.len() == PAGE_SIZE && found.iter().all(|&byte| byte == raw as u8)
}

#[test]
fn the_read_path_allocates_nothing() {
    let pool = pool();
    for raw in 1..=FRAMES as u32 {
        drop(fetch(&pool, raw));
    }

    // A hit, with everything a reader does with what it gets.
    let ((), allocations) = allocations_during(|| {
        for round in 0..1_000u32 {
            let raw = 1 + round % FRAMES as u32;
            let found = pool.get(page(raw), version(1)).expect("in the pool");
            let again = found.clone();
            black_box(found[0]);
            black_box((
                again.page(),
                again.born(),
                again.attached(1_u64.to_le_bytes()),
            ));
            drop(found);
            black_box(again[PAGE_SIZE - 1]);
        }
    });
    assert_eq!(allocations, 0, "a hit allocated");

    // A miss.
    let (miss, allocations) = allocations_during(|| pool.get(page(100), version(1)));
    assert!(miss.is_err());
    assert_eq!(allocations, 0, "a miss allocated");

    // A load, which evicts a page to get its frame.
    let ((), allocations) = allocations_during(|| {
        for raw in 100..140 {
            let loaded = fetch(&pool, raw);
            black_box(loaded[0]);
        }
    });
    assert_eq!(allocations, 0, "a load into a fixed frame allocated");
    assert_eq!(pool.stats().evictions, 40);
    assert_eq!(pool.overflow_frames(), 0);
}

#[test]
fn the_pool_allocates_its_configured_size_and_the_frames_that_are_pinned() {
    let before = allocated();
    let pool = pool();
    let used = |_: &BufferPool| (allocated() - before) as usize;

    // The fixed part is allocated at once, and is within the configured
    // size.
    assert_eq!(used(&pool), pool.fixed_memory());
    assert!(pool.fixed_memory() <= pool.capacity());
    assert!(pool.fixed_memory() >= FRAMES * PAGE_SIZE);

    // Any number of pages go through the pool without it growing.
    for raw in 1..=100 {
        assert!(is_page(&fetch(&pool, raw), raw));
        assert_eq!(used(&pool), pool.fixed_memory());
    }

    // Pinning more pages than the pool has frames allocates one frame for
    // each page beyond them, and nothing else.
    let mut pinned = Vec::with_capacity(64);
    let with_list = |pool: &BufferPool| used(pool) - 64 * size_of::<PageRef<'_>>();
    for raw in 1..=3 * FRAMES as u32 {
        pinned.push(fetch(&pool, raw));
        let beyond = pinned.len().saturating_sub(FRAMES);
        assert_eq!(pool.overflow_frames(), beyond);
        assert_eq!(with_list(&pool), pool.memory());
        assert_eq!(
            pool.memory(),
            pool.fixed_memory() + beyond * pool.overflow_cost()
        );
        assert!(pool.memory() <= pool.capacity() + pinned.len() * pool.overflow_cost());
        let census = pool.census();
        assert_eq!(census.pinned, pinned.len());
        assert_eq!(census.overflow, census.overflow_unevictable);
    }
    for (found, raw) in pinned.iter().zip(1..) {
        assert!(is_page(found, raw));
    }

    // Each frame beyond the fixed ones is given back the moment its page is
    // unpinned.
    while let Some(found) = pinned.pop() {
        drop(found);
        let beyond = pinned.len().saturating_sub(FRAMES);
        assert_eq!(pool.overflow_frames(), beyond);
        assert_eq!(with_list(&pool), pool.memory());
    }
    drop(pinned);
    assert_eq!(used(&pool), pool.fixed_memory());

    // A private copy is kept for its transaction whatever is pinned, so it
    // counts with the pinned frames until it is discarded.
    let mut copies = Vec::with_capacity(64);
    for raw in 1..=2 * FRAMES as u32 {
        let base = fetch(&pool, raw);
        let copy = pool
            .make_private(OwnerId::new(1), page(raw), version(1))
            .expect("held");
        drop(base);
        assert!(copy.read().iter().all(|&byte| byte == raw as u8));
        copies.push(copy);
        let census = pool.census();
        assert_eq!(with_list(&pool), pool.memory());
        assert!(pool.memory() <= pool.capacity() + census.unevictable * pool.overflow_cost());
        assert_eq!(census.overflow, census.overflow_unevictable);
    }
    assert_eq!(pool.census().private, 2 * FRAMES);
    assert!(pool.overflow_frames() >= FRAMES);
    for copy in copies.drain(..) {
        pool.discard(&copy).expect("a private copy");
    }
    drop(copies);
    assert_eq!(pool.overflow_frames(), 0);
    assert_eq!(used(&pool), pool.fixed_memory());

    drop(pool);
    assert_eq!(allocated(), before, "the pool gave everything back");
}
