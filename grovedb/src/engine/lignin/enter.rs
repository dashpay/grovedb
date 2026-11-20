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
//! An element that hosts more than one Merk, an indexed tree, is refused:
//! its Merks are named together, by its element (GROVE-04).
//!
//! What this does not do: a subtree is not dropped when its element goes
//! (GROVE-05).

use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    store::{
        lignin::{tree_type_bytes, LigninNodeStore, LigninSettled, LigninSubtree, LigninView},
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

/// The bytes the directory keeps for the tree type of `subtree`.
fn tree_type(
    view: LigninView<'_>,
    parts: &[LigninSettled],
    subtree: &LigninSubtree,
) -> Result<Vec<u8>, Error> {
    let Some(host) = &subtree.host else {
        return Ok(match subtree.prefix {
            ROOT => tree_type_bytes(TreeType::NormalTree),
            _ => Vec::new(),
        });
    };
    let stated = (parts.iter().rev()).find_map(|part| part.stated_value(&host.subtree, &host.key));
    let read;
    let value = match stated {
        Some(value) => Some(value),
        None => {
            let parent = LigninNodeStore::read_only(view, LigninSubtree::alone(host.subtree));
            let no_cost = None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
            let node = parent.get_node(&host.key, no_cost, GroveVersion::latest());
            read = node.unwrap().map_err(merk_error)?;
            read.as_ref().map(|node| node.value_as_slice())
        }
    };
    // The bytes of an element are the same on every grove version.
    let element = value.and_then(|value| Element::deserialize(value, GroveVersion::latest()).ok());
    match element.as_ref().and_then(Element::tree_type) {
        Some(tree_type) if tree_type.is_indexed_primary() => Err(not_supported(
            "a tree that keeps more than one Merk under its element",
        )),
        Some(tree_type) => Ok(tree_type_bytes(tree_type)),
        None => Ok(Vec::new()),
    }
}

/// The subtrees that are entered with `parts`: those a part writes into
/// that neither a part nor the transaction has entered, each with the
/// bytes of its tree type, and a subtree after the one its host is in.
pub(super) fn owed(
    view: LigninView<'_>,
    parts: &[LigninSettled],
) -> Result<Vec<(LigninSubtree, Vec<u8>)>, Error> {
    let mut owed: Vec<&LigninSubtree> = Vec::new();
    for subtree in parts.iter().flat_map(LigninSettled::written) {
        let prefix = &subtree.prefix;
        if owed.iter().any(|owed| owed.prefix == *prefix)
            || parts.iter().any(|part| part.enters(prefix))
        {
            continue;
        }
        let handle = LigninNodeStore::read_only(view, LigninSubtree::alone(*prefix));
        if handle.entry().map_err(merk_error)?.is_none() {
            owed.push(subtree);
        }
    }
    // A subtree whose host is in a subtree that is entered here too comes
    // after it. What is left when no such order is found is entered as it
    // stands, and the transaction refuses what it must.
    let mut ordered: Vec<&LigninSubtree> = Vec::with_capacity(owed.len());
    while !owed.is_empty() {
        let waits = |subtree: &&LigninSubtree| {
            let host = subtree.host.as_ref().map(|host| host.subtree);
            host.is_some_and(|host| owed.iter().any(|other| other.prefix == host))
        };
        let (wait, ready): (Vec<_>, Vec<_>) = owed.iter().copied().partition(waits);
        if ready.is_empty() {
            ordered.extend(wait);
            break;
        }
        ordered.extend(ready);
        owed = wait;
    }
    ordered
        .into_iter()
        .map(|subtree| Ok((subtree.clone(), tree_type(view, parts, subtree)?)))
        .collect()
}
