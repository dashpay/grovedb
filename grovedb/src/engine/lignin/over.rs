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
//! the Merks under its key.** Where the element as the group leaves it
//! is an indexed tree or a tree of one Merk that names no root, a batch
//! after the last one that puts its node writes nodes of a Merk that is
//! entered under its key, and the group puts nothing into that Merk,
//! the Merk is cleared with the last batch that writes nodes of it
//! (`SubtreeWrite::Clear`, `tree-layout.md` D4): the node of the element
//! names the root of each Merk it hosts where it is put, and a later
//! batch that removes the root node puts no node that could name the
//! Merk otherwise. On the grove versions that remove the records of an
//! indexed tree which a batch writes over, a batch that is applied in
//! parts and writes such an element over a filled indexed tree makes
//! such a group: the element in one part, and the removal of the old
//! tree's records, by the walk of each of its subtrees, in the part
//! after it. The Merk stays entered as it was, with the records of its
//! other columns, as it does where one batch holds the element and the
//! removals.
//!
//! **An empty indexed tree written over a tree whose records the group
//! does not remove.** Where the element as the group leaves it is an
//! indexed tree that names no root, the group writes no node of what is
//! entered under its key, and the subtree of its own path has a node or
//! is that of a tree which keeps its data flat, each subtree under the
//! key that the new element hosts is dropped, and the node of the
//! element hosts nothing: the first write into the new tree enters its
//! Merks. That is an indexed tree over a filled tree of one Merk or of
//! flat records, and over a filled indexed tree on the grove versions
//! that do not remove its records. One such group is refused, as not
//! supported: where a secondary that the new element hosts has a row
//! ([`rows_left`], D-292).
//!
//! **Another element written over an indexed tree.** The primary of the
//! old tree is the subtree of the element's own path, and is looked at as
//! the subtree of any tree is: it is dropped, and entered again where the
//! group writes into it, by the rules above, so that its entry names the
//! tree type of the new element. The secondaries of the old tree hang
//! under the same key and are hosted by no other element than an indexed
//! tree: each that is entered there is dropped with the group.
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
//!   (`BatchError::NotEntered`) and takes nothing of the group. The
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
//!   that no handle by path wrote into with the group.

use std::collections::BTreeSet;

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
    enter::{element_of, entered, host_of, key_of, Host},
    error::{failed, not_supported},
};
use crate::operations::indexed_tree::{axis_storage_tag, indexed_element_axes};

/// What a group that writes over trees does to their subtrees.
#[derive(Debug, Default)]
pub(super) struct Left {
    /// The subtrees that are dropped with the group. None of them is below
    /// another.
    pub(super) drops: Vec<[u8; 32]>,
    /// What is said of the elements that are written over a tree: one
    /// whose subtree is dropped and not entered again hosts nothing once
    /// the group is applied, and one that is a tree with one Merk, whose
    /// subtree stays, hosts that Merk.
    pub(super) hosts: Vec<LigninHost>,
    /// The subtrees that are entered again with the group, each with the
    /// bytes of its tree type: a dropped one before those below it.
    pub(super) enters: Vec<(LigninSubtree, Vec<u8>)>,
    /// The subtrees that the group holds a write for and that leave with
    /// it: a dropped one, or one below it, that is not entered again.
    bare: Vec<[u8; 32]>,
    /// The Merks of indexed trees that are cleared with the group, each
    /// with the batch that clears it ([`emptied_after`]).
    pub(super) clears: Vec<(usize, [u8; 32])>,
}

