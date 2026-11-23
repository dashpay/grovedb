//! Which subtrees a batch drops in the subtree directory: those of a tree
//! whose element the batch writes another element over.
//!
//! On RocksDB an element written over a tree changes one record. The
//! records under the tree's prefix stay where they are, and the prefix is
//! the path's, so a tree that is made at that path later stands on them.
//! On LigninDB the cell of a node hosts the Merk of its element and no
//! more than its element has (`tree-layout.md` D1), so the engine says
//! what leaves, for GroveDB, where a batch is applied.
//!
//! It is said of a **write over a tree**: a node that the group puts
//! with a stated value where its writer had loaded the element of a tree
//! ([`LigninSettled::over_trees`]), in a subtree whose path the engine
//! knows because a handle by path wrote into it with the group
//! ([`LigninSettled::path`]). The subtree looked at is the one of the
//! element's own path, where the directory has it under that element.
//! The element as the group leaves it is the one that the last batch of
//! the group to state a value for the node states.
//!
//! Of an element that is no tree, or a tree that hosts one subtree:
//!
//! - **No tree:** the subtree is dropped.
//! - **A tree, whose subtree the group does not write:** the subtree is
//!   dropped if the element has no root key and the subtree has a node or
//!   is entered as a tree of another type, and for a tree that keeps its
//!   data flat if the subtree is entered as a tree of another type.
//! - **A tree, whose subtree the group writes:** the subtree is dropped
//!   and entered again, as a tree of the element's type, if it has a node
//!   or is entered as a tree of another type and a change set of its Merk
//!   is of a writer that began with no node
//!   ([`LigninSettled::began_empty`]), as the writer of a tree that the
//!   group itself makes does. A tree that keeps its data flat is dropped
//!   and entered again where it is entered as a tree of another type.
//!
//! A drop takes every subtree below with it. A subtree below a dropped
//! one that the group writes into, under an element that the group
//! states, is entered again with the group, as a tree of that element's
//! type.
//!
//! A subtree that is dropped is dropped with the first batch of the
//! group. The node of its element is written as one that hosts nothing
//! where no Merk of the group takes the place of the one that leaves:
//! where the subtree is not entered again, and where it is entered again
//! as a tree that keeps its data flat. Where it is entered again with a
//! Merk, the node names that Merk.
//!
//! **A tree that is written as an empty one before the group empties
//! the Merks under its key.** On the grove versions that remove the
//! records of an indexed tree which a batch writes over, a batch that is
//! applied in parts, and puts an empty tree over a filled indexed tree
//! before it pauses, is a group of two: the element in one part, and
//! the removal of the old tree's records, by the walk of each of its
//! subtrees, in the part after it. Where the element is put after the
//! pause, both are in one part, as in a batch that is applied whole.
//! For the group of two, the look for what is entered has the node of the element
//! written as one that hosts nothing, and says which of the Merks that
//! the second part empties stay entered, with no node, and which leave
//! ([`super::enter`]): a secondary of an axis that a new indexed tree
//! has not leaves with the batch that puts the element.
//!
//! **An empty indexed tree written over a tree.** Where the element as
//! the group leaves it is an indexed tree that names no root, what this
//! look leaves entered under its key is without a node once the group
//! is applied, or the group is refused.
//!
//! - A subtree under the key of which a batch of the group writes
//!   nodes, and that no later batch drops or clears: the last batch
//!   that writes nodes of it has to state no root for it, or the group
//!   is refused as not supported ([`root_left`]). This look says
//!   nothing else of it.
//! - A subtree that is entered under the key, that no batch of the
//!   group writes a node of, and that has a node, or is the subtree of
//!   the element's own path and that of a tree which keeps its data
//!   flat: it holds records that the group does not remove, and is
//!   dropped. That is the Merk of a filled tree of one Merk, the
//!   subtree of a tree of flat records, and each Merk of a filled
//!   indexed tree on the grove versions that do not remove its records.
//!   Two such groups are refused, as not supported: one where a
//!   secondary of an axis that the new element has too holds a row
//!   ([`rows_left`], D-292), and one that drops a subtree and holds a
//!   write under the key beside ([`written_beside`]).
//! - Every other subtree that is entered under the key and that no
//!   batch of the group writes a node of has no node, and stays entered
//!   as it is.
//!   What a later group that writes into the new tree makes of it is
//!   the rule of the look for what is entered ([`super::enter`]). The
//!   subtree of the element's own path is stated anew, with the first
//!   batch that puts the node of the element, as the primary of the new
//!   tree where it is entered as a tree that is no indexed tree: what
//!   leaves with the element later is found by the tree type of that
//!   entry ([`super::gone`]).
//!
//! Where a subtree under the key is dropped, by this look or by a batch
//! of the group itself, the node of the element is written as one that
//! hosts nothing: it points at none of the subtrees that leave, and no
//! subtree that stays under the key has a node. So it is where more
//! subtrees are entered under the key than the new element has Merks
//! and no handle of the group names the element for a subtree under
//! its key: its cell kept the places of the element that was there.
//! Where a handle of the group names the element so, and no subtree
//! is dropped, the look for what is entered says what the node hosts
//! ([`super::enter`]). Otherwise nothing is said of the node.
//!
//! **Another element written over an indexed tree.** The primary of the
//! old tree is the subtree of the element's own path, and is looked at as
//! the subtree of any tree is: it is dropped, and entered again where the
//! group writes into it, by the rules above, so that its entry names the
//! tree type of the new element. The secondaries of the old tree hang
//! under the same key and are hosted by no other element than an indexed
//! tree: each that is entered there is dropped with the group, where the
//! primary is looked at by these rules. It is not where the group writes
//! the primary on through a handle that loaded its nodes, as a batch
//! does that removes the records of the old tree itself: then the
//! primary stays by the first item of what this does not do, and the
//! secondaries stay entered, with no node once the batch removed their
//! rows.
//!
//! Where the subtree of a tree with one Merk stays, the node of its
//! element is written as one that hosts that Merk, with the root the
//! directory has for it: an earlier group of the transaction can have
//! written the Merk and not the element, as the group does that removes
//! the records of a tree before another is written over it.
//!
//! **The look can be stale when the group is applied**, as the look for
//! what a group enters can ([`super::enter`]): a commit of another
//! transaction can be published between the reads here and the
//! application.
//!
//! - Where the look found the subtree and such a commit dropped it since,
//!   the transaction refuses the drop of a subtree that is not entered
//!   (`BatchError::NotEntered`) and takes nothing of the group. The drop
//!   names the node of the element
//!   (`lignindb::txn::SubtreeWrite::DropUnder`): where such commits
//!   left an entry under the prefix that hangs under another node, or
//!   under none, the transaction refuses that too
//!   (`BatchError::HungOtherwise`). The
//!   engine looks again by the rule of the enters
//!   ([`Left::found_otherwise`], `Engine::apply_write_group`), and applies
//!   the group with what that look states, which is no drop of a
//!   subtree that is not entered then. A group that writes into a
//!   subtree which leaves with it and is not entered again is refused for
//!   that subtree by its own doing ([`Left::own_doing`]), and is not
//!   looked for again.
//! - This look comes after the one for what the group enters. A subtree
//!   that the first found absent, and that this one finds entered and
//!   states the drop of, is entered once, as this one has it.
//! - Where both found no subtree of a tree that the group leaves as no
//!   tree, or as a tree that it does not write into, and such a commit
//!   entered the subtree before the group was applied, by a first write
//!   into the tree, nothing is dropped and this look says nothing of the
//!   element's node: the element is written as the group states it, its
//!   node keeps what the commit hung under it, which is a Merk where the
//!   tree's data is one, and the entry outlives the tree. That commit
//!   wrote the element's node too. Its
//!   transaction and this one do not meet as a conflict where this one
//!   had not written the node before it, on either engine, and RocksDB
//!   keeps that commit's records under the prefix. A group that writes
//!   into the subtree itself meets that commit as two first writers of
//!   one subtree meet ([`super::enter`]).
//! - So it is where the group writes an empty indexed tree over a tree,
//!   no drop is stated under its key, by this look or by a batch of the
//!   group itself, and such a commit wrote a
//!   child into the old tree before the group was applied: nothing is
//!   dropped, and the Merks keep the roots that the commit gave them,
//!   under an element that names none. The node of the element keeps
//!   what the commit hung under it, unless this look said that it hosts
//!   nothing, where more subtrees were entered under the key than the
//!   new element has Merks: then no cell points at those roots. Where
//!   a drop is stated, as this look states one of the subtree of a tree
//!   of flat records, the drop takes what the commit wrote there.
//!
//! What this does not do:
//!
//! - It keeps nothing of a dropped tree. RocksDB keeps the tree's
//!   records, and a read that takes the records under a prefix as they
//!   are still finds them there: that difference is `GROVE-03-F1`'s to
//!   close (D-227; `engine.md`, "An element written over a tree").
//! - It reads the element as the group states it and not the element
//!   that was there. An element that is written again as a tree with a
//!   root key, or whose subtree the group writes on through the handle
//!   that fetched its root, keeps its subtree, whatever the type the
//!   subtree was entered with.
//! - A tree that keeps its data flat and is written over with a tree of
//!   its own type keeps its records: the element of such a tree changes
//!   with every write into it, and nothing tells a new one from it.
//! - The Merks of an indexed tree stay where the tree is written again
//!   as an indexed tree, but in the cases above; and so does the
//!   subtree of an element that the group
//!   removes ([`super::gone`]), a subtree that a batch of the group enters
//!   itself, moves, or moves another subtree to, and a node of a subtree
//!   that no handle by path wrote into with the group. Nothing is said
//!   of an indexed tree whose element is written into a subtree whose
//!   writes the group leaves out ([`super::gone`]), and nothing of a
//!   subtree under the key of an empty indexed tree that a batch of the
//!   group drops or clears itself, where neither that batch nor a later
//!   one writes a node of it.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::lignin::{
        tree_type_bytes, LigninEntry, LigninEntryRefused, LigninHost, LigninOutcome, LigninSettled,
        LigninSubtree, LigninView,
    },
    TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    prefix::{build_prefix, secondary_prefix_for},
    Error,
};

