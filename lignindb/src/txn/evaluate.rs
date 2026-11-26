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
    directory::record,
    hash::{HashError, HashRules, Refusal, Stale},
    node::NodeRef,
};

use super::*;
use crate::txn::{LinkPut, NodePut, RootPut};

/// A Merk that an evaluation comes to.
struct Reached {
    subtree: Subtree,
    /// The Merk's id in the subtree directory: the Merks that its nodes
    /// host are entered under it.
    id: MerkId,
    /// The cells of its nodes that have a stale hash and that the
    /// evaluation comes to, in the order it comes to them. A Merk with
    /// none is read and not written: the hash of its root is made from
    /// the root's cell.
    nodes: Vec<CellAddress>,
}

/// What an evaluation of one Merk comes to, read from the frames before a
/// hash is made.
struct Plan {
    /// The Merk that is asked for, and then each Merk that the evaluation
    /// comes to through a host whose value hash is stale, in the order it
    /// comes to them.
    merks: Vec<Reached>,
}

/// The refusal of a host with a stale value hash that is behind the Merks
/// it hosts: its element is marked pending as the caller is shown the
/// marks (`transactions.md` SC9), its cell points at a cell that the
/// subtree directory does not have as the root of a Merk entered under
/// the host there, or another open scope holds the Merk that is.
fn behind(host: CellAddress) -> TxnError {
    TxnError::Hash(HashError::HostBehind { host })
}

impl Plan {
    /// How many cells have a hash to make.
    fn cells(&self) -> usize {
        self.merks.iter().map(|merk| merk.nodes.len()).sum()
    }

    /// The Merks with a hash to make, each by its place among `merks`.
    fn written(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.merks.len()).filter(|merk| !self.merks[*merk].nodes.is_empty())
    }
}

/// A pointer of a host whose value hash is stale, with the Merk it is of:
/// the one that the subtree directory has entered under the host, at the
/// place of the pointer, or none where none is entered there. The caller
/// has no claim on the Merk yet.
struct Found {
    host: CellAddress,
    /// Where the host's cell points for the Merk's root.
    pointer: CellAddress,
    subtree: Option<Subtree>,
}

/// A pointer of a host with the Merk it is of, which the caller holds a
/// claim on.
struct Claimed {
    host: CellAddress,
    pointer: CellAddress,
    subtree: Subtree,
}

/// For each element whose pending mark a scope changed, by its subtree
/// and its key, whether the mark is set.
type Marks = HashMap<Subtree, HashMap<Box<[u8]>, bool>>;

/// The walk that reads what an evaluation comes to. It goes where the
/// evaluation goes (`lignin_tree::hash`): down from a cell through stale
/// hashes only, to the child on a side whose hash is stale, and from a
/// stale value hash to the root of each Merk the node hosts. A cell with
/// no stale hash is not read. Each cell with one is passed once.
///
/// It stops at each Merk that a host points into until the caller holds
/// a claim on it: no entry and no cell of a Merk is read before
/// (`transactions.md` SC3, SC4). A place where a host's cell names no
/// root is of a Merk with no node as the cell has it: nothing is read
/// for it, as the evaluation reads nothing.
///
/// It ends with the refusal of the first host that is behind, where it
/// finds that: a host that is marked when it comes to it, and a pointer
/// when its turn comes among the Merks that are claimed, so that the
/// caller holds the same claims whatever under the host refuses it.
struct Walk {
    plan: Plan,
    /// For each pending mark that the caller's scope changed, whether it
    /// is set: a copy of the scope's changes, made when the walk comes to
    /// its first host, for the length of the call.
    marks: Option<Marks>,
    /// Where each Merk of the plan stands in it.
    places: HashMap<Subtree, usize>,
    passed: HashSet<CellAddress>,
    /// The cells to go to, each with its Merk.
    ahead: Vec<(usize, CellAddress)>,
    /// The Merks that hosts were found to host and that are not claimed.
    found: Vec<Found>,
}

