//! Which subtrees a batch enters in the subtree directory.
//!
//! LigninDB keeps an entry for every subtree in use, and reads and writes
//! no record of a subtree without one (`transactions.md` CV9). On RocksDB
//! a subtree is its prefix, and GroveDB names none when it makes a tree.
//! So the engine says it for GroveDB, where a batch is applied:
//!
//! - **A subtree is entered by the first batch that writes into it**, in
//!   any column: a node of its Merk, a flat record, an aux or a meta
//!   record. A tree that was inserted and never written into has no entry,
//!   and is read as the empty tree that it is.
//! - **Its host is the element its handle was opened under**: the key of
//!   the last segment of the path, in the subtree of the segments before
//!   ([`LigninSubtree::host`]). A subtree with no host is the root Merk of
//!   the grove, or a keyspace that no element names, opened by its prefix
//!   alone. A first write under a key that holds no element once the
//!   batch is applied is refused with `Error::NotSupported`: the
//!   directory would name a host that is no node, or name none for a
//!   subtree that an element inserted later points at.
//! - **Its tree type is the one of that element**, read from the value
//!   that the last batch of the group to state one states for the
//!   element, or with none from the transaction. Where the element is no
//!   tree, or the subtree has no host, the directory keeps no tree type
//!   for the subtree, and `Merk::open_from_directory` refuses it as it
//!   refuses any other bytes.
//!
//! An element that hosts more than one Merk, an indexed tree, is refused
//! with `Error::NotSupported`: its Merks are named together, by its
//! element (GROVE-04). It is refused where a group writes into a subtree
//! whose element the group states as one, and where a subtree is entered
//! under one. A batch that enters a subtree itself has said its host and
//! its tree type, and neither is looked at here.
//!
//! What this does not do:
//!
//! - A subtree is not dropped when its element goes (GROVE-05), and its
//!   entry keeps the tree type and the host it was entered with when its
//!   element is written again as a tree of another type.
//! - Two transactions that each write first into one subtree both enter
//!   it, unless one commits before the other looks, and the directory
//!   takes the enter of the first to commit. The second is refused where
//!   its batch is applied, if the first committed by then, by the
//!   directory and with the engine's words. Otherwise it is refused at
//!   its commit: as a conflict where the two wrote a key, which two
//!   writes into a tree always did, and by the directory where they
//!   wrote none (`engine.md`, "Subtrees are entered by the first batch
//!   that writes into them").

use std::collections::BTreeMap;

use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::{
        lignin::{
            tree_type_bytes, LigninHost, LigninNodeStore, LigninOutcome, LigninSettled,
            LigninSubtree, LigninView,
        },
        NodeStore,
    },
    tree::kv::ValueDefinedCostType,
    TreeType,
};
use grovedb_storage::Error;
use grovedb_version::version::GroveVersion;

use super::{
    error::{merk_error, not_supported},
    ROOT,
};
use crate::Element;

/// The tree type of the element that `value` is the bytes of, if it is a
/// tree. The bytes of an element are the same on every grove version.
fn tree_of(value: &[u8]) -> Option<TreeType> {
    let element = Element::deserialize(value, GroveVersion::latest()).ok()?;
    element.tree_type()
}

/// The element `host` as the transaction has it: `None` if there is no
/// such node, and otherwise its tree type, if it is a tree.
fn tree_read(view: LigninView<'_>, host: &LigninHost) -> Result<Option<Option<TreeType>>, Error> {
    let parent = LigninNodeStore::read_only(view, LigninSubtree::alone(host.subtree));
    let no_cost = None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
    let node = parent.get_node(&host.key, no_cost, GroveVersion::latest());
    let node = node.unwrap().map_err(merk_error)?;
    Ok(node.as_ref().map(|node| tree_of(node.value_as_slice())))
}

/// What the parts make of an element.
enum Host<'a> {
    /// The last part that states a value for it states this one.
    Stated(&'a [u8]),
    /// The last part that writes it removes it.
    Removed,
    /// No part writes it, or those that do keep the value it has: it is
    /// what the transaction holds.
    Stored,
}