use super::{
    enter::{element_of, entered, host_of, hosting, key_of, Host, Hosting},
    error::{failed, not_supported},
};
use crate::operations::indexed_tree::indexed_element_axes;

/// What a group that writes over trees does to their subtrees.
#[derive(Debug, Default)]
pub(super) struct Left {
    /// The subtrees that are dropped with the group. None of them is below
    /// another.
    pub(super) drops: Vec<[u8; 32]>,
    /// The node that each of them was found entered under: the subtree
    /// it is in, and its key.
    pub(super) under: BTreeMap<[u8; 32], ([u8; 32], Vec<u8>)>,
    /// What is said of the elements that are written over a tree: one
    /// whose subtree is dropped and not entered again hosts nothing once
    /// the group is applied, and so does an indexed tree that names no
    /// root, where a subtree under its key is dropped, by the look or by
    /// a batch of the group itself, or where more subtrees are entered
    /// there than it has Merks and no handle of the group names the
    /// element for one; one that is a tree with one Merk, whose subtree
    /// stays, hosts that Merk.
    pub(super) hosts: Vec<LigninHost>,
    /// The subtrees that are entered again with the group, each with the
    /// bytes of its tree type: a dropped one before those below it.
    pub(super) enters: Vec<(LigninSubtree, Vec<u8>)>,
    /// The subtree of the own path of an indexed tree which the group
    /// writes as an empty one, where it stays and was entered as a tree
    /// of one Merk: stated anew as the primary of that tree, with the
    /// batch that states it, which is the first to put the node of the
    /// element, and the bytes of its tree type.
    pub(super) restated: Vec<(usize, LigninSubtree, Vec<u8>)>,
    /// The subtrees that the group holds a write for and that leave with
    /// it: a dropped one, or one below it, that is not entered again.
    bare: Vec<[u8; 32]>,
}

