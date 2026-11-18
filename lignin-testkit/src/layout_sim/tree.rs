//! The logical side of the simulation: a mirror of every Merk in a GroveDB,
//! and the change set one state transition makes to it.
//!
//! The mirror holds no page and no byte of a value. It records which nodes
//! exist, how Merk linked them, and the sizes a cell would need. A page model
//! reads it; only [`super::observe`] and the tests' hand-built trees write it.

use std::{collections::HashMap, num::NonZeroU32};

/// A node of the mirror. Ids are never reused within one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(NonZeroU32);

impl NodeId {
    /// The position of the node in tables indexed by node.
    pub fn index(self) -> usize {
        self.0.get() as usize
    }

    fn from_index(index: usize) -> Self {
        let index = u32::try_from(index).expect("fewer than 2^32 nodes");
        NodeId(NonZeroU32::new(index).expect("node ids start at 1"))
    }
}

/// A Merk of the mirror. Ids are never reused within one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MerkId(u32);

impl MerkId {
    /// The GroveDB root Merk.
    pub const ROOT: MerkId = MerkId(0);

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Which child of a Merk node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The child with the smaller keys.
    Left,
    /// The child with the larger keys.
    Right,
}

/// One Merk node, as its stored record describes it.
#[derive(Clone, Debug)]
pub struct Node {
    /// The Merk the node belongs to.
    pub merk: MerkId,
    /// The node's key.
    pub key: Box<[u8]>,
    /// The Merk parent; `None` for the root of its Merk.
    pub parent: Option<NodeId>,
    /// The left child.
    pub left: Option<NodeId>,
    /// The right child.
    pub right: Option<NodeId>,
    /// The Merks this node's element hosts: one for a tree element, more for
    /// an indexed tree, none for anything else.
    pub hosted: Vec<MerkId>,
    /// The length of the value Merk stores: the serialized element.
    pub value_len: u32,
    /// The bytes Merk's record spends on the feature type and on the
    /// aggregate data of both links.
    pub meta_len: u32,
    /// The node's hash as its parent's link records it; zero for a root.
    pub(crate) node_hash: [u8; 32],
    /// A fingerprint of the stored record.
    pub(crate) record_print: u64,
    /// A fingerprint of the value.
    pub(crate) value_print: u64,
    /// A fingerprint of the value hash.
    pub(crate) value_hash_print: u64,
    /// The observation that last met this node in its tree.
    pub(crate) seen: u32,
    /// The node's event in the change set being built, plus one.
    pub(crate) event: u32,
    /// False once the node was removed or dropped.
    pub live: bool,
}

impl Node {
    fn unlinked(merk: MerkId, key: Box<[u8]>, live: bool) -> Self {
        Node {
            merk,
            key,
            parent: None,
            left: None,
            right: None,
            hosted: Vec::new(),
            value_len: 0,
            meta_len: 0,
            node_hash: [0; 32],
            record_print: 0,
            value_print: 0,
            value_hash_print: 0,
            seen: 0,
            event: 0,
            live,
        }
    }
}

/// One Merk: a subtree of the grove.
#[derive(Clone, Debug)]
pub struct Merk {
    /// The 32-byte storage prefix.
    pub prefix: [u8; 32],
    /// The path from the GroveDB root, when the Merk is reached by path. A
    /// secondary Merk of an indexed tree has a prefix but no path.
    pub path: Option<Vec<Vec<u8>>>,
    /// The axis tag of a secondary Merk of an indexed tree; `None` for a
    /// Merk reached by path.
    pub axis: Option<u8>,
    /// The root node; `None` for an empty Merk.
    pub root: Option<NodeId>,
    /// The node whose element hosts this Merk; `None` for the root Merk.
    pub host: Option<NodeId>,
    pub(crate) keys: HashMap<Box<[u8]>, NodeId>,
    /// False once the Merk was dropped.
    pub live: bool,
}

