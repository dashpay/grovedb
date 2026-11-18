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
/// aggregate encoding.
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
    /// What a link stores beside the child's pointer, hash, heights and
    /// aggregate: the length of the child's key and of the aggregate.
    pub link_lengths: u32,
    /// Whether the feature type and the aggregates are stored with each
    /// count and sum at its full width, so that their length never changes.
    /// Without it they are counted as Merk's record encodes them, with
    /// varints.
    pub fixed_width_aggregates: bool,
    /// The pointer a cell stores in place of an out-of-line value.
    pub value_pointer: u32,
    /// The header of a value-log record.
    pub value_record_header: u32,
    /// The most bytes of a value one value-log record carries. A longer
    /// value is cut into several records (`value-log-format.md`, V6).
    pub value_part_max: u32,
}

impl CellCosts {
    /// The sizes of the tree page format, `docs/lignindb/tree-page.md`
    /// (FMT-02), with the feature and the aggregates stored as it asks:
    /// Merk's tag, then each count and sum at its full width.
    pub const fn tree_page() -> Self {
        CellCosts {
            page_header: 22,
            slot: 2,
            cell_header: 8,
            pointer: 6,
            link_lengths: 2,
            fixed_width_aggregates: true,
            value_pointer: 12,
            value_record_header: 18,
            value_part_max: 134_217_714,
        }
    }

    /// The sizes the simulation assumed before the tree page format
    /// existed. Sections 14 and 15 of `docs/lignindb/tree-layout.md` were
    /// computed with them.
    pub const fn assumed() -> Self {
        CellCosts {
            page_header: 32,
            slot: 2,
            cell_header: 4,
            pointer: 6,
            link_lengths: 0,
            fixed_width_aggregates: false,
            value_pointer: 16,
            value_record_header: 18,
            value_part_max: 134_217_714,
        }
    }
}

