mod errors;
mod model;
mod scan;
mod snapshot;
mod steps;

use std::{
    collections::{BTreeMap, BTreeSet},
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
    cache::{BufferPool, OwnerId, PoolConfig},
    load::{LoadBuffers, PageLoader},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
};
use tempfile::TempDir;

use super::*;
use crate::node::{NodePage, NodePageMut};

/// The smallest page: a tree of a few thousand nodes then has many pages.
const SIZE: PageSize = PageSize::MIN;

/// The most nodes these tests put into one page, which fit whatever their
/// keys are.
const PAGE_NODES: usize = 24;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// What an operation of these tests holds while it reads. Nothing is
/// reclaimed in them.
struct Guard;

impl OperationGuard for Guard {}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for at in (1..items.len()).rev() {
            items.swap(at, self.below(at + 1));
        }
    }
}

/// `count` different keys of up to six bytes over four byte values, in
/// order: the empty key can be among them, and keys that begin with other
/// keys.
fn keys(rng: &mut Rng, count: usize) -> Vec<Vec<u8>> {
    let mut keys = BTreeSet::new();
    while keys.len() < count {
        let len = rng.below(7);
        keys.insert((0..len).map(|_| [0, 1, 0x7F, 0xFF][rng.below(4)]).collect());
    }
    keys.into_iter().collect()
}

/// The key of four bytes that is the `number`th in order.
fn numbered_key(number: u32) -> Vec<u8> {
    number.to_be_bytes().to_vec()
}

/// `count` keys of four bytes, in order.
fn numbered(count: usize) -> Vec<Vec<u8>> {
    (0..count as u32).map(numbered_key).collect()
}

/// The addresses by page: the slots of each.
fn by_page(cells: &[CellAddress]) -> BTreeMap<PageId, Vec<SlotId>> {
    let mut slots: BTreeMap<PageId, Vec<SlotId>> = BTreeMap::new();
    for at in cells {
        slots.entry(at.page()).or_default().push(at.slot());
    }
    slots
}

/// The value these tests give the node with `key` in the tree `mark`.
fn value_of(mark: u8, key: &[u8]) -> Vec<u8> {
    let mut value = vec![mark];
    value.extend(key.iter().rev());
    value
}

/// One node of a tree, with its children by their places in the tree.
#[derive(Debug, Clone)]
struct Model {
    key: Vec<u8>,
    left: Option<usize>,
    right: Option<usize>,
}

/// A binary search tree as Merk could have shaped it.
#[derive(Debug, Clone, Default)]
struct Tree {
    nodes: Vec<Model>,
    root: Option<usize>,
    /// What tells this tree's values from another's.
    mark: u8,
}

impl Tree {
    /// A tree of `keys`, which are in order, as balanced as it can be.
    fn balanced(keys: &[Vec<u8>], mark: u8) -> Self {
        fn build(tree: &mut Tree, keys: &[Vec<u8>]) -> Option<usize> {
            let (middle, rest) = keys.split_at(keys.len() / 2);
            let (key, right) = rest.split_first()?;
            let at = tree.nodes.len();
            tree.nodes.push(Model {
                key: key.clone(),
                left: None,
                right: None,
            });
            tree.nodes[at].left = build(tree, middle);
            tree.nodes[at].right = build(tree, right);
            Some(at)
        }
        let mut tree = Self {
            mark,
            ..Self::default()
        };
        tree.root = build(&mut tree, keys);
        tree
    }

    /// A tree that took `keys` one after the other and was never balanced.
    fn grown(keys: &[Vec<u8>], mark: u8) -> Self {
        let mut tree = Self {
            mark,
            ..Self::default()
        };
        for key in keys {
            let at = tree.nodes.len();
            let mut above = tree.root;
            while let Some(parent) = above {
                let node = &mut tree.nodes[parent];
                let side = match key.cmp(&node.key) {
                    Ordering::Less => &mut node.left,
                    _ => &mut node.right,
                };
                above = *side;
                if side.is_none() {
                    *side = Some(at);
                }
            }
            tree.root.get_or_insert(at);
            tree.nodes.push(Model {
                key: key.clone(),
                left: None,
                right: None,
            });
        }
        tree
    }

    fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The keys with their values, in order.
    fn records(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let record = |node: &Model| (node.key.clone(), value_of(self.mark, &node.key));
        self.nodes.iter().map(record).collect()
    }

    /// For every node, how many nodes are above it.
    fn depths(&self) -> Vec<usize> {
        let mut depths = vec![0; self.len()];
        let mut open: Vec<usize> = self.root.into_iter().collect();
        while let Some(at) = open.pop() {
            for below in [self.nodes[at].left, self.nodes[at].right]
                .into_iter()
                .flatten()
            {
                depths[below] = depths[at] + 1;
                open.push(below);
            }
        }
        depths
    }