/// The mirror of every Merk node in a GroveDB.
#[derive(Clone, Debug)]
pub struct Grove {
    nodes: Vec<Node>,
    merks: Vec<Merk>,
    live_nodes: u64,
    live_merks: u64,
}

impl Default for Grove {
    fn default() -> Self {
        Self::new()
    }
}

impl Grove {
    /// An empty grove: the root Merk with no node.
    pub fn new() -> Self {
        Grove {
            nodes: vec![Node::unlinked(MerkId::ROOT, Box::default(), false)],
            merks: vec![Merk {
                prefix: [0; 32],
                path: Some(Vec::new()),
                axis: None,
                root: None,
                host: None,
                keys: HashMap::new(),
                live: true,
            }],
            live_nodes: 0,
            live_merks: 1,
        }
    }

    /// The node `id`.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub(crate) fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    /// The Merk `id`.
    pub fn merk(&self, id: MerkId) -> &Merk {
        &self.merks[id.index()]
    }

    pub(crate) fn merk_mut(&mut self, id: MerkId) -> &mut Merk {
        &mut self.merks[id.index()]
    }

    /// Every node ever added, the retired ones included.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .skip(1)
            .map(|(index, node)| (NodeId::from_index(index), node))
    }

    /// Every Merk ever added, the dropped ones included.
    pub fn merks(&self) -> impl Iterator<Item = (MerkId, &Merk)> {
        self.merks
            .iter()
            .enumerate()
            .map(|(index, merk)| (MerkId(index as u32), merk))
    }

    /// One more than the largest node index handed out so far.
    pub fn node_slots(&self) -> usize {
        self.nodes.len()
    }

    /// How many nodes exist.
    pub fn live_nodes(&self) -> u64 {
        self.live_nodes
    }

    /// How many Merks exist, the empty ones included.
    pub fn live_merks(&self) -> u64 {
        self.live_merks
    }

    /// The node with `key` in `merk`.
    pub fn find(&self, merk: MerkId, key: &[u8]) -> Option<NodeId> {
        self.merk(merk).keys.get(key).copied()
    }

    /// Whether `id` is the root of its Merk.
    pub fn is_merk_root(&self, id: NodeId) -> bool {
        self.merk(self.node(id).merk).root == Some(id)
    }

    /// The parent for placement: the Merk parent, or for the root of a Merk
    /// the node whose element hosts that Merk. Only the root of the root
    /// Merk has none.
    pub fn grove_parent(&self, id: NodeId) -> Option<NodeId> {
        let node = self.node(id);
        node.parent.or_else(|| self.merk(node.merk).host)
    }

    /// The children for placement: the Merk children, then the roots of the
    /// Merks the node hosts.
    pub fn grove_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let node = self.node(id);
        node.left
            .into_iter()
            .chain(node.right)
            .chain(node.hosted.iter().filter_map(|merk| self.merk(*merk).root))
    }

    /// How many links to a child the node's cell holds.
    pub fn child_links(&self, id: NodeId) -> u32 {
        let node = self.node(id);
        u32::from(node.left.is_some()) + u32::from(node.right.is_some())
    }

    /// How many hosted Merks have a root for the node's cell to point at.
    pub fn hosted_roots(&self, id: NodeId) -> u32 {
        let node = self.node(id);
        node.hosted
            .iter()
            .filter(|merk| self.merk(**merk).root.is_some())
            .count() as u32
    }

    /// Whether `ancestor` is a proper ancestor of `id` inside one Merk.
    pub fn is_merk_ancestor(&self, ancestor: NodeId, id: NodeId) -> bool {
        let mut at = self.node(id).parent;
        while let Some(parent) = at {
            if parent == ancestor {
                return true;
            }
            at = self.node(parent).parent;
        }
        false
    }

    pub(crate) fn add_node(&mut self, merk: MerkId, key: &[u8]) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(Node::unlinked(merk, key.into(), true));
        self.merk_mut(merk).keys.insert(key.into(), id);
        self.live_nodes += 1;
        id
    }

    pub(crate) fn add_merk(
        &mut self,
        prefix: [u8; 32],
        path: Option<Vec<Vec<u8>>>,
        axis: Option<u8>,
        host: NodeId,
    ) -> MerkId {
        let id = MerkId(u32::try_from(self.merks.len()).expect("fewer than 2^32 Merks"));
        self.merks.push(Merk {
            prefix,
            path,
            axis,
            root: None,
            host: Some(host),
            keys: HashMap::new(),
            live: true,
        });
        self.live_merks += 1;
        id
    }

    /// Takes a node out of its Merk. Its links are left as they were, so a
    /// change set can still say where it used to be.
    pub(crate) fn retire_node(&mut self, id: NodeId) {
        let node = &mut self.nodes[id.index()];
        debug_assert!(node.live, "a node is retired once");
        node.live = false;
        self.merks[node.merk.index()].keys.remove(&node.key);
        self.live_nodes -= 1;
    }

    /// Empties a Merk and every Merk hosted inside it, and records in
    /// `changes` the nodes and Merks that went. With `whole` the Merk itself
    /// is gone too: its host was deleted or stopped being a tree.
    pub(crate) fn drop_merk(&mut self, id: MerkId, whole: bool, changes: &mut ChangeSet) {
        if whole && self.merks[id.index()].live {
            changes.subtrees_dropped += 1;
        }
        let mut pending = vec![(id, whole)];
        while let Some((merk, whole)) = pending.pop() {
            let keys = std::mem::take(&mut self.merks[merk.index()].keys);
            let mut nodes: Vec<NodeId> = keys.into_values().collect();
            nodes.sort_unstable();
            for node in nodes {
                let entry = &mut self.nodes[node.index()];
                entry.live = false;
                pending.extend(entry.hosted.iter().map(|hosted| (*hosted, true)));
                changes.dropped.push(node);
                self.live_nodes -= 1;
            }
            let entry = &mut self.merks[merk.index()];
            entry.root = None;
            if whole && entry.live {
                entry.live = false;
                self.live_merks -= 1;
                changes.merks_dropped += 1;
            }
        }
    }
}

