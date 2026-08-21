//! `delete_internal_on_transaction` — versioned dispatch.
//!
//! Deletes a single element (a value or a subtree, optionally non-empty)
//! from an already-committed state on a transaction. Every `GroveDb::delete`
//! family call routes through here.
//!
//! When the deleted element is a **non-empty child tree**, the parent layer
//! has to be mutated and its new link hash propagated upward. How the parent
//! Merk is obtained for that mutation is **consensus-critical** and
//! version-gated (issue #686):
//!
//! * **[v0]** — legacy behaviour, frozen for `GROVE_V1`..`GROVE_V3` (all live
//!   in production). The parent Merk is reopened labeled with the **deleted
//!   child's** tree type. The label is wrong: for the six Provable* types
//!   (whose link hash embeds the aggregate via `hash_for_link`) a mismatched
//!   parent/child pairing either commits a wrong link hash into the
//!   grandparent (Provable* parent, plain child) or panics in
//!   `hash_for_link` (plain parent, Provable* child). Any such delete that
//!   already executed on a live chain committed the resulting hash into a
//!   consensus root, and the reopen also has its own cost profile, so the
//!   released path is kept bug-for-bug for replay compatibility.
//! * **[v1]** — `GROVE_V4`+. The already-open parent Merk is reused; it
//!   carries the parent's true tree type, so delete propagation hashes and
//!   aggregates with the parent type, and the redundant reopen (an extra
//!   storage-context open plus `open_layered_with_root_key`) disappears.
//!   The branch also propagates through the full indexed-aware walk
//!   (`propagate_changes_with_transaction`, like the operation's other
//!   branches) instead of the legacy batch propagation, so a delete nested
//!   inside an indexed-tree primary's child subtree re-mirrors the
//!   primary's canonical secondary row instead of erroring (PSIT / PCPSIT)
//!   or desyncing the count index (PCIT).
//!
//! The two implementations differ ONLY in that non-empty-child-tree branch;
//! everything else is identical. See [v0] / [v1].
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::{
    storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::Error as MerkError;
use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
use grovedb_version::version::GroveVersion;

use super::DeleteOptions;
use crate::{Error, GroveDb, Transaction};

impl GroveDb {
    /// Delete an element on a transaction, clearing a non-empty subtree's
    /// storage when the options allow it, and propagate the parent layer's
    /// new link hash upward.
    ///
    /// Version dispatch (consensus-critical) — see the module documentation.
    pub(crate) fn delete_internal_on_transaction<B: AsRef<[u8]>>(
        &self,
        path: SubtreePath<B>,
        key: &[u8],
        options: &DeleteOptions,
        transaction: &Transaction,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .delete
            .delete_internal_on_transaction
        {
            0 => self.delete_internal_on_transaction_v0(
                path,
                key,
                options,
                transaction,
                sectioned_removal,
                batch,
                grove_version,
            ),
            1 => self.delete_internal_on_transaction_v1(
                path,
                key,
                options,
                transaction,
                sectioned_removal,
                batch,
                grove_version,
            ),
            version => Err(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "delete_internal_on_transaction".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                }
                .into(),
            )
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
