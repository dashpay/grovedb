//! `rewrap_non_merk_tree_parent_element` version 1.
//!
//! Differs from [v0](super::v0) ONLY in which families get a stored
//! `NonCounted` wrapper restored: v1 restores it for every non-Merk tree
//! family, so an append to a wrapped `CommitmentTree` / `MmrTree` /
//! `BulkAppendTree` / `DenseAppendOnlyFixedSizeTree` no longer strips the
//! wrapper and flips the tree's contribution to a `CountTree` /
//! `CountSumTree` parent from 0 to 1 (v0 restored it for
//! `PrivateDocumentStore` alone).
//!
//! Selected by `GROVE_V4`+.

use crate::{Element, Error};

pub(super) fn rewrap_non_merk_tree_parent_element_v1(
    rebuilt: Element,
    stored_was_non_counted: bool,
) -> Result<Element, Error> {
    if stored_was_non_counted {
        // Infallible for the bare non-Merk tree elements the rewrite paths
        // build; the typed error is surfaced rather than unwrapped in case
        // a future change ever feeds a wrapper through.
        rebuilt.into_non_counted().map_err(Error::from)
    } else {
        Ok(rebuilt)
    }
}
