//! Lazy hashing: which stored hashes are stale, and their evaluation on
//! demand.
//!
//! - [`Hashes`] keeps the marks of one write scope: for each node, which of
//!   the hashes its cell stores are stale ([`Stale`]). When it is asked, it
//!   makes each marked hash once and writes it into its cell.
//! - [`HashRules`] are the functions that make a hash. They are given by the
//!   caller, so nothing here knows a rule of Merk.
//! - [`ValueReader`] reads a value that is not in its cell.
//! - [`check`] recomputes the hashes of one cell and says which differ.
//! - [`made_with_other`] says of a value hash that differs whether it is
//!   one the rules name as made with another hash of a hosted root.
//!
//! # What is stored, and what is stale
//!
//! Every node's value hash, key-value hash and child hashes are in its cell
//! (`tree-page.md` TP9) and are read from it, with
//! [`NodeRef`](crate::node::NodeRef). Nothing here stands between a reader
//! and those bytes.
//!
//! **Loading a page computes no hash.** The rules are called by an
//! evaluation, which someone asked for, and by [`check`]. An evaluation
//! makes what is marked, and a mark is made by [`Hashes::mark`], which its
//! caller calls for a change it made, and by nothing else. A page that is
//! read, loaded or pinned gets no mark, so nothing is computed for it.
//!
//! A mark is one flag for one stored hash ([`HashField`]) of one node, kept
//! in memory under the address of the node's cell. It is never in a page: a
//! commit evaluates every mark first (`transactions.md` CM3, step 5), so no
//! committed page has a stale hash and the page format has no such flag.
//!
//! A stored hash is **stale** when it may differ from what the rules make
//! of the pages as they are. Whoever changes what a hash is made from marks
//! that hash, in the same operation:
//!
//! | What changed | The mark |
//! | --- | --- |
//! | a node's value | its value hash, and with it its key-value hash |
//! | a node's value hash, by its writer | its key-value hash, unless the writer states that too |
//! | what the hash of a node is made from: its key-value hash, the hash of a child, and whatever else of its cell the rules take in | the hash of that node in the link of its parent, or for the root of a Merk the value hash of its host |
//! | a link names another child, or appears | the hash of that child |
//! | a hosted Merk has another root, has none any more, or has one again | the value hash of the host |
//! | a node is new, and its writer did not make its hashes | every hash its cell stores |
//!
//! So the marks of a change run upwards from the node that changed to
//! wherever its writer stops: the root of the grove, or a stop point. A
//! writer that makes a hash itself and writes it, as Merk does on existing
//! GroveVersions, does not mark it, and takes its mark off if it had one
//! ([`Hashes::unmark`]). The value hash of a reference is always such a
//! hash: it is made from a value in another place, which no rule here is
//! given.
//!
//! Heights, aggregates and features are not hashes and are never marked:
//! their writer keeps them exact in every write.
//!
//! # Evaluation
//!
//! Nothing is evaluated until it is asked for:
//!
//! - [`Hashes::evaluate`] brings one node up to date, with everything that
//!   is marked below it.
//! - [`Hashes::hash_of`] does that and returns the node's own hash, which
//!   no cell of its own stores: the root hash of a Merk, when the node is
//!   its root.
//! - [`Hashes::evaluate_all`] leaves no mark, which is what a commit needs.
//!
//! An evaluation goes down from the node it was asked for **through marked
//! hashes only**. For a marked hash of a child it first brings the child up
//! to date and then makes the child's hash; for a marked value hash it does
//! the same with the root of each Merk the node hosts. It reads no cell that
//! is neither marked nor pointed at by a marked hash, so its cost follows
//! the marks, not the tree.
//!
//! **Each marked hash is made once.** The marks of a node are taken off
//! when all of its marked hashes are in its cell, so a second evaluation
//! finds nothing to do. After a change to one value and one request for the
//! root hash, the rules were called for exactly the nodes on the path from
//! that value to the root: the value hash and the key-value hash of the
//! node and of every host above it, and the hash of every node of the path.
//! A node that many changes pass through is marked by each and made once.
//! An evaluation that an error stops has made hashes it did not write: the
//! hash of a node whose parent was not written, or a value hash whose
//! key-value hash was refused. Those are made again when it is asked for
//! again. What was written is not.
//!
//! **A hash that changed is one hash record.** A hash is written with
//! [`NodeMut::set_hash`](crate::node::NodeMut::set_hash), so it is told to
//! the page's [`ChangeSink`](crate::node::ChangeSink) as the 35-byte record
//! of `page-delta.md`. It overwrites 32 bytes where they are: no cell
//! changes its length, no value is touched and no handle ends. A hash that
//! is made and equals the stored one is not written.
//!
//! **The hash of a hosted root is in no cell** (`tree-layout.md` C3). So
//! making the value hash of a host makes the hash of the root of each Merk
//! it hosts, also of one that did not change. That is one call of
//! [`HashRules::node_hash`] on the root's cell and reads nothing below it.
//! For a host of one Merk the root is on the path anyway. For a host of
//! several, the roots beside the path are the only nodes off the path that
//! a rule is called for.
//!
//! **One page at a time.** Every step is one call of
//! [`Pages`] on one page (`transactions.md` LT3):
//! a marked node is read once and written once, and a node that is not
//! marked and whose hash is needed is read once. A value that is not in its
//! cell is read between two steps, never inside one.
//!
//! **No allocation for each node.** What the open nodes of an evaluation
//! keep is in lists they share, which grow with the depth of what is
//! marked and are allocated once for an evaluation. A host's value hash
//! uses one more of them for the roots it hosts.
//!
//! **An error loses no mark.** Whatever stops an evaluation
//! ([`HashError`]), every stale hash is still marked afterwards and every
//! hash that was written is right, so the evaluation can be asked for
//! again.
//!
//! # Stop points
//!
//! A write that stops at a stop point marks up to there and no further, and
//! [`Hashes::hash_of`] of the node it stopped at evaluates what is below
//! and touches nothing above. That is all the engine does for a stop point:
//! what is above is marked by the write that later carries the change up,
//! and evaluated when that is asked for.
//!
//! A host whose element is still to be written is behind
//! ([`Pages::host_is_behind`]). Its
//! pointers are not followed and its value hash is not made: an evaluation
//! that reaches a marked value hash of such a host stops with
//! [`HashError::HostBehind`].
//!
//! # With fragment maintenance
//!
//! A [`Write`] states each of its nodes whole, hashes included. For a hash
//! that is marked or is about to be, it states the bytes the cell has, or
//! any bytes where the cell has none yet, in a node it creates or a link
//! that appears. Fragment maintenance then writes no hash record for a
//! hash the cell has, and evaluation writes the one record.
//!
//! A write moves cells, so the marks follow it: [`Hashes::follow`] is called
//! with every write and its [`Report`], before anything is marked for that
//! write. The addresses to mark afterwards are the report's.
//!
//! # What the callers do
//!
//! - **One writer.** Nothing else changes the pages during an evaluation,
//!   as during a write. An evaluation reads a marked cell in one step and
//!   writes it in a later one. What it keeps from the first step is what
//!   the cell points at: the child on a marked side, the roots it hosts and
//!   the place of a value that is not in the cell. It compares those in
//!   the second step, and a cell that points elsewhere is refused
//!   ([`HashError::Changed`]), so no hash is made from a pointer the cell
//!   no longer has. Everything else of the cell is read in the step that
//!   writes it. This guards one cell and is no test of the rule: a change
//!   to another cell between two steps is not seen.
//! - **Marks are complete.** A change that is not marked leaves a stale
//!   hash that nothing finds. [`check`] is the test of it: at every moment,
//!   what it reports for a node is marked for that node.
//! - **A marked hash is not read as a hash.** A reader inside the scope
//!   asks [`Hashes::stale`], or evaluates first.
//! - **The marks belong to their frames.** They are a part of the write
//!   scope that holds the private copies. A frame that is put back takes
//!   its marks back with it: the holder keeps a copy of the marks with each
//!   savepoint ([`Hashes`] is `Clone`). A scope that merges gives its marks
//!   to the transaction ([`Hashes::merge`]). Marks are addresses in one
//!   line of pages, so writes that are applied again on other pages are
//!   marked again.
//! - **Commit evaluates all** ([`Hashes::evaluate_all`]), after the tree is
//!   finalized, so no host is behind.
//! - **A dropped Merk** keeps its cells, and the marks in them. They are
//!   evaluated with the others, which costs their hashes and changes no
//!   hash of a live node, unless their holder takes them off
//!   ([`Hashes::forget`]).

