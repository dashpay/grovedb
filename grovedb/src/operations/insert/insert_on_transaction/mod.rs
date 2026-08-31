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
//! * **[v1]** — `GROVE_V4`+. Behaviour-preserving router: calls that neither
//!   insert a `BidirectionalReference` nor set
//!   `InsertOptions::propagate_backward_references` run the exact v0 body
//!   (same root hashes, same costs). The remainder run through the
//!   `MerkCache`-based flow in [v1], which performs backward-reference
//!   bookkeeping and propagation (see `adr/bidirectional_references.md`).
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::CostResult;
use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
use grovedb_version::{dispatch_version, version::GroveVersion};

use super::InsertOptions;
use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// Insert an element on a transaction — versioned dispatch, see the
    /// module documentation.
    pub(crate) fn insert_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        element: Element,
        options: InsertOptions,
        transaction: &'db Transaction,
        batch: &StorageBatch,
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
