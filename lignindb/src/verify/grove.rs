//! The walk of the Merks from their roots, the agreement of the directory
//! with the cells, and the cells that nothing reaches.

use std::{
    collections::{HashMap, HashSet},
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
    Grove, Place, Rule, Run, Unreached,
};

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

/// What the walks of the Merks in use learn of the cells that host a Merk.
/// A Merk is the place of its entry in [`Entries::merks`].
///
/// The comparison of the entries with their hosts reads only this, and so
/// says of a cell only what a walk read in it.
#[derive(Default)]
struct Hosts<'e> {
    /// The keys of the hosts that the entries name, by the Merk they are
    /// said to be in.
    wanted: HashMap<usize, HashSet<&'e [u8]>>,
    /// The cells of those hosts that a walk entered.
    cells: HashMap<(usize, &'e [u8]), CellAddress>,
    /// Every pointer to a hosted root in a cell that a walk entered: the
    /// host's cell and the pointer.
    pointers: HashMap<HostedAt, (CellAddress, CellAddress)>,
    /// The Merks whose walk did not enter every cell it was pointed at.
    /// What stopped it was reported, and a node that the walk did not come
    /// to may still be one of the Merk.
    partial: HashSet<usize>,
}

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
        let mut hosts = Hosts::default();
        match (self.store.grove, entries) {
            (Grove::Roots(roots), _) => {
                for root in roots {
                    self.census.merks += 1;
                    self.walk_merk(*root, None, Walk::Rooted, rules, &mut hosts);
                }
            }
            (Grove::Directory { .. }, Some(entries)) => {
                for merk in entries.merks.iter().filter(|merk| merk.live) {
                    if let Some((parent, key, _)) = &merk.host
                        && let Some(parent) = entries.by_prefix.get(parent)
                    {
                        let wanted = hosts.wanted.entry(*parent).or_default();
                        wanted.insert(key.as_slice());
                    }
                }
                for (index, merk) in entries.merks.iter().enumerate() {
                    let Some((root, key)) = merk.root.as_ref().filter(|_| merk.live) else {
                        continue;
                    };
                    let walk = Walk::Entered {
                        merk: index,
                        prefix: &merk.prefix,
                    };
                    self.walk_merk(*root, Some(key), walk, rules, &mut hosts);
                }
                self.compare_hosts(entries, &mut hosts);
                for dropped in &entries.drops {
                    if let Some(root) = dropped.root {
                        self.walk_merk(root, None, Walk::Left, rules, &mut hosts);
                    }
                }
            }
            // The directory was not read, which was reported. No walk
            // begins, and what the cells are reached by is not known.
            (Grove::Directory { .. }, None) => self.cut.cells = true,
        }
    }

    /// Walks the cells below `root`, the root of one Merk.
    ///
    /// A cell that the walk is pointed at and does not enter is reported
    /// where it is not entered, and the walk goes on beside it. What lies
    /// below such a cell is then reached by nothing that was read: a page
    /// that is not read sets [`Cut::cells`](super::Cut), and the Merk is
    /// noted as one whose walk is not whole ([`Hosts::partial`]).
    fn walk_merk<'e>(
        &mut self,
        root: CellAddress,
        root_key: Option<&Vec<u8>>,
        walk: Walk<'_>,
        rules: &mut Option<&mut dyn HashRules>,
        hosts: &mut Hosts<'e>,
    ) {
        let entered = match walk {
            Walk::Entered { merk, .. } => Some(merk),
            Walk::Rooted | Walk::Left => None,
        };
        let wanted = entered.and_then(|merk| hosts.wanted.get(&merk));
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
                Kind::Bad => {
                    self.cut.cells = true;
                    hosts.partial.extend(entered);
                    continue;
                }
                kind => {
                    self.add(dangling, holder, || match kind {
                        Kind::Flat => format!("the address {at} is in a flat page"),
                        _ => format!("the address {at} is in no page of the version"),
                    });
                    hosts.partial.extend(entered);
                    continue;
                }
            }
            let Some(frame) = self.enter(at.page()) else {
                self.cut.cells = true;
                hosts.partial.extend(entered);
                continue;
            };
            let page = match NodePage::open(&frame, self.lease) {
                Ok(page) => page,
                Err(error) => {
                    let place = Place::Page(at.page());
                    self.add(Rule::TreePageFormat, place, || error.to_string());
                    self.cut.cells = true;
                    hosts.partial.extend(entered);
                    continue;
                }
            };
            let Some(node) = page.node(at.slot()) else {
                let what = || format!("the address {at} names a slot that holds no cell");
                self.add(dangling, holder, what);
                hosts.partial.extend(entered);
                continue;
            };
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
            if let Some(stated) = step.key_len
                && usize::from(stated) != key.len()
            {
                let found = key.len();
                self.add(Rule::LinkKeyLength, holder, || {
                    format!(
                        "the link to {at} states a key of {stated} bytes, the child has {found}"
                    )
                });
            }
            let own = heights_of(&node);
            if let Some(stated) = step.heights
                && Some(stated) != own
            {
                self.add(Rule::LinkHeights, holder, || match own {
                    Some(own) => format!(
                        "the link to {at} states the heights {stated:?}, the child's links give \
                         {own:?}"
                    ),
                    None => format!("the links of the child at {at} give no height"),
                });
            }
            // What the link states of the cell was compared for every link
            // that names it. The cell itself is entered once.
            if !self.reach(at, reach) {
                let what = || format!("the address {at} names a cell that was reached before");
                self.add(Rule::CellNamedTwice, holder, what);
                hosts.partial.extend(entered);
                continue;
            }
            if let (Some(merk), Some(wanted)) = (entered, wanted)
                && let Some(key) = wanted.get(key)
            {
                hosts.cells.insert((merk, *key), at);
            }
            // The value of a cell that a dropped Merk left is read as well:
            // the reclaimer releases the pointer it finds there.
            if let Some(value) = node.cell().value_place() {
                self.check_value(value, at.page(), Place::Cell(at));
            }
            if reach == Reach::Live {
                self.check_hashes(&node, rules);
            }
            for (position, pointer) in node.hosted_roots().enumerate() {
                let Some(pointer) = pointer else {
                    continue;
                };
                match walk {
                    Walk::Entered { merk, .. } => {
                        let hosted_at = (merk, key.to_vec(), position as u8);
                        hosts.pointers.insert(hosted_at, (at, pointer));
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

    /// Recomputes the hashes that a cell in use stores.
    fn check_hashes(&mut self, node: &NodeRef<'_>, rules: &mut Option<&mut dyn HashRules>) {
        let at = node.address();
        let place = Place::Cell(at);
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
    /// host, as the walks read it. The pointers of no entry are reported.
    ///
    /// A host that no walk entered is no node of its Merk only where the
    /// walk of that Merk was whole. Where it was not, what stopped it was
    /// reported, and nothing is said of the host.
    fn compare_hosts(&mut self, entries: &Entries, hosts: &mut Hosts<'_>) {
        for merk in entries.merks.iter().filter(|merk| merk.live) {
            let Some((parent, key, position)) = &merk.host else {
                continue;
            };
            let place = Place::Merk(merk.prefix);
            let in_use = (entries.by_prefix.get(parent))
                .copied()
                .filter(|index| entries.merks[*index].live);
            let Some(index) = in_use else {
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
            match (
                root,
                hosts.pointers.remove(&(index, key.clone(), *position)),
            ) {
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
                (root, None) => match (hosts.cells.get(&(index, key.as_slice())), root) {
                    (Some(_), None) => {}
                    (Some(host), Some(root)) => {
                        self.add(Rule::HostPointer, Place::Cell(*host), || {
                            format!(
                                "the host has no pointer in position {position}, and the entry \
                                 of {place} names the root {root}"
                            )
                        });
                    }
                    (None, _) if hosts.partial.contains(&index) => {}
                    (None, _) => {
                        let what = || format!("the host is no node of {}", Place::Merk(*parent));
                        self.add(Rule::EntryHost, place, what);
                    }
                },
            }
        }
        if hosts.pointers.is_empty() {
            return;
        }
        // The Merks in use by their roots: a pointer that is left names
        // one of them, whose entry says another host or none, or nothing
        // the directory knows.
        let roots: HashMap<CellAddress, &Merk> = (entries.merks.iter())
            .filter(|merk| merk.live)
            .filter_map(|merk| Some((merk.root.as_ref()?.0, merk)))
            .collect();
        let mut left: Vec<_> = hosts.pointers.drain().collect();
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

    /// Counts the cells that no walk reached, and reports those among them
    /// that name a cell in use.
    ///
    /// Where a walk was pointed at a page that is not read
    /// ([`Cut::cells`](super::Cut)), the cells below that page are among
    /// those counted here, and one of them that names a cell in use may be
    /// the cell's own parent or host. So nothing is reported of what they
    /// name, and the count is marked ([`Unreached::uncertain`]).
    pub(super) fn count_unreached(&mut self) {
        self.unreached.uncertain = self.cut.cells;
        let holding: Vec<PageId> = (self.pages.iter())
            .filter(|(_, seen)| matches!(seen, Seen::Tree(page) if page.reached < page.cells))
            .map(|(page, _)| *page)
            .collect();
        for id in holding {
            self.begin_step();
            let Some(frame) = self.enter(id) else {
                self.unreached.uncertain = true;
                continue;
            };
            let page = match TreePage::from_validated(&frame, FRAME_FORMAT) {
                Ok(page) => page,
                Err(error) => {
                    self.add(Rule::TreePageFormat, Place::Page(id), || error.to_string());
                    self.unreached.uncertain = true;
                    continue;
                }
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
                if let Some(value) = cell.value_place() {
                    self.unreached.values += 1;
                    self.check_value(value, id, Place::Cell(at));
                }
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
                    if in_use && !self.cut.cells {
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
