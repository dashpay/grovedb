//! The evaluation of a Merk's stale hashes inside a write transaction,
//! before its commit ([`Scope::evaluate`], [`WriteTxn::evaluate`]).
//!
//! This file is a part of the transaction's own module (`txn/txn.rs`
//! names it), because it works on the state that the transaction keeps
//! under its lock. [`crate::txn`], "An evaluation before the commit", is
//! its documentation.

use std::collections::{HashMap, HashSet};

use lignin_format::{
    tree_page::{Hash, HashField},
    PageId,
};
use lignin_tree::{
    directory::record,
    fragment::{PageMut, Pages},
    hash::{HashError, HashRules, Refusal, Stale},
    node::{NodePage, NodeRef},
};

use super::*;
use crate::txn::{pages::TreePages, LinkPut, NodePut, RootPut};

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
    /// The keys of the nodes of the Merk that is asked for whose cells
    /// were passed on the way down from its root: they are asked for once
    /// that walk has ended, and none is kept afterwards.
    reached: HashSet<Vec<u8>>,
    /// Whether the walk has gone on from cells beside the root's
    /// ([`Self::start`]).
    apart: bool,
    /// The cells to go to, each with its Merk.
    ahead: Vec<(usize, CellAddress)>,
    /// The Merks that hosts were found to host and that are not claimed.
    found: Vec<Found>,
    /// The pages of the cells the walk went to, with a stale hash or with
    /// none: an evaluation reads no other ([`Evaluating`]).
    #[cfg(any(test, feature = "held_reads"))]
    looked: HashSet<PageId>,
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
    /// `root`, from the root.
    fn new(subtree: Subtree, id: MerkId, root: CellAddress) -> Self {
        let asked = Reached {
            subtree,
            id,
            nodes: Vec::new(),
        };
        Self {
            plan: Plan { merks: vec![asked] },
            marks: None,
            places: HashMap::from([(subtree, 0)]),
            passed: HashSet::new(),
            reached: HashSet::new(),
            apart: false,
            ahead: vec![(0, root)],
            found: Vec::new(),
            #[cfg(any(test, feature = "held_reads"))]
            looked: HashSet::new(),
        }
    }

    /// Has the walk go on from each of `apart`: cells of nodes of the
    /// Merk that is asked for.
    fn start(&mut self, apart: &[CellAddress]) {
        self.apart = true;
        self.ahead.extend(apart.iter().rev().map(|at| (0, *at)));
    }

    /// Goes on from a Merk that was found, which the caller holds a claim
    /// on by now: to its root, where its host points. A host that points
    /// elsewhere than the directory has the root is behind.
    fn follow(&mut self, frames: &Awake<'_, '_>, found: Claimed) -> Result<(), TxnError> {
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
        let pending = match own.get(&subtree).and_then(|own| own.get(key)) {
            Some(set) => *set,
            None => scopes.is_pending(Who::OUTSIDE, &(subtree, key.into())),
        };
        // The scope's own answer goes through its changes for each key.
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(
            pending,
            scopes.is_pending(who, &(subtree, key.into())),
            "the mark of {key:02x?} in {subtree:?} as the caller's scope is shown it",
        );
        pending
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
            #[cfg(any(test, feature = "held_reads"))]
            self.looked.insert(at.page());
            let stale = frames.hashes.stale(at);
            if stale.is_empty() || !self.passed.insert(at) {
                continue;
            }
            self.plan.merks[merk].nodes.push(at);
            let hosts = stale.has(HashField::Value);
            let reaches = merk == 0 && !self.apart;
            let read = frames.tree.page(at.page(), |page| {
                let node = page.node(at.slot())?;
                let children = Side::BOTH.map(|side| {
                    let link = node.link(side);
                    link.filter(|_| stale.has(HashField::Child(side)))
                        .map(|link| link.child)
                });
                let key = (hosts || reaches).then(|| node.key().to_vec());
                let pointers = hosts.then(|| node.hosted_roots().collect::<Vec<_>>());
                Some((children, key, pointers))
            })?;
            let (children, key, pointers) = read.ok_or(NodeError::NoNode { address: at })?;
            self.ahead
                .extend(children.into_iter().flatten().map(|child| (merk, child)));
            let Some(key) = key else {
                continue;
            };
            if let Some(pointers) = pointers {
                let (subtree, id) = (self.plan.merks[merk].subtree, self.plan.merks[merk].id);
                if self.is_pending(scopes, who, subtree, &key) {
                    return Err(behind(at));
                }
                for (position, pointer) in pointers.into_iter().enumerate() {
                    let Some(pointer) = pointer else {
                        continue;
                    };
                    // Where no Merk is entered under the host there, the
                    // one whose root the cell names was dropped, or moved
                    // from under the host: the host is behind. Which Merk
                    // has its root in a cell depends on what was laid
                    // out, and is not asked.
                    self.found.push(Found {
                        host: at,
                        pointer,
                        subtree: hosted_at(frames, id, position, &key)?,
                    });
                }
            }
            if reaches {
                self.reached.insert(key);
            }
        }
        Ok(())
    }
}

