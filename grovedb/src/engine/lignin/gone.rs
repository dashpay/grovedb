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
//! - **The drop is judged where the group is laid out.** For each node
//!   that the group removes in a subtree whose writes it hands over, the
//!   four prefixes are each stated as dropped if the subtree is entered
//!   under that node then
//!   (`lignindb::txn::SubtreeWrite::DropIfEntered`), whatever this look
//!   found under them, but for a prefix that the group hands a write
//!   over for as the look found it (below). So a subtree that a commit
//!   beside the
//!   transaction enters under the node after the look leaves with the
//!   node, one that such a commit took is not dropped again, and an
//!   entry that such commits left under the prefix and under another
//!   node, or under none, stays. That is said of a prefix that the
//!   group hands no write over for.
//! - **Where the group itself hands a write over for a prefix that the
//!   look did not find under the node**, it writes the prefix as the
//!   look found it ([`Gone::beside`]): under another node, under none,
//!   or not entered. No drop is stated for such a prefix. The first
//!   batch of the group says that the prefix is not entered under the
//!   node ([`LigninSettled::not_under`],
//!   `lignindb::txn::SubtreeWrite::NotUnder`), and the transaction
//!   refuses the group as a conflict where it is
//!   (`lignindb::txn::BatchError::EnteredSince`): nothing of the group is
//!   applied, and a look that is made afterwards finds the subtree under
//!   the node. Removals alone that state no root key are handed over to
//!   be made if the prefix is entered
//!   ([`LigninSettled::remove_if_entered`]); a put and removals with a
//!   root key are handed over as they are.
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
//!     holds removals for it and no put, and its change set states a
//!     root key.
//!
//!   They are settled before, so the group is charged for them as it is
//!   on RocksDB.
//! - **Removals alone, for a subtree that is not entered, are removed
//!   if the subtree is entered where the group is laid out**
//!   ([`LigninSettled::remove_if_entered`],
//!   `lignindb::txn::SubtreeWrite::RemoveIfEntered`): a record that a
//!   commit beside the transaction put under such a key after the look
//!   is removed where the group is applied, as on RocksDB, and a commit
//!   that writes such a key after the group was applied meets the
//!   removal. A group that is refused removes nothing: so it is where
//!   the subtree is a prefix under a node that the group removes and is
//!   entered under that node when the first batch of the group is
//!   applied (above). No enter is stated on their account either.
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
//!   or states one anew, leaves nothing out, and is refused where the
//!   look finds a subtree entered under a node it removes. Nothing is
//!   stated for what the look did not find, and what a commit beside
//!   the transaction enters under such a node after the look outlives
//!   the node.

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

/// A subtree that is dropped if it is entered under a node the group
/// removes.
#[derive(Debug)]
pub(super) struct Under {
    /// The batch that removes the node.
    pub(super) part: usize,
    /// The subtree.
    pub(super) prefix: [u8; 32],
    /// The subtree that the node is in.
    pub(super) above: [u8; 32],
    /// The node's key.
    pub(super) key: Vec<u8>,
}

/// What a group that removes nodes does to the subtrees under them.
#[derive(Debug, Default)]
pub(super) struct Gone {
    /// The subtrees that are dropped with the group if they are entered
    /// under a node it removes when it is laid out. None that the look
    /// found is below another that it found.
    pub(super) drops: Vec<Under>,
    /// Those of them that the look found entered under the node: what a
    /// flat drop leaves behind is worked out with what the look found
    /// ([`behind`](super::behind)).
    pub(super) found: BTreeSet<[u8; 32]>,
    /// The subtrees whose writes the group does not hand over.
    pub(super) left_out: BTreeSet<[u8; 32]>,
    /// The subtrees under which the group removes if they are entered
    /// when it is laid out: what it holds for them is removals alone,
    /// and the look found them not entered, or they are prefixes under
    /// a removed node that the look found no subtree under the node
    /// for.
    pub(super) removing: BTreeSet<[u8; 32]>,
    /// The drops of the look for what is written over that are below a
    /// drop of this look: they go with it, and are not stated.
    pub(super) covered: BTreeSet<[u8; 32]>,
    /// The prefixes under a removed node that the look found no subtree
    /// under the node for, and that the group hands a write over for: a
    /// put, removals with a root key stated, or removals that are made
    /// if the prefix is entered. The group writes such a prefix as the
    /// look found it, so no drop is stated for it: the first batch of
    /// the group says that the prefix is not entered under the node
    /// (`lignindb::txn::SubtreeWrite::NotUnder`), and the transaction
    /// refuses the group as a conflict where it is
    /// (`lignindb::txn::BatchError::EnteredSince`).
    pub(super) beside: BTreeSet<[u8; 32]>,
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