/// What one state transition did to one node that is still in its tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// The node whose stored record is new or changed.
    pub node: NodeId,
    /// Whether the node did not exist before.
    pub created: bool,
    /// The Merk parent before the transition.
    pub old_parent: Option<NodeId>,
    /// The left child before the transition.
    pub old_left: Option<NodeId>,
    /// The right child before the transition.
    pub old_right: Option<NodeId>,
    /// Whether the value's bytes changed: a value is written.
    pub value_changed: bool,
    /// Whether the node's parent or a child changed: a link is rewritten.
    pub links_changed: bool,
}

/// Everything one state transition changed, as Merk's node records show it.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    /// The nodes whose record is new or changed, each before its children
    /// and before the nodes of the Merks it hosts.
    pub events: Vec<Event>,
    /// The nodes Merk deleted from a tree that still exists.
    pub removed: Vec<NodeId>,
    /// The nodes that went with a Merk that was dropped or cleared.
    pub dropped: Vec<NodeId>,
    /// Position changes: the first node took the place of the second in its
    /// tree. The second was either deleted, or is now below the first after
    /// a rotation. Listed top-down.
    pub takes_place_of: Vec<(NodeId, NodeId)>,
    /// The Merks whose root is another node than before, or that became
    /// empty or stopped being empty.
    pub roots_changed: Vec<MerkId>,
    /// The Merks that came into being.
    pub merks_created: Vec<MerkId>,
    /// How many Merks were dropped, the ones inside a dropped Merk
    /// included.
    pub merks_dropped: u32,
    /// How many of them their own host let go of: its node was deleted or
    /// its element stopped being a tree. The others went with a Merk above
    /// them.
    pub subtrees_dropped: u32,
    /// The bytes of the records behind `events`, as RocksDB is given them:
    /// prefix, key and value.
    pub record_bytes: u64,
    /// How many of the `events` changed nothing but hashes.
    pub hash_only: u32,
}