impl Left {
    /// Whether `refused` is of a subtree whose entry this look found
    /// otherwise: no entry, or an entry under another node or under
    /// none, for a subtree that the look found entered under the node of
    /// its element and stated the drop of; or no entry, for a subtree
    /// that the look found entered and stated anew.
    pub(super) fn found_otherwise(&self, refused: LigninEntryRefused) -> bool {
        let mut restated = self.restated.iter();
        match refused {
            LigninEntryRefused::NotEntered(subtree) => {
                self.drops.contains(&subtree)
                    || restated.any(|(_, stated, _)| stated.prefix == subtree)
            }
            LigninEntryRefused::EnteredOtherwise(subtree) => self.drops.contains(&subtree),
        }
    }

    /// Whether `refused` is of a subtree that has no entry by the group's
    /// own doing: one that the group writes into, that leaves with a drop
    /// of this look and that is not entered again. No commit beside the
    /// group made that so.
    pub(super) fn own_doing(&self, refused: LigninEntryRefused) -> bool {
        match refused {
            LigninEntryRefused::NotEntered(subtree) => self.bare.contains(&subtree),
            LigninEntryRefused::EnteredOtherwise(_) => false,
        }
    }
}

/// The prefix of the subtree under `key` of the subtree at `path`.
pub(super) fn prefix_under(path: &[Vec<u8>], key: &[u8]) -> [u8; 32] {
    let segments: Vec<&[u8]> = (path.iter().map(Vec::as_slice))
        .chain(std::iter::once(key))
        .collect();
    build_prefix(SubtreePath::from(segments.as_slice())).value
}

