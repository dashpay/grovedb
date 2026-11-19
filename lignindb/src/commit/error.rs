//! How a commit can end other than by publishing its version.

use std::{error::Error, fmt};

use lignin_format::CommitVersion;
use lignin_store::{flush::FlushError, table::LinkError, wal::LogError};
use lignin_tree::hash::HashError;

use super::outcome::Published;
use crate::{snapshot::PublishError, txn::TxnError};

/// What kept a commit from being made, or from being shown: the error of
/// the part of the store that refused.
#[derive(Debug)]
pub enum Failure {
    /// The transaction, its pages, its values or the store behind them.
    Txn(TxnError),
    /// A hash that was stale could not be made (CM3 step 5).
    Hash(HashError),
    /// The log.
    Log(LogError),
    /// The page table refused the free link of a page.
    Table(LinkError),
    /// The version could not be published.
    Publish(PublishError),
    /// The log and the versions of the database do not agree on the
    /// version the next commit takes: they are not of one store, or the
    /// log holds a group that this session has not published, as after a
    /// commit that panicked between its append and its publication. The
    /// commit that finds it has failed, and the session stops
    /// ([`Stopped::Halted`]).
    VersionGap {
        /// The version the log would give the group.
        log: CommitVersion,
        /// The latest published version.
        latest: CommitVersion,
    },
    /// The log gave the group a place, wrote nothing there, and gave the
    /// place away again, more often than a commit tries. Something appends
    /// to the log without the lock on the counts of live values.
    PlaceLost,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Txn(error) => write!(f, "{error}"),
            Self::Hash(error) => write!(f, "a stale hash could not be made: {error}"),
            Self::Log(error) => write!(f, "the log: {error}"),
            Self::Table(error) => write!(f, "the page table: {error}"),
            Self::Publish(error) => write!(f, "{error}"),
            Self::VersionGap { log, latest } => write!(
                f,
                "the log would give the commit version {log}, and the latest published version \
                 is {latest}"
            ),
            Self::PlaceLost => f.write_str(
                "the place of the group in the log was taken again and again while the commit \
                 held the lock on the value counts",
            ),
        }
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Txn(error) => Some(error),
            Self::Hash(error) => Some(error),
            Self::Log(error) => Some(error),
            Self::Table(error) => Some(error),
            Self::Publish(error) => Some(error),
            Self::VersionGap { .. } | Self::PlaceLost => None,
        }
    }
}

impl<E: Into<TxnError>> From<E> for Failure {
    fn from(error: E) -> Self {
        Self::Txn(error.into())
    }
}

impl From<HashError> for Failure {
    fn from(error: HashError) -> Self {
        Self::Hash(error)
    }
}

impl From<LogError> for Failure {
    fn from(error: LogError) -> Self {
        Self::Log(error)
    }
}

impl From<LinkError> for Failure {
    fn from(error: LinkError) -> Self {
        Self::Table(error)
    }
}

impl From<PublishError> for Failure {
    fn from(error: PublishError) -> Self {
        Self::Publish(error)
    }
}

/// Why a session takes no commit any more (`transactions.md` CM9 items 5
/// and 6). Its reads go on. The next open of the database settles what the
/// log holds (DU2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// The engine could not find out whether an item is in the log: the
    /// group of the commit of this version, or an item that write-back
    /// appended before that commit could be made.
    Unsettled {
        /// The version that commit would have had.
        version: CommitVersion,
        /// What the log said.
        reason: String,
    },
    /// The group of the commit of this version is in the log, and this
    /// session has not installed and published the commit: its install
    /// was refused, or a later commit found the group in the log
    /// ([`Failure::VersionGap`]).
    Halted {
        /// The version of that commit.
        version: CommitVersion,
        /// What failed.
        reason: String,
    },
}

impl Stopped {
    /// The version of the commit that the session stopped at: the latest
    /// published version is the one before it, and stays so.
    pub const fn version(&self) -> CommitVersion {
        match self {
            Self::Unsettled { version, .. } | Self::Halted { version, .. } => *version,
        }
    }
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsettled { version, reason } => write!(
                f,
                "whether commit {version} is in the log is not known ({reason})"
            ),
            Self::Halted { version, reason } => write!(
                f,
                "commit {version} is in the log and could not be installed ({reason})"
            ),
        }
    }
}