/// The Merk that the node `key` of the Merk with the id `parent` hosts at
/// `position`, as the subtree directory of `frames` has it.
fn hosted_at(
    frames: &Awake<'_, '_>,
    parent: MerkId,
    position: usize,
    key: &[u8],
) -> Result<Option<Subtree>, TxnError> {
    let many = TxnError::Damaged("a cell with more places for Merks than a host has");
    let position = u8::try_from(position).map_err(|_| many)?;
    let at = record::child_key(parent, position, key);
    let mut named = Vec::new();
    let tree = frames.directory.tree();
    let found = tree.get(&*frames.flat, &at, &mut named);
    if !found.map_err(DirectoryError::from)? {
        return Ok(None);
    }
    match record::decode_child(&named) {
        Ok((prefix, _)) => Ok(Some(Subtree::new(prefix))),
        Err(what) => Err(DirectoryError::Damaged {
            key: at,
            what: what.0,
        }
        .into()),
    }
}

impl Walk {
    /// A walk of the Merk `subtree`, which has the id `id` and its root at
    /// `root`: from the root, and from each of `apart`, the cells of nodes
    /// of the Merk with a stale hash that no stale hash need lead to.
    fn new(subtree: Subtree, id: MerkId, root: CellAddress, apart: &[CellAddress]) -> Self {
        let asked = Reached {
            subtree,
            id,
            nodes: Vec::new(),
        };
        let mut ahead: Vec<(usize, CellAddress)> = apart.iter().rev().map(|at| (0, *at)).collect();
        ahead.push((0, root));
        Self {
            plan: Plan { merks: vec![asked] },
            marks: None,
            places: HashMap::from([(subtree, 0)]),
            passed: HashSet::new(),
            ahead,
            found: Vec::new(),
        }
    }

    /// Goes on from the Merks that were found, which the caller holds a
    /// claim on by now: to the root of each, where its host points. A
    /// host that points elsewhere than the directory has the root is
    /// behind.
    fn follow(&mut self, frames: &Awake<'_, '_>, claimed: Vec<Claimed>) -> Result<(), TxnError> {
        for found in claimed {
            let subtree = found.subtree;
            let entry = frames.entry(&subtree)?;
            let root = (entry.as_ref()).and_then(|entry| entry.root().map(|root| root.address));
            let Some(entry) = entry.filter(|_| root == Some(found.pointer)) else {
                return Err(behind(found.host));
            };
            let merks = &mut self.plan.merks;
            let place = *self.places.entry(subtree).or_insert_with(|| {
                merks.push(Reached {
                    subtree,
                    id: entry.id(),
                    nodes: Vec::new(),
                });
                merks.len() - 1
            });
            self.ahead.push((place, found.pointer));
        }
        Ok(())
    }

    /// Whether the element under `key` of `subtree` is marked pending as
    /// `who` is shown it (`Scopes::is_pending`): by the newest change
    /// that its scope made of the mark, and otherwise by the marks that
    /// the merged scopes left. The scope's changes are gone through once
    /// for the call, when the first host is asked for.
    fn is_pending(&mut self, scopes: &Scopes, who: Who, subtree: Subtree, key: &[u8]) -> bool {
        let own = self.marks.get_or_insert_with(|| {
            let changes = who.id().map_or(&[][..], |id| scopes.marks_of(id));
            #[cfg(test)]
            counted::marks(changes.len());
            let mut own = Marks::new();
            for (set, (subtree, key)) in changes {
                own.entry(*subtree).or_default().insert(key.clone(), *set);
            }
            own
        });
        match own.get(&subtree).and_then(|own| own.get(key)) {
            Some(set) => *set,
            None => scopes.is_pending(Who::OUTSIDE, &(subtree, key.into())),
        }
    }

    /// Reads every cell that is ahead, and what it leads to in the Merks
    /// of the plan. The Merks that the hosts among them point into are
    /// left in `found`: which Merk a pointer is of is read from the
    /// subtree directory, under the host and the place of the pointer.
    ///
    /// The first host that is marked pending for `who` refuses the call.
    fn go(
        &mut self,
        frames: &mut Awake<'_, '_>,
        scopes: &Scopes,
        who: Who,
    ) -> Result<(), TxnError> {
        while let Some((merk, at)) = self.ahead.pop() {
            let stale = frames.hashes.stale(at);
            if stale.is_empty() || !self.passed.insert(at) {
                continue;
            }
            self.plan.merks[merk].nodes.push(at);
            let hosts = stale.has(HashField::Value);
            let read = frames.tree.page(at.page(), |page| {
                let node = page.node(at.slot())?;
                let children = Side::BOTH.map(|side| {
                    let link = node.link(side);
                    link.filter(|_| stale.has(HashField::Child(side)))
                        .map(|link| link.child)
                });
                let hosted = hosts.then(|| {
                    let pointers: Vec<Option<CellAddress>> = node.hosted_roots().collect();
                    (node.key().to_vec(), pointers)
                });
                Some((children, hosted))
            })?;
            let (children, hosted) = read.ok_or(NodeError::NoNode { address: at })?;
            self.ahead
                .extend(children.into_iter().flatten().map(|child| (merk, child)));
            let Some((key, pointers)) = hosted else {
                continue;
            };
            let (subtree, id) = (self.plan.merks[merk].subtree, self.plan.merks[merk].id);
            if self.is_pending(scopes, who, subtree, &key) {
                return Err(behind(at));
            }
            for (position, pointer) in pointers.into_iter().enumerate() {
                let Some(pointer) = pointer else {
                    continue;
                };
                // Where no Merk is entered under the host there, the one
                // whose root the cell names was dropped, or moved from
                // under the host: the host is behind. Which Merk has its
                // root in a cell depends on what was laid out, and is
                // not asked.
                self.found.push(Found {
                    host: at,
                    pointer,
                    subtree: hosted_at(frames, id, position, &key)?,
                });
            }
        }
        Ok(())
    }
}

