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
//!   the grove, or a keyspace that no element names.
//! - **Its tree type is the one of that element**, read from the value
//!   the batch states for the element, or with none from the transaction.
//!   Where there is no such element, or it is no tree, the directory keeps
//!   no tree type for the subtree, and `Merk::open_from_directory` refuses
//!   it as it refuses any other bytes.
//!
//! An element that hosts more than one Merk, an indexed tree, is refused
//! with `Error::NotSupported`: its Merks are named together, by its
//! element (GROVE-04). It is refused where a batch writes into a subtree
//! whose element the batch states as one, and where a subtree is entered
//! under one.
//!
//! What this does not do:
//!
//! - A subtree is not dropped when its element goes (GROVE-05), and its
//!   entry keeps the tree type and the host it was entered with when its
//!   element is written again as a tree of another type.
//! - Two transactions that each write first into one subtree both enter
//!   it. The one that commits second is refused where it is applied or
//!   committed, with the engine's words and not as a conflict, and its
//!   caller makes the operation again (`engine.md`, "Subtrees are
//!   entered by the engine").

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

/// The tree type of the element `host` as the transaction has it.
fn tree_read(view: LigninView<'_>, host: &LigninHost) -> Result<Option<TreeType>, Error> {
    let parent = LigninNodeStore::read_only(view, LigninSubtree::alone(host.subtree));
    let no_cost = None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
    let node = parent.get_node(&host.key, no_cost, GroveVersion::latest());
    let node = node.unwrap().map_err(merk_error)?;
    Ok(node
        .as_ref()
        .and_then(|node| tree_of(node.value_as_slice())))
}

/// What the parts make of the element `host`: the outcome of the last
/// part that holds one for its node.
fn stated<'a>(parts: &'a [LigninSettled], host: &LigninHost) -> Option<LigninOutcome<'a>> {
    (parts.iter().rev()).find_map(|part| part.node(&host.subtree, &host.key))
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
    let mut owed: Vec<(&LigninSubtree, Vec<u8>)> = Vec::new();
    for subtree in written.into_values() {
        let prefix = &subtree.prefix;
        if parts.iter().any(|part| part.enters(prefix)) {
            continue;
        }
        let stated = subtree.host.as_ref().and_then(|host| stated(parts, host));
        let stated_tree = match stated {
            Some(LigninOutcome::Put(Some(value))) => one_merk(tree_of(value))?,
            _ => None,
        };
        let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(*prefix));
        if handle.entry().map_err(merk_error)?.is_some() {
            continue;
        }
        let tree_type = match (&subtree.host, stated) {
            (None, _) if *prefix == ROOT => Some(TreeType::NormalTree),
            // The batch deletes the element, or there is none to read.
            (None, _) | (_, Some(LigninOutcome::Removed)) => None,
            (_, Some(LigninOutcome::Put(Some(_)))) => stated_tree,
            // The element is not written, or keeps the value it has.
            (Some(host), Some(LigninOutcome::Put(None)) | None) => {
                one_merk(tree_read(view, host)?)?
            }
        };
        owed.push((subtree, tree_type.map(tree_type_bytes).unwrap_or_default()));
    }
    // A subtree whose host is in a subtree that is entered here too comes
    // after it: by how many of its hosts' subtrees are entered here.
    let depth = |subtree: &LigninSubtree| {
        let host = |subtree: &&LigninSubtree| {
            let host = subtree.host.as_ref()?.subtree;
            let mut owed = owed.iter().map(|(owed, _)| *owed);
            owed.find(|owed| owed.prefix == host)
        };
        let hosts = std::iter::successors(Some(subtree), host);
        hosts.take(owed.len()).count()
    };
    let mut ordered: Vec<_> = (owed.iter())
        .map(|(subtree, tree_type)| (depth(subtree), *subtree, tree_type))
        .collect();
    ordered.sort_by_key(|(depth, ..)| *depth);
    Ok(ordered
        .into_iter()
        .map(|(_, subtree, tree_type)| (subtree.clone(), tree_type.clone()))
        .collect())
}
