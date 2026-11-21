//! The errors of the storage layer: the enum of `storage/src/error.rs`,
//! with the model's stand-in where that one holds a RocksDB error.
//!
//! GroveDB matches on these enums, so each has the variants of the real
//! one. `lignin-testkit/tests/cost_model.rs` compares the two files in the
//! tests of the real workspace.

/// Storage and underlying errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Error
    #[error("storage error: {0}")]
    StorageError(String),
    /// Cost Error
    #[error("cost error: {0}")]
    CostError(grovedb_costs::error::Error),
    /// A write or commit was attempted through a snapshot read transaction.
    #[error("snapshot read transaction is read-only: refused {0}")]
    SnapshotReadOnlyTransaction(&'static str),
    /// What the engine under the storage layer refused.
    #[error("rocksDB error: {0}")]
    #[cfg(feature = "rocksdb_storage")]
    RocksDBError(#[from] crate::rocksdb_storage::Error),
    /// A commit ended in an outcome that an engine reports in terms of its
    /// own.
    #[error("commit error: {0}")]
    Commit(CommitError),
    /// A write scope could not do what was asked of it.
    #[error("write scope error: {0}")]
    WriteScope(WriteScopeError),
    /// The engine does not do what was asked of it: an operation it has
    /// not, or a limit of it that the operation met. Nothing was changed,
    /// and the stored data is not at fault.
    #[error("not supported: {0}")]
    NotSupported(String),
}

/// How a commit ended when it did not end in plain success, in terms that
/// name no engine.
///
/// For each outcome the transaction is over. The outcomes differ in what the
/// database holds afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum CommitError {
    /// Another commit wrote a key after this transaction first wrote it. The
    /// transaction's writes are discarded and the database is unchanged.
    /// This outcome is reported for a conflict and for nothing else.
    #[error(
        "conflict: another commit wrote a key after this transaction first wrote it, or took a value that a write of this transaction keeps"
    )]
    Conflict,
    /// The commit happened and is visible, but the engine was asked to make
    /// it durable and could not: a crash may still lose it.
    #[error("not durable: the commit happened, but it is not known to survive a crash")]
    NotDurable,
    /// The engine could not find out whether the commit happened. The
    /// database is unchanged in this session, which accepts no further
    /// commit; the next open settles it.
    #[error("unsettled: the engine could not find out whether the commit happened")]
    Unsettled,
}

/// Why a write scope could not do what was asked of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum WriteScopeError {
    /// The scope needs something another open scope holds. It never waits.
    /// An engine that allows one scope at a time reports every further scope
    /// this way.
    #[error("overlap: another open write scope holds what this one needs")]
    Overlap,
    /// The operation needs every write scope of the transaction closed.
    #[error("a write scope of the transaction is still open")]
    ScopesOpen,
    /// The engine cannot undo a scope on its own. The scope's writes are
    /// still in the transaction, which can be rolled back as a whole.
    #[error("this engine cannot roll back a write scope on its own")]
    RollbackUnsupported,
}

impl Error {
    /// The engine-neutral class of an error that a commit returned. The
    /// model commits every transaction, so only [`Error::Commit`] has one.
    pub fn commit_error(&self) -> Option<CommitError> {
        match self {
            Error::Commit(error) => Some(*error),
            _ => None,
        }
    }

    /// Whether a commit returned this error because another commit wrote a
    /// key after the transaction first wrote it.
    pub fn is_commit_conflict(&self) -> bool {
        self.commit_error() == Some(CommitError::Conflict)
    }
}
