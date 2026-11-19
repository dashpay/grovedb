//! Acceptance test of the bulk loader (TREE-10).
//!
//! A grove that GroveDB built on RocksDB is read node by node, a child
//! before its parent, and handed to `lignin_tree::bulk::Loader`:
//!
//! - the rules that make the hashes are Merk's own functions, so the root
//!   hash the load ends with is compared with the one GroveDB states
//! - a page the load has left is written to a segment file as an image, and
//!   the values out of line to a value log, each for the page that owns it
//! - what was written is read back through the store, node by node, against
//!   the records of GroveDB
//!
//! `the_reference_workload_loads_within_the_time_and_fill` is the task's
//! "done when": one million documents and their references. The time is
//! that of the load: the calls of the loader, writing its pages and values
//! to their files, and the sync. Reading the grove from RocksDB, which is
//! the stream's side, is not in it. It is ignored
//! by default, wants a release build, and builds the grove on RocksDB
//! first, which takes most of an hour. `BULK_LOAD_DIR` names a directory
//! for it (about 3 GB); a grove that was built there is used again.
//!
//! ```text
//! BULK_LOAD_DIR=/dev/shm/bulk-load cargo test --release -p lignin-testkit --test bulk_load -- \
//!     --ignored --nocapture the_reference_workload
//! ```
//!
//! `BULK_LOAD_STORE` names another directory for the store the load is
//! written to, to have it on a disk. Two more settings are for measuring
//! the load in instructions, as the difference of two runs under `perf
//! stat`: with `BULK_LOAD_ONLY_READ=1` the grove is read and nothing is
//! loaded, and with `BULK_LOAD_NO_READ_BACK=1` it is read and loaded, and
//! the store is not read back.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use grovedb::{Element, GroveDb, Transaction};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    tree::{
        combine_hash, kv::ValueDefinedCostType, kv_digest_to_kv_hash, node_hash, value_hash,
        AggregateData, Link, TreeFeatureType, TreeNode, NULL_HASH,
    },
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::RocksDbStorage, Storage, StorageContext};
use grovedb_version::version::GroveVersion;
use lignin_format::{
    delta::Delta,
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellValue, Hash, Side, TreePage},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    CellAddress, CommitVersion, PageId, PageSize,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cache::{BufferPool, PoolConfig},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link as TableLink, PageTable},
    values::{ValueHold, ValueStore, ValueWriter},
};
use lignin_testkit::workload::{apply::Transactions, drive::Documents};
use lignin_tree::{
    bulk::{HostedLoad, LinkLoad, LoadParams, LoadValues, Loaded, Loader, NodeLoad},
    fragment::{FragmentError, Layout, PageMut, Pages, Values},
    hash::{HashRules, Refusal},
    node::{
        ChangeSink, Lease, NodeError, NodePage, NodePageMut, NodeReader, NodeRef, OperationGuard,
    },
    value::InlinePolicy,
};

const SEED: u64 = 1;
const SIZE: PageSize = PageSize::DEFAULT;

/// GroveDB recurses once per level, and so does the walk of this test.
const STACK_BYTES: usize = 64 << 20;

/// A segment takes this many pages before the next one is started.
const PAGES_IN_A_SEGMENT: usize = 2_000;

type NoValueDefinedCost = fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>;
type Context<'db> = <RocksDbStorage as Storage<'db>>::BatchTransactionalStorageContext;

fn on_a_big_stack<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    thread::scope(|scope| {
        thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, body)
            .expect("spawn")
            .join()
            .expect("the load does not panic")
    })
}

