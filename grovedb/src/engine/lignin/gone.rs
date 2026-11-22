//! Which subtrees a group of batches drops in the subtree directory:
//! those that are entered under a node the group removes.
//!
//! On RocksDB the delete of a tree removes one record of the Merk above,
//! and GroveDB removes the records under the tree's prefix itself, one by
//! one, where it means them to go. On LigninDB a subtree is in the
//! subtree directory under the element that hosts it, and leaves with
//! that element (`tree-layout.md` D1): the directory names no host that
//! is no node. GroveDB names no subtree where it removes an element, so
//! the engine says it, where a group of batches is applied.
//!
//! The group **removes** a node where the last of its batches that holds
//! an outcome for the node removes it.
//!
//! - **What is entered under a removed node is dropped**, with the batch
//!   that removes the node. The prefix of the subtree under a key is
//!   derived from the path of the subtree that the key is in, which the
//!   handles by path tell their batch ([`LigninSettled::path`]). The
//!   directory is asked for that prefix, and for the three prefixes
//!   beside it where it is entered as the primary of an indexed tree.
//!   Each that is entered under the removed node is dropped, unless a
//!   batch of the group was told to move it
//!   ([`LigninSettled::moves`]): a move that leaves no element under
//!   the old key removes the node there, and a subtree that goes to
//!   another prefix with the element is stated by the look for what is
//!   moved ([`moved`](super::moved)).
//! - **A drop takes every subtree below it**, by the hosts the directory
//!   names, and none of them is dropped by name: the nodes that the group
//!   removes in a subtree which is below a drop are not looked at.
//! - **Where the group writes an element over a tree**, what leaves is
//!   said by the look for that ([`over`](super::over)). Where it states
//!   a drop, this look is made again with it: a node that is removed in
//!   a subtree which that look drops, or in one below it, is not looked
//!   at, and a drop of that look which is below a drop of this one goes
//!   with it ([`Gone::covered`]).
//! - **Three kinds of writes are not handed to the transaction**
//!   ([`LigninSettled::leave_out`]), and no enter is stated on their
//!   account ([`enter`](super::enter)):
//!   - every write into a subtree that this look drops or that is below
//!     such a drop;
//!   - the writes into a subtree that the other look drops and does not
//!     enter again, or into one below such a drop, where the group
//!     holds removals for it and no put. A put into such a subtree is
//!     that look's to refuse;
//!   - the writes into a subtree that is not entered, where the group
//!     holds removals for it and no put. There is nothing to remove.
//!
//!   They are settled before, so the group is charged for them as it is
//!   on RocksDB.
//! - **A look that states a drop follows the hosts above the subtrees
//!   the group writes into**, by a read of the entry of each, up to a
//!   dropped subtree or to one with no host, to tell which are below a
//!   drop. A look that states no drop follows no host.
//!
//! What this does not do:
//!
//! - A node that is removed in a subtree whose path no handle of the
//!   group was told states no drop: none is inferred from it.
//! - A group in which a batch enters, drops or clears a subtree itself,
//!   or states one anew, leaves nothing out, and is refused where a
//!   subtree would be dropped with it.
//! - Open, `TXN-03-F5`: what the look found is not judged again where
//!   the group is laid out. A subtree that a commit beside the
//!   transaction enters under a removed node after the look is not
//!   dropped; a removal that was left out for a subtree which such a
//!   commit enters is not applied; and a drop that was stated for a
//!   subtree takes an entry that such commits put in its place under
//!   another host (`engine.md`, "A subtree leaves with its element").

use std::collections::{BTreeMap, BTreeSet};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::store::lignin::{
    LigninEntry, LigninEntryRefused, LigninOutcome, LigninSettled, LigninView,
};
use grovedb_storage::{prefix::secondary_prefix_for, Error};

use super::{
    enter::entered,
    error::not_supported,
    over::{prefix_under, Left},
};

/// What a group that removes nodes does to the subtrees under them.
#[derive(Debug, Default)]
pub(super) struct Gone {
    /// The subtrees that are dropped with the group, each with the batch
    /// that removes the node it is entered under. None is below another.
    pub(super) drops: Vec<(usize, [u8; 32])>,
    /// The subtrees whose writes the group does not hand over.
    pub(super) left_out: BTreeSet<[u8; 32]>,
    /// The drops of the look for what is written over that are below a
    /// drop of this look: they go with it, and are not stated.
    pub(super) covered: BTreeSet<[u8; 32]>,
}

