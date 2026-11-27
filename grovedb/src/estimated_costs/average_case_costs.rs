//! Average case costs
//! Implements average case cost functions in GroveDb

use grovedb_costs::{cost_return_on_error_no_add, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    cost_model::storage_cost_model,
    estimated_costs::{
        add_cost_case_merk_insert_layered,
        average_case_costs::{
            add_average_case_get_merk_node, add_average_case_merk_delete,
            add_average_case_merk_propagate, add_average_case_merk_replace_layered,
            EstimatedLayerInformation, EstimatedLayerSizes,
        },
    },
    tree::TreeNode,
    tree_type::{CostSize, TreeType},
};
use grovedb_storage::{
    worst_case_costs::{get_storage_context_cost, WorstKeyLength},
    Storage,
};
use grovedb_version::{
    check_grovedb_v0, check_grovedb_v0_with_cost, error::GroveVersionError, version::GroveVersion,
};
use integer_encoding::VarInt;

use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    ElementFlags, Error, GroveDb,
};

mod finalize;
mod get_merk_at_path;
mod indexed_secondary_mirror;
mod merk_delete_tree;
mod merk_insert_element;
mod merk_patch_element;
mod merk_replace_element;

/// The keys of a layer at their longest, whatever the layer holds. A kind
/// that a layer of several kinds gives no weight holds no element there.
/// The estimate of a finalize sizes a root key by it, and on grove version 6
/// the estimate of the rows of an indexed tree the keys of its primary.
pub(crate) fn layer_key_size(sizes: &EstimatedLayerSizes) -> u32 {
    match sizes {
        EstimatedLayerSizes::AllSubtrees(key, ..)
        | EstimatedLayerSizes::AllItems(key, ..)
        | EstimatedLayerSizes::AllReference(key, ..)
        | EstimatedLayerSizes::AllItemsWithSumItem(key, ..)
        | EstimatedLayerSizes::AllReferencesWithSumItem(key, ..) => u32::from(*key),
        EstimatedLayerSizes::Mix {
            subtrees_size,
            items_size,
            references_size,
            items_with_sum_item_size,
            references_with_sum_item_size,
        } => [
            subtrees_size.map(|(key, .., weight)| (key, weight)),
            items_size.map(|(key, .., weight)| (key, weight)),
            references_size.map(|(key, .., weight)| (key, weight)),
            items_with_sum_item_size.map(|(key, .., weight)| (key, weight)),
            references_with_sum_item_size.map(|(key, .., weight)| (key, weight)),
        ]
        .into_iter()
        .flatten()
        .filter(|(_, weight)| *weight > 0)
        .map(|(key, _)| key)
        .max()
        .map_or(0, u32::from),
    }
}

