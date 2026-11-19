//! "Loading a page computes no hash": the pages of a grove go through the
//! store, and what comes back holds every hash as it was evaluated.

use std::sync::Arc;

use lignin_format::{
    delta::DeltaRunWriter,
    manifest::Manifest,
    segment::{BlockContent, Compression},
    CommitVersion,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cache::{BufferPool, PoolConfig},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
};
use tempfile::TempDir;

use super::{
    acceptance::{key, nested, value},
    *,
};
use crate::node::{NodeReader, OperationGuard};

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// What an operation of this test holds while it reads.
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

    /// The commit `born` wrote the tree page `page`, and write-back stored
    /// its image.
    fn base(&mut self, born: u64, page: NodePage<'_>) {
        let image = page.tree_page().to_image();
        let block = self
            .writer
            .append(page.id(), BlockContent::Base, Compression::Lz4, &image)
            .expect("a block");
        self.table
            .push(page.id(), Link::base(version(born), block))
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
}

/// The committed pages of a store, as one reader has them.
struct Loaded<'r> {
    reader: NodeReader<'r, Guard>,
    buffers: LoadBuffers,
    read: Vec<PageId>,
}

impl Pages for Loaded<'_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        let pinned = self.reader.pin(page, &mut self.buffers)?;
        self.read.push(page);
        Ok(read(pinned.view()))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        _: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        Err(FragmentError::Pages(format!("{page} is committed").into()))
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        Err(FragmentError::Pages("a reader starts no page".into()))
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        Err(FragmentError::Pages(format!("{page} is committed").into()))
    }
}

#[test]
fn loading_a_page_computes_no_hash() {
    // Version 1: a grove whose hashes were all evaluated, as base images.
    let (mut grove, _, b) = nested();
    let mut disk = Disk::new();
    for &id in grove.mem.frames.keys() {
        disk.base(1, grove.mem.view(id));
    }

    // Version 2: one value changes where it is, the hashes on its root path
    // are marked and evaluated, and the commit's records are the value and
    // one hash record for each hash that changed.
    let changed = key(b, 77);
    let path = grove.root_path(b, &changed);
    let at = grove.at(b, &changed);
    // The feature of the tests' nodes says how long the value is, so the
    // new value is as long as the old one and only the hashes follow it.
    let new_value = value(9_999, grove.nodes[path[0]].value.len());
    grove
        .mem
        .write(at.page(), |page| {
            let mut node = page.node_mut(at.slot()).expect("the node");
            node.set_value(CellValue::Inline(&new_value))
        })
        .expect("the value is set");
    grove.nodes[path[0]].value.clone_from(&new_value);
    grove.hashes.mark(at, HashField::Value);
    for pair in path.windows(2) {
        let (below, above) = (&grove.nodes[pair[0]], &grove.nodes[pair[1]]);
        let field = match (above.merk == below.merk, above.left == Some(pair[0])) {
            (false, _) => HashField::Value,
            (true, true) => HashField::Child(Side::Left),
            (true, false) => HashField::Child(Side::Right),
        };
        grove.hashes.mark(above.at.expect("a cell"), field);
    }
    let records = grove.mem.shadow.hashes.len();
    let root_hash = grove.root_hash();
    let stored: Vec<HashRecord> = grove.mem.shadow.hashes[records..].to_vec();
    let mut changes: BTreeMap<PageId, Vec<Delta<'_>>> = BTreeMap::new();
    changes.entry(at.page()).or_default().push(Delta::Value {
        slot: at.slot(),
        value: CellValue::Inline(&new_value),
    });
    for record in &stored {
        let run = changes.entry(record.at.page()).or_default();
        run.push(Delta::Hash {
            slot: record.at.slot(),
            field: record.field,
            hash: &record.hash,
        });
    }
    assert!(changes.len() > 1, "the hashes changed in several pages");
    for (id, run) in &changes {
        disk.delta(*id, 2, run);
    }
    grove.check_marks();

    // Every page is loaded, at both versions, and every node of it is read.
    let pool = BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: 4 << 20,
    })
    .expect("a pool");
    let loader = PageLoader::new(&pool, &disk.table, &disk.segments, SIZE).expect("a loader");
    let calls = grove.rules.calls.len();
    let guard = Guard;
    let mut nodes = 0;
    for at_version in [1, 2] {
        let reader = NodeReader::new(loader, &guard, Lease::take(), version(at_version));
        let mut buffers = LoadBuffers::new();
        for &id in grove.mem.frames.keys() {
            let pinned = reader.pin(id, &mut buffers).expect("the page loads");
            let (loaded, held) = (pinned.view(), grove.mem.view(id));
            for (node, kept) in loaded.nodes().zip(held.nodes()) {
                nodes += 1;
                if at_version == 1 {
                    continue;
                }
                assert_eq!(node.cell(), kept.cell(), "the node at {}", node.address());
                for field in HashField::ALL {
                    assert_eq!(node.hash(field), kept.hash(field));
                }
            }
            if at_version == 2 {
                let (loaded, held) = (loaded.tree_page(), held.tree_page());
                assert_eq!(loaded.to_image(), held.to_image(), "{id} with its records");
            }
        }
    }
    assert_eq!(nodes, 2 * grove.cells().len());
    // A page that the second commit did not change is one page version.
    let versions = grove.mem.frames.len() + changes.len();
    assert_eq!(pool.stats().misses as usize, versions);
    assert_eq!(grove.rules.calls.len(), calls, "loading computes no hash");

    // A scope over the loaded pages has no mark, so it evaluates nothing
    // and reads nothing.
    let reader = NodeReader::new(loader, &guard, Lease::take(), version(2));
    let mut loaded = Loaded {
        reader,
        buffers: LoadBuffers::new(),
        read: Vec::new(),
    };
    let mut hashes = Hashes::new();
    let root = grove.root_at.expect("a root");
    hashes
        .evaluate_all(&mut loaded, &mut (), &mut grove.rules)
        .expect("nothing to do");
    hashes
        .evaluate(&mut loaded, &mut (), &mut grove.rules, root)
        .expect("nothing to do");
    assert_eq!(grove.rules.calls.len(), calls);
    assert!(loaded.read.is_empty());
    assert_eq!(*hashes.counters(), Counters::default());

    // The root hash is in no cell: asked for, it is one hash of the root's
    // stored hashes, and it is the one the writer had.
    let asked = hashes.hash_of(&mut loaded, &mut (), &mut grove.rules, root);
    assert_eq!(asked.expect("the root's cell is read"), root_hash);
    assert_eq!(root_hash, grove.expected_root());
    let root_key = grove.nodes[*path.last().expect("a root")].key.clone();
    assert_eq!(grove.rules.since(calls), [(Rule::Node, root_key)]);
    assert_eq!(loaded.read, [root.page()]);

    // And every hash that was loaded equals its recomputation.
    for (_, at) in grove.cells() {
        let differing = check(&mut loaded, &mut (), &mut Plain, at);
        assert_eq!(differing.expect("the cell is recomputed"), Stale::NONE);
    }
    grove.check_hashes();
}
