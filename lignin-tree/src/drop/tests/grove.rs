//! A grove of search trees that states its writes to fragment maintenance
//! and to the directory, as the Merks of a write transaction would, and
//! knows which of its nodes a drop left behind.

use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};

use lignin_format::{
    tree_page::{CellValue, Hash, Side},
    CellAddress,
};
use lignin_store::load::LoadBuffers;

use super::{Db, Txn, GUARD};
use crate::{
    directory::{Directory, Dropped, Host, Prefix, Root, RootChange},
    drop::{unlink, Leaving},
    fragment::{
        Fragments, HostedWrite, Layout, LayoutParams, LinkWrite, NodeWrite, Report, RootWrite,
        Target, ValueWrite, Write,
    },
    node::{Lease, NodeReader},
};

const HASH: Hash = [7; 32];

#[derive(Debug, Clone)]
pub(in crate::drop) struct Node {
    pub merk: usize,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub parent: Option<usize>,
    /// The Merks the node hosts, by position.
    pub hosted: Vec<Option<usize>>,
    pub at: Option<CellAddress>,
    pub alive: bool,
    /// Whether a drop left the node's cell in its page.
    pub left_behind: bool,
    new_value: bool,
}

#[derive(Debug, Clone)]
pub(in crate::drop) struct Merk {
    pub prefix: Prefix,
    pub root: Option<usize>,
    pub host: Option<(usize, u8)>,
    pub alive: bool,
    /// The root's address as the directory was told.
    named: Option<CellAddress>,
    entered: bool,
}

#[derive(Clone)]
pub(in crate::drop) struct Grove {
    pub nodes: Vec<Node>,
    pub merks: Vec<Merk>,
    pub fragments: Fragments,
    pub directory: Directory,
    /// How long the last drop took in [`unlink`]: the write to the tree
    /// pages and to the directory, without what this model does around it.
    pub unlinked_in: Option<Duration>,
    dirty: BTreeSet<usize>,
    removed: Vec<CellAddress>,
    by_address: HashMap<CellAddress, usize>,
    prefixes: u64,
}

impl Grove {
    /// A grove of one Merk with no node, the root of the grove, with its
    /// directory.
    pub fn new(db: &Db, txn: &mut Txn<'_>) -> Self {
        let layout = Layout::new(db.size, LayoutParams::default()).expect("a layout");
        let directory = Directory::create(&mut txn.flat, db.config).expect("a directory");
        let mut grove = Self {
            nodes: Vec::new(),
            merks: Vec::new(),
            fragments: Fragments::new(layout),
            directory,
            unlinked_in: None,
            dirty: BTreeSet::new(),
            removed: Vec::new(),
            by_address: HashMap::new(),
            prefixes: 0,
        };
        grove.new_merk(None);
        grove
    }

    fn new_merk(&mut self, host: Option<(usize, u8)>) -> usize {
        self.prefixes += 1;
        let mut prefix = [0xA5; 32];
        prefix[..8].copy_from_slice(&self.prefixes.to_be_bytes());
        self.merks.push(Merk {
            prefix,
            root: None,
            host,
            alive: true,
            named: None,
            entered: false,
        });
        self.merks.len() - 1
    }

    fn mark(&mut self, mut node: usize) {
        loop {
            self.dirty.insert(node);
            node = match self.nodes[node].parent {
                Some(parent) => parent,
                None => match self.merks[self.nodes[node].merk].host {
                    Some((host, _)) => host,
                    None => return,
                },
            };
        }
    }

    /// Puts `value` under `key` in `merk`, and returns the node.
    pub fn put(&mut self, merk: usize, key: &[u8], value: &[u8]) -> usize {
        let mut parent = None;
        let mut at = self.merks[merk].root;
        while let Some(node) = at {
            let side = match key.cmp(&self.nodes[node].key) {
                std::cmp::Ordering::Equal => {
                    self.nodes[node].value = value.to_vec();
                    self.nodes[node].new_value = true;
                    self.mark(node);
                    return node;
                }
                std::cmp::Ordering::Less => Side::Left,
                std::cmp::Ordering::Greater => Side::Right,
            };
            parent = Some((node, side));
            at = match side {
                Side::Left => self.nodes[node].left,
                Side::Right => self.nodes[node].right,
            };
        }
        let node = self.nodes.len();
        self.nodes.push(Node {
            merk,
            key: key.to_vec(),
            value: value.to_vec(),
            left: None,
            right: None,
            parent: parent.map(|(parent, _)| parent),
            hosted: Vec::new(),
            at: None,
            alive: true,
            left_behind: false,
            new_value: true,
        });
        match parent {
            None => self.merks[merk].root = Some(node),
            Some((parent, Side::Left)) => self.nodes[parent].left = Some(node),
            Some((parent, Side::Right)) => self.nodes[parent].right = Some(node),
        }
        self.mark(node);
        node
    }