fn host_of<'a>(parts: &'a [LigninSettled], host: &LigninHost) -> Host<'a> {
    for part in parts.iter().rev() {
        match part.node(&host.subtree, &host.key) {
            Some(LigninOutcome::Put(Some(value))) => return Host::Stated(value),
            Some(LigninOutcome::Removed) => return Host::Removed,
            // A put that keeps the value keeps the one that an earlier
            // part states, and with none the transaction's.
            Some(LigninOutcome::Put(None)) | None => {}
        }
    }
    Host::Stored
}

fn one_merk(tree_type: Option<TreeType>) -> Result<Option<TreeType>, Error> {
    match tree_type {
        Some(tree_type) if tree_type.is_indexed_primary() => Err(not_supported(
            "a tree that keeps more than one Merk under its element",
        )),
        tree_type => Ok(tree_type),
    }
}

/// The subtrees that are entered with `parts`: those a part writes into
/// that neither a part nor the transaction has entered, each with the
/// bytes of its tree type, and a subtree after the one its host is in.
pub(super) fn owed(
    view: LigninView<'_>,
    parts: &[LigninSettled],
) -> Result<Vec<(LigninSubtree, Vec<u8>)>, Error> {
    // Each subtree once: of the parts that write into it, as the first
    // names it whose handle named a host.
    let mut written: BTreeMap<_, &LigninSubtree> = BTreeMap::new();
    for subtree in parts.iter().flat_map(LigninSettled::written) {
        match written.get(&subtree.prefix) {
            Some(known) if known.host.is_some() || subtree.host.is_none() => {}
            _ => {
                written.insert(subtree.prefix, subtree);
            }
        }
    }
    let mut owed: Vec<(LigninSubtree, Vec<u8>)> = Vec::new();
    for subtree in written.into_values() {
        let prefix = subtree.prefix;
        if parts.iter().any(|part| part.enters(&prefix)) {
            continue;
        }
        let host = subtree.host.as_ref();
        let host = host.map(|host| (host, host_of(parts, host)));
        let stated_tree = match host {
            Some((_, Host::Stated(value))) => one_merk(tree_of(value))?,
            _ => None,
        };
        let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(prefix));
        if handle.entry().map_err(merk_error)?.is_some() {
            continue;
        }
        // A subtree is entered under an element that is there once the
        // group is applied: the directory names no host that is no node.
        let no_element =
            || not_supported("a write into a subtree under a key that holds no element");
        let tree_type = match host {
            None if prefix == ROOT => Some(TreeType::NormalTree),
            None => None,
            Some((_, Host::Stated(_))) => stated_tree,
            Some((_, Host::Removed)) => return Err(no_element()),
            Some((host, Host::Stored)) => one_merk(tree_read(view, host)?.ok_or_else(no_element)?)?,
        };
        let named = subtree.clone();
        owed.push((named, tree_type.map(tree_type_bytes).unwrap_or_default()));
    }
    // A subtree whose host is in a subtree that is entered here too comes
    // after it: by how many of its hosts' subtrees are entered here.
    let depth = |subtree: &LigninSubtree| {
        let host = |subtree: &&LigninSubtree| {
            let host = subtree.host.as_ref()?.subtree;
            owed.iter()
                .map(|(owed, _)| owed)
                .find(|owed| owed.prefix == host)
        };
        let hosts = std::iter::successors(Some(subtree), host);
        hosts.take(owed.len()).count()
    };
    let mut ordered: Vec<_> = (owed.iter())
        .map(|(subtree, tree_type)| (depth(subtree), subtree, tree_type))
        .collect();
    ordered.sort_by_key(|(depth, ..)| *depth);
    Ok(ordered
        .into_iter()
        .map(|(_, subtree, tree_type)| (subtree.clone(), tree_type.clone()))
        .collect())
}
