use std::sync::Arc;

use lignin_format::{
    delta::{Delta, DeltaRunWriter},
    flat_page::{FlatLeafBuilder, FlatValue},
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{HashField, Side},
    value_log::ValuePointer,
    FileId, FormatError,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cache::{BufferPool, OwnerId, PoolConfig},
    load::{LoadBuffers, LoadError, PageLoader},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
};
use tempfile::TempDir;

use super::*;

/// What an operation of these tests holds while it reads. Nothing is
/// reclaimed in them.
struct Guard;

impl OperationGuard for Guard {}

/// The segments and the page table of a store.
struct Disk {
    _dir: TempDir,
    _manifest: ManifestStore,
    segments: SegmentStore,
    writer: SegmentWriter,
    table: PageTable,
}

impl Disk {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest = ManifestStore::create(vfs, dir.path(), empty).expect("a manifest");
        let segments = SegmentStore::open(&manifest).expect("the segments");
        let writer = segments.create().expect("a segment");
        segments
            .commit(&mut manifest)
            .expect("the segment is named");
        Self {
            _dir: dir,
            _manifest: manifest,
            segments,
            writer,
            table: PageTable::new(),
        }
    }

    /// The commit `born` wrote the tree page in `frame`, and write-back
    /// stored its image.
    fn base(&mut self, born: u64, frame: &[u8]) {
        let nodes = NodePage::open(frame, Lease::take()).expect("a tree page");
        self.image(nodes.id(), born, &nodes.tree_page().to_image());
    }

    fn image(&mut self, id: PageId, born: u64, image: &[u8]) {
        let block = self
            .writer
            .append(id, BlockContent::Base, Compression::Lz4, image)
            .expect("a block");
        self.table
            .push(id, Link::base(version(born), block))
            .expect("a link");
    }

    /// The commit `born` made `changes` to the page `id`.
    fn delta(&mut self, id: PageId, born: u64, changes: &[Delta<'_>]) {
        let mut run = Vec::new();
        let mut records = DeltaRunWriter::new(id, None, &mut run);
        for change in changes {
            records.push(change).expect("a record");
        }
        records.finish().expect("a run");
        let block = self
            .writer
            .append(id, BlockContent::Delta, Compression::Raw, &run)
            .expect("a block");
        self.table
            .push(id, Link::delta(version(born), block))
            .expect("a link");
    }

    fn loader<'s>(&'s self, pool: &'s BufferPool) -> PageLoader<'s> {
        PageLoader::new(pool, &self.table, &self.segments, SIZE).expect("a loader")
    }
}

fn pool() -> BufferPool {
    BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: 1 << 20,
    })
    .expect("a pool")
}

/// A page with three nodes: `root` in slot 1, with `left` in slot 2 of the
/// same page as its left child and a right child in page 9, and `spare` in
/// slot 3.
fn family(id: PageId) -> Vec<u8> {
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take()).expect("a page");
    let hash = [0x61; 32];
    nodes
        .insert(&CellContent {
            left: Some(link(CellAddress::new(id, slot(2)), &hash, &[])),
            right: Some(link(address(9, 1), &hash, &[])),
            ..leaf(b"root", &[1], b"root value")
        })
        .expect("fits");
    nodes
        .insert(&CellContent {
            fragment_root: false,
            ..leaf(b"left", &[2], b"left value")
        })
        .expect("fits");
    nodes
        .insert(&leaf(b"spare", &[3], b"spare value"))
        .expect("fits");
    frame
}

fn flat(id: PageId) -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(id, b"", SIZE.image_capacity()).expect("a leaf");
    leaf.push(b"key", FlatValue::Inline(b"value"))
        .expect("fits");
    leaf.finish()
}

