//! The page model: the rules of `docs/lignindb/tree-layout.md`, applied to
//! the change sets the observer reports.
//!
//! The model keeps one number per node, the page it is in, and one cell
//! size. It never looks at a key or a value. Everything it does to a page is
//! something an engine could do from the nodes one write holds: it starts
//! from a node the write changed, walks inside that node's page, and moves
//! only what it reached that way.

use super::{
    report::{Cause, Sample, Stats},
    tree::{ChangeSet, Grove, MerkId, NodeId},
};

/// The bytes a cell and a page spend besides keys, values and Merk's own
/// aggregate encoding. FMT-02 fixes the real layout; these are the sizes the
/// simulation assumes for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellCosts {
    /// The page header.
    pub page_header: u32,
    /// One entry of the slot directory.
    pub slot: u32,
    /// The fixed part of a cell: flags, key length, value length.
    pub cell_header: u32,
    /// A pointer to a child or to the root of a hosted Merk: a page id and a
    /// slot.
    pub pointer: u32,
    /// The pointer a cell stores in place of an out-of-line value.
    pub value_pointer: u32,
    /// The header of a value-log record.
    pub value_record_header: u32,
}

impl Default for CellCosts {
    fn default() -> Self {
        CellCosts {
            page_header: 32,
            slot: 2,
            cell_header: 4,
            pointer: 6,
            value_pointer: 16,
            value_record_header: 18,
        }
    }
}

/// A hash as Merk stores it.
const HASH: u32 = 32;
/// The two child heights a link stores.
const LINK_HEIGHTS: u32 = 2;

/// The parameters of one simulated layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The page size in bytes.
    pub page_size: u32,
    /// The longest value that is stored in its cell. A longer one goes to
    /// the value log and leaves a pointer.
    pub inline_max: u32,
    /// A fragment smaller than the page size divided by this belongs in its
    /// parent's page.
    pub small_fragment_divisor: u32,
    /// A fragment larger than this many percent of a page's capacity is
    /// split where it is: its root goes up and its halves stay. With 100 a
    /// fragment may fill a page.
    pub largest_fragment_percent: u32,
    /// The same limit for an inner fragment: one with a link to a cell of
    /// another page. Never below the limit for the others.
    pub largest_inner_fragment_percent: u32,
    /// A page takes a fragment from another page only if that leaves it at
    /// most this many percent full. With 0 such a fragment always starts a
    /// fresh page.
    pub seed_limit_percent: u32,
    /// When a delete leaves a page less than this many percent full, the
    /// fragment it was deleted from moves to a page beside it that has room.
    pub merge_below_percent: u32,
    /// Whether the root of a Merk is placed with the element that hosts it.
    /// Without it every Merk is laid out on pages of its own.
    pub colocate: bool,
    /// Whether a node that a rotation lifts over a node of another page is
    /// carried to that page. Without it a rotation only changes links.
    pub carry_on_rotation: bool,
    /// Whether a node that takes the place of a deleted node of another
    /// page is carried to that page.
    pub carry_on_delete: bool,
    /// The byte costs of the assumed cell layout.
    pub costs: CellCosts,
}

impl Config {
    /// The page size `docs/lignindb/tree-layout.md` proposes.
    pub const PROPOSED_PAGE_SIZE: u32 = 32_768;

    /// The layout the specification proposes.
    pub fn proposed() -> Self {
        Self::with_page_size(Self::PROPOSED_PAGE_SIZE)
    }

    /// The layout with pages of `page_size` bytes and every other parameter
    /// at its proposed value.
    pub fn with_page_size(page_size: u32) -> Self {
        Config {
            page_size,
            inline_max: 256,
            small_fragment_divisor: 16,
            largest_fragment_percent: 50,
            largest_inner_fragment_percent: 50,
            seed_limit_percent: 75,
            merge_below_percent: 35,
            colocate: true,
            carry_on_rotation: false,
            carry_on_delete: true,
            costs: CellCosts::default(),
        }
    }

    /// The bytes of a page that cells can use.
    pub fn capacity(&self) -> u32 {
        self.page_size - self.costs.page_header
    }

    /// The size below which a fragment belongs in its parent's page.
    pub fn small_fragment(&self) -> u32 {
        self.page_size / self.small_fragment_divisor
    }

    /// The size above which a fragment is split where it is.
    pub fn largest_fragment(&self) -> u32 {
        (u64::from(self.capacity()) * u64::from(self.largest_fragment_percent) / 100) as u32
    }

    /// The size above which an inner fragment is split where it is.
    pub fn largest_inner_fragment(&self) -> u32 {
        (u64::from(self.capacity()) * u64::from(self.largest_inner_fragment_percent) / 100) as u32
    }

    /// The most bytes a page may hold after it took a fragment from another
    /// page.
    pub fn seed_limit(&self) -> u32 {
        (u64::from(self.capacity()) * u64::from(self.seed_limit_percent) / 100) as u32
    }

    /// The bytes below which a page that a delete shrank gives its fragment
    /// away.
    pub fn merge_below(&self) -> u32 {
        (u64::from(self.capacity()) * u64::from(self.merge_below_percent) / 100) as u32
    }

    /// Whether a value of `value_len` bytes is stored outside its cell.
    pub fn is_out_of_line(&self, value_len: u32) -> bool {
        value_len > self.inline_max
    }

