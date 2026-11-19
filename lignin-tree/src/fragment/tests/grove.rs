//! A grove of balanced trees that states its writes to fragment
//! maintenance, as a Merk would, and compares the pages with itself.

use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
};

use lignin_format::tree_page::{Hash, Side};

use super::*;

/// The longest value the grove keeps in its cell.
pub(super) const INLINE_MAX: usize = 256;

#[derive(Debug, Clone)]
pub(super) struct Node {
    pub merk: usize,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub height: u8,
    pub count: u64,
    /// The Merks the node hosts.
    pub hosted: Vec<usize>,
    pub value_hash: Hash,
    pub kv_hash: Hash,
    pub hash: Hash,
    pub feature: Vec<u8>,
    pub aggregate: Vec<u8>,
    pub at: Option<CellAddress>,
    pub alive: bool,
    value_dirty: bool,
    /// The children and the hosted roots the node's cell was last given.
    stored: [Option<usize>; 2],
    stored_hosted: Vec<Option<usize>>,
}

#[derive(Debug, Clone)]
pub(super) struct Merk {
    pub root: Option<usize>,
    pub host: Option<usize>,
    /// Whether its nodes have a feature and aggregates of nine bytes.
    pub wide: bool,
    pub alive: bool,
}

#[derive(Debug)]
pub(super) struct Grove {
    pub mem: Mem,
    pub log: Log,
    pub fragments: Fragments,
    pub rng: Rng,
    pub nodes: Vec<Node>,
    pub merks: Vec<Merk>,
    /// What the pointer to the root of the grove names.
    pub root_at: Option<CellAddress>,
    /// The cells that dropped Merks left in the pages.
    pub ghosts: usize,
    /// Names every child by its address where it has one.
    pub plain_names: bool,
    /// Leaves the pointer to the root of the grove out of the writes.
    pub rootless: bool,
    /// The longest value the grove keeps in its cell.
    pub inline_max: usize,
    by_address: HashMap<CellAddress, usize>,
    dirty: BTreeSet<usize>,
    removed: Vec<usize>,
    orphans: Vec<CellAddress>,
}

fn digest(parts: &[&[u8]]) -> Hash {
    let mut lanes = [
        0x243F_6A88_85A3_08D3u64,
        0x1319_8A2E_0370_7344,
        0xA409_3822_299F_31D0,
        0x082E_FA98_EC4E_6C89,
    ];
    for part in parts {
        for (index, byte) in part.iter().enumerate() {
            let lane = &mut lanes[index % 4];
            *lane = (*lane ^ u64::from(*byte))
                .wrapping_mul(0x0000_0100_0000_01B3)
                .rotate_left(23);
        }
        lanes.rotate_left(1);
        lanes[0] ^= part.len() as u64;
        for lane in &mut lanes {
            *lane = lane.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (*lane >> 29);
        }
    }
    let mut hash = [0; 32];
    for (chunk, lane) in hash.chunks_mut(8).zip(lanes) {
        chunk.copy_from_slice(&lane.to_le_bytes());
    }
    hash
}

/// How a write names `node`: by its address, by its place in the write, or
/// as what the cell names already.
fn name(
    rng: &mut Rng,
    plain: bool,
    nodes: &[Node],
    index_of: &HashMap<usize, usize>,
    node: usize,
    kept: bool,
) -> Target {
    let by_place = index_of.get(&node).map(|index| Target::Node(*index));
    match nodes[node].at {
        None => by_place.expect("a new node is in the write"),
        Some(at) if plain => Target::At(at),
        Some(_) if kept && rng.chance(30) => Target::Kept,
        Some(at) => by_place
            .filter(|_| rng.chance(40))
            .unwrap_or(Target::At(at)),
    }
}

impl Grove {
    pub fn new(layout: Layout, seed: u64) -> Self {
        Self {
            mem: Mem::new(layout.page_size()),
            log: Log::default(),
            fragments: Fragments::new(layout),
            rng: Rng(seed),
            nodes: Vec::new(),
            merks: vec![Merk {
                root: None,
                host: None,
                wide: false,
                alive: true,
            }],
            root_at: None,
            ghosts: 0,
            plain_names: false,
            rootless: false,
            inline_max: INLINE_MAX,
            by_address: HashMap::new(),
            dirty: BTreeSet::new(),
            removed: Vec::new(),
            orphans: Vec::new(),
        }
    }