impl ChangeSet {
    /// Whether the transition changed no node.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.removed.is_empty() && self.dropped.is_empty()
    }

    /// Fills in [`ChangeSet::takes_place_of`] and [`ChangeSet::hash_only`]
    /// from the events, the removals and the grove as it now is.
    ///
    /// `old_roots` lists, for each Merk whose root changed, the root it had
    /// before.
    pub(crate) fn finish(&mut self, grove: &mut Grove, old_roots: &[(MerkId, NodeId)]) {
        let mut takes = Vec::new();
        for (merk, old_root) in old_roots {
            if let Some(root) = grove.merk(*merk).root
                && root != *old_root
                && !grove.node(*old_root).live
            {
                takes.push((root, *old_root));
            }
        }
        for index in 0..self.events.len() {
            let event = self.events[index];
            let node = grove.node(event.node);
            if !event.value_changed && !event.links_changed && !event.created {
                self.hash_only += 1;
            }
            for (old, new) in [(event.old_left, node.left), (event.old_right, node.right)] {
                if let (Some(old), Some(new)) = (old, new)
                    && old != new
                    && !grove.node(old).live
                {
                    takes.push((new, old));
                }
            }
            if event.created || node.parent == event.old_parent {
                continue;
            }
            if takes.iter().any(|(taker, _)| *taker == event.node) {
                continue;
            }
            if let Some(top) = self.risen_above(grove, event) {
                takes.push((event.node, top));
            }
        }
        for event in &self.events {
            grove.node_mut(event.node).event = 0;
        }
        self.takes_place_of = takes;
    }

    /// The highest node that was above `event.node` before the transition
    /// and is below it now, if a rotation lifted the node over any.
    fn risen_above(&self, grove: &Grove, event: Event) -> Option<NodeId> {
        let mut top = None;
        let mut above = event.old_parent;
        while let Some(ancestor) = above {
            let node = grove.node(ancestor);
            let old_parent = match node.event {
                0 => node.parent,
                index => self.events[index as usize - 1].old_parent,
            };
            if node.live {
                if !grove.is_merk_ancestor(event.node, ancestor) {
                    break;
                }
                top = Some(ancestor);
            }
            above = old_parent;
        }
        top
    }
}

/// Groves built by hand, for tests that need a tree shape of their choice.
#[cfg(test)]
pub(crate) mod hand {
    use std::collections::HashMap;

    use super::{ChangeSet, Event, Grove, MerkId, NodeId, Side};

    /// A grove edited link by link. [`Hand::commit`] returns the change set
    /// the observer would report for the edits since the last commit.
    pub(crate) struct Hand {
        pub(crate) grove: Grove,
        touched: HashMap<NodeId, Event>,
        old_roots: Vec<(MerkId, NodeId)>,
        changes: ChangeSet,
        merks: u32,
    }

    impl Hand {
        pub(crate) fn new() -> Self {
            Hand {
                grove: Grove::new(),
                touched: HashMap::new(),
                old_roots: Vec::new(),
                changes: ChangeSet::default(),
                merks: 0,
            }
        }

        /// Marks `node` as written, remembering its links as they are now.
        pub(crate) fn touch(&mut self, node: NodeId) {
            let entry = self.grove.node(node);
            self.touched.entry(node).or_insert(Event {
                node,
                created: false,
                old_parent: entry.parent,
                old_left: entry.left,
                old_right: entry.right,
                value_changed: false,
                links_changed: false,
            });
        }

        /// Marks `node` and everything above it up to the root of the grove
        /// as written, as a hash update does.
        pub(crate) fn touch_path(&mut self, node: NodeId) {
            let mut at = Some(node);
            while let Some(id) = at {
                self.touch(id);
                at = self.grove.grove_parent(id);
            }
        }