    /// The bytes the cell of `node` takes in its page, slot entry included.
    ///
    /// A cell holds the key, the feature type with its aggregate data, the
    /// value hash and the key-value hash, for each child a pointer with the
    /// child's hash, heights and aggregate data, a pointer to the root of
    /// each Merk the element hosts, and the value or a pointer to it.
    pub fn cell_bytes(&self, grove: &Grove, node: NodeId) -> u32 {
        let entry = grove.node(node);
        let costs = &self.costs;
        let value = if self.is_out_of_line(entry.value_len) {
            costs.value_pointer
        } else {
            entry.value_len
        };
        costs.slot
            + costs.cell_header
            + entry.key.len() as u32
            + 2 * HASH
            + entry.meta_len
            + grove.child_links(node) * (HASH + LINK_HEIGHTS + costs.pointer)
            + grove.hosted_roots(node) * costs.pointer
            + value
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Page {
    used: u32,
    cells: u32,
    dirty_in_transition: u64,
    dirty_in_block: u64,
    /// A cell of this page that the current write holds.
    held: Option<NodeId>,
    queued: bool,
}

/// One layout of a grove on pages.
pub struct Model {
    config: Config,
    capacity: u32,
    small: u32,
    largest: u32,
    largest_inner: u32,
    seed_limit: u32,
    merge_below: u32,
    /// The pages the current write deleted a cell from.
    shrunk: Vec<u32>,
    page_of: Vec<u32>,
    cell_of: Vec<u32>,
    pages: Vec<Page>,
    free: Vec<u32>,
    live_pages: u64,
    used_bytes: u64,
    cells: u64,
    transition: u64,
    block: u64,
    queue: Vec<u32>,
    /// The nodes the current write holds: the ones it changed and the roots
    /// of what it moved.
    held: Vec<NodeId>,
    directory: Vec<MerkId>,
    dirty_now: u32,
    dirty_block: u64,
    stats: Stats,
    samples: Vec<Sample>,
}

impl Model {
    /// An empty layout.
    pub fn new(config: Config) -> Self {
        assert!(
            config.page_size >= 1024 && config.page_size <= 65_536,
            "a page is 1 KiB to 64 KiB"
        );
        assert!(
            config.small_fragment_divisor >= 2,
            "a small fragment is at most half a page"
        );
        assert!(
            (10..=100).contains(&config.largest_fragment_percent),
            "a fragment is at most a page"
        );
        assert!(
            (config.largest_fragment_percent..=100)
                .contains(&config.largest_inner_fragment_percent),
            "an inner fragment may be at least as large as any other, and at most a page"
        );
        assert!(
            config.small_fragment() * 3 <= config.largest_fragment(),
            "half of a split fragment must not count as small"
        );
        assert!(config.seed_limit_percent <= 100, "a page is at most full");
        assert!(
            config.merge_below_percent <= config.seed_limit_percent,
            "a page that gives its fragment away must be able to take one"
        );
        Model {
            config,
            capacity: config.capacity(),
            small: config.small_fragment(),
            largest: config.largest_fragment(),
            largest_inner: config.largest_inner_fragment(),
            seed_limit: config.seed_limit(),
            merge_below: config.merge_below(),
            shrunk: Vec::new(),
            page_of: Vec::new(),
            cell_of: Vec::new(),
            pages: vec![Page::default()],
            free: Vec::new(),
            live_pages: 0,
            used_bytes: 0,
            cells: 0,
            transition: 0,
            block: 1,
            queue: Vec::new(),
            held: Vec::new(),
            directory: Vec::new(),
            dirty_now: 0,
            dirty_block: 0,
            stats: Stats::default(),
            samples: Vec::new(),
        }
    }

    /// The parameters of this layout.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The counters so far.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// The samples taken so far.
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// How many pages hold at least one cell.
    pub fn pages(&self) -> u64 {
        self.live_pages
    }

    /// How many cells the pages hold.
    pub fn cells(&self) -> u64 {
        self.cells
    }

    /// The bytes of all cells, slot entries included.
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// The share of all page bytes that holds a header or a cell.
    pub fn fill(&self) -> f64 {
        if self.live_pages == 0 {
            return 0.0;
        }
        let header = u64::from(self.config.costs.page_header);
        (self.used_bytes + header * self.live_pages) as f64
            / (self.live_pages * u64::from(self.config.page_size)) as f64
    }

    /// The page `node` is in, if it is placed.
    pub fn page_of(&self, node: NodeId) -> Option<u32> {
        match self.page_of.get(node.index()) {
            Some(0) | None => None,
            Some(page) => Some(*page),
        }
    }

    /// How many pages hold their share of cell bytes, in tenths of a page:
    /// entry 0 counts the pages up to 10% full, entry 9 those above 90%.
    pub fn fill_histogram(&self) -> [u64; 10] {
        let mut histogram = [0; 10];
        let header = self.config.costs.page_header;
        for page in self.pages.iter().filter(|page| page.cells > 0) {
            let tenth = u64::from(page.used + header) * 10 / u64::from(self.config.page_size);
            histogram[(tenth as usize).min(9)] += 1;
        }
        histogram
    }

    /// How many distinct pages hold the nodes from `node` up to the root of
    /// the grove: the pages a hash update of `node` dirties.
    pub fn pages_to_root(&self, grove: &Grove, node: NodeId) -> u32 {
        let mut pages = Vec::new();
        let mut at = Some(node);
        while let Some(id) = at {
            let page = self.page_of[id.index()];
            if !pages.contains(&page) {
                pages.push(page);
            }
            at = grove.grove_parent(id);
        }
        pages.len() as u32
    }

    /// Applies one state transition.
    pub fn apply(&mut self, grove: &Grove, changes: &ChangeSet) {
        self.transition += 1;
        self.dirty_now = 0;
        self.held.clear();
        self.shrunk.clear();
        self.directory.clear();
        self.page_of.resize(grove.node_slots(), 0);
        self.cell_of.resize(grove.node_slots(), 0);

        let inherited: Vec<(NodeId, u32, Cause)> = changes
            .takes_place_of
            .iter()
            .map(|(taker, left)| {
                let cause = if grove.node(*left).live {
                    Cause::Rotation
                } else {
                    Cause::Delete
                };
                (*taker, self.page_of[left.index()], cause)
            })
            .filter(|(_, _, cause)| match cause {
                Cause::Rotation => self.config.carry_on_rotation,
                _ => self.config.carry_on_delete,
            })
            .collect();
        for node in &changes.removed {
            let page = self.page_of[node.index()];
            if page != 0 && !self.shrunk.contains(&page) {
                self.shrunk.push(page);
            }
            self.remove_cell(*node, true);
        }
        for node in &changes.dropped {
            self.remove_cell(*node, false);
        }
        let mut reserved: Vec<(NodeId, u32)> = Vec::new();
        for (taker, page, cause) in inherited {
            if page == 0 || self.pages[page as usize].cells == 0 {
                continue;
            }
            if self.page_of[taker.index()] == 0 {
                reserved.push((taker, page));
            } else if self.page_of[taker.index()] != page {
                self.move_with_small_hosted(grove, taker, page, cause);
                self.stats.inherited += 1;
            }
        }

        for event in &changes.events {
            let node = event.node;
            let bytes = self.config.cell_bytes(grove, node);
            self.held.push(node);
            if self.page_of[node.index()] == 0 {
                let page = reserved
                    .iter()
                    .find(|(taker, _)| *taker == node)
                    .map(|(_, page)| *page)
                    .or_else(|| {
                        self.parent(grove, node)
                            .map(|parent| self.page_of[parent.index()])
                    })
                    .filter(|page| *page != 0)
                    .or_else(|| {
                        self.children(grove, node)
                            .map(|child| self.page_of[child.index()])
                            .find(|page| *page != 0)
                    })
                    .unwrap_or_else(|| self.allocate());
                self.insert_cell(node, page, bytes);
            } else {
                self.resize_cell(node, bytes);
            }
        }

        let carry_on_rotation = self.config.carry_on_rotation;
        let sandwiches = changes
            .events
            .iter()
            .filter(|event| !event.created && carry_on_rotation);
        for event in sandwiches {
            let node = event.node;
            let page = self.page_of[node.index()];
            let Some(parent) = self.parent(grove, node) else {
                continue;
            };
            let above = self.page_of[parent.index()];
            if above != page
                && self
                    .children(grove, node)
                    .any(|child| self.page_of[child.index()] == above)
            {
                self.move_with_small_hosted(grove, node, above, Cause::Rotation);
                self.stats.sandwiched += 1;
            }
        }

        self.cap_fragments(grove);
        self.settle_overflow(grove);
        self.absorb_small(grove);
        self.merge_underfull(grove);
        self.settle_overflow(grove);

        for merk in &changes.roots_changed {
            self.directory.push(*merk);
        }
        self.directory.extend(&changes.merks_created);
        self.directory.sort_unstable();
        self.directory.dedup();
        let directory = self.directory.len() as u64 + u64::from(changes.merks_dropped);

        let stats = &mut self.stats;
        stats.transitions += 1;
        stats.dirty_pages += u64::from(self.dirty_now);
        stats.directory_writes += directory;
        let total = (u64::from(self.dirty_now) + directory) as usize;
        stats.dirty_histogram[total.min(stats.dirty_histogram.len() - 1)] += 1;
        stats.max_dirty = stats.max_dirty.max(total as u64);
        stats.records += changes.events.len() as u64;
        stats.record_bytes += changes.record_bytes;
        stats.hash_only_records += u64::from(changes.hash_only);
        stats.value_records += changes
            .events
            .iter()
            .filter(|event| event.value_changed)
            .count() as u64;
        stats.created += changes.events.iter().filter(|event| event.created).count() as u64;
        for event in changes.events.iter().filter(|event| event.value_changed) {
            let value_len = grove.node(event.node).value_len;
            if self.config.is_out_of_line(value_len) {
                stats.out_of_line_bytes +=
                    u64::from(value_len + self.config.costs.value_record_header);
            }
        }
        stats.removed += changes.removed.len() as u64;
        stats.dropped += changes.dropped.len() as u64;
    }

    /// Ends a block: the pages dirtied since the last block are flushed
    /// once.
    pub fn end_block(&mut self) {
        self.stats.blocks += 1;
        self.stats.dirty_pages_by_block += self.dirty_block;
        self.dirty_block = 0;
        self.block += 1;
    }

    /// Records where the layout stands.
    pub fn sample(&mut self) {
        self.samples.push(Sample {
            stats: self.stats.clone(),
            cells: self.cells,
            pages: self.live_pages,
            used_bytes: self.used_bytes,
            fill: self.fill(),
        });
    }

    /// Checks the model's bookkeeping against a recount from the grove:
    /// every live node is in exactly one page with the cell size its record
    /// gives, every page's totals add up, and no page is over its capacity.
    pub fn check(&self, grove: &Grove) -> Result<(), String> {
        let mut used = vec![0u64; self.pages.len()];
        let mut cells = vec![0u32; self.pages.len()];
        let mut live = 0u64;
        for (node, entry) in grove.nodes() {
            let index = node.index();
            let page = self.page_of.get(index).copied().unwrap_or(0);
            if !entry.live {
                if page != 0 {
                    return Err(format!("node {index} is gone but still in page {page}"));
                }
                continue;
            }
            live += 1;
            if page == 0 {
                return Err(format!("node {index} is in no page"));
            }
            let bytes = self.config.cell_bytes(grove, node);
            if bytes != self.cell_of[index] {
                return Err(format!(
                    "node {index} has a cell of {} bytes, its record needs {bytes}",
                    self.cell_of[index]
                ));
            }
            used[page as usize] += u64::from(bytes);
            cells[page as usize] += 1;
        }
        if live != self.cells {
            return Err(format!("{live} live nodes but {} cells", self.cells));
        }
        let mut pages = 0;
        for (id, page) in self.pages.iter().enumerate().skip(1) {
            if u64::from(page.used) != used[id] || page.cells != cells[id] {
                return Err(format!(
                    "page {id} claims {} bytes in {} cells but holds {} in {}",
                    page.used, page.cells, used[id], cells[id]
                ));
            }
            if page.used > self.capacity {
                return Err(format!(
                    "page {id} holds {} bytes, above its capacity of {}",
                    page.used, self.capacity
                ));
            }
            pages += u64::from(page.cells > 0);
        }
        if pages != self.live_pages {
            return Err(format!(
                "{pages} pages in use but {} counted",
                self.live_pages
            ));
        }
        Ok(())
    }

    fn parent(&self, grove: &Grove, node: NodeId) -> Option<NodeId> {
        if self.config.colocate {
            grove.grove_parent(node)
        } else {
            grove.node(node).parent
        }
    }

    fn children<'g>(&self, grove: &'g Grove, node: NodeId) -> impl Iterator<Item = NodeId> + 'g {
        let entry = grove.node(node);
        let hosted: &[MerkId] = if self.config.colocate {
            &entry.hosted
        } else {
            &[]
        };
        entry
            .left
            .into_iter()
            .chain(entry.right)
            .chain(hosted.iter().filter_map(|merk| grove.merk(*merk).root))
    }

