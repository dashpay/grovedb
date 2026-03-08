use std::collections::HashMap;

use grovedb_costs::{cost_return_on_error_into, CostResult, CostsExt, OperationCost};
use grovedb_merk::{tree::TreeFeatureType, Merk, Op};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use grovedb_merk::element::insert::ElementInsertToStorageExtensions;

use crate::{
    batch::{key_info::KeyInfo, BatchApplyOptions, TreeCacheMerkByPath},
    Element, Error,
};

impl<'db, S, F> TreeCacheMerkByPath<S, F>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    /// V0: Insert item element without InsertOnly enforcement.
    /// Only checks for existing elements when
    /// `validate_insertion_does_not_override` is explicitly set.
    pub(in crate::batch) fn insert_item_element_v0(
        merks: &mut HashMap<Vec<Vec<u8>>, Merk<S>>,
        path: &[Vec<u8>],
        element: Element,
        key_info: KeyInfo,
        batch_operations: &mut Vec<(Vec<u8>, Op)>,
        batch_apply_options: &BatchApplyOptions,
        merk_feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        if batch_apply_options.validate_insertion_does_not_override {
            let merk = merks.get_mut(path).expect("the Merk is cached");

            let inserted = cost_return_on_error_into!(
                &mut cost,
                element.insert_if_not_exists_into_batch_operations(
                    merk,
                    key_info.get_key(),
                    batch_operations,
                    merk_feature_type,
                    grove_version,
                )
            );
            if !inserted {
                return Err(Error::InvalidBatchOperation(
                    "attempting to overwrite an element",
                ))
                .wrap_with_cost(cost);
            }
        } else {
            cost_return_on_error_into!(
                &mut cost,
                element.insert_into_batch_operations(
                    key_info.get_key(),
                    batch_operations,
                    merk_feature_type,
                    grove_version,
                )
            );
        }
        Ok(()).wrap_with_cost(cost)
    }
}