        /// A new node with no link yet.
        pub(crate) fn add(&mut self, merk: MerkId, key: &[u8], value_len: u32) -> NodeId {
            let node = self.grove.add_node(merk, key);
            let entry = self.grove.node_mut(node);
            entry.value_len = value_len;
            entry.meta_len = 1;
            self.touched.insert(
                node,
                Event {
                    node,
                    created: true,
                    old_parent: None,
                    old_left: None,
                    old_right: None,
                    value_changed: true,
                    links_changed: true,
                },
            );
            node
        }

        /// Makes `child` the child of `parent` on `side`.
        pub(crate) fn link(&mut self, parent: NodeId, side: Side, child: Option<NodeId>) {
            self.touch(parent);
            if let Some(child) = child {
                self.touch(child);
                self.grove.node_mut(child).parent = Some(parent);
                self.touched.get_mut(&child).expect("touched").links_changed = true;
            }
            let entry = self.grove.node_mut(parent);
            match side {
                Side::Left => entry.left = child,
                Side::Right => entry.right = child,
            }
            entry.meta_len = 1 + u32::from(entry.left.is_some()) + u32::from(entry.right.is_some());
            self.touched
                .get_mut(&parent)
                .expect("touched")
                .links_changed = true;
        }

        /// Makes `root` the root of `merk`.
        pub(crate) fn set_root(&mut self, merk: MerkId, root: Option<NodeId>) {
            let old = self.grove.merk(merk).root;
            if old == root {
                return;
            }
            if let Some(old) = old {
                self.old_roots.push((merk, old));
            }
            if let Some(root) = root {
                self.touch(root);
                self.grove.node_mut(root).parent = None;
                self.touched.get_mut(&root).expect("touched").links_changed = true;
            }
            self.grove.merk_mut(merk).root = root;
            self.changes.roots_changed.push(merk);
            if let Some(host) = self.grove.merk(merk).host {
                self.touch(host);
            }
        }

        /// Makes the element of `node` a tree and returns the Merk it hosts.
        pub(crate) fn host(&mut self, node: NodeId) -> MerkId {
            self.touch(node);
            self.merks += 1;
            let mut prefix = [0u8; 32];
            prefix[..4].copy_from_slice(&self.merks.to_le_bytes());
            let merk = self.grove.add_merk(prefix, None, None, node);
            self.grove.node_mut(node).hosted.push(merk);
            self.changes.merks_created.push(merk);
            merk
        }

        /// Deletes `node`, which the caller has already unlinked.
        pub(crate) fn remove(&mut self, node: NodeId) {
            self.touched.remove(&node);
            let hosted = self.grove.node(node).hosted.clone();
            self.grove.retire_node(node);
            self.changes.removed.push(node);
            for merk in hosted {
                self.grove.drop_merk(merk, true, &mut self.changes);
            }
        }

        /// The change set of the edits since the last commit.
        pub(crate) fn commit(&mut self) -> ChangeSet {
            let mut changes = std::mem::take(&mut self.changes);
            let mut touched = std::mem::take(&mut self.touched);
            let mut pending: Vec<NodeId> = self.grove.merk(MerkId::ROOT).root.into_iter().collect();
            while let Some(node) = pending.pop() {
                if let Some(event) = touched.remove(&node) {
                    changes.events.push(event);
                    self.grove.node_mut(node).event = changes.events.len() as u32;
                }
                let entry = self.grove.node(node);
                let hosted = entry
                    .hosted
                    .iter()
                    .rev()
                    .filter_map(|merk| self.grove.merk(*merk).root);
                pending.extend(hosted.chain(entry.right).chain(entry.left));
            }
            assert!(touched.is_empty(), "a touched node is not in the grove");
            let old_roots = std::mem::take(&mut self.old_roots);
            changes.finish(&mut self.grove, &old_roots);
            changes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hand::Hand, *};

    fn key(index: u8) -> [u8; 1] {
        [index]
    }

