//! The checks a batch makes on a tree element it is handed, and where a tree
//! the batch both creates and writes into meets them
//! (`apply_batch.filled_tree_insert_checks`).
//!
//! An insert of a tree with operations under it is never applied as an
//! insert: upward propagation replaces it with the root state the level
//! below computed (`insert_op_with_propagated_root`), and the op that is
//! applied is `InsertTreeWithRootHash`, `InsertNonMerkTree` or
//! `InsertAggregateIndexedTreeRootKeys`. The checks the insert arm of
//! `execute_ops_on_path` makes on the caller's element are therefore not
//! reached for it.
//!
//! - v0 (V1..V4): they are made in the insert arm only, so a tree the batch
//!   also fills skips them. Frozen, it is what GroveDB 6.1.1 does.
//! - v1 (V5+): the checks that need nothing but the element
//!   ([`check_inserted_tree_element`]) are made where every op enters the
//!   batch, and the ones that need the parent's tree type
//!   ([`check_wrapper_against_parent`], and the child rule of an indexed
//!   parent in `indexed_tree::pre_state`) where the propagated op is
//!   applied.
//!
//! The functions here are the single copy of each check: the insert arm
//! calls them on every version.

use grovedb_merk::TreeType;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::{operations::private_document_store::check_pds_enabled, Element, Error};

/// Whether a tree the batch both creates and fills is held to the insert
/// checks, as `grovedb_versions.apply_batch.filled_tree_insert_checks`
/// selects: `0` (V1..V4) leaves it out, `1` (V5) holds it to them. A slot
/// value it does not know is an error.
pub(in crate::batch) fn holds_filled_trees_to_insert_checks(
    grove_version: &GroveVersion,
) -> Result<bool, Error> {
    match grove_version
        .grovedb_versions
        .apply_batch
        .filled_tree_insert_checks
    {
        0 => Ok(false),
        1 => Ok(true),
        version => Err(GroveVersionError::UnknownVersionMismatch {
            method: "apply_batch: filled tree insert checks".to_string(),
            known_versions: vec![0, 1],
            received: version,
        }
        .into()),
    }
}

