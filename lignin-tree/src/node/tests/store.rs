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
        let id = NodePage::open(frame, Lease::take())
            .expect("a tree page")
            .id();
        self.image(id, born, &image(frame));
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
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
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
    let root = pinned.view().node(slot(1)).expect("a node");
    assert_eq!(
        (root.key(), root.value()),
        (&b"root"[..], CellValue::Inline(b"root value"))
    );
    assert_eq!(pinned.view().len(), 3);
    assert_eq!(pinned.view().resolve(root.handle()), Some(root));

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
    assert_eq!(
        again.clone().view().node(slot(3)).expect("a node").key(),
        b"spare"
    );
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

/// A page id is given to another page once the first is freed and no pin
/// can reach it. The new page counts its departures from 0, so the count of
/// a handle into the old page can be the new page's count. The lease is
/// what tells them apart: whoever kept the handle gave its lease up when it
/// let go of the version it read at, and without that the id was not given
/// away.
#[test]
fn a_handle_does_not_reach_the_page_that_took_its_id() {
    let mut disk = Disk::new();
    let id = page(7);
    disk.base(1, &family(id));
    let pool = pool();
    let guard = Guard;
    let mut buffers = LoadBuffers::new();

    // A snapshot pins version 1 and keeps a handle into the page.
    let snapshot = Lease::take();
    let loader = disk.loader(&pool);
    let handle = NodeReader::new(loader, &guard, snapshot, version(1))
        .node(address(7, 1), &mut buffers)
        .expect("the page loads")
        .handle();
    assert_eq!(handle.departures(), 0);

    // Commit 2 frees the page. Its id waits: the snapshot can reach the
    // page, and reads it, and its handle is good.
    disk.table.push(id, Link::free(version(2))).expect("a link");
    pool.free(id, version(2)).expect("a later version");
    let loader = disk.loader(&pool);
    let found = NodeReader::new(loader, &guard, snapshot, version(1))
        .resolve(handle, &mut buffers)
        .expect("version 1 of the page is kept")
        .expect("the handle's own page");
    assert_eq!(found.node().key(), b"root");
    drop(found);

    // The snapshot ends, and with it its pin and its lease. Only now does
    // commit 3 give the id to a new page, whose slot 1 holds another node.
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
    nodes.insert(&leaf(b"usurper", &[7], b"")).expect("fits");
    disk.base(3, &frame);
    let loader = disk.loader(&pool);

    // Whoever reads at version 3 has another lease. The handle is refused,
    // and nothing is read for it.
    let reader = NodeReader::new(loader, &guard, Lease::take(), version(3));
    let usurper = reader
        .node(handle.address(), &mut buffers)
        .expect("the new page loads");
    assert_eq!(usurper.node().key(), b"usurper");
    let (hits, misses) = (pool.stats().hits, pool.stats().misses);
    assert!(reader
        .resolve(handle, &mut buffers)
        .expect("nothing is read")
        .is_none());
    assert_eq!((pool.stats().hits, pool.stats().misses), (hits, misses));
    assert!(usurper.page().view().resolve(handle).is_none());

    // The count could not have told: the new page has the handle's count
    // and a cell in the handle's slot. Under the lease that was given up,
    // the handle would lead to the wrong node.
    let new = usurper.handle();
    assert_eq!(
        (new.address(), new.departures()),
        (handle.address(), handle.departures())
    );
    let under_the_old_lease =
        NodePage::open(usurper.page().frame(), snapshot).expect("a tree page");
    let wrong = under_the_old_lease.resolve(handle).expect("the count fits");
    assert_eq!(wrong.key(), b"usurper");
}

/// A handle whose page is no page any more, or a flat page, finds nothing,
/// from a load and from the pool. That is what a holder meets that reads
/// on at the latest version with no pin, before it has taken its new lease.
#[test]
fn a_handle_finds_nothing_where_its_page_is_gone_or_flat() {
    let mut disk = Disk::new();
    let id = page(7);
    disk.base(1, &family(id));
    disk.table.push(id, Link::free(version(2))).expect("a link");
    disk.image(id, 3, &flat(id));
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let loader = disk.loader(&pool);
    let reader = |at| NodeReader::new(loader, &guard, lease, version(at));
    let mut buffers = LoadBuffers::new();
    let handle = reader(1)
        .node(address(7, 1), &mut buffers)
        .expect("the page loads")
        .handle();

    assert!(reader(2)
        .resolve(handle, &mut buffers)
        .expect("a page that was freed is no error")
        .is_none());
    let error = reader(2)
        .pin(id, &mut buffers)
        .expect_err("the page was freed");
    assert!(matches!(
        error,
        NodeError::Load(LoadError::NoPage { freed: Some(at), .. }) if at == version(2)
    ));

    let misses = pool.stats().misses;
    assert!(reader(3)
        .resolve(handle, &mut buffers)
        .expect("a flat page is no error")
        .is_none());
    assert_eq!(pool.stats().misses, misses + 1);
    let hits = pool.stats().hits;
    assert!(reader(3)
        .resolve(handle, &mut buffers)
        .expect("a flat page is no error")
        .is_none());
    assert_eq!(
        (pool.stats().hits, pool.stats().misses),
        (hits + 1, misses + 1)
    );
    let error = reader(3).pin(id, &mut buffers).expect_err("a flat page");
    assert!(matches!(
        error,
        NodeError::Page(FormatError::UnknownRecordKind { .. })
    ));
}