    fn dirty(&mut self, page: u32) {
        let entry = &mut self.pages[page as usize];
        if entry.dirty_in_transition != self.transition {
            entry.dirty_in_transition = self.transition;
            self.dirty_now += 1;
        }
        if entry.dirty_in_block != self.block {
            entry.dirty_in_block = self.block;
            self.dirty_block += 1;
        }
    }

    fn allocate(&mut self) -> u32 {
        self.live_pages += 1;
        self.stats.pages_allocated += 1;
        match self.free.pop() {
            Some(page) => page,
            None => {
                self.pages.push(Page::default());
                (self.pages.len() - 1) as u32
            }
        }
    }

    fn insert_cell(&mut self, node: NodeId, page: u32, bytes: u32) {
        self.page_of[node.index()] = page;
        self.cell_of[node.index()] = bytes;
        self.used_bytes += u64::from(bytes);
        self.cells += 1;
        let entry = &mut self.pages[page as usize];
        entry.used += bytes;
        entry.cells += 1;
        entry.held = Some(node);
        self.dirty(page);
        self.queue_if_over(page);
    }

    fn resize_cell(&mut self, node: NodeId, bytes: u32) {
        let page = self.page_of[node.index()];
        let old = std::mem::replace(&mut self.cell_of[node.index()], bytes);
        self.used_bytes = self.used_bytes - u64::from(old) + u64::from(bytes);
        let entry = &mut self.pages[page as usize];
        entry.used = entry.used - old + bytes;
        entry.held = Some(node);
        self.dirty(page);
        self.queue_if_over(page);
    }

    fn remove_cell(&mut self, node: NodeId, dirties: bool) {
        let page = std::mem::take(&mut self.page_of[node.index()]);
        if page == 0 {
            return;
        }
        let bytes = std::mem::take(&mut self.cell_of[node.index()]);
        self.used_bytes -= u64::from(bytes);
        self.cells -= 1;
        let entry = &mut self.pages[page as usize];
        entry.used -= bytes;
        entry.cells -= 1;
        if entry.held == Some(node) {
            entry.held = None;
        }
        if dirties {
            self.dirty(page);
        }
        if self.pages[page as usize].cells == 0 {
            self.pages[page as usize].held = None;
            self.free.push(page);
            self.live_pages -= 1;
            self.stats.pages_freed += 1;
        }
    }

    fn queue_if_over(&mut self, page: u32) {
        let entry = &mut self.pages[page as usize];
        if entry.used > self.capacity && !entry.queued {
            entry.queued = true;
            self.queue.push(page);
        }
    }

    /// Carries one cell to another page. The page it leaves, the page it
    /// enters and the page of the cell that points at it are written.
    fn carry(&mut self, grove: &Grove, node: NodeId, to: u32, cause: Cause) {
        let bytes = self.cell_of[node.index()];
        self.remove_cell(node, true);
        self.insert_cell(node, to, bytes);
        let moves = &mut self.stats.moves[cause as usize];
        moves.cells += 1;
        moves.cell_bytes += u64::from(bytes);
        let entry = grove.node(node);
        if self.config.is_out_of_line(entry.value_len) {
            moves.values += 1;
            moves.value_bytes += u64::from(entry.value_len + self.config.costs.value_record_header);
        }
        if grove.is_merk_root(node) {
            self.directory.push(entry.merk);
        }
    }

