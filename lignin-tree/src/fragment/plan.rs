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
};

use lignin_format::{
    tree_page::{Side, TreeCell, TreeLink, TreePage},
    CellAddress, PageId, SlotId,
};

use super::{
    error::{FragmentError, WriteError},
    layout::Layout,
    pages::{used_bytes, Hints, Pages},
    report::Cause,
    write::{HostedWrite, NodeWrite, Target, ValueWrite, Write},
};

/// The page of a cell that is in none: a node the write creates and has not
/// placed yet, or a node it deletes.
pub(super) const NOWHERE: usize = usize::MAX;

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
    pub kids: Vec<Kid>,
    /// The rule that carried the cell last.
    pub cause: Option<Cause>,
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
    hosted: Vec<Option<CellAddress>>,
    value_len: usize,
    same_key: bool,
}

impl Stored {
    fn children(&self) -> impl Iterator<Item = CellAddress> + '_ {
        self.links
            .iter()
            .chain(&self.hosted)
            .filter_map(|child| *child)
    }

    fn child(&self, via: Via) -> Option<CellAddress> {
        match via {
            Via::Link(side) => self.links[side as usize],
            Via::Hosted(position) => self.hosted.get(usize::from(position)).copied().flatten(),
        }
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
    known: HashMap<CellAddress, usize>,
    page_of: HashMap<PageId, usize>,
    removed: HashSet<CellAddress>,
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
}

