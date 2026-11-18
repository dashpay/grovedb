//! A load with buffers that have grown allocates nothing, measured at the
//! allocator: the blocks are read into the caller's buffers, the image goes
//! straight into a frame, and the runs are applied there.
//!
//! The runs here hold the changes the page codec makes without allocating
//! (`page-delta.md` A1). A change that gives a cell another length
//! allocates in the codec while it is applied, which is not the loader's.
//!
//! The test binary runs under an allocator that counts the allocations of
//! the calling thread.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::Arc,
};

use lignin_format::{
    delta::{Delta, DeltaRunWriter},
    flat_page::{FlatLeafBuilder, FlatValue},
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellContent, CellValue, HashField, TreeCell, TreePage, TreePageMut, TreePageSize},
    CommitVersion, FormatVersion, PageId, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cache::{BufferPool, PoolConfig},
    load::{LoadBuffers, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore},
    table::{Link, PageTable},
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

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SIZE: TreePageSize = TreePageSize::DEFAULT;
const PAGES: u32 = 6;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn cell_bytes(key: &[u8], value: &[u8]) -> Vec<u8> {
    CellContent {
        key,
        feature: &[],
        value_hash: &[1; 32],
        kv_hash: &[2; 32],
        left: None,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(value),
        fragment_root: false,
    }
    .encode()
    .expect("a cell")
}

/// The image of a tree page with 40 cells, and three runs that change it:
/// hashes, a value of the same length, and a cell that leaves and one that
/// arrives.
fn tree_page(id: PageId) -> (Vec<u8>, [Vec<u8>; 3], Vec<u8>) {
    let mut frame = vec![0; SIZE.get()];
    let mut written = TreePageMut::init(&mut frame, SIZE, id, 0).expect("a page");
    for n in 1..=40u16 {
        let key = format!("key-{n:03}");
        let bytes = cell_bytes(key.as_bytes(), &[n as u8; 150]);
        let cell = TreeCell::decode(&bytes, FormatVersion::CURRENT).expect("a cell");
        written
            .insert_encoded(slot(n), cell)
            .expect("the cell fits");
    }
    let image = written.page().to_image();

    let arriving = cell_bytes(b"arrives", &[0x77; 400]);
    let arriving = TreeCell::decode(&arriving, FormatVersion::CURRENT).expect("a cell");
    let changes: [&[Delta<'_>]; 3] = [
        &[
            Delta::Hash {
                slot: slot(3),
                field: HashField::Value,
                hash: &[0xA1; 32],
            },
            Delta::Hash {
                slot: slot(9),
                field: HashField::KeyValue,
                hash: &[0xA2; 32],
            },
        ],
        &[Delta::Value {
            slot: slot(5),
            value: CellValue::Inline(&[0x55; 150]),
        }],
        &[
            Delta::Remove { slot: slot(7) },
            Delta::Insert {
                slot: slot(41),
                cell: arriving,
            },
        ],
    ];
    let runs = changes.map(|changes| {
        let mut run = Vec::new();
        let mut records = DeltaRunWriter::new(id, None, &mut run);
        for change in changes {
            change.apply(&mut written).expect("the change is made");
            records.push(change).expect("the change is written");
        }
        records.finish().expect("a run");
        run
    });
    (image, runs, written.page().to_image())
}

fn flat_page() -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(b"flat/", SIZE.image_capacity()).expect("a leaf");
    for n in 0..60 {
        let key = format!("flat/{n:04}");
        leaf.push(key.as_bytes(), FlatValue::Inline(&[n as u8; 90]))
            .expect("the entry fits");
    }
    leaf.finish()
}

#[test]
fn a_load_with_grown_buffers_allocates_nothing() {
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

    let mut expected = Vec::new();
    for raw in 1..=PAGES {
        let id = page(raw);
        if raw == PAGES {
            let image = flat_page();
            let block = writer
                .append(id, BlockContent::Base, Compression::Lz4, &image)
                .expect("a block");
            table
                .push(id, Link::base(version(4), block))
                .expect("a link");
            expected.push(image);
            continue;
        }
        let (image, runs, last) = tree_page(id);
        let block = writer
            .append(id, BlockContent::Base, Compression::Lz4, &image)
            .expect("a block");
        table
            .push(id, Link::base(version(1), block))
            .expect("a link");
        for (born, run) in (2..).zip(&runs) {
            let compression = if born == 3 {
                Compression::Raw
            } else {
                Compression::Lz4
            };
            let block = writer
                .append(id, BlockContent::Delta, compression, run)
                .expect("a block");
            table
                .push(id, Link::delta(version(born), block))
                .expect("a link");
        }
        expected.push(last);
    }

    let pool = BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: 2 * (SIZE.get() + 512),
    })
    .expect("a pool");
    assert_eq!(pool.frames(), 2);
    let loader = PageLoader::new(&pool, &table, &segments, SIZE).expect("a loader");
    let mut buffers = LoadBuffers::new();
    let mut image = Vec::with_capacity(SIZE.get());
    let mut check = |buffers: &mut LoadBuffers| {
        for (raw, expected) in (1..=PAGES).zip(&expected) {
            let frame = loader
                .get(page(raw), version(4), buffers)
                .expect("the page loads");
            image.clear();
            if raw == PAGES {
                image.extend_from_slice(&frame[..expected.len()]);
            } else {
                TreePage::from_validated(&frame, FRAME_FORMAT)
                    .expect("a tree page")
                    .write_image(&mut image);
            }
            assert!(image == *expected, "page {raw}");
        }
    };

    check(&mut buffers);
    let loads = pool.stats().misses;
    let before = ALLOCATIONS.get();
    check(&mut buffers);
    check(&mut buffers);
    let allocations = ALLOCATIONS.get() - before;
    let stats = pool.stats();
    assert_eq!(stats.misses - loads, 2 * u64::from(PAGES));
    assert_eq!((stats.hits, stats.overflows), (0, 0));
    assert_eq!(allocations, 0, "{} loads", stats.misses - loads);
}
