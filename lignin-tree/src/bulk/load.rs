//! The loader: nodes in, a child before its parent, and pages out.

use std::vec::Drain;

use lignin_format::{
    tree_page::{CellContent, CellValue, Hash, HashField, Side, TreeCell, TreeLink, TreePage},
    value_log::{PendingValue, ValuePlace},
    CellAddress, PageId, SlotId,
};
use lignin_store::load::FRAME_FORMAT;

use super::{
    error::LoadError,
    node::{HostedLoad, HostedRoot, LinkLoad, LoadValues, Loaded, LoadedPage, NodeLoad, Root},
};
use crate::{
    fragment::{Layout, Pages},
    hash::HashRules,
    node::{Lease, NodePageMut},
};

/// The id of the page a cell is built in before it has a page.
const SCRATCH: PageId = PageId::MAX;

/// What a cell that is being built names where its child has no address
/// yet: no cell.
const NOWHERE: CellAddress = CellAddress::new(SCRATCH, SlotId::MAX);

const LIVE: &str = "a node that is named is in the load";

/// How full a load leaves its pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadParams {
    /// A page is not left before its cells and slots take this many percent
    /// of what it holds, as long as the load has a piece that fits.
    pub least_fill_percent: u32,
    /// A page that holds a cell takes no piece that would take it above
    /// this many percent. What is left free is room for the writes that
    /// follow.
    pub most_fill_percent: u32,
}

impl LoadParams {
    /// Pages at least 90% full, and filled as far as pieces fit.
    pub const DEFAULT: Self = Self {
        least_fill_percent: 90,
        most_fill_percent: 100,
    };
}

impl Default for LoadParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What a load did so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadCounters {
    /// The nodes whose cells are in pages.
    pub nodes: u64,
    /// The fragments those cells form.
    pub fragments: u64,
    /// The pages the load started.
    pub pages: u64,
    /// The bytes of the cells that are in pages, with their slots.
    pub cell_bytes: u64,
    /// The values that are out of line, of the cells that are in pages.
    pub values: u64,
    /// The bytes of those values.
    pub value_bytes: u64,
    /// The value hashes the rules made.
    pub value_hashes: u64,
    /// The key-value hashes the rules made.
    pub kv_hashes: u64,
    /// The hashes of nodes the rules made.
    pub node_hashes: u64,
}

impl LoadCounters {
    /// The share of the pages the load started that cells and their slots
    /// take, in percent of what the pages hold.
    pub fn fill_percent(&self, layout: &Layout) -> f64 {
        let holds = self.pages * layout.capacity() as u64;
        if holds == 0 {
            return 0.0;
        }
        self.cell_bytes as f64 * 100.0 / holds as f64
    }
}

/// What a load did, when it is finished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// The counts of the whole load.
    pub counters: LoadCounters,
    /// The pages the load started that were not taken with
    /// [`Loader::drain_pages`], in the order it left them.
    pub pages: Vec<LoadedPage>,
    /// The hosted roots that were not taken with
    /// [`Loader::drain_hosted`].
    pub hosted: Vec<HostedRoot>,
}

/// Which pointer of a node names a child.
#[derive(Debug, Clone, Copy)]
enum Which {
    Left,
    Right,
    Hosted(usize),
}

/// What a node of the load points at.
#[derive(Debug, Clone, Copy)]
enum Child {
    /// A node of the load whose cell is in no page yet.
    Open(u32),
    /// A cell in a page.
    Placed(CellAddress),
}

impl Child {
    const fn open(self) -> Option<u32> {
        match self {
            Self::Open(index) => Some(index),
            Self::Placed(_) => None,
        }
    }
}

