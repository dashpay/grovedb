//! `worst_case_merk_insert_element` — **v2** (`GROVE_V5`+).
//!
//! [`super::v1`], except that creating a `PrivateDocumentStore` is priced as
//! the apply prices it: as the layered tree the apply writes (v1 prices it as
//! an item at its serialized size), plus the existence check every store
//! creation makes (a store is write-once) and the two hashes deriving its
//! empty root. Every other element is estimated by v1.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        add_cost_case_merk_insert_layered,
        worst_case_costs::{
            add_worst_case_merk_has_value, add_worst_case_merk_propagate,
            WorstCaseLayerInformation, MERK_BIGGEST_VALUE_SIZE,
        },
    },
    tree_type::{CostSize, TreeType},
};
use grovedb_private_document_store::EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS;
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{batch::key_info::KeyInfo, Element, Error, GroveDb};

impl GroveDb {
    /// `worst_case_merk_insert_element` v2 — see the module documentation.
    pub(super) fn worst_case_merk_insert_element_v2(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_type: TreeType,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let Element::PrivateDocumentStore(_, _, chunk_power, flags) = value.underlying() else {
            return Self::worst_case_merk_insert_element_v1(
                key,
                value,
                in_parent_tree_type,
                propagate_for_level,
                grove_version,
            );
        };
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        // The layered insert, as v1 prices a tree's.
        let wrapper_overhead = if value.is_wrapped() { 1u32 } else { 0 };
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let tree_cost_size = TreeType::PrivateDocumentStore(*chunk_power).cost_size();
        let value_len = tree_cost_size + flags_len + wrapper_overhead;
        add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, in_parent_tree_type);
        // The existence check, priced as `InsertIfNotExists` prices one, and
        // the empty root's derivation.
        add_worst_case_merk_has_value(&mut cost, key_len, MERK_BIGGEST_VALUE_SIZE);
        cost.hash_node_calls = cost
            .hash_node_calls
            .saturating_add(EMPTY_PRIVATE_DOCUMENT_STORE_STATE_ROOT_HASH_CALLS);
        if let Some(level) = propagate_for_level {
            add_worst_case_merk_propagate(&mut cost, level).map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }
}