/// The caller's rules, with a count of the calls: the hashes the tree
/// layer computes are counted for the store as those of a commit are.
/// The count is told to the store when this is dropped, so on every way
/// out of the call: with rules that panic, the calls before the panic are
/// counted.
struct Counted<'r, R> {
    rules: &'r mut R,
    calls: u64,
    stats: &'r Stats,
}

impl<R> Drop for Counted<'_, R> {
    fn drop(&mut self) {
        self.stats.count_hashes(self.calls);
    }
}

/// The transaction's pages as an evaluation reads and changes them.
///
/// A page is read where it is, and no private copy is made for a read, as
/// none is for the reads of the transaction. A cell with a stale hash is
/// in a private copy already: a write of the transaction left the mark.
/// A cell that is only read, for the hash of a child or of the root of a
/// hosted Merk, is of a Merk the caller holds, and is read once. A hash
/// is stored in the private copy of its page.
///
/// No host is behind for it: the walk refused the call for each that is
/// behind as the caller is shown the marks (`transactions.md` SC9), and
/// the pages' own answer, which is for a write, counts the marks of every
/// scope whose writes they show.
///
/// In a test build and under `held_reads`, each page that is read is held
/// to the walk: it is the page of a cell the walk went to.
struct Evaluating<'a, 's> {
    pages: &'a mut TreePages<'s>,
    #[cfg(any(test, feature = "held_reads"))]
    looked: &'a HashSet<PageId>,
}

impl Pages for Evaluating<'_, '_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        #[cfg(any(test, feature = "held_reads"))]
        assert!(
            self.looked.contains(&page),
            "an evaluation reads the page {page:?}, which holds no cell its walk went to",
        );
        (self.pages.page(page, read)).map_err(TxnError::into_pages)
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        self.pages.write(page, change)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        self.pages.create()
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        self.pages.free(page)
    }
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

/// The frames of an operation are there while it runs.
fn gone() -> TxnError {
    TxnError::Damaged("the frames of an operation are gone")
}

impl<'s> WriteTxn<'s> {
    /// [`Scope::evaluate`] and [`WriteTxn::evaluate`], for `who`.
    pub(super) fn evaluate_in<R: HashRules>(
        &self,
        who: Who,
        subtree: Subtree,
        rules: &mut R,
    ) -> Result<Option<Hash>, TxnError> {
        let mut rules = Counted {
            rules,
            calls: 0,
            stats: self.path.stats,
        };
        self.evaluate_counted(who, subtree, &mut rules)
    }

    /// Goes on with `walk` until nothing is ahead of it. The walk stops at
    /// each Merk that a host points into: a read of the Merk is claimed,
    /// and only then is its entry read and its root gone to, where the
    /// host points at it. Each is claimed and looked at before the next
    /// is claimed, so a call that a host refuses holds no Merk it did not
    /// open.
    fn walk_on(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        who: Who,
        walk: &mut Walk,
    ) -> Result<(), TxnError> {
        loop {
            let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
            walk.go(&mut frames, &state.scopes, who)?;
            drop(frames);
            if walk.found.is_empty() {
                return Ok(());
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
                let frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
                let claimed = Claimed {
                    host: found.host,
                    pointer: found.pointer,
                    subtree,
                };
                walk.follow(&frames, claimed)?;
            }
        }
    }