    /// `x` at the root with children `y` and `c`; `y` with children `a` and
    /// `z`; `z` with children `b1` and `b2`.
    fn left_heavy(hand: &mut Hand) -> [NodeId; 7] {
        let x = hand.add(MerkId::ROOT, &key(60), 1);
        hand.set_root(MerkId::ROOT, Some(x));
        let y = hand.add(MerkId::ROOT, &key(20), 1);
        let c = hand.add(MerkId::ROOT, &key(70), 1);
        hand.link(x, Side::Left, Some(y));
        hand.link(x, Side::Right, Some(c));
        let a = hand.add(MerkId::ROOT, &key(10), 1);
        let z = hand.add(MerkId::ROOT, &key(40), 1);
        hand.link(y, Side::Left, Some(a));
        hand.link(y, Side::Right, Some(z));
        let b1 = hand.add(MerkId::ROOT, &key(30), 1);
        let b2 = hand.add(MerkId::ROOT, &key(50), 1);
        hand.link(z, Side::Left, Some(b1));
        hand.link(z, Side::Right, Some(b2));
        let changes = hand.commit();
        assert_eq!(changes.events.len(), 7);
        assert!(changes.events.iter().all(|event| event.created));
        assert!(changes.takes_place_of.is_empty());
        assert_eq!(changes.hash_only, 0);
        assert!(!changes.is_empty());
        [x, y, c, a, z, b1, b2]
    }

    #[test]
    fn events_come_parent_first() {
        let mut hand = Hand::new();
        let [x, y, c, a, z, b1, b2] = left_heavy(&mut hand);
        hand.touch_path(b2);
        let changes = hand.commit();
        let order: Vec<NodeId> = changes.events.iter().map(|event| event.node).collect();
        assert_eq!(order, vec![x, y, z, b2]);
        assert_eq!(changes.hash_only, 4);
        let grove = &hand.grove;
        assert_eq!(grove.live_nodes(), 7);
        assert_eq!(grove.child_links(y), 2);
        assert_eq!(grove.child_links(a), 0);
        assert!(grove.is_merk_ancestor(x, b1) && !grove.is_merk_ancestor(c, b1));
        assert!(grove.is_merk_root(x) && !grove.is_merk_root(y));
        assert_eq!(grove.grove_parent(b1), Some(z));
        assert_eq!(grove.grove_parent(x), None);
        assert_eq!(grove.find(MerkId::ROOT, &key(40)), Some(z));
        assert_eq!(grove.find(MerkId::ROOT, &key(41)), None);
        assert_eq!(grove.grove_children(z).collect::<Vec<_>>(), vec![b1, b2]);
        assert_eq!(grove.nodes().count(), 7);
        assert_eq!(grove.node_slots(), 8);
        assert_eq!(grove.merks().count(), 1);
        assert_eq!(a.index(), 4);
        assert!(ChangeSet::default().is_empty());
    }

    #[test]
    fn a_double_rotation_puts_one_node_in_the_place_of_the_top() {
        let mut hand = Hand::new();
        let [x, y, _, _, z, b1, b2] = left_heavy(&mut hand);
        // Left-right rotation at `x`: `z` rises over `y` and `x`.
        hand.link(y, Side::Right, Some(b1));
        hand.link(x, Side::Left, Some(b2));
        hand.link(z, Side::Left, Some(y));
        hand.link(z, Side::Right, Some(x));
        hand.set_root(MerkId::ROOT, Some(z));
        let changes = hand.commit();
        assert_eq!(
            changes.takes_place_of,
            vec![(z, x)],
            "the node on top took the place of the node that was on top",
        );
        assert_eq!(changes.roots_changed, vec![MerkId::ROOT]);
        assert_eq!(hand.grove.node(x).parent, Some(z));
        assert_eq!(hand.grove.node(b2).parent, Some(x));
    }

