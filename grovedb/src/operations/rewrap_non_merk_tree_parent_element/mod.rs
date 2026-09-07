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

#[cfg(test)]
mod tests {
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{Element, Error, GroveDb};

    /// v0 (GROVE_V3) restores the wrapper for a PrivateDocumentStore — the
    /// carve-out that was always safe because the type cannot exist before
    /// GROVE_V4.
    #[test]
    fn v0_restores_wrapper_for_private_document_store_only() {
        let pds = Element::empty_private_document_store(32, 4).expect("valid config");
        let rewrapped = GroveDb::rewrap_non_merk_tree_parent_element(pds.clone(), true, &GROVE_V3)
            .expect("rewrap");
        assert!(rewrapped.is_non_counted());

        // Every other family keeps the released wrapper drop under v0.
        let mmr = Element::empty_mmr_tree();
        let bare =
            GroveDb::rewrap_non_merk_tree_parent_element(mmr, true, &GROVE_V3).expect("rewrap");
        assert!(!bare.is_non_counted());

        // An unwrapped stored element passes through on both variants.
        let untouched =
            GroveDb::rewrap_non_merk_tree_parent_element(pds, false, &GROVE_V3).expect("rewrap");
        assert!(!untouched.is_non_counted());
    }

    /// v1 (GROVE_V4+) restores the wrapper for every family, and surfaces a
    /// typed error if a non-wrappable element were ever fed through.
    #[test]
    fn v1_restores_wrapper_and_surfaces_wrap_errors() {
        let grove_version = GroveVersion::latest();
        let mmr = Element::empty_mmr_tree();
        let rewrapped =
            GroveDb::rewrap_non_merk_tree_parent_element(mmr.clone(), true, grove_version)
                .expect("rewrap");
        assert!(rewrapped.is_non_counted());

        let untouched = GroveDb::rewrap_non_merk_tree_parent_element(mmr, false, grove_version)
            .expect("rewrap");
        assert!(!untouched.is_non_counted());

        // The rewrite paths only build bare elements, but a cross-wrapper
        // input must error rather than nest wrappers.
        let not_summed = Element::empty_sum_tree()
            .into_not_summed()
            .expect("wrap in NotSummed");
        let result = GroveDb::rewrap_non_merk_tree_parent_element(not_summed, true, grove_version);
        assert!(matches!(result, Err(Error::ElementError(_))));
    }

    /// An unknown slot value fails closed with a version-mismatch error.
    #[test]
    fn unknown_version_fails_closed() {
        let mut version = GroveVersion::latest().clone();
        version
            .grovedb_versions
            .operations
            .non_merk_tree
            .parent_element_rewrap = 9;
        let result =
            GroveDb::rewrap_non_merk_tree_parent_element(Element::empty_mmr_tree(), true, &version);
        assert!(matches!(result, Err(Error::VersionError(_))));
    }
}
