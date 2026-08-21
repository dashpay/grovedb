//! Error type for the private document store.

/// Errors returned by [`PrivateDocumentStore`](crate::PrivateDocumentStore)
/// operations.
#[derive(Debug, thiserror::Error)]
pub enum PrivateDocumentStoreError {
    /// The store configuration is invalid (zero entry size, bad chunk power).
    #[error("invalid private document store config: {0}")]
    InvalidConfig(String),

    /// An entry's byte length does not match the committed `entry_size`.
    #[error("invalid entry size: expected {expected} bytes, got {actual}")]
    InvalidEntrySize {
        /// The committed entry size of the store.
        expected: u32,
        /// The actual length of the offered entry.
        actual: usize,
    },

    /// Underlying data is missing or inconsistent.
    #[error("corrupted private document store data: {0}")]
    CorruptedData(String),

    /// Wrapped storage / bulk tree failure.
    #[error("private document store data error: {0}")]
    InvalidData(String),
}