/// The grove of `workload` on RocksDB in `dir`, built there unless an
/// earlier run left it.
fn grove(dir: &Path, workload: Documents) -> GroveDb {
    let version = GroveVersion::latest();
    let built = dir.join(format!("built-{}-{}", workload.documents, workload.owners));
    let rocks = dir.join("rocksdb");
    if built.exists() {
        return GroveDb::open(&rocks).expect("the grove opens");
    }
    if rocks.exists() {
        std::fs::remove_dir_all(&rocks).expect("a grove that was not finished is removed");
    }
    let db = GroveDb::open(&rocks).expect("a grove");
    let started = Instant::now();
    let mut transactions = Transactions::new(&db);
    for (applied, op) in workload.stream(SEED).enumerate() {
        transactions
            .apply(&op, version)
            .unwrap()
            .expect("the workload applies");
        if applied % 250_000 == 0 && applied > 0 {
            eprintln!(
                "building the grove: {applied} operations, {:.0} s",
                started.elapsed().as_secs_f64()
            );
        }
    }
    drop(transactions);
    db.flush().expect("the grove is flushed");
    std::fs::write(&built, b"").expect("the mark of a grove that is built");
    db
}

/// The rules of Merk for a tree with no aggregate, by its own functions.
struct MerkRules;

impl HashRules for MerkRules {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        let own = value_hash(value).unwrap();
        match hosted {
            [] => Ok(own),
            [root] => Ok(combine_hash(&own, &root.unwrap_or(NULL_HASH)).unwrap()),
            _ => Err("an element that hosts several Merks".into()),
        }
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        Ok(kv_digest_to_kv_hash(key, value_hash).unwrap())
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        let child = |side| node.link(side).map_or(&NULL_HASH, |link| link.hash);
        Ok(node_hash(node.kv_hash(), child(Side::Left), child(Side::Right)).unwrap())
    }
}

/// The feature of a node as a cell stores it (`tree-page.md` TP12). The
/// grove of this test has no tree with an aggregate, and neither have the
/// rules.
fn feature_bytes(feature: TreeFeatureType) -> Vec<u8> {
    match feature {
        TreeFeatureType::BasicMerkNode => vec![0],
        other => unreachable!("a node of {other:?} in a grove of plain trees"),
    }
}

fn aggregate_bytes(aggregate: AggregateData) -> Vec<u8> {
    match aggregate {
        AggregateData::NoAggregateData => vec![0],
        other => unreachable!("a link with {other:?} in a grove of plain trees"),
    }
}

/// The root key of the Merk an element hosts, if it hosts one.
fn hosted_root(element: &Element) -> Option<Option<Vec<u8>>> {
    let element = element.underlying();
    if element.uses_non_merk_data_storage() {
        return None;
    }
    let (root_key, _) = element.root_key_and_tree_type()?;
    Some(root_key.clone())
}

/// What a link of Merk states of the child.
fn link_parts(link: &Link) -> (&[u8], Hash, (u8, u8), Vec<u8>) {
    let Link::Reference {
        key,
        hash,
        child_heights,
        aggregate_data,
    } = link
    else {
        unreachable!("a record that was decoded has links to records");
    };
    (key, *hash, *child_heights, aggregate_bytes(*aggregate_data))
}

/// No one is told of the changes to a page that is being loaded: the page
/// is written out as an image.
struct NoSink;

impl ChangeSink for NoSink {
    fn created(&mut self, _: PageId, _: u64) {}

    fn changed(&mut self, _: PageId, _: &Delta<'_>) {}
}

/// The files of a store that a load is written to.
struct Disk {
    manifest: ManifestStore,
    segments: SegmentStore,
    segment: SegmentWriter,
    values: ValueStore,
    value_file: ValueWriter,
    hold: ValueHold,
    table: PageTable,
    segment_bytes: u64,
    value_log_bytes: u64,
}

impl Disk {
    fn new(dir: &Path) -> Self {
        if dir.exists() {
            std::fs::remove_dir_all(dir).expect("the store of an earlier run is removed");
        }
        std::fs::create_dir_all(dir).expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest = ManifestStore::create(vfs, dir, empty).expect("a manifest");
        let segments = SegmentStore::open(&manifest).expect("the segments");
        let segment = segments.create().expect("a segment");
        segments
            .commit(&mut manifest)
            .expect("the segment is named");
        let values = ValueStore::open(&manifest, &[]).expect("the value log");
        let value_file = values.create().expect("a value file");
        values.commit(&mut manifest).expect("the file is named");
        let hold = values.hold_files();
        Self {
            manifest,
            segments,
            segment,
            values,
            value_file,
            hold,
            table: PageTable::new(),
            segment_bytes: 0,
            value_log_bytes: 0,
        }
    }