#[test]
fn a_node_is_read_from_a_page_that_is_loaded_and_then_from_the_pool() {
    let mut disk = Disk::new();
    disk.base(1, &family(page(7)));
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let reader = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
    assert_eq!((reader.at(), reader.lease()), (version(1), lease));
    assert!(std::ptr::eq(reader.guard(), &guard));
    assert_eq!(
        format!("{reader:?}"),
        format!(
            "NodeReader {{ at: {:?}, lease: {lease:?}, .. }}",
            version(1)
        )
    );
    let mut buffers = LoadBuffers::new();

    let pinned = reader.pin(page(7), &mut buffers).expect("the page loads");
    assert_eq!((pinned.id(), pinned.born()), (page(7), version(1)));
    assert_eq!(pinned.frame().page(), page(7));
    assert_eq!((pool.stats().misses, pool.stats().hits), (1, 0));
    let root = pinned.node(slot(1)).expect("a node");
    assert_eq!(
        (root.key(), root.value()),
        (&b"root"[..], CellValue::Inline(b"root value"))
    );
    assert_eq!(pinned.nodes().len(), 3);
    assert_eq!(pinned.resolve(root.handle()), Some(root));

    let child = root.link(Side::Left).expect("a link").child;
    let copy = reader;
    let left = copy
        .node(child, &mut buffers)
        .expect("the address is exact");
    assert_eq!((pool.stats().misses, pool.stats().hits), (1, 1));
    assert_eq!(left.node().key(), b"left");
    assert_eq!(left.handle().address(), child);
    assert_eq!(left.page().id(), page(7));

    let again = left.clone().into_page();
    drop((pinned, left));
    assert_eq!(again.clone().node(slot(3)).expect("a node").key(), b"spare");
}

#[test]
fn a_stored_address_must_name_a_node_of_a_tree_page() {
    let mut disk = Disk::new();
    disk.base(1, &family(page(7)));
    disk.image(page(8), 1, &flat(page(8)));
    let pool = pool();
    let guard: &dyn OperationGuard = &Guard;
    let reader = NodeReader::new(disk.loader(&pool), guard, Lease::take(), version(1));
    let mut buffers = LoadBuffers::new();

    let error = reader
        .node(address(7, 4), &mut buffers)
        .expect_err("a free slot");
    assert!(matches!(error, NodeError::NoNode { address: at } if at == address(7, 4)));

    let error = reader
        .node(address(9, 1), &mut buffers)
        .expect_err("no page");
    assert!(matches!(
        error,
        NodeError::Load(LoadError::NoPage { freed: None, .. })
    ));
    assert_eq!(
        error.to_string(),
        format!(
            "page load: {}",
            LoadError::NoPage {
                page: page(9),
                at: version(1),
                freed: None
            }
        )
    );
    assert!(std::error::Error::source(&error).is_some());

    let error = reader.pin(page(8), &mut buffers).expect_err("a flat page");
    assert!(matches!(
        error,
        NodeError::Page(FormatError::UnknownRecordKind { .. })
    ));
}

#[test]
fn a_handle_is_followed_while_its_page_keeps_its_cells() {
    let mut disk = Disk::new();
    disk.base(1, &family(page(7)));
    disk.delta(
        page(7),
        2,
        &[Delta::Hash {
            slot: slot(2),
            field: HashField::Value,
            hash: &[0x99; 32],
        }],
    );
    disk.delta(page(7), 3, &[Delta::Remove { slot: slot(3) }]);
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let loader = disk.loader(&pool);
    let reader = |at| NodeReader::new(loader, &guard, lease, version(at));
    let mut buffers = LoadBuffers::new();

    let first = reader(1)
        .node(address(7, 2), &mut buffers)
        .expect("the page loads");
    let handle = first.handle();
    assert_eq!(first.node().value_hash(), &[0x11; 32]);

    let found = reader(1)
        .resolve(handle, &mut buffers)
        .expect("the page is in the pool")
        .expect("the same page version");
    assert_eq!(found.node(), first.node());

    let found = reader(2)
        .resolve(handle, &mut buffers)
        .expect("the page loads")
        .expect("no cell left the page in commit 2");
    assert_eq!(found.page().born(), version(2));
    assert_eq!(found.node().key(), b"left");
    assert_eq!(found.node().value_hash(), &[0x99; 32]);
    assert_eq!(found.handle(), handle);

    let gone = reader(3)
        .resolve(handle, &mut buffers)
        .expect("the page loads");
    assert!(gone.is_none());
    let renewed = reader(3)
        .node(handle.address(), &mut buffers)
        .expect("the node is where it was")
        .handle();
    assert_eq!(
        (renewed.address(), renewed.departures()),
        (handle.address(), 1)
    );
    assert!(reader(3)
        .resolve(renewed, &mut buffers)
        .expect("the page is in the pool")
        .is_some());

    let (hits, misses) = (pool.stats().hits, pool.stats().misses);
    let other = NodeReader::new(loader, &guard, Lease::take(), version(1));
    assert!(other
        .resolve(handle, &mut buffers)
        .expect("nothing is read")
        .is_none());
    assert_eq!((pool.stats().hits, pool.stats().misses), (hits, misses));
}

