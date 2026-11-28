//! The plan of one write: which page every cell it holds is in when the
//! write ends.
//!
//! The plan changes no page. It reads the cells the write names and the
//! cells it reaches from them inside their pages, and applies the rules of
//! `docs/lignindb/tree-layout.md`, sections 5 to 8, to what it read. A page
//! may be above its capacity while the rules run, and none is when they are
//! done.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    hash::{BuildHasherDefault, Hasher},
};

use lignin_format::{
    tree_page::{Side, TreeCell, TreeLink, TreePage},
    CellAddress, PageId, SlotId,
};

use super::{
    error::{FragmentError, WriteError},
    layout::{Layout, Upkeep},
    pages::{used_bytes, Hints, Pages},
    report::Cause,
    write::{HostedWrite, NodeWrite, Target, ValueWrite, Write},
};
use crate::node::{NodePage, NodeRef};

/// The page of a cell that is in none: a node the write creates and has not
/// placed yet, or a node it deletes.
pub(super) const NOWHERE: usize = usize::MAX;

/// The hasher of the planner's maps. Their keys are a page id, or a page
/// id with a slot: small integers, which are mixed word by word and not
/// hashed as bytes. A map of the planner is asked for a key and never
/// walked, so nothing of a plan follows from the order of one.
#[derive(Debug, Default, Clone, Copy)]
struct Mixed(u64);

impl Mixed {
    fn mix(&mut self, word: u64) {
        self.0 = self
            .0
            .wrapping_add(word)
            .wrapping_mul(0xF135_7AEA_2E62_A9C5);
    }
}

impl Hasher for Mixed {
    /// A product has its best bits on top, and a table takes the place of
    /// a key from the low ones.
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.mix(u64::from(*byte));
        }
    }

    fn write_u16(&mut self, word: u16) {
        self.mix(u64::from(word));
    }

    fn write_u32(&mut self, word: u32) {
        self.mix(u64::from(word));
    }
}

type Mixing = BuildHasherDefault<Mixed>;

/// How many cells a plan comes to know for each cell its write names, as
/// the first size of its map of them: a write reads the fragments of the
/// cells it names. More than this costs the map a growth, and nothing
/// else.
const KNOWN_FOR_ONE: usize = 16;

/// The same for the pages of those cells.
const PAGES_FOR_ONE: usize = 4;

/// The most cells of a write that its plan's maps are sized for before
/// the plan is made. A write that names more starts with maps of this
/// size, which grow: what a write names is not held to be true before it
/// is planned, and a size made of it must not be one that cannot be had.
const SIZED_FOR: usize = 1 << 16;

const SLOT: usize = TreePage::SLOT_LEN;

/// Up to this many parts every choice of parts is compared (V2).
const EXHAUSTIVE: usize = 12;

/// How a cell points at another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Via {
    Link(Side),
    Hosted(u8),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Kid {
    pub cell: usize,
    pub via: Via,
}

/// How many kids a cell keeps in itself: two links and one hosted root.
const KIDS_INLINE: usize = 3;

/// What a cell points at, in the order it was told: a slice of [`Kid`]s
/// that asks the allocator for nothing while the cell has no more than
/// [`KIDS_INLINE`] of them.
#[derive(Debug)]
pub(super) struct Kids {
    inline: [Kid; KIDS_INLINE],
    len: usize,
    /// Every kid, once there are more than the cell keeps in itself.
    spilled: Vec<Kid>,
}

impl Kids {
    const fn new() -> Self {
        let none = Kid {
            cell: NOWHERE,
            via: Via::Link(Side::Left),
        };
        Self {
            inline: [none; KIDS_INLINE],
            len: 0,
            spilled: Vec::new(),
        }
    }

    fn push(&mut self, kid: Kid) {
        if self.len < KIDS_INLINE {
            self.inline[self.len] = kid;
        } else {
            if self.len == KIDS_INLINE {
                self.spilled.extend_from_slice(&self.inline);
            }
            self.spilled.push(kid);
        }
        self.len += 1;
    }
}

impl std::ops::Deref for Kids {
    type Target = [Kid];

    fn deref(&self) -> &[Kid] {
        match self.len <= KIDS_INLINE {
            true => &self.inline[..self.len],
            false => &self.spilled,
        }
    }
}

impl<'a> IntoIterator for &'a Kids {
    type IntoIter = std::slice::Iter<'a, Kid>;
    type Item = &'a Kid;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// One cell the plan knows: a node of the write, a cell the write removes,
/// or a cell that was reached from one of them.
#[derive(Debug)]
pub(super) struct Cell {
    /// Where the cell is in the pages; `None` for a node the write creates.
    pub origin: Option<CellAddress>,
    /// The page of `origin`.
    pub home: usize,
    /// The page the plan has the cell in.
    pub page: usize,
    /// The bytes of the cell as the write leaves it, without its slot.
    pub len: usize,
    /// Whether `len` and `kids` are known.
    read: bool,
    /// The cell that points at this one after the write, where the plan
    /// knows it.
    pub parent: Option<usize>,
    pub via: Option<Via>,
    /// The node's position in the write.
    pub node: Option<usize>,
    pub removed: bool,
    /// A root pointer of the write names the cell.
    pub free: bool,
    /// A node of the write that nothing in the write points at: it is the
    /// root of its fragment whatever its page holds, and stays in its page.
    anchored: bool,
    /// What the cell points at after the write.
    pub kids: Kids,
    /// The rule that carried the cell last.
    pub cause: Option<Cause>,
    /// A node of the write whose cell is rewritten in place: it had a
    /// cell, every part of the cell keeps its length, and every link and
    /// hosted pointer names the cell it named. Its cell takes the bytes it
    /// took, and no rule has carried it. The fragment it is in is as large
    /// as the write found it unless another cell of that fragment is
    /// changed ([`Upkeep::Changed`]).
    pub in_place: bool,
}

impl Cell {
    /// Whether the cell is in the page it was in before the write.
    pub fn stays(&self) -> bool {
        self.origin.is_some() && self.page == self.home
    }

    pub fn kid(&self, via: Via) -> Option<usize> {
        self.kids
            .iter()
            .find(|kid| kid.via == via)
            .map(|kid| kid.cell)
    }
}

/// One page as the plan has it.
#[derive(Debug)]
pub(super) struct PagePlan {
    /// The page's id; `None` for a page the write starts.
    pub id: Option<PageId>,
    loaded: bool,
    /// The highest slot of a cell that stays.
    top: usize,
    staying: usize,
    arriving: usize,
    cell_bytes: usize,
    /// A cell of this page that the write holds.
    held: Option<usize>,
    queued: bool,
    /// Whether a cell arrived in the page, left it or was written in it.
    pub touched: bool,
    order: usize,
}

impl PagePlan {
    fn of(id: Option<PageId>, order: usize) -> Self {
        Self {
            id,
            loaded: id.is_none(),
            top: 0,
            staying: 0,
            arriving: 0,
            cell_bytes: 0,
            held: None,
            queued: false,
            touched: id.is_none(),
            order,
        }
    }

    /// How many cells the page holds.
    pub fn cells(&self) -> usize {
        self.staying + self.arriving
    }

    /// The bytes its cells and its slot directory take. A cell that arrives
    /// takes the lowest free slot, so the directory is as long as the
    /// highest slot that stays, or as the number of cells.
    pub fn used(&self) -> usize {
        self.cell_bytes + SLOT * self.top.max(self.cells())
    }

    fn rank(&self) -> (bool, u64) {
        match self.id {
            Some(id) => (false, u64::from(id.get())),
            None => (true, self.order as u64),
        }
    }
}

/// What a cell holds in its page.
struct Stored {
    len: usize,
    links: [Option<CellAddress>; 2],
    /// How many hosted-root pointers the cell has.
    hosted: usize,
    /// The hosted-root pointers that are followed. A host that is behind
    /// has none (T3): what its pointers name stays in the page, and no
    /// rule sees it.
    roots: Vec<Option<CellAddress>>,
    value_len: usize,
    same_key: bool,
    /// The length of the feature, and of the aggregate of each link, of a
    /// cell that is read for a node of the write: what
    /// [`Self::same_lengths`] asks about. A cell that the plan reads on its
    /// way through a fragment has none.
    part_lens: Option<(usize, [Option<usize>; 2])>,
    /// Whether the cell hosts and is behind (T3): `roots` was emptied, and
    /// what its pointers hold in the page is not known here.
    behind: bool,
}

impl Stored {
    /// What `node` holds in its page. `key` is the key the write gives the
    /// node, where the cell is read for a node of the write.
    fn of(node: &NodeRef<'_>, key: Option<&[u8]>) -> Self {
        Self {
            len: node.cell().encoded_len(),
            links: Side::BOTH.map(|side| node.link(side).map(|link| link.child)),
            hosted: node.hosted_roots().len(),
            roots: node.hosted_roots().collect(),
            value_len: node.value().stored_len(),
            same_key: key.is_none_or(|key| key == node.key()),
            part_lens: key.map(|_| {
                let aggregate = |side| node.link(side).map(|link| link.aggregate.len());
                (node.feature().len(), Side::BOTH.map(aggregate))
            }),
            behind: false,
        }
    }