    fn value(&mut self, owner: PageId, value: &[u8]) -> ValuePointer {
        if self.value_file.is_full() {
            let next = self.values.create().expect("a value file");
            self.values
                .commit(&mut self.manifest)
                .expect("the file is named");
            let full = std::mem::replace(&mut self.value_file, next);
            self.value_log_bytes += full.finish().expect("a value file is closed");
        }
        self.value_file
            .append(&mut self.hold, owner, value)
            .expect("a value is appended")
    }

    fn page(&mut self, page: PageId, image: &[u8]) {
        if self.segment.block_count() >= PAGES_IN_A_SEGMENT {
            let next = self.segments.create().expect("a segment");
            self.segments
                .commit(&mut self.manifest)
                .expect("the segment is named");
            let mut full = std::mem::replace(&mut self.segment, next);
            self.segment_bytes += full.seal().expect("a segment is sealed");
        }
        let block = self
            .segment
            .append(page, BlockContent::Base, Compression::Lz4, image)
            .expect("a block");
        let version = CommitVersion::new(1).expect("a version");
        self.table
            .push(page, TableLink::base(version, block))
            .expect("a link");
    }

    /// Makes everything durable, and returns the bytes of the segments and
    /// of the value log.
    fn sync(&mut self) -> (u64, u64) {
        let segment = self.segment.sync().expect("the segment is durable");
        let value_file = self.value_file.sync().expect("the value file is durable");
        (
            self.segment_bytes + segment,
            self.value_log_bytes + value_file,
        )
    }
}

/// The pages of a load while they are filled: frames in memory.
struct Frames {
    lease: Lease,
    filling: HashMap<PageId, Vec<u8>>,
    spare: Vec<Vec<u8>>,
    next_page: u32,
}

impl Pages for Frames {
    fn read<R>(
        &mut self,
        page: PageId,
        _: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        Err(FragmentError::Pages(
            format!("{page} is read by a load").into(),
        ))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        let frame = self
            .filling
            .get_mut(&page)
            .ok_or_else(|| FragmentError::Pages(format!("{page} was left").into()))?;
        let sink: &mut dyn ChangeSink = &mut NoSink;
        let mut page = NodePageMut::open(frame, SIZE, self.lease, sink)?;
        Ok(change(&mut page)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        self.next_page += 1;
        let id = PageId::new(self.next_page).expect("not zero");
        let mut frame = self.spare.pop().unwrap_or_else(|| vec![0; SIZE.get()]);
        NodePageMut::create(&mut frame, SIZE, id, self.lease, NoSink)?;
        self.filling.insert(id, frame);
        Ok(id)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        Err(FragmentError::Pages(
            format!("{page} is freed by a load").into(),
        ))
    }
}

/// The values out of line of the cells that are in no segment yet.
struct Held {
    values: HashMap<[u8; 8], Vec<u8>>,
    next: u64,
    policy: InlinePolicy,
}

impl Values for Held {
    fn released(&mut self, _: ValuePlace, _: PageId) {
        unreachable!("a load releases no value");
    }

    fn placed(&mut self, pending: PendingValue, _: PageId) {
        assert!(
            self.values.contains_key(&pending.id()),
            "a value that is held"
        );
    }

