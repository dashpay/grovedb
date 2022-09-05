//! Cost Errors File

/// An Error coming from costs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Cost Value mismatch
    #[error("storage_cost cost mismatch")]
    StorageCostMismatch,
}
