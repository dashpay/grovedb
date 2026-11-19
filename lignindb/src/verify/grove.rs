//! The walk of the Merks from their roots, the agreement of the directory
//! with the cells, and the cells that nothing reaches.

use std::{
    cmp::Ordering,
    collections::{hash_map::Entry, HashMap},
    rc::Rc,
};

use lignin_format::{
    tree_page::{Side, TreePage},
    CellAddress, PageId,
};
use lignin_store::load::FRAME_FORMAT;
use lignin_tree::{
    directory::{Dropped, Prefix},
    hash::{self, HashRules},
    node::{NodePage, NodeRef},
};

use super::{
    pages::{Kind, Reach, Seen},
    read::ValueFault,
    Grove, Place, Rule, Run, Unreached,
};

/// The most cells a descent by key passes before the pages are taken for
/// damaged. A Merk of 2^64 nodes is at most 93 levels deep.
const MAX_DEPTH: usize = 128;

/// A Merk as its entry in the directory has it.
#[derive(Debug)]
pub(super) struct Merk {
    pub(super) prefix: Prefix,
    /// Whether the Merk is in use: not below a dropped one.
    pub(super) live: bool,
    /// The root's cell and its key.
    pub(super) root: Option<(CellAddress, Vec<u8>)>,
    /// The element that hosts the Merk: the prefix of the Merk it is in,
    /// its key, and the position of this Merk among those it hosts.
    pub(super) host: Option<(Prefix, Vec<u8>, u8)>,
}

/// What the directory holds.
#[derive(Debug, Default)]
pub(super) struct Entries {
    pub(super) merks: Vec<Merk>,
    pub(super) by_prefix: HashMap<Prefix, usize>,
    pub(super) drops: Vec<Dropped>,
}

/// A pointer to a hosted root, as a cell of a Merk in use holds it: the
/// Merk, the host's key and the position.
type HostedAt = (usize, Vec<u8>, u8);

/// Which tree of cells a walk is in.
#[derive(Clone, Copy)]
enum Walk<'e> {
    /// A Merk in use that the directory names: `merk` in its entries. A
    /// pointer to a hosted root is kept for the entry of that Merk.
    Entered { merk: usize, prefix: &'e Prefix },
    /// The Merks below a root that the caller named, in a store with no
    /// directory. Every pointer to a hosted root is followed.
    Rooted,
    /// What a dropped Merk left. Every pointer is followed.
    Left,
}

/// A cell that a walk has still to enter.
struct Step {
    at: CellAddress,
    /// The cell that names it.
    from: Option<CellAddress>,
    /// The node's key is above `low` and below `high`.
    low: Option<Rc<[u8]>>,
    high: Option<Rc<[u8]>>,
    /// What the link that names the cell states of it.
    key_len: Option<u8>,
    heights: Option<(u8, u8)>,
}

impl Step {
    fn root(at: CellAddress, from: Option<CellAddress>) -> Self {
        Self {
            at,
            from,
            low: None,
            high: None,
            key_len: None,
            heights: None,
        }
    }
}

/// The heights of the two subtrees of `node`, as its own links give them.
fn heights_of(node: &NodeRef<'_>) -> Option<(u8, u8)> {
    let height = |side| match node.link(side) {
        None => Some(0u8),
        Some(link) => link.heights.0.max(link.heights.1).checked_add(1),
    };
    Some((height(Side::Left)?, height(Side::Right)?))
}