    fn carried(&mut self, _: ValuePlace, _: PageId, _: PageId) -> PendingValue {
        unreachable!("a load carries no cell");
    }
}

impl LoadValues for Held {
    fn hold(&mut self, value: &[u8]) -> Result<Option<PendingValue>, Refusal> {
        if self.policy.is_inline(value.len()) {
            return Ok(None);
        }
        self.next += 1;
        let id = self.next.to_le_bytes();
        self.values.insert(id, value.to_vec());
        Ok(Some(PendingValue::new(id, value.len() as u32)?))
    }
}

/// What the pages of a load look like, counted as each is written out.
#[derive(Debug, Default)]
struct Shape {
    pages: u64,
    cells: u64,
    fragments: u64,
    /// The bytes of the pages' headers, cells and slots.
    used: u64,
    /// The bytes of cells and slots in the emptiest page that the load
    /// left before it ended.
    least_used: usize,
    largest_fragment: usize,
    small_fragments: u64,
}

/// Where a load goes: frames while its pages are filled, and the files of
/// a store from then on.
struct Sink {
    layout: Layout,
    frames: Frames,
    held: Held,
    disk: Disk,
    shape: Shape,
    image: Vec<u8>,
    writing_out: Duration,
}

impl Sink {
    fn new(dir: &Path, layout: Layout) -> Self {
        Self {
            layout,
            frames: Frames {
                lease: Lease::take(),
                filling: HashMap::new(),
                spare: Vec::new(),
                next_page: 0,
            },
            held: Held {
                values: HashMap::new(),
                next: 0,
                policy: InlinePolicy::DEFAULT,
            },
            disk: Disk::new(dir),
            shape: Shape {
                least_used: usize::MAX,
                ..Shape::default()
            },
            image: Vec::new(),
            writing_out: Duration::ZERO,
        }
    }

    /// Writes out a page the load has left, as write-back does: its values
    /// go to the value log with the page as their owner, their pointers
    /// into their cells, and the image into a segment.
    fn write_out(&mut self, page: PageId, used: usize, last: bool) {
        let started = Instant::now();
        let lease = self.frames.lease;
        let mut frame = self
            .frames
            .filling
            .remove(&page)
            .expect("a page that was filled");
        let pending: Vec<_> = {
            let view = NodePage::open(&frame, lease).expect("a tree page");
            view.tree_page().pending_values().collect()
        };
        for (slot, pending) in pending {
            let value = self.held.values.remove(&pending.id());
            let pointer = self
                .disk
                .value(page, &value.expect("a value that was held"));
            let mut open = NodePageMut::open(&mut frame, SIZE, lease, NoSink).expect("a tree page");
            let mut node = open.node_mut(slot).expect("the cell");
            node.set_value(CellValue::OutOfLine(pointer))
                .expect("the pointer takes the place of the pending value");
        }
        let tree_page = NodePage::open(&frame, lease)
            .expect("a tree page")
            .tree_page();
        self.image.clear();
        tree_page.write_image(&mut self.image);
        self.disk.page(page, &self.image);
        self.writing_out += started.elapsed();

        tree_page
            .check_fragments()
            .expect("the fragments of a page");
        assert_eq!(tree_page.used_bytes() - TreePage::HEADER_LEN, used);
        assert!(!tree_page.has_pending_value());
        self.shape.pages += 1;
        self.shape.cells += tree_page.cell_count() as u64;
        self.shape.used += tree_page.used_bytes() as u64;
        if !last {
            self.shape.least_used = self.shape.least_used.min(used);
        }
        for root in tree_page.fragment_roots() {
            let (mut bytes, mut cells, mut open) = (0, 0, vec![root]);
            while let Some(slot) = open.pop() {
                let cell = tree_page.cell(slot).expect("a cell of the page");
                bytes += cell.encoded_len() + TreePage::SLOT_LEN;
                cells += 1;
                let children = Side::BOTH.into_iter().filter_map(|side| cell.link(side));
                let children = children.map(|link| link.child);
                let below = children.chain(cell.hosted_roots().flatten());
                open.extend(below.filter(|at| at.page() == page).map(|at| at.slot()));
            }
            assert!(
                bytes <= self.layout.largest_fragment() || cells == 1,
                "a fragment of {bytes} bytes"
            );
            self.shape.fragments += 1;
            self.shape.largest_fragment = self.shape.largest_fragment.max(bytes);
            self.shape.small_fragments += u64::from(bytes < self.layout.small_fragment());
        }
        self.frames.spare.push(frame);
    }
}

/// The root of a subtree that was read, as the link of its parent states
/// it.
struct Read {
    loaded: Option<Loaded>,
    hash: Hash,
    heights: (u8, u8),
    aggregate: Vec<u8>,
}

impl Read {
    /// The link to this subtree, which was loaded.
    fn link(&self) -> LinkLoad<'_> {
        LinkLoad {
            child: self.loaded.expect("a child that was loaded"),
            heights: self.heights,
            aggregate: &self.aggregate,
        }
    }
}

/// A grove on RocksDB, read record by record.
struct Source<'db> {
    storage: &'db RocksDbStorage,
    transaction: &'db Transaction<'db>,
    version: &'static GroveVersion,
}

