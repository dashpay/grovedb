//! Worst case costs
//! Implements worst case cost functions in GroveDb

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    cost_model::storage_cost_model,
    estimated_costs::{
        add_cost_case_merk_insert_layered,
        worst_case_costs::{
            add_worst_case_get_merk_node, add_worst_case_merk_delete,
            add_worst_case_merk_delete_layered, add_worst_case_merk_propagate_versioned,
            add_worst_case_merk_replace_layered, WorstCaseLayerInformation,
            MERK_BIGGEST_VALUE_SIZE,
        },
    },
    tree::TreeNode,
    tree_type::{CostSize, TreeType},
};
use grovedb_storage::{
    worst_case_costs::{get_storage_context_cost, WorstKeyLength},
    Storage,
};
use grovedb_version::{check_grovedb_v0, check_grovedb_v0_with_cost, version::GroveVersion};
use integer_encoding::VarInt;

use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    ElementFlags, Error, GroveDb,
};

mod finalize;
mod get_merk_at_path;
mod merk_insert_element;
mod merk_patch_element;
mod merk_replace_element;

pub const WORST_CASE_FLAGS_LEN: u32 = 16386; // 2 bytes to represent this number for varint

impl GroveDb {
    /// The estimate of [`Self::add_worst_case_get_merk_at_path_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_worst_case_get_merk_at_path<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_worst_case_get_merk_at_path_engine_free(cost, path, tree_type, grove_version)
    }

    /// Add worst case for insertion into merk
    pub(crate) fn worst_case_merk_replace_tree(
        key: &KeyInfo,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        worst_case_layer_information: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "worst_case_merk_replace_tree",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .worst_case_merk_replace_tree
        );

        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let tree_cost = tree_type.cost_size();
        let layer_extra_size = tree_cost + WORST_CASE_FLAGS_LEN;
        add_worst_case_merk_replace_layered(
            &mut cost,
            key_len,
            layer_extra_size,
            in_parent_tree_type.inner_node_type(),
        );
        if propagate {
            add_worst_case_merk_propagate_versioned(
                &mut cost,
                worst_case_layer_information,
                grove_version,
            )
            .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    pub fn worst_case_merk_insert_tree(
        key: &KeyInfo,
        flags: &Option<ElementFlags>,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        wrapper_overhead: u32,
        propagate_if_input: Option<&WorstCaseLayerInformation>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "worst_case_merk_insert_tree",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .worst_case_merk_insert_tree
        );

        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let flags_len = flags.as_ref().map_or(0, |flags| {
            let flags_len = flags.len() as u32;
            flags_len + flags_len.required_space() as u32
        });
        let tree_cost = tree_type.cost_size();
        // `wrapper_overhead` accounts for the wrapper discriminant byte
        // (see corresponding comment in `average_case_merk_insert_tree`).
        let value_len = tree_cost + flags_len + wrapper_overhead;
        add_cost_case_merk_insert_layered(&mut cost, key_len, value_len, in_parent_tree_type);
        if let Some(input) = propagate_if_input {
            add_worst_case_merk_propagate_versioned(&mut cost, input, grove_version)
                .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case for insertion into merk
    pub fn worst_case_merk_delete_tree(
        key: &KeyInfo,
        tree_type: TreeType,
        worst_case_layer_information: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "worst_case_merk_delete_tree",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .worst_case_merk_delete_tree
        );

        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        let tree_cost = tree_type.cost_size();
        let layer_extra_size = tree_cost + WORST_CASE_FLAGS_LEN;
        add_worst_case_merk_delete_layered(&mut cost, key_len, layer_extra_size);
        if propagate {
            add_worst_case_merk_propagate_versioned(
                &mut cost,
                worst_case_layer_information,
                grove_version,
            )
            .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// Add worst case cost for deletion into merk
    pub fn worst_case_merk_delete_element(
        key: &KeyInfo,
        worst_case_layer_information: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "worst_case_merk_delete_element",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .worst_case_merk_delete_element
        );

        let mut cost = OperationCost::default();
        let key_len = key.max_length() as u32;
        add_worst_case_merk_delete(&mut cost, key_len, MERK_BIGGEST_VALUE_SIZE);
        if propagate {
            add_worst_case_merk_propagate_versioned(
                &mut cost,
                worst_case_layer_information,
                grove_version,
            )
            .map_err(Error::MerkError)
        } else {
            Ok(())
        }
        .wrap_with_cost(cost)
    }

    /// The estimate of [`Self::add_worst_case_has_raw_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_worst_case_has_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_worst_case_has_raw_cost_engine_free(
            cost,
            path,
            key,
            max_element_size,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// Add worst case cost for "has raw" into merk
    pub fn add_worst_case_has_raw_cost_engine_free(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_worst_case_has_raw_cost",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .add_worst_case_has_raw_cost
        );

        let value_size = TreeNode::worst_case_encoded_tree_size(
            key.max_length() as u32,
            max_element_size,
            in_parent_tree_type.inner_node_type(),
        );
        cost.seek_count += 1;
        cost.storage_loaded_bytes += value_size as u64;
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        Ok(())
    }

    /// The estimate of [`Self::add_worst_case_get_raw_tree_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_worst_case_get_raw_tree_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_worst_case_get_raw_tree_cost_engine_free(
            cost,
            path,
            key,
            tree_type,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// Add worst case cost for get raw tree into merk
    pub fn add_worst_case_get_raw_tree_cost_engine_free(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        tree_type: TreeType,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_worst_case_get_raw_tree_cost",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .add_worst_case_get_raw_tree_cost
        );

        cost.seek_count += 1;
        let tree_cost_size = tree_type.cost_size();
        add_worst_case_get_merk_node(
            cost,
            key.max_length() as u32,
            tree_cost_size,
            in_parent_tree_type.inner_node_type(),
        )
        .map_err(Error::MerkError)
    }

    /// The estimate of [`Self::add_worst_case_get_raw_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_worst_case_get_raw_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_worst_case_get_raw_cost_engine_free(
            cost,
            path,
            key,
            max_element_size,
            in_parent_tree_type,
            grove_version,
        )
    }

    /// Add worst case cost for get raw into merk
    pub fn add_worst_case_get_raw_cost_engine_free(
        cost: &mut OperationCost,
        _path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_worst_case_get_raw_cost",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .add_worst_case_get_raw_cost
        );

        cost.seek_count += 1;
        add_worst_case_get_merk_node(
            cost,
            key.max_length() as u32,
            max_element_size,
            in_parent_tree_type.inner_node_type(),
        )
        .map_err(Error::MerkError)
    }

    /// The estimate of [`Self::add_worst_case_get_cost_engine_free`], for a
    /// caller that names a storage engine. `S` is not used: the estimate
    /// is the same on every engine.
    pub fn add_worst_case_get_cost<'db, S: Storage<'db>>(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_type: TreeType,
        max_references_sizes: Vec<u32>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        Self::add_worst_case_get_cost_engine_free(
            cost,
            path,
            key,
            max_element_size,
            in_parent_tree_type,
            max_references_sizes,
            grove_version,
        )
    }

    /// Add worst case cost for get into merk
    pub fn add_worst_case_get_cost_engine_free(
        cost: &mut OperationCost,
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_type: TreeType,
        max_references_sizes: Vec<u32>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        check_grovedb_v0!(
            "add_worst_case_get_cost",
            grove_version
                .grovedb_versions
                .operations
                .worst_case
                .add_worst_case_get_cost
        );

        // todo: verify
        let value_size: u32 = TreeNode::worst_case_encoded_tree_size(
            key.max_length() as u32,
            max_element_size,
            in_parent_tree_type.inner_node_type(),
        );
        cost.seek_count += 1 + max_references_sizes.len() as u32;
        cost.storage_loaded_bytes +=
            value_size as u64 + max_references_sizes.iter().map(|x| *x as u64).sum::<u64>();
        *cost += get_storage_context_cost(storage_cost_model(grove_version), path.as_vec());
        Ok(())
    }
}