/// How a commit ended, if not with its version published and, where the
/// sync policy asked for it, durable (`transactions.md` CM5).
///
/// | Error | The transaction | The database |
/// | --- | --- | --- |
/// | [`ScopesOpen`](Self::ScopesOpen), [`NotFinalized`](Self::NotFinalized), [`Refused`](Self::Refused), [`NotReady`](Self::NotReady), [`Stopped`](Self::Stopped) | still open, as it was, savepoints included | unchanged |
/// | [`Conflict`](Self::Conflict) | ended, its writes discarded | unchanged |
/// | [`Failed`](Self::Failed) | ended, its writes discarded | unchanged, and no recovery ever brings the commit back; the session takes no further commit if the failure is [`Failure::VersionGap`] |
/// | [`NotDurable`](Self::NotDurable) | ended | **the commit is published**, and is not known to be durable |
/// | [`Unsettled`](Self::Unsettled), [`Halted`](Self::Halted) | ended | unchanged in this session, which takes no further commit; the next open settles it |
#[derive(Debug)]
pub enum CommitError {
    /// A scope of the transaction is open (CM1, SC8). The transaction is
    /// as it was.
    ScopesOpen,
    /// The transaction has a pending mark left: its tree is not finalized
    /// up to the root, and it is not committed so (CM1, SC9). The
    /// transaction is as it was, the mark included.
    NotFinalized,
    /// The commit could not take the transaction: its write set cannot be
    /// laid out over the latest committed state, a page it needs could not
    /// be read, or it was begun from another database. The transaction is
    /// as it was, and what it does with its writes is its caller's: it can
    /// roll back, or try again.
    ///
    /// For a write with no transaction
    /// ([`Committer::write`](super::Committer::write)) it is also a batch
    /// that the write's own transaction refused. Nothing was written.
    Refused(TxnError),
    /// The log takes no group: an append to it failed, and the cut that
    /// has to be made before anything else is appended could not be made
    /// yet, for example because the disk is still full (CM9 items 4 and
    /// 5). The transaction is as it was, and its commit can be tried
    /// again.
    ///
    /// This is what a commit finds before it takes the transaction. An
    /// append of write-back that fails after that, before the commit has
    /// the lock on the counts of live values, fails the commit
    /// ([`Self::Failed`] with [`LogError::NoActiveFile`]).
    NotReady(FlushError),
    /// A commit outside the transaction wrote a key after the transaction
    /// first wrote it (CF2). This error is used for that and for nothing
    /// else (CF6).
    Conflict,
    /// The commit was not made: its group is not in the log, and nothing
    /// ever puts it there (CM9 (b)). Where it was the append that failed,
    /// the log takes no group until its failed bytes are cut off (CM9
    /// items 4 and 5), which is made before the next commit and not by
    /// this one ([`Self::NotReady`]).
    ///
    /// The session goes on, with one exception: a commit that found a
    /// group in the log that the session never published
    /// ([`Failure::VersionGap`]) has stopped it.
    Failed(Failure),
    /// **The commit has happened**: its group is in the log and its
    /// version is published. The sync that the sync policy asked for
    /// failed, so the version is not known to be durable (DU1).
    NotDurable {
        /// The commit, as [`Committed::Published`](super::Committed::Published)
        /// would have given it.
        published: Box<Published>,
        /// The error of the sync.
        source: FlushError,
    },
    /// The engine could not find out whether the group of the commit is in
    /// the log (CM9 item 6): the log could not settle an append it had
    /// taken back, or could not try to. Nothing was installed and nothing
    /// is published. The session takes no further commit (CM9 (d)), and
    /// the next open settles whether the commit happened: this is the one
    /// error after which a commit that was reported as an error can turn
    /// out to have happened.
    ///
    /// It is also what a commit ends with whose transaction was taken
    /// when the log said the same of an item that write-back had appended
    /// before the group, and took the group no more. A commit that finds
    /// the log so before it takes its transaction is refused with
    /// [`Self::Stopped`].
    Unsettled {
        /// The version the commit would have.
        version: CommitVersion,
        /// What the log said.
        source: LogError,
    },
    /// The group of the commit is in the log, and the commit could not be
    /// installed in memory, which no rule lets happen: a part of the store
    /// refused what the commit lock and the rebase make sure of. Nothing
    /// is published. The session takes no further commit, and the next
    /// open recovers the commit if its bytes survive. What the install had
    /// not come to is given up as by a rollback, so no copy stays private
    /// and no page id stays taken; the page versions it had installed stay
    /// in the buffer pool, where no reader asks for them.
    Halted {
        /// The version of the commit.
        version: CommitVersion,
        /// What refused.
        source: Failure,
    },
    /// The session takes no commit any more. The transaction is as it
    /// was: nothing of it will be committed in this session.
    Stopped(Stopped),
}

impl CommitError {
    /// The commit, if it was published although an error is reported: the
    /// case of [`Self::NotDurable`].
    pub fn published(&self) -> Option<&Published> {
        match self {
            Self::NotDurable { published, .. } => Some(published),
            _ => None,
        }
    }

    /// Whether the transaction is still open with its writes and its
    /// savepoints: the commit was refused before it took the transaction.
    /// After every other error the transaction is ended (CM6).
    pub const fn leaves_transaction_open(&self) -> bool {
        matches!(
            self,
            Self::ScopesOpen
                | Self::NotFinalized
                | Self::Refused(_)
                | Self::NotReady(_)
                | Self::Stopped(_)
        )
    }
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScopesOpen => f.write_str("a scope of the transaction is open"),
            Self::NotFinalized => f.write_str("the transaction has a pending mark left"),
            Self::Conflict => f.write_str(
                "a commit outside the transaction wrote a key after the transaction first wrote it",
            ),
            Self::Refused(source) => write!(f, "the commit was refused: {source}"),
            Self::NotReady(source) => write!(f, "the log takes no commit yet: {source}"),
            Self::Failed(source) => write!(f, "the commit was not made: {source}"),
            Self::NotDurable { published, source } => write!(
                f,
                "commit {} is published and not known to be durable: {source}",
                published.version
            ),
            Self::Unsettled { version, source } => write!(
                f,
                "whether commit {version} is in the log is not known, and the session takes no \
                 further commit: {source}"
            ),
            Self::Halted { version, source } => write!(
                f,
                "commit {version} is in the log and could not be installed, and the session \
                 takes no further commit: {source}"
            ),
            Self::Stopped(stopped) => write!(f, "the session takes no commit: {stopped}"),
        }
    }
}

impl Error for CommitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Refused(source) => Some(source),
            Self::Failed(source) | Self::Halted { source, .. } => Some(source),
            Self::NotReady(source) | Self::NotDurable { source, .. } => Some(source),
            Self::Unsettled { source, .. } => Some(source),
            Self::ScopesOpen | Self::NotFinalized | Self::Conflict | Self::Stopped(_) => None,
        }
    }
}