impl Run<'_, '_, '_> {
    /// Walks every Merk from its root, and compares the directory with the
    /// cells.
    pub(super) fn walk_grove(
        &mut self,
        entries: Option<&Entries>,
        rules: &mut Option<&mut dyn HashRules>,
    ) {
        let mut hosted = HashMap::new();
        match (self.store.grove, entries) {
            (Grove::Roots(roots), _) => {
                for root in roots {
                    self.census.merks += 1;
                    self.walk_merk(*root, None, Walk::Rooted, rules, &mut hosted);
                }
            }
            (Grove::Directory { .. }, Some(entries)) => {
                for (index, merk) in entries.merks.iter().enumerate() {
                    let Some((root, key)) = merk.root.as_ref().filter(|_| merk.live) else {
                        continue;
                    };
                    let walk = Walk::Entered {
                        merk: index,
                        prefix: &merk.prefix,
                    };
                    self.walk_merk(*root, Some(key), walk, rules, &mut hosted);
                }
                self.compare_hosts(entries, &mut hosted);
                for dropped in &entries.drops {
                    if let Some(root) = dropped.root {
                        self.walk_merk(root, None, Walk::Left, rules, &mut hosted);
                    }
                }
            }
            // The directory was not read, which was reported.
            (Grove::Directory { .. }, None) => {}
        }
    }

    /// Walks the cells below `root`, the root of one Merk.
    fn walk_merk(
        &mut self,
        root: CellAddress,
        root_key: Option<&Vec<u8>>,
        walk: Walk<'_>,
        rules: &mut Option<&mut dyn HashRules>,
        hosted: &mut HashMap<HostedAt, (CellAddress, CellAddress)>,
    ) {
        let reach = match walk {
            Walk::Entered { .. } | Walk::Rooted => Reach::Live,
            Walk::Left => Reach::Left,
        };
        let top = match walk {
            Walk::Entered { prefix, .. } => Place::Merk(*prefix),
            Walk::Rooted | Walk::Left => Place::Cell(root),
        };
        let mut stack = vec![Step::root(root, None)];
        while let Some(step) = stack.pop() {
            self.begin_step();
            let at = step.at;
            let holder = step.from.map_or(top, Place::Cell);
            // The address of a root that an entry names is the entry's.
            let dangling = match (step.from, walk) {
                (None, Walk::Entered { .. }) => Rule::EntryRoot,
                _ => Rule::DanglingAddress,
            };
            match self.see(at.page()) {
                Kind::Tree => {}
                Kind::Bad => continue,
                kind => {
                    self.add(dangling, holder, || match kind {
                        Kind::Flat => format!("the address {at} is in a flat page"),
                        _ => format!("the address {at} is in no page of the version"),
                    });
                    continue;
                }
            }
            let Ok(frame) = self.frame(at.page()) else {
                continue;
            };
            let Ok(page) = NodePage::open(&frame, self.lease) else {
                continue;
            };
            let Some(node) = page.node(at.slot()) else {
                let what = || format!("the address {at} names a slot that holds no cell");
                self.add(dangling, holder, what);
                continue;
            };
            if !self.reach(at, reach) {
                let what = || format!("the address {at} names a cell that was reached before");
                self.add(Rule::CellNamedTwice, holder, what);
                continue;
            }
            let key = node.key();
            let above = step.low.as_ref().is_none_or(|low| low[..] < *key);
            let below = step.high.as_ref().is_none_or(|high| *key < high[..]);
            if !above || !below {
                let what = || "the node's key is not between the keys above it".to_owned();
                self.add(Rule::KeyOrder, Place::Cell(at), what);
            }
            if step.from.is_none() && root_key.is_some_and(|root_key| root_key != key) {
                let what = || format!("the cell at {at} has another key than the entry states");
                self.add(Rule::EntryRoot, top, what);
            }
            if step
                .key_len
                .is_some_and(|len| usize::from(len) != key.len())
            {
                let (stated, found) = (step.key_len.unwrap_or_default(), key.len());
                self.add(Rule::LinkKeyLength, holder, || {
                    format!(
                        "the link to {at} states a key of {stated} bytes, the child has {found}"
                    )
                });
            }
            let own = heights_of(&node);
            if step.heights.is_some_and(|stated| Some(stated) != own) {
                let stated = step.heights.unwrap_or_default();
                self.add(Rule::LinkHeights, holder, || match own {
                    Some(own) => format!(
                        "the link to {at} states the heights {stated:?}, the child's links give \
                         {own:?}"
                    ),
                    None => format!("the links of the child at {at} give no height"),
                });
            }
            if reach == Reach::Live {
                self.check_cell(&node, rules);
            }
            for (position, pointer) in node.hosted_roots().enumerate() {
                let Some(pointer) = pointer else {
                    continue;
                };
                match walk {
                    Walk::Entered { merk, .. } => {
                        let hosted_at = (merk, key.to_vec(), position as u8);
                        hosted.insert(hosted_at, (at, pointer));
                    }
                    Walk::Rooted => {
                        self.census.merks += 1;
                        self.census.hosted_roots += 1;
                        stack.push(Step::root(pointer, Some(at)));
                    }
                    Walk::Left => stack.push(Step::root(pointer, Some(at))),
                }
            }
            let key: Rc<[u8]> = Rc::from(key);
            for side in Side::BOTH {
                let Some(link) = node.link(side) else {
                    continue;
                };
                let (low, high) = match side {
                    Side::Left => (step.low.clone(), Some(key.clone())),
                    Side::Right => (Some(key.clone()), step.high.clone()),
                };
                stack.push(Step {
                    at: link.child,
                    from: Some(at),
                    low,
                    high,
                    key_len: Some(link.child_key_len),
                    heights: Some(link.heights),
                });
            }
        }
    }

    /// Reads the value of a cell in use, if it is out of line, and
    /// recomputes its hashes.
    fn check_cell(&mut self, node: &NodeRef<'_>, rules: &mut Option<&mut dyn HashRules>) {
        let at = node.address();
        let place = Place::Cell(at);
        if let Some(value) = node.cell().value_place()
            && let Err(fault) = self.values.fetch(value, at.page())
        {
            let rule = match fault {
                ValueFault::Owner(_) => Some(Rule::ValueOwner),
                // The census of the pages reported it.
                ValueFault::NoBytes { .. } => None,
                ValueFault::Unreadable(_) | ValueFault::Page(_) => Some(Rule::ValueUnreadable),
            };
            if let Some(rule) = rule {
                self.add(rule, place, || fault.to_string());
            }
        }
        let Some(rules) = rules.as_deref_mut() else {
            return;
        };
        let mut rules: &mut dyn HashRules = rules;
        self.census.hashed_cells += 1;
        match hash::check(&mut self.tree, &mut self.values, &mut rules, at) {
            Ok(differing) => {
                for field in differing.fields() {
                    let what = || "the stored hash differs from its recomputation".to_owned();
                    self.add(Rule::Hash(field), place, what);
                }
            }
            Err(error) => self.add(Rule::HashUnchecked, place, || error.to_string()),
        }
    }

    /// Compares the entry of every hosted Merk in use with the cell of its
    /// host. `hosted` has the pointers that the walks found, and is left
    /// with those of no entry, which are reported.
    fn compare_hosts(
        &mut self,
        entries: &Entries,
        hosted: &mut HashMap<HostedAt, (CellAddress, CellAddress)>,
    ) {
        for merk in entries.merks.iter().filter(|merk| merk.live) {
            let Some((parent, key, position)) = &merk.host else {
                continue;
            };
            let place = Place::Merk(merk.prefix);
            let parent_merk = (entries.by_prefix.get(parent))
                .map(|index| (*index, &entries.merks[*index]))
                .filter(|(_, parent)| parent.live);
            let Some((index, parent_merk)) = parent_merk else {
                let what = || {
                    format!(
                        "the host is in {}, which is not in use",
                        Place::Merk(*parent)
                    )
                };
                self.add(Rule::EntryHost, place, what);
                continue;
            };
            let root = merk.root.as_ref().map(|(root, _)| *root);
            let pointer = hosted.entry((index, key.clone(), *position));
            let pointer = match pointer {
                Entry::Occupied(pointer) => Some(pointer.remove()),
                Entry::Vacant(_) => None,
            };
            match (root, pointer) {
                (Some(root), Some((_, pointer))) if root == pointer => {
                    self.census.hosted_roots += 1;
                }
                (root, Some((host, pointer))) => {
                    self.add(Rule::HostPointer, Place::Cell(host), || match root {
                        Some(root) => format!(
                            "the host points at {pointer}, and the entry of {place} names the \
                             root {root}"
                        ),
                        None => format!(
                            "the host points at {pointer}, and the entry of {place} names no root"
                        ),
                    });
                }
                (root, None) => {
                    let parent_root = parent_merk.root.as_ref().map(|(root, _)| *root);
                    match (self.find(parent_root, key), root) {
                        (Some(_), None) => {}
                        (Some(host), Some(root)) => {
                            self.add(Rule::HostPointer, Place::Cell(host), || {
                                format!(
                                    "the host has no pointer in position {position}, and the \
                                     entry of {place} names the root {root}"
                                )
                            });
                        }
                        (None, _) => {
                            let what =
                                || format!("the host is no node of {}", Place::Merk(*parent));
                            self.add(Rule::EntryHost, place, what);
                        }
                    }
                }
            }
        }
        if hosted.is_empty() {
            return;
        }
        // The Merks in use by their roots: a pointer that is left names
        // one of them, whose entry says another host or none, or nothing
        // the directory knows.
        let roots: HashMap<CellAddress, &Merk> = (entries.merks.iter())
            .filter(|merk| merk.live)
            .filter_map(|merk| Some((merk.root.as_ref()?.0, merk)))
            .collect();
        let mut left: Vec<_> = hosted.drain().collect();
        left.sort();
        for ((merk, _, position), (host, pointer)) in left {
            let prefix = entries.merks[merk].prefix;
            match roots.get(&pointer) {
                Some(named) => {
                    let other = named.host.is_some();
                    self.add(Rule::HostUnnamed, Place::Merk(named.prefix), || {
                        format!(
                            "the node at {host} of {} hosts the Merk in position {position}, and \
                             its entry names {}",
                            Place::Merk(prefix),
                            if other { "another host" } else { "no host" }
                        )
                    });
                }
                None => self.add(Rule::HostedUnentered, Place::Cell(host), || {
                    format!(
                        "the pointer to {pointer} in position {position}, in a host of {}, has \
                         no entry",
                        Place::Merk(prefix)
                    )
                }),
            }
        }
    }

    /// The cell of the node under `key` in the Merk whose root is at
    /// `root`, found by its key.
    fn find(&mut self, root: Option<CellAddress>, key: &[u8]) -> Option<CellAddress> {
        let mut next = root;
        for _ in 0..MAX_DEPTH {
            self.begin_step();
            let at = next?;
            if self.see(at.page()) != Kind::Tree {
                return None;
            }
            let frame = self.frame(at.page()).ok()?;
            let page = NodePage::open(&frame, self.lease).ok()?;
            let node = page.node(at.slot())?;
            let side = match key.cmp(node.key()) {
                Ordering::Equal => return Some(at),
                Ordering::Less => Side::Left,
                Ordering::Greater => Side::Right,
            };
            next = node.link(side).map(|link| link.child);
        }
        None
    }

    /// Counts the cells that no walk reached, and reports those among them
    /// that name a cell in use.
    pub(super) fn count_unreached(&mut self) {
        let holding: Vec<PageId> = (self.pages.iter())
            .filter(|(_, seen)| matches!(seen, Seen::Tree(page) if page.reached < page.cells))
            .map(|(page, _)| *page)
            .collect();
        for id in holding {
            self.begin_step();
            let Ok(frame) = self.frame(id) else {
                continue;
            };
            let Ok(page) = TreePage::from_validated(&frame, FRAME_FORMAT) else {
                continue;
            };
            let mut any = false;
            for (slot, cell) in page.cells() {
                let at = CellAddress::new(id, slot);
                let reached = match self.pages.get(&id) {
                    Some(Seen::Tree(seen)) => seen.is_reached(slot),
                    _ => true,
                };
                if reached {
                    continue;
                }
                any = true;
                self.unreached.cells += 1;
                self.unreached.bytes += (cell.encoded_len() + TreePage::SLOT_LEN) as u64;
                self.unreached.values += u64::from(cell.value_place().is_some());
                if self.unreached.first.len() < Unreached::SAMPLE {
                    self.unreached.first.push(at);
                }
                let links = (Side::BOTH.into_iter()).filter_map(|side| cell.link(side));
                let named = (links.map(|link| link.child)).chain(cell.hosted_roots().flatten());
                for named in named {
                    let in_use = match self.pages.get(&named.page()) {
                        Some(Seen::Tree(seen)) => seen.is_live(named.slot()),
                        _ => false,
                    };
                    if in_use {
                        self.add(Rule::CellNamedTwice, Place::Cell(at), || {
                            format!("a cell that nothing reaches names {named}, a cell in use")
                        });
                    }
                }
            }
            self.unreached.pages += u64::from(any);
        }
    }
}
