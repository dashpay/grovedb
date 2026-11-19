mod errors;
mod load;
mod order;
mod writes;

use std::collections::{BTreeMap, HashMap};

use lignin_format::{
    delta::Delta,
    tree_page::{CellValue, Hash, Side, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePlace},
    CellAddress, FormatVersion, PageId, PageSize,
};
use lignin_store::load::FRAME_FORMAT;

use super::*;
use crate::{
    fragment::{FragmentError, Layout, PageMut, Pages, Values},
    hash::{HashRules, Refusal, ValueReader},
    node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut, NodeRef},
};

const SIZE: PageSize = PageSize::MIN;

/// The longest value of the tests that stays in its cell.
const THRESHOLD: usize = 256;

/// SplitMix64.
#[derive(Debug, Clone)]
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

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// A hash of the tests: every byte of every part, and where each part ends.
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

fn value_rule(value: &[u8], hosted: &[Option<Hash>]) -> Hash {
    let mut parts: Vec<&[u8]> = vec![b"value", value];
    for root in hosted {
        parts.push(match root {
            Some(hash) => hash,
            None => b"no root",
        });
    }
    digest(&parts)
}

fn kv_rule(key: &[u8], value_hash: &Hash) -> Hash {
    digest(&[b"kv", key, value_hash])
}

/// What a link of the tests gives to the hash of its node.
type LinkPart<'a> = Option<(&'a Hash, &'a [u8], (u8, u8), u8)>;

fn node_rule(kv_hash: &Hash, feature: &[u8], left: LinkPart<'_>, right: LinkPart<'_>) -> Hash {
    let mut parts: Vec<&[u8]> = vec![b"node", kv_hash, feature];
    let mut numbers = Vec::new();
    for link in [left, right] {
        match link {
            None => parts.push(b"no child"),
            Some((hash, aggregate, heights, key_len)) => {
                parts.extend([&hash[..], aggregate]);
                numbers.extend([heights.0, heights.1, key_len]);
            }
        }
    }
    parts.push(&numbers);
    digest(&parts)
}

fn node_rule_of(node: NodeRef<'_>) -> Hash {
    let link = |side| {
        let link = node.link(side)?;
        Some((link.hash, link.aggregate, link.heights, link.child_key_len))
    };
    node_rule(
        node.kv_hash(),
        node.feature(),
        link(Side::Left),
        link(Side::Right),
    )
}

/// Which rule was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Rule {
    Value,
    Kv,
    Node,
}

/// The rules of the tests. They count their calls, and refuse the call of
/// one rule for one key.
#[derive(Debug, Default)]
struct Rules {
    calls: HashMap<Rule, usize>,
    refuse: Option<(Rule, Vec<u8>)>,
}

impl Rules {
    fn called(&mut self, rule: Rule, key: &[u8]) -> Result<(), Refusal> {
        if self.refuse.as_ref() == Some(&(rule, key.to_vec())) {
            return Err(format!("no {rule:?} hash for this node").into());
        }
        *self.calls.entry(rule).or_default() += 1;
        Ok(())
    }

    fn count(&self, rule: Rule) -> usize {
        self.calls.get(&rule).copied().unwrap_or_default()
    }
}

impl HashRules for Rules {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.called(Rule::Value, node.key())?;
        assert_eq!(value.len(), node.value_len() as usize, "the whole value");
        assert_eq!(hosted.len(), node.hosted_roots().len());
        Ok(value_rule(value, hosted))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.called(Rule::Kv, key)?;
        Ok(kv_rule(key, value_hash))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.called(Rule::Node, node.key())?;
        Ok(node_rule_of(node))
    }
}

/// The sink of the tests. It makes every record it is told on a copy of the
/// page, and counts the records that are not the insert of a cell.
#[derive(Debug)]
struct Shadow {
    size: PageSize,
    frames: BTreeMap<PageId, Vec<u8>>,
    inserts: usize,
    others: usize,
}

impl ChangeSink for Shadow {
    fn created(&mut self, page: PageId, departures: u64) {
        let mut frame = vec![0; self.size.get()];
        TreePageMut::init(&mut frame, self.size, page, departures).expect("a frame of a page");
        assert!(self.frames.insert(page, frame).is_none(), "started once");
    }