/// Whether the subtree `prefix` is below one of `drops` in the directory
/// as the transaction has it.
fn below(view: LigninView<'_>, drops: &[[u8; 32]], prefix: [u8; 32]) -> Result<bool, Error> {
    let mut entry = entered(view, prefix)?;
    while let Some(host) = entry.and_then(|entry| entry.host) {
        if drops.contains(&host.subtree) {
            return Ok(true);
        }
        entry = entered(view, host.subtree)?;
    }
    Ok(false)
}

/// An indexed tree that is written as an empty one over an indexed tree,
/// with the rows of a secondary that the new tree keeps too left where
/// they are (D-292). GroveDB removes them from grove version 4 on. Before
/// it, RocksDB opens the secondary of the new tree by the root-key record
/// of the old one, and the new tree's rankings hold the rows of the old
/// tree: a grove that fails GroveDB's own check, and whose root hashes
/// LigninDB would have to make from rows that no element commits to.
fn rows_left(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "an empty indexed tree written under the key {key} over an indexed tree, a secondary of \
         which holds rows that the batch does not remove"
    ))
}

/// An indexed tree that is written as an empty one over a tree whose
/// records the group leaves, in a group that also holds a write under
/// the key of that tree: a node or a record of the new tree, written with
/// the batch that makes it. The subtree of the old tree leaves with the
/// group, and what the group writes under the key would have to be
/// entered anew beside it. No operation of GroveDB makes such a group: a
/// batch that writes an indexed tree over an element and a child of it
/// is refused by GroveDB itself.
fn written_beside(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "an empty indexed tree written under the key {key} over a tree whose records the batch \
         leaves, in a group that writes under that key"
    ))
}

/// An indexed tree that is written as an empty one in a group that
/// leaves a node in a subtree under its key: the element names no root,
/// and no node would name the root of that Merk.
fn root_left(host: &LigninHost) -> Error {
    let key = key_of(host);
    not_supported(&format!(
        "an empty indexed tree written under the key {key} in a group that leaves a node in a \
         subtree under that key"
    ))
}

/// What the look finds under the key of an indexed tree that the group
/// writes as an empty one.
#[derive(Debug, Default)]
struct UnderAnEmptyIndex {
    /// The subtrees that leave.
    drops: Vec<[u8; 32]>,
    /// The subtree of the element's own path, where it stays and is
    /// entered as a tree of one Merk: with the tree type of the
    /// element's primary, as which it is stated anew.
    restated: Option<([u8; 32], TreeType)>,
    /// How many subtrees are entered under the key, but for those that
    /// a batch of the group drops or clears itself.
    entered: usize,
    /// Whether a batch of the group drops or clears a subtree under the
    /// key itself.
    taken: bool,
    /// Whether a handle of the group that names an element holds a write
    /// for a subtree under the key, in any column.
    named: bool,
}

