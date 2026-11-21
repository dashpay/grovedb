//! A cursor that has grown to its tree allocates nothing, measured at the
//! allocator.
//!
//! The test binary runs under an allocator that counts the allocations of
//! the calling thread. A cursor keeps its way and its key in two vectors,
//! which grow to the height of the tree and to its longest key. After
//! that a seek, a step and a read allocate nothing: not in the cursor, and
//! not in the store, whether the page is in the pool or is loaded into a
//! frame that another page had.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::Arc,
};

use lignin_format::{
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellContent, CellValue, TreeLink},
    CellAddress, CommitVersion, PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cache::{BufferPool, PoolConfig},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore},
    table::{Link, PageTable},
};
use lignin_tree::{
    cursor::Cursor,
    node::{Lease, NodePage, NodePageMut, NodeReader, OperationGuard},
};

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn count() {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: every method counts and then hands its arguments, unchanged, to
// the same method of `System`, so the memory is what `System` gives and takes
// back under the contract the caller keeps. Counting adds to thread-local
// cells that are constants at first and have no destructor, so it allocates
// nothing, and it cannot unwind short of a count that overflows 64 bits.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count();
        // SAFETY: the caller keeps the contract of `GlobalAlloc::alloc`, which
        // is the contract of `System.alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count();
        // SAFETY: the caller keeps the contract of `GlobalAlloc::alloc_zeroed`, which
        // is the contract of `System.alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count();
        // SAFETY: the caller keeps the contract of `GlobalAlloc::realloc`, which
        // is the contract of `System.realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller keeps the contract of `GlobalAlloc::dealloc`, which
        // is the contract of `System.dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

const SIZE: PageSize = PageSize::MIN;
const NODES: usize = 1_000;
const PER_PAGE: usize = 16;

struct Guard;

impl OperationGuard for Guard {}

fn key(node: usize) -> [u8; 4] {
    (node as u32).to_be_bytes()
}

/// Where the node with the `node`th key is: sixteen keys to a page, in key
/// order.
fn address(node: usize) -> CellAddress {
    let page = PageId::new(1 + (node / PER_PAGE) as u32).expect("not zero");
    let slot = SlotId::new(1 + (node % PER_PAGE) as u16).expect("not zero");
    CellAddress::new(page, slot)
}

/// The root of the balanced tree of the keys from `low` up to `high`.
fn root_of(low: usize, high: usize) -> Option<usize> {
    (low < high).then(|| low + (high - low) / 2)
}

/// The children of `node` in the balanced tree of all keys.
fn children(node: usize) -> (Option<usize>, Option<usize>) {
    let (mut low, mut high) = (0, NODES);
    loop {
        let root = root_of(low, high).expect("the node is in the range");
        match node.cmp(&root) {
            std::cmp::Ordering::Less => high = root,
            std::cmp::Ordering::Greater => low = root + 1,
            std::cmp::Ordering::Equal => return (root_of(low, root), root_of(root + 1, high)),
        }
    }
}

#[test]
fn a_cursor_that_has_grown_allocates_nothing() {
    let version = CommitVersion::new(1).expect("a version");
    let dir = tempfile::tempdir().expect("a directory");
    let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
    let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
    let mut manifest = ManifestStore::create(vfs, dir.path(), empty).expect("a manifest");
    let segments = SegmentStore::open(&manifest).expect("the segments");
    let mut writer = segments.create().expect("a segment");
    segments
        .commit(&mut manifest)
        .expect("the segment is named");
    let table = PageTable::new();

    let hash = [0x5A; 32];
    let link = |child: Option<usize>| {
        child.map(|child| TreeLink {
            child: address(child),
            hash: &hash,
            heights: (1, 1),
            child_key_len: 4,
            aggregate: &[],
        })
    };
    for first in (0..NODES).step_by(PER_PAGE) {
        let id = address(first).page();
        let mut frame = vec![0; SIZE.get()];
        let mut nodes =
            NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
        for node in first..(first + PER_PAGE).min(NODES) {
            let (left, right) = children(node);
            let handle = nodes
                .insert(&CellContent {
                    key: &key(node),
                    feature: &[0],
                    value_hash: &[0x11; 32],
                    kv_hash: &[0x22; 32],
                    left: link(left),
                    right: link(right),
                    hosted_roots: &[],
                    value: CellValue::Inline(b"value"),
                    fragment_root: true,
                })
                .expect("fits");
            assert_eq!(handle.address(), address(node));
        }
        let image = NodePage::open(&frame, Lease::take())
            .expect("a tree page")
            .tree_page()
            .to_image();
        let block = writer
            .append(id, BlockContent::Base, Compression::Lz4, &image)
            .expect("a block");
        table.push(id, Link::base(version, block)).expect("a link");
    }
    let root = root_of(0, NODES).map(address);

    // In a pool that holds every page, and in one of a single frame, where
    // every page a step goes to is loaded.
    let roomy = NODES / PER_PAGE * (SIZE.get() + 4_096);
    for (capacity, all_hits) in [(roomy, true), (SIZE.get() + 4_096, false)] {
        let pool = BufferPool::new(PoolConfig {
            page_size: SIZE,
            capacity,
        })
        .expect("a pool");
        assert_eq!(pool.frames() == 1, !all_hits);
        let loader = PageLoader::new(&pool, &table, &segments, SIZE).expect("a loader");
        let guard = Guard;
        let reader = NodeReader::new(loader, &guard, Lease::take(), version);
        let mut buffers = LoadBuffers::new();
        let mut cursor = Cursor::open(&reader, root);

        let round = |cursor: &mut Cursor, buffers: &mut LoadBuffers| {
            let mut seen = 0;
            let mut node = cursor.seek_to_first(&reader, buffers).expect("readable");
            while let Some(found) = node {
                assert_eq!(found.node().key(), key(seen));
                seen += 1;
                drop(found);
                node = cursor.next(&reader, buffers).expect("readable");
            }
            assert_eq!(seen, NODES);
            let mut node = cursor.seek_to_last(&reader, buffers).expect("readable");
            while let Some(found) = node {
                seen -= 1;
                assert_eq!(found.node().key(), key(seen));
                drop(found);
                node = cursor.prev(&reader, buffers).expect("readable");
            }
            assert_eq!(seen, 0);
            for probe in (0..NODES).step_by(7) {
                let found = cursor
                    .seek(&reader, buffers, &key(probe))
                    .expect("readable");
                assert_eq!(found.expect("a node").node().key(), key(probe));
                let found = cursor
                    .seek_for_prev(&reader, buffers, &key(probe))
                    .expect("readable");
                assert_eq!(found.expect("a node").node().key(), key(probe));
                let found = cursor.node(&reader, buffers).expect("readable");
                assert_eq!(found.expect("a node").node().value_len(), 5);
            }
        };

        // The first round grows the cursor's way and key, and the buffers.
        round(&mut cursor, &mut buffers);
        let misses = pool.stats().misses;
        let before = allocations();
        round(&mut cursor, &mut buffers);
        let allocated = allocations() - before;
        assert_eq!(allocated, 0, "all hits: {all_hits}");
        assert_eq!(pool.stats().misses == misses, all_hits);
        assert_eq!(pool.stats().overflows, 0);
    }
}
