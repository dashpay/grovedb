//! Errors

use grovedb_costs::CostResult;

#[cfg(feature = "minimal")]
use crate::proofs::chunk::error::ChunkError;

#[cfg(any(feature = "minimal", feature = "verify"))]
#[derive(Debug, thiserror::Error)]
/// Errors
pub enum Error {
    // Input data errors
    /// Overflow
    #[error("overflow error {0}")]
    Overflow(&'static str),

    /// Division by zero error
    #[error("divide by zero error {0}")]
    DivideByZero(&'static str),

    /// Wrong estimated costs element type for level error
    #[error("wrong estimated costs element type for level error {0}")]
    WrongEstimatedCostsElementTypeForLevel(&'static str),

    /// Corrupted sum node error
    #[error("corrupted sum node error {0}")]
    CorruptedSumNode(&'static str),

    /// Invalid input error
    #[error("invalid input error {0}")]
    InvalidInputError(&'static str),

    /// Corrupted code execution
    #[error("corrupted code execution error {0}")]
    CorruptedCodeExecution(&'static str),

    /// Corrupted state
    #[error("corrupted state: {0}")]
    CorruptedState(&'static str),

    /// Chunking error
    #[cfg(feature = "minimal")]
    #[error("chunking error {0}")]
    ChunkingError(ChunkError),

    // TODO: remove
    /// Old chunking error
    #[error("chunking error {0}")]
    OldChunkingError(&'static str),

    /// Chunk restoring error
    #[cfg(feature = "minimal")]
    #[error("chunk restoring error {0}")]
    ChunkRestoringError(ChunkError),

    // TODO: remove
    /// Chunk restoring error
    #[error("chunk restoring error {0}")]
    OldChunkRestoringError(String),

    /// Key not found error
    #[error("key not found error {0}")]
    KeyNotFoundError(&'static str),

    /// Key ordering error
    #[error("key ordering error {0}")]
    KeyOrderingError(&'static str),

    /// Invalid proof error
    #[error("invalid proof error {0}")]
    InvalidProofError(String),

    /// Proof creation error
    #[error("proof creation error {0}")]
    ProofCreationError(String),

    /// Cyclic error
    #[error("cyclic error {0}")]
    CyclicError(&'static str),

    /// Not supported error
    #[error("not supported error {0}")]
    NotSupported(String),

    /// Request amount exceeded error
    #[error("request amount exceeded error {0}")]
    RequestAmountExceeded(String),

    /// Invalid operation error
    #[error("invalid operation error {0}")]
    InvalidOperation(&'static str),

    /// Internal error
    #[error("internal error {0}")]
    InternalError(&'static str),

    /// Specialized costs error
    #[error("specialized costs error {0}")]
    SpecializedCostsError(&'static str),

    /// Client corruption error
    #[error("client corruption error {0}")]
    ClientCorruptionError(String),

    #[cfg(feature = "minimal")]
    /// Storage error
    #[error("storage error {0}")]
    StorageError(grovedb_storage::Error),

    // Merk errors
    /// Ed error
    #[error("ed error: {0}")]
    EdError(ed::Error),

    // Costs errors
    /// Costs errors
    #[error("costs error: {0}")]
    CostsError(grovedb_costs::error::Error),
    // Version errors
    #[error(transparent)]
    /// Version error
    VersionError(grovedb_version::error::GroveVersionError),

    #[error("big sum tree under normal sum tree error {0}")]
    BigSumTreeUnderNormalSumTree(String),

    #[error("unknown tree type {0}")]
    UnknownTreeType(String),

    // Path errors
    /// The path key not found could represent a valid query, just where the
    /// path key isn't there
    #[error("path key not found: {0}")]
    PathKeyNotFound(String),
    /// The path not found could represent a valid query, just where the path
    /// isn't there
    #[error("path not found: {0}")]
    PathNotFound(String),
    /// The path not found could represent a valid query, just where the parent
    /// path merk isn't there
    #[error("path parent layer not found: {0}")]
    PathParentLayerNotFound(String),

    #[error("data corruption error: {0}")]
    /// Corrupted data
    CorruptedData(String),

    #[error(transparent)]
    /// Element error
    ElementError(grovedb_element::error::ElementError),
}

impl Error {
    pub(crate) fn add_context(&mut self, append: impl AsRef<str>) {
        match self {
            Self::OldChunkRestoringError(s)
            | Self::InvalidProofError(s)
            | Self::ProofCreationError(s)
            | Self::NotSupported(s)
            | Self::RequestAmountExceeded(s)
            | Self::ClientCorruptionError(s)
            | Self::BigSumTreeUnderNormalSumTree(s)
            | Self::UnknownTreeType(s)
            | Self::PathKeyNotFound(s)
            | Self::PathNotFound(s)
            | Self::PathParentLayerNotFound(s)
            | Self::CorruptedData(s) => {
                s.push_str(", ");
                s.push_str(append.as_ref());
            }
            _ => {}
        }
    }
}

impl From<grovedb_version::error::GroveVersionError> for Error {
    fn from(value: grovedb_version::error::GroveVersionError) -> Self {
        Error::VersionError(value)
    }
}

impl From<grovedb_element::error::ElementError> for Error {
    fn from(value: grovedb_element::error::ElementError) -> Self {
        Error::ElementError(value)
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl From<grovedb_query::error::Error> for Error {
    fn from(value: grovedb_query::error::Error) -> Self {
        match value {
            grovedb_query::error::Error::NotSupported(s) => Error::NotSupported(s),
            grovedb_query::error::Error::RequestAmountExceeded(s) => {
                Error::RequestAmountExceeded(s)
            }
            grovedb_query::error::Error::CorruptedCodeExecution(s) => {
                Error::CorruptedCodeExecution(s)
            }
            grovedb_query::error::Error::InvalidOperation(s) => Error::InvalidOperation(s),
            // These variants exist when grovedb-query has verify feature enabled.
            // Since minimal implies verify, they're always present when this impl is compiled.
            grovedb_query::error::Error::ProofCreationError(s) => Error::ProofCreationError(s),
            grovedb_query::error::Error::InvalidProofError(s) => Error::InvalidProofError(s),
            grovedb_query::error::Error::KeyOrderingError(s) => Error::KeyOrderingError(s),
            grovedb_query::error::Error::EdError(e) => Error::EdError(e),
        }
    }
}

pub trait MerkErrorExt {
    fn add_context(self, append: impl AsRef<str>) -> Self;
}

impl<T> MerkErrorExt for CostResult<T, Error> {
    fn add_context(self, append: impl AsRef<str>) -> Self {
        self.map_err(|mut e| {
            e.add_context(append.as_ref());
            e
        })
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::{CostResult, CostsExt};

    use super::{Error, MerkErrorExt};

    #[test]
    fn test_add_context_mutates_string_errors() {
        let mut err = Error::PathNotFound("missing".to_string());
        err.add_context("ctx");
        match err {
            Error::PathNotFound(s) => assert_eq!(s, "missing, ctx"),
            _ => panic!("expected path not found"),
        }
    }

    #[test]
    fn test_add_context_noop_for_static_errors() {
        let mut err = Error::InvalidOperation("bad op");
        err.add_context("ctx");
        match err {
            Error::InvalidOperation(s) => assert_eq!(s, "bad op"),
            _ => panic!("expected invalid operation"),
        }
    }

    #[test]
    fn test_cost_result_add_context() {
        let result: CostResult<(), Error> =
            Err(Error::CorruptedData("data".to_string())).wrap_with_cost(Default::default());

        let err = result.add_context("decode").unwrap().unwrap_err();
        match err {
            Error::CorruptedData(s) => assert_eq!(s, "data, decode"),
            _ => panic!("expected corrupted data"),
        }
    }

    #[test]
    fn test_from_query_error_mapping() {
        let e: Error = grovedb_query::error::Error::NotSupported("n".to_string()).into();
        assert!(matches!(e, Error::NotSupported(s) if s == "n"));

        let e: Error = grovedb_query::error::Error::RequestAmountExceeded("r".to_string()).into();
        assert!(matches!(e, Error::RequestAmountExceeded(s) if s == "r"));

        let e: Error = grovedb_query::error::Error::CorruptedCodeExecution("c").into();
        assert!(matches!(e, Error::CorruptedCodeExecution("c")));

        let e: Error = grovedb_query::error::Error::InvalidOperation("i").into();
        assert!(matches!(e, Error::InvalidOperation("i")));

        let e: Error = grovedb_query::error::Error::ProofCreationError("p".to_string()).into();
        assert!(matches!(e, Error::ProofCreationError(s) if s == "p"));

        let e: Error = grovedb_query::error::Error::InvalidProofError("v".to_string()).into();
        assert!(matches!(e, Error::InvalidProofError(s) if s == "v"));

        let e: Error = grovedb_query::error::Error::KeyOrderingError("k").into();
        assert!(matches!(e, Error::KeyOrderingError("k")));
    }

    #[test]
    fn test_from_query_error_ed_error_mapping() {
        let e: Error = grovedb_query::error::Error::EdError(ed::Error::UnexpectedByte(7)).into();
        assert!(matches!(e, Error::EdError(ed::Error::UnexpectedByte(7))));
    }
}