    /// Makes `node` the host of one more Merk, and returns that Merk.
    pub fn host(&mut self, node: usize) -> usize {
        let position = self.nodes[node].hosted.len() as u8;
        let merk = self.new_merk(Some((node, position)));
        self.nodes[node].hosted.push(Some(merk));
        self.mark(node);
        merk
    }

    /// A Merk again in the place of the dropped one that `node` hosted at
    /// `position`.
    pub fn host_again(&mut self, node: usize, position: u8, prefix: Prefix) -> usize {
        let merk = self.new_merk(Some((node, position)));
        self.merks[merk].prefix = prefix;
        self.nodes[node].hosted[usize::from(position)] = Some(merk);
        self.mark(node);
        merk
    }

    /// A Merk under `prefix` that `node` hosts as one more of its Merks.
    pub fn host_under(&mut self, node: usize, prefix: Prefix) -> usize {
        let merk = self.host(node);
        self.merks[merk].prefix = prefix;
        merk
    }

    /// The nodes of `merk` and of every Merk nested in it.
    pub fn below(&self, merk: usize) -> Vec<usize> {
        let mut found = Vec::new();
        let mut next: Vec<usize> = self.merks[merk].root.into_iter().collect();
        while let Some(node) = next.pop() {
            found.push(node);
            let node = &self.nodes[node];
            next.extend(node.left.into_iter().chain(node.right));
            let hosted = node.hosted.iter().flatten();
            next.extend(hosted.filter_map(|merk| self.merks[*merk].root));
        }
        found
    }

    /// The Merks nested in `merk`.
    pub fn merks_below(&self, merk: usize) -> Vec<usize> {
        let nodes = self.below(merk);
        let hosted = nodes.iter().flat_map(|node| &self.nodes[*node].hosted);
        hosted.flatten().copied().collect()
    }

    /// How many nodes are in use.
    pub fn alive(&self) -> usize {
        self.nodes.iter().filter(|node| node.alive).count()
    }

    /// How many cells drops left behind.
    pub fn left_behind(&self) -> usize {
        self.nodes.iter().filter(|node| node.left_behind).count()
    }

    /// States what changed since the last write: to the tree pages, and the
    /// roots to the directory.
    pub fn flush(&mut self, txn: &mut Txn<'_>) -> Report {
        self.write(txn, &[], false).0
    }

    /// Drops the Merks `merks`, or clears them: each host loses its
    /// pointer, in one write with what else changed.
    pub fn drop_merks(&mut self, txn: &mut Txn<'_>, merks: &[usize], clear: bool) -> Vec<Dropped> {
        for &merk in merks {
            let (host, _) = self.merks[merk].host.expect("a hosted Merk");
            self.mark(host);
        }
        self.write(txn, merks, clear).1
    }

    /// Drops every Merk that the leaf `host` hosts, and deletes the leaf.
    pub fn drop_with_host(&mut self, txn: &mut Txn<'_>, host: usize) -> Vec<Dropped> {
        let node = self.nodes[host].clone();
        assert!(node.left.is_none() && node.right.is_none(), "a leaf");
        let merks: Vec<usize> = node.hosted.iter().flatten().copied().collect();
        match node.parent {
            Some(parent) => {
                let above = &mut self.nodes[parent];
                if above.left == Some(host) {
                    above.left = None;
                } else {
                    above.right = None;
                }
                self.mark(parent);
            }
            None => {
                self.merks[node.merk].root = None;
                if let Some((above, _)) = self.merks[node.merk].host {
                    self.mark(above);
                }
            }
        }
        self.dirty.remove(&host);
        self.nodes[host].alive = false;
        let at = self.nodes[host].at.take().expect("a written node");
        self.by_address.remove(&at);
        self.removed.push(at);
        self.write(txn, &merks, false).1
    }