    fn changed(&mut self, page: PageId, change: &Delta<'_>) {
        let frame = self.frames.get_mut(&page).expect("the page was started");
        let mut copy = TreePageMut::open(frame, self.size, FRAME_FORMAT).expect("a tree page");
        change.apply(&mut copy).expect("a record its page took");
        match change {
            Delta::Insert { .. } => self.inserts += 1,
            _ => self.others += 1,
        }
    }
}

/// Tree pages in memory, as one write scope has them.
#[derive(Debug)]
struct Mem {
    size: PageSize,
    lease: Lease,
    frames: BTreeMap<PageId, Vec<u8>>,
    shadow: Shadow,
    next: u32,
    reads: Vec<PageId>,
    /// How many times each page was written.
    writes: BTreeMap<PageId, usize>,
    /// How many more pages can be started.
    creates_left: Option<usize>,
    /// How many more writes the pages take.
    writes_left: Option<usize>,
    /// Starts every page with a cell in it.
    dirty: bool,
}

impl Mem {
    fn new(size: PageSize) -> Self {
        Self {
            size,
            lease: Lease::take(),
            frames: BTreeMap::new(),
            shadow: Shadow {
                size,
                frames: BTreeMap::new(),
                inserts: 0,
                others: 0,
            },
            next: 1,
            reads: Vec::new(),
            writes: BTreeMap::new(),
            creates_left: None,
            writes_left: None,
            dirty: false,
        }
    }

    fn view(&self, page: PageId) -> NodePage<'_> {
        NodePage::open(&self.frames[&page], self.lease).expect("a tree page")
    }

    fn node(&self, at: CellAddress) -> NodeRef<'_> {
        let node = self.view(at.page()).node(at.slot());
        node.expect("a stored address names a cell")
    }

    fn cells(&self) -> usize {
        self.frames.keys().map(|page| self.view(*page).len()).sum()
    }

    fn used(&self, page: PageId) -> usize {
        self.view(page).tree_page().used_bytes() - TreePage::HEADER_LEN
    }

    fn missing(page: PageId) -> FragmentError {
        FragmentError::Pages(format!("no {page}").into())
    }

    fn take(left: &mut Option<usize>) -> Result<(), FragmentError> {
        if let Some(left) = left {
            *left = left
                .checked_sub(1)
                .ok_or_else(|| FragmentError::Pages("the pages are closed".into()))?;
        }
        Ok(())
    }

    /// Every page is what its records alone give, passes the check of its
    /// fragments, and reads back from its image.
    fn check(&self) {
        assert_eq!(self.shadow.frames.len(), self.frames.len());
        for (id, frame) in &self.shadow.frames {
            let shadow = NodePage::open(frame, self.lease).expect("a page");
            let page = self.view(*id).tree_page();
            let image = page.to_image();
            assert_eq!(shadow.tree_page().to_image(), image, "the records of {id}");
            page.check_fragments().expect("the fragments of a page");
            let decoded = TreePage::decode(&image, FormatVersion::CURRENT, self.size);
            decoded.expect("an image").check(self.size).expect("a page");
        }
    }

    /// The bytes of each fragment of `page`, with the number of its cells.
    fn fragments(&self, page: PageId) -> Vec<(usize, usize)> {
        let view = self.view(page);
        let mut fragments = Vec::new();
        for root in view.tree_page().fragment_roots() {
            let (mut bytes, mut cells, mut open) = (0, 0, vec![root]);
            while let Some(slot) = open.pop() {
                let node = view.node(slot).expect("a cell of the page");
                bytes += node.cell().encoded_len() + TreePage::SLOT_LEN;
                cells += 1;
                let children = Side::BOTH.into_iter().filter_map(|side| node.link(side));
                let children = children.map(|link| link.child);
                let below = children.chain(node.hosted_roots().flatten());
                open.extend(below.filter(|at| at.page() == page).map(|at| at.slot()));
            }
            fragments.push((bytes, cells));
        }
        let (bytes, cells): (Vec<_>, Vec<_>) = fragments.iter().copied().unzip();
        assert_eq!(
            bytes.iter().sum::<usize>(),
            self.used(page),
            "whole fragments"
        );
        assert_eq!(cells.iter().sum::<usize>(), view.len());
        fragments
    }

    /// No fragment is above the largest size, but one of a single cell.
    fn check_fragment_sizes(&self, layout: &Layout) {
        for &page in self.frames.keys() {
            for (bytes, cells) in self.fragments(page) {
                assert!(
                    bytes <= layout.largest_fragment() || cells == 1,
                    "a fragment of {bytes} bytes in {cells} cells"
                );
            }
        }
    }
}