/// What the group does to the subtrees under the key of an indexed tree
/// that it writes as an empty one, which names no root. `merks` are the
/// Merks of the element, by position, each with its tree type. What is
/// entered under the key is the subtree of the element's own path and
/// the secondaries of an indexed tree that was there.
///
/// - A subtree that a batch of the group drops or clears itself, where
///   neither that batch nor a later one writes a node of it, is left to
///   that batch: it is not judged, and not counted among the subtrees
///   that are entered under the key. A batch applies its drops and
///   clears before its writes, so a node of the subtree that the
///   dropping batch still holds is written after the drop.
/// - A subtree of which a batch of the group writes a node, and that
///   no later batch drops or clears, has no node once the group is
///   applied, or the group is refused ([`root_left`]): the last batch
///   that writes nodes of it states no root for it
///   (`LigninSettled::empties`). This look says nothing else of it.
/// - A subtree that has a node, and the subtree of the element's own
///   path where that is of a tree which keeps its data flat, whatever
///   its entry says of a root, is dropped: the primary of an indexed
///   tree, the Merk of a tree of one Merk or the subtree of a tree of
///   flat records that the indexed tree is written over, and a
///   secondary of an axis that the element has not. A secondary that
///   the element hosts and that has a row refuses the group
///   ([`rows_left`]).
/// - Every other subtree, which has no node, stays entered as it is.
///   The subtree of the element's own path is stated anew, as the
///   primary of the
///   element, where it is entered as a tree that is no indexed tree:
///   what leaves with the element later is found by the tree type of
///   that entry ([`super::gone`]).
/// - A group that drops a subtree and holds a write under the key
///   beside, of a node or of a record in any column, is refused
///   ([`written_beside`]): what it writes would be written into a
///   subtree that leaves, or beside one.
///
/// `host` names the element, with the subtree `own` of its own path.
fn emptied_after(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    host: &LigninHost,
    own: [u8; 32],
    merks: &[([u8; 32], TreeType)],
) -> Result<UnderAnEmptyIndex, Error> {
    let under = |entry: &LigninEntry| {
        let at = entry.host.as_ref();
        at.is_some_and(|at| (at.subtree, &at.key) == (host.subtree, &host.key))
    };
    // A secondary is stored under the tag of its axis, and the big sum
    // axis under that of the sum axis: these are all.
    let secondaries = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
        .map(|axis| secondary_prefix_for(&own, axis.tag()).value);
    let mut found = UnderAnEmptyIndex::default();
    let mut touched = false;
    for prefix in std::iter::once(own).chain(secondaries) {
        // What the group holds a write for, in any column.
        let written = parts.iter().flat_map(LigninSettled::written);
        for subtree in written.filter(|subtree| subtree.prefix == prefix) {
            touched = true;
            found.named |= subtree.host.is_some();
        }
        // A subtree that a batch of the group drops or clears itself is
        // as that batch leaves it, but where that batch or a later one
        // writes a node of it: a batch applies its drops and clears
        // before its writes.
        let left = parts.iter().rposition(|part| part.leaves(&prefix));
        let taken = left.is_some();
        found.taken |= taken;
        let written = parts.iter().rposition(|part| part.writes_merk(&prefix));
        let stands = |last: &usize| !left.is_some_and(|left| *last < left);
        if let Some(last) = written.filter(stands) {
            if !parts[last].empties(&prefix) {
                return Err(root_left(host));
            }
            found.entered +=
                usize::from(!taken && entered(view, prefix)?.is_some_and(|e| under(&e)));
            continue;
        }
        if taken {
            continue;
        }
        let entry = entered(view, prefix)?.filter(under);
        found.entered += usize::from(entry.is_some());
        let Some(entry) = entry else {
            continue;
        };
        // A tree that keeps flat records has them whatever its entry
        // says of a root.
        let flat = |tree_type: TreeType| tree_type.uses_non_merk_data_storage();
        let records = prefix == own && entry.tree_type().is_ok_and(flat);
        if entry.root_key.is_some() || records {
            if prefix != own && merks.iter().any(|(merk, _)| *merk == prefix) {
                return Err(rows_left(host));
            }
            found.drops.push(prefix);
            continue;
        }
        let indexed = |tree_type: TreeType| tree_type.is_indexed_primary();
        if prefix == own && !entry.tree_type().is_ok_and(indexed) {
            found.restated = merks.first().copied();
        }
    }
    match !found.drops.is_empty() && touched {
        true => Err(written_beside(host)),
        false => Ok(found),
    }
}