    /// The parent of every node but the root.
    fn parents(&self) -> Vec<Option<usize>> {
        let mut parents = vec![None; self.len()];
        for (at, node) in self.nodes.iter().enumerate() {
            for below in [node.left, node.right].into_iter().flatten() {
                parents[below] = Some(at);
            }
        }
        parents
    }

    /// The nodes with every parent before its children.
    fn top_down(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.len());
        let mut open: Vec<usize> = self.root.into_iter().collect();
        while let Some(at) = open.pop() {
            order.push(at);
            open.extend(
                [self.nodes[at].right, self.nodes[at].left]
                    .into_iter()
                    .flatten(),
            );
        }
        order
    }
}

/// Which page each node of a tree is in.
#[derive(Debug, Clone, Copy)]
enum Placement {
    /// Every node in a page of its own.
    Own,
    /// A node in the page of its parent while that has fewer than this many
    /// nodes, and otherwise in a new page: fragments, as the layout rules
    /// make them.
    Beside(usize),
    /// Every node in one of this many pages, by chance.
    Scattered(usize),
}

/// The pages of a store that these tests fill, and what they put where.
struct Layout {
    /// The first page id this layout has not used.
    next: u32,
    /// How many cells each page has been given.
    filled: BTreeMap<PageId, usize>,
}

/// One cell to put into a page: its slot there, and what it holds.
struct Planted {
    slot: SlotId,
    key: Vec<u8>,
    value: Vec<u8>,
    left: Option<(CellAddress, u8)>,
    right: Option<(CellAddress, u8)>,
    fragment_root: bool,
}

impl Planted {
    /// A cell that no cell points at, as a dropped subtree leaves them.
    fn left_over(at: CellAddress) -> Self {
        Self {
            slot: at.slot(),
            key: format!("left {at}").into_bytes(),
            value: b"dropped".to_vec(),
            left: None,
            right: None,
            fragment_root: true,
        }
    }

    fn insert_into(&self, nodes: &mut NodePageMut<'_>) {
        let hash = [0x5A; 32];
        let link = |child: &Option<(CellAddress, u8)>| {
            child.map(|(child, key_len)| TreeLink {
                child,
                hash: &hash,
                heights: (1, 1),
                child_key_len: key_len,
                aggregate: &[],
            })
        };
        let handle = nodes
            .insert(&CellContent {
                key: &self.key,
                feature: &[0],
                value_hash: &[0x11; 32],
                kv_hash: &[0x22; 32],
                left: link(&self.left),
                right: link(&self.right),
                hosted_roots: &[],
                value: CellValue::Inline(&self.value),
                fragment_root: self.fragment_root,
            })
            .expect("the page has room");
        assert_eq!(handle.slot(), self.slot, "the lowest free slot");
    }
}

/// The cells of some pages, by page.
type Cells = BTreeMap<PageId, Vec<Planted>>;

impl Layout {
    fn new() -> Self {
        Self::starting_at(1)
    }

    /// A layout whose first new page is `next`.
    fn starting_at(next: u32) -> Self {
        Self {
            next,
            filled: BTreeMap::new(),
        }
    }

    fn new_page(&mut self) -> PageId {
        self.next += 1;
        page(self.next - 1)
    }

    /// The next slot of `page`: its lowest free one, as these tests fill
    /// their pages in slot order.
    fn take_slot(&mut self, page: PageId) -> CellAddress {
        let filled = self.filled.entry(page).or_default();
        *filled += 1;
        CellAddress::new(page, SlotId::new(*filled as u16).expect("not zero"))
    }

    fn has_room(&self, page: PageId, most: usize) -> bool {
        self.filled.get(&page).copied().unwrap_or(0) < most.min(PAGE_NODES)
    }

    /// Gives every node of `tree` a page and a slot, and returns the cells
    /// by page and the address of the root.
    fn place(
        &mut self,
        tree: &Tree,
        placement: Placement,
        rng: &mut Rng,
    ) -> (Cells, Option<CellAddress>) {
        let among: Vec<PageId> = match placement {
            Placement::Scattered(pages) => (0..pages).map(|_| self.new_page()).collect(),
            _ => Vec::new(),
        };
        self.place_among(tree, placement, &among, rng)
    }