impl Gone {
    /// Whether `refused` is of a subtree whose writes the group leaves
    /// out.
    pub(super) fn leaves_out(&self, refused: LigninEntryRefused) -> bool {
        match refused {
            LigninEntryRefused::NotEntered(subtree) => self.left_out.contains(&subtree),
            LigninEntryRefused::EnteredOtherwise(_) => false,
        }
    }

    /// Whether `refused` is of a subtree whose entry this look found
    /// otherwise: no entry, for a subtree that the look found entered and
    /// stated the drop of.
    pub(super) fn found_otherwise(&self, refused: LigninEntryRefused) -> bool {
        match refused {
            LigninEntryRefused::NotEntered(subtree) => {
                self.drops.iter().any(|(_, dropped)| *dropped == subtree)
            }
            LigninEntryRefused::EnteredOtherwise(_) => false,
        }
    }
}

/// The batch of `parts` that removes the node `key` of `subtree`, if the
/// group leaves no node under it: the last batch that holds an outcome for
/// the node removes it.
fn removed_by(parts: &[LigninSettled], subtree: &[u8; 32], key: &[u8]) -> Option<usize> {
    let held = parts.iter().enumerate().rev();
    let mut held = held.filter_map(|(at, part)| Some((at, part.node(subtree, key)?)));
    match held.next()? {
        (at, LigninOutcome::Removed) => Some(at),
        (_, LigninOutcome::Put(_)) => None,
    }
}

/// The directory as one look reads it: what [`Look::host`] read of a
/// subtree is kept for the look.
struct Look<'a> {
    view: LigninView<'a>,
    /// For each subtree asked for: `None` where it is not entered, and
    /// otherwise the subtree that its host is in, if it has a host.
    hosts: BTreeMap<[u8; 32], Option<Option<[u8; 32]>>>,
    /// The subtrees that this look drops.
    dropped: BTreeSet<[u8; 32]>,
    /// The subtrees that the look for what is written over drops
    /// ([`over`](super::over)).
    over: &'a [[u8; 32]],
}

impl Look<'_> {
    /// Whether `subtree` is entered, and the subtree that its host is in.
    fn host(&mut self, subtree: [u8; 32]) -> Result<Option<Option<[u8; 32]>>, Error> {
        if let Some(known) = self.hosts.get(&subtree) {
            return Ok(*known);
        }
        let entry = entered(self.view, subtree)?;
        let host = entry.map(|entry| entry.host.map(|host| host.subtree));
        self.hosts.insert(subtree, host);
        Ok(host)
    }

    /// Whether `subtree` is dropped or is below a drop, by the hosts
    /// that the directory names: a drop of this look where `own` says
    /// so, and one of the look for what is written over where `over`
    /// does.
    fn leaves(&mut self, subtree: [u8; 32], own: bool, over: bool) -> Result<bool, Error> {
        let (own, over) = (
            own && !self.dropped.is_empty(),
            over && !self.over.is_empty(),
        );
        if !own && !over {
            return Ok(false);
        }
        let mut asked = BTreeSet::new();
        let mut at = subtree;
        // No subtree is asked twice, whatever the directory names.
        while asked.insert(at) {
            if (own && self.dropped.contains(&at)) || (over && self.over.contains(&at)) {
                return Ok(true);
            }
            match self.host(at)? {
                Some(Some(above)) => at = above,
                Some(None) | None => break,
            }
        }
        Ok(false)
    }
}

