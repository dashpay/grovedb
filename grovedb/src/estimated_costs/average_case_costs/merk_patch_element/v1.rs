//! `average_case_merk_patch_element` — **v1** (`GROVE_V6`).
//!
//! The patch of an item, sized from the bytes of the item as it is
//! serialized, which have its flags. [`super::v0`] adds the flags to those
//! bytes once more: where that takes the length of the record over a byte
//! of its own length, or keeps it from one, the added bytes are off. A
//! change of more bytes than the item has is refused: v0 subtracts it from
//! the size unchecked. So is an item too long for a node to be sized by its
//! bytes (`sized_for_a_node`). The item is measured once, and the hash calls
//! of the put are counted from that length (`element_put_hash_calls`).
//!
//! On grove versions 1 to 5 a batch's estimate has every level up to the
//! root of the grove. A write that stops at a stop point has its own level
//! alone (`docs/lignindb/cost-model.md`, section 16.5).

use grovedb_costs::{cost_return_on_error_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        add_cost_case_merk_patch,
        average_case_costs::{add_average_case_merk_propagate, EstimatedLayerInformation},
    },
    tree_type::TreeType,
};
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::key_info::KeyInfo,
    estimated_costs::{element_len_for_an_estimate, set_element_put_hash_calls},
    Element, Error, GroveDb,
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
        if !matches!(value, Element::Item(..)) {
            return Err(Error::InvalidParameter("patching can only be on Items"))
                .wrap_with_cost(cost);
        }
        // The bytes of the item, which have its flags: the size of the
        // record after the patch, and `change_in_bytes` fewer before it.
        // Measured once, and refused where a node cannot be sized by them.
        let size =
            cost_return_on_error_no_add!(cost, element_len_for_an_estimate(value, grove_version));
        if i64::from(change_in_bytes) > i64::from(size) {
            return Err(Error::InvalidParameter(
                "a patch cannot grow an item by more bytes than it has",
            ))
            .wrap_with_cost(cost);
        }
        add_cost_case_merk_patch(
            &mut cost,
            u32::from(key.max_length()),
            size,
            change_in_bytes,
            in_tree_type,
        );
        cost_return_on_error_no_add!(
            cost,
            set_element_put_hash_calls(&mut cost, key, value, size, grove_version)
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