impl Pages for Mem {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        self.reads.push(page);
        let frame = self.frames.get(&page).ok_or_else(|| Self::missing(page))?;
        Ok(read(NodePage::open(frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        Self::take(&mut self.writes_left)?;
        *self.writes.entry(page).or_default() += 1;
        let frame = self
            .frames
            .get_mut(&page)
            .ok_or_else(|| Self::missing(page))?;
        let sink: &mut dyn ChangeSink = &mut self.shadow;
        let mut page = NodePageMut::open(frame, self.size, self.lease, sink)?;
        Ok(change(&mut page)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        Self::take(&mut self.creates_left)?;
        let id = PageId::new(self.next).expect("not zero");
        self.next += 1;
        let mut frame = vec![0; self.size.get()];
        let sink: &mut dyn ChangeSink = &mut self.shadow;
        let mut page = NodePageMut::create(&mut frame, self.size, id, self.lease, sink)?;
        if self.dirty {
            page.insert(&leaf_cell(b"in the way"))?;
        }
        self.frames.insert(id, frame);
        Ok(id)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        self.frames
            .remove(&page)
            .ok_or_else(|| Self::missing(page))?;
        self.shadow.frames.remove(&page);
        Ok(())
    }
}

fn leaf_cell(key: &[u8]) -> lignin_format::tree_page::CellContent<'_> {
    lignin_format::tree_page::CellContent {
        key,
        feature: &[0],
        value_hash: &[1; 32],
        kv_hash: &[2; 32],
        left: None,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(b""),
        fragment_root: true,
    }
}

/// The values of the tests that are not in their cells, with the page that
/// owns each.
#[derive(Debug, Default)]
struct Log {
    values: HashMap<[u8; 8], (Vec<u8>, Option<PageId>)>,
    next: u64,
    /// Refuses every value.
    refuse: bool,
}

impl Log {
    fn pending(&mut self, bytes: &[u8]) -> PendingValue {
        self.next += 1;
        let id = self.next.to_le_bytes();
        self.values.insert(id, (bytes.to_vec(), None));
        PendingValue::new(id, bytes.len() as u32).expect("a pending value")
    }

    /// The bytes of the value a cell of `page` holds.
    fn value(&self, value: CellValue<'_>, page: PageId) -> Vec<u8> {
        match value {
            CellValue::Inline(bytes) => bytes.to_vec(),
            CellValue::Pending(pending) => {
                let (bytes, owner) = &self.values[&pending.id()];
                assert_eq!(*owner, Some(page), "the cell's page owns its value");
                bytes.clone()
            }
            CellValue::OutOfLine(_) => unreachable!("no value of the tests has a pointer"),
        }
    }
}

impl Values for Log {
    fn released(&mut self, place: ValuePlace, _: PageId) {
        let ValuePlace::Pending(pending) = place else {
            unreachable!("no value of the tests has a pointer");
        };
        self.values.remove(&pending.id());
    }

    fn placed(&mut self, pending: PendingValue, page: PageId) {
        let value = self.values.get_mut(&pending.id());
        let owner = &mut value.expect("a value that was held").1;
        assert_eq!(owner.replace(page), None, "a value has one owner");
    }

    fn carried(&mut self, place: ValuePlace, from: PageId, to: PageId) -> PendingValue {
        let ValuePlace::Pending(pending) = place else {
            unreachable!("no value of the tests has a pointer");
        };
        let (bytes, owner) = self.values.remove(&pending.id()).expect("a held value");
        assert_eq!(owner, Some(from));
        let pending = self.pending(&bytes);
        self.placed(pending, to);
        pending
    }
}

impl LoadValues for Log {
    fn hold(&mut self, value: &[u8]) -> Result<Option<PendingValue>, Refusal> {
        if self.refuse {
            return Err("no room for a value".into());
        }
        Ok((value.len() > THRESHOLD).then(|| self.pending(value)))
    }
}

impl ValueReader for Log {
    fn read(&mut self, place: ValuePlace, page: PageId, into: &mut Vec<u8>) -> Result<(), Refusal> {
        into.extend(self.value(CellValue::of_place(place), page));
        Ok(())
    }
}

/// How a host of the model names the root of a Merk it hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hosting {
    /// As a subtree of the load.
    Loaded,
    /// By the address a root got that was placed before.
    At,
}

/// One node of the model: what was given to the load, and what its cell
/// holds afterwards.
#[derive(Debug, Clone)]
struct Node {
    key: Vec<u8>,
    feature: Vec<u8>,
    value: Vec<u8>,
    /// A value hash that is given, not made.
    given: Option<Hash>,
    left: Option<usize>,
    right: Option<usize>,
    hosted: Vec<Option<(usize, Hosting)>>,
    heights: (u8, u8),
    /// The aggregate that the link to this node states.
    aggregate: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    hash: Hash,
}

/// A grove of the tests.
#[derive(Debug, Default)]
struct Model {
    nodes: Vec<Node>,
    keys: u32,
    /// How deep Merks are nested in tree elements.
    nesting: usize,
    /// How many percent of the nodes host a Merk.
    hosts: usize,
    /// How many percent of the values are longer than a cell takes.
    long: usize,
    /// How many percent of the hosted Merks are named by an address.
    at: usize,
    /// How many percent of the nodes split their Merk at random, not in
    /// the middle.
    skew: usize,
}

impl Model {
    fn new(nesting: usize, hosts: usize, long: usize, at: usize) -> Self {
        Self {
            nesting,
            hosts,
            long,
            at,
            skew: 15,
            ..Self::default()
        }
    }