/// A page id is given to another page once the first is freed and no reader
/// can reach it. The new page counts its departures from 0, so the count of
/// a handle into the old page can be the new page's count. The lease is
/// what tells them apart.
#[test]
fn a_handle_does_not_reach_the_page_that_took_its_id() {
    let mut disk = Disk::new();
    let id = page(7);
    disk.base(1, &family(id));
    let pool = pool();
    let guard = Guard;
    let loader = disk.loader(&pool);
    let mut buffers = LoadBuffers::new();

    // A snapshot of version 1 keeps a handle into the page.
    let snapshot = Lease::take();
    let old = NodeReader::new(loader, &guard, snapshot, version(1))
        .node(address(7, 1), &mut buffers)
        .expect("the page loads");
    let handle = old.handle();
    assert_eq!((old.node().key(), handle.departures()), (&b"root"[..], 0));
    drop(old);

    // Commit 2 frees the page, and commit 3 gives its id to a new page,
    // whose slot 1 holds another node.
    disk.table.push(id, Link::free(version(2))).expect("a link");
    pool.free(id, version(2)).expect("a later version");
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take()).expect("a page");
    nodes.insert(&leaf(b"usurper", &[7], b"")).expect("fits");
    disk.base(3, &frame);
    let loader = disk.loader(&pool);

    // The snapshot still reads its own page, and its handle is good there.
    let reader = NodeReader::new(loader, &guard, snapshot, version(1));
    let found = reader
        .resolve(handle, &mut buffers)
        .expect("version 1 of the page is kept")
        .expect("the handle's own page");
    assert_eq!(found.node().key(), b"root");
    drop(found);

    // At version 2 there is no such page.
    let reader = NodeReader::new(loader, &guard, snapshot, version(2));
    assert!(reader
        .resolve(handle, &mut buffers)
        .expect("a page that was freed is no error")
        .is_none());
    let error = reader
        .pin(id, &mut buffers)
        .expect_err("the page was freed");
    assert!(matches!(
        error,
        NodeError::Load(LoadError::NoPage { freed: Some(at), .. }) if at == version(2)
    ));

    // A reader of version 3 has another lease: whoever read at version 1
    // and now reads at 3 has let go of version 1, or the id could not have
    // been given away. The handle is refused, though the new page has its
    // count and a cell in its slot.
    let reader = NodeReader::new(loader, &guard, Lease::take(), version(3));
    let usurper = reader
        .node(handle.address(), &mut buffers)
        .expect("the new page loads");
    assert_eq!(usurper.node().key(), b"usurper");
    assert_eq!(usurper.handle().departures(), handle.departures());
    assert_ne!(usurper.handle(), handle);
    assert!(usurper.page().resolve(handle).is_none());
    assert!(reader
        .resolve(handle, &mut buffers)
        .expect("nothing is read")
        .is_none());
    drop(usurper);

    // The id can also go to a flat page, which has no nodes at all.
    disk.table.push(id, Link::free(version(4))).expect("a link");
    pool.free(id, version(4)).expect("a later version");
    disk.image(id, 5, &flat(id));
    let loader = disk.loader(&pool);
    let reader = NodeReader::new(loader, &guard, snapshot, version(5));
    assert!(reader
        .resolve(handle, &mut buffers)
        .expect("a flat page is no error")
        .is_none());
}