/// What `parts` do to the subtrees under the nodes they remove. `own`
/// says that a batch of them enters, drops or clears a subtree itself,
/// or states one anew, and `over` is what the group does to the subtrees
/// of the trees it writes other elements over.
pub(super) fn gone(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    own: bool,
    over: &Left,
) -> Result<Gone, Error> {
    let mut gone = Gone::default();
    let mut look = Look {
        view,
        hosts: BTreeMap::new(),
        dropped: BTreeSet::new(),
        over: &over.drops,
    };
    // The removed nodes by the subtree they are in, for the subtrees
    // whose path the group was told: a subtree before those below it.
    let mut removed: BTreeMap<[u8; 32], BTreeSet<&[u8]>> = BTreeMap::new();
    for (subtree, key) in parts.iter().flat_map(LigninSettled::removed_nodes) {
        removed.entry(subtree).or_default().insert(key);
    }
    let path_of = |subtree: &[u8; 32]| parts.iter().find_map(|part| part.path(subtree));
    let mut removed: Vec<(&[Vec<u8>], [u8; 32], BTreeSet<&[u8]>)> = (removed.into_iter())
        .filter_map(|(subtree, keys)| Some((path_of(&subtree)?, subtree, keys)))
        .collect();
    removed.sort_by_key(|(path, ..)| path.len());
    // The subtrees that a batch of the group was told to move: each goes
    // to another prefix, and does not leave with the node it hung under.
    let moved: BTreeSet<[u8; 32]> = (parts.iter())
        .flat_map(|part| part.moves().iter().map(|(from, _)| *from))
        .collect();
    for (path, subtree, keys) in removed {
        // Nothing is entered under a node of a subtree that is not
        // entered, and what is under a node of one that leaves goes with
        // the drop above, of either look.
        if look.host(subtree)?.is_none() || look.leaves(subtree, true, true)? {
            continue;
        }
        for key in keys {
            let Some(part) = removed_by(parts, &subtree, key) else {
                continue;
            };
            let under = |entry: &LigninEntry| {
                (entry.host.as_ref())
                    .is_some_and(|at| (&at.subtree, at.key.as_slice()) == (&subtree, key))
            };
            let own_prefix = prefix_under(path, key);
            let Some(entry) = entered(view, own_prefix)?.filter(under) else {
                continue;
            };
            let mut drop = |prefix: [u8; 32]| {
                if !moved.contains(&prefix) {
                    gone.drops.push((part, prefix));
                    look.dropped.insert(prefix);
                }
            };
            drop(own_prefix);
            if (entry.tree_type()).is_ok_and(|tree| tree.is_indexed_primary()) {
                // A secondary is stored under the tag of its axis, and
                // the big sum axis under that of the sum axis: these are
                // all.
                for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
                    let secondary = secondary_prefix_for(&own_prefix, axis.tag()).value;
                    if entered(view, secondary)?.filter(under).is_some() {
                        drop(secondary);
                    }
                }
            }
        }
    }
    if own {
        // Nothing says which of a batch's own directory writes and a
        // drop stated here comes first.
        return match gone.drops.is_empty() {
            true => Ok(gone),
            false => Err(not_supported(
                "a group that removes a node which a subtree is entered under, and in which a \
                 batch enters, drops or clears a subtree itself, or states one anew",
            )),
        };
    }
    // A drop of the other look that is below one of this look goes with
    // it, and is not stated again.
    for dropped in &over.drops {
        let above = look.host(*dropped)?.flatten();
        if let Some(above) = above
            && look.leaves(above, true, false)?
        {
            gone.covered.insert(*dropped);
        }
    }
    // The subtrees that the other look enters again with the group: what
    // the group writes into one of them is written, unless a drop of
    // this look takes the subtree.
    let again: BTreeSet<[u8; 32]> = (over.enters.iter())
        .map(|(subtree, _)| subtree.prefix)
        .collect();
    // The subtrees that some batch of the group puts a node or a record
    // into.
    let put: BTreeSet<[u8; 32]> = (parts.iter().flat_map(LigninSettled::put_into)).collect();
    let mut written = BTreeSet::new();
    for subtree in parts.iter().flat_map(LigninSettled::written) {
        let prefix = subtree.prefix;
        if !written.insert(prefix) {
            continue;
        }
        if look.leaves(prefix, true, false)? {
            gone.left_out.insert(prefix);
            continue;
        }
        if again.contains(&prefix) {
            continue;
        }
        let removes = !put.contains(&prefix);
        let removes_nothing = removes && look.host(prefix)?.is_none();
        // Under a drop of the other look a put is that look's to refuse.
        if removes_nothing || (removes && look.leaves(prefix, false, true)?) {
            gone.left_out.insert(prefix);
        }
    }
    Ok(gone)
}
