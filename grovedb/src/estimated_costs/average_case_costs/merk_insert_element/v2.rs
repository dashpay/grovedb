//! `average_case_merk_insert_element` — **v2** (`GROVE_V5`+).
//!
//! [`super::v1`], plus the price of creating a `PrivateDocumentStore` as the
//! apply prices it: the existence check every store creation makes (a store
//! is write-once) and the two hashes deriving its empty root. v1 already
//! prices a store's insert as the layered tree it is. Every other element is
//! estimated exactly as v1 estimates it.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    estimated_costs::average_case_costs::{
        add_average_case_merk_has_value, EstimatedLayerInformation,
    },
    tree_type::TreeType,
};
use grovedb_private_document_store::EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS;
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

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
        cost_return_on_error!(
            &mut cost,
            Self::average_case_merk_insert_element_v1(
                key,
                value,
                in_tree_type,
                propagate_for_level,
                grove_version,
            )
        );
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
