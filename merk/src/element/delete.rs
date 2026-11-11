//! Delete
//! Implements functions in Element for deleting

use grovedb_costs::{
    cost_return_on_error_default,
    storage_cost::{removal::StorageRemovedBytes, transition::ElementFlagsUpdate},
    CostResult, CostsExt, OperationCost,
};
use grovedb_element::Element;
use grovedb_storage::StorageContext;
use grovedb_version::{
    error::GroveVersionError,
    version::{FeatureVersion, GroveVersion},
};

use crate::{
    element::costs::ElementCostExtensions, BatchEntry, Error, Merk, MerkOptions, Op, TreeType,
};

/// The Merk op that deletes an element from a tree of `in_tree_type`.
///
/// `is_layered` is what the caller declares about the element: a tree or not.
/// `version` is the feature version of the calling `method`.
///
/// In every tree type but `NormalTree` the op reads the stored element and
/// refunds what its insertion was charged. In a `NormalTree`, version 0
/// refunds an element that is not declared a tree by its raw value bytes.
/// That is more than a tree was charged, since a tree does not pay for its
/// value hash, and less than a sum item was charged, since a sum item pays
/// for a fixed size. From version 1 a `NormalTree` refunds from the stored
/// element as well, so a delete gives back what the insertion took whatever
/// the caller declares.
fn delete_op(
    method: &str,
    version: FeatureVersion,
    in_tree_type: TreeType,
    is_layered: bool,
) -> Result<Op, Error> {
    let refunds_raw_value_bytes = match version {
        0 => true,
        1 => false,
        received => {
            return Err(GroveVersionError::UnknownVersionMismatch {
                method: method.to_string(),
                known_versions: vec![0, 1],
                received,
            }
            .into())
        }
    };
    let op = match (in_tree_type, is_layered) {
        (TreeType::NormalTree, true) => Op::DeleteLayered,
        (TreeType::NormalTree, false) if refunds_raw_value_bytes => Op::Delete,
        (TreeType::NormalTree, false) => Op::DeleteMaybeSpecialized,
        (TreeType::SumTree, true)
        | (TreeType::BigSumTree, true)
        | (TreeType::CountTree, true)
        | (TreeType::CountSumTree, true)
        | (TreeType::ProvableCountTree, true)
        | (TreeType::ProvableCountSumTree, true)
        | (TreeType::ProvableSumTree, true)
        | (TreeType::ProvableCountProvableSumTree, true)
        | (TreeType::CommitmentTree(_), true)
        | (TreeType::MmrTree, true)
        | (TreeType::BulkAppendTree(_), true)
        | (TreeType::DenseAppendOnlyFixedSizeTree(_), true)
        | (TreeType::ProvableSumIndexedTree, true)
        | (TreeType::ProvableCountProvableSumIndexedTree, true)
        | (TreeType::PrivateDocumentStore(_), true)
        | (TreeType::ProvableCountIndexedTree, true) => Op::DeleteLayeredMaybeSpecialized,
        (TreeType::SumTree, false)
        | (TreeType::BigSumTree, false)
        | (TreeType::CountTree, false)
        | (TreeType::CountSumTree, false)
        | (TreeType::ProvableCountTree, false)
        | (TreeType::ProvableCountSumTree, false)
        | (TreeType::ProvableSumTree, false)
        | (TreeType::ProvableCountProvableSumTree, false)
        | (TreeType::CommitmentTree(_), false)
        | (TreeType::MmrTree, false)
        | (TreeType::BulkAppendTree(_), false)
        | (TreeType::DenseAppendOnlyFixedSizeTree(_), false)
        | (TreeType::ProvableSumIndexedTree, false)
        | (TreeType::ProvableCountProvableSumIndexedTree, false)
        | (TreeType::PrivateDocumentStore(_), false)
        | (TreeType::ProvableCountIndexedTree, false) => Op::DeleteMaybeSpecialized,
    };
    Ok(op)
}

/// Extension trait for deleting elements from Merk storage.
pub trait ElementDeleteFromStorageExtensions {
    /// Delete an element from Merk under a key
    fn delete<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        in_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Delete an element from Merk under a key
    fn delete_with_sectioned_removal_bytes<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        in_tree_type: TreeType,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Delete an element from Merk under a key to batch operations
    fn delete_into_batch_operations<K: AsRef<[u8]>>(
        key: K,
        is_layered: bool,
        in_tree_type: TreeType,
        batch_operations: &mut Vec<BatchEntry<K>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;
}

impl ElementDeleteFromStorageExtensions for Element {
    /// Delete an element from Merk under a key
    fn delete<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        in_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let op = cost_return_on_error_default!(delete_op(
            "delete",
            grove_version.grovedb_versions.element.delete,
            in_tree_type,
            is_layered,
        ));
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
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Delete an element from Merk under a key
    fn delete_with_sectioned_removal_bytes<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        merk: &mut Merk<S>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        in_tree_type: TreeType,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let op = cost_return_on_error_default!(delete_op(
            "delete_with_sectioned_removal_bytes",
            grove_version
                .grovedb_versions
                .element
                .delete_with_sectioned_removal_bytes,
            in_tree_type,
            is_layered,
        ));
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
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            &|_, _| Ok(None),
            &mut |_costs, _old_value, _value| Ok((ElementFlagsUpdate::Unchanged, None)),
            sectioned_removal,
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Delete an element from Merk under a key to batch operations
    fn delete_into_batch_operations<K: AsRef<[u8]>>(
        key: K,
        is_layered: bool,
        in_tree_type: TreeType,
        batch_operations: &mut Vec<BatchEntry<K>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let op = cost_return_on_error_default!(delete_op(
            "delete_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .delete_into_batch_operations,
            in_tree_type,
            is_layered,
        ));
        let entry = (key, op);
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }
}
