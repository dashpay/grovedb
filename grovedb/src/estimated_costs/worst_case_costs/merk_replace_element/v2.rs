//! `worst_case_merk_replace_element` — **v2** (`GROVE_V6`).
//!
//! [`super::v1`], with two things it does not have.
//!
//! The storage of a replacement that may be of anything by anything. v1
//! takes a tree and a sum item to be put where one of the same size was,
//! and has no added bytes for them: a sum item that gains flags and a tree
//! put where an item was add bytes it does not have. And it takes any other
//! element to be put where nothing of its value was, and has the key's
//! bytes alone as replaced. This version has, for every element, all of the
//! node's bytes as replaced, which is the replacement of an element as long
//! or longer, and all of the value's as added, which is the replacement of
//! one with no value: each field is a bound, and the two are of different
//! replacements.
//!
//! And the hash calls of the put counted from the bytes of the element
//! (`element_put_hash_calls`): the value is hashed with its length before
//! it, every tree and every reference combines its hash with another, and
//! an indexed tree with the roots of its secondaries. The version before
//! counts the blocks of a length that is the element's cost.
//!
//! An element too long for a node to be sized by its bytes is refused
//! (`sized_for_a_node`).
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
    tree::kv::KV,
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
    /// `worst_case_merk_replace_element` v2 — see the module documentation.
    pub(super) fn worst_case_merk_replace_element_v2(
        key: &KeyInfo,
        value: &Element,
        in_parent_tree_type: TreeType,
        propagate_for_level: Option<&WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        // Before the version before sizes the element, in 32 bits unchecked.
        let len =
            cost_return_on_error_no_add!(cost, element_len_for_an_estimate(value, grove_version));
        cost_return_on_error!(
            &mut cost,
            Self::worst_case_merk_replace_element_v1(
                key,
                value,
                in_parent_tree_type,
                None,
                grove_version
            )
        );
        // v1 has the bytes of the node, its key's and its value's, as
        // replaced or as added, by the kind of the element. Here every one
        // is replaced, and every one of the value is added.
        let node = cost.storage_cost.replaced_bytes + cost.storage_cost.added_bytes;
        let key_bytes = KV::node_key_byte_cost_size(u32::from(key.max_length()));
        cost.storage_cost.replaced_bytes = node;
        cost.storage_cost.added_bytes = node.saturating_sub(key_bytes);
        cost_return_on_error_no_add!(
            cost,
            set_element_put_hash_calls(&mut cost, key, value, len, grove_version)
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
