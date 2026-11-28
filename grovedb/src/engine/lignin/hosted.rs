//! What an element hosts on this engine, said to a writer that states a
//! grove to the engine's database itself.
//!
//! The engine keeps the subtrees of an element together, each at its
//! position under the element's node ([`super::enter`]), and it is the one
//! place that knows which elements host more than one Merk and how the
//! secondaries of an indexed tree are named. The batches of GroveDB's own
//! operations are told by the engine where a group is applied. A writer
//! that hands whole Merks to the database with no batch of GroveDB's, as
//! the migrator from RocksDB does (`lignin-tools`, OPS-02), asks
//! [`hosted_by_element`] instead, and states what it answers: the enter of
//! each subtree under the element at its position, and the subtrees that
//! are entered in the put of the element's node.

use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, TreeType};
use grovedb_storage::{prefix::secondary_prefix_for, Error};

use super::{
    enter::{element_of, hosting, Hosting},
    error::failed,
};
use crate::operations::indexed_tree::{axis_storage_tag, indexed_element_axes};

/// One subtree that an element hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedSubtree {
    /// The 32 bytes that name the subtree.
    pub prefix: [u8; 32],
    /// The tree type the engine enters the subtree with.
    pub tree_type: TreeType,
    /// The key of the root node that the element names for the Merk of the
    /// subtree. `None` where the element names none: for a Merk with no
    /// node, for the secondary of an axis whose root the element does not
    /// name, which GroveDB opens by its root-key record, and for a tree
    /// that keeps its data as flat records.
    pub root_key: Option<Vec<u8>>,
}

/// The subtrees that the element with the bytes `value` hosts on this
/// engine, by position, where `own` is the subtree of the element's own
/// path.
///
/// - Bytes of no element, and an element that is no tree, host none.
/// - A tree that is no indexed tree hosts the subtree of its own path: a
///   tree whose data is one Merk, and a tree that keeps its data as flat
///   records.
/// - An indexed tree hosts its primary, at `own`, and then the secondary
///   of each axis in the element's order. An axis that the element names
///   twice has one secondary, at the place of its first naming, with the
///   root key of that naming.
///
/// These are the subtrees the engine enters for the element when one of
/// them is first written, each at its place in this list, and no other.
/// Whether a subtree is entered is not said here: the engine enters none
/// for a tree that nothing was written into.
pub fn hosted_by_element(value: &[u8], own: [u8; 32]) -> Result<Vec<HostedSubtree>, Error> {
    let Some(element) = element_of(value) else {
        return Ok(Vec::new());
    };
    let named = |element: &crate::Element| {
        let named = element.root_key_and_tree_type();
        named.and_then(|(root_key, _)| root_key.clone())
    };
    match hosting(&element, own)? {
        Hosting::Nothing | Hosting::One(None) => Ok(Vec::new()),
        Hosting::One(Some(tree_type)) => Ok(vec![HostedSubtree {
            prefix: own,
            tree_type,
            root_key: named(&element),
        }]),
        Hosting::Many(merks) => {
            let axes = indexed_element_axes(&element)
                .map_err(|error| failed(format!("an indexed tree: {error}")))?;
            let hosted = merks.into_iter().enumerate();
            let hosted = hosted.map(|(position, (prefix, tree_type))| {
                let of_axis = || {
                    let first = axes.iter().find(|(axis, _)| {
                        secondary_prefix_for(&own, axis_storage_tag(*axis)).value == prefix
                    });
                    first.and_then(|(_, root_key)| root_key.clone())
                };
                HostedSubtree {
                    prefix,
                    tree_type,
                    root_key: match position {
                        0 => named(&element),
                        _ => of_axis(),
                    },
                }
            });
            Ok(hosted.collect())
        }
    }
}