#[test]
fn a_write_transaction_changes_its_private_copy_under_the_copys_latch() {
    let mut disk = Disk::new();
    let id = page(7);
    disk.base(1, &family(id));
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let loader = disk.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let owner = OwnerId::new(1);

    // The transaction reads the committed page, and keeps a handle.
    let reader = NodeReader::new(loader, &guard, lease, version(1));
    let committed = reader.pin(id, &mut buffers).expect("the page loads");
    let left = committed.node(slot(2)).expect("a node").handle();

    // It changes a node of the page, in its own copy. The handle is good in
    // the copy: the copy is the page, one step on.
    let copy = pool
        .make_private(owner, id, version(1))
        .expect("the page is pinned");
    {
        let mut latch = copy.write().expect("the copy is private");
        let mut nodes = NodePageMut::open(&mut latch, SIZE, lease).expect("a tree page");
        let mut node = nodes.resolve_mut(left).expect("no cell has left the page");
        node.set_value(CellValue::Inline(b"a new and longer value"))
            .expect("the page has room");
        node.set_hash(HashField::Value, &[0x77; 32])
            .expect("a hash");
    }
    {
        let latch = copy.read();
        let nodes = NodePage::open(&latch, lease).expect("a tree page");
        let node = nodes.resolve(left).expect("good");
        assert_eq!(node.value(), CellValue::Inline(b"a new and longer value"));
    }
    assert_eq!(
        committed.resolve(left).expect("good").value(),
        CellValue::Inline(b"left value")
    );

    // It creates a page, and puts a node into it.
    let created = pool.new_private(owner, page(12)).expect("a new page");
    let fresh = {
        let mut latch = created.write().expect("the copy is private");
        let mut nodes = NodePageMut::create(&mut latch, SIZE, page(12), lease).expect("a page");
        nodes.insert(&leaf(b"fresh", &[4], b"new")).expect("fits")
    };

    // Its commit publishes both, and a reader of the new version follows
    // the handles of the lease: nothing went back.
    drop(committed);
    pool.install(&copy, version(2)).expect("the commit");
    pool.install(&created, version(2)).expect("the commit");
    let reader = NodeReader::new(loader, &guard, lease, version(2));
    let found = reader
        .resolve(left, &mut buffers)
        .expect("the page is in the pool")
        .expect("no cell left the page");
    assert_eq!(found.page().born(), version(2));
    assert_eq!(found.node().value_hash(), &[0x77; 32]);
    let found = reader
        .resolve(fresh, &mut buffers)
        .expect("the page is in the pool")
        .expect("good");
    assert_eq!(found.node().key(), b"fresh");

    // A reader of version 1 still has the page as it was.
    let reader = NodeReader::new(loader, &guard, lease, version(1));
    let before = reader
        .resolve(left, &mut buffers)
        .expect("version 1 is kept")
        .expect("good");
    assert_eq!(before.node().value(), CellValue::Inline(b"left value"));
    assert!(reader
        .resolve(fresh, &mut buffers)
        .expect("a page that is not there yet is no error")
        .is_none());
}

/// Background work changes how a page version is stored and never what it
/// says: it takes no cell out of a page, so the handles into the page stay
/// good.
#[test]
fn a_page_step_keeps_the_handles_into_its_page() {
    let mut disk = Disk::new();
    let id = page(7);
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take()).expect("a page");
    let old = ValuePointer::new(FileId::MIN, 4_096, 5_000).expect("a value pointer");
    let new = ValuePointer::new(FileId::MIN, 90_000, 5_000).expect("a value pointer");
    nodes
        .insert(&CellContent {
            value: CellValue::OutOfLine(old),
            ..leaf(b"large", &[1], b"")
        })
        .expect("fits");
    disk.base(1, &frame);
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let reader = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
    let mut buffers = LoadBuffers::new();
    let before = reader
        .node(address(7, 1), &mut buffers)
        .expect("the page loads");
    let handle = before.handle();

    // The value log's cleaner moves the value, and rewrites its pointer.
    let step = pool.page_step(id);
    let current = step.current().expect("the page is in the pool");
    let mut blank = pool.blank();
    blank.copy_from(&current);
    NodePageMut::open(&mut blank, SIZE, Lease::take())
        .expect("a tree page")
        .node_mut(slot(1))
        .expect("a node")
        .set_value(CellValue::OutOfLine(new))
        .expect("a pointer in the room of a pointer");
    step.replace(&current, blank)
        .expect("the step goes through");
    drop((step, current));

    let after = reader
        .resolve(handle, &mut buffers)
        .expect("the page is in the pool")
        .expect("no cell left the page");
    assert_eq!(after.node().value(), CellValue::OutOfLine(new));
    assert_eq!(after.handle(), handle);
    assert_eq!(before.node().value(), CellValue::OutOfLine(old));
}

#[test]
fn a_page_that_cannot_be_read_is_an_error_and_not_a_stale_handle() {
    let mut disk = Disk::new();
    let image = NodePage::open(&family(page(7)), Lease::take())
        .expect("a tree page")
        .tree_page()
        .to_image();
    disk.image(page(8), 1, &image);
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let reader = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
    let mut buffers = LoadBuffers::new();

    let handle = NodeHandle::new(address(8, 1), 0, lease);
    let error = reader
        .resolve(handle, &mut buffers)
        .expect_err("the block holds another page");
    assert!(matches!(
        error,
        NodeError::Load(LoadError::Payload { page: at, .. }) if at == page(8)
    ));
    let error = reader
        .node(handle.address(), &mut buffers)
        .expect_err("the block holds another page");
    assert!(matches!(error, NodeError::Load(LoadError::Payload { .. })));
}