    /// The cells the stored cell points at.
    fn children(&self) -> impl Iterator<Item = CellAddress> + '_ {
        self.links
            .iter()
            .chain(&self.roots)
            .filter_map(|child| *child)
    }

    fn child(&self, via: Via) -> Option<CellAddress> {
        match via {
            Via::Link(side) => self.links[side as usize],
            Via::Hosted(position) => self.roots.get(usize::from(position)).copied().flatten(),
        }
    }

    /// Whether every part of the cell is as long under `node` as it is
    /// stored: the feature, the aggregate of each link, the pointers to
    /// hosted roots as a whole, and the value as the cell holds it. Which
    /// cells the links and pointers name is not asked here.
    fn same_lengths(&self, node: &NodeWrite<'_>) -> bool {
        let aggregates = [node.left, node.right].map(|link| link.map(|link| link.aggregate.len()));
        let hosted = match node.hosted {
            HostedWrite::Keep => self.hosted,
            HostedWrite::Set(roots) => roots.len(),
        };
        let value = match node.value {
            ValueWrite::Keep => self.value_len,
            ValueWrite::Inline(value) => value.len(),
            ValueWrite::Pending(_) => TreeCell::VALUE_LOCATION_LEN,
        };
        self.part_lens == Some((node.feature.len(), aggregates))
            && self.hosted == hosted
            && self.value_len == value
    }
}

/// The outcome of planning: every cell the write holds, with its page.
#[derive(Debug)]
pub(super) struct Plan<'w> {
    pub write: &'w Write<'w>,
    pub cells: Vec<Cell>,
    pub pages: Vec<PagePlan>,
    /// The cell of each node of the write.
    pub of_node: Vec<usize>,
    /// The cell each root pointer of the write names.
    pub roots: Vec<Option<usize>>,
    /// The cells that stay where they are and that no cell of their page
    /// points at any more.
    pub unpointed: Vec<CellAddress>,
    /// The cells whose mark has to be set in a page the write may change
    /// for nothing else: the cell that pointed at them was a host that is
    /// behind (T3).
    pub unmarked: Vec<CellAddress>,
    known: HashMap<CellAddress, usize, Mixing>,
    page_of: HashMap<PageId, usize, Mixing>,
    removed: HashSet<CellAddress, Mixing>,
    /// How many cells the plan has read that the write does not write
    /// ([`Self::take_in`]): what a walk is held to ([`held`]).
    #[cfg(any(test, feature = "held_reads"))]
    taken: usize,
}

impl Plan<'_> {
    fn page(&mut self, id: PageId) -> usize {
        let pages = &mut self.pages;
        *self.page_of.entry(id).or_insert_with(|| {
            pages.push(PagePlan::of(Some(id), pages.len()));
            pages.len() - 1
        })
    }

    fn cell(&mut self, origin: Option<CellAddress>) -> usize {
        let home = origin.map_or(NOWHERE, |address| self.page(address.page()));
        let cell = self.cells.len();
        self.cells.push(Cell {
            origin,
            home,
            page: home,
            len: 0,
            read: false,
            parent: None,
            via: None,
            node: None,
            removed: false,
            free: false,
            anchored: false,
            kids: Kids::new(),
            cause: None,
            in_place: false,
        });
        if let Some(address) = origin {
            self.known.insert(address, cell);
        }
        cell
    }

    /// The cell at `address`, which a link or pointer names.
    fn existing(&mut self, address: CellAddress) -> Result<usize, FragmentError> {
        if self.removed.contains(&address) {
            return Err(WriteError::Removed { address }.into());
        }
        Ok(match self.known.get(&address) {
            Some(cell) => *cell,
            None => self.cell(Some(address)),
        })
    }

    fn adopt(&mut self, parent: usize, kid: usize, via: Via) -> Result<(), FragmentError> {
        let cell = &mut self.cells[kid];
        if kid == parent || cell.parent.is_some() || cell.free {
            let address = cell.origin;
            return Err(WriteError::TwoPointers { address }.into());
        }
        cell.parent = Some(parent);
        cell.via = Some(via);
        self.cells[parent].kids.push(Kid { cell: kid, via });
        Ok(())
    }

    /// Takes in what was read of `cell`, a cell the write does not write:
    /// its length and what it points at.
    ///
    /// A hosted root that the write lets go of (`dropped`) is not among
    /// that: it stays in its page (D1), so the pointer of a host that the
    /// write does not write names nothing here, and keeps its bytes.
    fn take_in(
        &mut self,
        cell: usize,
        stored: &Stored,
        dropped: &HashSet<CellAddress, Mixing>,
    ) -> Result<(), FragmentError> {
        #[cfg(any(test, feature = "held_reads"))]
        {
            self.taken += 1;
        }
        let entry = &mut self.cells[cell];
        entry.len = stored.len;
        entry.read = true;
        for (side, child) in Side::BOTH.into_iter().zip(stored.links) {
            if let Some(child) = child {
                let known = self.cells.len();
                let kid = self.existing(child)?;
                if kid < known {
                    self.below(cell, kid)?;
                }
                self.adopt(cell, kid, Via::Link(side))?;
            }
        }
        for (position, root) in stored.roots.iter().enumerate() {
            if let Some(root) = root.filter(|root| !dropped.contains(root)) {
                let known = self.cells.len();
                let kid = self.existing(root)?;
                if kid < known {
                    self.below(cell, kid)?;
                }
                self.adopt(cell, kid, Via::Hosted(position as u8))?;
            }
        }
        Ok(())
    }

    /// Refuses a pointer that `cell` stores to `kid` if `kid` is above
    /// `cell`: one of the cells that lead to it, parent by parent.
    ///
    /// [`Self::adopt`] refuses a pointer to a cell that has a parent, and
    /// one of a cell to itself. A cell above that has none, a node of the
    /// write that nothing in the write points at, it would take, and the
    /// cells would then lead to each other in a circle that no walk comes
    /// out of. The circle of [`WriteError::Cycle`] is of the nodes a write
    /// states, and is looked for before a cell the write does not write is
    /// read.
    ///
    /// Only a cell that the plan knew before the pointer was read, and
    /// that points at something, can be above another. Whoever takes a
    /// cell in asks this of such a cell alone ([`Self::take_in`]): for a
    /// cell that the pointer is the first to name, nothing is asked. The
    /// cells that lead to `cell` end, since every pointer that could have
    /// closed a circle of them was refused here or by the write's own
    /// check.
    fn below(&self, cell: usize, kid: usize) -> Result<(), FragmentError> {
        if self.cells[kid].kids.is_empty() {
            return Ok(());
        }
        let mut above = self.cells[cell].parent;
        while let Some(at) = above {
            if at == kid {
                let address = self.cells[kid].origin;
                return Err(WriteError::TwoPointers { address }.into());
            }
            above = self.cells[at].parent;
        }
        Ok(())
    }

    /// The kids of `cell` that the plan has in `page`, in their order.
    fn kids_in(&self, cell: usize, page: usize) -> impl Iterator<Item = usize> + '_ {
        let kids = self.cells[cell].kids.iter().map(|kid| kid.cell);
        kids.filter(move |kid| self.cells[*kid].page == page)
    }
}

/// A walk over the cells of one fragment that counts their bytes
/// ([`Planner::fragment_bytes`]).
struct Walk {
    /// The page of the fragment, as the plan has it.
    page: usize,
    /// The bytes at which the walk ends.
    limit: usize,
    /// The bytes of the cells that were counted, with their slots.
    bytes: usize,
    /// The cells the walk has come to and not counted: the last one is
    /// counted next.
    pending: Vec<usize>,
    /// Whether the count has reached the limit.
    done: bool,
    /// The cells that were counted, in their order: what the walk is held
    /// to ([`held`]).
    #[cfg(any(test, feature = "held_reads"))]
    counted: Vec<usize>,
    /// How many cells the walk read in a step it was already in.
    #[cfg(any(test, feature = "held_reads"))]
    read_along: usize,
    /// How many cells the plan had read when the walk began
    /// ([`Plan::taken`]), and how many the walk took in ([`Self::take`]).
    #[cfg(any(test, feature = "held_reads"))]
    taken_before: usize,
    #[cfg(any(test, feature = "held_reads"))]
    took: usize,
}