/// The checks an insert makes on the tree element it is handed that need
/// nothing but the element.
///
/// - An indexed tree is created empty: no root key, a zero aggregate, and
///   for the tree with several axes canonical axes. The root keys the
///   element carries are the ones its secondaries are opened with, and the
///   axes are committed in the order given.
/// - A `PrivateDocumentStore` is created empty, with a config its
///   constructors accept, on a version that knows the element.
///
/// Any other element passes: a Merk tree's root key and aggregate are
/// neither read nor checked by a batch insert.
pub(in crate::batch) fn check_inserted_tree_element(
    element: &Element,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    match element.underlying() {
        Element::ProvableCountIndexedTree(primary, secondary, count_value, _) => {
            if primary.is_some() || secondary.is_some() || *count_value != 0 {
                return Err(Error::InvalidBatchOperation(
                    "a CountIndexedTree must be empty at the moment of batch insertion (both \
                     primary_root_key and secondary_root_key must be None and count = 0); \
                     item-level mutations require the dedicated insert_into_count_indexed_tree \
                     API",
                ));
            }
        }
        Element::ProvableSumIndexedTree(primary, secondary, sum_value, _) => {
            if primary.is_some() || secondary.is_some() || *sum_value != 0 {
                return Err(Error::InvalidBatchOperation(
                    "a ProvableSumIndexedTree must be empty at the moment of batch insertion \
                     (both primary_root_key and secondary_root_key must be None and sum = 0); \
                     item-level mutations require the dedicated insert_into_indexed_tree API",
                ));
            }
        }
        Element::ProvableCountProvableSumIndexedTree(primary, count_value, sum_value, axes, _) => {
            let axes_all_empty = axes.iter().all(|(_, sk)| sk.is_none());
            if primary.is_some() || *count_value != 0 || *sum_value != 0 || !axes_all_empty {
                return Err(Error::InvalidBatchOperation(
                    "a ProvableCountProvableSumIndexedTree must be empty at the moment of batch \
                     insertion (primary_root_key = None, count = 0, sum = 0, every axis \
                     secondary_root_key = None); item-level mutations require the dedicated \
                     insert_into_indexed_tree API",
                ));
            }
            // The same check the element's constructors run: the axes digest
            // is computed over whatever list is supplied, in its order.
            Element::validate_pcpsit_axes(axes).map_err(|_| {
                Error::InvalidBatchOperation(
                    "a ProvableCountProvableSumIndexedTree must have canonical axes (1..=3 \
                     entries, sorted ascending by tag, no duplicates, tags in 0..=2)",
                )
            })?;
        }
        Element::ProvableBigSumIndexedTree(primary, secondary, sum_value, _) => {
            if primary.is_some() || secondary.is_some() || *sum_value != 0 {
                return Err(Error::InvalidBatchOperation(
                    "a ProvableBigSumIndexedTree must be empty at the moment of batch insertion \
                     (both primary_root_key and secondary_root_key must be None and sum = 0); \
                     item-level mutations require the dedicated insert_into_indexed_tree API",
                ));
            }
        }
        Element::ProvableCountProvableBigSumIndexedTree(
            primary,
            count_value,
            sum_value,
            axes,
            _,
        ) => {
            let axes_all_empty = axes.iter().all(|(_, sk)| sk.is_none());
            if primary.is_some() || *count_value != 0 || *sum_value != 0 || !axes_all_empty {
                return Err(Error::InvalidBatchOperation(
                    "a ProvableCountProvableBigSumIndexedTree must be empty at the moment of \
                     batch insertion (primary_root_key = None, count = 0, sum = 0, every axis \
                     secondary_root_key = None); item-level mutations require the dedicated \
                     insert_into_indexed_tree API",
                ));
            }
            Element::validate_pcpbsit_axes(axes).map_err(|_| {
                Error::InvalidBatchOperation(
                    "a ProvableCountProvableBigSumIndexedTree must have canonical axes (1..=3 \
                     entries, sorted ascending by tag, no duplicates, each of the count, avg \
                     and big sum axes)",
                )
            })?;
        }
        Element::PrivateDocumentStore(total_count, entry_size, chunk_power, _) => {
            // Fail closed on protocol versions that predate the element type.
            check_pds_enabled(
                "batch insert Element::PrivateDocumentStore",
                grove_version
                    .grovedb_versions
                    .operations
                    .private_document_store
                    .element_creation,
            )?;
            // Only empty-creation is allowed: the child hash written is the
            // empty state root for this config, and entries can only be added
            // through the typed append path.
            if *total_count != 0 {
                return Err(Error::InvalidBatchOperation(
                    "a PrivateDocumentStore must be empty at the moment of batch insertion \
                     (total_count = 0); entries are appended via the PrivateDocumentStoreInsert \
                     operation",
                ));
            }
            // The config validation the element constructors enforce: the
            // config is committed into the state root.
            if *entry_size == 0 || *entry_size > u16::MAX as u32 || !(1..=16).contains(chunk_power)
            {
                return Err(Error::InvalidBatchOperation(
                    "a PrivateDocumentStore requires entry_size in 1..=65535 and chunk_power in \
                     1..=16",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// The parent's rules for a wrapped child: a wrapper is only valid inside a
/// tree that keeps the aggregate it leaves the child out of. Mirrors the
/// per-Merk insert guard (`validate_insertable_into`), with the batch's own
/// error.
pub(in crate::batch) fn check_wrapper_against_parent(
    element: &Element,
    in_tree_type: TreeType,
) -> Result<(), Error> {
    if element.is_non_counted() && !in_tree_type.accepts_non_counted_children() {
        return Err(Error::InvalidBatchOperation(
            "non-counted elements may only be inserted into non-provable count-bearing trees \
             (CountTree or CountSumTree); Provable* count trees commit the count \
             cryptographically and cannot host NonCounted children",
        ));
    }
    if element.is_not_summed() && !in_tree_type.is_sum_bearing() {
        return Err(Error::InvalidBatchOperation(
            "not-summed elements may only be inserted into sum-bearing trees",
        ));
    }
    if element.is_not_counted_or_summed() && !in_tree_type.accepts_not_counted_or_summed_children()
    {
        return Err(Error::InvalidBatchOperation(
            "not-counted-or-summed elements may only be inserted into CountSumTree; \
             ProvableCountSumTree commits the count cryptographically and cannot host \
             NotCountedOrSummed children",
        ));
    }
    Ok(())
}
