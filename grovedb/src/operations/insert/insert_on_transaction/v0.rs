//! `insert_on_transaction` — **v0** (shipped behaviour, `GROVE_V1`..`GROVE_V3`).
//!
//! `add_element_on_transaction` (itself versioned) followed by explicit
//! parent-propagation. The backward-references element family is rejected —
//! it activates with `GROVE_V4`, whose router ([`super::v1`]) still funnels
//! every non-backward-references call through [`insert_on_transaction_body`]
//! unchanged.

use std::collections::HashMap;

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::Merk;
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch};
use grovedb_version::version::GroveVersion;

use super::super::InsertOptions;
use crate::{Element, Error, GroveDb, OperationCost, Transaction};

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
    // Fail closed: the backward-references family activates with GROVE_V4.
    if matches!(
        element,
        Element::BidirectionalReference(..)
            | Element::ItemWithBackwardsReferences(..)
            | Element::SumItemWithBackwardsReferences(..)
    ) {
        return Err(Error::NotSupported(
            "backward-references elements (BidirectionalReference, \
             ItemWithBackwardsReferences, SumItemWithBackwardsReferences) require GROVE_V4+"
                .to_owned(),
        ))
        .wrap_with_cost(Default::default());
    }

    insert_on_transaction_body(
        db,
        path,
        key,
        element,
        options,
        transaction,
        batch,
        grove_version,
    )
}

/// The shipped insert flow, shared verbatim by v0 and by v1's
/// non-backward-references route so the two produce identical root hashes
/// and costs.
pub(super) fn insert_on_transaction_body<'db, 'b, B: AsRef<[u8]>>(
    db: &GroveDb,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &'db Transaction,
    batch: &StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();

    let mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>> =
        HashMap::default();

    let merk = cost_return_on_error!(
        &mut cost,
        db.add_element_on_transaction(
            path.clone(),
            key,
            element,
            options,
            transaction,
            batch,
            grove_version
        )
    );
    // A generic insert cannot mirror the new child's ordering value into
    // an indexed primary's secondary index. Reject before propagation, so
    // the `StorageBatch` is discarded and nothing is committed.
    cost_return_on_error_no_add!(
        cost,
        crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
            merk.tree_type,
            "insert",
        )
    );
    merk_cache.insert(path.clone(), merk);
    cost_return_on_error!(
        &mut cost,
        db.propagate_changes_with_transaction(merk_cache, path, transaction, batch, grove_version)
    );

    Ok(()).wrap_with_cost(cost)
}