impl Walk {
    /// The cell that is counted next, or none when the walk has ended.
    fn next(&mut self) -> Option<usize> {
        match self.done {
            true => None,
            false => self.pending.pop(),
        }
    }

    /// Counts `cell`, which the plan has read, and comes to its kids in
    /// the page unless the count ends the walk.
    fn count(&mut self, plan: &Plan<'_>, cell: usize) {
        #[cfg(any(test, feature = "held_reads"))]
        self.counted.push(cell);
        self.bytes += plan.cells[cell].len + SLOT;
        self.done = self.bytes >= self.limit;
        if !self.done {
            self.pending.extend(plan.kids_in(cell, self.page));
        }
    }

    /// Takes into the plan what was read of `cell`, which the walk has come
    /// to and the plan had not read, and counts it. A walk reads no cell
    /// in another way.
    fn take(
        &mut self,
        plan: &mut Plan<'_>,
        dropped: &HashSet<CellAddress, Mixing>,
        cell: usize,
        stored: &Stored,
    ) -> Result<(), FragmentError> {
        plan.take_in(cell, stored, dropped)?;
        #[cfg(any(test, feature = "held_reads"))]
        {
            self.took += 1;
        }
        self.count(plan, cell);
        Ok(())
    }

    /// Goes on from `cell`, which the plan has not read and which is in
    /// the page `view` shows, for as long as the walk needs nothing but
    /// this page: each cell it comes to is counted, after it is read from
    /// `view` if the plan has not read it, in the order and with the
    /// outcome of reading and counting them one by one.
    ///
    /// Returns a cell that hosts, with what its page holds of it, when
    /// the walk came to one that is not read: it is neither taken into
    /// the plan nor counted. Otherwise the walk has ended, or the cell it
    /// counts next is in another page.
    fn along(
        &mut self,
        plan: &mut Plan<'_>,
        dropped: &HashSet<CellAddress, Mixing>,
        view: &NodePage<'_>,
        mut cell: usize,
    ) -> Result<Option<(usize, CellAddress, Stored)>, FragmentError> {
        loop {
            let address = plan.cells[cell]
                .origin
                .expect("a node the write creates is known");
            let node = view.node(address.slot());
            let stored = node.map(|node| Stored::of(&node, None));
            let stored = stored.ok_or(WriteError::NoNode { address })?;
            if stored.hosted > 0 {
                return Ok(Some((cell, address, stored)));
            }
            self.take(plan, dropped, cell, &stored)?;
            cell = loop {
                let Some(next) = self.next() else {
                    return Ok(None);
                };
                let origin = plan.cells[next].origin;
                if plan.cells[next].read {
                    self.count(plan, next);
                } else if origin.is_some_and(|origin| origin.page() == address.page()) {
                    #[cfg(any(test, feature = "held_reads"))]
                    {
                        self.read_along += 1;
                    }
                    break next;
                } else {
                    self.pending.push(next);
                    return Ok(None);
                }
            };
        }
    }
}

/// The check of the feature `held_reads`, which is on in tests: the walk
/// that counts the bytes of a fragment is held to the walk it stands for.
#[cfg(any(test, feature = "held_reads"))]
pub(super) mod held {
    use std::cell::Cell;

    use super::{Plan, Walk, SLOT};

    /// What was held on one thread.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct Walks {
        /// The walks that were held.
        pub walks: u64,
        /// Those that ended because the count reached their limit.
        pub ended_at_limit: u64,
        /// The cells that walks read in a step they were already in.
        pub read_along: u64,
        /// How many times a walk left a step at a cell that hosts.
        pub left_at_host: u64,
    }

    thread_local! {
        static WALKS: Cell<Walks> = const {
            Cell::new(Walks {
                walks: 0,
                ended_at_limit: 0,
                read_along: 0,
                left_at_host: 0,
            })
        };
    }

    /// What was held on this thread since it began.
    #[cfg(test)]
    pub(crate) fn walks() -> Walks {
        WALKS.get()
    }

    #[cfg(test)]
    thread_local! {
        static LAST: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// The cells that the walk which was held last on this thread
    /// counted, in their order.
    #[cfg(test)]
    pub(super) fn last_counted() -> Vec<usize> {
        LAST.with_borrow(Vec::clone)
    }

    pub(super) fn left_at_a_host() {
        let mut walks = WALKS.get();
        walks.left_at_host += 1;
        WALKS.set(walks);
    }

    /// Makes the walk under `root` again over the plan as `walk` left it,
    /// a cell at a time and with no read: the walk that takes a step for
    /// each cell.
    ///
    /// It runs over what the plan holds of each cell. So it holds the
    /// order of a walk and where it ends, and cannot hold a length to the
    /// cell's page: the tests that count against pages made by hand do
    /// (`a_walk_counts_what_its_page_holds_in_one_step`).
    ///
    /// # Panics
    ///
    /// If it comes to a cell the plan has not read; if it does not count
    /// the cells that `walk` counted, in the same order and to the same
    /// bytes; or if the plan read a cell while `walk` went on that `walk`
    /// did not take in and count ([`Walk::take`]).
    pub(super) fn cell_by_cell(plan: &Plan<'_>, root: usize, walk: &Walk) {
        let mut bytes = 0;
        let mut counted = Vec::with_capacity(walk.counted.len());
        let mut pending = vec![root];
        while let Some(cell) = pending.pop() {
            assert!(plan.cells[cell].read, "cell {cell} is counted and not read");
            counted.push(cell);
            bytes += plan.cells[cell].len + SLOT;
            if bytes >= walk.limit {
                break;
            }
            pending.extend(plan.kids_in(cell, walk.page));
        }
        assert_eq!(
            (bytes, &counted),
            (walk.bytes, &walk.counted),
            "the fragment under cell {root}, counted to {}",
            walk.limit
        );
        assert_eq!(
            plan.taken - walk.taken_before,
            walk.took,
            "the cells read while the fragment under cell {root} was counted"
        );
        let mut walks = WALKS.get();
        walks.walks += 1;
        walks.ended_at_limit += u64::from(walk.done);
        walks.read_along += walk.read_along as u64;
        WALKS.set(walks);
        #[cfg(test)]
        LAST.with_borrow_mut(|last| last.clone_from(&walk.counted));
    }
}

struct Planner<'p, 'w, P> {
    layout: Layout,
    hints: &'p Hints,
    pages: &'p mut P,
    plan: Plan<'w>,
    /// The cells the write holds: its nodes, and the roots of what it moved.
    held: Vec<usize>,
    /// The pages the write deleted a cell from.
    shrunk: Vec<usize>,
    /// The pages that may be above their capacity.
    queue: Vec<usize>,
    /// The fragment roots that no rule can take a cell from.
    stuck: Vec<usize>,
    /// The roots that the hosts of the write name by position
    /// ([`HostedWrite::Set`]).
    stated: Vec<usize>,
    /// Whether the write met a host that is behind (T3).
    met_behind: bool,
    /// The roots the write lets go of ([`Write::dropped`]).
    dropped: HashSet<CellAddress, Mixing>,
}

impl<'p, 'w, P: Pages> Planner<'p, 'w, P> {
    /// A planner of `write` that has read nothing.
    fn new(layout: &Layout, hints: &'p Hints, pages: &'p mut P, write: &'w Write<'w>) -> Self {
        let named = (write.nodes.len() + write.removed.len()).min(SIZED_FOR);
        Self {
            layout: *layout,
            hints,
            pages,
            plan: Plan {
                write,
                cells: Vec::new(),
                pages: Vec::new(),
                of_node: Vec::new(),
                roots: Vec::new(),
                unpointed: Vec::new(),
                unmarked: Vec::new(),
                known: HashMap::with_capacity_and_hasher(KNOWN_FOR_ONE * named, Mixing::default()),
                page_of: HashMap::with_capacity_and_hasher(
                    PAGES_FOR_ONE * named,
                    Mixing::default(),
                ),
                removed: HashSet::with_capacity_and_hasher(
                    write.removed.len().min(SIZED_FOR),
                    Mixing::default(),
                ),
                #[cfg(any(test, feature = "held_reads"))]
                taken: 0,
            },
            held: Vec::new(),
            shrunk: Vec::new(),
            queue: Vec::new(),
            stuck: Vec::new(),
            stated: Vec::new(),
            met_behind: false,
            dropped: write.dropped.iter().copied().collect(),
        }
    }
}

