//! Delete
//! Implements functions in Element for deleting

#[cfg(feature = "minimal")]
use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt};
#[cfg(feature = "minimal")]
use grovedb_merk::tree_type::TreeType;
#[cfg(feature = "minimal")]
use grovedb_merk::{BatchEntry, Error as MerkError, Merk, MerkOptions, Op};
#[cfg(feature = "minimal")]
use grovedb_storage::StorageContext;
#[cfg(feature = "minimal")]
use grovedb_version::check_grovedb_v0_with_cost;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::{Element, Error};

impl Element {
    #[cfg(feature = "minimal")]
    /// Delete an element from Merk under a key
    pub fn delete<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        in_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!("delete", grove_version.grovedb_versions.element.delete);
        let op = match (in_tree_type, is_layered) {
            (TreeType::NormalTree, true) => Op::DeleteLayered,
            (TreeType::NormalTree, false) => Op::Delete,
            (TreeType::SumTree, true)
            | (TreeType::BigSumTree, true)
            | (TreeType::CountTree, true)
            | (TreeType::CountSumTree, true) => Op::DeleteLayeredMaybeSpecialized,
            (TreeType::SumTree, false)
            | (TreeType::BigSumTree, false)
            | (TreeType::CountTree, false)
            | (TreeType::CountSumTree, false) => Op::DeleteMaybeSpecialized,
        };
        let batch = [(key, op)];
        // todo not sure we get it again, we need to see if this is necessary
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch,
            &[],
            merk_options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "minimal")]
    /// Delete an element from Merk under a key
    pub fn delete_with_sectioned_removal_bytes<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        in_tree_type: TreeType,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "delete_with_sectioned_removal_bytes",
            grove_version
                .grovedb_versions
                .element
                .delete_with_sectioned_removal_bytes
        );
        let op = match (in_tree_type, is_layered) {
            (TreeType::NormalTree, true) => Op::DeleteLayered,
            (TreeType::NormalTree, false) => Op::Delete,
            (TreeType::SumTree, true)
            | (TreeType::BigSumTree, true)
            | (TreeType::CountTree, true)
            | (TreeType::CountSumTree, true) => Op::DeleteLayeredMaybeSpecialized,
            (TreeType::SumTree, false)
            | (TreeType::BigSumTree, false)
            | (TreeType::CountTree, false)
            | (TreeType::CountSumTree, false) => Op::DeleteMaybeSpecialized,
        };
        let batch = [(key, op)];
        // todo not sure we get it again, we need to see if this is necessary
        let tree_type = merk.tree_type;
        merk.apply_with_costs_just_in_time_value_update::<_, Vec<u8>>(
            &batch,
            &[],
            merk_options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            &|_, _| Ok(None),
            &mut |_costs, _old_value, _value| Ok((false, None)),
            sectioned_removal,
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "minimal")]
    /// Delete an element from Merk under a key to batch operations
    pub fn delete_into_batch_operations<K: AsRef<[u8]>>(
        key: K,
        is_layered: bool,
        in_tree_type: TreeType,
        batch_operations: &mut Vec<BatchEntry<K>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "delete_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .delete_into_batch_operations
        );
        let op = match (in_tree_type, is_layered) {
            (TreeType::NormalTree, true) => Op::DeleteLayered,
            (TreeType::NormalTree, false) => Op::Delete,
            (TreeType::SumTree, true)
            | (TreeType::BigSumTree, true)
            | (TreeType::CountTree, true)
            | (TreeType::CountSumTree, true) => Op::DeleteLayeredMaybeSpecialized,
            (TreeType::SumTree, false)
            | (TreeType::BigSumTree, false)
            | (TreeType::CountTree, false)
            | (TreeType::CountSumTree, false) => Op::DeleteMaybeSpecialized,
        };
        let entry = (key, op);
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }
}