/// The caller's rules, with a count of the calls: the hashes the tree
/// layer computes are counted for the store as those of a commit are.
struct Counted<'r, R> {
    rules: &'r mut R,
    calls: u64,
}

impl<R: HashRules> HashRules for Counted<'_, R> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.node_hash(node)
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
    for merk in &plan.merks {
        #[cfg(any(test, feature = "held_reads"))]
        let root = frames.root(&merk.subtree)?;
        let mut nodes = Vec::with_capacity(merk.nodes.len());
        for at in &merk.nodes {
            let node = read_made(frames, *at)?;
            #[cfg(any(test, feature = "held_reads"))]
            {
                let answer = held::Answer::Evaluated(*at);
                held::to_the_search(&mut frames.tree, &merk.subtree, root, &node.key, answer)?;
            }
            nodes.push(node);
        }
        all.push(nodes);
    }
    Ok(all)
}

/// Holds what a walk passed in the Merk `subtree`, whose root is at
/// `root`, to every key the transaction has put a node under there: a
/// cell that has a stale hash was passed. So no stale hash of the Merk is
/// left by the call. It is so in a test build and in one with
/// `held_reads`, and the searches are not counted as keyed lookups.
#[cfg(any(test, feature = "held_reads"))]
fn hold_passed(
    frames: &mut Awake<'_, '_>,
    set: &WriteSet,
    subtree: Subtree,
    root: CellAddress,
    passed: &HashSet<CellAddress>,
) -> Result<(), TxnError> {
    for key in set.nodes_by_every_key(subtree) {
        let Some(at) = held::sought(&mut frames.tree, Some(root), key)? else {
            continue;
        };
        assert!(
            frames.hashes.stale(at).is_empty() || passed.contains(&at),
            "the node {key:02x?} of {subtree:?} has a stale hash that the call does not come to",
        );
    }
    Ok(())
}

impl<'s> WriteTxn<'s> {
    /// [`Scope::evaluate`] and [`WriteTxn::evaluate`], for `who`.
    pub(super) fn evaluate_in<R: HashRules>(
        &self,
        who: Who,
        subtree: Subtree,
        rules: &mut R,
    ) -> Result<Option<Hash>, TxnError> {
        let mut rules = Counted { rules, calls: 0 };
        let hash = self.evaluate_counted(who, subtree, &mut rules);
        self.path.stats.count_hashes(rules.calls);
        hash
    }

