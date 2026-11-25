//! The evaluation of a Merk's stale hashes inside a write transaction,
//! before its commit ([`Scope::evaluate`], [`WriteTxn::evaluate`]).
//!
//! This file is a part of the transaction's own module (`txn/txn.rs`
//! names it), because it works on the state that the transaction keeps
//! under its lock. [`crate::txn`], "An evaluation before the commit", is
//! its documentation.

use std::collections::{HashMap, HashSet};

use lignin_format::tree_page::{Hash, HashField};
use lignin_tree::{
    fragment::Pages,
    hash::{HashError, HashRules, Stale},
};

use super::*;
use crate::txn::{LinkPut, NodePut, RootPut};

/// What an evaluation of one Merk comes to, read from the frames before a
/// hash is made: the cells whose stale hashes it makes, and the Merks they
/// are of.
struct Plan {
    /// The Merk that is asked for, and then each Merk that the evaluation
    /// comes to through the pointer of a host whose value hash is stale,
    /// in the order it comes to them: the subtree, and the cell of the
    /// root.
    merks: Vec<(Subtree, CellAddress)>,
    /// For each of `merks`, the cells of its nodes that have a stale hash
    /// and that the evaluation comes to, in the order it comes to them.
    /// A Merk with none is read and not written: the hash of its root is
    /// made from the root's cell.
    nodes: Vec<Vec<CellAddress>>,
    /// The first host with a stale value hash that is behind the roots it
    /// hosts: its element is marked pending (`transactions.md` SC9), or
    /// its cell points at a cell that is the root of no Merk. The
    /// pointers of the first are not followed, and that pointer of the
    /// second is not.
    behind: Option<CellAddress>,
}

impl Plan {
    /// How many cells have a hash to make.
    fn cells(&self) -> usize {
        self.nodes.iter().map(Vec::len).sum()
    }

    /// The Merks with a hash to make, each by its place among `merks`.
    fn written(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.merks.len()).filter(|merk| !self.nodes[*merk].is_empty())
    }

    /// The Merks whose root is hashed and that have no hash to make.
    fn read(&self) -> impl Iterator<Item = Subtree> + '_ {
        let read = (0..self.merks.len()).filter(|merk| self.nodes[*merk].is_empty());
        read.map(|merk| self.merks[merk].0)
    }
}

/// A link of a node whose hashes were made, as its cell has it afterwards.
struct MadeLink {
    key: Vec<u8>,
    hash: Hash,
    heights: (u8, u8),
    aggregate: Vec<u8>,
}

/// A node whose hashes were made, as its cell has it afterwards.
struct Made {
    key: Vec<u8>,
    feature: Vec<u8>,
    value_hash: Hash,
    kv_hash: Hash,
    links: [Option<MadeLink>; 2],
}

impl Made {
    /// The node as a write of it states it: every hash made, and its
    /// value, its links and the Merks it hosts as they are.
    fn put(&self) -> NodePut<'_> {
        let [left, right] = self.links.each_ref().map(|link| {
            link.as_ref().map(|link| LinkPut {
                key: &link.key,
                hash: &link.hash,
                heights: link.heights,
                aggregate: &link.aggregate,
            })
        });
        NodePut {
            key: &self.key,
            feature: &self.feature,
            value_hash: &self.value_hash,
            kv_hash: &self.kv_hash,
            left,
            right,
            hosted: Hosted::Keep,
            value: ValuePut::Keep,
            stale: Stale::NONE,
            at: None,
        }
    }
}

/// Every tree type, as one that keeps flat records: what the operations
/// of an evaluation are laid out again by ([`Scope::apply_by_kind`]). An
/// evaluation writes a node only where one is, so the tree type of its
/// subtree refuses nothing of it.
fn every_kind(_: &[u8]) -> bool {
    true
}

