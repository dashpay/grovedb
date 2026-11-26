//! `average_case_merk_patch_element` — **v1** (`GROVE_V6`).
//!
//! [`super::v0`], with the hash calls of the put counted from the bytes of
//! the element (`element_put_hash_calls`). v0 counts the blocks of a length
//! that is the element's cost. A change of more bytes than the item has is
//! refused: v0 subtracts it from the size unchecked.
//!
//! On grove versions 1 to 5 a batch's estimate has every level up to the
//! root of the grove. A write that stops at a stop point has its own level
//! alone (`docs/lignindb/cost-model.md`, section 16.5).

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    estimated_costs::average_case_costs::{
        add_average_case_merk_propagate, EstimatedLayerInformation,
    },
    tree_type::TreeType,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::key_info::KeyInfo, estimated_costs::set_element_put_hash_calls, Element, Error, GroveDb,
};

impl GroveDb {
    /// `average_case_merk_patch_element` v1 — see the module documentation.
    pub(super) fn average_case_merk_patch_element_v1(
        key: &KeyInfo,
        value: &Element,
        change_in_bytes: i32,
        in_tree_type: TreeType,
        propagate_for_level: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        // v0 takes the change from the size of the item in unsigned
        // arithmetic.
        let size = cost_return_on_error_into_no_add!(cost, value.serialized_size(grove_version));
        if i64::from(change_in_bytes) > size as i64 {
            return Err(Error::InvalidParameter(
                "a patch cannot grow an item by more bytes than it has",
            ))
            .wrap_with_cost(cost);
        }
        cost_return_on_error!(
            &mut cost,
            Self::average_case_merk_patch_element_v0(
                key,
                value,
                change_in_bytes,
                in_tree_type,
                None,
                grove_version,
            )
        );
        cost_return_on_error_no_add!(
            cost,
            set_element_put_hash_calls(&mut cost, key, value, grove_version)
        );
        if let Some(level) = propagate_for_level {
            add_average_case_merk_propagate(&mut cost, level, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
