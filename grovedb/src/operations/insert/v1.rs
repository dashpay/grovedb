use std::collections::HashMap;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt};
use grovedb_merk::Merk;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch};
use grovedb_version::{check_grovedb_v1_with_cost, version::GroveVersion};

use super::InsertOptions;
use crate::{Element, Error, GroveDb, Transaction};

pub(super) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &'db Transaction,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    check_grovedb_v1_with_cost!(
        "insert_on_transaction",
        grove_version
            .grovedb_versions
            .operations
            .insert
            .insert_on_transaction
    );

    let mut cost = Default::default();

    // TODO

    // let mut merk_cache: HashMap<SubtreePath<'b, B>,
    // Merk<PrefixedRocksDbTransactionContext>> =     HashMap::default();

    // let merk = cost_return_on_error!(
    //     &mut cost,
    //     db.add_element_on_transaction(
    //         path.clone(),
    //         key,
    //         element,
    //         options,
    //         transaction,
    //         batch,
    //         grove_version
    //     )
    // );
    // merk_cache.insert(path.clone(), merk);
    // cost_return_on_error!(
    //     &mut cost,
    //     db.propagate_changes_with_transaction(merk_cache, path, transaction,
    // batch, grove_version) );

    Ok(()).wrap_with_cost(cost)
}
