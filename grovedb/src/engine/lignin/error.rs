//! The errors of LigninDB as the seam reports them (`engine.md`, "Commit
//! errors"): the outcomes of a commit by their engine-neutral class, a
//! scope's refusals by theirs, a limit of the engine as
//! `Error::NotSupported`, and everything else as a storage error with the
//! engine's own words.

use grovedb_storage::{
    error::{CommitError, WriteScopeError},
    Error,
};
use lignin_tree::directory::DirectoryError;
use lignindb::{
    commit::{CommitError as Commit, Failure},
    db::WriteError,
    savepoint::SavepointError,
    txn::TxnError,
};

/// An error of the engine that the seam has no class for, by its words.
pub(super) fn failed(error: impl std::fmt::Display) -> Error {
    Error::StorageError(format!("lignindb: {error}"))
}

/// What the engine does not do, or not yet.
pub(super) fn not_supported(what: &str) -> Error {
    Error::NotSupported(format!("lignindb: {what}"))
}

pub(super) fn txn_error(error: TxnError) -> Error {
    match error {
        TxnError::ScopeOpen => Error::WriteScope(WriteScopeError::ScopesOpen),
        TxnError::ScopeOverlap { .. } => Error::WriteScope(WriteScopeError::Overlap),
        // The subtree directory takes a Merk no more than 65,535 Merks
        // deep (`node-store.md`, "A Merk too deep for the directory").
        TxnError::Directory(DirectoryError::TooDeep) => not_supported(&error.to_string()),
        error => failed(error),
    }
}

pub(super) fn savepoint_error(error: SavepointError) -> Error {
    match error {
        SavepointError::ScopesOpen => Error::WriteScope(WriteScopeError::ScopesOpen),
        SavepointError::NoSavepoint => failed(error),
    }
}

/// How a commit, a write with no transaction or a wipe ended.
pub(super) fn write_error(error: WriteError) -> Error {
    match error {
        WriteError::Commit(Commit::Conflict) => Error::Commit(CommitError::Conflict),
        WriteError::Commit(Commit::NotDurable { .. }) => Error::Commit(CommitError::NotDurable),
        // A commit that halted the session is not published, and the next
        // open recovers it if its bytes survive: whether it happened is
        // settled there.
        WriteError::Commit(Commit::Unsettled { .. } | Commit::Halted { .. }) => {
            Error::Commit(CommitError::Unsettled)
        }
        WriteError::Commit(Commit::ScopesOpen) => Error::WriteScope(WriteScopeError::ScopesOpen),
        WriteError::Commit(Commit::Refused(error) | Commit::Failed(Failure::Txn(error))) => {
            txn_error(error)
        }
        // A commit that ended the session without being one of the
        // outcomes above says so: the database has to be opened again.
        error => match error.reopen() {
            Some(reopen) if matches!(error, WriteError::Commit(_)) => {
                failed(format!("{error}: {reopen}"))
            }
            _ => failed(error),
        },
    }
}

/// The error of the node store as an error of the seam: its storage error
/// as it is, and anything else by its words.
pub(super) fn merk_error(error: grovedb_merk::Error) -> Error {
    match error {
        grovedb_merk::Error::StorageError(error) => error,
        grovedb_merk::Error::NotSupported(what) => Error::NotSupported(what),
        error => Error::StorageError(error.to_string()),
    }
}