/// What `parts` do to the subtrees of the trees they write over.
///
/// `left_out` are the subtrees whose writes the group leaves out
/// ([`gone`](super::gone)): nothing is said of an indexed tree whose
/// element is written into one of them, a write that is not applied.
pub(super) fn left(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    left_out: &BTreeSet<[u8; 32]>,
) -> Result<Left, Error> {
    let written = |prefix: &[u8; 32]| {
        let mut written = parts.iter().flat_map(LigninSettled::written);
        written.any(|subtree| subtree.prefix == *prefix)
    };
    let own_business = |prefix: &[u8; 32]| {
        let moved = |part: &LigninSettled| {
            let mut pairs = part.moves().iter();
            pairs.any(|(from, to)| from == prefix || to == prefix)
        };
        parts.iter().any(|part| part.enters(prefix) || moved(part))
    };
    let mut left = Left::default();
    let mut seen = BTreeSet::new();
    // Each with the subtree that its element is in.
    let mut dropped: Vec<([u8; 32], [u8; 32])> = Vec::new();
    for (subtree, key) in parts.iter().flat_map(LigninSettled::over_trees) {
        if !seen.insert((subtree, key)) {
            continue;
        }
        let Some(path) = parts.iter().find_map(|part| part.path(subtree)) else {
            continue;
        };
        let own = prefix_under(path, key);
        if own_business(&own) {
            continue;
        }
        let host = LigninHost {
            subtree: *subtree,
            key: key.to_vec(),
            hosted: vec![own],
        };
        let element = match host_of(parts, &host) {
            Host::Stated(value) => element_of(value),
            Host::Removed | Host::Stored => continue,
        };
        // An indexed tree keeps the Merks that are entered under its key,
        // and the look for what is entered states anew those that its
        // element names otherwise than the directory has them
        // ([`super::enter`]). Where the element names no root, what is
        // entered under the key with records that the group does not
        // remove is dropped, and the node of the element hosts nothing
        // where the group writes no node under the key.
        if let Some(element) = (element.as_ref()).filter(|element| element.is_indexed_tree()) {
            // The write of the element is left out: the look says
            // nothing of it.
            if left_out.contains(subtree) {
                continue;
            }
            let axes = indexed_element_axes(element)
                .map_err(|error| failed(format!("an indexed tree: {error}")))?;
            let root = element.root_key_and_tree_type();
            let has_root = root.is_some_and(|(root, _)| root.is_some());
            if has_root || axes.iter().any(|(_, root_key)| root_key.is_some()) {
                continue;
            }
            let Hosting::Many(merks) = hosting(element, own)? else {
                continue;
            };
            let found = emptied_after(view, parts, &host, own, &merks)?;
            // The node points at none of the subtrees that leave, by a
            // drop of this look or of a batch of the group itself. And
            // its cell kept the places of the element that was there:
            // more than the new element has Merks where more subtrees
            // are entered under the key.
            let leaves = !found.drops.is_empty() || found.taken;
            let bare = leaves || found.entered > merks.len();
            let leave = (found.drops.into_iter()).filter(|prefix| !own_business(prefix));
            for prefix in leave {
                left.under.insert(prefix, (*subtree, key.to_vec()));
                dropped.push((prefix, *subtree));
            }
            // A statement anew goes with the first batch that puts the
            // node of the element, as it does where the group writes
            // into the tree ([`super::enter`]).
            let named = LigninHost {
                hosted: merks.iter().map(|(merk, _)| *merk).collect(),
                ..host.clone()
            };
            let puts = |part: &LigninSettled| {
                matches!(part.node(subtree, key), Some(LigninOutcome::Put(_)))
            };
            let put_by = parts.iter().position(puts);
            if let (Some(part), Some((prefix, tree_type))) = (put_by, found.restated) {
                let stated = LigninSubtree {
                    prefix,
                    host: Some(named),
                };
                (left.restated).push((part, stated, tree_type_bytes(tree_type)));
            }
            // Where a handle of the group names the element for a
            // subtree under its key, the look for what is entered says
            // what its node hosts, but where a subtree leaves: a batch
            // that drops one after the node is put does not put the
            // node again.
            if leaves || (bare && !found.named) {
                left.hosts.push(LigninHost {
                    hosted: Vec::new(),
                    ..host
                });
            }
            continue;
        }
        let tree = element.and_then(|element| element.root_key_and_tree_type_owned());
        // The tree written again, through a handle that loaded its nodes:
        // nothing of the directory is read for it.
        let begins_empty = || parts.iter().any(|part| part.began_empty(&own));
        let merk = tree
            .as_ref()
            .map(|(_, tree_type)| !tree_type.uses_non_merk_data_storage());
        if merk == Some(true) && written(&own) && !begins_empty() {
            continue;
        }
        let Some(entry) = entered(view, own)? else {
            continue;
        };
        let hangs = entry
            .host
            .as_ref()
            .is_some_and(|at| (at.subtree, at.key.as_slice(), at.position) == (*subtree, key, 0));
        if !hangs {
            continue;
        }
        // The secondaries of an indexed tree hang under its element
        // beside the primary, and the element that is written over it
        // hosts none of them: each that is entered there is dropped. A
        // secondary is stored under the tag of its axis, and the big sum
        // axis under that of the sum axis: these are all.
        if (entry.tree_type()).is_ok_and(|tree_type: TreeType| tree_type.is_indexed_primary()) {
            for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
                let secondary = secondary_prefix_for(&own, axis.tag()).value;
                let under = |entry: LigninEntry| {
                    let host = entry.host;
                    host.is_some_and(|at| (at.subtree, at.key.as_slice()) == (*subtree, key))
                };
                if !own_business(&secondary) && entered(view, secondary)?.is_some_and(under) {
                    left.under.insert(secondary, (*subtree, key.to_vec()));
                    dropped.push((secondary, *subtree));
                }
            }
        }
        let again = match tree {
            None => None,
            Some((root_key, tree_type)) => {
                let bytes = tree_type_bytes(tree_type);
                let other_type = entry.tree_type != bytes;
                let stale = other_type || entry.root_key.is_some();
                let flat = tree_type.uses_non_merk_data_storage();
                let fresh = match (written(&own), flat) {
                    (_, true) => other_type,
                    (true, false) => stale,
                    (false, false) => stale && root_key.is_none(),
                };
                if !fresh {
                    // The subtree stays. Its Merk can have another root
                    // than the cell of the element's node points at,
                    // where an earlier group of the transaction wrote
                    // the Merk and not the element, as the group that
                    // removes the records of a tree before another is
                    // written over it does: the node names the Merk as
                    // the directory has it.
                    if !flat {
                        left.hosts.push(host);
                    }
                    continue;
                }
                written(&own).then_some((bytes, flat))
            }
        };
        dropped.push((own, *subtree));
        left.under.insert(own, (*subtree, key.to_vec()));
        // The element's node hosts nothing where no Merk of the group
        // takes the place of the one that is dropped: where the subtree
        // is not entered again, and where it is entered again as a tree
        // that keeps its data flat, which has no Merk to name.
        if again.as_ref().is_none_or(|(_, flat)| *flat) {
            left.hosts.push(LigninHost {
                hosted: Vec::new(),
                ..host.clone()
            });
        }
        if let Some((tree_type, _)) = again {
            let subtree = LigninSubtree {
                prefix: own,
                host: Some(host),
            };
            left.enters.push((subtree, tree_type));
        }
    }
    if dropped.is_empty() {
        return Ok(left);
    }
    // A drop takes what is below it, and names none of it.
    let all: Vec<[u8; 32]> = dropped.iter().map(|(prefix, _)| *prefix).collect();
    for (prefix, above) in dropped {
        if !(all.contains(&above) || below(view, &all, above)?) {
            left.drops.push(prefix);
        }
    }
    // What the group writes below a dropped subtree is entered again,
    // under the element that the group states for it.
    for subtree in parts.iter().flat_map(LigninSettled::written) {
        let prefix = subtree.prefix;
        if left.enters.iter().any(|(known, _)| known.prefix == prefix) {
            continue;
        }
        let Some(host) = &subtree.host else {
            continue;
        };
        if own_business(&prefix) || !below(view, &left.drops, prefix)? {
            continue;
        }
        let Host::Stated(value) = host_of(parts, host) else {
            continue;
        };
        let tree_type = element_of(value).and_then(|element| element.tree_type());
        if tree_type.is_some_and(|tree_type| tree_type.is_indexed_primary()) {
            continue;
        }
        let bytes = tree_type.map(tree_type_bytes).unwrap_or_default();
        left.enters.push((subtree.clone(), bytes));
    }
    // What the group writes into a subtree that leaves and is not entered
    // again is refused by the transaction, whatever is published beside
    // the group.
    for subtree in parts.iter().flat_map(LigninSettled::written) {
        let prefix = subtree.prefix;
        let again = left.enters.iter().any(|(known, _)| known.prefix == prefix);
        if again || own_business(&prefix) {
            continue;
        }
        if left.drops.contains(&prefix) || below(view, &left.drops, prefix)? {
            left.bare.push(prefix);
        }
    }
    Ok(left)
}