    /// As [`Self::place`], with `among` as the pages of a scattered tree:
    /// pages that can hold cells of other trees already.
    fn place_among(
        &mut self,
        tree: &Tree,
        placement: Placement,
        among: &[PageId],
        rng: &mut Rng,
    ) -> (Cells, Option<CellAddress>) {
        let parents = tree.parents();
        let mut at: Vec<Option<CellAddress>> = vec![None; tree.len()];
        for node in tree.top_down() {
            let beside = parents[node]
                .and_then(|parent| at[parent])
                .map(CellAddress::page);
            let page = match placement {
                Placement::Own => self.new_page(),
                Placement::Beside(most) => beside
                    .filter(|&page| self.has_room(page, most))
                    .unwrap_or_else(|| self.new_page()),
                Placement::Scattered(_) => loop {
                    let page = among[rng.below(among.len())];
                    if self.has_room(page, PAGE_NODES) {
                        break page;
                    }
                },
            };
            at[node] = Some(self.take_slot(page));
        }
        cells_of(tree, &parents, &at)
    }
}

/// The cells of `tree`, whose nodes are at `at`, by page, and the
/// address of its root.
fn cells_of(
    tree: &Tree,
    parents: &[Option<usize>],
    at: &[Option<CellAddress>],
) -> (Cells, Option<CellAddress>) {
    let address = |node: usize| at[node].expect("every node was placed");
    let mut cells = Cells::new();
    for node in tree.top_down() {
        let model = &tree.nodes[node];
        let child = |child: Option<usize>| {
            child.map(|child| (address(child), tree.nodes[child].key.len() as u8))
        };
        let here = address(node);
        let above = parents[node].map(|parent| address(parent).page());
        cells.entry(here.page()).or_default().push(Planted {
            slot: here.slot(),
            key: model.key.clone(),
            value: value_of(tree.mark, &model.key),
            left: child(model.left),
            right: child(model.right),
            fragment_root: above != Some(here.page()),
        });
    }
    for planted in cells.values_mut() {
        planted.sort_by_key(|cell| cell.slot);
    }
    (cells, tree.root.map(address))
}

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

    /// Write-back stores the tree page in `frame` as the commit `born` left
    /// it.
    fn base(&mut self, born: u64, frame: &[u8]) {
        let nodes = NodePage::open(frame, Lease::take()).expect("a tree page");
        let block = self
            .writer
            .append(
                nodes.id(),
                BlockContent::Base,
                Compression::Lz4,
                &nodes.tree_page().to_image(),
            )
            .expect("a block");
        self.table
            .push(nodes.id(), Link::base(version(born), block))
            .expect("a link");
    }

    /// The commit `born` wrote `cells` into new pages, and write-back
    /// stored them.
    fn plant(&mut self, born: u64, cells: &Cells) {
        for (&id, planted) in cells {
            let mut frame = vec![0; SIZE.get()];
            let mut nodes =
                NodePageMut::create(&mut frame, SIZE, id, Lease::take(), ()).expect("a page");
            for cell in planted {
                cell.insert_into(&mut nodes);
            }
            self.base(born, &frame);
        }
    }

    /// The commit `next` changes the page `id`, which is as the reader at
    /// `next - 1` sees it, through its private copy in `pool`, and
    /// write-back stores the new version.
    fn rewrite(
        &mut self,
        pool: &BufferPool,
        id: PageId,
        next: u64,
        change: impl FnOnce(&mut NodePageMut<'_>),
    ) {
        let before = version(next - 1);
        let held = self
            .loader(pool)
            .get(id, before, &mut LoadBuffers::new())
            .expect("the page loads");
        let copy = pool
            .make_private(OwnerId::new(1), id, before)
            .expect("the page is pinned");
        drop(held);
        {
            let mut latch = copy.write().expect("the copy is private");
            let mut nodes =
                NodePageMut::open(&mut latch, SIZE, Lease::take(), ()).expect("a tree page");
            change(&mut nodes);
        }
        let installed = pool.install(&copy, version(next)).expect("the commit");
        self.base(next, &installed);
        pool.mark_written(&installed).expect("written back");
    }

    /// The commit `next` frees the page `id`.
    fn free(&mut self, pool: &BufferPool, id: PageId, next: u64) {
        self.table
            .push(id, Link::free(version(next)))
            .expect("a link");
        pool.free(id, version(next)).expect("a later version");
    }

    fn loader<'s>(&'s self, pool: &'s BufferPool) -> PageLoader<'s> {
        PageLoader::new(pool, &self.table, &self.segments, SIZE).expect("a loader")
    }
}

/// Background work takes the cell in `slot` out of the current version of
/// the page `id`, in a page step: what a dropped subtree left there
/// (`transactions.md` PV7). Every handle into the page ends.
fn take_out(loader: PageLoader<'_>, pool: &BufferPool, id: PageId, slot: SlotId) {
    // The step works on the page in the pool.
    let held = loader
        .get(id, version(1), &mut LoadBuffers::new())
        .expect("the page loads");
    let step = pool.page_step(id);
    let current = step.current().expect("the page is in the pool");
    let mut blank = pool.blank();
    blank.copy_from(&current);
    NodePageMut::open(&mut blank, SIZE, Lease::take(), ())
        .expect("a tree page")
        .node_mut(slot)
        .expect("a cell")
        .remove()
        .expect("it leaves");
    step.replace(&current, blank)
        .expect("the step goes through");
    drop((step, current, held));
}

