//! A grove of balanced trees that writes through fragment maintenance, with
//! its hashes made or left to be evaluated, and that knows from its own
//! nodes what every stored hash has to be.

use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
};

use super::*;
use crate::fragment::{
    Fragments, HostedWrite, Layout, LinkWrite, NodeWrite, RootWrite, Target, ValueWrite,
};

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
    pub alive: bool,
}

/// What the hashes of a node are by the rules of the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Expected {
    pub value_hash: Hash,
    pub kv_hash: Hash,
    pub hash: Hash,
}

#[derive(Debug)]
pub(super) struct Grove {
    pub mem: Mem,
    pub log: Log,
    pub fragments: Fragments,
    pub hashes: Hashes,
    pub rules: Counting,
    pub nodes: Vec<Node>,
    pub merks: Vec<Merk>,
    /// What the pointer to the root of the grove names.
    pub root_at: Option<CellAddress>,
    /// The longest value the grove keeps in its cell.
    pub inline_max: usize,
    /// Whether a write leaves its hashes to be evaluated. Otherwise it
    /// evaluates what is marked, makes its hashes and states them.
    pub lazy: bool,
    /// The cells that dropped Merks left in the pages.
    pub ghosts: BTreeSet<CellAddress>,
    /// How many cells were carried with a mark.
    pub carried_marked: usize,
    /// How many marks went with a removed cell or a removed link.
    pub marks_dropped: usize,
    /// How many hash records the writes made that stated their hashes.
    pub eager_hash_records: usize,
    /// The directory entries of the Merks whose host is behind.
    pub entries: HashMap<usize, Option<CellAddress>>,
    /// The Merk whose root the next write names as a root that stays, with
    /// the address its directory entry has.
    stopped: Option<(usize, Option<CellAddress>)>,
    /// The hosts whose element the next write rewrites.
    stating: BTreeSet<usize>,
    by_address: HashMap<CellAddress, usize>,
    touched: BTreeSet<usize>,
    removed: Vec<usize>,
    orphans: Vec<CellAddress>,
}

fn tagged(number: u64) -> Vec<u8> {
    [&[3][..], &number.to_le_bytes()].concat()
}