    fn height(&self, node: Option<usize>) -> u8 {
        node.map_or(0, |node| self.nodes[node].height)
    }

    fn count(&self, node: Option<usize>) -> u64 {
        node.map_or(0, |node| self.nodes[node].count)
    }

    fn child(&self, node: usize, side: Side) -> Option<usize> {
        match side {
            Side::Left => self.nodes[node].left,
            Side::Right => self.nodes[node].right,
        }
    }

    fn set_child(&mut self, node: usize, side: Side, child: Option<usize>) {
        match side {
            Side::Left => self.nodes[node].left = child,
            Side::Right => self.nodes[node].right = child,
        }
    }

    fn fix(&mut self, node: usize) {
        let (left, right) = (self.nodes[node].left, self.nodes[node].right);
        self.nodes[node].height = 1 + self.height(left).max(self.height(right));
        self.nodes[node].count = 1 + self.count(left) + self.count(right);
        self.dirty.insert(node);
    }

    /// Lifts the child on `side` of `node` over it.
    fn rotate(&mut self, node: usize, side: Side) -> usize {
        let other = match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
        let child = self
            .child(node, side)
            .expect("the side that rises has a child");
        let inner = self.child(child, other);
        self.set_child(node, side, inner);
        self.set_child(child, other, Some(node));
        self.fix(node);
        self.fix(child);
        child
    }

    fn balance(&mut self, node: usize) -> usize {
        self.fix(node);
        let (left, right) = (self.nodes[node].left, self.nodes[node].right);
        let lean = i32::from(self.height(right)) - i32::from(self.height(left));
        let heavy = match lean {
            2.. => Side::Right,
            ..=-2 => Side::Left,
            _ => return node,
        };
        let child = self.child(node, heavy).expect("the heavy side has a child");
        let (inner, outer) = match heavy {
            Side::Right => (self.nodes[child].left, self.nodes[child].right),
            Side::Left => (self.nodes[child].right, self.nodes[child].left),
        };
        if self.height(inner) > self.height(outer) {
            let inner_side = match heavy {
                Side::Right => Side::Left,
                Side::Left => Side::Right,
            };
            let risen = self.rotate(child, inner_side);
            self.set_child(node, heavy, Some(risen));
        }
        self.rotate(node, heavy)
    }

    fn new_node(&mut self, merk: usize, key: &[u8], value: &[u8]) -> usize {
        self.nodes.push(Node {
            merk,
            key: key.to_vec(),
            value: value.to_vec(),
            left: None,
            right: None,
            height: 1,
            count: 1,
            hosted: Vec::new(),
            value_hash: [0; 32],
            kv_hash: [0; 32],
            hash: [0; 32],
            feature: Vec::new(),
            aggregate: Vec::new(),
            at: None,
            alive: true,
            value_dirty: true,
            stored: [None; 2],
            stored_hosted: Vec::new(),
        });
        let node = self.nodes.len() - 1;
        self.dirty.insert(node);
        node
    }

    fn insert_at(&mut self, at: Option<usize>, merk: usize, key: &[u8], value: &[u8]) -> usize {
        let Some(node) = at else {
            return self.new_node(merk, key, value);
        };
        match key.cmp(&self.nodes[node].key[..]) {
            Ordering::Less => {
                let left = self.insert_at(self.nodes[node].left, merk, key, value);
                self.nodes[node].left = Some(left);
            }
            Ordering::Greater => {
                let right = self.insert_at(self.nodes[node].right, merk, key, value);
                self.nodes[node].right = Some(right);
            }
            Ordering::Equal => {
                self.nodes[node].value = value.to_vec();
                self.nodes[node].value_dirty = true;
            }
        }
        self.balance(node)
    }

    /// Takes the node with the smallest key out of the tree under `node`.
    fn take_min(&mut self, node: usize) -> (usize, Option<usize>) {
        match self.nodes[node].left {
            None => (node, self.nodes[node].right),
            Some(left) => {
                let (min, rest) = self.take_min(left);
                self.nodes[node].left = rest;
                (min, Some(self.balance(node)))
            }
        }
    }