/// For the tests of a walk: what the plan of `write` counts of the fragment
/// under the cell at `root`, up to `limit` ([`Planner::fragment_bytes`]),
/// once it has read the cells that `write` names and before any rule has
/// run. Returns the bytes as the rules are given them, and where the cells
/// that were counted are in the pages, in the order they were counted.
#[cfg(test)]
pub(super) fn counted<P: Pages>(
    layout: &Layout,
    pages: &mut P,
    write: &Write<'_>,
    root: CellAddress,
    limit: usize,
) -> Result<(usize, Vec<CellAddress>), FragmentError> {
    let hints = Hints::new();
    let mut planner = Planner::new(layout, &hints, pages, write);
    planner.build()?;
    let root = planner.plan.known[&root];
    let bytes = planner.fragment_bytes(root, limit)?;
    let at = |cell: usize| planner.plan.cells[cell].origin;
    let cells = held::last_counted().into_iter().map(at);
    let cells = cells.map(|at| at.expect("a cell of the pages"));
    Ok((bytes, cells.collect()))
}

/// Plans `write` on the pages as they are.
///
/// Where the largest size is for what a write changed
/// ([`Upkeep::Changed`]) and the plan finds no room
/// ([`FragmentError::NoRoom`]), the write is planned once more with the
/// largest size asked of every node of it ([`Upkeep::Written`]), and that
/// plan, or its refusal, is the answer. So a write is refused for room
/// under `Changed` only where the plan of the same write under `Written`
/// is, with every other parameter of the layout, the pages, the hints and
/// the hosts that are behind the same. Any other error is the answer of
/// the plan that met it: the two can read other pages, and a page that
/// cannot be read, or a write that names what is not there, can be met by
/// one of them alone.
///
/// The two can differ in that. A page that is above its capacity is
/// settled by what the write holds in it ([`Planner::held_in`]), and a turn
/// of L1 that is passed over can be the one that brings a cell of the
/// write into such a page, above cells the write does not hold: with it
/// they can leave, and without it nothing in the page can. A plan changes
/// no page and no hint, so a second one finds what the first found. What
/// the first read stays read: pages that copy a page for the write that
/// reads it keep the copies of both plans.
pub(super) fn plan<'w, P: Pages>(
    layout: &Layout,
    hints: &Hints,
    pages: &mut P,
    write: &'w Write<'w>,
) -> Result<Plan<'w>, FragmentError> {
    match plan_once(layout, hints, pages, write) {
        Err(FragmentError::NoRoom { .. }) if layout.upkeep() == Upkeep::Changed => {
            plan_once(&layout.under(Upkeep::Written), hints, pages, write)
        }
        planned => planned,
    }
}

/// Plans `write` on the pages as they are, under `layout` and no other.
pub(super) fn plan_once<'w, P: Pages>(
    layout: &Layout,
    hints: &Hints,
    pages: &mut P,
    write: &'w Write<'w>,
) -> Result<Plan<'w>, FragmentError> {
    let mut planner = Planner::new(layout, hints, pages, write);
    let (order, lens, takes, let_go) = planner.build()?;
    planner.place(&order, &lens, takes)?;
    planner.cap()?;
    planner.settle()?;
    planner.absorb()?;
    planner.merge()?;
    planner.settle()?;
    planner.restate()?;
    let mut plan = planner.plan;
    let stay = let_go.into_iter().map(|cell| &plan.cells[cell]);
    let stay = stay
        .filter(|cell| cell.stays())
        .filter_map(|cell| cell.origin);
    plan.unpointed = stay.chain(write.dropped.iter().copied()).collect();
    Ok(plan)
}

fn slot_number(address: CellAddress) -> usize {
    address.slot().index() + 1
}

/// The bytes the cell of `node` takes, without its slot.
fn node_len(
    node: &NodeWrite<'_>,
    index: usize,
    old: Option<&Stored>,
    max: usize,
) -> Result<usize, WriteError> {
    let value = match node.value {
        ValueWrite::Keep => old.ok_or(WriteError::NoValue { node: index })?.value_len,
        ValueWrite::Inline(value) => value.len(),
        ValueWrite::Pending(_) => TreeCell::VALUE_LOCATION_LEN,
    };
    let hosted = match node.hosted {
        HostedWrite::Keep => old.map_or(0, |old| old.hosted),
        HostedWrite::Set(roots) => roots.len(),
    };
    let aggregates = [node.left, node.right].map(|link| link.map(|link| link.aggregate.len()));
    let fits = node.key.len() <= TreeCell::MAX_KEY_LEN
        && node.feature.len() <= TreeCell::MAX_FEATURE_LEN
        && hosted <= TreeCell::MAX_HOSTED_ROOTS
        && aggregates
            .iter()
            .flatten()
            .all(|len| *len <= TreeLink::MAX_AGGREGATE_LEN);
    if !fits {
        return Err(WriteError::FieldTooLong { node: index });
    }
    let len = TreeCell::len_of(
        node.key.len(),
        node.feature.len(),
        aggregates[0],
        aggregates[1],
        hosted,
        value,
    );
    if len > max {
        return Err(WriteError::CellTooLarge {
            node: index,
            len,
            max,
        });
    }
    Ok(len)
}

type Built = (
    Vec<usize>,
    Vec<usize>,
    Vec<(usize, CellAddress)>,
    Vec<usize>,
);

impl<P: Pages> Planner<'_, '_, P> {
    /// Reads the cells the write names and links them as the write leaves
    /// them. Returns the nodes of the write from the top down, the length of
    /// each node's cell, the nodes that take the place of a deleted one
    /// (R2), and the cells that the cell which pointed at them let go of
    /// while no other cell of the write took them.
    fn build(&mut self) -> Result<Built, FragmentError> {
        let write = self.plan.write;
        let max = self.layout.page_size().max_cell_len();
        for &address in write.removed {
            if !self.plan.removed.insert(address) {
                return Err(WriteError::Twice { address }.into());
            }
        }
        let mut olds = Vec::with_capacity(write.nodes.len());
        let mut lens = Vec::with_capacity(write.nodes.len());
        for (index, node) in write.nodes.iter().enumerate() {
            let old = match node.at {
                Some(address) => {
                    let twice = self.plan.removed.contains(&address)
                        || self.plan.known.contains_key(&address);
                    if twice {
                        return Err(WriteError::Twice { address }.into());
                    }
                    let old = self.stored(address, Some(node.key))?;
                    if !old.same_key {
                        return Err(WriteError::KeyChanged { node: index }.into());
                    }
                    Some(old)
                }
                None => None,
            };
            let len = node_len(node, index, old.as_ref(), max)?;
            let cell = self.cell(node.at);
            let entry = &mut self.plan.cells[cell];
            entry.node = Some(index);
            entry.read = true;
            entry.len = old.as_ref().map_or(len, |old| old.len);
            self.plan.of_node.push(cell);
            olds.push(old);
            lens.push(len);
        }

        for (index, node) in write.nodes.iter().enumerate() {
            let cell = self.plan.of_node[index];
            let old = olds[index].as_ref();
            // Whether the cell is rewritten in place: it has every part at
            // its length, and below, every pointer at the cell it named.
            let mut in_place = old.is_some_and(|old| old.same_lengths(node));
            for (side, link) in Side::BOTH.into_iter().zip([node.left, node.right]) {
                let Some(link) = link else { continue };
                let via = Via::Link(side);
                let kept = old.and_then(|old| old.child(via));
                let kid = self
                    .target(link.child, kept)?
                    .ok_or(WriteError::NothingKept { node: index })?;
                in_place &= kept.is_some() && self.plan.cells[kid].origin == kept;
                self.adopt(cell, kid, via)?;
            }
            match node.hosted {
                HostedWrite::Keep => {
                    let roots = old.map_or(&[][..], |old| &old.roots);
                    for (position, root) in roots.iter().enumerate() {
                        if let Some(root) = *root {
                            let kid = self.existing(root)?;
                            self.adopt(cell, kid, Via::Hosted(position as u8))?;
                        }
                    }
                }
                HostedWrite::Set(roots) => {
                    // The pointers of a host that is behind are not known
                    // here, so a write that states them may change one.
                    in_place &= old.is_some_and(|old| !old.behind);
                    for (position, root) in roots.iter().enumerate() {
                        let via = Via::Hosted(position as u8);
                        let kept = old.and_then(|old| old.child(via));
                        let kid = match *root {
                            Some(root) => self.target(root, kept)?,
                            None => None,
                        };
                        in_place &= match kid {
                            Some(kid) => kept.is_some() && self.plan.cells[kid].origin == kept,
                            None => kept.is_none(),
                        };
                        if let Some(kid) = kid {
                            self.adopt(cell, kid, via)?;
                            self.stated.push(kid);
                        }
                    }
                }
            }
            self.plan.cells[cell].in_place = in_place;
        }

        let mut takes = Vec::new();
        let mut tops = Vec::new();
        let mut let_go = Vec::new();
        let mut named = Vec::new();
        for root in write.roots {
            let now = match root.now {
                Some(target) => self.target(target, root.was)?,
                None => None,
            };
            if let Some(cell) = now {
                let entry = &mut self.plan.cells[cell];
                if entry.parent.is_some() || named.contains(&cell) {
                    let address = entry.origin;
                    return Err(WriteError::TwoPointers { address }.into());
                }
                named.push(cell);
                if root.anchored {
                    if entry.origin != root.was {
                        let_go.push(cell);
                    }
                } else {
                    entry.free = true;
                    if entry.node.is_some() {
                        tops.push(cell);
                    }
                    if let Some(was) = root.was.filter(|was| self.plan.removed.contains(was)) {
                        takes.push((cell, was));
                    }
                }
            }
            self.plan.roots.push(now);
        }
        for &address in write.dropped {
            let named = self
                .plan
                .known
                .get(&address)
                .map(|cell| &self.plan.cells[*cell]);
            let written = named.is_some_and(|cell| cell.node.is_some());
            if written || self.plan.removed.contains(&address) {
                return Err(WriteError::Twice { address }.into());
            }
            if named.is_some() {
                return Err(WriteError::Dropped { address }.into());
            }
        }

        let mut former = HashSet::new();
        for &address in write.removed {
            let old = self.stored(address, None)?;
            let cell = self.cell(Some(address));
            let entry = &mut self.plan.cells[cell];
            entry.removed = true;
            entry.read = true;
            entry.len = old.len;
            former.extend(old.children());
        }

        for &cell in &self.plan.of_node {
            let entry = &mut self.plan.cells[cell];
            if entry.parent.is_none() && !entry.free {
                entry.anchored = true;
                tops.push(cell);
            }
        }
        let mut order = Vec::with_capacity(write.nodes.len());
        tops.reverse();
        while let Some(cell) = tops.pop() {
            order.push(cell);
            let kids = self.plan.cells[cell].kids.iter().rev();
            tops.extend(
                kids.map(|kid| kid.cell)
                    .filter(|kid| self.plan.cells[*kid].node.is_some()),
            );
        }
        if order.len() != write.nodes.len() {
            return Err(WriteError::Cycle.into());
        }

        for &cell in &order {
            let entry = &self.plan.cells[cell];
            let Some(old) = entry.node.and_then(|node| olds[node].as_ref()) else {
                continue;
            };
            for kid in &entry.kids {
                let was = old.child(kid.via);
                if let Some(was) = was.filter(|was| self.plan.removed.contains(was)) {
                    takes.push((kid.cell, was));
                }
            }
            former.extend(old.children());
        }
        for &cell in &order {
            let entry = &self.plan.cells[cell];
            if entry.anchored && entry.origin.is_some_and(|at| former.contains(&at)) {
                let_go.push(cell);
            }
        }
        // A dropped root that a cell of the write pointed at is in that
        // cell's page, which the write changes, or it has its mark. The mark
        // of any other is read here: the cell that points at it is a host
        // that is behind (T3), which the write does not hold or does not
        // follow.
        for &address in write.dropped {
            if !former.contains(&address) && !self.marked(address)? {
                self.plan.unmarked.push(address);
            }
        }
        Ok((order, lens, takes, let_go))
    }