impl Grove {
    pub fn new(lazy: bool) -> Self {
        Self {
            mem: Mem::new(SIZE),
            log: Log::default(),
            fragments: Fragments::new(Layout::proposed(SIZE)),
            hashes: Hashes::new(),
            rules: Counting::default(),
            nodes: Vec::new(),
            merks: vec![Merk {
                root: None,
                host: None,
                alive: true,
            }],
            root_at: None,
            inline_max: INLINE_MAX,
            lazy,
            ghosts: BTreeSet::new(),
            carried_marked: 0,
            marks_dropped: 0,
            eager_hash_records: 0,
            entries: HashMap::new(),
            stopped: None,
            stating: BTreeSet::new(),
            by_address: HashMap::new(),
            touched: BTreeSet::new(),
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
        self.touched.insert(node);
    }

    /// Lifts the child on `side` of `node` over it.
    fn rotate(&mut self, node: usize, side: Side) -> usize {
        let other = match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
        let child = self.child(node, side).expect("the side that rises");
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
        let (heavy, light) = match lean {
            2.. => (Side::Right, Side::Left),
            ..=-2 => (Side::Left, Side::Right),
            _ => return node,
        };
        let child = self.child(node, heavy).expect("the heavy side");
        if self.height(self.child(child, light)) > self.height(self.child(child, heavy)) {
            let risen = self.rotate(child, light);
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
            at: None,
            alive: true,
            value_dirty: true,
            stored: [None; 2],
            stored_hosted: Vec::new(),
        });
        let node = self.nodes.len() - 1;
        self.touched.insert(node);
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
        self.touched.remove(&node);
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
            let at = entry.at.expect("a node of an earlier write has a cell");
            self.ghosts.insert(at);
            self.by_address.remove(&at);
            pending.extend(entry.left.into_iter().chain(entry.right));
            for hosted in self.nodes[node].hosted.clone() {
                self.merks[hosted].alive = false;
                pending.extend(self.merks[hosted].root.take());
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

    /// The nodes from the root of `merk` down to `key`.
    fn down_to(&self, merk: usize, key: &[u8]) -> Vec<usize> {
        let mut path = Vec::new();
        let mut at = self.merks[merk].root;
        while let Some(node) = at {
            path.push(node);
            at = match key.cmp(&self.nodes[node].key[..]) {
                Ordering::Less => self.nodes[node].left,
                Ordering::Greater => self.nodes[node].right,
                Ordering::Equal => None,
            };
        }
        path
    }

    /// The nodes on the way from the node under `key` up to the root of the
    /// grove, with that node first: its ancestors in its Merk, the host of
    /// the Merk, the host's ancestors, and so on.
    pub fn root_path(&self, merk: usize, key: &[u8]) -> Vec<usize> {
        let (mut path, mut merk, mut key) = (Vec::new(), merk, key.to_vec());
        loop {
            path.extend(self.down_to(merk, &key).into_iter().rev());
            let Some(host) = self.merks[merk].host else {
                return path;
            };
            merk = self.nodes[host].merk;
            key = self.nodes[host].key.clone();
        }
    }

    pub fn node(&self, merk: usize, key: &[u8]) -> &Node {
        &self.nodes[self.find(merk, key).expect("the key is in the Merk")]
    }

    pub fn at(&self, merk: usize, key: &[u8]) -> CellAddress {
        self.node(merk, key).at.expect("the node has a cell")
    }

    /// The address of the root of `merk`.
    pub fn root_of(&self, merk: usize) -> CellAddress {
        let root = self.merks[merk].root.expect("the Merk has a root");
        self.nodes[root].at.expect("the root has a cell")
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

    /// Makes the node under `key` host one more Merk, and returns it.
    pub fn host(&mut self, merk: usize, key: &[u8]) -> usize {
        let node = self.find(merk, key).expect("the key is in the Merk");
        self.merks.push(Merk {
            root: None,
            host: Some(node),
            alive: true,
        });
        let hosted = self.merks.len() - 1;
        self.nodes[node].hosted.push(hosted);
        self.touched.extend(self.down_to(merk, key));
        self.stating.insert(node);
        self.flush(merk);
        hosted
    }

    /// Puts `value` under `key` in the hosted `merk`, in a write that stops
    /// at the Merk's root (`tree-layout.md` T3): it holds no node above it.
    pub fn put_stopped(&mut self, merk: usize, key: &[u8], value: &[u8]) -> Report {
        let at = self.merks[merk].root.and_then(|root| self.nodes[root].at);
        let was = self.entries.get(&merk).copied().unwrap_or(at);
        self.stopped = Some((merk, was));
        let root = self.insert_at(self.merks[merk].root, merk, key, value);
        self.merks[merk].root = Some(root);
        let report = self.apply();
        self.stopped = None;
        self.entries.insert(merk, report.roots[0]);
        let host = self.merks[merk].host.expect("a hosted Merk");
        self.mem.behind.extend(self.nodes[host].at);
        report
    }

    /// Writes the host of `merk`, and all above it, with the root the Merk
    /// has now. The host is behind until that write is applied.
    pub fn finalize(&mut self, merk: usize) -> Report {
        let report = self.flush(merk);
        assert!(!self.entries.contains_key(&merk));
        report
    }

    /// Writes what was done to `merk`, with every node from there up to the
    /// root of the grove.
    fn flush(&mut self, merk: usize) -> Report {
        let mut at = merk;
        while let Some(host) = self.merks[at].host {
            self.stating.insert(host);
            at = self.nodes[host].merk;
            let key = self.nodes[host].key.clone();
            self.touched.extend(self.down_to(at, &key));
        }
        self.apply()
    }

    fn feature(&self, node: usize) -> Vec<u8> {
        tagged(self.nodes[node].value.len() as u64)
    }

    fn aggregate(&self, node: usize) -> Vec<u8> {
        tagged(self.nodes[node].count)
    }

    fn expect(&self, node: usize, memo: &mut HashMap<usize, Expected>) -> Expected {
        if let Some(known) = memo.get(&node) {
            return *known;
        }
        let entry = &self.nodes[node];
        let mut hosted = Vec::new();
        for merk in &entry.hosted {
            let root = self.merks[*merk].root;
            hosted.push(root.map(|root| self.expect(root, memo).hash));
        }
        let value_hash = value_rule(&entry.value, &hosted);
        let kv_hash = kv_rule(&entry.key, &value_hash);
        let mut below = |child: Option<usize>| {
            let child = child?;
            Some((self.expect(child, memo).hash, self.aggregate(child)))
        };
        let (left, right) = (below(entry.left), below(entry.right));
        let hash = node_rule(
            &kv_hash,
            &self.feature(node),
            left.as_ref()
                .map(|(hash, aggregate)| (hash, &aggregate[..])),
            right
                .as_ref()
                .map(|(hash, aggregate)| (hash, &aggregate[..])),
        );
        let expected = Expected {
            value_hash,
            kv_hash,
            hash,
        };
        memo.insert(node, expected);
        expected
    }

    /// What the hashes of the node under `key` have to be.
    pub fn expected(&self, merk: usize, key: &[u8]) -> Expected {
        let node = self.find(merk, key).expect("the key is in the Merk");
        self.expect(node, &mut HashMap::new())
    }

    /// What the root hash of the grove has to be.
    pub fn expected_root(&self) -> Hash {
        let root = self.merks[0].root.expect("the grove has a root");
        self.expect(root, &mut HashMap::new()).hash
    }

    /// States the nodes that were touched since the last write, and applies
    /// the write.
    pub fn apply(&mut self) -> Report {
        struct Link {
            child: Target,
            hash: Hash,
            heights: (u8, u8),
            key_len: u8,
            aggregate: Vec<u8>,
        }
        struct Parts {
            value_hash: Hash,
            kv_hash: Hash,
            feature: Vec<u8>,
            links: [Option<Link>; 2],
            hosted: Option<Vec<Option<Target>>>,
            roots: Option<Vec<Option<usize>>>,
            pending: Option<PendingValue>,
            marks: Stale,
        }
        if !self.lazy {
            self.evaluate_all();
        }
        let stating = std::mem::take(&mut self.stating);
        let written: Vec<usize> = self.touched.iter().copied().collect();
        let index_of: HashMap<usize, usize> = written
            .iter()
            .enumerate()
            .map(|(index, node)| (*node, index))
            .collect();
        let mut memo = HashMap::new();
        let name = |node: usize| match self.nodes[node].at {
            Some(at) => Target::At(at),
            None => Target::Node(index_of[&node]),
        };
        let mut parts = Vec::new();
        for &id in &written {
            let node = &self.nodes[id];
            let cell = node.at.map(|at| self.mem.node(at));
            let made = (!self.lazy).then(|| self.expect(id, &mut memo));
            let mut marks = Stale::NONE;
            if self.lazy && (node.at.is_none() || node.value_dirty || stating.contains(&id)) {
                marks = marks.with(HashField::Value);
            }
            let mut links = [None, None];
            let sides = Side::BOTH.into_iter().zip([node.left, node.right]);
            for (index, (side, child)) in sides.enumerate() {
                let Some(child) = child else { continue };
                let below = &self.nodes[child];
                let stored = cell.and_then(|cell| cell.link(side)).map(|link| *link.hash);
                let moved = node.stored[index] != Some(child);
                if self.lazy && (moved || self.touched.contains(&child)) {
                    marks = marks.with(HashField::Child(side));
                }
                links[index] = Some(Link {
                    child: name(child),
                    hash: match self.lazy {
                        true => stored.unwrap_or([0; 32]),
                        false => self.expect(child, &mut memo).hash,
                    },
                    heights: (self.height(below.left), self.height(below.right)),
                    key_len: below.key.len() as u8,
                    aggregate: self.aggregate(child),
                });
            }
            let roots: Vec<Option<usize>> = node
                .hosted
                .iter()
                .map(|merk| self.merks[*merk].root)
                .collect();
            // A host that is behind keeps its pointers as they are, unless
            // the write rewrites its element: then it names every root.
            let behind = node
                .hosted
                .iter()
                .any(|merk| self.entries.contains_key(merk));
            let kept = node.hosted.is_empty() || (behind && !stating.contains(&id));
            let named = roots.iter().map(|root| root.map(name)).collect();
            let fresh = node.value_dirty && node.value.len() > self.inline_max;
            parts.push(Parts {
                value_hash: match (made, cell) {
                    (Some(made), _) => made.value_hash,
                    (None, Some(cell)) => *cell.value_hash(),
                    (None, None) => [0; 32],
                },
                kv_hash: match (made, cell) {
                    (Some(made), _) => made.kv_hash,
                    (None, Some(cell)) => *cell.kv_hash(),
                    (None, None) => [0; 32],
                },
                feature: self.feature(id),
                links,
                hosted: (!kept).then_some(named),
                roots: (!kept).then_some(roots),
                pending: fresh.then(|| self.log.pending(&node.value)),
                marks,
            });
        }
        let nodes = &self.nodes;
        let node_writes: Vec<NodeWrite<'_>> = written
            .iter()
            .zip(&parts)
            .map(|(id, parts)| {
                let node = &nodes[*id];
                let link = |side: usize| {
                    let link = parts.links[side].as_ref()?;
                    Some(LinkWrite {
                        child: link.child,
                        hash: &link.hash,
                        heights: link.heights,
                        child_key_len: link.key_len,
                        aggregate: &link.aggregate,
                    })
                };
                NodeWrite {
                    at: node.at,
                    key: &node.key,
                    feature: &parts.feature,
                    value_hash: &parts.value_hash,
                    kv_hash: &parts.kv_hash,
                    left: link(0),
                    right: link(1),
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
        let roots = [match self.stopped {
            Some((merk, was)) => RootWrite {
                was,
                now: self.merks[merk].root.map(name),
                anchored: true,
            },
            None => RootWrite {
                was: self.root_at,
                now: root.map(name),
                anchored: false,
            },
        }];
        let dropped = std::mem::take(&mut self.orphans);
        let write = Write {
            nodes: &node_writes,
            removed: &removed,
            dropped: &dropped,
            roots: &roots,
        };
        let hash_records = self.mem.shadow.hashes.len();
        let report = self
            .fragments
            .apply(&mut self.mem, &mut self.log, &write)
            .expect("the write is applied");
        let stated = self.mem.shadow.hashes.len() - hash_records;
        match self.lazy {
            true => assert_eq!(stated, 0, "a write that leaves its hashes writes none"),
            false => self.eager_hash_records += stated,
        }

        let marked = |at: &CellAddress| !self.hashes.stale(*at).is_empty();
        let carried = report.carried.iter().map(|cell| &cell.from);
        self.carried_marked += carried.filter(|at| marked(at)).count();
        let before: usize = self.hashes.marked().map(|(_, stale)| stale.len()).sum();
        self.hashes.follow(&write, &report);
        let after: usize = self.hashes.marked().map(|(_, stale)| stale.len()).sum();
        self.marks_dropped += before - after;

        for address in &removed {
            self.by_address.remove(address);
        }
        let mut moved = Vec::new();
        for carried in &report.carried {
            let node = match carried.node {
                Some(index) => written[index],
                None => self.by_address[&carried.from],
            };
            assert_eq!(self.nodes[node].at, Some(carried.from));
            moved.push((node, carried.to.address()));
        }
        let handles = written.iter().zip(&report.nodes);
        moved.extend(handles.map(|(node, handle)| (*node, handle.address())));
        for (node, _) in &moved {
            if let Some(old) = self.nodes[*node].at.take() {
                self.by_address.remove(&old);
            }
        }
        for (node, at) in moved {
            self.nodes[node].at = Some(at);
            let other = self.by_address.insert(at, node);
            assert!(other.is_none_or(|other| other == node));
        }
        for (id, parts) in written.iter().zip(parts) {
            let node = &mut self.nodes[*id];
            node.value_dirty = false;
            node.stored = [node.left, node.right];
            if let Some(roots) = parts.roots {
                node.stored_hosted = roots;
            }
            let at = node.at.expect("a written node has a cell");
            for field in parts.marks.fields() {
                self.hashes.mark(at, field);
            }
            if stating.contains(id) {
                for merk in &node.hosted {
                    self.entries.remove(merk);
                }
            }
        }
        let merks = &self.merks;
        self.entries.retain(|merk, _| merks[*merk].alive);
        let hosts = self.entries.keys().filter_map(|merk| merks[*merk].host);
        self.mem.behind = hosts.filter_map(|host| self.nodes[host].at).collect();
        if self.stopped.is_none() {
            self.root_at = report.roots[0];
        }
        self.touched.clear();
        self.removed.clear();
        self.mem.check_records();
        report
    }

    /// The root hash of the grove, with whatever is marked evaluated.
    pub fn root_hash(&mut self) -> Hash {
        let root = self.root_at.expect("the grove has a root");
        self.hash_of(root)
    }

    pub fn hash_of(&mut self, at: CellAddress) -> Hash {
        let (mem, log, rules) = (&mut self.mem, &mut self.log, &mut self.rules);
        let hash = self.hashes.hash_of(mem, log, rules, at);
        hash.expect("the hash is made")
    }

    pub fn evaluate(&mut self, at: CellAddress) {
        let (mem, log, rules) = (&mut self.mem, &mut self.log, &mut self.rules);
        let evaluated = self.hashes.evaluate(mem, log, rules, at);
        evaluated.expect("the node is evaluated");
    }

    pub fn evaluate_all(&mut self) {
        let (mem, log, rules) = (&mut self.mem, &mut self.log, &mut self.rules);
        let evaluated = self.hashes.evaluate_all(mem, log, rules);
        evaluated.expect("every mark is evaluated");
        assert!(self.hashes.is_empty());
    }

    /// What [`check`] says of the cell at `at`.
    pub fn differing(&mut self, at: CellAddress) -> Stale {
        let differing = check(&mut self.mem, &mut self.log, &mut Plain, at);
        differing.expect("the cell is recomputed")
    }

    /// The nodes that are alive and have a cell, with their addresses.
    pub fn cells(&self) -> Vec<(usize, CellAddress)> {
        let alive = self.nodes.iter().enumerate().filter(|(_, node)| node.alive);
        alive
            .filter_map(|(id, node)| Some((id, node.at?)))
            .collect()
    }

    /// The marks are complete: what differs from its recomputation is
    /// marked, and a mark names a node that is alive or a cell that a
    /// dropped Merk left.
    pub fn check_marks(&mut self) {
        let behind = self.mem.behind.clone();
        for (id, at) in self.cells() {
            assert_eq!(self.by_address.get(&at), Some(&id));
            if behind.contains(&at) {
                continue;
            }
            let (differing, stale) = (self.differing(at), self.hashes.stale(at));
            assert!(
                stale.contains(differing),
                "{differing:?} of {at} differ and {stale:?} are marked"
            );
        }
        for (at, _) in self.hashes.marked() {
            let known = self.by_address.contains_key(&at) || self.ghosts.contains(&at);
            assert!(known, "a mark at {at} names no node");
        }
    }

    /// Every stored hash of every node is what the grove says it is, and
    /// equals its recomputation.
    pub fn check_hashes(&mut self) {
        let mut memo = HashMap::new();
        for (id, at) in self.cells() {
            let expected = self.expect(id, &mut memo);
            let cell = self.mem.node(at);
            let node = &self.nodes[id];
            assert_eq!(cell.key(), node.key);
            assert_eq!(self.log.value(cell.value()), node.value);
            assert_eq!(cell.value_hash(), &expected.value_hash, "of {at}");
            assert_eq!(cell.kv_hash(), &expected.kv_hash, "of {at}");
            for (side, child) in Side::BOTH.into_iter().zip([node.left, node.right]) {
                let link = cell.link(side);
                let stored = link.map(|link| (link.child, *link.hash));
                let below = child.map(|child| {
                    let at = self.nodes[child].at.expect("a child has a cell");
                    (at, self.expect(child, &mut memo).hash)
                });
                assert_eq!(stored, below, "the {side} child of {at}");
            }
            assert_eq!(self.differing(at), Stale::NONE, "of {at}");
        }
    }
}