    #[test]
    fn a_deleted_node_is_replaced_by_the_one_that_takes_its_link() {
        let mut hand = Hand::new();
        let [x, y, _, a, z, _, _] = left_heavy(&mut hand);
        // Delete the leaf `a`: nothing takes its place.
        hand.link(y, Side::Left, None);
        hand.remove(a);
        let changes = hand.commit();
        assert_eq!(changes.removed, vec![a]);
        assert!(changes.takes_place_of.is_empty());
        assert!(!hand.grove.node(a).live);
        // Delete `y`, which now has one child: `z` moves up into its place.
        hand.link(x, Side::Left, Some(z));
        hand.remove(y);
        let changes = hand.commit();
        assert_eq!(changes.takes_place_of, vec![(z, y)]);
        assert_eq!(hand.grove.live_nodes(), 5);
    }

    #[test]
    fn a_deleted_root_is_replaced_by_the_new_root() {
        let mut hand = Hand::new();
        let old = hand.add(MerkId::ROOT, &key(1), 1);
        hand.set_root(MerkId::ROOT, Some(old));
        hand.commit();
        let new = hand.add(MerkId::ROOT, &key(2), 1);
        hand.set_root(MerkId::ROOT, Some(new));
        hand.remove(old);
        let changes = hand.commit();
        assert_eq!(changes.takes_place_of, vec![(new, old)]);
    }

    #[test]
    fn dropping_a_merk_takes_the_merks_inside_it() {
        let mut hand = Hand::new();
        let top = hand.add(MerkId::ROOT, &key(1), 1);
        hand.set_root(MerkId::ROOT, Some(top));
        let outer = hand.host(top);
        let middle = hand.add(outer, &key(2), 1);
        hand.set_root(outer, Some(middle));
        let inner = hand.host(middle);
        let leaf = hand.add(inner, &key(3), 1);
        hand.set_root(inner, Some(leaf));
        let changes = hand.commit();
        assert_eq!(changes.merks_created, vec![outer, inner]);
        assert_eq!(hand.grove.live_merks(), 3);
        assert_eq!(hand.grove.hosted_roots(top), 1);
        assert_eq!(hand.grove.grove_parent(leaf), Some(middle));
        assert_eq!(
            hand.grove.grove_children(top).collect::<Vec<_>>(),
            vec![middle]
        );
        assert_eq!(hand.grove.merk(inner).host, Some(middle));

        // Clearing the outer Merk empties it and drops the one inside.
        let mut cleared = ChangeSet::default();
        hand.grove.drop_merk(outer, false, &mut cleared);
        assert_eq!(cleared.dropped, vec![middle, leaf]);
        assert_eq!((cleared.merks_dropped, cleared.subtrees_dropped), (1, 0));
        assert_eq!(hand.grove.live_merks(), 2);
        assert!(hand.grove.merk(outer).live && !hand.grove.merk(inner).live);
        assert_eq!(hand.grove.merk(outer).root, None);
        assert_eq!(hand.grove.hosted_roots(top), 0);
        assert_eq!(hand.grove.live_nodes(), 1);
        assert_eq!(Grove::default().live_merks(), 1);
    }

    #[test]
    fn a_deleted_host_lets_go_of_one_subtree_whatever_is_inside_it() {
        let mut hand = Hand::new();
        let top = hand.add(MerkId::ROOT, &key(1), 1);
        hand.set_root(MerkId::ROOT, Some(top));
        let host = hand.add(MerkId::ROOT, &key(2), 1);
        hand.link(top, Side::Right, Some(host));
        let outer = hand.host(host);
        let middle = hand.add(outer, &key(3), 1);
        hand.set_root(outer, Some(middle));
        let inner = hand.host(middle);
        let leaf = hand.add(inner, &key(4), 1);
        hand.set_root(inner, Some(leaf));
        hand.commit();

        hand.link(top, Side::Right, None);
        hand.remove(host);
        let changes = hand.commit();
        assert_eq!(changes.removed, vec![host]);
        assert_eq!(changes.dropped, vec![middle, leaf]);
        assert_eq!((changes.merks_dropped, changes.subtrees_dropped), (2, 1));
        assert_eq!(hand.grove.live_merks(), 1);
    }
}
