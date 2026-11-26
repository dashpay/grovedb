//! `add_average_case_get_merk_at_path` **v0** (released behaviour,
//! `GROVE_V1`..`GROVE_V5`).
//!
//! The open reads the host. Differs from [`super::v1`] only in the seek and
//! the loaded bytes of that read. See the module docs in
//! [`super`][`mod@super`].

use grovedb_costs::OperationCost;
use grovedb_merk::{
    cost_model::storage_cost_model, tree::TreeNode, tree_type::TreeType, HASH_LENGTH,
};
use grovedb_storage::worst_case_costs::{get_storage_context_cost, WorstKeyLength};
use grovedb_version::version::GroveVersion;

use crate::{batch::KeyInfoPath, Error, GroveDb};

impl GroveDb {
    /// `add_average_case_get_merk_at_path` v0: see the module documentation.
    pub(super) fn add_average_case_get_merk_at_path_v0(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        merk_should_be_empty: bool,
        in_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        cost.seek_count += 1;
        // If the merk is not empty we load the tree
        if !merk_should_be_empty {
            cost.seek_count += 1;
        }
        match path.last() {
            None => {}
            Some(key) => {
                cost.storage_loaded_bytes += TreeNode::average_case_encoded_tree_size(
                    key.max_length() as u32,
                    HASH_LENGTH as u32,
                    in_tree_type.inner_node_type(),
                ) as u64;
            }
        }
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());

        Ok(())
    }
}
