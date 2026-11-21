//! What a restore asks of the subtree directory (state sync, GROVE-07).
//!
//! A restore writes a subtree through an immediate handle, which enters
//! nothing ([`super::enter`] is for the batches of an operation), and
//! hands a Merk over as a write of that Merk alone, so that the Merk is
//! anchored: the directory names its root and the cell of its host does
//! not (`node-store.md`, "A Merk under an element"). [`adopt`] is what
//! [`Engine::adopt_restored_subtrees`](crate::Engine::adopt_restored_subtrees)
//! does on this engine, for the element under the last segment of a path:
//!
//! - **It enters the subtrees the element hosts that are not entered**,
//!   each under the element at its position and with its tree type: the
//!   subtree of the path, and for an indexed tree its primary and the
//!   secondary of each axis together ([`hosting`]). A subtree that is
//!   entered under the element at its position is left as it is. One that
//!   is entered at another place is refused with `Error::NotSupported`,
//!   and nothing is entered.
//! - **It names the roots in the element's node**, where the directory
//!   holds a root for one of those subtrees: the node is put again with
//!   the value, the feature, the hashes and the links it has, and with
//!   every subtree its element hosts, by position
//!   (`LigninNodeStore::name_hosted`).
//!
//! Both are writes of the transaction the restore writes through, and go
//! with it when it is rolled back. Neither is a write of a record for the
//! cost rules. A read that the transaction refuses is the error of the
//! call: the element is read by its key and an entry by its prefix, and
//! neither read is taken for an absence.

use grovedb_merk::store::lignin::{LigninHost, LigninNodeStore, LigninSubtree, LigninView};
use grovedb_path::SubtreePath;
use grovedb_storage::{prefix::build_prefix, Error};

use super::{
    enter::{element_read, hosting, Hosting},
    error::{merk_error, not_supported},
    named,
};

/// Enters the subtrees of the element at `path` that are not entered, and
/// names in the element's node the roots the directory holds for them. The
/// empty path has no element, and nothing is done for it: the engine
/// enters the root Merk with the database.
pub(super) fn adopt<'b, B: AsRef<[u8]> + 'b>(
    view: LigninView<'_>,
    path: SubtreePath<'b, B>,
) -> Result<(), Error> {
    let Some((parent, key)) = path.derive_parent() else {
        return Ok(());
    };
    // The prefixes are derived for the directory and charged nothing.
    let own = build_prefix(path).value;
    let above = named(parent, true).value;
    let element = LigninHost {
        subtree: above.prefix,
        key: key.to_vec(),
        hosted: vec![own],
    };
    let hosted = match element_read(view, &element)? {
        Some(Some(read)) => match hosting(&read, own)? {
            Hosting::One(Some(tree_type)) => vec![(own, tree_type)],
            Hosting::Many(hosted) => hosted,
            Hosting::One(None) | Hosting::Nothing => {
                return Err(not_supported(
                    "a restore of the subtree of an element that is no tree",
                ))
            }
        },
        Some(None) | None => {
            return Err(not_supported(
                "a restore of the subtree under a key that holds no element",
            ))
        }
    };
    let host = LigninHost {
        hosted: hosted.iter().map(|(prefix, _)| *prefix).collect(),
        ..element
    };

    // What the directory holds is read for every subtree before one is
    // entered, so that a refusal leaves the directory as it was.
    let mut owed = Vec::new();
    let mut rooted = false;
    for (position, (prefix, tree_type)) in hosted.iter().enumerate() {
        let subtree = LigninSubtree {
            prefix: *prefix,
            host: Some(host.clone()),
        };
        let handle = LigninNodeStore::immediate(view, subtree);
        match handle.entry().map_err(merk_error)? {
            None => owed.push((handle, *tree_type)),
            Some(entry) => {
                let here = entry.host.is_some_and(|at| {
                    (at.subtree, &at.key, usize::from(at.position))
                        == (host.subtree, &host.key, position)
                });
                if !here {
                    return Err(not_supported(
                        "a restore of a subtree that is entered at another place than its \
                         element names",
                    ));
                }
                rooted |= entry.root_key.is_some();
            }
        }
    }
    for (handle, tree_type) in owed {
        handle.enter(tree_type).map_err(merk_error)?;
    }
    if rooted {
        let handle = LigninNodeStore::immediate(view, above);
        (handle.name_hosted(&host.key, &host.hosted)).map_err(merk_error)?;
    }
    Ok(())
}