/// A frame of the pool that holds no page of a kind there is, or a tree
/// page of another id, is damage. It is an error for a handle as for an
/// address, never a handle that went stale.
#[test]
fn a_frame_that_holds_the_wrong_thing_is_an_error() {
    let disk = Disk::new();
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let owner = OwnerId::new(1);
    let mut buffers = LoadBuffers::new();

    let unknown = pool.new_private(owner, page(20)).expect("a new page");
    unknown.write().expect("the copy is private")[0] = 0x7F;
    pool.install(&unknown, version(1)).expect("the commit");
    let other = pool.new_private(owner, page(21)).expect("a new page");
    {
        let mut latch = other.write().expect("the copy is private");
        let mut nodes = NodePageMut::create(&mut latch, SIZE, page(22), lease, ()).expect("a page");
        nodes.insert(&leaf(b"elsewhere", &[], b"")).expect("fits");
    }
    pool.install(&other, version(1)).expect("the commit");

    let reader = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
    let handle = NodeHandle::new(address(20, 1), 0, lease);
    let error = reader
        .resolve(handle, &mut buffers)
        .expect_err("no kind of page");
    assert!(matches!(
        error,
        NodeError::Page(FormatError::UnknownRecordKind { code: 0x7F })
    ));
    let error = reader
        .pin(page(20), &mut buffers)
        .expect_err("no kind of page");
    assert!(matches!(
        error,
        NodeError::Page(FormatError::UnknownRecordKind { code: 0x7F })
    ));

    let handle = NodeHandle::new(address(21, 1), 0, lease);
    let wrong_page = |error| {
        matches!(
            error,
            NodeError::WrongPage { found, expected } if (found, expected) == (page(22), page(21))
        )
    };
    let error = reader
        .resolve(handle, &mut buffers)
        .expect_err("another page's bytes");
    assert_eq!(
        error.to_string(),
        format!("the frame of {} holds {}", page(21), page(22))
    );
    assert!(std::error::Error::source(&error).is_none());
    assert!(wrong_page(error));
    let error = reader
        .node(handle.address(), &mut buffers)
        .expect_err("another page's bytes");
    assert!(wrong_page(error));
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
    let left = committed.view().node(slot(2)).expect("a node").handle();

    // It changes a node of the page, in its own copy. The handle is good in
    // the copy: the copy is the page, one step on.
    let copy = pool
        .make_private(owner, id, version(1))
        .expect("the page is pinned");
    {
        let mut latch = copy.write().expect("the copy is private");
        let mut nodes = NodePageMut::open(&mut latch, SIZE, lease, ()).expect("a tree page");
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
        committed.view().resolve(left).expect("good").value(),
        CellValue::Inline(b"left value")
    );

    // It creates a page, and puts a node into it.
    let created = pool.new_private(owner, page(12)).expect("a new page");
    let fresh = {
        let mut latch = created.write().expect("the copy is private");
        let mut nodes = NodePageMut::create(&mut latch, SIZE, page(12), lease, ()).expect("a page");
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

/// The value log's cleaner moves a value and rewrites its pointer in a page
/// step. No cell leaves the page, so the handles into the page stay good.
#[test]
fn a_value_pointer_rewrite_keeps_the_handles_into_its_page() {
    let mut disk = Disk::new();
    let id = page(7);
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
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
    NodePageMut::open(&mut blank, SIZE, Lease::take(), ())
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
    let image = image(&family(page(7)));
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

/// A page step also takes out what a dropped subtree left in a page that it
/// shares with live cells. That is a departure: the handles into the page
/// end, those of the cells that stay among them, and a pin that was taken
/// before the step keeps the page as it was.
#[test]
fn a_page_step_that_takes_a_cell_out_ends_the_handles_into_its_page() {
    let mut disk = Disk::new();
    let id = page(7);
    let mut frame = frame();
    let mut nodes = NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
    nodes
        .insert(&leaf(b"dropped", &[1], b"left behind"))
        .expect("fits");
    nodes.insert(&leaf(b"live", &[2], b"stays")).expect("fits");
    disk.base(1, &frame);
    let pool = pool();
    let (guard, lease) = (Guard, Lease::take());
    let reader = NodeReader::new(disk.loader(&pool), &guard, lease, version(1));
    let mut buffers = LoadBuffers::new();
    let before = reader
        .node(address(7, 2), &mut buffers)
        .expect("the page loads");
    let handle = before.handle();

    let step = pool.page_step(id);
    let current = step.current().expect("the page is in the pool");
    let mut blank = pool.blank();
    blank.copy_from(&current);
    NodePageMut::open(&mut blank, SIZE, Lease::take(), ())
        .expect("a tree page")
        .node_mut(slot(1))
        .expect("a node")
        .remove()
        .expect("it leaves");
    step.replace(&current, blank)
        .expect("the step goes through");
    drop((step, current));

    assert!(reader
        .resolve(handle, &mut buffers)
        .expect("the page is in the pool")
        .is_none());
    let after = reader
        .node(handle.address(), &mut buffers)
        .expect("the live node is where it was");
    assert_eq!(after.node().key(), b"live");
    assert_eq!(after.page().view().len(), 1);
    let renewed = after.handle();
    assert_eq!(
        (renewed.address(), renewed.departures()),
        (handle.address(), 1)
    );
    assert!(reader
        .resolve(renewed, &mut buffers)
        .expect("the page is in the pool")
        .is_some());

    assert_eq!(before.page().view().len(), 2);
    assert_eq!(before.page().view().resolve(handle), Some(before.node()));
}
