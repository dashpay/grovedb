//! `worst_case_merk_insert_element` — **v3** (`GROVE_V6`).
//!
//! [`super::v2`], with the hash calls of the put counted from the bytes of the
//! element (`element_put_hash_calls`): the value is hashed with its length
//! before it, every tree and every reference combines its hash with
//! another, and an indexed tree with the roots of its secondaries. The
//! version before counts the blocks of a length that is the element's
//! cost.
//!
//! On grove versions 1 to 5 a batch's estimate has every level up to the
//! root of the grove. A write that stops at a stop point has its own level
//! alone (`docs/lignindb/cost-model.md`, section 16.5).

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    estimated_costs::worst_case_costs::{
        add_worst_case_merk_propagate_versioned, WorstCaseLayerInformation,
    },
    tree_type::TreeType,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::key_info::KeyInfo, estimated_costs::set_element_put_hash_calls, Element, Error, GroveDb,
};

impl GroveDb {
    /// `worst_case_merk_insert_element` v3 — see the module documentation.
    pub(super) fn worst_case_merk_insert_element_v3(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_type: TreeType,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        cost_return_on_error!(
            &mut cost,
            Self::worst_case_merk_insert_element_v2(
                key,
                value,
                in_parent_tree_type,
                None,
                grove_version
            )
        );
        cost_return_on_error_no_add!(
            cost,
            set_element_put_hash_calls(&mut cost, key, value, grove_version)
        );
        if let Some(level) = propagate_for_level {
            add_worst_case_merk_propagate_versioned(&mut cost, level, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
