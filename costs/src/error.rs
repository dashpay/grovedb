//! Cost Errors File

use crate::StorageCost;

/// An Error coming from costs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Cost Value mismatch
    #[error("storage_cost cost mismatch")]
    StorageCostMismatch {
        /// The expected storage cost, decomposed
        expected: StorageCost,
        /// The actual storage cost in summed bytes
        actual_total_bytes: u32,
    },
}