    /// The subtrees that no enter is stated on account of: those whose
    /// writes the group leaves out, and those under which it removes if
    /// they are entered.
    pub(super) fn unentered(&self) -> BTreeSet<[u8; 32]> {
        self.left_out.union(&self.removing).copied().collect()
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
    // The subtrees that some batch of the group puts a node or a record
    // into, and those that it holds a write for.
    let put: BTreeSet<[u8; 32]> = (parts.iter().flat_map(LigninSettled::put_into)).collect();
    let held: BTreeSet<[u8; 32]> = (parts.iter().flat_map(LigninSettled::written))
        .map(|subtree| subtree.prefix)
        .collect();
    // Whether what the group holds for a subtree is removals with a
    // root key stated for its Merk, and no put: where the subtree is not
    // entered, they are left out.
    let rooted = |prefix: &[u8; 32]| {
        !put.contains(prefix) && !parts.iter().all(|part| part.removes_alone(prefix))
    };
    // The prefixes under a removed node that the look found no subtree
    // under the node for: those that the group holds removals alone
    // for, with no root key stated, and the others.
    let mut if_entered = BTreeSet::new();
    let mut not_under = BTreeSet::new();
    for (path, subtree, keys) in removed {
        // What is under a node of a subtree that leaves goes with the
        // drop above, of either look.
        if look.leaves(subtree, true, true)? {
            continue;
        }
        // Nothing is entered under a node of a subtree that is not
        // entered. A group in which a batch writes the directory itself
        // states what the look found, and a node whose removal is left
        // out stays.
        if look.host(subtree)?.is_none() && (own || rooted(&subtree)) {
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
            let primary = entered(view, own_prefix)?.filter(under);
            let indexed = (primary.as_ref()).is_some_and(|entry| {
                entry
                    .tree_type()
                    .is_ok_and(|tree| tree.is_indexed_primary())
            });
            let mut beside = vec![(own_prefix, primary.is_some())];
            // A secondary is stored under the tag of its axis, and the
            // big sum axis under that of the sum axis: these are all.
            // The directory is asked for them where the primary is
            // entered as one, and for each that the group holds a write
            // for: what is said of a prefix that the group writes is
            // what a read of its entry found.
            for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
                let secondary = secondary_prefix_for(&own_prefix, axis.tag()).value;
                let asked = indexed || held.contains(&secondary);
                let found = asked && entered(view, secondary)?.filter(under).is_some();
                beside.push((secondary, found));
            }
            for (prefix, found) in beside {
                if moved.contains(&prefix) {
                    continue;
                }
                match found {
                    true => {
                        look.dropped.insert(prefix);
                        gone.found.insert(prefix);
                    }
                    // Of what the look did not find, nothing is stated
                    // by a group that writes the directory itself.
                    false if own => continue,
                    // Removals alone that state no root key are made
                    // if the prefix is entered once the drop is judged.
                    false
                        if held.contains(&prefix) && !put.contains(&prefix) && !rooted(&prefix) =>
                    {
                        if_entered.insert(prefix);
                    }
                    false => drop(not_under.insert(prefix)),
                }
                gone.drops.push(Under {
                    part,
                    prefix,
                    above: subtree,
                    key: key.to_vec(),
                });
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
        if if_entered.contains(&prefix) {
            gone.removing.insert(prefix);
            continue;
        }
        if again.contains(&prefix) {
            continue;
        }
        let removes = !put.contains(&prefix);
        if removes && look.host(prefix)?.is_none() {
            // Removals alone, for a subtree that is not entered: they
            // are made if it is entered where the group is laid out. A
            // change set that states a root key is left out.
            match rooted(&prefix) {
                false => drop(gone.removing.insert(prefix)),
                true => drop(gone.left_out.insert(prefix)),
            }
            continue;
        }
        // Under a drop of the other look a put is that look's to refuse.
        if removes && look.leaves(prefix, false, true)? {
            gone.left_out.insert(prefix);
        }
    }
    // What the group hands over for a prefix that the look did not find
    // under its removed node is written as the look found the prefix:
    // not under that node.
    let handed = |prefix: &[u8; 32]| written.contains(prefix) && !gone.left_out.contains(prefix);
    let unfound = not_under.into_iter().chain(if_entered);
    gone.beside = unfound.filter(handed).collect();
    Ok(gone)
}
