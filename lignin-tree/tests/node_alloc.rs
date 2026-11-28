//! Reading a node performs no heap allocation, measured at the allocator.
//!
//! The test binary runs under an allocator that counts the allocations of
//! the calling thread. Nodes are read in every way the module has: from a
//! page that is in the buffer pool, from a page that has to be loaded from
//! its segments first, by a stored address, by a kept handle, and from the
//! private copy of a write transaction.
//!
//! What the store allocates to get a frame is counted with it. A load
//! allocates nothing here: the buffers have grown, and the runs hold
//! changes the page codec makes where they are. The one case in which the
//! store allocates is measured on its own: a page that is not in the pool
//! while every frame is pinned gets a frame beside the fixed ones.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::Arc,
};

use lignin_format::{
    delta::{Delta, DeltaRunWriter},
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellContent, CellValue, HashField, Side, TreeLink},
    value_log::{PendingValue, ValuePointer},
    CellAddress, CommitVersion, FileId, PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cache::{BufferPool, OwnerId, PoolConfig},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore},
    table::{Link, PageTable},
};
use lignin_tree::node::{
    Lease, NodeHandle, NodePage, NodePageMut, NodeReader, NodeRef, OperationGuard,
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

const SIZE: PageSize = PageSize::DEFAULT;
const PAGES: u32 = 6;
const NODES: u16 = 60;

struct Guard;

impl OperationGuard for Guard {}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// The base image of a page of `NODES` nodes, and the run of a later commit
/// that changes hashes, a child's address and a value in it. Node `n` has
/// node `n + 1` of its page as its left child and node `n` of another page
/// as its right child, a different page from one node to the next. Every
/// third value is in the value log.
fn tree_page(id: PageId) -> (Vec<u8>, Vec<u8>) {
    let beside = |n: u16| page((id.get() + u32::from(n)) % PAGES + 1);
    let mut frame = vec![0; SIZE.get()];
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
    let hash = [0xC4; 32];
    let hosted = [Some(CellAddress::new(beside(0), slot(1))), None];
    for n in 1..=NODES {
        let key = format!("key-{n:04}");
        let value = vec![n as u8; 40 + usize::from(n) * 3];
        let link = |child| TreeLink {
            child,
            hash: &hash,
            heights: (2, 1),
            child_key_len: 8,
            aggregate: &[3, 1, 0, 0, 0, 0, 0, 0, 0],
        };
        let value = if n % 3 == 0 {
            CellValue::OutOfLine(
                ValuePointer::new(FileId::MIN, 4_096 * u64::from(n), 9_000).expect("a pointer"),
            )
        } else {
            CellValue::Inline(&value)
        };
        nodes
            .insert(&CellContent {
                key: key.as_bytes(),
                feature: &[3, n as u8, 0, 0, 0, 0, 0, 0, 0],
                value_hash: &[n as u8; 32],
                kv_hash: &[!(n as u8); 32],
                left: (n < NODES).then(|| link(CellAddress::new(id, slot(n + 1)))),
                right: Some(link(CellAddress::new(beside(n), slot(n)))),
                hosted_roots: if n % 10 == 0 { &hosted } else { &[] },
                value,
                fragment_root: n == 1,
            })
            .expect("the node fits");
    }
    let image = nodes.view().tree_page().to_image();

    let mut run = Vec::new();
    let mut records = DeltaRunWriter::new(id, None, &mut run);
    let changes = [
        Delta::Hash {
            slot: slot(1),
            field: HashField::Value,
            hash: &[0xD1; 32],
        },
        Delta::Hash {
            slot: slot(2),
            field: HashField::Child(Side::Right),
            hash: &[0xD2; 32],
        },
        Delta::Child {
            slot: slot(4),
            side: Side::Right,
            child: CellAddress::new(beside(5), slot(4)),
        },
        Delta::Value {
            slot: slot(5),
            value: CellValue::Inline(&[0xD5; 55]),
        },
    ];
    for change in &changes {
        records.push(change).expect("a record");
    }
    records.finish().expect("a run");
    (image, run)
}

/// Reads every part of a node, and returns a number that depends on every
/// byte that was read.
fn read(node: &NodeRef<'_>) -> u64 {
    let sum = |bytes: &[u8]| bytes.iter().map(|&byte| u64::from(byte)).sum::<u64>();
    let mut total = sum(node.key()) + sum(node.feature()) + u64::from(node.value_len());
    total += match node.value() {
        CellValue::Inline(value) => sum(value),
        CellValue::OutOfLine(pointer) => pointer.offset(),
        CellValue::Pending(pending) => sum(&pending.id()),
    };
    total += sum(node.value_hash()) + sum(node.kv_hash());
    for side in Side::BOTH {
        if let Some(link) = node.link(side) {
            total += sum(link.hash) + sum(link.aggregate) + u64::from(link.child.slot().get());
            total += u64::from(link.heights.0 + link.heights.1 + link.child_key_len);
            total += sum(node.hash(HashField::Child(side)).expect("the link's hash"));
        }
    }
    for root in node.hosted_roots() {
        total += root.map_or(1, |root| u64::from(root.page().get()));
    }
    total + u64::from(node.is_fragment_root()) + node.handle().departures()
}

/// Reads every node of every page: the nodes of a page in slot order, then
/// each again through the links that lead to it, then each again by the
/// handle that was kept for it.
fn read_all(
    reader: &NodeReader<'_, Guard>,
    buffers: &mut LoadBuffers,
    handles: &mut [Option<NodeHandle>],
) -> (u64, u64) {
    let (mut nodes, mut total) = (0, 0);
    let mut kept = handles.iter_mut();
    for raw in 1..=PAGES {
        let pinned = reader.pin(page(raw), buffers).expect("the page loads");
        for node in pinned.view().nodes() {
            total += read(&node);
            nodes += 1;
            *kept.next().expect("room for every handle") = Some(node.handle());

            let left = node.link(Side::Left).map(|link| link.child);
            if let Some(child) = left.and_then(|child| pinned.view().node(child.slot())) {
                total += read(&child);
                nodes += 1;
            }
            let right = node.link(Side::Right).expect("a right link").child;
            let child = reader.node(right, buffers).expect("the address is exact");
            total += read(&child.node());
            nodes += 1;
        }
    }
    for handle in handles.iter().flatten() {
        let found = reader
            .resolve(*handle, buffers)
            .expect("the page loads")
            .expect("no cell has left the page");
        total += read(&found.node());
        nodes += 1;
    }
    (nodes, total)
}

#[test]
fn reading_a_node_allocates_nothing() {
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
    for raw in 1..=PAGES {
        let id = page(raw);
        let (image, run) = tree_page(id);
        let block = writer
            .append(id, BlockContent::Base, Compression::Lz4, &image)
            .expect("a block");
        table
            .push(id, Link::base(version(1), block))
            .expect("a link");
        let block = writer
            .append(id, BlockContent::Delta, Compression::Lz4, &run)
            .expect("a block");
        table
            .push(id, Link::delta(version(2), block))
            .expect("a link");
    }

    let guard = Guard;
    let lease = Lease::take();
    let mut buffers = LoadBuffers::new();
    let mut handles = vec![None; usize::from(NODES) * PAGES as usize];
    let per_round = u64::from(NODES) * u64::from(PAGES) * 4 - u64::from(PAGES);

    // Every page is in the pool: a read is a hit.
    let pool = BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: 4 << 20,
    })
    .expect("a pool");
    let loader = PageLoader::new(&pool, &table, &segments, SIZE).expect("a loader");
    let reader = NodeReader::new(loader, &guard, lease, version(2));
    let warm = read_all(&reader, &mut buffers, &mut handles);
    assert_eq!(warm.0, per_round);
    let misses = pool.stats().misses;
    let before = ALLOCATIONS.get();
    let first = read_all(&reader, &mut buffers, &mut handles);
    let second = read_all(&reader, &mut buffers, &mut handles);
    let allocations = ALLOCATIONS.get() - before;
    assert_eq!((first, second), (warm, warm));
    assert_eq!(pool.stats().misses, misses);
    println!(
        "pages in the pool: {} nodes read, {allocations} allocations",
        first.0 + second.0
    );
    assert_eq!(allocations, 0);

    // The pool holds two of the six pages: most reads load their page from
    // its segment first, the base image and a run on top of it.
    let small = BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: 2 * (SIZE.get() + 512),
    })
    .expect("a pool");
    assert_eq!(small.frames(), 2);
    let loader = PageLoader::new(&small, &table, &segments, SIZE).expect("a loader");
    let reader = NodeReader::new(loader, &guard, lease, version(2));
    assert_eq!(read_all(&reader, &mut buffers, &mut handles), warm);
    let (loads, overflows) = (small.stats().misses, small.stats().overflows);
    let before = ALLOCATIONS.get();
    let first = read_all(&reader, &mut buffers, &mut handles);
    let second = read_all(&reader, &mut buffers, &mut handles);
    let allocations = ALLOCATIONS.get() - before;
    assert_eq!((first, second), (warm, warm));
    let loads = small.stats().misses - loads;
    assert!(loads > 500, "{loads} loads");
    assert_eq!(small.stats().overflows, overflows);
    println!(
        "pages loaded from segments: {} nodes read, {loads} loads, {allocations} allocations",
        first.0 + second.0
    );
    assert_eq!(allocations, 0);

    // A page that is not in the pool, while every frame of the pool is
    // pinned: the pool takes a frame beside its fixed ones, which is the
    // one allocation a read can cost. Reading the nodes of the page costs
    // none.
    let held = [1, 2].map(|raw| reader.pin(page(raw), &mut buffers).expect("the page loads"));
    let overflows = small.stats().overflows;
    let before = ALLOCATIONS.get();
    let beside = reader.pin(page(3), &mut buffers).expect("the page loads");
    let for_the_frame = ALLOCATIONS.get() - before;
    assert_eq!(small.stats().overflows, overflows + 1);
    assert!(for_the_frame > 0);
    let before = ALLOCATIONS.get();
    let (mut read_nodes, mut total) = (0, 0);
    for node in beside.view().nodes() {
        total += read(&node);
        total += read(
            &beside
                .view()
                .resolve(node.handle())
                .expect("no cell has left"),
        );
        read_nodes += 2;
    }
    let allocations = ALLOCATIONS.get() - before;
    println!(
        "a page beside a pool whose frames are all pinned: {for_the_frame} allocations for \
         the frame, {read_nodes} nodes read, {allocations} allocations"
    );
    assert!(total > 0);
    assert_eq!(allocations, 0);
    drop((held, beside));

    // A write transaction reads its private copy under the copy's latch,
    // and changes it where a change takes the room of what was there.
    let pinned = reader.pin(page(1), &mut buffers).expect("the page loads");
    let copy = small
        .make_private(OwnerId::new(1), page(1), version(2))
        .expect("the page is pinned");
    let before = ALLOCATIONS.get();
    let mut read_nodes = 0;
    let mut total = 0;
    {
        let latch = copy.read();
        let nodes = NodePage::open(&latch, lease).expect("a tree page");
        for handle in handles.iter().flatten().take(usize::from(NODES)) {
            let node = nodes.resolve(*handle).expect("no cell has left the page");
            total += read(&node);
            read_nodes += 1;
        }
    }
    {
        let mut latch = copy.write().expect("the copy is private");
        let mut nodes = NodePageMut::open(&mut latch, SIZE, lease, ()).expect("a tree page");
        for handle in handles.iter().flatten().take(usize::from(NODES)) {
            let mut node = nodes
                .resolve_mut(*handle)
                .expect("no cell has left the page");
            let (child, heights, child_key_len, value_len) = {
                let read = node.node();
                let link = read.link(Side::Right).expect("a right link");
                let value_len = read.value().inline().map(<[u8]>::len);
                (link.child, link.heights, link.child_key_len, value_len)
            };
            node.set_hash(HashField::KeyValue, &[0xE1; 32])
                .expect("a hash");
            node.set_hash(HashField::Child(Side::Right), &[0xE2; 32])
                .expect("a hash");
            node.set_child(Side::Right, CellAddress::new(page(2), slot(1)))
                .expect("an address");
            node.set_link(
                Side::Right,
                Some(TreeLink {
                    child,
                    hash: &[0xE3; 32],
                    heights,
                    child_key_len,
                    aggregate: &[3, 2, 0, 0, 0, 0, 0, 0, 0],
                }),
            )
            .expect("a link of the same size");
            node.set_feature(&[3, 9, 0, 0, 0, 0, 0, 0, 0])
                .expect("a feature of the same size");
            node.set_fragment_root(true).expect("a mark");
            let value = match value_len {
                Some(len) => CellValue::Inline(&[0xE4; 256][..len]),
                None => CellValue::Pending(PendingValue::new([0xE5; 8], 700).expect("pending")),
            };
            node.set_value(value)
                .expect("a value that takes the room of the one before it");
            total += read(&node.node());
            read_nodes += 1;
        }
    }
    let allocations = ALLOCATIONS.get() - before;
    println!("a private copy: {read_nodes} nodes read, {allocations} allocations");
    assert!(total > 0);
    assert_eq!(allocations, 0);
    drop(pinned);
}
