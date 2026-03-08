mod v0;
mod v1;

use std::collections::HashMap;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::{tree::TreeFeatureType, Merk, Op};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{key_info::KeyInfo, BatchApplyOptions, TreeCacheMerkByPath},
    Element, Error,
};

impl<'db, S, F> TreeCacheMerkByPath<S, F>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    S: StorageContext<'db>,
{
    /// Insert an item element into batch operations, dispatching to the
    /// appropriate version based on `grove_version`.
    pub(in crate::batch) fn insert_item_element(
        merks: &mut HashMap<Vec<Vec<u8>>, Merk<S>>,
        path: &[Vec<u8>],
        element: Element,
        key_info: KeyInfo,
        is_insert_only: bool,
        batch_operations: &mut Vec<(Vec<u8>, Op)>,
        batch_apply_options: &BatchApplyOptions,
        merk_feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .apply_batch
            .execute_ops_on_path
        {
            0 => Self::insert_item_element_v0(
                merks,
                path,
                element,
                key_info,
                batch_operations,
                batch_apply_options,
                merk_feature_type,
                grove_version,
            ),
            // V1 enforces InsertOnly uniqueness: operations marked as
            // InsertOnly will check for existing elements and reject
            // overwrites, adding a seek cost. This was not enforced in V0
            // to preserve consensus on historical block replay.
            1 => Self::insert_item_element_v1(
                merks,
                path,
                element,
                key_info,
                is_insert_only,
                batch_operations,
                batch_apply_options,
                merk_feature_type,
                grove_version,
            ),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "insert_item_element".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