    /// Nothing is changed before the hashes are made, but for the layout
    /// of the scope's own writes and the claims it is granted.
    fn evaluate_counted<R: HashRules>(
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
        let gone = || TxnError::Damaged("the frames of an operation are gone");
        let (root, hosted, mut walk, apart) = {
            let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) else {
                // The transaction has written nothing, so no hash is
                // stale: the Merk is the committed one.
                let Some(root) = self.seen(state, &op.guard, op.at, &subtree)? else {
                    return Err(BatchError::NotEntered { subtree }.into());
                };
                let Some(root) = root else {
                    return Ok(None);
                };
                let (guard, lease) = (&op.guard, state.lease);
                let mut pages =
                    (self.path).committed_pages(guard, op.at, lease, &mut state.buffers);
                return node_hash(&mut pages, rules, root).map(Some);
            };
            let Some(entry) = frames.entry(&subtree)? else {
                return Err(BatchError::NotEntered { subtree }.into());
            };
            let Some(root) = entry.root().map(|root| root.address) else {
                return Ok(None);
            };
            // A stale hash of the Merk that no stale hash above it leads
            // to is found by the write that left it: the write set lists
            // those, and the key of each leads to its cell.
            let mut apart = Vec::new();
            for (_, key) in state.set.unmade(subtree) {
                let (at, _) = frames.sought(&subtree, Some(root), key)?;
                apart.extend(at.filter(|at| !frames.hashes.stale(*at).is_empty()));
            }
            let walk = Walk::new(subtree, entry.id(), root, &apart);
            (root, entry.host().is_some(), walk, apart)
        };
        // The walk stops at each Merk that a host hosts, and the claim on
        // it is taken before anything of it is read.
        let mut claimed = Vec::new();
        loop {
            let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
            walk.follow(&frames, std::mem::take(&mut claimed))?;
            walk.go(&mut frames, &state.scopes, who)?;
            drop(frames);
            if walk.found.is_empty() {
                break;
            }
            for found in std::mem::take(&mut walk.found) {
                // A pointer with no Merk under its host, and one into a
                // Merk that another open scope holds, refuse the call
                // alike: which of the two the caller meets depends on
                // whether that scope's writes are laid out, and so does
                // the Merk it would be told of.
                let subtree = found.subtree.ok_or_else(|| behind(found.host))?;
                let read = self.claim_read(state, &op.guard, op.at, who, Unit::data(subtree));
                match read {
                    Err(TxnError::ScopeOverlap { .. }) => return Err(behind(found.host)),
                    read => read?,
                }
                claimed.push(Claimed {
                    host: found.host,
                    pointer: found.pointer,
                    subtree,
                });
            }
        }
        #[cfg(any(test, feature = "held_reads"))]
        {
            let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
            hold_passed(&mut frames, &state.set, subtree, root, &walk.passed)?;
        }
        let plan = walk.plan;
        if plan.cells() == 0 {
            // No hash of the Merk is stale: its root is read.
            let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
            return node_hash(&mut frames.tree, rules, root).map(Some);
        }
        // A Merk with a host is anchored in a write that does not hold
        // the host: its root stays where the host's cell points. The
        // Merks after the first were come to through such a pointer.
        let anchored = |merk: usize| merk > 0 || hosted;

        // Nothing refuses the call from here on but a claim. The write
        // claims are taken before a hash is made, all of them or none
        // (SC3), as those of a batch that holds a change set of every
        // Merk a hash is made in. The batch is applied to nothing.
        let stated: Vec<MerkWrite<'_>> = (plan.written())
            .map(|merk| MerkWrite {
                subtree: plan.merks[merk].subtree,
                nodes: &[],
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

        let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
        let before = *frames.hashes.counters();
        // A host that is behind for the caller refused the call above. A
        // mark of another scope that is still open is not shown to it
        // (SC9), and the caller holds every Merk that a host it comes to
        // points into: the tree layer is told of no host that is behind.
        let behind = std::mem::take(&mut frames.tree.behind);
        let hash = {
            let Frames {
                tree,
                values,
                source,
                hashes,
                ..
            } = &mut *frames;
            let mut values = values.reader(source);
            let hash = hashes.hash_of(tree, &mut values, rules, root);
            hash.and_then(|hash| {
                for at in &apart {
                    if !hashes.stale(*at).is_empty() {
                        hashes.evaluate(tree, &mut values, rules, *at)?;
                    }
                }
                Ok(hash)
            })
        };
        frames.tree.behind = behind;
        let after = *frames.hashes.counters();
        let nodes = plan.merks.iter().flat_map(|merk| &merk.nodes);
        let all = (after.nodes - before.nodes) as usize == plan.cells()
            && nodes
                .into_iter()
                .all(|at| frames.hashes.stale(*at).is_empty());
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
                subtree: plan.merks[merk].subtree,
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

/// How many changes of pending marks the walks of this thread went
/// through, for the tests.
#[cfg(test)]
pub(crate) mod counted {
    use std::cell::Cell;

    thread_local! {
        static MARKS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn marks(changes: usize) {
        MARKS.with(|marks| marks.set(marks.get() + changes));
    }

    /// The count since it was last taken.
    pub(crate) fn take_marks() -> usize {
        MARKS.with(|marks| marks.replace(0))
    }
}
