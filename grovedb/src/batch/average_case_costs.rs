use std::collections::{BTreeMap, HashMap};
use std::fmt;
use itertools::Itertools;
use costs::{CostResult, CostsExt, OperationCost};
use merk::CryptoHash;
use merk::estimated_costs::average_case_costs::{add_average_case_merk_propagate, MerkAverageCaseInput};
use storage::rocksdb_storage::RocksDbStorage;
use crate::batch::{BatchApplyOptions, GroveDbOp, KeyInfoPath, Op, TreeCache};
use crate::{Error, GroveDb, MAX_ELEMENTS_NUMBER};
use crate::batch::key_info::KeyInfo;
use crate::batch::mode::BatchRunMode;
use crate::batch::mode::BatchRunMode::AverageCaseMode;

/// Cache for subtree paths for average case scenario costs.
#[derive(Default)]
pub(super) struct AverageCaseTreeCacheKnownPaths {
    paths: HashMap<KeyInfoPath, MerkAverageCaseInput>,
}


impl fmt::Debug for AverageCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}


impl<G, SR> TreeCache<G, SR> for AverageCaseTreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        if !self.paths.contains_key(&inserted_path) {
            return Err(Error::PathNotFoundInCacheForEstimatedCosts(format!(
                "inserting into average case costs path: {}",
                inserted_path.0.iter().map(|k| hex::encode(k.get_key())).join("/")
            ))).wrap_with_cost(OperationCost::default())
        }
        let mut average_case_cost = OperationCost::default();
        GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(&mut average_case_cost, &op.path);
        Ok(()).wrap_with_cost(average_case_cost)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        AverageCaseMode(self.paths.clone())
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<(CryptoHash, Option<Vec<u8>>), Error> {
        let mut cost = OperationCost::default();

        if let Some(input) = self.paths.get(path) {
            // Then we have to get the tree
            GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(&mut cost, path);
        }
        for (key, op) in ops_at_path_by_key.into_iter() {
            cost += op.average_case_cost(&key, None)?;
        }

        add_average_case_merk_propagate(
            &mut cost,
            MerkAverageCaseInput::MaxElementsNumber(MAX_ELEMENTS_NUMBER),
        )?;
        Ok(([0u8; 32], None)).wrap_with_cost(cost)
    }

    fn update_base_merk_root_key(&mut self, root_key: Option<Vec<u8>>) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let base_path = KeyInfoPath(vec![]);
        if let Some(input) = self.paths.get(path) {
            // Then we have to get the tree
            GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(&mut cost, &base_path);
        }
        if let Some(_root_key) = root_key {
            // todo: add average case of updating the base root
            // GroveDb::add_average_case_insert_merk_node()
        } else {
        }
        Ok(()).wrap_with_cost(cost)
    }
}