    /// A Merk of `count` nodes, `depth` tree elements below the top:
    /// balanced, but for the nodes that split it at random.
    fn merk(&mut self, rng: &mut Rng, count: usize, depth: usize) -> Option<usize> {
        if count == 0 {
            return None;
        }
        let rest = count - 1;
        let left_count = match rng.chance(self.skew) {
            true => rng.below(rest + 1),
            false => rest / 2,
        };
        let left = self.merk(rng, left_count, depth);
        self.keys += 1;
        let mut key = self.keys.to_be_bytes().to_vec();
        let filler = rng.below(30);
        key.extend(rng.bytes(filler));
        let value_len = match rng.chance(self.long) {
            true => THRESHOLD + 1 + rng.below(200),
            false => rng.below(120),
        };
        let mut hosted = Vec::new();
        if depth < self.nesting && rng.chance(self.hosts) {
            for _ in 0..1 + rng.below(2) {
                let count = [0, 1, 3, 12, 60][rng.below(5)];
                let hosting = match rng.chance(self.at) {
                    true => Hosting::At,
                    false => Hosting::Loaded,
                };
                let root = self.merk(rng, count, depth + 1);
                hosted.push(root.map(|root| (root, hosting)));
            }
        }
        let given = (hosted.is_empty() && rng.chance(10)).then(|| digest(&[&rng.bytes(8)]));
        let right = self.merk(rng, rest - left_count, depth);
        let feature_len = [1, 9][rng.below(2)];
        let feature = rng.bytes(feature_len);
        let value = rng.bytes(value_len);

        let height = |child: Option<usize>| {
            let heights = child.map(|child| self.nodes[child].heights);
            heights.map_or(0, |heights| 1 + heights.0.max(heights.1))
        };
        let heights = (height(left), height(right));
        let roots: Vec<Option<Hash>> = hosted
            .iter()
            .map(|root| root.map(|(root, _)| self.nodes[root].hash))
            .collect();
        let value_hash = given.unwrap_or_else(|| value_rule(&value, &roots));
        let kv_hash = kv_rule(&key, &value_hash);
        let part = |child: Option<usize>| -> LinkPart<'_> {
            let child = &self.nodes[child?];
            Some((
                &child.hash,
                &child.aggregate,
                child.heights,
                child.key.len() as u8,
            ))
        };
        let hash = node_rule(&kv_hash, &feature, part(left), part(right));
        let aggregate = feature.iter().rev().copied().collect();
        self.nodes.push(Node {
            key,
            feature,
            value,
            given,
            left,
            right,
            hosted,
            heights,
            aggregate,
            value_hash,
            kv_hash,
            hash,
        });
        Some(self.nodes.len() - 1)
    }

    /// The node `index` as a load is given it, with `children` naming its
    /// children.
    fn load<'a, C: Copy>(
        &'a self,
        index: usize,
        children: [Option<C>; 2],
        hosted: &'a [HostedLoad],
    ) -> NodeLoad<'a, C> {
        let (nodes, node) = (&self.nodes, &self.nodes[index]);
        let link = |side: usize, below: Option<usize>| {
            let below = &nodes[below?];
            Some(LinkLoad {
                child: children[side]?,
                heights: below.heights,
                aggregate: &below.aggregate[..],
            })
        };
        NodeLoad {
            key: &node.key,
            feature: &node.feature,
            value: &node.value,
            value_hash: node.given.as_ref(),
            left: link(0, node.left),
            right: link(1, node.right),
            hosted,
        }
    }
}