impl<'db> Source<'db> {
    fn context(&self, path: &[Vec<u8>]) -> Context<'db> {
        let prefix = RocksDbStorage::build_prefix(SubtreePath::from(path)).unwrap();
        self.storage
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, self.transaction)
            .unwrap()
    }

    fn tree(&self, context: &Context<'db>, key: &[u8]) -> TreeNode {
        let record = context
            .get(key)
            .unwrap()
            .expect("storage answers")
            .expect("a node that is linked is stored");
        let decoded = TreeNode::decode_raw(
            &record,
            key.to_vec(),
            None::<NoValueDefinedCost>,
            self.version,
        );
        decoded.expect("a record decodes")
    }
}

/// The walk of a grove that hands every node to a loader, a child before
/// its parent and the Merk of a tree element before the element.
struct Walk<'db> {
    source: Source<'db>,
    /// `None` to read the grove and load nothing.
    load: Option<(Loader, Sink)>,
    nodes: u64,
    merks: u64,
    given: u64,
    loading: Duration,
    /// The path of each Merk that was given to its host as a subtree.
    hosted: HashMap<Loaded, Vec<Vec<u8>>>,
}

impl<'db> Walk<'db> {
    fn merk(&mut self, path: &mut Vec<Vec<u8>>, root_key: Option<&[u8]>) -> Option<Read> {
        self.merks += 1;
        let context = self.source.context(path);
        Some(self.node(&context, path, root_key?))
    }

    fn node(&mut self, context: &Context<'db>, path: &mut Vec<Vec<u8>>, key: &[u8]) -> Read {
        let tree = self.source.tree(context, key);
        let below = [true, false].map(|left| {
            let (key, hash, heights, aggregate) = link_parts(tree.link(left)?);
            let read = self.node(context, path, key);
            assert_eq!(read.hash, hash, "the hash of a child");
            assert_eq!(read.heights, heights, "the heights of a child");
            Some(Read { aggregate, ..read })
        });
        let value = tree.value_as_slice();
        let element = Element::deserialize(value, self.source.version).expect("an element");
        let mut hosted = Vec::new();
        if let Some(root_key) = hosted_root(&element) {
            path.push(key.to_vec());
            let root = self.merk(path, root_key.as_deref());
            hosted.push(match root.and_then(|root| root.loaded) {
                Some(root) => {
                    self.hosted.insert(root, path.clone());
                    HostedLoad::Loaded(root)
                }
                None => HostedLoad::Empty,
            });
            path.pop();
        }
        self.nodes += 1;
        let read = |loaded| Read {
            loaded,
            hash: tree.hash().unwrap(),
            heights: tree.child_heights(),
            aggregate: Vec::new(),
        };
        let Some((loader, sink)) = &mut self.load else {
            return read(None);
        };

        let given = element.is_reference().then_some(tree.value_hash());
        self.given += u64::from(given.is_some());
        let [left, right] = &below;
        let feature = feature_bytes(tree.feature_type());
        let node = NodeLoad {
            key,
            feature: &feature,
            value,
            value_hash: given,
            left: left.as_ref().map(Read::link),
            right: right.as_ref().map(Read::link),
            hosted: &hosted,
        };
        let started = Instant::now();
        let loaded = loader.node(&mut sink.frames, &mut sink.held, &mut MerkRules, &node);
        let loaded = loaded.expect("a node is loaded");
        self.loading += started.elapsed();
        let read = read(Some(loaded));
        assert_eq!(loader.hash_of(loaded).expect("a subtree"), read.hash);
        for page in loader.drain_pages() {
            sink.write_out(page.page, page.used, false);
        }
        read
    }
}

