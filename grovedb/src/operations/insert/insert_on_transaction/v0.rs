//! `insert_on_transaction` — **v0** (shipped behaviour, `GROVE_V1`..`GROVE_V3`).
//!
//! `add_element_on_transaction` (itself versioned) followed by explicit
//! parent-propagation. The backward-references element family is rejected —
//! it activates with `GROVE_V4`, whose router ([`super::v1`]) still funnels
//! every non-backward-references call through [`insert_on_transaction_body`]
//! unchanged.

use crate::operations::indexed_tree::reject_generic_write_into_indexed_primary;
use std::collections::HashMap;

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::Merk;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::super::InsertOptions;
use crate::{Element, Engine, Error, GroveDb, OperationCost, Transaction};

pub(super) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>, E: Engine>(
    db: &'db GroveDb<E>,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &Transaction<'db, E>,
    batch: &E::Batch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    // Fail closed: the backward-references family activates with GROVE_V4.
    // Checked on the underlying element so a hand-built wrapper
    // (`NonCounted(ItemWithBackwardsReferences)`, which no constructor
    // produces) cannot slip past the gate into the shipped insert body.
    if element.underlying().supports_backward_references() {
        return Err(Error::NotSupported(
            "backward-references elements (BidirectionalReference, \
             ItemWithBackwardsReferences, SumItemWithBackwardsReferences, \
             ItemWithSumItemWithBackwardsReferences) require GROVE_V4+"
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
pub(super) fn insert_on_transaction_body<'db, 'b, B: AsRef<[u8]>, E: Engine>(
    db: &'db GroveDb<E>,
    path: SubtreePath<'b, B>,
    key: &[u8],
    element: Element,
    options: InsertOptions,
    transaction: &Transaction<'db, E>,
    batch: &E::Batch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();

    let mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<E::Subtree<'_>>> = HashMap::default();

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
    // the `E::Batch` is discarded and nothing is committed.
    cost_return_on_error_no_add!(
        cost,
        reject_generic_write_into_indexed_primary(merk.tree_type, "insert",)
    );
    merk_cache.insert(path.clone(), merk);
    cost_return_on_error!(
        &mut cost,
        db.propagate_changes_with_transaction(merk_cache, path, transaction, batch, grove_version)
    );

    Ok(()).wrap_with_cost(cost)
}