impl Left {
    /// Whether `refused` is of a subtree whose entry this look found
    /// otherwise: no entry, for a subtree that the look found entered and
    /// stated the drop of.
    pub(super) fn found_otherwise(&self, refused: LigninEntryRefused) -> bool {
        match refused {
            LigninEntryRefused::NotEntered(subtree) => self.drops.contains(&subtree),
            LigninEntryRefused::EnteredOtherwise(_) => false,
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

/// What [`emptied_after`] finds of the Merks of an indexed tree whose
/// element the group leaves as an empty one.
#[derive(Default)]
struct Emptied {
    /// The Merks to clear, each with the batch that clears it.
    clears: Vec<(usize, [u8; 32])>,
    /// The Merks to drop.
    drops: Vec<[u8; 32]>,
}

/// What becomes of the Merks under the key of an element that the group
/// writes as a tree which names no root: an indexed tree with the axes
/// `axes`, or, with none, a tree of one Merk. Of the Merks that are
/// entered under the key, which are the subtree of the element's own
/// path and the secondaries of an indexed tree that was there:
///
/// - one that the group puts nothing into, and that a batch after the
///   last one which puts the element's node writes nodes of, is cleared
///   with the last batch that writes nodes of it.
///
/// And for an indexed tree:
///
/// - a secondary that the element hosts, that has a row, and that no
///   batch of the group writes a node of, refuses the group
///   ([`rows_left`]);
/// - where the subtree of the element's own path has a node, or is that
///   of a tree which keeps its data flat, the group
///   writes a node of none of the Merks that the element hosts and
///   clears none, each of them is dropped: no element names a root from
///   then on, and the node of the element hosts nothing until the next
///   write into the tree enters its Merks. That subtree is the primary
///   of an indexed tree, or the Merk of a tree of one Merk that the
///   indexed tree is written over.
///
/// `host` names the element, with the subtree `own` of its own path.
fn emptied_after(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    host: &LigninHost,
    own: [u8; 32],
    axes: Option<&[IndexAxis]>,
) -> Result<Emptied, Error> {
    let mut emptied = Emptied::default();
    let puts = |part: &LigninSettled| {
        let node = part.node(&host.subtree, &host.key);
        matches!(node, Some(LigninOutcome::Put(_)))
    };
    let Some(put_at) = parts.iter().rposition(puts) else {
        return Ok(emptied);
    };
    let under = |entry: &LigninEntry| {
        let at = entry.host.as_ref();
        at.is_some_and(|at| (at.subtree, &at.key) == (host.subtree, &host.key))
    };
    let put_into = |prefix: &[u8; 32]| {
        let mut put = parts.iter().flat_map(LigninSettled::put_into);
        put.any(|into| into == *prefix)
    };
    // A secondary is stored under the tag of its axis, and the big sum
    // axis under that of the sum axis: these are all.
    let secondaries = [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
        .map(|axis| secondary_prefix_for(&own, axis.tag()).value);
    // The secondaries that the element hosts.
    let hosted: Vec<[u8; 32]> = (axes.into_iter().flatten())
        .map(|axis| secondary_prefix_for(&own, axis_storage_tag(*axis)).value)
        .collect();
    let mut rooted = false;
    for prefix in std::iter::once(own).chain(secondaries) {
        let Some(entry) = entered(view, prefix)?.filter(under) else {
            continue;
        };
        match parts.iter().rposition(|part| part.writes_merk(&prefix)) {
            Some(last) if last > put_at && !put_into(&prefix) => {
                emptied.clears.push((last, prefix));
            }
            // Of an element that is no indexed tree, what leaves is
            // said by the rules of any tree.
            None if axes.is_none() => {}
            // A secondary of an axis that the element has no more leaves
            // by the look for what is entered ([`super::enter`]).
            None if prefix != own && !hosted.contains(&prefix) => {}
            None if prefix != own && entry.root_key.is_some() => return Err(rows_left(host)),
            None => {
                // A tree that keeps flat records has them whatever its
                // entry says of a root.
                let flat = |tree_type: TreeType| tree_type.uses_non_merk_data_storage();
                rooted |= entry.root_key.is_some() || entry.tree_type().is_ok_and(flat);
                emptied.drops.push(prefix);
            }
            _ => {}
        }
    }
    if !(rooted && emptied.clears.is_empty()) {
        emptied.drops.clear();
    }
    Ok(emptied)
}

/// What `parts` do to the subtrees of the trees they write over.
pub(super) fn left(view: LigninView<'_>, parts: &[LigninSettled]) -> Result<Left, Error> {
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
        // ([`super::enter`]). Where the group empties one after it wrote
        // the element as an empty tree, the Merk is cleared with the
        // batch that empties it. Where one has a node and the group
        // writes none of them, they are dropped, and the node of the
        // element hosts nothing.
        if let Some(element) = (element.as_ref()).filter(|element| element.is_indexed_tree()) {
            let axes = indexed_element_axes(element)
                .map_err(|error| failed(format!("an indexed tree: {error}")))?;
            let root = element.root_key_and_tree_type();
            let has_root = root.is_some_and(|(root, _)| root.is_some());
            if has_root || axes.iter().any(|(_, root_key)| root_key.is_some()) {
                continue;
            }
            let axes: Vec<IndexAxis> = axes.into_iter().map(|(axis, _)| axis).collect();
            let emptied = emptied_after(view, parts, &host, own, Some(&axes))?;
            left.clears.extend(emptied.clears);
            if !emptied.drops.is_empty() {
                let leave = (emptied.drops.into_iter()).filter(|prefix| !own_business(prefix));
                dropped.extend(leave.map(|prefix| (prefix, *subtree)));
                left.hosts.push(LigninHost {
                    hosted: Vec::new(),
                    ..host
                });
            }
            continue;
        }
        let tree = element.and_then(|element| element.root_key_and_tree_type_owned());
        // A tree of one Merk that is written as an empty one, where the
        // group empties what is entered under its key in a later batch,
        // as a batch in parts does that writes it over a filled indexed
        // tree: each such Merk is cleared with the batch that empties it.
        if let Some((None, tree_type)) = &tree
            && !tree_type.uses_non_merk_data_storage()
        {
            let emptied = emptied_after(view, parts, &host, own, None)?;
            left.clears.extend(emptied.clears);
        }
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