    /// Nothing is changed before the hashes are made, but for the layout
    /// of the scope's own writes and the read claims it is granted.
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
        let (root, hosted, mut walk) = {
            let Some(frames) = Woken::wake(&mut state.frames, &op.guard) else {
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
            let walk = Walk::new(subtree, entry.id(), root);
            (root, entry.host().is_some(), walk)
        };
        // What the root leads to through stale hashes, first: the walk
        // comes to those cells by their addresses, with no search.
        self.walk_on(state, &op, who, &mut walk)?;
        // A stale hash of the Merk that no stale hash above it leads to
        // is found by the write that left it: the write set lists those,
        // and the key of each that the walk did not come to leads to its
        // cell.
        let mut apart = Vec::new();
        {
            let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
            for (_, key) in state.set.unmade(subtree) {
                if walk.reached.contains(key) {
                    continue;
                }
                let (at, _) = frames.sought(&subtree, Some(root), key)?;
                let left = |at: &CellAddress| {
                    !frames.hashes.stale(*at).is_empty() && !walk.passed.contains(at)
                };
                apart.extend(at.filter(left));
            }
        }
        if !apart.is_empty() {
            walk.start(&apart);
            self.walk_on(state, &op, who, &mut walk)?;
        }
        #[cfg(any(test, feature = "held_reads"))]
        {
            let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
            hold_passed(&mut frames, &state.set, subtree, root, &walk.passed)?;
        }
        #[cfg(any(test, feature = "held_reads"))]
        let looked = walk.looked;
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

        // No host and no read has refused the call. The write claims are
        // those of a batch that holds a change set of every Merk a hash
        // is made in, and is applied to nothing. They are asked for now,
        // before a hash is made, all of them (SC3), and are taken once
        // the hashes are made: a rule that refuses a hash, and a page
        // that is not read, leave the caller the read claims it had. The
        // transaction's lock is held from the one to the other, so what
        // is granted here is granted then.
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
            Some(id) => {
                let units: Vec<Unit> = (stated.merks.iter())
                    .map(|merk| Unit::data(merk.subtree))
                    .collect();
                self.check_writes(state, &op, id, &units)?;
            }
            // A write goes through a scope (SC1), or is made while none
            // is open.
            None if state.scopes.any_open() => return Err(TxnError::ScopeOpen),
            None => {}
        }
        self.note_under(state, &op, &stated)?;

        let mut frames = Woken::wake(&mut state.frames, &op.guard).ok_or_else(gone)?;
        let before = *frames.hashes.counters();
        let hash = {
            let Unbound {
                tree,
                values,
                source,
                hashes,
                ..
            } = &mut **frames;
            let mut values = values.reader(source);
            let mut pages = Evaluating {
                pages: tree,
                #[cfg(any(test, feature = "held_reads"))]
                looked: &looked,
            };
            let hash = hashes.hash_of(&mut pages, &mut values, rules, root);
            hash.and_then(|hash| {
                for at in &apart {
                    if !hashes.stale(*at).is_empty() {
                        hashes.evaluate(&mut pages, &mut values, rules, *at)?;
                    }
                }
                Ok(hash)
            })
        };
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
        let changed = after.written != before.written || after.nodes != before.nodes;
        drop(frames);
        let taken = read.and_then(|read| match who.id() {
            Some(id) => self.claim_batch(state, &op, id, &stated).map(|()| read),
            None => Ok(read),
        });
        let (hash, made) = match taken {
            Ok(read) => read,
            Err(error) => {
                if changed {
                    // The frames hold hashes that the write set does not:
                    // they are made again from it (WT3, WT10).
                    if let Some(frames) = Woken::wake(&mut state.frames, &op.guard) {
                        frames.discard();
                    }
                    state.renew_lease();
                }
                return Err(error);
            }
        };

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
