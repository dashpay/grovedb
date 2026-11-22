//! The move of a tree whose walk found no subtree.
//!
//! GroveDB works out a batch `Move` from the element it read under the
//! old key, stages the put and the delete of that element, and asks the
//! engine afterwards to walk the subtree of the tree's path
//! (`Engine::move_subtree`): for a tree whose data is a Merk where its
//! element names a root, and for a tree that keeps its data flat always
//! (`start_tree_copy` in `grovedb/src/batch/move_element.rs`). A walk
//! that ends on a subtree with no entry tells the batch so
//! ([`LigninSettled::moves_of_none`]). Where the element that the batch
//! puts under the new key says that its tree holds no data, the move is
//! the put and the delete of that element.
//!
//! Where the element that the batch puts under the new key says that the
//! tree holds data, the directory does not hold what that element says:
//! the batch would put an element that names data over a prefix with no
//! subtree. The engine refuses it as a conflict, with nothing of it
//! applied. A commit beside the transaction that writes an item over the
//! tree, between the mover's read of the element and the walk, makes
//! this. So does one writer that inserts an element which counts entries
//! that its tree does not hold, and moves it.
//!
//! An element says that its tree holds data where it names a root, for a
//! tree whose data is a Merk, and where it counts at least one entry, for
//! a tree that keeps its data flat. Of a secondary Merk of an indexed
//! tree the asked walk says it: GroveDB asks for it where the element
//! names a root for that Merk.

use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, store::lignin::LigninSettled};
use grovedb_storage::{error::CommitError, Error};

use super::{
    enter::{element_of, hosted_by},
    over::prefix_under,
};

/// Refuses, as a conflict, a group with a batch that was told of a walk
/// which found no subtree ([`LigninSettled::moves_of_none`]) and that
/// puts, over the prefix the subtree was to go to, an element which says
/// that its tree holds data. The element is looked for among the nodes
/// that the batch puts with a stated value in the Merks whose path the
/// engine knows.
pub(super) fn found_none(parts: &[LigninSettled]) -> Result<(), Error> {
    for part in parts {
        for (_, to) in part.moves_of_none() {
            for subtree in part.written() {
                let Some(path) = part.path(&subtree.prefix) else {
                    continue;
                };
                for (key, value) in part.stated(&subtree.prefix) {
                    let hosted = hosted_by(value, prefix_under(path, key))?;
                    let at = hosted.iter().position(|hosted| hosted == to);
                    if at.is_some_and(|at| holds_data(value, at)) {
                        return Err(Error::Commit(CommitError::Conflict));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether the element with the bytes `value` says that the subtree at
/// `position` among those it hosts holds data.
fn holds_data(value: &[u8], position: usize) -> bool {
    let Some(element) = element_of(value) else {
        return false;
    };
    match (position, element.non_merk_entry_count()) {
        (0, Some(entries)) => entries > 0,
        (0, None) => {
            let tree = element.root_key_and_tree_type_owned();
            tree.is_some_and(|(root_key, _)| root_key.is_some())
        }
        // A secondary Merk, which the walk is asked for where the element
        // names a root for it.
        _ => true,
    }
}