/// What an operation of this test holds while it reads.
struct Guard;

impl OperationGuard for Guard {}

/// The walk of a grove that compares every record with the cell that was
/// loaded for it, read back through the store.
struct Check<'db, 'r> {
    source: Source<'db>,
    reader: NodeReader<'r, Guard>,
    values: &'r ValueStore,
    buffers: LoadBuffers,
    value: Vec<u8>,
    /// Where the root of each Merk went that was given to its host as a
    /// subtree, by the Merk's path.
    reported: HashMap<Vec<Vec<u8>>, CellAddress>,
    nodes: u64,
    out_of_line: u64,
}

impl<'db> Check<'db, '_> {
    fn node(
        &mut self,
        context: &Context<'db>,
        path: &mut Vec<Vec<u8>>,
        key: &[u8],
        at: CellAddress,
    ) {
        let tree = self.source.tree(context, key);
        let element = Element::deserialize(tree.value_as_slice(), self.source.version);
        let hosted = hosted_root(&element.expect("an element"));
        let (children, root) = {
            let pinned = self.reader.node(at, &mut self.buffers).expect("a cell");
            let cell = pinned.node();
            assert_eq!(cell.key(), key);
            assert_eq!(cell.feature(), feature_bytes(tree.feature_type()));
            assert_eq!(cell.value_hash(), tree.value_hash());
            assert_eq!(cell.kv_hash(), tree.kv_hash());
            match cell.value() {
                CellValue::Inline(value) => assert_eq!(value, tree.value_as_slice()),
                CellValue::OutOfLine(pointer) => {
                    let value = self.values.read(pointer, at.page(), &mut self.value);
                    let value = value.expect("a value of the page");
                    assert_eq!(value.contiguous(), Some(tree.value_as_slice()));
                    self.out_of_line += 1;
                }
                CellValue::Pending(_) => panic!("a value that was not written out"),
            }
            let children = [(true, Side::Left), (false, Side::Right)].map(|(left, side)| {
                let (stored, link) = (tree.link(left), cell.link(side));
                assert_eq!(stored.is_some(), link.is_some(), "a child on the {side}");
                let (key, hash, heights, aggregate) = link_parts(stored?);
                let link = link?;
                assert_eq!(link.hash, &hash);
                assert_eq!(link.heights, heights);
                assert_eq!(usize::from(link.child_key_len), key.len());
                assert_eq!(link.aggregate, aggregate);
                Some((key.to_vec(), link.child))
            });
            let roots: Vec<_> = cell.hosted_roots().collect();
            assert_eq!(roots.len(), usize::from(hosted.is_some()));
            (children, roots.first().copied().flatten())
        };
        self.nodes += 1;
        for (key, child) in children.into_iter().flatten() {
            self.node(context, path, &key, child);
        }
        if let Some(root_key) = hosted {
            assert_eq!(
                root.is_some(),
                root_key.is_some(),
                "the root of a hosted Merk"
            );
            path.push(key.to_vec());
            if let (Some(root_key), Some(root)) = (root_key, root) {
                assert_eq!(self.reported.get(path), Some(&root), "the address reported");
                let context = self.source.context(path);
                self.node(&context, path, &root_key, root);
            }
            path.pop();
        }
    }
}

/// What a load of a grove came to.
#[derive(Debug)]
struct Outcome {
    nodes: u64,
    merks: u64,
    shape: Shape,
    /// The share of the pages' bytes that headers, cells and slots take:
    /// the fill of `tree-layout.md`, section 14.1.
    fill: f64,
    seconds: f64,
}