mod error;
mod evaluate;
mod rules;
mod stale;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use lignin_format::{
    tree_page::{Hash, HashField, Side},
    CellAddress,
};

pub use error::HashError;
pub use rules::{HashRules, NoValueReader, Refusal, ValueReader};
pub use stale::Stale;

use crate::fragment::{Pages, Report, Write};

/// The marks of one node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Marks {
    stale: Stale,
    /// Whether the node is being evaluated: only inside an evaluation.
    open: bool,
}

/// What the evaluations of one [`Hashes`] did, in all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// The marked nodes that were brought up to date.
    pub nodes: u64,
    /// The value hashes that were made.
    pub value_hashes: u64,
    /// The key-value hashes that were made.
    pub kv_hashes: u64,
    /// The hashes of nodes that were made: for the link of a parent, for the
    /// value hash of a host, or for whoever asked.
    pub node_hashes: u64,
    /// The hashes that were written into their cells, each as one hash
    /// record.
    pub written: u64,
    /// The hashes that were made and equal the stored ones, and were not
    /// written.
    pub unchanged: u64,
}

impl Counters {
    fn add(&mut self, other: &Self) {
        self.nodes += other.nodes;
        self.value_hashes += other.value_hashes;
        self.kv_hashes += other.kv_hashes;
        self.node_hashes += other.node_hashes;
        self.written += other.written;
        self.unchanged += other.unchanged;
    }
}