#[cfg(all(test, feature = "full", grovedb_test_engine))]
mod test {
    use std::option::Option::None;

    use grovedb_costs::OperationCost;
    use grovedb_merk::{
        estimated_costs::worst_case_costs::add_worst_case_get_merk_node, merk::NodeType,
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
    fn test_get_merk_node_worst_case() {
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

        // To simulate worst case, we need to pick a node that:
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

        // By tweaking the max element size, we can adapt the worst case function to
        // this scenario. make_batch_seq creates values that are 60 bytes in size
        // (this will be the max_element_size)
        let mut cost = OperationCost::default();
        let key = KnownKey(8_u64.to_be_bytes().to_vec());
        add_worst_case_get_merk_node(&mut cost, key.max_length() as u32, 60, NodeType::NormalNode)
            .expect("no issue with version");
        assert_eq!(cost, node_result.cost);
    }

    #[test]
    fn test_has_raw_worst_case() {
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
        // right links this will serve as our worst case candidate.
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
        let mut worst_case_has_raw_cost = OperationCost::default();
        GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
            &mut worst_case_has_raw_cost,
            &path,
            &key,
            elem.serialized_size(grove_version).expect("expected size") as u32,
            TreeType::NormalTree,
            GroveVersion::latest(),
        )
        .expect("expected to add cost");

        let actual_cost = db.has_raw([TEST_LEAF].as_ref(), &[2], None, GroveVersion::latest());

        assert_eq!(worst_case_has_raw_cost, actual_cost.cost);
    }
}