/// The hash of the node at `at`, whose cell stores no stale hash: what
/// the rules make of the cell. The page is read and not copied.
fn node_hash<P: NodePages, R: HashRules>(
    pages: &mut P,
    rules: &mut R,
    at: CellAddress,
) -> Result<Hash, TxnError> {
    let made = pages.page(at.page(), |page| {
        page.node(at.slot()).map(|node| rules.node_hash(node))
    })?;
    let made = made.ok_or(NodeError::NoNode { address: at })?;
    made.map_err(|refused| TxnError::Hash(HashError::Rule(refused)))
}

/// Reads from `frames` what an evaluation of the Merk `subtree`, whose
/// root is at `root`, comes to. Nothing is changed.
///
/// It goes where the evaluation goes (`lignin_tree::hash`): down from the
/// root through stale hashes only, to the child on a side whose hash is
/// stale, and from a stale value hash to the root of each Merk the node
/// hosts. A cell with no stale hash is not read. Each cell with one is
/// passed once, so the walk ends with the stale hashes it can reach.
///
/// A Merk that a host points at is named by the subtree directory: the
/// record of the root's cell says which subtree it is the root of. A
/// pointer at a cell that the directory has no such record of is not
/// followed, and its host is taken to be behind.
fn reached(
    frames: &mut Awake<'_, '_>,
    subtree: Subtree,
    root: CellAddress,
) -> Result<Plan, TxnError> {
    let mut plan = Plan {
        merks: vec![(subtree, root)],
        nodes: vec![Vec::new()],
        behind: None,
    };
    let mut places = HashMap::from([(subtree, 0)]);
    let mut passed = HashSet::new();
    let mut ahead = vec![(0, root)];
    while let Some((merk, at)) = ahead.pop() {
        let stale = frames.hashes.stale(at);
        if stale.is_empty() || !passed.insert(at) {
            continue;
        }
        plan.nodes[merk].push(at);
        let hosts = stale.has(HashField::Value);
        let behind = hosts && frames.tree.host_is_behind(at);
        if behind {
            plan.behind.get_or_insert(at);
        }
        let read = frames.tree.page(at.page(), |page| {
            let node = page.node(at.slot())?;
            let children = Side::BOTH.map(|side| {
                let link = node.link(side);
                link.filter(|_| stale.has(HashField::Child(side)))
                    .map(|link| link.child)
            });
            let hosted: Vec<CellAddress> = match hosts && !behind {
                true => node.hosted_roots().flatten().collect(),
                false => Vec::new(),
            };
            Some((children, hosted))
        })?;
        let (children, hosted) = read.ok_or(NodeError::NoNode { address: at })?;
        ahead.extend(children.into_iter().flatten().map(|child| (merk, child)));
        for hosted_root in hosted {
            let owner = (frames.directory).root_owner(&*frames.flat, hosted_root)?;
            let Some(owner) = owner.map(Subtree::new) else {
                // The cell is the root of no Merk: of one that the
                // transaction cleared, whose host it has not written
                // since. The host is behind as one that is marked is.
                plan.behind.get_or_insert(at);
                continue;
            };
            let place = *places.entry(owner).or_insert_with(|| {
                plan.merks.push((owner, hosted_root));
                plan.nodes.push(Vec::new());
                plan.merks.len() - 1
            });
            ahead.push((place, hosted_root));
        }
    }
    Ok(plan)
}

/// Reads the cell at `at`, whose hashes were made, and then the key of
/// each of its children from the child's cell (`tree-page.md` TP10).
fn read_made(frames: &mut Awake<'_, '_>, at: CellAddress) -> Result<Made, TxnError> {
    let read = frames.tree.page(at.page(), |page| {
        let node = page.node(at.slot())?;
        let children = Side::BOTH.map(|side| node.link(side).map(|link| link.child));
        let links = Side::BOTH.map(|side| {
            node.link(side).map(|link| MadeLink {
                key: Vec::new(),
                hash: *link.hash,
                heights: link.heights,
                aggregate: link.aggregate.to_vec(),
            })
        });
        let made = Made {
            key: node.key().to_vec(),
            feature: node.feature().to_vec(),
            value_hash: *node.value_hash(),
            kv_hash: *node.kv_hash(),
            links,
        };
        Some((made, children))
    })?;
    let (mut made, children) = read.ok_or(NodeError::NoNode { address: at })?;
    for (link, child) in made.links.iter_mut().zip(children) {
        let (Some(link), Some(child)) = (link, child) else {
            continue;
        };
        let key = frames.tree.page(child.page(), |page| {
            page.node(child.slot()).map(|node| node.key().to_vec())
        })?;
        link.key = key.ok_or(NodeError::NoNode { address: child })?;
    }
    Ok(made)
}