    /// Reads whether the cell at `address` is marked as the root of its
    /// fragment.
    fn marked(&mut self, address: CellAddress) -> Result<bool, FragmentError> {
        let marked = self.pages.read(address.page(), |view| {
            let node = view.node(address.slot());
            node.map(|node| node.is_fragment_root())
        })?;
        Ok(marked.ok_or(WriteError::NoNode { address })?)
    }

    /// The roots that hosts name in a write that met a host that is behind
    /// (T3). The mark of such a root can be behind as the pointer was: the
    /// host may have left the root's page in a write that did not follow
    /// the pointer. The host that names the root now is that host, or a new
    /// cell that took its Merks over (D5), so every root that is named by
    /// position is looked at. One that the write leaves in another page
    /// than its host has its mark read here. The mark of any other is the
    /// plan's to give, in a page the write changes.
    ///
    /// No host is behind on existing GroveVersions, and nothing is read.
    fn restate(&mut self) -> Result<(), FragmentError> {
        if !self.met_behind {
            return Ok(());
        }
        for cell in std::mem::take(&mut self.stated) {
            let entry = &self.plan.cells[cell];
            let host = entry.parent.expect("a root that is named has its host");
            let apart = self.plan.cells[host].page != entry.page;
            let unwritten = entry.stays() && entry.node.is_none();
            if let Some(address) = entry.origin.filter(|_| apart && unwritten)
                && !self.marked(address)?
            {
                self.plan.unmarked.push(address);
            }
        }
        Ok(())
    }

    /// Sections 5 and 8: the cells of deleted nodes go, a node that takes
    /// the place of a deleted one takes its page, and every node of the
    /// write gets its cell, a new one in the page of its parent.
    fn place(
        &mut self,
        order: &[usize],
        lens: &[usize],
        takes: Vec<(usize, CellAddress)>,
    ) -> Result<(), FragmentError> {
        let inherited: Vec<(usize, usize)> = takes
            .into_iter()
            .map(|(taker, was)| (taker, self.plan.cells[self.plan.known[&was]].home))
            .collect();
        for cell in 0..self.plan.cells.len() {
            if !self.plan.cells[cell].removed {
                continue;
            }
            let page = self.plan.cells[cell].home;
            if !self.shrunk.contains(&page) {
                self.shrunk.push(page);
            }
            self.leave(cell)?;
        }
        let mut reserved = Vec::new();
        for (taker, page) in inherited {
            if self.plan.pages[page].cells() == 0 {
                continue;
            }
            match self.plan.cells[taker].page {
                NOWHERE => reserved.push((taker, page)),
                at if at != page => self.lift(taker, page, Cause::Delete)?,
                _ => {}
            }
        }
        let changed_alone = self.layout.upkeep() == Upkeep::Changed;
        for &cell in order {
            let node = self.plan.cells[cell]
                .node
                .expect("the order lists nodes of the write");
            self.held.push(cell);
            let page = self.plan.cells[cell].page;
            let entry = &self.plan.cells[cell];
            debug_assert!(
                !entry.in_place || (entry.len == lens[node] && page != NOWHERE),
                "a cell that is rewritten in place keeps its length and its page"
            );
            // A cell that is rewritten in place takes the bytes it took, so
            // its page is as full as it was. Where the largest size is for
            // what a write changed, the page is not read for such a cell:
            // it is the cell the write holds there, and no more. A rule
            // that asks how full the page is reads it then (`used`,
            // `beside`).
            if changed_alone && self.plan.cells[cell].in_place {
                self.plan.pages[page].held = Some(cell);
                continue;
            }
            if page != NOWHERE {
                self.resize(cell, lens[node])?;
                continue;
            }
            let entry = &self.plan.cells[cell];
            let placed =
                |cell: usize| Some(self.plan.cells[cell].page).filter(|page| *page != NOWHERE);
            let page = reserved
                .iter()
                .find(|(taker, _)| *taker == cell)
                .map(|(_, page)| *page)
                .filter(|page| self.plan.pages[*page].cells() > 0)
                .or_else(|| entry.parent.and_then(placed))
                .or_else(|| entry.kids.iter().find_map(|kid| placed(kid.cell)));
            let page = match page {
                Some(page) => page,
                None => self.fresh(),
            };
            self.plan.cells[cell].len = lens[node];
            self.arrive(cell, page)?;
        }
        Ok(())
    }

    fn fresh(&mut self) -> usize {
        let order = self.plan.pages.len();
        self.plan.pages.push(PagePlan::of(None, order));
        order
    }

    /// Reads what the page holds, before the plan changes anything in it.
    fn load(&mut self, page: usize) -> Result<(), FragmentError> {
        if self.plan.pages[page].loaded {
            return Ok(());
        }
        let id = self.plan.pages[page]
            .id
            .expect("a page the write starts needs no read");
        let (slots, cells, used) = self.pages.read(id, |view| {
            let page = view.tree_page();
            (page.slot_count(), page.cell_count(), used_bytes(&view))
        })?;
        let entry = &mut self.plan.pages[page];
        entry.loaded = true;
        entry.top = slots;
        entry.staying = cells;
        entry.cell_bytes = used - SLOT * slots;
        Ok(())
    }

    fn used(&mut self, page: usize) -> Result<usize, FragmentError> {
        self.load(page)?;
        Ok(self.plan.pages[page].used())
    }

