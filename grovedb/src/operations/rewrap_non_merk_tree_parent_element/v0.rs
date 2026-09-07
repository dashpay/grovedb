//! `rewrap_non_merk_tree_parent_element` version 0.
//!
//! Differs from [v1](super::v1) ONLY in which families get a stored
//! `NonCounted` wrapper restored: v0 restores it for `PrivateDocumentStore`
//! alone and writes every other family bare, dropping the wrapper — the
//! released behaviour, preserved because restoring it changes a `CountTree`
//! / `CountSumTree` parent's aggregate and the root hash. The
//! `PrivateDocumentStore` exception is safe at every version: the element
//! type cannot exist before `GROVE_V4`, so its preservation never altered a
//! committed outcome.
//!
//! Selected by `GROVE_V1`..`GROVE_V3`.

use crate::{Element, Error};

pub(super) fn rewrap_non_merk_tree_parent_element_v0(
    rebuilt: Element,
    stored_was_non_counted: bool,
) -> Result<Element, Error> {
    if stored_was_non_counted && matches!(rebuilt, Element::PrivateDocumentStore(..)) {
        rebuilt.into_non_counted().map_err(|_| {
            Error::CorruptedCodeExecution(
                "into_non_counted called on a wrapped element during non-Merk tree parent \
                 element rewrap",
            )
        })
    } else {
        Ok(rebuilt)
    }
}