impl GroveDb {
    /// The estimate of [`Self::add_average_case_get_merk_at_path_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_average_case_get_merk_at_path<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        merk_should_be_empty: bool,
        in_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_average_case_get_merk_at_path_engine_free(
            cost,
            path,
            merk_should_be_empty,
            in_tree_type,
            grove_version,
        )
    }

    /// Add average case for insertion into merk
    pub(crate) fn average_case_merk_replace_tree(
        key: &KeyInfo,
        estimated_layer_information: &EstimatedLayerInformation,
        replacing_tree_type: TreeType,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_merk_replace_tree
        {
            0 => Self::average_case_merk_replace_tree_v0(
                key,
                estimated_layer_information,
                replacing_tree_type,
                propagate,
                grove_version,
            ),
            1 => Self::average_case_merk_replace_tree_v1(
                key,
                estimated_layer_information,
                replacing_tree_type,
                propagate,
                grove_version,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_merk_replace_tree".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }

    /// Add average case for insertion into merk
    fn average_case_merk_replace_tree_v0(
        key: &KeyInfo,
        estimated_layer_information: &EstimatedLayerInformation,
        _replacing_tree_type: TreeType,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        // In v0 we used the estimated layer information tree type (which is the parent)
        // in order to figure out the cost
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_size = cost_return_on_error_no_add!(
            cost,
            estimated_layer_information
                .estimated_layer_sizes
                .layered_flags_size()
                .map_err(Error::MerkError)
        )
        .map(|f| f + f.required_space() as u32)
        .unwrap_or_default();
        let tree_cost_size = estimated_layer_information.tree_type.cost_size(); // this was wrong
        let layer_extra_size = tree_cost_size + flags_size;
        add_average_case_merk_replace_layered(
            &mut cost,
            key_len,
            layer_extra_size,
            estimated_layer_information.tree_type.inner_node_type(),
        );
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for insertion into merk
    fn average_case_merk_replace_tree_v1(
        key: &KeyInfo,
        estimated_layer_information: &EstimatedLayerInformation,
        replacing_tree_type: TreeType,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_size = cost_return_on_error_no_add!(
            cost,
            estimated_layer_information
                .estimated_layer_sizes
                .layered_flags_size()
                .map_err(Error::MerkError)
        )
        .map(|f| f + f.required_space() as u32)
        .unwrap_or_default();
        let tree_cost_size = replacing_tree_type.cost_size();
        let layer_extra_size = tree_cost_size + flags_size;
        add_average_case_merk_replace_layered(
            &mut cost,
            key_len,
            layer_extra_size,
            estimated_layer_information.tree_type.inner_node_type(),
        );
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for insertion into merk
    pub fn average_case_merk_insert_tree(
        key: &KeyInfo,
        flags: &Option<ElementFlags>,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        wrapper_overhead: u32,
        propagate_if_input: Option<&EstimatedLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "average_case_merk_insert_tree",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .average_case_merk_insert_tree
        );

        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let tree_cost_size = tree_type.cost_size();
        // `wrapper_overhead` accounts for the wrapper discriminant byte
        // that `Element::NonCounted` / `Element::NotSummed` /
        // `Element::NotCountedOrSummed` add to the serialized payload.
        // Callers pass 1 when the on-disk shape will be wrapped, else 0.
        let value_len = tree_cost_size + flags_len + wrapper_overhead;
        add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, in_parent_tree_type);
        if let Some(input) = propagate_if_input {
            add_average_case_merk_propagate(&mut cost, input, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add average case for deletion into Merk
    /// Largest average key size described by a layer, used to derive a
    /// secondary index layer's key sizes from its primary's.
    pub(crate) fn average_case_layer_key_size(sizes: &EstimatedLayerSizes) -> u32 {
        match sizes {
            EstimatedLayerSizes::AllSubtrees(k, ..)
            | EstimatedLayerSizes::AllItems(k, ..)
            | EstimatedLayerSizes::AllReference(k, ..)
            | EstimatedLayerSizes::AllItemsWithSumItem(k, ..)
            | EstimatedLayerSizes::AllReferencesWithSumItem(k, ..) => *k as u32,
            EstimatedLayerSizes::Mix {
                subtrees_size,
                items_size,
                references_size,
                ..
            } => [
                subtrees_size.map(|(k, ..)| k),
                items_size.map(|(k, ..)| k),
                references_size.map(|(k, ..)| k),
            ]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(0) as u32,
        }
    }

    /// Add average case cost for deleting an element from a merk.
    pub fn average_case_merk_delete_element(
        key: &KeyInfo,
        estimated_layer_information: &EstimatedLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "average_case_merk_delete_element",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .average_case_merk_delete_element
        );

        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let value_size = cost_return_on_error_no_add!(
            cost,
            estimated_layer_information
                .estimated_layer_sizes
                .value_with_feature_and_flags_size(grove_version)
                .map_err(Error::MerkError)
        );
        add_average_case_merk_delete(&mut cost, key_len, value_size);
        if propagate {
            add_average_case_merk_propagate(&mut cost, estimated_layer_information, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// The estimate of [`Self::add_average_case_has_raw_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_average_case_has_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_average_case_has_raw_cost_engine_free(
            cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// Adds the average case of checking to see if a raw value exists
    pub fn add_average_case_has_raw_cost_engine_free(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_average_case_has_raw_cost",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .add_average_case_has_raw_cost
        );

        let value_size = TreeNode::average_case_encoded_tree_size(
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_type.inner_node_type(),
        );
        cost.seek_count += 1;
        cost.storage_loaded_bytes += value_size as u64;
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        Ok(())
    }

    /// The estimate of [`Self::add_average_case_has_raw_tree_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_average_case_has_raw_tree_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_average_case_has_raw_tree_cost_engine_free(
            cost,
            path,
            key,
            estimated_flags_size,
            tree_type,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// Adds the average case of checking to see if a tree exists
    pub fn add_average_case_has_raw_tree_cost_engine_free(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_average_case_has_raw_tree_cost",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .add_average_case_has_raw_tree_cost
        );

        let estimated_element_size = tree_type.cost_size() + estimated_flags_size;
        Self::add_average_case_has_raw_cost_engine_free(
            cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// The estimate of [`Self::add_average_case_get_raw_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_average_case_get_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_average_case_get_raw_cost_engine_free(
            cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// Add average case to get raw cost into merk
    pub fn add_average_case_get_raw_cost_engine_free(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_average_case_get_raw_cost",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .add_average_case_get_raw_cost
        );

        cost.seek_count += 1;
        add_average_case_get_merk_node(
            cost,
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_type.inner_node_type(),
        )
        .map_err(Error::MerkError)
    }

    /// The estimate of [`Self::add_average_case_get_raw_tree_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_average_case_get_raw_tree_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_average_case_get_raw_tree_cost_engine_free(
            cost,
            path,
            key,
            estimated_flags_size,
            tree_type,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// adds the average cost of getting a tree
    pub fn add_average_case_get_raw_tree_cost_engine_free(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_average_case_get_raw_tree_cost",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .add_average_case_get_raw_tree_cost
        );

        let estimated_element_size = tree_type.cost_size() + estimated_flags_size;
        cost.seek_count += 1;
        add_average_case_get_merk_node(
            cost,
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_type.inner_node_type(),
        )
        .map_err(Error::MerkError)
    }

    /// The estimate of [`Self::add_average_case_get_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_average_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        estimated_element_size: u32,
        estimated_references_sizes: Vec<u32>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_average_case_get_cost_engine_free(
            cost,
            path,
            key,
            in_parent_tree_type,
            estimated_element_size,
            estimated_references_sizes,
            grove_version,
        )
    }

    /// adds the average cost of getting an element knowing there can be
    /// intermediate references
    pub fn add_average_case_get_cost_engine_free(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        estimated_element_size: u32,
        estimated_references_sizes: Vec<u32>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_average_case_get_cost",
            grove_version
                .grovedb_versions
                .operations
                .average_case
                .add_average_case_get_cost
        );

        // todo: verify
        let value_size: u32 = TreeNode::average_case_encoded_tree_size(
            key.max_length() as u32,
            estimated_element_size,
            in_parent_tree_type.inner_node_type(),
        );
        cost.seek_count += 1 + estimated_references_sizes.len() as u32;
        cost.storage_loaded_bytes += value_size as u64
            + estimated_references_sizes
                .iter()
                .map(|x| *x as u64)
                .sum::<u64>();
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        Ok(())
    }
}

#[cfg(all(test, feature = "full", grovedb_test_engine))]
mod test {
    use std::option::Option::None;

    use grovedb_costs::OperationCost;
    use grovedb_merk::{
        estimated_costs::average_case_costs::add_average_case_get_merk_node,
        test_utils::make_batch_seq, tree::kv::ValueDefinedCostType, tree_type::TreeType, Merk,
    };
    use grovedb_storage::{rocksdb_storage::RocksDbStorage, worst_case_costs::WorstKeyLength};
    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
        tests::{common::EMPTY_PATH, make_empty_grovedb, SuiteBatch, TEST_LEAF},
        Element, Engine, GroveDb,
    };

    #[test]
    fn test_get_merk_node_average_case() {
        let grove_version = GroveVersion::latest();
        // Open a merk and insert 10 elements.
        let db = make_empty_grovedb();
        let storage = db.engine();
        let batch = SuiteBatch::default();
        let transaction = storage.start_transaction();

        let mut merk = Merk::open_base(
            storage
                .subtree(EMPTY_PATH, Some(&batch), &transaction)
                .unwrap(),
            TreeType::NormalTree,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .expect("cannot open merk");
        let merk_batch = make_batch_seq(1..10);
        merk.apply::<_, Vec<_>>(merk_batch.as_slice(), &[], None, grove_version)
            .unwrap()
            .unwrap();

        // this consumes the batch so storage contexts and merks will be dropped
        storage.apply_batch(batch, &transaction).unwrap().unwrap();

        // Reopen merk: this time, only root node is loaded to memory
        let merk = Merk::open_base(
            storage.subtree(EMPTY_PATH, None, &transaction).unwrap(),
            TreeType::NormalTree,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .expect("cannot open merk");

        // To simulate average case, we need to pick a node that:
        // 1. Is not in memory
        // 2. Left link exists
        // 3. Right link exists
        // Based on merk's avl rotation algorithm node is key 8 satisfies this
        let node_result = merk.get(
            &8_u64.to_be_bytes(),
            true,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        );

        // By tweaking the max element size, we can adapt the average case function to
        // this scenario. make_batch_seq creates values that are 60 bytes in size
        // (this will be the max_element_size)
        let mut cost = OperationCost::default();
        let key = KnownKey(8_u64.to_be_bytes().to_vec());
        add_average_case_get_merk_node(
            &mut cost,
            key.max_length() as u32,
            60,
            TreeType::NormalTree.inner_node_type(),
        )
        .expect("expected to add cost");
        assert_eq!(cost, node_result.cost);
    }

    #[test]
    fn test_has_raw_average_case() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // insert empty tree to start
        db.insert(
            EMPTY_PATH,
            TEST_LEAF,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        // In this tree, we insert 3 items with keys [1, 2, 3]
        // after tree rotation, 2 will be at the top hence would have both left and
        // right links this will serve as our average case candidate.
        let elem = Element::new_item(b"value".to_vec());
        db.insert(
            [TEST_LEAF].as_ref(),
            &[1],
            elem.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            &[2],
            elem.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected insert");
        db.insert(
            [TEST_LEAF].as_ref(),
            &[3],
            elem.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected insert");

        let path = KeyInfoPath::from_vec(vec![KnownKey(TEST_LEAF.to_vec())]);
        let key = KnownKey(vec![1]);
        let mut average_case_has_raw_cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
            &mut average_case_has_raw_cost,
            &path,
            &key,
            elem.serialized_size(grove_version).expect("expected size") as u32,
            TreeType::NormalTree,
            GroveVersion::latest(),
        )
        .expect("expected to add cost");

        let actual_cost = db.has_raw([TEST_LEAF].as_ref(), &[2], None, GroveVersion::latest());

        assert_eq!(average_case_has_raw_cost, actual_cost.cost);
    }
}
