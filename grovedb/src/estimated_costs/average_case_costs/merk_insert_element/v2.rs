//! `average_case_merk_insert_element` — **v2** (`GROVE_V5`+).
//!
//! [`super::v1`], with two additions that price an insert as the apply
//! prices it:
//!
//! * A tree wrapped in `NonCounted`, `NotSummed` or `NotCountedOrSummed` is
//!   sized with the byte of its wrapper, as the dedicated
//!   `average_case_merk_insert_tree` sizes one. v1 looks through the wrapper
//!   and sizes the tree alone, so its `added_bytes` come in 1 below the
//!   applied ones.
//! * Creating a `PrivateDocumentStore` also pays the existence check every
//!   store creation makes (a store is write-once) and the two hashes deriving
//!   its empty root. v1 already prices a store's insert as the layered tree
//!   it is.
//!
//! Every other element, an unwrapped tree included, is estimated exactly as
//! v1 estimates it.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    estimated_costs::average_case_costs::{
        add_average_case_merk_has_value, EstimatedLayerInformation,
    },
    tree_type::TreeType,
};
use grovedb_private_document_store::EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS;
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

/// The byte a `NonCounted`, `NotSummed` or `NotCountedOrSummed` wrapper adds
/// to the element it wraps, which the apply charges on top of the tree's own
/// size.
const WRAPPER_OVERHEAD: u32 = 1;

impl GroveDb {
    /// `average_case_merk_insert_element` v2 — see the module documentation.
    pub(super) fn average_case_merk_insert_element_v2(
        key: &KeyInfo,
        value: &Element,
        in_tree_type: TreeType,
        propagate_for_level: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let insert = match value.tree_flags_and_type() {
            // Wrappers do not nest, so a wrapped tree carries exactly one.
            Some((flags, tree_type)) if value.is_wrapped() => Self::average_case_merk_insert_tree(
                key,
                flags,
                tree_type,
                in_tree_type,
                WRAPPER_OVERHEAD,
                propagate_for_level,
                grove_version,
            ),
            _ => Self::average_case_merk_insert_element_v1(
                key,
                value,
                in_tree_type,
                propagate_for_level,
                grove_version,
            ),
        };
        cost_return_on_error!(&mut cost, insert);
        if matches!(value.underlying(), Element::PrivateDocumentStore(..)) {
            // The existence check, priced as `InsertIfNotExists` prices one,
            // and the empty root's derivation.
            let estimated_element_size =
                cost_return_on_error_into_no_add!(cost, value.serialized_size(grove_version))
                    as u32;
            add_average_case_merk_has_value(
                &mut cost,
                key.max_length() as u32,
                estimated_element_size,
            );
            cost.hash_node_calls = cost
                .hash_node_calls
                .saturating_add(EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS);
        }
        Ok(()).wrap_with_cost(cost)
    }
}
