//! Cost Errors File

use crate::StorageCost;

/// An Error coming from costs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Cost Value mismatch
    #[error("storage_cost cost mismatch added: {0} replaced: {1} actual:{actual_total_bytes}", expected.added_bytes, expected.replaced_bytes)]
    StorageCostMismatch {
        /// The expected storage cost, decomposed
        expected: StorageCost,
        /// The actual storage cost in summed bytes
        actual_total_bytes: u32,
    },
}