    fn stored(
        &mut self,
        address: CellAddress,
        key: Option<&[u8]>,
    ) -> Result<Stored, FragmentError> {
        let stored = self.pages.read(address.page(), |view| {
            let node = view.node(address.slot());
            node.map(|node| Stored::of(&node, key))
        })?;
        let stored = stored.ok_or(WriteError::NoNode { address })?;
        Ok(self.followed(address, stored))
    }

    /// What the cell at `address` holds, as the plan follows it: a host
    /// that is behind has no root (T3).
    fn followed(&mut self, address: CellAddress, mut stored: Stored) -> Stored {
        if stored.hosted > 0 && self.pages.host_is_behind(address) {
            self.met_behind = true;
            stored.roots.clear();
            stored.behind = true;
        }
        stored
    }

    fn cell(&mut self, origin: Option<CellAddress>) -> usize {
        self.plan.cell(origin)
    }

    /// The cell at `address`, which a link or pointer names.
    fn existing(&mut self, address: CellAddress) -> Result<usize, FragmentError> {
        self.plan.existing(address)
    }

    fn target(
        &mut self,
        target: Target,
        kept: Option<CellAddress>,
    ) -> Result<Option<usize>, FragmentError> {
        match target {
            Target::Node(node) => match self.plan.of_node.get(node) {
                Some(cell) => Ok(Some(*cell)),
                None => Err(WriteError::NoSuchNode { node }.into()),
            },
            Target::At(address) => self.existing(address).map(Some),
            Target::Kept => kept.map(|address| self.existing(address)).transpose(),
        }
    }

    fn adopt(&mut self, parent: usize, kid: usize, via: Via) -> Result<(), FragmentError> {
        self.plan.adopt(parent, kid, via)
    }

    /// Reads a cell the write does not write: its length and what it points
    /// at.
    ///
    /// A hosted root that the write lets go of is not among that: it stays
    /// in its page (D1), so the pointer of a host that the write does not
    /// write names nothing here, and keeps its bytes.
    fn read(&mut self, cell: usize) -> Result<(), FragmentError> {
        if self.plan.cells[cell].read {
            return Ok(());
        }
        let address = self.plan.cells[cell]
            .origin
            .expect("a node the write creates is known");
        let stored = self.stored(address, None)?;
        self.plan.take_in(cell, &stored, &self.dropped)
    }

    /// Takes a cell out of the page the plan has it in.
    fn leave(&mut self, cell: usize) -> Result<(), FragmentError> {
        let page = self.plan.cells[cell].page;
        self.load(page)?;
        let entry = &mut self.plan.cells[cell];
        entry.page = NOWHERE;
        let (len, home, origin) = (entry.len, entry.home, entry.origin);
        let entry = &mut self.plan.pages[page];
        entry.cell_bytes -= len;
        entry.touched = true;
        if entry.held == Some(cell) {
            entry.held = None;
        }
        if home != page {
            entry.arriving -= 1;
            return Ok(());
        }
        entry.staying -= 1;
        if origin.map(slot_number) == Some(entry.top) {
            self.lower_top(page)?;
        }
        Ok(())
    }

    /// Finds the highest slot of the page whose cell stays.
    fn lower_top(&mut self, page: usize) -> Result<(), FragmentError> {
        let entry = &self.plan.pages[page];
        let (id, mut top) = (entry.id.expect("a cell of the pages left it"), entry.top);
        if entry.staying == 0 {
            top = 0;
        }
        let (known, cells) = (&self.plan.known, &self.plan.cells);
        let top = self.pages.read(id, |view| {
            while let Some(slot) = top.checked_sub(1).and_then(SlotId::from_index) {
                let stays = match known.get(&CellAddress::new(id, slot)) {
                    Some(cell) => cells[*cell].page == page,
                    None => view.node(slot).is_some(),
                };
                if stays {
                    break;
                }
                top -= 1;
            }
            top
        })?;
        self.plan.pages[page].top = top;
        Ok(())
    }

    /// Puts a cell into a page.
    fn arrive(&mut self, cell: usize, page: usize) -> Result<(), FragmentError> {
        self.load(page)?;
        let entry = &mut self.plan.cells[cell];
        entry.page = page;
        let (len, home, origin) = (entry.len, entry.home, entry.origin);
        let entry = &mut self.plan.pages[page];
        entry.cell_bytes += len;
        entry.touched = true;
        entry.held = Some(cell);
        if home == page {
            entry.staying += 1;
            entry.top = entry.top.max(origin.map_or(0, slot_number));
        } else {
            entry.arriving += 1;
        }
        self.queue(page);
        Ok(())
    }

    fn resize(&mut self, cell: usize, len: usize) -> Result<(), FragmentError> {
        let page = self.plan.cells[cell].page;
        self.load(page)?;
        let old = std::mem::replace(&mut self.plan.cells[cell].len, len);
        let entry = &mut self.plan.pages[page];
        entry.cell_bytes = entry.cell_bytes - old + len;
        entry.touched = true;
        entry.held = Some(cell);
        self.queue(page);
        Ok(())
    }

    fn queue(&mut self, page: usize) {
        let entry = &mut self.plan.pages[page];
        if entry.used() > self.layout.capacity() && !entry.queued {
            entry.queued = true;
            self.queue.push(page);
        }
    }

    /// Carries one cell to another page. It is not rewritten in place
    /// then, whatever the write does to it.
    fn carry(&mut self, cell: usize, to: usize, cause: Cause) -> Result<(), FragmentError> {
        self.read(cell)?;
        self.leave(cell)?;
        self.arrive(cell, to)?;
        let entry = &mut self.plan.cells[cell];
        entry.cause = Some(cause);
        entry.in_place = false;
        Ok(())
    }

    /// The cell that points at `cell`, for the rules: none for a node that
    /// nothing in the write points at (T3).
    fn up(&self, cell: usize) -> Option<usize> {
        let cell = &self.plan.cells[cell];
        cell.parent.filter(|_| !cell.anchored)
    }

    /// Whether the write may carry the cell: it holds the cell that points
    /// at it, or its caller holds the pointer (F4).
    fn movable(&self, cell: usize) -> bool {
        self.up(cell).is_some() || self.plan.cells[cell].free
    }

