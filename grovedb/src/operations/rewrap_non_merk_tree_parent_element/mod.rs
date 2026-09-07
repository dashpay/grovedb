//! `rewrap_non_merk_tree_parent_element` — versioned dispatch.
//!
//! Every typed write to a non-Merk data tree — the direct appends
//! (`commitment_tree_insert`, `mmr_tree_append`, `bulk_append`,
//! `dense_tree_insert`, `private_document_store_insert`) and the batch
//! `ReplaceNonMerkTreeRoot` op they all preprocess into — rewrites the
//! parent-Merk element that anchors the tree, rebuilding it from the new
//! entry count and the stored element's flags. The rebuild constructors
//! (`Element::new_mmr_tree` and friends) produce a BARE element, so a stored
//! `NonCounted(tree)` needs its wrapper restored on the way out or the
//! rewrite strips it.
//!
//! Whether the wrapper is restored is **consensus-critical** and
//! version-gated on `operations.non_merk_tree.parent_element_rewrap`: the
//! wrapper is what makes the tree contribute 0 instead of 1 to a `CountTree`
//! / `CountSumTree` parent, so restoring it changes the parent aggregate and
//! with it the root hash.
//!
//! * **[v0]** — released behaviour, `GROVE_V1`..`GROVE_V3`. The element is
//!   written bare and the wrapper is silently dropped by the first append —
//!   the parent count flips from 0 to 1. The one exception is
//!   `PrivateDocumentStore`, whose wrapper is restored: the element type
//!   cannot exist before `GROVE_V4` (all its slots are 0), so preserving it
//!   never altered a committed outcome.
//! * **[v1]** — `GROVE_V4`+. The wrapper is restored for every family.
//!
//! Every rewrite path already reads the stored element (its flags are
//! preserved from it), so the versions charge the same reads — the gate
//! exists because v1 moves a committed root hash and rewrites wrapper bytes
//! that v0 dropped.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_version::version::GroveVersion;

use crate::{Element, Error, GroveDb};

impl GroveDb {
    /// Restore a stored `NonCounted` wrapper on a rebuilt non-Merk tree
    /// parent element, if the grove version calls for it.
    ///
    /// `rebuilt` is the bare element freshly constructed with the tree's
    /// updated entry count and the stored element's flags;
    /// `stored_was_non_counted` says whether the stored element the rewrite
    /// replaces was `NonCounted`-wrapped.
    pub(crate) fn rewrap_non_merk_tree_parent_element(
        rebuilt: Element,
        stored_was_non_counted: bool,
        grove_version: &GroveVersion,
    ) -> Result<Element, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .non_merk_tree
            .parent_element_rewrap
        {
            0 => v0::rewrap_non_merk_tree_parent_element_v0(rebuilt, stored_was_non_counted),
            1 => v1::rewrap_non_merk_tree_parent_element_v1(rebuilt, stored_was_non_counted),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "rewrap_non_merk_tree_parent_element".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