    fn delete_at(&mut self, at: Option<usize>, key: &[u8]) -> Option<usize> {
        let node = at?;
        match key.cmp(&self.nodes[node].key[..]) {
            Ordering::Less => {
                let left = self.delete_at(self.nodes[node].left, key);
                self.nodes[node].left = left;
            }
            Ordering::Greater => {
                let right = self.delete_at(self.nodes[node].right, key);
                self.nodes[node].right = right;
            }
            Ordering::Equal => {
                self.kill(node);
                return match (self.nodes[node].left, self.nodes[node].right) {
                    (None, other) | (other, None) => other,
                    (Some(left), Some(right)) => {
                        let (min, rest) = self.take_min(right);
                        self.nodes[min].left = Some(left);
                        self.nodes[min].right = rest;
                        Some(self.balance(min))
                    }
                };
            }
        }
        Some(self.balance(node))
    }

    fn kill(&mut self, node: usize) {
        self.nodes[node].alive = false;
        self.dirty.remove(&node);
        self.removed.push(node);
        for merk in self.nodes[node].hosted.clone() {
            self.merks[merk].alive = false;
            self.empty(merk);
        }
    }

    /// Turns the nodes of a Merk, and of the Merks inside it, into cells
    /// that nothing points at.
    fn empty(&mut self, merk: usize) {
        let Some(root) = self.merks[merk].root.take() else {
            return;
        };
        self.orphans.extend(self.nodes[root].at);
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            let entry = &mut self.nodes[node];
            entry.alive = false;
            self.ghosts += 1;
            let at = entry.at.expect("a node of an earlier write has a cell");
            self.by_address.remove(&at);
            pending.extend(entry.left.into_iter().chain(entry.right));
            for hosted in self.nodes[node].hosted.clone() {
                self.merks[hosted].alive = false;
                if let Some(root) = self.merks[hosted].root.take() {
                    pending.push(root);
                }
            }
        }
    }

    pub fn find(&self, merk: usize, key: &[u8]) -> Option<usize> {
        let mut at = self.merks[merk].root;
        while let Some(node) = at {
            at = match key.cmp(&self.nodes[node].key[..]) {
                Ordering::Less => self.nodes[node].left,
                Ordering::Greater => self.nodes[node].right,
                Ordering::Equal => return Some(node),
            };
        }
        None
    }

    /// Marks the nodes from the root of `merk` down to `key` as written.
    fn mark_path(&mut self, merk: usize, key: &[u8]) {
        let mut at = self.merks[merk].root;
        while let Some(node) = at {
            self.dirty.insert(node);
            at = match key.cmp(&self.nodes[node].key[..]) {
                Ordering::Less => self.nodes[node].left,
                Ordering::Greater => self.nodes[node].right,
                Ordering::Equal => None,
            };
        }
    }

    fn rehash(&mut self, node: usize) {
        let (left, right) = (self.nodes[node].left, self.nodes[node].right);
        for child in [left, right].into_iter().flatten() {
            if self.dirty.contains(&child) {
                self.rehash(child);
            }
        }
        let zero = [0; 32];
        let hash_of = |node: Option<usize>| node.map_or(zero, |node| self.nodes[node].hash);
        let entry = &self.nodes[node];
        let mut hashed: Vec<Hash> = Vec::new();
        for merk in &entry.hosted {
            hashed.push(hash_of(self.merks[*merk].root));
        }
        let mut parts: Vec<&[u8]> = vec![&entry.value];
        parts.extend(hashed.iter().map(|hash| &hash[..]));
        let value_hash = digest(&parts);
        let kv_hash = digest(&[&entry.key, &value_hash]);
        let hash = digest(&[&kv_hash, &hash_of(left), &hash_of(right)]);
        let wide = self.merks[entry.merk].wide;
        let tagged = |number: u64| match wide {
            true => [&[3][..], &number.to_le_bytes()].concat(),
            false => vec![0],
        };
        let feature = tagged(entry.value.len() as u64);
        let aggregate = tagged(entry.count);
        let entry = &mut self.nodes[node];
        entry.value_hash = value_hash;
        entry.kv_hash = kv_hash;
        entry.hash = hash;
        entry.feature = feature;
        entry.aggregate = aggregate;
    }

    /// Puts `value` under `key` in `merk`.
    pub fn put(&mut self, merk: usize, key: &[u8], value: &[u8]) -> Report {
        let root = self.insert_at(self.merks[merk].root, merk, key, value);
        self.merks[merk].root = Some(root);
        self.flush(merk)
    }

    /// Deletes `key` from `merk`, with every Merk its node hosts.
    pub fn delete(&mut self, merk: usize, key: &[u8]) -> Report {
        self.merks[merk].root = self.delete_at(self.merks[merk].root, key);
        self.flush(merk)
    }

    /// Writes the nodes from `key` up to the root of the grove again, as
    /// they are.
    pub fn touch(&mut self, merk: usize, key: &[u8]) -> Report {
        self.mark_path(merk, key);
        self.flush(merk)
    }

    /// Makes the node under `key` host one more Merk, and returns it.
    pub fn host(&mut self, merk: usize, key: &[u8], wide: bool) -> usize {
        let node = self.find(merk, key).expect("the key is in the Merk");
        self.merks.push(Merk {
            root: None,
            host: Some(node),
            wide,
            alive: true,
        });
        let hosted = self.merks.len() - 1;
        self.nodes[node].hosted.push(hosted);
        self.touch(merk, key);
        hosted
    }

    /// Clears a hosted Merk: its host keeps it, with no root.
    pub fn clear(&mut self, merk: usize) -> Report {
        let host = self.merks[merk].host.expect("a hosted Merk");
        self.empty(merk);
        let (above, key) = (self.nodes[host].merk, self.nodes[host].key.clone());
        self.touch(above, &key)
    }

    /// Deletes one key of `merk` and puts another in one write.
    pub fn delete_and_put(&mut self, merk: usize, gone: &[u8], key: &[u8], value: &[u8]) -> Report {
        self.merks[merk].root = self.delete_at(self.merks[merk].root, gone);
        let root = self.insert_at(self.merks[merk].root, merk, key, value);
        self.merks[merk].root = Some(root);
        self.flush(merk)
    }

    /// Gives the node under `key` another value in a write that holds no
    /// other node and no root pointer, and leaves its hashes as they are. A
    /// write that is refused leaves the grove as it was.
    pub fn alone(
        &mut self,
        merk: usize,
        key: &[u8],
        value: &[u8],
    ) -> Result<Report, FragmentError> {
        let node = self.find(merk, key).expect("the key is in the Merk");
        let old = std::mem::replace(&mut self.nodes[node].value, value.to_vec());
        self.nodes[node].value_dirty = true;
        self.dirty.insert(node);
        let rootless = std::mem::replace(&mut self.rootless, true);
        let report = self.apply();
        self.rootless = rootless;
        if report.is_err() {
            self.nodes[node].value = old;
            self.nodes[node].value_dirty = false;
            self.dirty.clear();
        }
        report
    }

    fn flush(&mut self, merk: usize) -> Report {
        self.prepare(merk);
        self.apply().expect("the write is applied")
    }

    /// Brings the hashes up to date from `merk` to the root of the grove,
    /// and marks every node on the way as written.
    pub fn prepare(&mut self, merk: usize) {
        let mut at = merk;
        loop {
            if let Some(root) = self.merks[at].root {
                self.rehash(root);
            }
            let Some(host) = self.merks[at].host else {
                break;
            };
            at = self.nodes[host].merk;
            let key = self.nodes[host].key.clone();
            self.mark_path(at, &key);
        }
    }

    /// States the nodes that were written since the last write, and applies
    /// the write.
    pub fn apply(&mut self) -> Result<Report, FragmentError> {
        struct Parts {
            links: [Option<Target>; 2],
            hosted: Option<Vec<Option<Target>>>,
            roots: Vec<Option<usize>>,
            pending: Option<PendingValue>,
        }
        let written: Vec<usize> = self.dirty.iter().copied().collect();
        let index_of: HashMap<usize, usize> = written
            .iter()
            .enumerate()
            .map(|(index, node)| (*node, index))
            .collect();
        let (rng, plain, nodes) = (&mut self.rng, self.plain_names, &self.nodes);
        let height = |node: Option<usize>| node.map_or(0, |node| nodes[node].height);
        let mut parts = Vec::new();
        for &id in &written {
            let node = &nodes[id];
            let mut links = [None; 2];
            for (side, child) in [node.left, node.right].into_iter().enumerate() {
                let kept = child.is_some() && node.stored[side] == child;
                links[side] = child.map(|child| name(rng, plain, nodes, &index_of, child, kept));
            }
            let roots: Vec<Option<usize>> = node
                .hosted
                .iter()
                .map(|merk| self.merks[*merk].root)
                .collect();
            let keep = node.at.is_some() && roots == node.stored_hosted && !plain && rng.chance(50);
            let mut hosted = (!keep).then(Vec::new);
            for (position, root) in roots.iter().enumerate() {
                let Some(named) = &mut hosted else { break };
                let kept = root.is_some() && node.stored_hosted.get(position) == Some(root);
                named.push(root.map(|root| name(rng, plain, nodes, &index_of, root, kept)));
            }
            let out_of_line = node.value_dirty && node.value.len() > self.inline_max;
            parts.push(Parts {
                links,
                hosted,
                roots,
                pending: out_of_line.then(|| self.log.pending(&node.value)),
            });
        }
        let node_writes: Vec<NodeWrite<'_>> = written
            .iter()
            .zip(&parts)
            .map(|(id, parts)| {
                let node = &nodes[*id];
                let link = |side: usize, child: Option<usize>| {
                    let below = &nodes[child?];
                    Some(LinkWrite {
                        child: parts.links[side]?,
                        hash: &below.hash,
                        heights: (height(below.left), height(below.right)),
                        child_key_len: below.key.len() as u8,
                        aggregate: &below.aggregate,
                    })
                };
                NodeWrite {
                    at: node.at,
                    key: &node.key,
                    feature: &node.feature,
                    value_hash: &node.value_hash,
                    kv_hash: &node.kv_hash,
                    left: link(0, node.left),
                    right: link(1, node.right),
                    hosted: match &parts.hosted {
                        Some(roots) => HostedWrite::Set(roots),
                        None => HostedWrite::Keep,
                    },
                    value: match (parts.pending, node.value_dirty) {
                        (Some(pending), _) => ValueWrite::Pending(pending),
                        (None, true) => ValueWrite::Inline(&node.value),
                        (None, false) => ValueWrite::Keep,
                    },
                }
            })
            .collect();
        let removed: Vec<CellAddress> = self
            .removed
            .iter()
            .filter_map(|node| nodes[*node].at)
            .collect();
        let root = self.merks[0].root;
        let roots = [RootWrite {
            was: self.root_at,
            now: root.map(|root| {
                let kept = self.root_at.is_some() && nodes[root].at == self.root_at;
                name(rng, plain, nodes, &index_of, root, kept)
            }),
        }];
        let write = Write {
            nodes: &node_writes,
            removed: &removed,
            roots: match self.rootless {
                true => &[],
                false => &roots,
            },
        };
        let from = self.mem.shadow.records.len();
        let pages_before = self.mem.frames.len();
        let report = self.fragments.apply(&mut self.mem, &mut self.log, &write)?;
        assert_eq!(
            self.mem.frames.len() + report.freed.len(),
            pages_before + report.created.len(),
            "the report names the pages that came and went: {report:?}"
        );

        assert_eq!(report.nodes.len(), written.len());
        assert_eq!(report.carries.total().cells, report.carried.len() as u64);
        let created = written
            .iter()
            .filter(|node| nodes[**node].at.is_none())
            .count();
        let arrived = created + report.carried.len();
        assert_eq!(
            self.mem.count(from, Rec::Insert),
            arrived,
            "a cell arrives once"
        );
        let left = removed.len() + report.carried.len();
        assert_eq!(
            self.mem.count(from, Rec::Remove),
            left,
            "a cell leaves once"
        );
        assert_eq!(self.mem.count(from, Rec::Replace), 0);
        for address in &removed {
            self.by_address.remove(address);
        }
        let mut moved = Vec::new();
        for carried in &report.carried {
            assert_ne!(
                carried.from.page(),
                carried.to.page(),
                "a carry changes the page"
            );
            let node = match carried.node {
                Some(index) => written[index],
                None => self.by_address[&carried.from],
            };
            assert_eq!(self.nodes[node].at, Some(carried.from));
            moved.push((node, carried.to.address()));
        }
        moved.extend(
            written
                .iter()
                .zip(&report.nodes)
                .map(|(node, handle)| (*node, handle.address())),
        );
        for (node, _) in &moved {
            if let Some(old) = self.nodes[*node].at.take() {
                self.by_address.remove(&old);
            }
        }
        for (node, at) in moved {
            self.nodes[node].at = Some(at);
            assert!(self
                .by_address
                .insert(at, node)
                .is_none_or(|other| other == node));
        }
        for (id, parts) in written.iter().zip(parts) {
            let node = &mut self.nodes[*id];
            node.value_dirty = false;
            node.stored = [node.left, node.right];
            node.stored_hosted = parts.roots;
        }
        if let Some(named) = report.roots.first() {
            self.root_at = *named;
        }
        assert_eq!(self.root_at, root.and_then(|root| self.nodes[root].at));
        let mut orphans = std::mem::take(&mut self.orphans);
        let mut reported = report.orphaned.clone();
        orphans.sort_unstable();
        reported.sort_unstable();
        assert_eq!(reported, orphans, "the cells nothing points at any more");
        self.dirty.clear();
        self.removed.clear();

        let mut changed: Vec<PageId> = self.mem.shadow.records[from..]
            .iter()
            .map(|(page, _)| *page)
            .collect();
        changed.sort_unstable();
        changed.dedup();
        for id in changed {
            if self.mem.frames.contains_key(&id) {
                self.check_page(id);
            }
        }
        for handle in &report.nodes {
            let view = self.mem.view(handle.page());
            assert!(
                view.resolve(*handle).is_some(),
                "the report's handles are good"
            );
        }
        Ok(report)
    }

    fn check_page(&self, id: PageId) {
        let view = self.mem.view(id);
        let page = view.tree_page();
        page.check(self.mem.size)
            .expect("a valid page in its frame");
        page.check_fragments()
            .expect("the marks are what the links say");
        assert!(!view.is_empty(), "a page with no cell is freed");
        let shadow = NodePage::open(&self.mem.shadow.frames[&id], self.mem.lease).expect("a page");
        assert_eq!(
            shadow.tree_page().to_image(),
            page.to_image(),
            "the records of {id} give the page"
        );
        if let Some(hint) = self.fragments.hints().used(id) {
            assert_eq!(hint, self.mem.used(id), "the hint of {id}");
        }
    }

    /// Compares the pages with the grove: every node is reached from the
    /// root by the addresses the pages store, with its key, its value, its
    /// hashes and its children, and the pages hold nothing else but what
    /// dropped Merks left.
    pub fn check(&self) {
        let (mut cells, mut pointers, mut pending) = (0, 0, 0);
        for &id in self.mem.frames.keys() {
            self.check_page(id);
            cells += self.mem.view(id).len();
            for (_, cell) in self.mem.view(id).tree_page().cells() {
                if cell.value_place().is_some() {
                    self.log.read(cell.value(), id);
                }
                pointers += usize::from(cell.value_pointer().is_some());
                pending += usize::from(cell.pending_value().is_some());
            }
        }
        assert_eq!(self.mem.shadow.frames.len(), self.mem.frames.len());
        let live = self
            .log
            .logged
            .values()
            .filter(|logged| logged.live)
            .count();
        assert_eq!(pointers, live, "each live value has one owner");
        assert_eq!(
            pending,
            self.log.pending.len(),
            "each pending value has one owner"
        );

        let mut reached = 0;
        match (self.merks[0].root, self.root_at) {
            (Some(root), Some(at)) => self.walk(root, at, None, &mut reached),
            (None, None) => {}
            other => panic!("the root pointer and the grove disagree: {other:?}"),
        }
        let alive = self.nodes.iter().filter(|node| node.alive).count();
        assert_eq!(reached, alive, "every node is reached from the root");
        assert_eq!(cells, alive + self.ghosts, "one node, one cell");
    }

    fn walk(&self, id: usize, at: CellAddress, above: Option<PageId>, reached: &mut usize) {
        let node = &self.nodes[id];
        assert_eq!(node.at, Some(at), "the write reported where the node is");
        let view = self.mem.view(at.page());
        let cell = view.node(at.slot()).expect("a stored address names a cell");
        assert_eq!(cell.key(), node.key);
        assert_eq!(cell.feature(), node.feature);
        assert_eq!(cell.value_hash(), &node.value_hash);
        assert_eq!(cell.kv_hash(), &node.kv_hash);
        assert_eq!(self.log.read(cell.value(), at.page()), node.value);
        assert_eq!(
            cell.value().is_inline(),
            node.value.len() <= self.inline_max
        );
        assert_eq!(cell.is_fragment_root(), above != Some(at.page()));
        *reached += 1;
        for (side, child) in Side::BOTH.into_iter().zip([node.left, node.right]) {
            match (cell.link(side), child) {
                (None, None) => {}
                (Some(link), Some(child)) => {
                    let below = &self.nodes[child];
                    assert_eq!(link.hash, &below.hash);
                    assert_eq!(
                        link.heights,
                        (self.height(below.left), self.height(below.right))
                    );
                    assert_eq!(usize::from(link.child_key_len), below.key.len());
                    assert_eq!(link.aggregate, below.aggregate);
                    self.walk(child, link.child, Some(at.page()), reached);
                }
                other => panic!("the {side} child of {at}: {other:?}"),
            }
        }
        let roots: Vec<Option<CellAddress>> = cell.hosted_roots().collect();
        assert_eq!(roots.len(), node.hosted.len());
        for (root, merk) in roots.into_iter().zip(&node.hosted) {
            match (root, self.merks[*merk].root) {
                (None, None) => {}
                (Some(root_at), Some(root)) => self.walk(root, root_at, Some(at.page()), reached),
                other => panic!("a hosted root of {at}: {other:?}"),
            }
        }
    }

    /// The logical tree as the pages alone give it, with no address in it:
    /// every node's key, value, feature and hashes, and what hangs where.
    pub fn logical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pending = Vec::from_iter(self.root_at);
        while let Some(at) = pending.pop() {
            let view = self.mem.view(at.page());
            let cell = view.node(at.slot()).expect("a stored address names a cell");
            let value = self.log.read(cell.value(), at.page());
            for part in [cell.key(), &value, cell.feature()] {
                out.extend_from_slice(&(part.len() as u32).to_le_bytes());
                out.extend_from_slice(part);
            }
            out.extend_from_slice(cell.value_hash());
            out.extend_from_slice(cell.kv_hash());
            for side in Side::BOTH {
                out.push(u8::from(cell.has_link(side)));
                if let Some(link) = cell.link(side) {
                    out.extend_from_slice(link.hash);
                    out.extend_from_slice(&[link.heights.0, link.heights.1, link.child_key_len]);
                    out.extend_from_slice(link.aggregate);
                    pending.push(link.child);
                }
            }
            for root in cell.hosted_roots() {
                out.push(u8::from(root.is_some()));
                pending.extend(root);
            }
        }
        out
    }

    pub fn node(&self, merk: usize, key: &[u8]) -> &Node {
        &self.nodes[self.find(merk, key).expect("the key is in the Merk")]
    }

    /// The page the node under `key` is in.
    pub fn page_of(&self, merk: usize, key: &[u8]) -> PageId {
        self.node(merk, key).at.expect("the node has a cell").page()
    }

    /// The bytes of the largest fragment that does not hold the root of
    /// the grove, with its slots.
    pub fn largest_fragment(&self) -> usize {
        let mut largest = 0;
        for &id in self.mem.frames.keys() {
            let view = self.mem.view(id);
            for root in view.nodes().filter(|node| node.is_fragment_root()) {
                if Some(root.address()) == self.root_at {
                    continue;
                }
                let (mut bytes, mut pending) = (0, vec![root]);
                while let Some(node) = pending.pop() {
                    bytes += node.cell().encoded_len() + 2;
                    let links = Side::BOTH.into_iter().filter_map(|side| node.link(side));
                    let below = links
                        .map(|link| link.child)
                        .chain(node.hosted_roots().flatten());
                    let here = below.filter(|child| child.page() == id);
                    pending.extend(here.filter_map(|child| view.node(child.slot())));
                }
                largest = largest.max(bytes);
            }
        }
        largest
    }

    /// The share of the pages' bytes that cells, slots and headers take.
    pub fn fill(&self) -> f64 {
        let header = lignin_format::tree_page::TreePage::HEADER_LEN;
        let used: usize = self
            .mem
            .frames
            .keys()
            .map(|page| self.mem.used(*page) + header)
            .sum();
        used as f64 / (self.mem.frames.len() * self.mem.size.get()) as f64
    }
}