    fn relink(&mut self, grove: &Grove, node: NodeId) {
        if let Some(parent) = self.parent(grove, node) {
            let page = self.page_of[parent.index()];
            if page != 0 {
                self.dirty(page);
            }
        } else if let Some(host) = grove.grove_parent(node) {
            let page = self.page_of[host.index()];
            if page != 0 {
                self.dirty(page);
            }
        }
    }

    /// The nodes of the fragment under `root`: `root` and everything below
    /// it that is reached without leaving its page.
    fn fragment(&self, grove: &Grove, root: NodeId) -> Vec<NodeId> {
        let page = self.page_of[root.index()];
        let mut nodes = vec![root];
        let mut next = 0;
        while next < nodes.len() {
            let node = nodes[next];
            next += 1;
            nodes.extend(
                self.children(grove, node)
                    .filter(|child| self.page_of[child.index()] == page),
            );
        }
        nodes
    }

    /// The bytes of the fragment under `root`, counted up to `limit`.
    fn fragment_bytes(&self, grove: &Grove, root: NodeId, limit: u32) -> u32 {
        let page = self.page_of[root.index()];
        let mut bytes = 0u32;
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            bytes += self.cell_of[node.index()];
            if bytes >= limit {
                break;
            }
            pending.extend(
                self.children(grove, node)
                    .filter(|child| self.page_of[child.index()] == page),
            );
        }
        bytes
    }

    /// The root of the fragment `node` is in.
    fn fragment_root(&self, grove: &Grove, mut node: NodeId) -> NodeId {
        let page = self.page_of[node.index()];
        while let Some(parent) = self.parent(grove, node) {
            if self.page_of[parent.index()] != page {
                break;
            }
            node = parent;
        }
        node
    }

    /// Moves the fragment under `root` to the page `to`.
    fn move_fragment(&mut self, grove: &Grove, root: NodeId, to: u32, cause: Cause) {
        if self.page_of[root.index()] == to {
            return;
        }
        for node in self.fragment(grove, root) {
            self.carry(grove, node, to, cause);
        }
        self.relink(grove, root);
        self.pages[to as usize].held = Some(root);
        self.held.push(root);
    }

    /// Moves one node to the page `to`. A small Merk it hosts in its own
    /// page goes with it.
    fn move_with_small_hosted(&mut self, grove: &Grove, node: NodeId, to: u32, cause: Cause) {
        let page = self.page_of[node.index()];
        if page == to {
            return;
        }
        if self.config.colocate {
            let hosted: Vec<NodeId> = grove
                .node(node)
                .hosted
                .iter()
                .filter_map(|merk| grove.merk(*merk).root)
                .filter(|root| self.page_of[root.index()] == page)
                .collect();
            for root in hosted {
                if self.fragment_bytes(grove, root, self.small) < self.small {
                    for carried in self.fragment(grove, root) {
                        self.carry(grove, carried, to, cause);
                    }
                }
            }
        }
        self.carry(grove, node, to, cause);
        self.relink(grove, node);
        self.held.push(node);
    }

    fn settle_overflow(&mut self, grove: &Grove) {
        let mut rounds = 0u32;
        while let Some(page) = self.queue.pop() {
            self.pages[page as usize].queued = false;
            if self.pages[page as usize].used <= self.capacity {
                continue;
            }
            rounds += 1;
            assert!(
                rounds < 100_000,
                "page {page} cannot be brought under its capacity"
            );
            let held = self.pages[page as usize].held.or_else(|| {
                self.held
                    .iter()
                    .rev()
                    .copied()
                    .find(|node| self.page_of[node.index()] == page)
            });
            let Some(held) = held else {
                self.stats.overfull_left += 1;
                continue;
            };
            let root = self.fragment_root(grove, held);
            let bytes = self.fragment_bytes(grove, root, u32::MAX);
            if u64::from(bytes) * 2 >= u64::from(self.capacity) {
                self.split(grove, page, root);
            } else {
                self.move_out(grove, root, bytes);
            }
            self.queue_if_over(page);
        }
    }

    /// A fragment that is not the bulk of an overfull page leaves it: to
    /// its parent's page when it is small, otherwise to a page beside it
    /// that has room, or to a fresh page.
    fn move_out(&mut self, grove: &Grove, root: NodeId, bytes: u32) {
        let above = self
            .parent(grove, root)
            .map(|parent| self.page_of[parent.index()])
            .filter(|page| *page != 0);
        match above {
            Some(above) if bytes < self.small => {
                self.stats.crowded_up += 1;
                self.move_fragment(grove, root, above, Cause::Merge);
            }
            _ => {
                self.stats.crowded_out += 1;
                let to = self.destination(grove, root, bytes);
                self.move_fragment(grove, root, to, Cause::Split);
            }
        }
    }

    /// Where a fragment of `bytes` bytes that leaves its page goes: a page
    /// beside it that has room, or a fresh page.
    fn destination(&mut self, grove: &Grove, root: NodeId, bytes: u32) -> u32 {
        match self.beside(grove, root, bytes) {
            Some(page) => {
                self.stats.packed += 1;
                page
            }
            None => self.allocate(),
        }
    }

    /// The fullest page that holds another child of the parent page of the
    /// fragment under `root` and stays within the seed limit when it takes
    /// `bytes` more.
    fn beside(&self, grove: &Grove, root: NodeId, bytes: u32) -> Option<u32> {
        let from = self.page_of[root.index()];
        let mut best: Option<(u32, u32)> = None;
        if self.seed_limit >= bytes
            && let Some(parent) = self.parent(grove, root)
        {
            let above = self.page_of[parent.index()];
            let top = self.fragment_root(grove, parent);
            for node in self.fragment(grove, top) {
                for child in self.children(grove, node) {
                    let page = self.page_of[child.index()];
                    if page == above || page == from || page == 0 {
                        continue;
                    }
                    let used = self.pages[page as usize].used;
                    if used + bytes > self.seed_limit {
                        continue;
                    }
                    let candidate = (used, u32::MAX - page);
                    if best.is_none_or(|best| candidate > best) {
                        best = Some(candidate);
                    }
                }
            }
        }
        best.map(|(_, page)| u32::MAX - page)
    }

    /// A page that a delete left nearly empty gives the fragment the delete
    /// was in to a page beside it.
    fn merge_underfull(&mut self, grove: &Grove) {
        for index in 0..self.shrunk.len() {
            let page = self.shrunk[index];
            let entry = self.pages[page as usize];
            if entry.cells == 0 || entry.used >= self.merge_below {
                continue;
            }
            let mut roots: Vec<NodeId> = Vec::new();
            for held in 0..self.held.len() {
                let node = self.held[held];
                if self.page_of[node.index()] == page {
                    let root = self.fragment_root(grove, node);
                    if !roots.contains(&root) {
                        roots.push(root);
                    }
                }
            }
            for root in roots {
                let bytes = self.fragment_bytes(grove, root, u32::MAX);
                if let Some(to) = self.beside(grove, root, bytes) {
                    self.stats.merged += 1;
                    self.move_fragment(grove, root, to, Cause::Merge);
                }
            }
        }
    }

    /// Splits where it is every fragment the write holds that is above the
    /// largest size: its root goes up to the page of its parent, and what
    /// hung below it stays as fragments of its own.
    fn cap_fragments(&mut self, grove: &Grove) {
        if self.largest >= self.capacity {
            return;
        }
        let mut next = 0;
        while next < self.held.len() {
            let node = self.held[next];
            next += 1;
            let page = self.page_of[node.index()];
            if page == 0 {
                continue;
            }
            let root = self.fragment_root(grove, node);
            let Some(parent) = self.parent(grove, root) else {
                continue;
            };
            if self.fragment_bytes(grove, root, self.largest + 1) <= self.largest {
                continue;
            }
            if self.largest_inner > self.largest {
                let root_page = self.page_of[root.index()];
                let mut bytes = 0u32;
                let mut inner = false;
                for node in self.fragment(grove, root) {
                    bytes += self.cell_of[node.index()];
                    inner |= self
                        .children(grove, node)
                        .any(|child| self.page_of[child.index()] != root_page);
                }
                if inner && bytes <= self.largest_inner {
                    continue;
                }
            }
            let above = self.page_of[parent.index()];
            self.stats.capped += 1;
            self.stats.promoted += 1;
            self.move_with_small_hosted(grove, root, above, Cause::Split);
        }
    }

    /// Splits the fragment under `root`, which is the bulk of the overfull
    /// page `page`: its root goes up to the page of its parent, and about
    /// half of what is left goes to a fresh page.
    fn split(&mut self, grove: &Grove, page: u32, root: NodeId) {
        self.stats.splits += 1;
        let above = self
            .parent(grove, root)
            .map(|parent| self.page_of[parent.index()])
            .filter(|above| *above != 0 && *above != page);
        let mut parts: Vec<(NodeId, u32)> = Vec::new();
        self.peel(grove, page, root, above, &mut parts);
        loop {
            let total: u64 = parts.iter().map(|part| u64::from(part.1)).sum();
            let Some((index, (largest, bytes))) = parts
                .iter()
                .copied()
                .enumerate()
                .max_by_key(|(index, part)| (part.1, std::cmp::Reverse(*index)))
            else {
                break;
            };
            let lopsided = parts.len() == 1 || u64::from(bytes) * 4 > total * 3;
            if !lopsided || u64::from(bytes) * 2 < u64::from(self.capacity) {
                break;
            }
            parts.remove(index);
            self.peel(grove, page, largest, above, &mut parts);
        }
        let used = self.pages[page as usize].used;
        if parts.is_empty() || used * 2 <= self.capacity {
            return;
        }
        let target = used - self.capacity / 2;
        let chosen = choose(&parts, target);
        let leaving: Vec<(NodeId, u32)> = parts
            .iter()
            .enumerate()
            .filter(|(index, _)| chosen & (1 << index) != 0)
            .map(|(_, part)| *part)
            .collect();
        let bytes: u32 = leaving.iter().map(|part| part.1).sum();
        let to = self.destination(grove, leaving[0].0, bytes);
        for (part, _) in leaving {
            self.stats.split_off += 1;
            self.move_fragment(grove, part, to, Cause::Split);
        }
    }

    /// Takes `node` off the top of a fragment. It moves to `above`, the
    /// page of its parent, or stays where it is when its parent is in this
    /// page or it has none. What hangs below it in this page becomes parts.
    fn peel(
        &mut self,
        grove: &Grove,
        page: u32,
        node: NodeId,
        above: Option<u32>,
        parts: &mut Vec<(NodeId, u32)>,
    ) {
        let below: Vec<NodeId> = self
            .children(grove, node)
            .filter(|child| self.page_of[child.index()] == page)
            .collect();
        if let Some(above) = above {
            self.move_with_small_hosted(grove, node, above, Cause::Split);
            self.stats.promoted += 1;
        }
        for child in below {
            if self.page_of[child.index()] == page {
                parts.push((child, self.fragment_bytes(grove, child, u32::MAX)));
            }
        }
    }

    /// A small fragment whose parent's page has room for it moves there.
    fn absorb_small(&mut self, grove: &Grove) {
        let mut roots: Vec<NodeId> = Vec::new();
        for index in 0..self.held.len() {
            let node = self.held[index];
            if self.page_of[node.index()] == 0 {
                continue;
            }
            let root = self.fragment_root(grove, node);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        for root in roots {
            let page = self.page_of[root.index()];
            let Some(parent) = self.parent(grove, root) else {
                continue;
            };
            let above = self.page_of[parent.index()];
            if above == 0 || above == page {
                continue;
            }
            let bytes = self.fragment_bytes(grove, root, self.small);
            if bytes < self.small && self.pages[above as usize].used + bytes <= self.capacity {
                self.stats.absorbed += 1;
                self.move_fragment(grove, root, above, Cause::Merge);
            }
        }
    }
}