/// A load of the tests, with what it loads into.
struct Bench {
    layout: Layout,
    loader: Loader,
    mem: Mem,
    log: Log,
    rules: Rules,
    /// The subtree each hosted root of the model was given to its host as:
    /// by the host and the position.
    named: HashMap<(usize, usize), Loaded>,
    /// The address each root got that was placed before its host.
    placed: HashMap<usize, CellAddress>,
}

impl Bench {
    fn new(layout: Layout, params: LoadParams) -> Self {
        Self {
            layout,
            loader: Loader::new(layout, params).expect("a fill"),
            mem: Mem::new(layout.page_size()),
            log: Log::default(),
            rules: Rules::default(),
            named: HashMap::new(),
            placed: HashMap::new(),
        }
    }

    fn proposed() -> Self {
        Self::new(Layout::proposed(SIZE), LoadParams::DEFAULT)
    }

    fn node(&mut self, node: &NodeLoad<'_>) -> Result<Loaded, LoadError> {
        self.loader
            .node(&mut self.mem, &mut self.log, &mut self.rules, node)
    }

    fn root(&mut self, subtree: Loaded) -> Result<Root, LoadError> {
        self.loader.root(&mut self.mem, &mut self.log, subtree)
    }

    /// A node with no child and a short value.
    fn leaf(&mut self, key: &[u8]) -> Result<Loaded, LoadError> {
        self.node(&plain(key, b"a value"))
    }

    /// The hosted roots of the node `index` of `model`, each loaded before
    /// its host, in the way the model says.
    fn hosted(&mut self, model: &Model, index: usize, in_order: bool) -> Vec<HostedLoad> {
        let roots = model.nodes[index].hosted.iter().enumerate();
        let load = |(position, root): (usize, &Option<(usize, Hosting)>)| {
            let Some((root, hosting)) = *root else {
                return HostedLoad::Empty;
            };
            let subtree = match in_order {
                true => self.merk_in_order(model, root),
                false => self.subtree(model, root),
            };
            match hosting {
                Hosting::Loaded => {
                    self.named.insert((index, position), subtree);
                    HostedLoad::Loaded(subtree)
                }
                Hosting::At => {
                    let placed = self.root(subtree).expect("a root");
                    assert_eq!(placed.hash, model.nodes[root].hash, "the root hash");
                    self.placed.insert(root, placed.address);
                    HostedLoad::At(placed.address)
                }
            }
        };
        roots.map(load).collect()
    }

    /// Loads the subtree of the node `index` of `model`, a child before its
    /// parent.
    fn subtree(&mut self, model: &Model, index: usize) -> Loaded {
        let node = &model.nodes[index];
        let children = [node.left, node.right];
        let loaded = children.map(|child| child.map(|child| self.subtree(model, child)));
        let hosted = self.hosted(model, index, false);
        let subtree = self.node(&model.load(index, loaded, &hosted));
        let subtree = subtree.expect("a node");
        let hash = self.loader.hash_of(subtree).expect("a free subtree");
        assert_eq!(hash, node.hash, "the hash of the node");
        subtree
    }

    /// Loads the Merk whose root is the node `root` of `model` in the order
    /// of its keys.
    fn merk_in_order(&mut self, model: &Model, root: usize) -> Loaded {
        let mut merk = InOrder::new();
        let (mut open, mut next) = (Vec::new(), Some(root));
        while next.is_some() || !open.is_empty() {
            while let Some(index) = next {
                open.push(index);
                next = model.nodes[index].left;
            }
            let index = open.pop().expect("a node");
            let hosted = self.hosted(model, index, true);
            let node = model.load(index, [Some(()); 2], &hosted);
            let (mem, log, rules) = (&mut self.mem, &mut self.log, &mut self.rules);
            merk.push(&mut self.loader, mem, log, rules, &node)
                .expect("the next node");
            next = model.nodes[index].right;
        }
        let root = merk.finish(&mut self.loader).expect("a Merk");
        root.expect("a Merk with a node")
    }

