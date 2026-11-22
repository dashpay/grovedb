// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Storage Errors File

/// Storage and underlying errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Error
    #[error("storage error: {0}")]
    StorageError(String),
    /// Cost Error
    #[error("cost error: {0}")]
    CostError(grovedb_costs::error::Error),
    /// A write or commit was attempted through a snapshot read
    /// transaction. Snapshot read transactions
    /// (`start_snapshot_read_transaction`) exist to pin multi-operation
    /// READS to one committed state; writing through one is refused
    /// because `set_snapshot` arms commit-time conflict detection, which
    /// can fail such a commit with `Busy` where a plain transaction's
    /// would have succeeded. The payload names the refused operation.
    #[error("snapshot read transaction is read-only: refused {0}")]
    SnapshotReadOnlyTransaction(&'static str),
    /// Rocks DB error
    #[error("rocksDB error: {0}")]
    #[cfg(feature = "rocksdb_storage")]
    RocksDBError(#[from] rocksdb::Error),
    /// A commit ended in an outcome that an engine reports in terms of its
    /// own, or a write was refused as a conflict
    /// ([`CommitError::Conflict`]). The RocksDB engine never returns this
    /// variant: its conflict is a [`Error::RocksDBError`] of kind `Busy`,
    /// which [`Error::commit_error`] classes as [`CommitError::Conflict`].
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
/// For each outcome of a commit the transaction is over. The outcomes
/// differ in what the database holds afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum CommitError {
    /// Another commit wrote a key after this transaction first wrote it. The
    /// transaction's writes are discarded and the database is unchanged.
    /// This outcome is reported for a conflict and for nothing else.
    ///
    /// An engine can report it from a write as well, before any commit:
    /// another commit took away a value that the write keeps as its
    /// transaction read it. That write changed nothing: its transaction is
    /// open and holds the writes it held before, and a write scope of the
    /// caller's keeps what the write claimed in it.
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
    /// The engine-neutral class of an error that a commit returned, or
    /// `None` when the commit failed for a reason that has no such class.
    /// The transaction is then over and the database unchanged, as for any
    /// failed commit. A write that an engine refused as a conflict has the
    /// class [`CommitError::Conflict`] too.
    ///
    /// The RocksDB engine reports a conflict as a RocksDB error of kind
    /// `Busy`, and this method classes it as [`CommitError::Conflict`]. A
    /// RocksDB `TryAgain` has no class: RocksDB returns it when it can no
    /// longer tell whether there was a conflict, so it is not one.
    pub fn commit_error(&self) -> Option<CommitError> {
        match self {
            Error::Commit(error) => Some(*error),
            #[cfg(feature = "rocksdb_storage")]
            Error::RocksDBError(error) if error.kind() == rocksdb::ErrorKind::Busy => {
                Some(CommitError::Conflict)
            }
            _ => None,
        }
    }

    /// Whether a commit returned this error because another commit wrote a
    /// key after the transaction first wrote it, or a write returned it as
    /// a conflict ([`CommitError::Conflict`]).
    pub fn is_commit_conflict(&self) -> bool {
        self.commit_error() == Some(CommitError::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::{CommitError, Error, WriteScopeError};

    #[test]
    fn an_engine_neutral_commit_error_is_its_own_class() {
        for outcome in [
            CommitError::Conflict,
            CommitError::NotDurable,
            CommitError::Unsettled,
        ] {
            let error = Error::Commit(outcome);
            assert_eq!(error.commit_error(), Some(outcome));
            assert_eq!(error.is_commit_conflict(), outcome == CommitError::Conflict);
            assert_eq!(error.to_string(), format!("commit error: {outcome}"));
        }
        assert!(CommitError::Conflict.to_string().starts_with("conflict:"));
        assert!(CommitError::NotDurable
            .to_string()
            .starts_with("not durable:"));
        assert!(CommitError::Unsettled.to_string().starts_with("unsettled:"));
    }

    #[test]
    fn an_error_that_is_not_a_commit_outcome_has_no_commit_class() {
        let others = [
            Error::StorageError("disk".to_owned()),
            Error::SnapshotReadOnlyTransaction("commit"),
            Error::WriteScope(WriteScopeError::Overlap),
            Error::WriteScope(WriteScopeError::ScopesOpen),
            Error::WriteScope(WriteScopeError::RollbackUnsupported),
        ];
        for error in &others {
            assert_eq!(error.commit_error(), None);
            assert!(!error.is_commit_conflict());
        }
        assert_eq!(
            others[2].to_string(),
            "write scope error: overlap: another open write scope holds what this one needs"
        );
        assert_eq!(
            others[3].to_string(),
            "write scope error: a write scope of the transaction is still open"
        );
        assert_eq!(
            others[4].to_string(),
            "write scope error: this engine cannot roll back a write scope on its own"
        );
    }

    #[cfg(feature = "rocksdb_storage")]
    #[test]
    fn a_rocksdb_commit_that_loses_is_busy_and_classed_as_a_conflict() {
        use grovedb_path::SubtreePath;

        use crate::{rocksdb_storage::test_utils::TempStorage, Storage, StorageContext};

        let storage = TempStorage::new();
        let put = |transaction, value: &[u8]| {
            storage
                .get_immediate_storage_context(SubtreePath::empty(), transaction)
                .unwrap()
                .put(b"key", value, None, None)
                .unwrap()
                .expect("put");
        };
        let loser = storage.start_transaction();
        put(&loser, b"first");
        let winner = storage.start_transaction();
        put(&winner, b"second");
        storage
            .commit_transaction(winner)
            .unwrap()
            .expect("the first commit of the key succeeds");

        let error = storage
            .commit_transaction(loser)
            .unwrap()
            .expect_err("the second commit of the key loses");
        assert!(
            matches!(&error, Error::RocksDBError(e) if e.kind() == rocksdb::ErrorKind::Busy),
            "the RocksDB engine reports the conflict as it always did: {error:?}"
        );
        assert_eq!(error.commit_error(), Some(CommitError::Conflict));
        assert!(error.is_commit_conflict());
    }

    #[cfg(feature = "rocksdb_storage")]
    #[test]
    fn a_rocksdb_error_of_another_kind_is_not_a_conflict() {
        use crate::{rocksdb_storage::test_utils::TempStorage, Storage};

        let storage = TempStorage::new();
        let transaction = storage.start_transaction();
        let error = transaction
            .rollback_to_savepoint()
            .expect_err("there is no savepoint to roll back to");
        assert!(matches!(&error, Error::RocksDBError(e) if e.kind() != rocksdb::ErrorKind::Busy));
        assert_eq!(error.commit_error(), None);
        assert!(!error.is_commit_conflict());
    }
}
