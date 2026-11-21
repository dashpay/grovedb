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
//! ([`LigninSettled::over_trees`]), through a handle by path. The subtree
//! looked at is the one of the element's own path, where the directory
//! has it under that element. The element as the group leaves it is the
//! one that the last batch of the group to state a value for the node
//! states.
//!
//! - **No tree:** the subtree is dropped.
//! - **A tree, whose subtree the group does not write:** the subtree is
//!   dropped if the element has no root key and the subtree has a node or
//!   is entered as a tree of another type, and for a tree that keeps its
//!   data flat if the subtree is entered as a tree of another type.
//! - **A tree, whose subtree the group writes:** the subtree is dropped
//!   and entered again, as a tree of the element's type, if it has a node
//!   or is entered as a tree of another type and the handle that wrote its
//!   nodes had loaded none ([`LigninSettled::began_empty`]): the writer of
//!   a tree that the group itself makes begins with no node. A tree that
//!   keeps its data flat is dropped and entered again where it is entered
//!   as a tree of another type.
//!
//! A drop takes every subtree below with it. A subtree below a dropped
//! one that the group writes into, under an element that the group
//! states, is entered again with the group, as a tree of that element's
//! type.
//!
//! A subtree that is dropped is dropped with the first batch of the
//! group, and its element is written as one that hosts nothing, or with
//! the subtree that is entered again.
//!
//! **The look can be stale when the group is applied**, as the look for
//! what a group enters can ([`super::enter`]): a commit of another
//! transaction can be published between the reads here and the
//! application. A group that this states a drop for writes the directory
//! itself and is applied once: a refusal is the caller's answer, and the
//! engine does not look again.
//!
//! - Where the look found the subtree and such a commit dropped it since,
//!   the transaction refuses the group (`BatchError::NotEntered`) and
//!   takes nothing of it.
//! - Where the look found no subtree and such a commit entered it since,
//!   by a first write into the tree, nothing is dropped: the element is
//!   written as the group states it and the entry outlives it, as the
//!   entry of a deleted tree does (GROVE-05). That commit wrote the
//!   element's node too. Its transaction and this one do not meet as a
//!   conflict where this one had not written the node before, on either
//!   engine, and RocksDB keeps that commit's records under the prefix.
//!
//! What this does not do:
//!
//! - It keeps nothing of a dropped tree. RocksDB keeps the tree's
//!   records, and a read that takes the records under a prefix as they
//!   are still finds them there: that difference is `GROVE-03-F1`'s to
//!   close (D-227; `engine.md`, "An element written over a tree").
//! - It reads the element as the group states it and not the element
//!   that was there. An element that is written again as the tree it was,
//!   with a root key or through a handle that loaded nodes of its Merk,
//!   keeps its subtree, whatever the type the subtree was entered with.
//! - A tree that keeps its data flat and is written over with a tree of
//!   its own type keeps its records: the element of such a tree changes
//!   with every write into it, and nothing tells a new one from it.
//! - An indexed tree, written over or written, is left as it is
//!   (GROVE-04-F1), and so is the subtree of an element that the group
//!   removes (GROVE-05), a subtree that a batch of the group enters
//!   itself or moves, and a node that was put through a handle which was
//!   not opened by a path.

use std::collections::BTreeSet;

use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::lignin::{tree_type_bytes, LigninHost, LigninSettled, LigninSubtree, LigninView},
    TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{prefix::build_prefix, Error};

use super::enter::{element_of, entered, host_of, Host};

/// What a group that writes over trees does to their subtrees.
#[derive(Debug, Default)]
pub(super) struct Left {
    /// The subtrees that are dropped with the group. None of them is below
    /// another.
    pub(super) drops: Vec<[u8; 32]>,
    /// The elements whose subtree is dropped and not entered again: each
    /// hosts nothing once the group is applied.
    pub(super) hosts: Vec<LigninHost>,
    /// The subtrees that are entered again with the group, each with the
    /// bytes of its tree type: a dropped one before those below it.
    pub(super) enters: Vec<(LigninSubtree, Vec<u8>)>,
}

/// The prefix of the subtree under `key` of the subtree at `path`.
fn prefix_under(path: &[Vec<u8>], key: &[u8]) -> [u8; 32] {
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

/// What `parts` do to the subtrees of the trees they write over.
pub(super) fn left(view: LigninView<'_>, parts: &[LigninSettled]) -> Result<Left, Error> {
    let written = |prefix: &[u8; 32]| {
        let mut written = parts.iter().flat_map(LigninSettled::written);
        written.any(|subtree| subtree.prefix == *prefix)
    };
    let own_business = |prefix: &[u8; 32]| {
        parts
            .iter()
            .any(|part| part.enters(prefix) || part.moves().iter().any(|(from, _)| from == prefix))
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
        let tree = match host_of(parts, &host) {
            Host::Stated(value) => {
                element_of(value).and_then(|element| element.root_key_and_tree_type_owned())
            }
            Host::Removed | Host::Stored => continue,
        };
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
        let indexed = |tree_type: TreeType| tree_type.is_indexed_primary();
        if !hangs || entry.tree_type().is_ok_and(indexed) {
            continue;
        }
        let again = match tree {
            None => None,
            Some((_, tree_type)) if indexed(tree_type) => continue,
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
                    continue;
                }
                written(&own).then_some(bytes)
            }
        };
        dropped.push((own, *subtree));
        match again {
            Some(tree_type) => {
                let subtree = LigninSubtree {
                    prefix: own,
                    host: Some(host),
                };
                left.enters.push((subtree, tree_type));
            }
            None => left.hosts.push(LigninHost {
                hosted: Vec::new(),
                ..host
            }),
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
    Ok(left)
}