/// Loads the grove of `workload`, which is built in `dir` unless it is
/// there, into a store in `dir`, and reads it back.
fn load(dir: &Path, workload: Documents) -> Outcome {
    let version = GroveVersion::latest();
    let db = grove(dir, workload);
    let transaction = db.start_transaction();
    let source = || Source {
        storage: db.raw_storage(),
        transaction: &transaction,
        version,
    };
    let root_key = db.root_key(Some(&transaction), version).unwrap();
    let root_key = root_key.expect("the root Merk opens");
    let layout = Layout::proposed(SIZE);
    let only_read = std::env::var_os("BULK_LOAD_ONLY_READ").is_some();
    let store = std::env::var_os("BULK_LOAD_STORE").map(PathBuf::from);
    let store = store.unwrap_or_else(|| dir.join("lignin"));
    let loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
    let mut walk = Walk {
        source: source(),
        load: (!only_read).then(|| (loader, Sink::new(&store, layout))),
        nodes: 0,
        merks: 0,
        given: 0,
        loading: Duration::ZERO,
        hosted: HashMap::new(),
    };
    let started = Instant::now();
    let top = walk.merk(&mut Vec::new(), root_key.as_deref());
    let walked = started.elapsed().as_secs_f64();
    println!(
        "read {} nodes of {} Merks in {walked:.1} s",
        walk.nodes, walk.merks
    );
    let Some((mut loader, mut sink)) = walk.load.take() else {
        return Outcome {
            nodes: walk.nodes,
            merks: walk.merks,
            shape: Shape::default(),
            fill: 0.0,
            seconds: 0.0,
        };
    };

    let top = top.expect("a grove with a node");
    let started = Instant::now();
    let root = loader.root(
        &mut sink.frames,
        &mut sink.held,
        top.loaded.expect("the top"),
    );
    let root = root.expect("the root of the grove");
    let reported: Vec<_> = loader.drain_hosted().collect();
    let left: Vec<_> = loader.drain_pages().collect();
    let report = loader.finish().expect("every subtree has its place");
    let ending = started.elapsed();
    for page in left {
        sink.write_out(page.page, page.used, false);
    }
    for page in &report.pages {
        sink.write_out(page.page, page.used, true);
    }
    let started = Instant::now();
    let (segment_bytes, value_log_bytes) = sink.disk.sync();
    let syncing = started.elapsed();

    let grove_hash = db.root_hash(Some(&transaction), version).unwrap();
    assert_eq!(
        root.hash,
        grove_hash.expect("a root hash"),
        "the root hash of the grove"
    );
    assert_eq!(top.hash, root.hash);
    assert!(
        sink.held.values.is_empty(),
        "every value is in the value log"
    );
    assert!(sink.frames.filling.is_empty(), "every page is in a segment");
    let (counters, shape) = (report.counters, &sink.shape);
    assert_eq!(counters.nodes, walk.nodes);
    assert_eq!(shape.cells, walk.nodes);
    assert_eq!(shape.pages, counters.pages);
    assert_eq!(shape.fragments, counters.fragments);
    assert_eq!(counters.kv_hashes, walk.nodes);
    assert_eq!(counters.node_hashes, walk.nodes);
    assert_eq!(counters.value_hashes, walk.nodes - walk.given);
    assert_eq!(
        reported.len(),
        walk.hosted.len(),
        "every hosted root is reported"
    );

    let seconds = (walk.loading + ending + sink.writing_out + syncing).as_secs_f64();
    let fill = shape.used as f64 / (shape.pages * SIZE.get() as u64) as f64;
    let capacity = layout.capacity() as f64;
    println!(
        "loaded {} nodes into {} pages: {:.1} nodes per page, fill {:.2}% ({:.2}% of what \
         cells can use), the emptiest page that was left {:.1}% full",
        counters.nodes,
        shape.pages,
        counters.nodes as f64 / shape.pages as f64,
        fill * 100.0,
        counters.fill_percent(&layout),
        shape.least_used as f64 * 100.0 / capacity,
    );
    println!(
        "{} fragments, {:.2} to a page, the largest {} bytes of at most {}, {} below the small \
         size of {}",
        shape.fragments,
        shape.fragments as f64 / shape.pages as f64,
        shape.largest_fragment,
        layout.largest_fragment(),
        shape.small_fragments,
        layout.small_fragment(),
    );
    println!(
        "{} values out of line of {} bytes; {} value hashes given, {} made",
        counters.values, counters.value_bytes, walk.given, counters.value_hashes
    );
    println!(
        "the load took {seconds:.2} s: {:.2} s in the loader with its {} hashes, {:.2} s to \
         write {} bytes of segments and {} bytes of value log, {:.2} s to sync them",
        (walk.loading + ending).as_secs_f64(),
        counters.value_hashes + counters.kv_hashes + counters.node_hashes,
        sink.writing_out.as_secs_f64(),
        segment_bytes,
        value_log_bytes,
        syncing.as_secs_f64(),
    );

    let outcome = |shape| Outcome {
        nodes: walk.nodes,
        merks: walk.merks,
        shape,
        fill,
        seconds,
    };
    if std::env::var_os("BULK_LOAD_NO_READ_BACK").is_some() {
        return outcome(sink.shape);
    }

    let reported: HashMap<Loaded, CellAddress> = reported
        .into_iter()
        .map(|root| (root.subtree, root.address))
        .collect();
    let pool = BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: 64 << 20,
    });
    let pool = pool.expect("a pool");
    let disk = &sink.disk;
    let loader = PageLoader::new(&pool, &disk.table, &disk.segments, SIZE).expect("a loader");
    let at = CommitVersion::new(1).expect("a version");
    let mut check = Check {
        source: source(),
        reader: NodeReader::new(loader, &Guard, Lease::take(), at),
        values: &disk.values,
        buffers: LoadBuffers::new(),
        value: Vec::new(),
        reported: walk
            .hosted
            .iter()
            .map(|(subtree, path)| (path.clone(), reported[subtree]))
            .collect(),
        nodes: 0,
        out_of_line: 0,
    };
    let started = Instant::now();
    let context = check.source.context(&[]);
    let root_key = root_key.expect("a grove with a node");
    check.node(&context, &mut Vec::new(), &root_key, root.address);
    assert_eq!(check.nodes, walk.nodes, "every node is read back");
    assert_eq!(check.out_of_line, counters.values);
    println!(
        "read back {} nodes from the store in {:.1} s",
        check.nodes,
        started.elapsed().as_secs_f64()
    );
    outcome(sink.shape)
}