    fn write(
        &mut self,
        txn: &mut Txn<'_>,
        leaving: &[usize],
        clear: bool,
    ) -> (Report, Vec<Dropped>) {
        // The hosts of the Merks that leave name them no more, and what
        // they held stays in the pages.
        let mut leaving_prefixes = Vec::new();
        for &merk in leaving {
            for node in self.below(merk) {
                let node = &mut self.nodes[node];
                node.alive = false;
                node.left_behind = true;
                if let Some(at) = node.at {
                    self.by_address.remove(&at);
                }
            }
            for nested in self.merks_below(merk) {
                self.merks[nested].alive = false;
            }
            leaving_prefixes.push(self.merks[merk].prefix);
            let (host, position) = self.merks[merk].host.expect("a hosted Merk");
            self.merks[merk].root = None;
            self.merks[merk].named = None;
            if !clear {
                self.merks[merk].alive = false;
                if self.nodes[host].alive {
                    self.nodes[host].hosted[usize::from(position)] = None;
                }
            }
        }
        self.dirty.retain(|node| self.nodes[*node].alive);

        // A Merk is entered before its host is written.
        for merk in 0..self.merks.len() {
            if self.merks[merk].entered || !self.merks[merk].alive {
                continue;
            }
            let prefix = self.merks[merk].prefix;
            let host = self.merks[merk].host.map(|(node, position)| {
                let node = &self.nodes[node];
                (self.merks[node.merk].prefix, node.key.clone(), position)
            });
            let host = host.as_ref().map(|(parent, key, position)| Host {
                parent,
                key,
                position: *position,
            });
            self.directory
                .add(&mut txn.flat, &prefix, b"tree", host)
                .expect("the Merk is entered");
            self.merks[merk].entered = true;
        }

        let order: Vec<usize> = self.dirty.iter().copied().collect();
        let index_of: HashMap<usize, usize> = order
            .iter()
            .enumerate()
            .map(|(index, node)| (*node, index))
            .collect();
        let (nodes, merks) = (&self.nodes, &self.merks);
        let target = |node: usize| match index_of.get(&node) {
            Some(index) => Target::Node(*index),
            None => Target::At(nodes[node].at.expect("a node outside the write has a cell")),
        };
        let link = |child: Option<usize>| {
            child.map(|child| LinkWrite {
                child: target(child),
                hash: &HASH,
                heights: (1, 1),
                child_key_len: nodes[child].key.len() as u8,
                aggregate: &[],
            })
        };
        let hosted: Vec<Vec<Option<Target>>> = order
            .iter()
            .map(|&node| {
                let roots = nodes[node].hosted.iter();
                roots
                    .map(|merk| merk.and_then(|merk| merks[merk].root).map(target))
                    .collect()
            })
            .collect();
        let mut writes = Vec::with_capacity(order.len());
        for (&node, hosted) in order.iter().zip(&hosted) {
            let node = &nodes[node];
            let value = match node.new_value {
                true => txn.values.write(&node.value).expect("a value"),
                false => ValueWrite::Keep,
            };
            writes.push(NodeWrite {
                at: node.at,
                key: &node.key,
                feature: &[0],
                value_hash: &HASH,
                kv_hash: &HASH,
                left: link(node.left),
                right: link(node.right),
                hosted: match hosted.is_empty() {
                    true => HostedWrite::Keep,
                    false => HostedWrite::Set(hosted),
                },
                value,
            });
        }
        let roots = [RootWrite {
            was: merks[0].named,
            now: merks[0].root.map(target),
            anchored: false,
        }];
        let write = Write {
            nodes: &writes,
            removed: &self.removed,
            dropped: &[],
            roots: &roots,
        };
        let (report, left) = if leaving.is_empty() {
            let report = self
                .fragments
                .apply(&mut txn.tree, &mut txn.values, &write)
                .expect("the write is applied");
            (report, Vec::new())
        } else {
            let leaving: Vec<Leaving<'_>> = leaving_prefixes
                .iter()
                .map(|prefix| match clear {
                    true => Leaving::Cleared(prefix),
                    false => Leaving::Dropped(prefix),
                })
                .collect();
            let began = Instant::now();
            let done = unlink(
                &mut self.fragments,
                &mut txn.tree,
                &mut txn.values,
                &mut self.directory,
                &mut txn.flat,
                &write,
                &leaving,
            )
            .expect("the Merks are dropped");
            self.unlinked_in = Some(began.elapsed());
            (done.report, done.left)
        };

        let others: Vec<(usize, CellAddress)> = report
            .carried
            .iter()
            .filter(|carried| carried.node.is_none())
            .map(|carried| (self.by_address[&carried.from], carried.to.address()))
            .collect();
        for (index, handle) in report.nodes.iter().enumerate() {
            let node = order[index];
            if let Some(was) = self.nodes[node].at.replace(handle.address()) {
                self.by_address.remove(&was);
            }
            self.nodes[node].new_value = false;
        }
        for (node, at) in &others {
            let was = self.nodes[*node].at.replace(*at);
            self.by_address
                .remove(&was.expect("a carried cell had an address"));
        }
        for &node in order.iter().chain(others.iter().map(|(node, _)| node)) {
            let at = self.nodes[node].at.expect("a written node");
            self.by_address.insert(at, node);
        }
        self.dirty.clear();
        self.removed.clear();

        // The roots that are elsewhere than the directory was told.
        let mut moved = Vec::new();
        for (index, merk) in self.merks.iter().enumerate() {
            let now = merk.root.and_then(|root| self.nodes[root].at);
            if merk.alive && merk.named != now {
                moved.push((index, merk.root, now));
            }
        }
        let changes: Vec<RootChange<'_>> = moved
            .iter()
            .map(|&(merk, root, now)| RootChange::Named {
                prefix: &self.merks[merk].prefix,
                root: now.map(|address| Root {
                    address,
                    key: &self.nodes[root.expect("a root")].key,
                }),
            })
            .collect();
        self.directory
            .set_roots(&mut txn.flat, &changes)
            .expect("the roots are stated");
        for (merk, _, now) in moved {
            self.merks[merk].named = now;
        }
        (report, left)
    }