/// The stale hashes of one write scope, and their evaluation.
///
/// It holds no page. Between two calls it keeps the marks and its counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hashes {
    marks: BTreeMap<CellAddress, Marks>,
    counters: Counters,
}

impl Hashes {
    /// No mark.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the hash `field` that the cell at `node` stores as stale. A
    /// value hash takes the key-value hash with it, which is made from it.
    pub fn mark(&mut self, node: CellAddress, field: HashField) {
        let marks = self.marks.entry(node).or_default();
        marks.stale = marks.stale.with(field);
        if field == HashField::Value {
            marks.stale = marks.stale.with(HashField::KeyValue);
        }
    }

    /// Takes the mark off the hash `field` of the cell at `node`: its
    /// writer made the hash and wrote it. No other mark of the node goes
    /// with it.
    pub fn unmark(&mut self, node: CellAddress, field: HashField) {
        if let Some(marks) = self.marks.get_mut(&node) {
            marks.stale = marks.stale.without(field);
            if marks.stale.is_empty() {
                self.marks.remove(&node);
            }
        }
    }

    /// Takes every mark off the cell at `node` and returns what was marked:
    /// the node is gone, or its hashes are of no use to anyone.
    pub fn forget(&mut self, node: CellAddress) -> Stale {
        self.marks.remove(&node).unwrap_or_default().stale
    }

    /// Takes every mark off.
    pub fn clear(&mut self) {
        self.marks.clear();
    }

    /// Which hashes of the cell at `node` are marked.
    pub fn stale(&self, node: CellAddress) -> Stale {
        self.marks.get(&node).copied().unwrap_or_default().stale
    }

    /// How many nodes have a marked hash.
    pub fn len(&self) -> usize {
        self.marks.len()
    }