/// A node whose cell is in no page yet.
#[derive(Debug)]
struct Open {
    serial: u64,
    /// The cell as the rules saw it: every part of it but the addresses and
    /// the mark of a fragment root.
    cell: Box<[u8]>,
    hash: Hash,
    key_len: u8,
    pending: Option<PendingValue>,
    /// What the cell holds in the place of its value, once its piece goes
    /// into a page ([`LoadValues::place`]).
    place: Option<ValuePlace>,
    left: Option<Child>,
    right: Option<Child>,
    hosted: Box<[Option<Child>]>,
    /// The bytes of the cell with its slot.
    size: u32,
    /// The bytes of the node's piece: its own, and those of the open nodes
    /// below it.
    below: u32,
    /// The level of the pages the node's piece goes to: above every page
    /// that holds a cell the piece points at and that is still filled.
    level: usize,
    /// Whether a node of the load points at this one.
    adopted: bool,
    /// The name its host gave it, for the root of a hosted Merk.
    hosted_as: Option<Loaded>,
    /// Where the cell goes, once its piece goes into a page.
    at: CellAddress,
}

impl Open {
    fn children(&self) -> impl Iterator<Item = (Which, Child)> + '_ {
        let sides = [(Which::Left, self.left), (Which::Right, self.right)];
        let hosted = self.hosted.iter().enumerate();
        let hosted = hosted.map(|(position, child)| (Which::Hosted(position), *child));
        let all = sides.into_iter().chain(hosted);
        all.filter_map(|(which, child)| Some((which, child?)))
    }

    fn open_children(&self) -> impl Iterator<Item = (Which, u32)> + '_ {
        self.children()
            .filter_map(|(which, child)| Some((which, child.open()?)))
    }

    fn child_mut(&mut self, which: Which) -> &mut Option<Child> {
        match which {
            Which::Left => &mut self.left,
            Which::Right => &mut self.right,
            Which::Hosted(position) => &mut self.hosted[position],
        }
    }
}

/// The link of a cell that is being built to a child that is in no page
/// yet.
fn scratch_link<'a>(
    nodes: &'a [Option<Open>],
    link: &Option<LinkLoad<'a>>,
    child: Option<u32>,
) -> Option<TreeLink<'a>> {
    let (link, child) = (link.as_ref()?, nodes[child? as usize].as_ref()?);
    Some(TreeLink {
        child: NOWHERE,
        hash: &child.hash,
        heights: link.heights,
        child_key_len: child.key_len,
        aggregate: link.aggregate,
    })
}

/// A page that pieces still go to.
#[derive(Debug, Clone, Copy)]
struct Filling {
    page: PageId,
    used: usize,
    cells: usize,
}

/// A subtree of a piece that fits the room of a page.
#[derive(Debug, Clone, Copy)]
struct Cut {
    parent: u32,
    which: Which,
    node: u32,
    below: u32,
}

/// Builds the pages of Merk trees from their nodes, a child before its
/// parent. See the [module documentation](super).
///
/// It holds no page. Between two calls it keeps the nodes whose cells are
/// in no page yet, which are those of the subtrees that still wait for
/// their parent, and for each level the page it fills.
#[derive(Debug)]
pub struct Loader {
    layout: Layout,
    least: usize,
    most: usize,
    lease: Lease,
    scratch: Vec<u8>,
    nodes: Vec<Option<Open>>,
    free: Vec<u32>,
    serial: u64,
    /// How many subtrees no node points at.
    unowned: usize,
    levels: Vec<Option<Filling>>,
    order: Vec<u32>,
    walk: Vec<u32>,
    path: Vec<u32>,
    search: Vec<(u32, usize)>,
    roots: Vec<Option<CellAddress>>,
    hashes: Vec<Option<Hash>>,
    left: Vec<LoadedPage>,
    hosted: Vec<HostedRoot>,
    counters: LoadCounters,
    failed: bool,
}

impl Loader {
    /// A load into pages of `layout`'s size, with fragments of at most its
    /// largest size, filled as `params` says.
    pub fn new(layout: Layout, params: LoadParams) -> Result<Self, LoadError> {
        let (least, most) = (params.least_fill_percent, params.most_fill_percent);
        if least > most || most == 0 || most > 100 {
            return Err(LoadError::Fill { least, most });
        }
        let percent = |percent: u32| layout.capacity() * percent as usize / 100;
        Ok(Self {
            layout,
            least: percent(least),
            most: percent(most),
            lease: Lease::take(),
            scratch: vec![0; layout.page_size().get()],
            nodes: Vec::new(),
            free: Vec::new(),
            serial: 0,
            unowned: 0,
            levels: Vec::new(),
            order: Vec::new(),
            walk: Vec::new(),
            path: Vec::new(),
            search: Vec::new(),
            roots: Vec::new(),
            hashes: Vec::new(),
            left: Vec::new(),
            hosted: Vec::new(),
            counters: LoadCounters::default(),
            failed: false,
        })
    }