/// Which parts to move so that their bytes come closest to `target`: a bit
/// set over `parts`, never empty. Ties go to fewer bytes, then to the parts
/// listed first.
fn choose(parts: &[(NodeId, u32)], target: u32) -> u32 {
    const EXHAUSTIVE: usize = 12;
    if parts.len() > EXHAUSTIVE {
        let mut order: Vec<usize> = (0..parts.len().min(32)).collect();
        order.sort_by_key(|index| (std::cmp::Reverse(parts[*index].1), *index));
        let mut chosen = 0u32;
        let mut sum = 0u64;
        for index in order {
            let bytes = u64::from(parts[index].1);
            if chosen == 0
                || (sum + bytes).abs_diff(u64::from(target)) < sum.abs_diff(u64::from(target))
            {
                chosen |= 1 << index;
                sum += bytes;
            }
        }
        return chosen;
    }
    let mut best = (u64::MAX, u64::MAX, 0u32);
    for mask in 1u32..(1 << parts.len()) {
        let sum: u64 = parts
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, part)| u64::from(part.1))
            .sum();
        let candidate = (sum.abs_diff(u64::from(target)), sum, mask);
        if candidate < best {
            best = candidate;
        }
    }
    best.2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_sim::tree::{hand::Hand, Side};

    /// A small page, so a handful of nodes fills it. With a 32-byte value a
    /// leaf cell is 2 + 4 + 4 + 64 + 1 + 32 = 107 bytes and each link adds
    /// 41, so a page of 1024 bytes holds about seven nodes. A fragment may
    /// fill the page, a fragment that leaves starts a fresh page and nothing
    /// is merged, so each test meets one rule at a time.
    fn config() -> Config {
        Config {
            small_fragment_divisor: 8,
            largest_fragment_percent: 100,
            largest_inner_fragment_percent: 100,
            seed_limit_percent: 0,
            merge_below_percent: 0,
            ..Config::with_page_size(1024)
        }
    }

    fn key(index: u32) -> [u8; 4] {
        index.to_be_bytes()
    }

    /// Applies the edits made to `hand` since the last step as one
    /// transition.
    fn step(hand: &mut Hand, model: &mut Model) -> ChangeSet {
        let changes = hand.commit();
        model.apply(&hand.grove, &changes);
        changes
    }

    /// Adds `count` nodes to `merk` as a balanced tree, one per transition
    /// from the top down, applying each transition to `model`.
    fn grow(hand: &mut Hand, model: &mut Model, merk: MerkId, count: u32) -> Vec<NodeId> {
        fn order(
            low: u32,
            high: u32,
            depth: u32,
            out: &mut Vec<(u32, u32, Option<u32>, Side)>,
            parent: Option<u32>,
            side: Side,
        ) {
            if low >= high {
                return;
            }
            let middle = low + (high - low) / 2;
            out.push((depth, middle, parent, side));
            order(low, middle, depth + 1, out, Some(middle), Side::Left);
            order(middle + 1, high, depth + 1, out, Some(middle), Side::Right);
        }
        let mut plan = Vec::new();
        order(0, count, 0, &mut plan, None, Side::Left);
        plan.sort_by_key(|entry| (entry.0, entry.1));
        let mut nodes = vec![None; count as usize];
        for (_, index, parent, side) in plan {
            let node = hand.add(merk, &key(index), 32);
            match parent {
                Some(parent) => {
                    hand.link(nodes[parent as usize].expect("placed"), side, Some(node))
                }
                None => hand.set_root(merk, Some(node)),
            }
            hand.touch_path(node);
            nodes[index as usize] = Some(node);
            step(hand, model);
            model.check(&hand.grove).expect("consistent");
        }
        nodes
            .into_iter()
            .map(|node| node.expect("placed"))
            .collect()
    }

    #[test]
    fn a_cell_counts_every_part_merk_stores() {
        let mut hand = Hand::new();
        let root = hand.add(MerkId::ROOT, b"key", 32);
        hand.set_root(MerkId::ROOT, Some(root));
        let config = config();
        assert_eq!(
            config.cell_bytes(&hand.grove, root),
            2 + 4 + 3 + 64 + 1 + 32
        );

        let child = hand.add(MerkId::ROOT, b"a", 300);
        hand.link(root, Side::Left, Some(child));
        assert_eq!(
            config.cell_bytes(&hand.grove, root),
            2 + 4 + 3 + 64 + 2 + (32 + 2 + 6) + 32,
            "a link adds the child's hash, its heights and a pointer",
        );
        assert_eq!(
            config.cell_bytes(&hand.grove, child),
            2 + 4 + 1 + 64 + 1 + 16,
            "a value above the inline threshold leaves a pointer",
        );
        assert!(config.is_out_of_line(257) && !config.is_out_of_line(256));

        let hosted = hand.host(child);
        let before = config.cell_bytes(&hand.grove, child);
        let inner = hand.add(hosted, b"inner", 1);
        hand.set_root(hosted, Some(inner));
        assert_eq!(
            config.cell_bytes(&hand.grove, child),
            before + 6,
            "a hosted Merk with a root adds a pointer to it",
        );
        assert_eq!(config.capacity(), 1024 - 32);
        assert_eq!(config.small_fragment(), 128);
    }

    #[test]
    fn a_node_is_placed_with_its_parent() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 5);
        assert_eq!(model.pages(), 1);
        assert_eq!(model.cells(), 5);
        let page = model.page_of(nodes[0]);
        assert!(page.is_some() && nodes.iter().all(|node| model.page_of(*node) == page));
        assert_eq!(
            model.stats().dirty_pages,
            5,
            "each insert writes the one page"
        );
        assert_eq!(model.stats().moved().cells, 0);
        assert!(model.fill() > 0.5);
        assert_eq!(model.pages_to_root(&hand.grove, nodes[4]), 1);
    }

    #[test]
    fn a_full_page_keeps_the_top_and_sends_half_away() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 40);
        let stats = model.stats().clone();
        assert!(stats.splits > 0 && stats.split_off > 0);
        assert_eq!(stats.overfull_left, 0);
        assert!(model.pages() > 4);
        // The root of the grove has no parent to go up to, so it stays in
        // its page through every split.
        let root = hand.grove.merk(MerkId::ROOT).root.expect("root");
        assert_eq!(root, nodes[20]);
        // A leaf is at most a few pages from the root: the tree is five
        // levels deep and a page holds more than two of them.
        for node in &nodes {
            assert!(model.pages_to_root(&hand.grove, *node) <= 3);
        }
        assert!(stats.moves[Cause::Split as usize].cells > 0);
        assert_eq!(stats.moves[Cause::Rotation as usize].cells, 0);
        let histogram = model.fill_histogram();
        assert_eq!(histogram.iter().sum::<u64>(), model.pages());
    }

    #[test]
    fn a_split_sends_the_fragment_root_up_to_its_parents_page() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        grow(&mut hand, &mut model, MerkId::ROOT, 200);
        let stats = model.stats();
        assert!(
            stats.promoted > 0,
            "a fragment below the top page was split"
        );
        assert_eq!(stats.overfull_left, 0);
        model.check(&hand.grove).expect("consistent");
    }

    #[test]
    fn a_hosted_merk_starts_in_the_page_of_its_element() {
        for colocate in [true, false] {
            let mut hand = Hand::new();
            let mut model = Model::new(Config {
                colocate,
                ..config()
            });
            let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 3);
            let before = model.stats().clone();
            let hosted = hand.host(nodes[0]);
            let inner = hand.add(hosted, b"inner", 8);
            hand.set_root(hosted, Some(inner));
            hand.touch_path(inner);
            let changes = step(&mut hand, &mut model);
            assert_eq!(changes.merks_created, vec![hosted]);
            model.check(&hand.grove).expect("consistent");
            assert_eq!(
                model.page_of(inner) == model.page_of(nodes[0]),
                colocate,
                "colocate {colocate}",
            );
            assert_eq!(model.pages(), if colocate { 1 } else { 2 });
            assert_eq!(
                model.stats().since(&before).directory_writes,
                1,
                "the new Merk's directory entry",
            );
        }
    }

    /// A layout in which no fragment counts as small, so nothing moves to
    /// its parent's page for having room there.
    fn nothing_small() -> Config {
        Config {
            small_fragment_divisor: 1024,
            ..config()
        }
    }

    /// Two pages: `top` holds the root `x` and its right child; `bottom`
    /// holds `y`, the left child of `x`, with a child on each side.
    fn two_pages(model: &mut Model) -> (Hand, [NodeId; 5]) {
        let mut hand = Hand::new();
        let x = hand.add(MerkId::ROOT, &key(40), 32);
        hand.set_root(MerkId::ROOT, Some(x));
        let c = hand.add(MerkId::ROOT, &key(50), 32);
        hand.link(x, Side::Right, Some(c));
        let y = hand.add(MerkId::ROOT, &key(20), 32);
        hand.link(x, Side::Left, Some(y));
        let a = hand.add(MerkId::ROOT, &key(10), 32);
        hand.link(y, Side::Left, Some(a));
        let b = hand.add(MerkId::ROOT, &key(30), 32);
        hand.link(y, Side::Right, Some(b));
        step(&mut hand, model);
        let top = model.page_of(x).expect("placed");
        let bottom = model.allocate();
        for node in [y, a, b] {
            model.carry(&hand.grove, node, bottom, Cause::Split);
        }
        assert_ne!(top, bottom);
        model.check(&hand.grove).expect("consistent");
        (hand, [x, y, a, b, c])
    }

    /// Rotates right at `x`: its left child `y` rises and `b` changes sides.
    fn rotate_right(hand: &mut Hand, [x, y, _, b, _]: [NodeId; 5]) {
        hand.link(x, Side::Left, Some(b));
        hand.link(y, Side::Right, Some(x));
        hand.set_root(MerkId::ROOT, Some(y));
    }

    #[test]
    fn a_node_lifted_over_another_page_is_carried_there() {
        let mut model = Model::new(Config {
            carry_on_rotation: true,
            ..nothing_small()
        });
        let (mut hand, nodes) = two_pages(&mut model);
        let [x, y, a, b, c] = nodes;
        rotate_right(&mut hand, nodes);
        let changes = hand.commit();
        assert_eq!(changes.takes_place_of, vec![(y, x)]);
        model.apply(&hand.grove, &changes);
        model.check(&hand.grove).expect("consistent");
        assert_eq!(
            model.page_of(y),
            model.page_of(x),
            "the lifted node follows"
        );
        assert_eq!(model.page_of(c), model.page_of(x));
        assert_ne!(model.page_of(a), model.page_of(x), "what hung below stays");
        assert_eq!(model.page_of(a), model.page_of(b));
        let moves = model.stats().moves[Cause::Rotation as usize];
        assert_eq!((moves.cells, moves.values), (1, 0));
        assert_eq!(model.stats().inherited, 1);
        assert_eq!(model.pages_to_root(&hand.grove, c), 1);
    }

    #[test]
    fn without_the_carry_a_rotation_only_changes_links() {
        let mut model = Model::new(Config {
            carry_on_rotation: false,
            ..nothing_small()
        });
        let (mut hand, nodes) = two_pages(&mut model);
        let [x, y, _, _, c] = nodes;
        rotate_right(&mut hand, nodes);
        step(&mut hand, &mut model);
        model.check(&hand.grove).expect("consistent");
        assert_ne!(model.page_of(y), model.page_of(x));
        assert_eq!(model.stats().moves[Cause::Rotation as usize].cells, 0);
        assert_eq!(
            model.pages_to_root(&hand.grove, c),
            2,
            "the path to the right child now passes through the other page",
        );
    }

    #[test]
    fn a_node_between_two_nodes_of_another_page_joins_them() {
        let mut model = Model::new(Config {
            carry_on_rotation: true,
            ..nothing_small()
        });
        let (mut hand, [x, y, a, _, _]) = two_pages(&mut model);
        // Put `a` in the top page by hand: `y` now sits in the other page
        // between its parent and one of its children.
        let top = model.page_of(x).expect("placed");
        model.carry(&hand.grove, a, top, Cause::Split);
        hand.touch_path(y);
        step(&mut hand, &mut model);
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.page_of(y), Some(top));
        assert_eq!(model.stats().sandwiched, 1);
    }

    #[test]
    fn a_node_that_replaces_a_deleted_one_takes_its_page() {
        for carry_on_delete in [true, false] {
            let mut model = Model::new(Config {
                carry_on_delete,
                ..nothing_small()
            });
            let (mut hand, [x, y, a, b, c]) = two_pages(&mut model);
            let top = model.page_of(x);
            // Delete the root `x`: `b`, its in-order predecessor, takes its
            // place.
            hand.link(y, Side::Right, None);
            hand.link(b, Side::Left, Some(y));
            hand.link(b, Side::Right, Some(c));
            hand.set_root(MerkId::ROOT, Some(b));
            hand.remove(x);
            let changes = hand.commit();
            assert_eq!(changes.takes_place_of, vec![(b, x)]);
            assert_eq!(changes.removed, vec![x]);
            model.apply(&hand.grove, &changes);
            model.check(&hand.grove).expect("consistent");
            assert_eq!(model.page_of(b) == top, carry_on_delete);
            assert_eq!(model.page_of(a), model.page_of(y));
            assert_eq!(
                model.stats().moves[Cause::Delete as usize].cells,
                u64::from(carry_on_delete),
            );
            assert_eq!(model.stats().removed, 1);
        }
    }

    #[test]
    fn a_new_node_in_the_place_of_a_deleted_one_goes_to_its_page() {
        let mut model = Model::new(nothing_small());
        let (mut hand, [x, y, _, _, _]) = two_pages(&mut model);
        let bottom = model.page_of(y);
        // Replace `y` by a new node in one transition.
        let left = hand.grove.node(y).left;
        let right = hand.grove.node(y).right;
        let new = hand.add(MerkId::ROOT, &key(21), 32);
        hand.link(new, Side::Left, left);
        hand.link(new, Side::Right, right);
        hand.link(x, Side::Left, Some(new));
        hand.remove(y);
        step(&mut hand, &mut model);
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.page_of(new), bottom);
    }

    #[test]
    fn a_small_fragment_moves_into_its_parents_page_when_there_is_room() {
        let mut model = Model::new(config());
        let (mut hand, [x, y, a, b, _]) = two_pages(&mut model);
        // Delete `a` and `b`: what is left of the bottom page is one cell.
        hand.link(y, Side::Left, None);
        hand.link(y, Side::Right, None);
        hand.remove(a);
        hand.remove(b);
        hand.touch_path(y);
        step(&mut hand, &mut model);
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.page_of(y), model.page_of(x));
        assert_eq!(model.pages(), 1);
        assert_eq!(model.stats().absorbed, 1);
        assert_eq!(model.stats().pages_freed, 1);
        assert_eq!(model.stats().moves[Cause::Merge as usize].cells, 1);
    }

    #[test]
    fn a_dropped_merk_leaves_its_pages_without_writing_them() {
        let mut hand = Hand::new();
        let mut model = Model::new(Config {
            colocate: false,
            ..config()
        });
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 3);
        let hosted = hand.host(nodes[0]);
        hand.touch_path(nodes[0]);
        step(&mut hand, &mut model);
        let inner = grow(&mut hand, &mut model, hosted, 6);
        let pages = model.pages();
        assert!(pages >= 2);

        // Delete the leaf that hosts the Merk.
        hand.link(nodes[1], Side::Left, None);
        hand.remove(nodes[0]);
        hand.touch_path(nodes[1]);
        let before = model.stats().clone();
        let changes = step(&mut hand, &mut model);
        assert_eq!(changes.dropped.len(), 6);
        assert_eq!(changes.merks_dropped, 1);
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.page_of(inner[0]), None);
        let drop = model.stats().since(&before);
        assert_eq!(drop.dropped, 6);
        assert_eq!(
            (drop.dirty_pages, drop.directory_writes),
            (1, 1),
            "the element's page and the directory entry, not the dropped Merk's pages",
        );
        assert!(model.pages() < pages);
    }

    #[test]
    fn a_value_carried_out_of_line_is_counted_as_appended_again() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let root = hand.add(MerkId::ROOT, b"r", 1000);
        hand.set_root(MerkId::ROOT, Some(root));
        step(&mut hand, &mut model);
        assert_eq!(model.stats().out_of_line_bytes, 1018);
        let fresh = model.allocate();
        model.carry(&hand.grove, root, fresh, Cause::Split);
        let moves = model.stats().moved();
        assert_eq!((moves.cells, moves.values, moves.value_bytes), (1, 1, 1018));
        assert_eq!(model.stats().pages_freed, 1);
    }

    #[test]
    fn blocks_count_a_page_once() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        grow(&mut hand, &mut model, MerkId::ROOT, 4);
        model.end_block();
        model.sample();
        let stats = model.stats();
        assert_eq!(
            (stats.blocks, stats.dirty_pages, stats.dirty_pages_by_block),
            (1, 4, 1)
        );
        assert_eq!(stats.directory_writes, 1, "the root record, written once");
        assert_eq!(stats.dirty_per_transition(), 1.25);
        assert_eq!(stats.dirty_per_transition_by_block(), 0.25);
        assert_eq!(model.samples().len(), 1);
        assert_eq!(model.used_bytes(), u64::from(model.pages[1].used));
    }

    #[test]
    fn the_recount_reports_each_kind_of_damage() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 3);
        let index = nodes[0].index();
        let page = model.page_of[index] as usize;

        let mut damaged = |change: &dyn Fn(&mut Model), expected: &str| {
            let saved = (
                model.page_of.clone(),
                model.cell_of.clone(),
                model.pages.clone(),
                model.cells,
                model.live_pages,
            );
            change(&mut model);
            let problem = model.check(&hand.grove).expect_err(expected);
            assert!(problem.contains(expected), "{problem}");
            (
                model.page_of,
                model.cell_of,
                model.pages,
                model.cells,
                model.live_pages,
            ) = saved;
        };
        damaged(&|model| model.page_of[index] = 0, "is in no page");
        damaged(&|model| model.cell_of[index] += 1, "its record needs");
        damaged(&|model| model.cells += 1, "live nodes but");
        damaged(&|model| model.pages[page].cells += 1, "claims");
        damaged(&|model| model.live_pages += 1, "pages in use but");
        damaged(
            &|model| {
                model.capacity = 10;
            },
            "above its capacity",
        );
        model.capacity = model.config.capacity();

        hand.grove.retire_node(nodes[0]);
        let problem = model
            .check(&hand.grove)
            .expect_err("a retired node is still placed");
        assert!(problem.contains("is gone but still in page"), "{problem}");
    }

    #[test]
    fn the_parts_closest_to_the_target_are_chosen() {
        let node = {
            let mut hand = Hand::new();
            hand.add(MerkId::ROOT, b"n", 1)
        };
        let parts = |sizes: &[u32]| -> Vec<(NodeId, u32)> {
            sizes.iter().map(|size| (node, *size)).collect()
        };
        assert_eq!(
            choose(&parts(&[500, 400]), 450),
            0b10,
            "a tie goes to fewer bytes"
        );
        assert_eq!(choose(&parts(&[500, 400]), 460), 0b01);
        assert_eq!(
            choose(&parts(&[100, 200, 300]), 300),
            0b011,
            "then to the parts listed first"
        );
        assert_eq!(choose(&parts(&[100, 200, 300]), 10_000), 0b111);
        assert_eq!(choose(&parts(&[700]), 1), 0b1, "something always moves");
        let many: Vec<u32> = (1..=20).map(|size| size * 10).collect();
        let chosen = choose(&parts(&many), 450);
        let sum: u32 = many
            .iter()
            .enumerate()
            .filter(|(index, _)| chosen & (1 << index) != 0)
            .map(|(_, size)| *size)
            .sum();
        assert!(chosen != 0 && sum.abs_diff(450) <= 100, "{sum}");
    }

    #[test]
    fn a_fragment_above_the_largest_size_is_split_where_it_is() {
        let mut hand = Hand::new();
        let mut model = Model::new(Config {
            largest_fragment_percent: 50,
            ..config()
        });
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 3);
        let top = model.page_of(nodes[1]).expect("placed");
        // Give the left leaf a page of its own and grow a chain below it.
        let below = model.allocate();
        model.carry(&hand.grove, nodes[0], below, Cause::Split);
        let before = model.stats().clone();
        let mut parent = nodes[0];
        let mut chain = Vec::new();
        for index in 0..4u32 {
            let node = hand.add(MerkId::ROOT, &[0, 0, 0, 0, index as u8], 32);
            hand.link(parent, Side::Left, Some(node));
            hand.touch_path(node);
            step(&mut hand, &mut model);
            model.check(&hand.grove).expect("consistent");
            chain.push(node);
            parent = node;
        }
        let stats = model.stats().since(&before);
        assert!(stats.capped > 0, "five nodes pass half a page");
        assert_eq!(stats.splits, 0, "no page was full");
        assert_eq!(
            model.page_of(nodes[0]),
            Some(top),
            "the fragment root went up"
        );
        assert_eq!(
            model.page_of(chain[3]),
            Some(below),
            "what hung below it stayed"
        );
        assert_eq!(stats.moves[Cause::Split as usize].cells, stats.promoted);
    }

    #[test]
    fn a_fragment_that_leaves_goes_to_a_page_beside_it_with_room() {
        let run = |seed_limit_percent| {
            let mut hand = Hand::new();
            let mut model = Model::new(Config {
                seed_limit_percent,
                merge_below_percent: 0,
                ..Config::with_page_size(4096)
            });
            grow(&mut hand, &mut model, MerkId::ROOT, 1500);
            model.check(&hand.grove).expect("consistent");
            assert_eq!(model.stats().overfull_left, 0);
            (model.stats().clone(), model.pages(), model.fill())
        };
        let (fresh, fresh_pages, fresh_fill) = run(0);
        let (packed, packed_pages, packed_fill) = run(75);
        assert_eq!(fresh.packed, 0);
        assert!(packed.packed > 0);
        assert!(
            packed_pages < fresh_pages,
            "{packed_pages} pages against {fresh_pages}"
        );
        assert!(
            packed_fill > fresh_fill + 0.03,
            "{packed_fill} against {fresh_fill}"
        );
    }

    #[test]
    fn a_page_a_delete_nearly_empties_gives_its_fragment_to_a_page_beside_it() {
        for merge_below_percent in [0, 60] {
            let mut model = Model::new(Config {
                seed_limit_percent: 100,
                merge_below_percent,
                ..nothing_small()
            });
            let (mut hand, [x, y, a, b, c]) = two_pages(&mut model);
            // A third page beside the bottom one, holding the right child.
            let third = model.allocate();
            model.carry(&hand.grove, c, third, Cause::Split);
            assert_eq!(model.pages(), 3);
            // Delete `a`: the bottom page is left with two cells.
            hand.link(y, Side::Left, None);
            hand.remove(a);
            hand.touch_path(y);
            step(&mut hand, &mut model);
            model.check(&hand.grove).expect("consistent");
            let merged = merge_below_percent > 0;
            assert_eq!(model.page_of(y) == model.page_of(c), merged);
            assert_eq!(model.page_of(b), model.page_of(y));
            assert_ne!(model.page_of(y), model.page_of(x));
            assert_eq!(model.pages(), if merged { 2 } else { 3 });
            assert_eq!(model.stats().merged, u64::from(merged));
        }
    }

    #[test]
    #[should_panic(expected = "a page is 1 KiB to 64 KiB")]
    fn a_page_size_out_of_range_is_refused() {
        Model::new(Config::with_page_size(512));
    }

    #[test]
    fn the_proposed_layout_is_consistent() {
        let config = Config::proposed();
        assert_eq!(config.page_size, 32_768);
        assert!(config.small_fragment() * 3 <= config.largest_fragment());
        assert!(config.merge_below() <= config.seed_limit());
        assert!(config.largest_fragment() <= config.capacity());
        Model::new(config);
    }
}