/// Plans `write` on the pages as they are.
pub(super) fn plan<'w, P: Pages>(
    layout: &Layout,
    hints: &Hints,
    pages: &mut P,
    write: &'w Write<'w>,
) -> Result<Plan<'w>, FragmentError> {
    let mut planner = Planner {
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
            known: HashMap::new(),
            page_of: HashMap::new(),
            removed: HashSet::new(),
        },
        held: Vec::new(),
        shrunk: Vec::new(),
        queue: Vec::new(),
        stuck: Vec::new(),
    };
    let (order, lens, takes, let_go) = planner.build()?;
    planner.place(&order, &lens, takes)?;
    planner.cap()?;
    planner.settle()?;
    planner.absorb()?;
    planner.merge()?;
    planner.settle()?;
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
        HostedWrite::Keep => old.map_or(0, |old| old.hosted.len()),
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
            for (side, link) in Side::BOTH.into_iter().zip([node.left, node.right]) {
                let Some(link) = link else { continue };
                let via = Via::Link(side);
                let kept = old.and_then(|old| old.child(via));
                let kid = self
                    .target(link.child, kept)?
                    .ok_or(WriteError::NothingKept { node: index })?;
                self.adopt(cell, kid, via)?;
            }
            match node.hosted {
                HostedWrite::Keep => {
                    let roots = old.map_or(&[][..], |old| &old.hosted);
                    for (position, root) in roots.iter().enumerate() {
                        if let Some(root) = *root {
                            let kid = self.existing(root)?;
                            self.adopt(cell, kid, Via::Hosted(position as u8))?;
                        }
                    }
                }
                HostedWrite::Set(roots) => {
                    for (position, root) in roots.iter().enumerate() {
                        let via = Via::Hosted(position as u8);
                        let kept = old.and_then(|old| old.child(via));
                        let kid = match *root {
                            Some(root) => self.target(root, kept)?,
                            None => None,
                        };
                        if let Some(kid) = kid {
                            self.adopt(cell, kid, via)?;
                        }
                    }
                }
            }
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
        Ok((order, lens, takes, let_go))
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
        for &cell in order {
            let node = self.plan.cells[cell]
                .node
                .expect("the order lists nodes of the write");
            self.held.push(cell);
            if self.plan.cells[cell].page != NOWHERE {
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

    fn page(&mut self, id: PageId) -> usize {
        let pages = &mut self.plan.pages;
        *self.plan.page_of.entry(id).or_insert_with(|| {
            pages.push(PagePlan::of(Some(id), pages.len()));
            pages.len() - 1
        })
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
            view.node(address.slot()).map(|node| Stored {
                len: node.cell().encoded_len(),
                links: Side::BOTH.map(|side| node.link(side).map(|link| link.child)),
                hosted: node.hosted_roots().collect(),
                value_len: node.value().stored_len(),
                same_key: key.is_none_or(|key| key == node.key()),
            })
        })?;
        stored.ok_or_else(|| WriteError::NoNode { address }.into())
    }

    fn cell(&mut self, origin: Option<CellAddress>) -> usize {
        let home = origin.map_or(NOWHERE, |address| self.page(address.page()));
        let cell = self.plan.cells.len();
        self.plan.cells.push(Cell {
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
            kids: Vec::new(),
            cause: None,
        });
        if let Some(address) = origin {
            self.plan.known.insert(address, cell);
        }
        cell
    }

    /// The cell at `address`, which a link or pointer names.
    fn existing(&mut self, address: CellAddress) -> Result<usize, FragmentError> {
        if self.plan.removed.contains(&address) {
            return Err(WriteError::Removed { address }.into());
        }
        Ok(match self.plan.known.get(&address) {
            Some(cell) => *cell,
            None => self.cell(Some(address)),
        })
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
        let cell = &mut self.plan.cells[kid];
        if kid == parent || cell.parent.is_some() || cell.free {
            let address = cell.origin;
            return Err(WriteError::TwoPointers { address }.into());
        }
        cell.parent = Some(parent);
        cell.via = Some(via);
        self.plan.cells[parent].kids.push(Kid { cell: kid, via });
        Ok(())
    }

    /// Reads a cell the write does not write: its length and what it points
    /// at.
    fn read(&mut self, cell: usize) -> Result<(), FragmentError> {
        if self.plan.cells[cell].read {
            return Ok(());
        }
        let address = self.plan.cells[cell]
            .origin
            .expect("a node the write creates is known");
        let stored = self.stored(address, None)?;
        let entry = &mut self.plan.cells[cell];
        entry.len = stored.len;
        entry.read = true;
        for (side, child) in Side::BOTH.into_iter().zip(stored.links) {
            if let Some(child) = child {
                let kid = self.existing(child)?;
                self.adopt(cell, kid, Via::Link(side))?;
            }
        }
        for (position, root) in stored.hosted.iter().enumerate() {
            if let Some(root) = *root {
                let kid = self.existing(root)?;
                self.adopt(cell, kid, Via::Hosted(position as u8))?;
            }
        }
        Ok(())
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

    /// Carries one cell to another page.
    fn carry(&mut self, cell: usize, to: usize, cause: Cause) -> Result<(), FragmentError> {
        self.read(cell)?;
        self.leave(cell)?;
        self.arrive(cell, to)?;
        self.plan.cells[cell].cause = Some(cause);
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

    fn kids_in(&self, cell: usize, page: usize) -> Vec<usize> {
        let kids = self.plan.cells[cell].kids.iter().map(|kid| kid.cell);
        kids.filter(|kid| self.plan.cells[*kid].page == page)
            .collect()
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
    fn fragment_bytes(&mut self, root: usize, limit: usize) -> Result<usize, FragmentError> {
        let page = self.plan.cells[root].page;
        let mut bytes = 0;
        let mut pending = vec![root];
        while let Some(cell) = pending.pop() {
            self.read(cell)?;
            bytes += self.plan.cells[cell].len + SLOT;
            if bytes >= limit {
                break;
            }
            pending.extend(self.kids_in(cell, page));
        }
        Ok(bytes)
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
    fn cap(&mut self) -> Result<(), FragmentError> {
        let largest = self.layout.largest_fragment();
        if largest >= self.layout.capacity() {
            return Ok(());
        }
        let mut within = Vec::new();
        let mut next = 0;
        while let Some(&cell) = self.held.get(next) {
            next += 1;
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
        let below = self.kids_in(cell, page);
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
