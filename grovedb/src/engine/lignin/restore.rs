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
//!   (`LigninNodeStore::name_hosted`). The cell of the node grows by
//!   what it comes to name, so the write holds it: the node above it in
//!   its Merk is put again as it is, too. Where the node is the root of
//!   its Merk, the element of that Merk is named again in the same
//!   write, with the node above that one, and so on up
//!   ([`elements_above`]).
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

/// The elements of the Merk at `merk` and of the Merks above it,
/// innermost first, each with every Merk it hosts, for as long as the
/// node below is the root of its Merk: first the node under `key` of the
/// Merk at `merk`, then the node of each element that is taken in. A
/// write that names a Merk in such a node holds the element above it
/// (`LigninNodeStore::name_hosted`). The way up ends at the root Merk,
/// and at an element that is not read as a tree.
fn elements_above<'b, B: AsRef<[u8]> + 'b>(
    view: LigninView<'_>,
    mut merk: SubtreePath<'b, B>,
    key: &[u8],
) -> Result<Vec<LigninHost>, Error> {
    let mut above = Vec::new();
    let mut key = key.to_vec();
    loop {
        let own = build_prefix(merk.clone()).value;
        let entry = view.entry(&own);
        let root = entry.map_err(merk_error)?.and_then(|entry| entry.root_key);
        let is_root = root.as_deref() == Some(key.as_slice());
        let Some((upper, upper_key)) = merk.derive_parent().filter(|_| is_root) else {
            return Ok(above);
        };
        let element = LigninHost {
            subtree: build_prefix(upper.clone()).value,
            key: upper_key.to_vec(),
            hosted: vec![own],
        };
        let hosted = match element_read(view, &element)? {
            Some(Some(read)) => match hosting(&read, own)? {
                Hosting::One(Some(_)) => vec![own],
                Hosting::Many(hosted) => hosted.into_iter().map(|(prefix, _)| prefix).collect(),
                Hosting::One(None) | Hosting::Nothing => return Ok(above),
            },
            Some(None) | None => return Ok(above),
        };
        above.push(LigninHost { hosted, ..element });
        (merk, key) = (upper, upper_key.to_vec());
    }
}

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
    let above = named(parent.clone(), true).value;
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
        let elements = elements_above(view, parent, key)?;
        let handle = LigninNodeStore::immediate(view, above);
        (handle.name_hosted(&host.key, &host.hosted, &elements)).map_err(merk_error)?;
    }
    Ok(())
}