    /// The directory as the latest version of `db` has it.
    pub fn directory(&self, db: &Db) -> Directory {
        let pages = db.reader();
        Directory::load(&pages, self.directory.root_page(), db.config).expect("a directory")
    }

    /// Checks the nodes in use against the pages of the latest version:
    /// each is in its cell, with its key, its children and the roots of the
    /// Merks it hosts, and the directory opens each Merk in use at its
    /// root and no other.
    pub fn check(&self, db: &Db) {
        let pages = db.reader();
        let directory = self.directory(db);
        let reader = NodeReader::new(db.loader(), &GUARD, Lease::take(), db.latest());
        let mut buffers = LoadBuffers::new();
        let mut in_use: HashMap<Prefix, usize> = HashMap::new();
        for (index, merk) in self.merks.iter().enumerate() {
            if merk.alive {
                in_use.insert(merk.prefix, index);
            }
        }
        for merk in &self.merks {
            let entry = directory.open(&pages, &merk.prefix).expect("a lookup");
            match in_use.get(&merk.prefix) {
                None => assert!(entry.is_none(), "a dropped Merk is opened"),
                Some(&index) => {
                    let root = self.merks[index].root.and_then(|root| self.nodes[root].at);
                    let entry = entry.expect("a Merk in use is opened");
                    assert_eq!(entry.root().map(|root| root.address), root);
                }
            }
        }
        let at = |node: Option<usize>| node.map(|node| self.nodes[node].at.expect("an address"));
        for node in self.nodes.iter().filter(|node| node.alive) {
            let address = node.at.expect("a node in use has a cell");
            let pinned = reader.node(address, &mut buffers).expect("the cell");
            let cell = pinned.node();
            assert_eq!(cell.key(), node.key);
            let child = |side| cell.link(side).map(|link| link.child);
            assert_eq!(child(Side::Left), at(node.left));
            assert_eq!(child(Side::Right), at(node.right));
            let hosted: Vec<Option<CellAddress>> = cell.hosted_roots().collect();
            let expected: Vec<Option<CellAddress>> = node
                .hosted
                .iter()
                .map(|merk| at(merk.and_then(|merk| self.merks[merk].root)))
                .collect();
            assert_eq!(hosted, expected);
            if let CellValue::Inline(value) = cell.value() {
                assert_eq!(value, node.value);
            }
        }
    }
}
