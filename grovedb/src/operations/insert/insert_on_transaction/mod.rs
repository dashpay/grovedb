//! `insert_on_transaction` — versioned dispatch.
//!
//! The single-element insert executor behind `GroveDb::insert` (and through
//! it `insert_if_not_exists` / `insert_if_changed_value`).
//!
//! * **[v0]** — shipped behaviour, selected by `GROVE_V1`..`GROVE_V3`:
//!   `add_element_on_transaction` + explicit parent propagation. Rejects the
//!   backward-references element family (`BidirectionalReference`,
//!   `ItemWithBackwardsReferences`, `SumItemWithBackwardsReferences`), which
//!   activates with `GROVE_V4`.
//! * **[v1]** — V4 automatic maintenance. Observes old values before mutation,
//!   retaining the fetched nodes for execution. Ordinary values retain indexed
//!   propagation; participants use the reference cache and shared planners.

mod v0;
mod v1;

use grovedb_costs::CostResult;
use grovedb_path::SubtreePath;
use grovedb_version::{dispatch_version, version::GroveVersion};

use super::InsertOptions;
use crate::{Element, Engine, Error, GroveDb, Transaction};

impl<E: Engine> GroveDb<E> {
    /// Insert an element on a transaction — versioned dispatch, see the
    /// module documentation.
    pub(crate) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &Transaction<'db, E>,
        batch: &E::Batch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        dispatch_version!(
            "insert_on_transaction",
            grove_version
                .grovedb_versions
                .operations
                .insert
                .insert_on_transaction,
            0 => {
                v0::insert_on_transaction(
                    self,
                    path,
                    key,
                    element,
                    options,
                    transaction,
                    batch,
                    grove_version,
                )
            }
            1 => {
                v1::insert_on_transaction(
                    self,
                    path,
                    key,
                    element,
                    options,
                    transaction,
                    batch,
                    grove_version,
                )
            }
        )
    }
}
