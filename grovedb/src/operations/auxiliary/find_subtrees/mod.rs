//! Version dispatch for recursive subtree discovery.
//!
//! V0 preserves historical traversal for Grove V1..V3. V1 recognizes
//! non-Merk descendants for Grove V4+. Keep their implementations separate
//! so changes to current discovery cannot alter historical replay.

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use crate::{Error, GroveDb, TransactionArg};

impl GroveDb {
    /// Finds subtree namespaces recursively, including the starting Merk
    /// namespace at `path`. Returned paths are absolute.
    ///
    /// On V4+, non-Merk descendants are included for cleanup but are not
    /// traversed: their data records are not Merk nodes and they cannot
    /// contain child subtrees. V1..V3 retain the historical traversal and
    /// its costs and errors for replay compatibility.
    ///
    /// # Storage batch visibility
    ///
    /// This method reads directly from the transaction (passing `None` for
    /// the storage batch parameter), so it only sees data that has been
    /// **committed** to the transaction. Any writes staged in a pending
    /// `StorageBatch` (e.g., from `apply_body` during batch processing)
    /// are invisible.
    ///
    /// In practice this is safe because the batch consistency check
    /// ([`crate::batch::QualifiedGroveDbOp::verify_consistency_of_operations`])
    /// rejects batches that insert subtrees under paths being deleted.
    /// The stale-state window only matters if the consistency check is
    /// bypassed via
    /// [`BatchApplyOptions::disable_operation_consistency_check`](crate::batch::BatchApplyOptions::disable_operation_consistency_check).
    pub fn find_subtrees<B: AsRef<[u8]>>(
        &self,
        path: &SubtreePath<B>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<Vec<Vec<u8>>>, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .non_merk_tree
            .subtree_discovery
        {
            0 => self.find_subtrees_v0(path, transaction, grove_version),
            1 => self.find_subtrees_v1(path, transaction, grove_version),
            version => Err(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "find_subtrees".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                }
                .into(),
            )
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