impl Default for CellCosts {
    fn default() -> Self {
        Self::tree_page()
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

    /// The largest page image the format's 16-bit offsets allow.
    pub const LARGEST_IMAGE: u32 = 65_535;

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
            seed_limit_percent: 75,
            merge_below_percent: 35,
            colocate: true,
            carry_on_rotation: false,
            carry_on_delete: true,
            costs: CellCosts::default(),
        }
    }

    /// The bytes of a page that cells can use. A page image is at most
    /// 65,535 bytes, because offsets in it are 16 bits, so a page of 64 KiB
    /// has one byte it cannot use (`flat-page.md`, P10).
    pub fn capacity(&self) -> u32 {
        self.page_size.min(Self::LARGEST_IMAGE) - self.costs.page_header
    }

    /// The bytes a value of `value_len` bytes takes in the value log: the
    /// value and one record header for each part it is cut into.
    pub fn value_log_bytes(&self, value_len: u32) -> u64 {
        let parts = value_len.div_ceil(self.costs.value_part_max);
        u64::from(value_len) + u64::from(parts) * u64::from(self.costs.value_record_header)
    }

    /// The size below which a fragment belongs in its parent's page.
    pub fn small_fragment(&self) -> u32 {
        self.page_size / self.small_fragment_divisor
    }

    /// The size above which a fragment is split where it is.
    pub fn largest_fragment(&self) -> u32 {
        (u64::from(self.capacity()) * u64::from(self.largest_fragment_percent) / 100) as u32
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
        let meta = if costs.fixed_width_aggregates {
            entry.meta_fixed_len
        } else {
            entry.meta_len
        };
        costs.slot
            + costs.cell_header
            + entry.key.len() as u32
            + 2 * HASH
            + meta
            + grove.child_links(node) * (HASH + LINK_HEIGHTS + costs.pointer + costs.link_lengths)
            + grove.hosted_roots(node) * costs.pointer
            + value
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Page {
    /// The bytes of the cells in the page, dead ones included.
    used: u32,
    /// How many cells the page holds, dead ones included.
    cells: u32,
    /// The bytes of the cells of dropped Merks that the reclaimer has not
    /// taken yet.
    dead: u32,
    dirty_in_transition: u64,
    dirty_in_block: u64,
    /// A cell of this page that the write numbered `held_in` holds.
    held: Option<NodeId>,
    held_in: u64,
    queued: bool,
}

/// One layout of a grove on pages.
pub struct Model {
    config: Config,
    capacity: u32,
    small: u32,
    largest: u32,
    seed_limit: u32,
    merge_below: u32,
    /// The pages the current write deleted a cell from.
    shrunk: Vec<u32>,
    page_of: Vec<u32>,
    cell_of: Vec<u32>,
    pages: Vec<Page>,
    free: Vec<u32>,
    /// The pages emptied in the current block. A page id is not handed out
    /// again before the block that freed it has committed.
    released: Vec<u32>,
    /// The cells of dropped Merks that still take room in their pages: the
    /// page and the bytes of each.
    dead: Vec<(u32, u32)>,
    live_pages: u64,
    used_bytes: u64,
    cells: u64,
    transition: u64,
    block: u64,
    queue: Vec<u32>,
    /// The nodes the current write holds: the ones it changed and the roots
    /// of what it moved.
    held: Vec<NodeId>,
    /// The cells the current write placed or carried: the page each was in
    /// before the write, zero for a new cell, and the rule that carried it
    /// last.
    carried: Vec<(NodeId, u32, Cause)>,
    /// For each node its place in `carried`, plus one.
    carry_slot: Vec<u32>,
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
            seed_limit: config.seed_limit(),
            merge_below: config.merge_below(),
            shrunk: Vec::new(),
            page_of: Vec::new(),
            cell_of: Vec::new(),
            pages: vec![Page::default()],
            free: Vec::new(),
            released: Vec::new(),
            dead: Vec::new(),
            live_pages: 0,
            used_bytes: 0,
            cells: 0,
            transition: 0,
            block: 1,
            queue: Vec::new(),
            held: Vec::new(),
            carried: Vec::new(),
            carry_slot: Vec::new(),
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

    /// How many pages hold at least one cell, the cell of a dropped Merk
    /// that is not reclaimed yet included.
    pub fn pages(&self) -> u64 {
        self.live_pages
    }

    /// How many cells of nodes that exist the pages hold.
    pub fn cells(&self) -> u64 {
        self.cells
    }

    /// The bytes of the cells of all nodes that exist, slot entries
    /// included.
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// The share of all page bytes that holds a header or the cell of a node
    /// that exists. What a dropped Merk left behind counts as empty.
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
            let live = page.used - page.dead + header;
            let tenth = u64::from(live) * 10 / u64::from(self.config.page_size);
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
        self.carry_slot.resize(grove.node_slots(), 0);

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
            self.remove_cell(*node);
        }
        for node in &changes.dropped {
            self.retire_cell(*node);
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
                    .filter(|page| self.pages[*page as usize].cells > 0)
                    .or_else(|| self.parent_page(grove, node))
                    .filter(|page| *page != 0)
                    .or_else(|| {
                        self.children(grove, node)
                            .map(|child| self.page_of[child.index()])
                            .find(|page| *page != 0)
                    })
                    .unwrap_or_else(|| self.allocate());
                self.insert_cell(node, page, bytes);
                self.carried.push((node, 0, Cause::Split));
                self.carry_slot[node.index()] = self.carried.len() as u32;
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
            let Some(above) = self.parent_page(grove, node) else {
                continue;
            };
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
        self.count_moves(grove, changes);

        self.directory.extend(&changes.roots_changed);
        self.directory.extend(&changes.merks_created);
        self.directory.sort_unstable();
        self.directory.dedup();
        let directory = self.directory.len() as u64 + u64::from(changes.subtrees_dropped);

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
        stats.created += changes.events.iter().filter(|event| event.created).count() as u64;
        for event in changes.events.iter().filter(|event| event.value_changed) {
            stats.value_records += 1;
            let value_len = grove.node(event.node).value_len;
            if self.config.is_out_of_line(value_len) {
                stats.out_of_line_bytes += self.config.value_log_bytes(value_len);
            }
        }
        stats.removed += changes.removed.len() as u64;
        stats.dropped += changes.dropped.len() as u64;
    }

    /// Ends a block: the pages dirtied since the last block are flushed
    /// once, and the reclaimer then takes the cells of the Merks the block
    /// dropped.
    pub fn end_block(&mut self) {
        self.stats.blocks += 1;
        self.stats.dirty_pages_by_block += self.dirty_block;
        self.dirty_block = 0;
        self.block += 1;
        self.reclaim();
    }

    /// Removes the cells of dropped Merks from their pages. A page that
    /// keeps other cells is written; one that keeps none is freed.
    fn reclaim(&mut self) {
        let mut dead = std::mem::take(&mut self.dead);
        dead.sort_unstable();
        let mut cells = dead.iter().peekable();
        while let Some((page, bytes)) = cells.next() {
            let entry = &mut self.pages[*page as usize];
            entry.used -= bytes;
            entry.dead -= bytes;
            entry.cells -= 1;
            self.stats.reclaimed += 1;
            if cells.peek().is_some_and(|(next, _)| next == page) {
                continue;
            }
            if entry.cells == 0 {
                self.release(*page);
            } else {
                self.stats.reclaim_writes += 1;
            }
        }
        dead.clear();
        self.dead = dead;
        self.free.append(&mut self.released);
    }

    /// Records where the layout stands.
    pub fn sample(&mut self) {
        let sample = self.snapshot();
        self.samples.push(sample);
    }

    /// Where the layout stands now.
    pub(crate) fn snapshot(&self) -> Sample {
        Sample {
            stats: self.stats.clone(),
            cells: self.cells,
            pages: self.live_pages,
            used_bytes: self.used_bytes,
            fill: self.fill(),
        }
    }

    /// Checks the model's bookkeeping against a recount from the grove:
    /// every live node is in exactly one page with the cell size its record
    /// gives, every page's totals and the totals of the layout add up, no
    /// page is over its capacity, and the free pages are the empty ones.
    pub fn check(&self, grove: &Grove) -> Result<(), String> {
        let mut used = vec![0u64; self.pages.len()];
        let mut cells = vec![0u32; self.pages.len()];
        let mut dead = vec![0u64; self.pages.len()];
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
        let bytes: u64 = used.iter().sum();
        if bytes != self.used_bytes {
            return Err(format!(
                "the pages hold {bytes} bytes of cells but {} are counted",
                self.used_bytes
            ));
        }
        for (page, bytes) in &self.dead {
            used[*page as usize] += u64::from(*bytes);
            dead[*page as usize] += u64::from(*bytes);
            cells[*page as usize] += 1;
        }
        let mut pages = 0;
        for (id, page) in self.pages.iter().enumerate().skip(1) {
            if u64::from(page.used) != used[id]
                || page.cells != cells[id]
                || u64::from(page.dead) != dead[id]
            {
                return Err(format!(
                    "page {id} claims {} bytes in {} cells, {} of them dead, but holds {} in {}, \
                     {} dead",
                    page.used, page.cells, page.dead, used[id], cells[id], dead[id]
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
        let mut free = self.free.clone();
        free.extend(&self.released);
        free.sort_unstable();
        let empty: Vec<u32> = (1..self.pages.len() as u32)
            .filter(|id| cells[*id as usize] == 0)
            .collect();
        if free != empty {
            return Err(format!(
                "{} pages are free but {} are empty",
                free.len(),
                empty.len()
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

    fn parent_page(&self, grove: &Grove, node: NodeId) -> Option<u32> {
        self.parent(grove, node)
            .map(|parent| self.page_of[parent.index()])
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

    fn hold(&mut self, page: u32, node: NodeId) {
        let entry = &mut self.pages[page as usize];
        entry.held = Some(node);
        entry.held_in = self.transition;
    }

    fn insert_cell(&mut self, node: NodeId, page: u32, bytes: u32) {
        assert!(
            bytes * 2 < self.capacity,
            "a cell of {bytes} bytes is not below half a page: the page is too small for the \
             keys and inline values of this workload"
        );
        self.page_of[node.index()] = page;
        self.cell_of[node.index()] = bytes;
        self.used_bytes += u64::from(bytes);
        self.cells += 1;
        let entry = &mut self.pages[page as usize];
        entry.used += bytes;
        entry.cells += 1;
        self.hold(page, node);
        self.dirty(page);
        self.queue_if_over(page);
    }

    fn resize_cell(&mut self, node: NodeId, bytes: u32) {
        assert!(
            bytes * 2 < self.capacity,
            "a cell of {bytes} bytes is not below half a page: the page is too small for the \
             keys and inline values of this workload"
        );
        let page = self.page_of[node.index()];
        let old = std::mem::replace(&mut self.cell_of[node.index()], bytes);
        self.used_bytes = self.used_bytes - u64::from(old) + u64::from(bytes);
        let entry = &mut self.pages[page as usize];
        entry.used = entry.used - old + bytes;
        self.hold(page, node);
        self.dirty(page);
        self.queue_if_over(page);
    }

    fn remove_cell(&mut self, node: NodeId) {
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
        self.dirty(page);
        if self.pages[page as usize].cells == 0 {
            self.release(page);
        }
    }

    /// Takes the cell of a node that went with a dropped Merk out of the
    /// layout. Its page is not written, and the cell keeps its room there
    /// until the reclaimer comes (D2).
    fn retire_cell(&mut self, node: NodeId) {
        let page = std::mem::take(&mut self.page_of[node.index()]);
        if page == 0 {
            return;
        }
        let bytes = std::mem::take(&mut self.cell_of[node.index()]);
        self.used_bytes -= u64::from(bytes);
        self.cells -= 1;
        self.dead.push((page, bytes));
        let entry = &mut self.pages[page as usize];
        entry.dead += bytes;
        if entry.held == Some(node) {
            entry.held = None;
        }
    }

    /// Gives back a page that holds no cell. Its id can be used again once
    /// the block has ended.
    fn release(&mut self, page: u32) {
        self.pages[page as usize].held = None;
        self.released.push(page);
        self.live_pages -= 1;
        self.stats.pages_freed += 1;
    }

    fn queue_if_over(&mut self, page: u32) {
        let entry = &mut self.pages[page as usize];
        if entry.used > self.capacity && !entry.queued {
            entry.queued = true;
            self.queue.push(page);
        }
    }

    /// Carries one cell to another page. The page it leaves and the page it
    /// enters are written; the caller writes the page of the cell that
    /// points at it.
    fn carry(&mut self, grove: &Grove, node: NodeId, to: u32, cause: Cause) {
        let bytes = self.cell_of[node.index()];
        let from = self.page_of[node.index()];
        self.remove_cell(node);
        self.insert_cell(node, to, bytes);
        match self.carry_slot[node.index()] {
            0 => {
                self.carried.push((node, from, cause));
                self.carry_slot[node.index()] = self.carried.len() as u32;
            }
            slot => self.carried[slot as usize - 1].2 = cause,
        }
        if grove.is_merk_root(node) {
            self.directory.push(grove.node(node).merk);
        }
    }

    /// Counts the cells the write leaves in another page than it found them
    /// in. A cell carried several times counts once, under the rule that
    /// carried it last, and not at all when it is back where it was or the
    /// write created it. Its out-of-line value is appended again unless the
    /// write wrote that value anyway.
    fn count_moves(&mut self, grove: &Grove, changes: &ChangeSet) {
        let mut rewritten: Vec<NodeId> = Vec::new();
        for index in 0..self.carried.len() {
            let (node, from, cause) = self.carried[index];
            self.carry_slot[node.index()] = 0;
            if from == 0 || from == self.page_of[node.index()] {
                continue;
            }
            if rewritten.is_empty() {
                rewritten.extend(
                    changes
                        .events
                        .iter()
                        .filter(|event| event.value_changed)
                        .map(|event| event.node),
                );
                rewritten.sort_unstable();
            }
            let moves = &mut self.stats.moves[cause as usize];
            moves.cells += 1;
            moves.cell_bytes += u64::from(self.cell_of[node.index()]);
            let value_len = grove.node(node).value_len;
            if self.config.is_out_of_line(value_len) && rewritten.binary_search(&node).is_err() {
                moves.values += 1;
                moves.value_bytes += self.config.value_log_bytes(value_len);
            }
        }
        self.carried.clear();
    }

    fn relink(&mut self, grove: &Grove, node: NodeId) {
        if let Some(parent) = grove.grove_parent(node) {
            let page = self.page_of[parent.index()];
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
        self.hold(to, root);
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
            let entry = &self.pages[page as usize];
            let held = entry
                .held
                .filter(|_| entry.held_in == self.transition)
                .or_else(|| {
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
        let above = self.parent_page(grove, root).filter(|page| *page != 0);
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

    /// A page that a delete left nearly empty gives the fragments the write
    /// holds in it to a page beside it.
    fn merge_underfull(&mut self, grove: &Grove) {
        for index in 0..self.shrunk.len() {
            let page = self.shrunk[index];
            let entry = self.pages[page as usize];
            if entry.cells == 0 || entry.used >= self.merge_below {
                continue;
            }
            for root in self.held_roots(grove, |held| held == page) {
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
            .parent_page(grove, root)
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
        let leaving: Vec<(NodeId, u32)> = choose(&parts, target)
            .into_iter()
            .map(|index| parts[index])
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
        for root in self.held_roots(grove, |page| page != 0) {
            let page = self.page_of[root.index()];
            let Some(above) = self.parent_page(grove, root) else {
                continue;
            };
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

    /// The distinct roots of the fragments that hold a node the write holds
    /// in a page that passes `keep`, in the order the write took them.
    fn held_roots(&self, grove: &Grove, keep: impl Fn(u32) -> bool) -> Vec<NodeId> {
        let mut roots: Vec<NodeId> = Vec::new();
        for node in &self.held {
            if keep(self.page_of[node.index()]) {
                let root = self.fragment_root(grove, *node);
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        roots
    }
}

/// Which parts to move so that their bytes come closest to `target`: their
/// places in `parts`, in rising order, never none. Ties go to fewer bytes,
/// then to the choice whose last part is listed earliest.
///
/// Up to twelve parts every choice is tried. Above that the parts are taken
/// largest first, each only if it brings the sum closer.
fn choose(parts: &[(NodeId, u32)], target: u32) -> Vec<usize> {
    const EXHAUSTIVE: usize = 12;
    let target = u64::from(target);
    if parts.len() > EXHAUSTIVE {
        let mut order: Vec<usize> = (0..parts.len()).collect();
        order.sort_by_key(|index| (std::cmp::Reverse(parts[*index].1), *index));
        let mut chosen = Vec::new();
        let mut sum = 0u64;
        for index in order {
            let bytes = u64::from(parts[index].1);
            if chosen.is_empty() || (sum + bytes).abs_diff(target) < sum.abs_diff(target) {
                chosen.push(index);
                sum += bytes;
            }
        }
        chosen.sort_unstable();
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
        let candidate = (sum.abs_diff(target), sum, mask);
        if candidate < best {
            best = candidate;
        }
    }
    (0..parts.len())
        .filter(|index| best.2 & (1 << index) != 0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_sim::tree::{hand::Hand, Event, Side};

    /// A small page, so a handful of nodes fills it. The byte counts in
    /// these tests are worked out for the sizes the simulation first
    /// assumed: with a 32-byte value a leaf cell is 2 + 4 + 4 + 64 + 1 + 32 =
    /// 107 bytes and each link adds 41, so a page of 1024 bytes holds about
    /// seven nodes. A fragment may fill the page, a fragment that leaves
    /// starts a fresh page and nothing is merged, so each test meets one
    /// rule at a time.
    fn config() -> Config {
        Config {
            small_fragment_divisor: 8,
            largest_fragment_percent: 100,
            seed_limit_percent: 0,
            merge_below_percent: 0,
            costs: CellCosts::assumed(),
            ..Config::with_page_size(1024)
        }
    }

    fn key(index: u32) -> [u8; 4] {
        index.to_be_bytes()
    }

    /// Carries cells to `to` outside any transition, as a rule would.
    fn carry_by_hand(model: &mut Model, grove: &Grove, nodes: &[NodeId], to: u32) {
        for node in nodes {
            model.carry(grove, *node, to, Cause::Split);
        }
        model.count_moves(grove, &ChangeSet::default());
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
    fn a_cell_of_the_tree_page_format_counts_its_real_parts() {
        use lignin_format::tree_page::{TreeCell, TreeLink, TreePage};

        let costs = CellCosts::tree_page();
        assert_eq!(costs, CellCosts::default());
        assert!(costs.fixed_width_aggregates && !CellCosts::assumed().fixed_width_aggregates);
        assert_eq!(costs.page_header as usize, TreePage::HEADER_LEN);
        assert_eq!(costs.slot as usize, TreePage::SLOT_LEN);
        assert_eq!(costs.cell_header as usize, TreeCell::HEADER_LEN);
        assert_eq!(costs.pointer as usize, TreeCell::HOSTED_ROOT_LEN);
        assert_eq!(costs.value_pointer as usize, TreeCell::VALUE_LOCATION_LEN);
        assert_eq!(
            (HASH + LINK_HEIGHTS + costs.pointer + costs.link_lengths) as usize,
            TreeLink::FIXED_LEN
        );
        assert_eq!((costs.cell_header + 2 * HASH) as usize, TreeCell::FIXED_LEN);

        // The same nodes as in `a_cell_counts_every_part_merk_stores`, each
        // against the format's own formula. The feature and each aggregate
        // are one byte here: Merk's tag, in a tree with no count or sum.
        let mut hand = Hand::new();
        let root = hand.add(MerkId::ROOT, b"key", 32);
        hand.set_root(MerkId::ROOT, Some(root));
        let config = Config::proposed();
        let slot = TreePage::SLOT_LEN;
        assert_eq!(
            config.cell_bytes(&hand.grove, root) as usize,
            slot + TreeCell::len_of(3, 1, None, None, 0, 32)
        );
        let child = hand.add(MerkId::ROOT, b"a", 300);
        hand.link(root, Side::Left, Some(child));
        assert_eq!(
            config.cell_bytes(&hand.grove, root) as usize,
            slot + TreeCell::len_of(3, 1, Some(1), None, 0, 32)
        );
        assert_eq!(
            config.cell_bytes(&hand.grove, child) as usize,
            slot + TreeCell::len_of(1, 1, None, None, 0, TreeCell::VALUE_LOCATION_LEN)
        );
        let hosted = hand.host(child);
        let inner = hand.add(hosted, b"inner", 1);
        hand.set_root(hosted, Some(inner));
        assert_eq!(
            config.cell_bytes(&hand.grove, child) as usize,
            slot + TreeCell::len_of(1, 1, None, None, 1, TreeCell::VALUE_LOCATION_LEN)
        );
        assert_eq!(
            config.capacity() as usize,
            lignin_format::tree_page::TreePageSize::DEFAULT.cell_capacity()
        );
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
    fn the_first_split_sends_about_half_of_the_page_away() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        // Seven nodes need 7 * 107 + 6 * 41 = 995 bytes, three more than a
        // page holds.
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 7);
        let stats = model.stats();
        assert_eq!((stats.splits, stats.split_off, stats.promoted), (1, 1, 0));
        assert_eq!(model.pages(), 2);
        let top = model.page_of(nodes[3]).expect("placed");
        let away = model.page_of(nodes[1]).expect("placed");
        assert_ne!(top, away);
        for kept in [5, 4, 6] {
            assert_eq!(model.page_of(nodes[kept]), Some(top));
        }
        for left in [0, 2] {
            assert_eq!(model.page_of(nodes[left]), Some(away));
        }
        let capacity = model.config().capacity();
        for page in [top, away] {
            let used = model.pages[page as usize].used;
            assert!(
                used * 3 >= capacity && used * 3 <= capacity * 2,
                "{used} of {capacity}"
            );
        }
        assert_eq!(model.stats().moved().cells, 3);
    }

    #[test]
    fn a_split_sends_the_fragment_root_up_to_its_parents_page() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 7);
        let top = model.page_of(nodes[3]).expect("placed");
        let below = model.page_of(nodes[1]).expect("placed");
        assert_ne!(top, below);
        // Four more leaves fill the page below the top one.
        let before = model.stats().clone();
        let places = [
            (nodes[0], Side::Left),
            (nodes[0], Side::Right),
            (nodes[2], Side::Left),
            (nodes[2], Side::Right),
        ];
        for (index, (parent, side)) in places.into_iter().enumerate() {
            let node = hand.add(MerkId::ROOT, &key(100 + index as u32), 32);
            hand.link(parent, side, Some(node));
            hand.touch_path(node);
            step(&mut hand, &mut model);
            model.check(&hand.grove).expect("consistent");
        }
        let stats = model.stats().since(&before);
        assert_eq!((stats.splits, stats.promoted, stats.split_off), (1, 1, 1));
        assert_eq!(
            model.page_of(nodes[1]),
            Some(top),
            "the root of the fragment is now beside its parent"
        );
        assert_eq!(model.page_of(nodes[2]), Some(below));
        let third = model.page_of(nodes[0]).expect("placed");
        assert!(third != top && third != below);
        assert_eq!(model.pages(), 3);
        assert_eq!(
            stats.moved().cells,
            4,
            "the root that went up and the three cells of the half that left"
        );
    }

    #[test]
    fn a_growing_tree_leaves_no_page_overfull() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        grow(&mut hand, &mut model, MerkId::ROOT, 200);
        let stats = model.stats();
        assert!(stats.promoted > 1 && stats.split_off > 1);
        assert_eq!(stats.overfull_left, 0);
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
        carry_by_hand(model, &hand.grove, &[y, a, b], bottom);
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
        carry_by_hand(&mut model, &hand.grove, &[a], top);
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
        assert_eq!(model.cells(), 2);
        assert_eq!(
            (model.pages(), drop.reclaimed),
            (pages, 0),
            "the dropped cells keep their room until the block ends"
        );
        let fill = model.fill();

        // The reclaimer comes after the block. The dropped Merk had pages
        // of its own, so they are freed and none is written.
        model.end_block();
        model.check(&hand.grove).expect("consistent");
        let reclaim = model.stats().since(&before);
        assert_eq!((reclaim.reclaimed, reclaim.reclaim_writes), (6, 0));
        assert_eq!(model.pages(), 1);
        assert!(model.fill() > fill);
    }

    #[test]
    fn the_reclaimer_writes_a_page_that_keeps_other_cells() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 3);
        let hosted = hand.host(nodes[0]);
        hand.touch_path(nodes[0]);
        step(&mut hand, &mut model);
        grow(&mut hand, &mut model, hosted, 2);
        assert_eq!(model.pages(), 1, "the hosted Merk is in its host's page");
        let used = model.pages[1].used;

        hand.link(nodes[1], Side::Left, None);
        hand.remove(nodes[0]);
        hand.touch_path(nodes[1]);
        step(&mut hand, &mut model);
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.cells(), 2);
        let page = model.pages[1];
        assert_eq!((page.cells, page.dead), (4, 107 + 41 + 107));
        assert!(
            page.used > used - 300,
            "the dead cells still take their room"
        );
        assert_eq!(model.fill_histogram()[2], 1, "but do not count as filled");

        model.end_block();
        model.check(&hand.grove).expect("consistent");
        let stats = model.stats();
        assert_eq!((stats.reclaimed, stats.reclaim_writes), (2, 1));
        assert_eq!((model.pages[1].cells, model.pages[1].dead), (2, 0));
    }

    #[test]
    fn a_cell_that_ends_in_another_page_is_counted_once_with_its_value() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let root = hand.add(MerkId::ROOT, b"r", 1000);
        hand.set_root(MerkId::ROOT, Some(root));
        let child = hand.add(MerkId::ROOT, b"c", 1);
        hand.link(root, Side::Left, Some(child));
        step(&mut hand, &mut model);
        assert_eq!(model.stats().out_of_line_bytes, 1018);
        assert_eq!(model.stats().moved().cells, 0, "a new cell is placed");
        let home = model.page_of(root).expect("placed");
        let nothing = ChangeSet::default();
        let moved = |model: &Model, cause: Cause| {
            let moves = model.stats().moves[cause as usize];
            (
                moves.cells,
                moves.cell_bytes,
                moves.values,
                moves.value_bytes,
            )
        };

        // Carried away and back by one write: no move.
        let away = model.allocate();
        model.carry(&hand.grove, root, away, Cause::Split);
        model.carry(&hand.grove, root, home, Cause::Merge);
        model.count_moves(&hand.grove, &nothing);
        assert_eq!(model.stats().moved().cells, 0);

        // Carried twice by one write: one move, under the last rule.
        let (first, second) = (model.allocate(), model.allocate());
        model.carry(&hand.grove, root, first, Cause::Merge);
        model.carry(&hand.grove, root, second, Cause::Split);
        model.count_moves(&hand.grove, &nothing);
        assert_eq!(moved(&model, Cause::Split), (1, 129, 1, 1018));
        assert_eq!(moved(&model, Cause::Merge), (0, 0, 0, 0));

        // Carried by the write that gives it a new value: the value is
        // written once, into the page the cell ends in.
        let rewritten = ChangeSet {
            events: vec![Event {
                node: root,
                created: false,
                old_parent: None,
                old_left: None,
                old_right: None,
                value_changed: true,
                links_changed: false,
            }],
            ..ChangeSet::default()
        };
        let last = model.allocate();
        model.carry(&hand.grove, root, last, Cause::Delete);
        model.count_moves(&hand.grove, &rewritten);
        assert_eq!(moved(&model, Cause::Delete), (1, 129, 0, 0));
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.pages(), 2);
    }

    #[test]
    fn a_freed_page_is_not_handed_out_before_the_block_ends() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let root = hand.add(MerkId::ROOT, b"r", 1);
        hand.set_root(MerkId::ROOT, Some(root));
        step(&mut hand, &mut model);
        let home = model.page_of(root).expect("placed");
        let away = model.allocate();
        carry_by_hand(&mut model, &hand.grove, &[root], away);
        assert_eq!(model.pages(), 1);
        let fresh = model.allocate();
        assert!(fresh != home && fresh != away);
        carry_by_hand(&mut model, &hand.grove, &[root], fresh);
        model.check(&hand.grove).expect("consistent");

        model.end_block();
        model.check(&hand.grove).expect("consistent");
        assert_eq!(model.allocate(), away, "the page freed last goes out first");
    }

    #[test]
    fn a_long_value_pays_a_record_header_for_each_part() {
        let config = Config::proposed();
        assert_eq!(config.value_log_bytes(1000), 1018);
        assert_eq!(config.value_log_bytes(134_217_714), 134_217_714 + 18);
        assert_eq!(config.value_log_bytes(134_217_715), 134_217_715 + 2 * 18);
        assert_eq!(
            config.value_log_bytes(u32::MAX),
            u64::from(u32::MAX) + 33 * 18
        );
    }

    #[test]
    fn a_new_cell_that_a_split_carries_is_not_a_move() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let nodes = grow(&mut hand, &mut model, MerkId::ROOT, 6);
        assert_eq!(model.pages(), 1);
        // The seventh node fills the page. It hangs in the left half, which
        // is the half that leaves.
        let node = hand.add(MerkId::ROOT, &[0, 0, 0, 0, 0], 32);
        hand.link(nodes[0], Side::Left, Some(node));
        hand.touch_path(node);
        step(&mut hand, &mut model);
        assert_eq!(model.pages(), 2);
        assert_eq!(model.page_of(node), model.page_of(nodes[0]));
        assert_ne!(model.page_of(node), model.page_of(nodes[3]));
        assert_eq!(
            model.stats().moved().cells,
            3,
            "the three cells of that half that were there before"
        );
    }

    #[test]
    #[should_panic(expected = "is not below half a page")]
    fn a_cell_of_half_a_page_is_refused() {
        let mut hand = Hand::new();
        let mut model = Model::new(config());
        let root = hand.add(MerkId::ROOT, &[7; 200], 256);
        hand.set_root(MerkId::ROOT, Some(root));
        step(&mut hand, &mut model);
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
                model.used_bytes,
                model.free.clone(),
                model.released.clone(),
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
                model.used_bytes,
                model.free,
                model.released,
            ) = saved;
        };
        damaged(&|model| model.page_of[index] = 0, "is in no page");
        damaged(&|model| model.cell_of[index] += 1, "its record needs");
        damaged(&|model| model.cells += 1, "live nodes but");
        damaged(&|model| model.pages[page].cells += 1, "claims");
        damaged(&|model| model.pages[page].dead += 1, "claims");
        damaged(&|model| model.live_pages += 1, "pages in use but");
        damaged(&|model| model.used_bytes += 1, "bytes of cells but");
        damaged(&|model| model.free.push(1), "are free but");
        damaged(&|model| model.released.push(1), "are free but");
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
            vec![1],
            "a tie goes to fewer bytes"
        );
        assert_eq!(choose(&parts(&[500, 400]), 460), vec![0]);
        assert_eq!(
            choose(&parts(&[100, 200, 300]), 300),
            vec![0, 1],
            "then to the choice whose last part is listed earliest"
        );
        assert_eq!(choose(&parts(&[100, 200, 300]), 10_000), vec![0, 1, 2]);
        assert_eq!(choose(&parts(&[700]), 1), vec![0], "something always moves");
        let many: Vec<u32> = (1..=20).map(|size| size * 10).collect();
        let chosen = choose(&parts(&many), 450);
        let sum: u32 = chosen.iter().map(|index| many[*index]).sum();
        assert!(!chosen.is_empty() && sum.abs_diff(450) <= 100, "{sum}");
        assert!(chosen.is_sorted());
        // Past twelve parts the largest goes first, wherever it is listed.
        let mut long = vec![1; 40];
        long[39] = 1000;
        assert_eq!(choose(&parts(&long), 1000), vec![39]);
        long[35] = 600;
        assert_eq!(choose(&parts(&long), 1600), vec![35, 39]);
        assert_eq!(choose(&parts(&long), 1602), vec![0, 1, 35, 39]);
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
        carry_by_hand(&mut model, &hand.grove, &[nodes[0]], below);
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
            carry_by_hand(&mut model, &hand.grove, &[c], third);
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
    #[should_panic(expected = "a small fragment is at most half a page")]
    fn a_small_fragment_of_a_whole_page_is_refused() {
        Model::new(Config {
            small_fragment_divisor: 1,
            ..Config::proposed()
        });
    }

    #[test]
    #[should_panic(expected = "a fragment is at most a page")]
    fn a_largest_fragment_above_a_page_is_refused() {
        Model::new(Config {
            largest_fragment_percent: 101,
            ..Config::proposed()
        });
    }

    #[test]
    #[should_panic(expected = "half of a split fragment must not count as small")]
    fn a_largest_fragment_of_two_small_ones_is_refused() {
        Model::new(Config {
            small_fragment_divisor: 4,
            ..Config::proposed()
        });
    }

    #[test]
    #[should_panic(expected = "a page is at most full")]
    fn a_seed_limit_above_a_page_is_refused() {
        Model::new(Config {
            seed_limit_percent: 101,
            ..Config::proposed()
        });
    }

    #[test]
    #[should_panic(expected = "a page that gives its fragment away must be able to take one")]
    fn a_merge_threshold_above_the_seed_limit_is_refused() {
        Model::new(Config {
            merge_below_percent: 80,
            ..Config::proposed()
        });
    }

    #[test]
    fn a_page_of_64_kib_keeps_one_byte_unused() {
        let config = Config::with_page_size(65_536);
        assert_eq!(config.capacity(), 65_535 - 22);
        assert_eq!(Config::with_page_size(65_535).capacity(), 65_535 - 22);
        Model::new(config);
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