/// Reads the nodes of `plan`, whose hashes were made, as their cells have
/// them, for each Merk of the plan. Each cell is the one its key leads to
/// from the root that the subtree directory has for the Merk it is kept
/// under, which a test build and a build with `held_reads` hold it to
/// (`pages::held`).
fn read_all(frames: &mut Awake<'_, '_>, plan: &Plan) -> Result<Vec<Vec<Made>>, TxnError> {
    let mut all = Vec::with_capacity(plan.merks.len());
    for (cells, (_subtree, _)) in plan.nodes.iter().zip(&plan.merks) {
        #[cfg(any(test, feature = "held_reads"))]
        let root = frames.root(_subtree)?;
        let mut nodes = Vec::with_capacity(cells.len());
        for at in cells {
            let node = read_made(frames, *at)?;
            #[cfg(any(test, feature = "held_reads"))]
            {
                let answer = held::Answer::Evaluated(*at);
                held::to_the_search(&mut frames.tree, _subtree, root, &node.key, answer)?;
            }
            nodes.push(node);
        }
        all.push(nodes);
    }
    Ok(all)
}

impl<'s> WriteTxn<'s> {
    /// [`Scope::evaluate`] and [`WriteTxn::evaluate`], for `who`.
    ///
    /// Nothing is changed before the hashes are made, but for the layout
    /// of the scope's own writes and the claims it is granted.
    pub(super) fn evaluate_in<R: HashRules>(
        &self,
        who: Who,
        subtree: Subtree,
        rules: &mut R,
    ) -> Result<Option<Hash>, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        if let Some(id) = who.id() {
            // The hashes are made in the cells of the transaction's
            // pages, so the scope's own writes are laid out first (LT4).
            self.lay(state, id)?;
        }
        let op = self.begin(state, false)?;
        if state.set.met() > 0 {
            return Err(BatchError::EvaluatedByKey { subtree }.into());
        }
        if state.frames.is_none() && !state.set.is_empty() {
            let refused = self.layout_refused(state, &op.guard);
            return Err(refused.unwrap_or(TxnError::Damaged(
                "a transaction with writes has no frames, and no layout of them was refused",
            )));
        }
        self.claim_read(state, &op.guard, op.at, who, Unit::data(subtree))?;
        let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) else {
            // The transaction has written nothing, so no hash is stale:
            // the Merk is the committed one.
            let Some(root) = self.seen(state, &op.guard, op.at, &subtree)? else {
                return Err(BatchError::NotEntered { subtree }.into());
            };
            let Some(root) = root else {
                return Ok(None);
            };
            let (guard, lease) = (&op.guard, state.lease);
            let mut pages = (self.path).committed_pages(guard, op.at, lease, &mut state.buffers);
            return node_hash(&mut pages, rules, root).map(Some);
        };
        let Some(entry) = frames.entry(&subtree)? else {
            return Err(BatchError::NotEntered { subtree }.into());
        };
        let Some(root) = entry.root().map(|root| root.address) else {
            return Ok(None);
        };
        let plan = reached(&mut frames, subtree, root)?;
        if plan.cells() == 0 {
            return node_hash(&mut frames.tree, rules, root).map(Some);
        }
        // A Merk with a host is anchored in a write that does not hold
        // the host: its root stays where the host's cell points. The
        // Merks after the first were come to through such a pointer.
        let hosted = entry.host().is_some();
        let anchored = |merk: usize| merk > 0 || hosted;
        drop(frames);

        // The claims are taken before a hash is made, all of them or none
        // (SC3), as those of a batch: one that holds a change set of every
        // Merk a hash is made in, and in which a node names, as Merks it
        // hosts, the others whose roots are hashed. The value hash of
        // their host is made from those roots, as the cell of a host
        // that names a Merk is written with its root: either reads the
        // Merk's unit. The node stands for nothing else, and the batch
        // is applied to nothing.
        let read: Vec<Subtree> = plan.read().collect();
        let none = [0; 32];
        let names = [NodePut {
            key: &[],
            feature: &[],
            value_hash: &none,
            kv_hash: &none,
            left: None,
            right: None,
            hosted: Hosted::Set(&read),
            value: ValuePut::Keep,
            stale: Stale::NONE,
            at: None,
        }];
        let stated: Vec<MerkWrite<'_>> = (plan.written())
            .map(|merk| MerkWrite {
                subtree: plan.merks[merk].0,
                // The Merk that is asked for is the first, and has a
                // hash to make.
                nodes: match merk {
                    0 => &names,
                    _ => &[],
                },
                removed: &[],
                root: RootPut::Keep,
                anchored: anchored(merk),
                whole: false,
            })
            .collect();
        let stated = Batch {
            merks: &stated,
            ..Batch::default()
        };
        match who.id() {
            Some(id) => self.claim_batch(state, &op, id, &stated)?,
            // A write goes through a scope (SC1), or is made while none
            // is open.
            None if state.scopes.any_open() => return Err(TxnError::ScopeOpen),
            None => {}
        }
        self.note_under(state, &op, &stated)?;
        if let Some(host) = plan.behind {
            return Err(TxnError::Hash(HashError::HostBehind { host }));
        }

        let woken = Woken::wake(&mut state.frames, &op.guard);
        let mut frames = woken.ok_or(TxnError::Damaged("the frames of an operation are gone"))?;
        let before = *frames.hashes.counters();
        let hash = {
            let Frames {
                tree,
                values,
                source,
                hashes,
                ..
            } = &mut *frames;
            hashes.hash_of(tree, &mut values.reader(source), rules, root)
        };
        let after = *frames.hashes.counters();
        let all = (after.nodes - before.nodes) as usize == plan.cells()
            && (plan.nodes.iter().flatten()).all(|at| frames.hashes.stale(*at).is_empty());
        let read = match hash {
            Ok(hash) if all => read_all(&mut frames, &plan).map(|made| (hash, made)),
            Ok(_) => Err(TxnError::Damaged(
                "an evaluation made other hashes than it was to make",
            )),
            Err(error) => Err(TxnError::Hash(error)),
        };
        let (hash, made) = match read {
            Ok(read) => read,
            Err(error) => {
                if after.written != before.written || after.nodes != before.nodes {
                    // The frames hold hashes that the write set does not:
                    // they are made again from it (WT3, WT10).
                    frames.discard();
                    state.renew_lease();
                }
                return Err(error);
            }
        };
        drop(frames);

        // What was made is a write of each node, of its hashes alone: one
        // operation for each Merk.
        for (merk, made) in made.iter().enumerate().filter(|(_, made)| !made.is_empty()) {
            let nodes: Vec<NodePut<'_>> = made.iter().map(Made::put).collect();
            let merks = [MerkWrite {
                subtree: plan.merks[merk].0,
                nodes: &nodes,
                removed: &[],
                root: RootPut::Keep,
                anchored: anchored(merk),
                whole: false,
            }];
            let batch = Batch {
                merks: &merks,
                ..Batch::default()
            };
            debug_assert!(check(&batch, false).is_ok(), "nodes that are there");
            (state.set).add(&batch, op.at, &[false], &[], &[], &[], &[]);
            state.set.tell_kinds(Some(every_kind));
            if let Some(id) = who.id() {
                state.set.tag_last(id, true);
            }
        }
        Ok(Some(hash))
    }
}