    /// The kids of `cell` that the plan has in `page`, in their order.
    fn kids_in(&self, cell: usize, page: usize) -> impl Iterator<Item = usize> + '_ {
        self.plan.kids_in(cell, page)
    }

    /// The root of the fragment `cell` is in.
    fn fragment_root(&self, mut cell: usize) -> usize {
        let page = self.plan.cells[cell].page;
        while let Some(parent) = self.up(cell) {
            if self.plan.cells[parent].page != page {
                break;
            }
            cell = parent;
        }
        cell
    }

    /// The cells of the fragment under `root`: `root` and everything below
    /// it that is reached without leaving its page.
    fn fragment(&mut self, root: usize) -> Result<Vec<usize>, FragmentError> {
        let page = self.plan.cells[root].page;
        let mut cells = vec![root];
        let mut next = 0;
        while let Some(&cell) = cells.get(next) {
            next += 1;
            self.read(cell)?;
            cells.extend(self.kids_in(cell, page));
        }
        Ok(cells)
    }

    /// The bytes of the fragment under `root` with their slots, counted up
    /// to `limit`.
    ///
    /// The cells of the fragment that the plan has not read are read as
    /// the walk comes to them, as [`Self::read`] reads a cell. They are
    /// cells of one page, so the walk stays in one step on that page for
    /// as long as it can ([`Walk::along`]), where a read of each by itself
    /// is a step for each. It leaves the step at a cell that hosts: whether
    /// a host is behind is asked of the pages (T3), which answer between
    /// two steps.
    ///
    /// What is read of a cell, and what follows from it, is what a step
    /// for that cell alone gives, on pages that show a page alike in every
    /// step of a plan. Pages that fail a step by how many were taken can
    /// fail at another cell than they would with a step for each, or not
    /// fail: the walk takes fewer steps where a step reads more than one
    /// cell.
    ///
    /// In tests, and in a build with the feature `held_reads`, a walk that
    /// ends with a count is held to the walk cell by cell over the plan as
    /// it left it (`held::cell_by_cell`).
    fn fragment_bytes(&mut self, root: usize, limit: usize) -> Result<usize, FragmentError> {
        let mut walk = Walk {
            page: self.plan.cells[root].page,
            limit,
            bytes: 0,
            pending: vec![root],
            done: false,
            #[cfg(any(test, feature = "held_reads"))]
            counted: Vec::new(),
            #[cfg(any(test, feature = "held_reads"))]
            read_along: 0,
            #[cfg(any(test, feature = "held_reads"))]
            taken_before: self.plan.taken,
            #[cfg(any(test, feature = "held_reads"))]
            took: 0,
        };
        while let Some(cell) = walk.next() {
            if self.plan.cells[cell].read {
                walk.count(&self.plan, cell);
                continue;
            }
            let address = self.plan.cells[cell]
                .origin
                .expect("a node the write creates is known");
            let Self {
                pages,
                plan,
                dropped,
                ..
            } = self;
            let host = pages.read(address.page(), |view| {
                walk.along(plan, dropped, &view, cell)
            })??;
            if let Some((host, address, stored)) = host {
                #[cfg(any(test, feature = "held_reads"))]
                held::left_at_a_host();
                let stored = self.followed(address, stored);
                walk.take(&mut self.plan, &self.dropped, host, &stored)?;
            }
        }
        #[cfg(any(test, feature = "held_reads"))]
        held::cell_by_cell(&self.plan, root, &walk);
        Ok(walk.bytes)
    }

    /// Moves the fragment under `root` to the page `to`.
    fn move_fragment(&mut self, root: usize, to: usize, cause: Cause) -> Result<(), FragmentError> {
        for cell in self.fragment(root)? {
            self.carry(cell, to, cause)?;
        }
        self.plan.pages[to].held = Some(root);
        self.held.push(root);
        Ok(())
    }

    /// Moves one cell to the page `to`. A small Merk it hosts in its own
    /// page goes with it (L2).
    fn lift(&mut self, cell: usize, to: usize, cause: Cause) -> Result<(), FragmentError> {
        let page = self.plan.cells[cell].page;
        self.read(cell)?;
        let small = self.layout.small_fragment();
        let hosted: Vec<usize> = self.plan.cells[cell]
            .kids
            .iter()
            .filter(|kid| matches!(kid.via, Via::Hosted(_)))
            .map(|kid| kid.cell)
            .filter(|kid| self.plan.cells[*kid].page == page)
            .collect();
        for root in hosted {
            if self.fragment_bytes(root, small)? < small {
                for member in self.fragment(root)? {
                    self.carry(member, to, cause)?;
                }
            }
        }
        self.carry(cell, to, cause)?;
        self.held.push(cell);
        Ok(())
    }

    /// The distinct roots of the fragments that hold a cell the write holds
    /// in a page that passes `keep`, in the order the write took them.
    fn held_roots(&self, keep: impl Fn(usize) -> bool) -> Vec<usize> {
        let mut roots = Vec::new();
        for &cell in &self.held {
            if keep(self.plan.cells[cell].page) {
                let root = self.fragment_root(cell);
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        roots
    }

    /// L1 and L2: every fragment the write holds that is above the largest
    /// size is split where it is. Its root goes up to the page of its
    /// parent, and what hung below it stays as fragments of its own.
    ///
    /// Each cell the write holds has one turn, in the order the write
    /// took them and then in the order this rule carried them, and a turn
    /// sends up one root at most.
    ///
    /// Where the largest size is for what a write changed
    /// ([`Upkeep::Changed`]), the turn of a cell that is rewritten in
    /// place is passed over: the write leaves that cell as long as it was,
    /// with the cells below it that it had. A fragment the write made
    /// larger holds a cell that is not rewritten in place, which has its
    /// turn: one that arrived, got longer or points at another cell.
    ///
    /// Nothing is done in the place of a turn that is passed over. So what
    /// is left below a root that went up can stay above the largest size,
    /// where the turn of a node of the write between that root and the
    /// changed cell would have sent the next root up.
    fn cap(&mut self) -> Result<(), FragmentError> {
        let largest = self.layout.largest_fragment();
        if largest >= self.layout.capacity() {
            return Ok(());
        }
        let changed_alone = self.layout.upkeep() == Upkeep::Changed;
        let mut within = Vec::new();
        let mut next = 0;
        while let Some(&cell) = self.held.get(next) {
            next += 1;
            if changed_alone && self.plan.cells[cell].in_place {
                continue;
            }
            let root = self.fragment_root(cell);
            let Some(parent) = self.up(root) else {
                continue;
            };
            if within.contains(&root) {
                continue;
            }
            if self.fragment_bytes(root, largest + 1)? <= largest {
                within.push(root);
                continue;
            }
            within.clear();
            let above = self.plan.cells[parent].page;
            self.lift(root, above, Cause::Split)?;
        }
        Ok(())
    }

    /// A cell of `page` that the write holds, and whose fragment a rule can
    /// still take something from.
    fn held_in(&self, page: usize) -> Option<usize> {
        let last = self.plan.pages[page].held;
        last.into_iter()
            .chain(self.held.iter().rev().copied())
            .filter(|cell| self.plan.cells[*cell].page == page)
            .find(|cell| !self.stuck.contains(&self.fragment_root(*cell)))
    }

    /// Section 6.2: until no page is above its capacity, the fragment the
    /// write holds in an overfull page leaves it, or is split.
    fn settle(&mut self) -> Result<(), FragmentError> {
        let capacity = self.layout.capacity();
        self.stuck.clear();
        let mut rounds = 0;
        while let Some(page) = self.queue.pop() {
            self.plan.pages[page].queued = false;
            let before = self.plan.pages[page].used();
            if before <= capacity {
                continue;
            }
            rounds += 1;
            let held = self
                .held_in(page)
                .filter(|_| rounds <= 64 * self.plan.cells.len());
            let Some(held) = held else {
                let page = self.plan.pages[page].id;
                return Err(FragmentError::NoRoom { page });
            };
            let root = self.fragment_root(held);
            let bytes = self.fragment_bytes(root, usize::MAX)?;
            if bytes * 2 >= capacity || !self.movable(root) {
                self.split(page, root)?;
            } else {
                self.move_out(root, bytes)?;
            }
            if self.plan.pages[page].used() >= before {
                self.stuck.push(root);
            }
            self.queue(page);
        }
        Ok(())
    }

    /// V1, V3: a fragment that is not the bulk of an overfull page leaves
    /// it: to its parent's page when it is small, otherwise to a page beside
    /// it that has room, or to a fresh page.
    fn move_out(&mut self, root: usize, bytes: usize) -> Result<(), FragmentError> {
        let above = self.up(root).map(|parent| self.plan.cells[parent].page);
        match above {
            Some(above) if bytes < self.layout.small_fragment() => {
                self.move_fragment(root, above, Cause::Merge)
            }
            _ => {
                let to = self.destination(root, bytes)?;
                self.move_fragment(root, to, Cause::Split)
            }
        }
    }

    /// V4: where a fragment of `bytes` bytes that leaves its page goes.
    fn destination(&mut self, root: usize, bytes: usize) -> Result<usize, FragmentError> {
        Ok(match self.beside(root, bytes)? {
            Some(page) => page,
            None => self.fresh(),
        })
    }

    /// The fullest page that holds another child of the fragment the parent
    /// of `root` is in, and that stays within the seed limit when it takes
    /// `bytes` more. A page the plan has not read is judged by its hint and
    /// read before it is chosen: it is taken if it has the room, whatever
    /// the hint said, and passed over for the next one if it has not.
    fn beside(&mut self, root: usize, bytes: usize) -> Result<Option<usize>, FragmentError> {
        let seed_limit = self.layout.seed_limit();
        let Some(parent) = self.up(root).filter(|_| bytes <= seed_limit) else {
            return Ok(None);
        };
        let from = self.plan.cells[root].page;
        let above = self.plan.cells[parent].page;
        let top = self.fragment_root(parent);
        let mut beside = Vec::new();
        for cell in self.fragment(top)? {
            for kid in &self.plan.cells[cell].kids {
                let page = self.plan.cells[kid.cell].page;
                if page != above && page != from && page != NOWHERE && !beside.contains(&page) {
                    beside.push(page);
                }
            }
        }
        // A page in which the write holds a cell is known to it as it is,
        // and not by a hint: also where the cell is rewritten in place
        // and the page was not read for it ([`Upkeep::Changed`]).
        for &page in &beside {
            if self.plan.pages[page].held.is_some() {
                self.load(page)?;
            }
        }
        loop {
            let best = beside
                .iter()
                .filter_map(|&page| {
                    let entry = &self.plan.pages[page];
                    let used = match entry.loaded {
                        true => entry.used(),
                        false => self.hints.used(entry.id?)?,
                    };
                    (used + bytes <= seed_limit).then_some((used, Reverse(entry.rank()), page))
                })
                .max();
            let Some((_, _, page)) = best else {
                return Ok(None);
            };
            if self.plan.pages[page].loaded {
                return Ok(Some(page));
            }
            self.load(page)?;
            if self.plan.pages[page].used() + bytes <= seed_limit {
                return Ok(Some(page));
            }
        }
    }

    /// V2: the fragment under `root` is the bulk of the overfull page
    /// `page`. Its root goes up to the page of its parent, or stays when the
    /// write cannot carry it, and of what is left the parts that come
    /// closest to half a page leave together.
    fn split(&mut self, page: usize, root: usize) -> Result<(), FragmentError> {
        let capacity = self.layout.capacity();
        let above = self
            .up(root)
            .map(|parent| self.plan.cells[parent].page)
            .filter(|above| *above != page);
        let mut parts = Vec::new();
        self.peel(page, root, above, &mut parts)?;
        loop {
            let total: usize = parts.iter().map(|part: &(usize, usize)| part.1).sum();
            let largest = parts
                .iter()
                .enumerate()
                .max_by_key(|(index, part)| (part.1, Reverse(*index)));
            let Some((index, &(largest, bytes))) = largest else {
                break;
            };
            let lopsided = parts.len() == 1 || bytes * 4 > total * 3;
            if !lopsided || bytes * 2 < capacity {
                break;
            }
            parts.remove(index);
            self.peel(page, largest, above, &mut parts)?;
        }
        let used = self.plan.pages[page].used();
        if parts.is_empty() || used * 2 <= capacity {
            return Ok(());
        }
        let leaving: Vec<(usize, usize)> = choose(&parts, used - capacity / 2)
            .into_iter()
            .map(|index| parts[index])
            .collect();
        let bytes = leaving.iter().map(|part| part.1).sum();
        let to = self.destination(leaving[0].0, bytes)?;
        for (part, _) in leaving {
            self.move_fragment(part, to, Cause::Split)?;
        }
        Ok(())
    }

    /// Takes `cell` off the top of a fragment of `page`. It moves to
    /// `above`, the page of its parent, or stays where it is. What hangs
    /// below it in the page becomes parts.
    fn peel(
        &mut self,
        page: usize,
        cell: usize,
        above: Option<usize>,
        parts: &mut Vec<(usize, usize)>,
    ) -> Result<(), FragmentError> {
        self.read(cell)?;
        let below: Vec<usize> = self.kids_in(cell, page).collect();
        if let Some(above) = above {
            self.lift(cell, above, Cause::Split)?;
        }
        for kid in below {
            if self.plan.cells[kid].page == page {
                let bytes = self.fragment_bytes(kid, usize::MAX)?;
                parts.push((kid, bytes));
            }
        }
        Ok(())
    }

    /// M1: a small fragment whose parent's page has room for it moves
    /// there.
    fn absorb(&mut self) -> Result<(), FragmentError> {
        let small = self.layout.small_fragment();
        for root in self.held_roots(|_| true) {
            let Some(parent) = self.up(root) else {
                continue;
            };
            let above = self.plan.cells[parent].page;
            if above == self.plan.cells[root].page {
                continue;
            }
            let bytes = self.fragment_bytes(root, small)?;
            if bytes < small && self.used(above)? + bytes <= self.layout.capacity() {
                self.move_fragment(root, above, Cause::Merge)?;
            }
        }
        Ok(())
    }

    /// M2: a page that a delete left below the merge threshold gives the
    /// fragments the write holds in it to a page beside it.
    fn merge(&mut self) -> Result<(), FragmentError> {
        for index in 0..self.shrunk.len() {
            let page = self.shrunk[index];
            let entry = &self.plan.pages[page];
            if entry.cells() == 0 || entry.used() >= self.layout.merge_below() {
                continue;
            }
            for root in self.held_roots(|held| held == page) {
                let bytes = self.fragment_bytes(root, usize::MAX)?;
                if let Some(to) = self.beside(root, bytes)? {
                    self.move_fragment(root, to, Cause::Merge)?;
                }
            }
        }
        Ok(())
    }
}

/// Which parts to move so that their bytes come closest to `target`: their
/// places in `parts`, in rising order, never none. Ties go to fewer bytes,
/// then to the choice whose last part is listed earliest (V6).
///
/// Up to twelve parts every choice is tried. Above that the parts are taken
/// largest first, each only if it brings the sum closer.
fn choose(parts: &[(usize, usize)], target: usize) -> Vec<usize> {
    if parts.len() > EXHAUSTIVE {
        let mut order: Vec<usize> = (0..parts.len()).collect();
        order.sort_by_key(|index| (Reverse(parts[*index].1), *index));
        let mut chosen = Vec::new();
        let mut sum = 0usize;
        for index in order {
            let bytes = parts[index].1;
            if chosen.is_empty() || (sum + bytes).abs_diff(target) < sum.abs_diff(target) {
                chosen.push(index);
                sum += bytes;
            }
        }
        chosen.sort_unstable();
        return chosen;
    }
    let chosen = |mask: u32, index: usize| mask & (1 << index) != 0;
    let best = (1u32..1 << parts.len())
        .map(|mask| {
            let sum: usize = (0..parts.len())
                .filter(|index| chosen(mask, *index))
                .map(|index| parts[index].1)
                .sum();
            (sum.abs_diff(target), sum, mask)
        })
        .min()
        .map_or(0, |best| best.2);
    (0..parts.len())
        .filter(|index| chosen(best, *index))
        .collect()
}

#[cfg(test)]
pub(super) fn choose_parts(parts: &[usize], target: usize) -> Vec<usize> {
    let parts: Vec<(usize, usize)> = parts.iter().map(|bytes| (0, *bytes)).collect();
    choose(&parts, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kid(cell: usize) -> Kid {
        let via = match cell % 3 {
            0 => Via::Link(Side::Left),
            1 => Via::Link(Side::Right),
            _ => Via::Hosted(cell as u8),
        };
        Kid { cell, via }
    }

    fn hash_of(key: impl std::hash::Hash) -> u64 {
        let mut hasher = Mixed::default();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// The keys of the planner's maps, the cells of many pages and the
    /// pages themselves, get hashes of their own, and a table of any size
    /// up to a thousand places finds them spread over its places: none
    /// holds more than a few times its share.
    #[test]
    fn the_keys_of_the_planners_maps_are_spread() {
        let page = |raw: u32| PageId::new(raw).expect("not zero");
        let slot = |raw: u16| SlotId::new(raw).expect("not zero");
        let mut hashes = HashSet::new();
        for places in [16usize, 128, 1024] {
            let mut cells = vec![0u32; places];
            let mut pages = vec![0u32; places];
            for raw in 1..=400u32 {
                pages[hash_of(page(raw)) as usize % places] += 1;
                hashes.insert(hash_of(page(raw)));
                for at in 1..=64u16 {
                    let address = CellAddress::new(page(raw), slot(at));
                    cells[hash_of(address) as usize % places] += 1;
                    hashes.insert(hash_of(address));
                }
            }
            let share = |keys: usize| (keys / places).max(1) as u32;
            let most = |counts: &[u32]| counts.iter().copied().max().unwrap_or(0);
            assert!(
                most(&cells) <= 3 * share(400 * 64),
                "{places}: {}",
                most(&cells)
            );
            assert!(most(&pages) <= 8 * share(400), "{places}: {}", most(&pages));
            // The tag of a key is taken from the top of its hash.
            let mut tags = [0u32; 128];
            for raw in 1..=400u32 {
                tags[(hash_of(page(raw)) >> 57) as usize] += 1;
            }
            assert!(most(&tags) <= 16, "{}", most(&tags));
        }
        assert_eq!(hashes.len(), 400 + 400 * 64);
        // A key of bytes is mixed byte by byte.
        let bytes = |key: &[u8]| {
            let mut hasher = Mixed::default();
            hasher.write(key);
            hasher.finish()
        };
        assert_ne!(bytes(b"ab"), bytes(b"ba"));
        assert_ne!(bytes(b"a"), bytes(b""));
    }

    /// A cell's kids are the slice of what was pushed, in that order,
    /// whether the cell keeps them in itself or not.
    #[test]
    fn kids_are_what_was_pushed_in_their_order() {
        for count in 0..=2 * KIDS_INLINE + 1 {
            let mut kids = Kids::new();
            assert!(kids.is_empty());
            for cell in 0..count {
                kids.push(kid(cell));
                assert_eq!(kids.len(), cell + 1);
            }
            let cells: Vec<usize> = kids.iter().map(|kid| kid.cell).collect();
            assert_eq!(cells, (0..count).collect::<Vec<_>>());
            let back: Vec<usize> = kids.iter().rev().map(|kid| kid.cell).collect();
            assert_eq!(back, (0..count).rev().collect::<Vec<_>>());
            for (at, found) in (&kids).into_iter().enumerate() {
                assert_eq!((found.cell, found.via), (at, kid(at).via));
            }
            assert_eq!(kids.get(count).map(|kid| kid.cell), None);
            assert_eq!(
                kids.spilled.capacity() == 0,
                count <= KIDS_INLINE,
                "{count} kids"
            );
        }
    }
}