    /// Whether no hash is marked.
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// The nodes that have a marked hash, in the order of their addresses.
    pub fn marked(&self) -> impl Iterator<Item = (CellAddress, Stale)> + '_ {
        self.marks.iter().map(|(node, marks)| (*node, marks.stale))
    }

    /// What the evaluations so far did.
    pub const fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Takes over the marks and the counts of `other`: a scope that merges
    /// into its transaction (`transactions.md` SC6). Both were made on the
    /// same line of pages.
    pub fn merge(&mut self, other: Self) {
        for (node, marks) in other.marks {
            let own = self.marks.entry(node).or_default();
            own.stale = own.stale.union(marks.stale);
        }
        self.counters.add(&other.counters);
    }

    /// Takes the marks through a write that was applied: `write`, and what
    /// [`Fragments::apply`](crate::fragment::Fragments::apply) reported for
    /// it.
    ///
    /// - The marks of a cell the write removed are taken off.
    /// - The mark of the hash of a child goes with the link, in a node that
    ///   the write leaves without a child on that side.
    /// - The marks of a cell that was carried are under its new address.
    ///
    /// Call it for every write, before marking anything the write changed:
    /// a mark names its cell by the address the pages have now.
    pub fn follow(&mut self, write: &Write<'_>, report: &Report) {
        if self.marks.is_empty() {
            return;
        }
        for removed in write.removed {
            self.marks.remove(removed);
        }
        for node in write.nodes {
            let Some(at) = node.at else { continue };
            for (side, link) in Side::BOTH.into_iter().zip([&node.left, &node.right]) {
                if link.is_none() {
                    self.unmark(at, HashField::Child(side));
                }
            }
        }
        let carried: Vec<(CellAddress, Marks)> = report
            .carried
            .iter()
            .filter_map(|cell| Some((cell.to.address(), self.marks.remove(&cell.from)?)))
            .collect();
        self.marks.extend(carried);
    }

    /// Brings the cell at `node` up to date: makes every marked hash of it,
    /// and before that of everything that is marked below it, and writes
    /// those that differ.
    ///
    /// A node with no mark is not read, whatever is marked below it: what is
    /// below a node is found through the node's marks.
    pub fn evaluate<P: Pages, V: ValueReader, R: HashRules>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
        node: CellAddress,
    ) -> Result<(), HashError> {
        let mut run = evaluate::Run::new(self, pages, values, rules);
        run.bring(node, false).map(drop)
    }

    /// Brings the cell at `node` up to date, as [`Self::evaluate`] does,
    /// and returns the node's hash: what the link of its parent stores, and
    /// for the root of a Merk the root hash of the Merk.
    ///
    /// The hash is made from the node's cell each time it is asked for.
    pub fn hash_of<P: Pages, V: ValueReader, R: HashRules>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
        node: CellAddress,
    ) -> Result<Hash, HashError> {
        let mut run = evaluate::Run::new(self, pages, values, rules);
        let hash = run.bring(node, true)?;
        Ok(hash.expect("the hash that was asked for"))
    }

    /// Evaluates every mark. Afterwards no hash is marked, and no stored
    /// hash of a node that was marked differs from its recomputation.
    ///
    /// The marked nodes are taken in the order of their addresses, each
    /// with what is marked below it.
    pub fn evaluate_all<P: Pages, V: ValueReader, R: HashRules>(
        &mut self,
        pages: &mut P,
        values: &mut V,
        rules: &mut R,
    ) -> Result<(), HashError> {
        let mut run = evaluate::Run::new(self, pages, values, rules);
        while let Some(node) = run.first() {
            run.bring(node, false)?;
        }
        Ok(())
    }
}

/// Recomputes the hashes that the cell at `node` stores, from the pages as
/// they are, and returns those that differ from the stored ones.
///
/// Each stored hash is compared with what the rules make of what it is made
/// from, as that is stored: the value hash from the node's value and the
/// roots of the Merks it hosts, the key-value hash from the key and the
/// stored value hash, and the hash of a child from the child's cell. So a
/// hash that is wrong is reported once, where it is stored, and not again
/// in every node above it.
///
/// It needs no mark and makes none, and it changes no page. In a scope
/// whose marks are complete, what it returns is marked
/// ([`Hashes::stale`]), and after [`Hashes::evaluate_all`] it returns
/// nothing for any node. A hash that no rule can make from the pages, the
/// value hash of a reference, is the rules' to answer for.
pub fn check<P: Pages, V: ValueReader, R: HashRules>(
    pages: &mut P,
    values: &mut V,
    rules: &mut R,
    node: CellAddress,
) -> Result<Stale, HashError> {
    evaluate::check(pages, values, rules, node)
}

/// Whether the value hash that the cell at `node` stores is the one that
/// the rules name as made with another hash of a hosted root: the value
/// hash [`HashRules::value_hash_with_other`] makes, with
/// [`HashRules::other_node_hash`] of each hosted root that has one in the
/// place of the root's hash.
///
/// It is for a value hash that [`check`] returned: whoever checks a store
/// can tell a hash that a writer made in a way the rules name from one
/// that nothing explains. A cell that hosts no root, and one none of whose
/// roots has another hash, is answered `false` with no value read. Like
/// [`check`] it needs no mark, makes none and changes no page.
pub fn made_with_other<P: Pages, V: ValueReader, R: HashRules>(
    pages: &mut P,
    values: &mut V,
    rules: &mut R,
    node: CellAddress,
) -> Result<bool, HashError> {
    evaluate::made_with_other(pages, values, rules, node)
}
