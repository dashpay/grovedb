//! Cost Errors File

/// An Error coming from costs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Cost Value mismatch
    #[error("storage cost mismatch")]
    StorageCostMismatch,
}