#[test]
fn a_grove_of_documents_is_loaded_as_grovedb_has_it() {
    let dir = tempfile::tempdir().expect("a directory");
    let workload = Documents {
        documents: 3_000,
        owners: 30,
        ..Documents::reference()
    };
    let outcome = on_a_big_stack(|| load(dir.path(), workload));
    assert_eq!(outcome.nodes, 2 * 3_000 + 2 * 30 + 7, "{outcome:?}");
    assert_eq!(outcome.merks, 2 * 30 + 7);
    if outcome.shape.pages > 0 {
        assert!(outcome.fill >= 0.80, "{outcome:?}");
    }
}

/// TREE-10's "done when": one million documents and their references load
/// within 50 seconds, into pages that are at least 90% full.
#[test]
#[ignore = "loads one million documents; run it in release mode"]
fn the_reference_workload_loads_within_the_time_and_fill() {
    let kept = std::env::var_os("BULK_LOAD_DIR").map(PathBuf::from);
    let temporary = tempfile::tempdir().expect("a directory");
    let dir = kept.as_deref().unwrap_or(temporary.path());
    std::fs::create_dir_all(dir).expect("the directory of the load");
    let outcome = on_a_big_stack(|| load(dir, Documents::reference()));
    assert_eq!(outcome.nodes, 2_020_007, "{outcome:?}");
    assert_eq!(outcome.merks, 20_007);
    if outcome.shape.pages == 0 {
        return;
    }
    assert!(outcome.fill >= 0.90, "fill {:.4}", outcome.fill);
    let capacity = Layout::proposed(SIZE).capacity();
    assert!(
        outcome.shape.least_used * 10 >= capacity * 9,
        "a page with {} bytes used",
        outcome.shape.least_used
    );
    assert!(outcome.seconds <= 50.0, "{:.1} s", outcome.seconds);
}