/// A pool of exactly `frames` frames.
fn pool_of(frames: usize) -> BufferPool {
    let mut capacity = frames * SIZE.get();
    loop {
        let config = PoolConfig {
            page_size: SIZE,
            capacity,
        };
        match BufferPool::new(config) {
            Ok(pool) if pool.frames() == frames => return pool,
            Ok(pool) => assert!(pool.frames() < frames, "the pool grows a frame at a time"),
            Err(_) => {}
        }
        capacity += 64;
    }
}

/// A pool with room for `frames` frames, and some more.
fn roomy(frames: usize) -> BufferPool {
    BufferPool::new(PoolConfig {
        page_size: SIZE,
        capacity: (frames + 16) * (SIZE.get() + 1_024),
    })
    .expect("a pool")
}

/// A store with one tree at version 1.
struct Store {
    disk: Disk,
    tree: Tree,
    root: Option<CellAddress>,
    pages: Vec<PageId>,
}

fn planted(tree: Tree, placement: Placement, seed: u64) -> Store {
    planted_with(tree, placement, seed, 0).0
}

/// A store with one tree at version 1, and in each of its pages
/// `left_over` cells that a dropped subtree left. Returns where those are.
fn planted_with(
    tree: Tree,
    placement: Placement,
    seed: u64,
    left_over: usize,
) -> (Store, Vec<CellAddress>) {
    let mut disk = Disk::new();
    let mut layout = Layout::new();
    let (mut cells, root) = layout.place(&tree, placement, &mut Rng(seed));
    let mut dropped = Vec::new();
    for (&page, planted) in &mut cells {
        for _ in 0..left_over {
            let at = layout.take_slot(page);
            planted.push(Planted::left_over(at));
            dropped.push(at);
        }
    }
    disk.plant(1, &cells);
    let store = Store {
        disk,
        tree,
        root,
        pages: cells.keys().copied().collect(),
    };
    (store, dropped)
}

/// The key and the value of a node a cursor returned.
fn record(node: &PinnedNode<'_>) -> (Vec<u8>, Vec<u8>) {
    let node = node.node();
    let CellValue::Inline(value) = node.value() else {
        panic!("these tests keep every value in its cell");
    };
    (node.key().to_vec(), value.to_vec())
}

/// What a move returns.
type Came<'r> = Result<Option<PinnedNode<'r>>, CursorError>;

/// A move that takes no key.
type Move = for<'r> fn(&mut Cursor, &NodeReader<'r, Guard>, &mut LoadBuffers) -> Came<'r>;

const FIRST: Move = Cursor::seek_to_first;
const LAST: Move = Cursor::seek_to_last;
const NEXT: Move = Cursor::next;
const PREV: Move = Cursor::prev;
const NODE: Move = Cursor::node;

fn seek_a_key<'r>(
    cursor: &mut Cursor,
    reader: &NodeReader<'r, Guard>,
    buffers: &mut LoadBuffers,
) -> Came<'r> {
    cursor.seek(reader, buffers, b"key")
}

fn seek_a_key_for_prev<'r>(
    cursor: &mut Cursor,
    reader: &NodeReader<'r, Guard>,
    buffers: &mut LoadBuffers,
) -> Came<'r> {
    cursor.seek_for_prev(reader, buffers, b"key")
}

/// Every move, the seeks with a key that none of these tests' trees has.
const MOVES: [Move; 7] = [
    seek_a_key,
    seek_a_key_for_prev,
    FIRST,
    LAST,
    NEXT,
    PREV,
    NODE,
];

/// Makes one move, and returns the record it came to.
fn go(
    cursor: &mut Cursor,
    reader: &NodeReader<'_, Guard>,
    buffers: &mut LoadBuffers,
    step: Move,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let moved = step(cursor, reader, buffers);
    arrived(cursor, moved)
}

/// What a move came to: the record of the node, with the cursor checked
/// against it.
fn arrived(cursor: &Cursor, moved: Came<'_>) -> Option<(Vec<u8>, Vec<u8>)> {
    let node = moved.expect("the pages are readable");
    let found = node.as_ref().map(record);
    assert_eq!(cursor.valid(), found.is_some());
    assert_eq!(cursor.key(), found.as_ref().map(|(key, _)| key.as_slice()));
    assert_eq!(cursor.handle(), node.as_ref().map(PinnedNode::handle));
    assert_eq!(cursor.depth().is_some(), found.is_some());
    found
}