    /// Ends the load, and returns what it did with the pages it left in
    /// the order it left them.
    fn finish(&mut self) -> LoadReport {
        let layout = self.layout;
        let loader = Loader::new(layout, LoadParams::DEFAULT).expect("a fill");
        let loader = std::mem::replace(&mut self.loader, loader);
        loader.finish().expect("every subtree has its place")
    }

    /// The node `index` of `model` is at `at`, with everything below it,
    /// as it was given and with the hashes the rules make. Returns how many
    /// cells that is.
    fn compare(&self, model: &Model, index: usize, at: CellAddress, report: &LoadReport) -> usize {
        let (expected, mut cells, mut open) = (&model.nodes, 0, vec![(index, at)]);
        let hosted: HashMap<Loaded, CellAddress> = report
            .hosted
            .iter()
            .map(|root| (root.subtree, root.address))
            .collect();
        while let Some((index, at)) = open.pop() {
            let (node, cell) = (&expected[index], self.mem.node(at));
            cells += 1;
            assert_eq!(cell.key(), node.key);
            assert_eq!(cell.feature(), node.feature);
            assert_eq!(self.log.value(cell.value(), at.page()), node.value);
            assert_eq!(cell.value().is_inline(), node.value.len() <= THRESHOLD);
            assert_eq!(cell.value_hash(), &node.value_hash);
            assert_eq!(cell.kv_hash(), &node.kv_hash);
            for (side, child) in Side::BOTH.into_iter().zip([node.left, node.right]) {
                let link = cell.link(side);
                assert_eq!(link.is_some(), child.is_some(), "a child on the {side}");
                let (Some(link), Some(child)) = (link, child) else {
                    continue;
                };
                let below = &expected[child];
                assert_eq!(link.hash, &below.hash);
                assert_eq!(link.heights, below.heights);
                assert_eq!(usize::from(link.child_key_len), below.key.len());
                assert_eq!(link.aggregate, below.aggregate);
                open.push((child, link.child));
            }
            let roots: Vec<_> = cell.hosted_roots().collect();
            assert_eq!(roots.len(), node.hosted.len());
            for (position, (root, hosted_root)) in roots.iter().zip(&node.hosted).enumerate() {
                assert_eq!(root.is_some(), hosted_root.is_some());
                let (Some(root), Some((below, hosting))) = (*root, *hosted_root) else {
                    continue;
                };
                let reported = match hosting {
                    Hosting::Loaded => hosted[&self.named[&(index, position)]],
                    Hosting::At => self.placed[&below],
                };
                assert_eq!(root, reported, "the address that was reported");
                open.push((below, root));
            }
        }
        cells
    }

    /// No stored hash of any cell differs from what the rules make of it.
    fn check_hashes(&mut self) {
        let pages: Vec<PageId> = self.mem.frames.keys().copied().collect();
        for page in pages {
            let slots: Vec<_> = self
                .mem
                .view(page)
                .nodes()
                .map(|node| node.address())
                .collect();
            for at in slots {
                let (mem, log) = (&mut self.mem, &mut self.log);
                let stale = crate::hash::check(mem, log, &mut Rules::default(), at);
                let stale = stale.expect("a cell that is checked");
                let given = self.mem.node(at).value_hash() != &value_rule_of(self, at);
                assert!(
                    stale.is_empty() || (given && stale.len() == 1),
                    "a stale hash at {at}: {stale:?}"
                );
            }
        }
    }
}

/// What the rules make of the value of the cell at `at`.
fn value_rule_of(bench: &Bench, at: CellAddress) -> Hash {
    let cell = bench.mem.node(at);
    let value = bench.log.value(cell.value(), at.page());
    let roots: Vec<Option<Hash>> = cell
        .hosted_roots()
        .map(|root| root.map(|root| node_rule_of(bench.mem.node(root))))
        .collect();
    value_rule(&value, &roots)
}

/// A node with no child, no hosted Merk and a value hash the rules make.
fn plain<'a>(key: &'a [u8], value: &'a [u8]) -> NodeLoad<'a> {
    NodeLoad {
        key,
        feature: &[0],
        value,
        value_hash: None,
        left: None,
        right: None,
        hosted: &[],
    }
}

/// A link to `child` as a leaf.
fn link(child: Loaded) -> Option<LinkLoad<'static>> {
    Some(LinkLoad {
        child,
        heights: (0, 0),
        aggregate: &[],
    })
}