    /// The layout the pages are built for.
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// What the load did so far.
    pub const fn counters(&self) -> &LoadCounters {
        &self.counters
    }

    /// Takes one node, whose children the load has taken before, and
    /// returns the subtree it is the root of.
    ///
    /// The rules are called here, once each: for the node's value hash
    /// unless it is given, for its key-value hash, and for its own hash.
    /// The node they are shown has no address yet ([`NodeRef::address`]
    /// names no cell), and neither have its children that are in no page.
    ///
    /// [`NodeRef::address`]: crate::node::NodeRef::address
    pub fn node<P: Pages, V: LoadValues, R: HashRules>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
        node: &NodeLoad<'_>,
    ) -> Result<Loaded, LoadError> {
        self.check()?;
        let taken = self.take(pages, values, rules, node);
        self.settle(taken)
    }

    /// Places a subtree that no node of the load points at: the root of a
    /// Merk that has no host in this load. Its cell is marked as the root
    /// of a fragment, and no later node can take the subtree.
    pub fn root<P: Pages, V: LoadValues>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        subtree: Loaded,
    ) -> Result<Root, LoadError> {
        self.check()?;
        let placed = self.adopt(subtree).and_then(|index| {
            let hash = self.open(index).hash;
            let (address, _) = self.pack(pages, values, index)?;
            Ok(Root { address, hash })
        });
        self.settle(placed)
    }

    /// The hash of the root of a subtree that no node of the load points at
    /// yet: the root hash of a Merk, before the Merk is given to its host.
    pub fn hash_of(&self, subtree: Loaded) -> Result<Hash, LoadError> {
        self.check()?;
        let node = self
            .unowned(subtree)
            .ok_or(LoadError::NotLoaded { subtree })?;
        Ok(node.hash)
    }

    /// The pages the load started and will not write again, since the last
    /// call: each can be written out, and its hint taken.
    pub fn drain_pages(&mut self) -> Drain<'_, LoadedPage> {
        self.left.drain(..)
    }

    /// Where the roots of hosted Merks went, since the last call.
    pub fn drain_hosted(&mut self) -> Drain<'_, HostedRoot> {
        self.hosted.drain(..)
    }

    /// Ends the load. Every subtree has been given to a node or placed as a
    /// root, so every cell is in its page.
    pub fn finish(mut self) -> Result<LoadReport, LoadError> {
        self.check()?;
        if self.unowned > 0 {
            return Err(LoadError::Unfinished {
                subtrees: self.unowned,
            });
        }
        for level in 0..self.levels.len() {
            self.leave(level);
        }
        Ok(LoadReport {
            counters: self.counters,
            pages: self.left,
            hosted: self.hosted,
        })
    }

    pub(super) fn settle<T>(&mut self, result: Result<T, LoadError>) -> Result<T, LoadError> {
        self.failed |= result.is_err();
        result
    }

    pub(super) const fn check(&self) -> Result<(), LoadError> {
        if self.failed {
            return Err(LoadError::Failed);
        }
        Ok(())
    }

    fn open(&self, index: u32) -> &Open {
        self.nodes[index as usize].as_ref().expect(LIVE)
    }

    fn open_mut(&mut self, index: u32) -> &mut Open {
        self.nodes[index as usize].as_mut().expect(LIVE)
    }

    fn unowned(&self, subtree: Loaded) -> Option<&Open> {
        let node = self.nodes.get(subtree.index as usize)?.as_ref()?;
        let free = subtree.load == self.lease && node.serial == subtree.serial && !node.adopted;
        free.then_some(node)
    }

    /// Gives the subtree to the node or the call that names it.
    fn adopt(&mut self, subtree: Loaded) -> Result<u32, LoadError> {
        if self.unowned(subtree).is_none() {
            return Err(LoadError::NotLoaded { subtree });
        }
        self.open_mut(subtree.index).adopted = true;
        self.unowned -= 1;
        Ok(subtree.index)
    }

    /// The level above the page of a cell that is in a page: 0 unless the
    /// load still fills that page.
    fn level_above(&self, page: PageId) -> usize {
        self.levels
            .iter()
            .position(|filling| filling.is_some_and(|filling| filling.page == page))
            .map_or(0, |level| level + 1)
    }

    fn take<P: Pages, V: LoadValues, R: HashRules>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
        node: &NodeLoad<'_>,
    ) -> Result<Loaded, LoadError> {
        let left = node.left.map(|link| self.adopt(link.child)).transpose()?;
        let right = node.right.map(|link| self.adopt(link.child)).transpose()?;
        let mut hosted = Vec::with_capacity(node.hosted.len());
        for root in node.hosted {
            hosted.push(match *root {
                HostedLoad::Empty => None,
                HostedLoad::At(address) => Some(Child::Placed(address)),
                HostedLoad::Loaded(subtree) => {
                    let index = self.adopt(subtree)?;
                    self.open_mut(index).hosted_as = Some(subtree);
                    Some(Child::Open(index))
                }
            });
        }

        self.hashes.clear();
        self.roots.clear();
        for root in &hosted {
            self.roots.push(root.map(|root| match root {
                Child::Open(_) => NOWHERE,
                Child::Placed(address) => address,
            }));
            if node.value_hash.is_some() {
                continue;
            }
            let hash = match *root {
                None => None,
                Some(Child::Open(index)) => Some(self.open(index).hash),
                Some(Child::Placed(address)) => {
                    let read = pages.read(address.page(), |page| {
                        let root = page.node(address.slot())?;
                        Some(rules.node_hash(root))
                    })?;
                    let hash = read.ok_or(LoadError::NoRoot { address })?;
                    self.counters.node_hashes += 1;
                    Some(hash.map_err(LoadError::Rule)?)
                }
            };
            self.hashes.push(hash);
        }

        let pending = values.hold(node.value).map_err(LoadError::Value)?;
        let content = CellContent {
            key: node.key,
            feature: node.feature,
            value_hash: node.value_hash.unwrap_or(&[0; 32]),
            kv_hash: &[0; 32],
            left: scratch_link(&self.nodes, &node.left, left),
            right: scratch_link(&self.nodes, &node.right, right),
            hosted_roots: &self.roots,
            value: pending.map_or(CellValue::Inline(node.value), CellValue::Pending),
            fragment_root: true,
        };
        let size = self.layout.page_size();
        let mut scratch = NodePageMut::create(&mut self.scratch, size, SCRATCH, self.lease, ())?;
        let slot = scratch.insert(&content)?.slot();
        let mut cell = scratch.node_mut(slot).expect("the cell was put there");
        let value_hash = match node.value_hash {
            Some(hash) => *hash,
            None => {
                let hash = rules.value_hash(cell.node(), node.value, &self.hashes);
                let hash = hash.map_err(LoadError::Rule)?;
                cell.set_hash(HashField::Value, &hash)?;
                self.counters.value_hashes += 1;
                hash
            }
        };
        let kv_hash = rules.kv_hash(node.key, &value_hash);
        cell.set_hash(HashField::KeyValue, &kv_hash.map_err(LoadError::Rule)?)?;
        let hash = rules.node_hash(cell.node()).map_err(LoadError::Rule)?;
        self.counters.kv_hashes += 1;
        self.counters.node_hashes += 1;
        let cell: Box<[u8]> = cell.node().cell().as_bytes().into();

        let size = (cell.len() + TreePage::SLOT_LEN) as u32;
        let (left, right) = (left.map(Child::Open), right.map(Child::Open));
        let (mut below, mut level) = (size, 0);
        for child in [left, right].into_iter().chain(hosted.iter().copied()) {
            level = level.max(match child {
                None => continue,
                Some(Child::Placed(address)) => self.level_above(address.page()),
                Some(Child::Open(index)) => {
                    let child = self.open(index);
                    below += child.below;
                    child.level
                }
            });
        }
        self.serial += 1;
        let open = Open {
            serial: self.serial,
            cell,
            hash,
            key_len: node.key.len() as u8,
            pending,
            place: None,
            left,
            right,
            hosted: hosted.into(),
            size,
            below,
            level,
            adopted: false,
            hosted_as: None,
            at: NOWHERE,
        };
        let index = match self.free.pop() {
            Some(index) => {
                self.nodes[index as usize] = Some(open);
                index
            }
            None => {
                self.nodes.push(Some(open));
                (self.nodes.len() - 1) as u32
            }
        };
        self.unowned += 1;
        self.cut(pages, values, index)?;
        Ok(Loaded {
            load: self.lease,
            index,
            serial: self.serial,
        })
    }

    /// Cuts the piece of the node `index`, which just arrived, where it is
    /// to be cut: a piece below the node goes into a page
    ///
    /// - if it is of at least the small size, and either the node's piece
    ///   is above the largest fragment or the piece below is of a lower
    ///   level than another one, so that the node starts the piece above
    ///   them and takes along only what is small
    /// - whatever its size, for as long as the node's piece is above the
    ///   largest fragment
    ///
    /// the heaviest first.
    fn cut<P: Pages, V: LoadValues>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        index: u32,
    ) -> Result<(), LoadError> {
        let (largest, small) = (self.layout.largest_fragment(), self.layout.small_fragment());
        let above = |node: &Open| node.below as usize > largest;
        let was_above = above(self.open(index));
        loop {
            let node = self.open(index);
            let leaves = |(_, child): &(Which, u32)| {
                let child = self.open(*child);
                let lower = was_above || child.level < node.level;
                above(node) || (lower && child.below as usize >= small)
            };
            let weight = |(_, child): &(Which, u32)| self.open(*child).below;
            let heaviest = node.open_children().filter(leaves).max_by_key(weight);
            let Some((which, child)) = heaviest else {
                return Ok(());
            };
            let below = self.open(child).below;
            let (address, level) = self.pack(pages, values, child)?;
            let node = self.open_mut(index);
            *node.child_mut(which) = Some(Child::Placed(address));
            node.below -= below;
            node.level = node.level.max(level + 1);
        }
    }

    /// Puts the piece of the node `root` into the page of its level, and
    /// returns where the root went and the level.
    ///
    /// A piece that the page has no room for starts the next page. Before
    /// that, a page that is not full enough takes subtrees of the piece,
    /// each the largest that fits, as fragments of their own.
    fn pack<P: Pages, V: LoadValues>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        root: u32,
    ) -> Result<(CellAddress, usize), LoadError> {
        let level = self.open(root).level;
        if self.levels.len() <= level {
            self.levels.resize(level + 1, None);
        }
        if let Some(filling) = self.levels[level]
            && filling.used + self.open(root).below as usize > self.most
        {
            let mut used = filling.used;
            while used < self.least {
                let Some(cut) = self.largest_within(root, self.most - used) else {
                    break;
                };
                let address = self.put(pages, values, cut.node, level)?;
                *self.open_mut(cut.parent).child_mut(cut.which) = Some(Child::Placed(address));
                for above in 0..self.path.len() {
                    let above = self.path[above];
                    self.open_mut(above).below -= cut.below;
                }
                used += cut.below as usize;
            }
            self.leave(level);
        }
        if self.levels[level].is_none() {
            let page = pages.create()?;
            self.counters.pages += 1;
            self.levels[level] = Some(Filling {
                page,
                used: 0,
                cells: 0,
            });
        }
        let address = self.put(pages, values, root, level)?;
        Ok((address, level))
    }

    /// The largest subtree of the piece of `root`, other than the piece,
    /// that takes at most `room` bytes: the first of them in the order the
    /// piece is walked. The nodes from the root down to its parent are left
    /// in `self.path`.
    fn largest_within(&mut self, root: u32, room: usize) -> Option<Cut> {
        let (nodes, chain) = (&self.nodes, &mut self.walk);
        let mut best = None::<Cut>;
        chain.clear();
        self.search.clear();
        self.search.push((root, 0));
        while let Some((parent, depth)) = self.search.pop() {
            chain.truncate(depth);
            chain.push(parent);
            for (which, node) in nodes[parent as usize].as_ref().expect(LIVE).open_children() {
                let below = nodes[node as usize].as_ref().expect(LIVE).below;
                if below as usize > room {
                    self.search.push((node, depth + 1));
                } else if best.is_none_or(|best| below > best.below) {
                    best = Some(Cut {
                        parent,
                        which,
                        node,
                        below,
                    });
                    self.path.clone_from(chain);
                }
            }
        }
        best
    }

    /// Puts the piece of the node `root` into the page that is filled at
    /// `level`, as one fragment, and returns where the root went.
    fn put<P: Pages, V: LoadValues>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        root: u32,
        level: usize,
    ) -> Result<CellAddress, LoadError> {
        let mut filling = self.levels[level].expect("a page is filled at the level");
        self.order.clear();
        self.walk.clear();
        self.walk.push(root);
        while let Some(index) = self.walk.pop() {
            self.order.push(index);
            let node = self.nodes[index as usize].as_ref().expect(LIVE);
            self.walk
                .extend(node.open_children().map(|(_, child)| child));
        }
        for &index in self.order.iter().rev() {
            let node = self.nodes[index as usize].as_mut().expect(LIVE);
            let slot = SlotId::from_index(filling.cells);
            node.at = CellAddress::new(filling.page, slot.expect("a page has a slot for a cell"));
            filling.cells += 1;
            filling.used += node.size as usize;
            self.counters.cell_bytes += u64::from(node.size);
            if let Some(pending) = node.pending {
                let place = values.place(pending, filling.page);
                let place = place.map_err(LoadError::Value)?;
                // The cell was counted with the length of the value it
                // was given: a place of another length is of another value.
                if place.value_len() != pending.value_len() {
                    let (held, placed) = (pending.value_len(), place.value_len());
                    let other = format!("a value of {held} bytes was given a place of {placed}");
                    return Err(LoadError::Value(other.into()));
                }
                node.place = Some(place);
                self.counters.values += 1;
                self.counters.value_bytes += u64::from(pending.value_len());
            }
        }

        let (nodes, order, roots) = (&self.nodes, &self.order, &mut self.roots);
        let written = pages.write(filling.page, |page| {
            let at = |child: Child| match child {
                Child::Open(index) => nodes[index as usize].as_ref().expect(LIVE).at,
                Child::Placed(address) => address,
            };
            for &index in order.iter().rev() {
                let node = nodes[index as usize].as_ref().expect(LIVE);
                let cell = TreeCell::decode(&node.cell, FRAME_FORMAT)?;
                let link = |side, child: Option<Child>| {
                    let (link, child) = (cell.link(side)?, at(child?));
                    Some(TreeLink { child, ..link })
                };
                roots.clear();
                roots.extend(node.hosted.iter().map(|root| root.map(at)));
                let handle = page.insert(&CellContent {
                    key: cell.key(),
                    feature: cell.feature(),
                    value_hash: cell.value_hash(),
                    kv_hash: cell.kv_hash(),
                    left: link(Side::Left, node.left),
                    right: link(Side::Right, node.right),
                    hosted_roots: roots,
                    value: match node.place {
                        Some(ValuePlace::Placed(pointer)) => CellValue::OutOfLine(pointer),
                        Some(ValuePlace::Pending(pending)) => CellValue::Pending(pending),
                        None => cell.value(),
                    },
                    fragment_root: index == root,
                })?;
                if handle.address() != node.at {
                    return Ok(Err(LoadError::Slot {
                        expected: node.at,
                        found: handle.address(),
                    }));
                }
            }
            Ok(Ok(()))
        });
        written??;

        let at = self.open(root).at;
        for &index in &self.order {
            let node = self.nodes[index as usize].take().expect(LIVE);
            self.free.push(index);
            self.hosted.extend(node.hosted_as.map(|subtree| HostedRoot {
                subtree,
                address: node.at,
            }));
        }
        self.counters.nodes += self.order.len() as u64;
        self.counters.fragments += 1;
        self.levels[level] = Some(filling);
        Ok(at)
    }

    /// The load writes the page that is filled at `level` no more.
    fn leave(&mut self, level: usize) {
        self.left
            .extend(self.levels[level].take().map(|filling| LoadedPage {
                page: filling.page,
                used: filling.used,
            }));
    }
}
