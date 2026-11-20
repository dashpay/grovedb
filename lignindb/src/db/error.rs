//! The errors of the facade.

use std::{
    error::Error,
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{CommitVersion, PageSize};
use lignin_store::{
    cleaner::CleanerError, flush::FlushError, recovery::RecoveryError, segment::SegmentStoreError,
};

use crate::commit::{CommitError, Failure, Published, Stopped};

/// Why a database was not opened ([`Db::open`](super::Db::open)).
///
/// The first five say that the directory is not one this build opens, and
/// each of them is found before anything is written: the directory is
/// byte for byte as it was.
#[derive(Debug)]
#[non_exhaustive]
pub enum OpenError {
    /// The directory holds no database, and
    /// [`Options::create_if_missing`](crate::Options::create_if_missing) is
    /// off. Nothing was created.
    NoDatabase {
        /// The directory.
        dir: PathBuf,
    },
    /// The directory was written by another storage engine (P-02): it has
    /// an entry that only that engine makes, or its `MANIFEST` is no
    /// LigninDB file. No second database is made beside the first.
    OtherEngine {
        /// The directory.
        dir: PathBuf,
        /// The engine, where the entry says which.
        engine: &'static str,
        /// The entry that says so.
        entry: OsString,
    },
    /// The database was written in a format version this build does not
    /// read (`format.md`, the file header).
    FormatVersion {
        /// The directory.
        dir: PathBuf,
        /// The format version in the header of its manifest.
        found: u16,
    },
    /// The directory has files of a database and no manifest, so what they
    /// hold cannot be known. No database is made over them.
    NoManifest {
        /// The directory.
        dir: PathBuf,
        /// One of the entries that are a database's.
        entry: OsString,
    },
    /// The database has pages of another size than the one that was asked
    /// for (`segment-manifest.md` M17).
    PageSize {
        /// The directory.
        dir: PathBuf,
        /// The size the database was made with.
        stored: PageSize,
        /// The size [`Options::page_size`](crate::Options::page_size) names.
        requested: PageSize,
    },
    /// Another handle has the database open, in this process or in
    /// another (P-04), or another open of the directory holds its lock
    /// file at this moment: an open holds it from before its look under
    /// the lock, also one that is then refused.
    Locked {
        /// The directory.
        dir: PathBuf,
    },
    /// The file layer failed.
    Io {
        /// What was being done.
        action: &'static str,
        /// The path it was done to.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
    /// The manifest could not be read, written or replaced.
    Manifest(SegmentStoreError),
    /// The store could not be recovered. [`RecoveryError::is_damage`] says
    /// whether the directory is damaged or a file operation failed.
    Recovery(Box<RecoveryError>),
    /// The first commit of a new database, which makes its keyspaces,
    /// failed. The next open makes it again, unless its group is whole in
    /// the log: then the recovery of that open has it.
    Create {
        /// The step that failed.
        step: &'static str,
        /// What it said.
        source: Box<dyn Error + Send + Sync>,
    },
    /// The record of where the keyspaces are could not be read, or is not
    /// what this build writes.
    Catalog {
        /// What is wrong with it.
        what: &'static str,
        /// The error of the read, if a read failed.
        source: Option<Box<dyn Error + Send + Sync>>,
    },
    /// A thread of the database's background work could not be started.
    Thread(io::Error),
    /// The directory holds `MANIFEST.deleting`: the removal of a database
    /// or of a checkpoint was begun there and cut short
    /// (`lignindb::checkpoint::delete`). Nothing opens what is left and no
    /// database is made in it, also where a `MANIFEST` is beside the
    /// mark. The same call finishes the removal, and the path is a place
    /// for a new database then. Whoever wants a database that is beside
    /// the mark removes the mark.
    Deleting {
        /// The directory.
        dir: PathBuf,
    },
    /// A look of this open found a database of this engine in the
    /// directory, and there was no lock file to lock, before that look
    /// and after it. A database has its `LOCK` from the open that made
    /// it: the file is missing where a copy left it out, or where someone
    /// removed it. The open makes none where it found a database. With an
    /// empty file named `LOCK` put there the database opens; whoever puts
    /// it there has to know that no handle has the database open, since a
    /// handle goes on holding a lock file whose name was removed.
    NoLockFile {
        /// The directory.
        dir: PathBuf,
    },
    /// What is at the path is not what this open saw, or not what it
    /// locked: someone changed it while it was being opened. Either a
    /// look of this open found a database and a later look, with the lock
    /// held or without, found none, in a directory that was removed, in
    /// another that was put in its place, or in the same one with the
    /// database's files taken out: no database is made where one was
    /// seen, whatever [`Options::create_if_missing`] says. Or the lock
    /// file that this open took, with none made, is not the one at the
    /// path any more: another directory was put at the path, or the
    /// directory is the same and its lock file was removed or another put
    /// there. The lock that is held is then not the lock of what is at
    /// the path, whatever that holds. No file was made and nothing was
    /// written.
    ///
    /// [`Options::create_if_missing`]: crate::Options::create_if_missing
    Replaced {
        /// The directory.
        dir: PathBuf,
    },
    /// The directory is refused under a lock whose file this open asked
    /// to be made. A database was to be made in it: it was looked at and
    /// found to hold nothing of any engine and no lock file, and under the
    /// lock it is one that is refused, for the reason in `found`. Someone
    /// changed it between the two: another process wrote into it, another
    /// open made a database in it that this open's options do not fit, or
    /// another directory was put in its place, or the lock file that was
    /// made was removed or replaced. Because the lock was asked
    /// for with its file to be made, the directory may have gained an
    /// empty `LOCK` ([`Self::left_untouched`] is false), which is left
    /// where it is, because another opener may hold it. Nothing else was
    /// written.
    ///
    /// A directory that is written into while it is opened is not refused
    /// for that alone: if another open made a database there meanwhile,
    /// or someone put a database of this engine there, and this open's
    /// options fit it, it is opened. And what a directory
    /// is refused for under a lock that was taken through a lock file
    /// which was there, with none made, is reported as itself.
    Changed {
        /// The directory.
        dir: PathBuf,
        /// What the directory is refused for now, as text: the refusal is
        /// one that would say the directory was left untouched, which is
        /// not known of this one.
        found: String,
    },
}

impl OpenError {
    pub(super) fn io<'a>(
        action: &'static str,
        path: &'a Path,
    ) -> impl FnOnce(io::Error) -> Self + use<'a> {
        move |source| Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(super) fn create<E: Error + Send + Sync + 'static>(
        step: &'static str,
    ) -> impl FnOnce(E) -> Self {
        move |source| Self::Create {
            step,
            source: Box::new(source),
        }
    }

    pub(super) fn catalog<E: Error + Send + Sync + 'static>(
        what: &'static str,
    ) -> impl FnOnce(E) -> Self {
        move |source| Self::Catalog {
            what,
            source: Some(Box::new(source)),
        }
    }

    /// Whether the directory is not one this build opens, and was left
    /// byte for byte as it was: it holds no database and none was to be
    /// made, it is another engine's, its format version is unknown, it has
    /// lost its manifest or its lock file, its removal was begun, its
    /// pages are of another size,
    /// or it was removed or put in the place of another while it was
    /// being opened ([`Self::Replaced`]). Each of these was found with no
    /// file made: before the lock, or under the lock of a lock file that
    /// was there. That holds for a directory that nobody else changes
    /// while it is being opened, and for one that is put in the place of
    /// a database during the open, up to the open's last question before
    /// its first write (the documentation of [`crate::db`], "Where the
    /// guarantee ends"). A directory with no lock file into which another
    /// process writes while a database is being made in it is
    /// [`Self::Changed`].
    pub const fn left_untouched(&self) -> bool {
        matches!(
            self,
            Self::NoDatabase { .. }
                | Self::OtherEngine { .. }
                | Self::FormatVersion { .. }
                | Self::NoManifest { .. }
                | Self::NoLockFile { .. }
                | Self::Deleting { .. }
                | Self::Replaced { .. }
                | Self::PageSize { .. }
        )
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDatabase { dir } => write!(f, "{} holds no database", dir.display()),
            Self::OtherEngine { dir, engine, entry } => write!(
                f,
                "{} was written by another storage engine ({engine}: {})",
                dir.display(),
                Path::new(entry).display()
            ),
            Self::FormatVersion { dir, found } => write!(
                f,
                "{} is a LigninDB database of format version {found}, which this build does \
                 not read",
                dir.display()
            ),
            Self::NoManifest { dir, entry } => write!(
                f,
                "{} has files of a database ({}) and no manifest",
                dir.display(),
                Path::new(entry).display()
            ),
            Self::Replaced { dir } => write!(
                f,
                "{} was changed while it was being opened: the database that was seen there \
                 is gone, or the lock file that was taken is not the one at the path any \
                 more. No database is opened or made there",
                dir.display()
            ),
            Self::Deleting { dir } => write!(
                f,
                "{} holds MANIFEST.deleting: the removal of a database was begun there and not \
                 finished, and no database is opened or made in what is left",
                dir.display()
            ),
            Self::NoLockFile { dir } => write!(
                f,
                "{} holds a database and no LOCK file (a copy left it out, or it was \
                 removed), and the file is made in no directory that holds a database",
                dir.display()
            ),
            Self::PageSize {
                dir,
                stored,
                requested,
            } => write!(
                f,
                "{} has pages of {} bytes, and {} were asked for",
                dir.display(),
                stored.get(),
                requested.get()
            ),
            Self::Locked { dir } => write!(f, "{} is open in another handle", dir.display()),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
            Self::Manifest(source) => write!(f, "manifest: {source}"),
            Self::Recovery(source) => write!(f, "recovery: {source}"),
            Self::Create { step, source } => {
                write!(f, "the first commit of the database ({step}): {source}")
            }
            Self::Catalog { what, source } => match source {
                Some(source) => write!(f, "the record of the keyspaces: {what}: {source}"),
                None => write!(f, "the record of the keyspaces: {what}"),
            },
            Self::Thread(source) => write!(f, "cannot start the background work: {source}"),
            Self::Changed { dir, found } => write!(
                f,
                "{} was changed while it was being opened, and a lock file may have been made \
                 in it: {found}",
                dir.display()
            ),
        }
    }
}

impl Error for OpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Thread(source) => Some(source),
            Self::Manifest(source) => Some(source),
            Self::Recovery(source) => Some(&**source),
            Self::Create { source, .. } => Some(&**source),
            Self::Catalog { source, .. } => source.as_deref().map(|source| source as _),
            Self::NoLockFile { .. }
            | Self::Deleting { .. }
            | Self::Replaced { .. }
            | Self::Changed { .. } => None,
            Self::NoDatabase { .. }
            | Self::OtherEngine { .. }
            | Self::FormatVersion { .. }
            | Self::NoManifest { .. }
            | Self::PageSize { .. }
            | Self::Locked { .. } => None,
        }
    }
}

impl From<RecoveryError> for OpenError {
    fn from(error: RecoveryError) -> Self {
        Self::Recovery(Box::new(error))
    }
}

/// Why a session takes no write any more, and has to be ended: the
/// database is closed and opened again, which settles what the log holds
/// (DU2).
///
/// Reads go on, and so does a rollback. What was published stays
/// published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reopen {
    /// A sync of the log failed: of a commit under a sync policy that asks
    /// for one, of a [`flush`](super::Db::flush), or of the background
    /// work, which syncs the log for a table checkpoint and when it cuts a
    /// failed append off. The log makes nothing durable any more in this
    /// session (`lignin_store::wal`), so a write that was taken now could
    /// be lost with no error to say so.
    NotDurable {
        /// The latest version when the failed sync was recorded. It is
        /// published, and not known to be durable.
        version: CommitVersion,
        /// What the sync said.
        reason: String,
    },
    /// A sync of a segment failed in this session, in a turn of the
    /// background work. The blocks that were not durable then cannot be
    /// made durable any more, so the store installs no table checkpoint
    /// until it is opened again (`lignin_store::flush`, "When something
    /// fails"): the log would keep every commit from here on, and grow for
    /// as long as the session took writes.
    ///
    /// Nothing that was published is lost by it. A
    /// [`flush`](super::Db::flush) still makes the log durable, and the
    /// next open replays it.
    SegmentsNotDurable,
    /// The commit path stopped the session
    /// ([`Stopped`]): whether a commit is in the log
    /// could not be found out, or a commit is in the log and could not be
    /// installed.
    Stopped(Stopped),
}

impl From<Stopped> for Reopen {
    fn from(stopped: Stopped) -> Self {
        match stopped {
            Stopped::NotDurable { version, reason } => Self::NotDurable { version, reason },
            stopped => Self::Stopped(stopped),
        }
    }
}

impl fmt::Display for Reopen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the session takes no write, and the database has to be opened again: ")?;
        match self {
            Self::NotDurable { version, reason } => write!(
                f,
                "a sync of the log failed with version {version} published ({reason})"
            ),
            Self::SegmentsNotDurable => f.write_str(
                "a sync of a segment failed, and no table checkpoint is installed until then",
            ),
            Self::Stopped(stopped) => write!(f, "{stopped}"),
        }
    }
}

impl Error for Reopen {}

/// How a write through the facade ended, if not with its commit published
/// and, where the sync policy asked for it, durable: a commit of a write
/// transaction, a write with no transaction, and a wipe.
#[derive(Debug)]
pub enum WriteError {
    /// The commit ended as the commit path says ([`CommitError`], with the
    /// table of what each outcome leaves).
    Commit(CommitError),
    /// The session takes no write any more, and nothing was tried. A
    /// transaction is as it was, and nothing of it will be committed in
    /// this session.
    Reopen(Reopen),
}

impl WriteError {
    /// Whether the commit failed with a conflict: a commit outside the
    /// transaction wrote a key after the transaction first wrote it
    /// (CF2), or the transaction kept the committed value of a Merk node
    /// and a commit outside wrote the node after the transaction read it,
    /// or the engine cannot tell whether one did ([`crate::txn`], "A kept
    /// value and the read it is of").
    ///
    /// It is true as well for the refusal of a write that says commits
    /// outside took away a node whose value the write keeps
    /// ([`BatchError::ValueTaken`](crate::txn::BatchError::ValueTaken)).
    /// A scope's write answers that itself, as a
    /// [`TxnError`](crate::txn::TxnError), which says the same of it
    /// ([`TxnError::is_conflict`](crate::txn::TxnError::is_conflict)).
    ///
    /// These are the outcomes for which the caller makes its reads and its
    /// writes again.
    pub const fn is_conflict(&self) -> bool {
        match self {
            Self::Commit(CommitError::Conflict) => true,
            Self::Commit(CommitError::Refused(refused)) => refused.is_conflict(),
            _ => false,
        }
    }

    /// The commit, if it was published although an error is reported
    /// ([`CommitError::NotDurable`]).
    pub fn published(&self) -> Option<&Published> {
        match self {
            Self::Commit(error) => error.published(),
            Self::Reopen(_) => None,
        }
    }

    /// Whether the transaction is still open with its writes and its
    /// savepoints ([`CommitError::leaves_transaction_open`]).
    pub const fn leaves_transaction_open(&self) -> bool {
        match self {
            Self::Commit(error) => error.leaves_transaction_open(),
            Self::Reopen(_) => true,
        }
    }

    /// Why the database has to be opened again, if this error says so: the
    /// session took no write, or this commit is the one that ended it.
    pub fn reopen(&self) -> Option<Reopen> {
        match self {
            Self::Reopen(reopen) => Some(reopen.clone()),
            Self::Commit(CommitError::NotDurable { published, source }) => {
                Some(Reopen::NotDurable {
                    version: published.version,
                    reason: source.to_string(),
                })
            }
            Self::Commit(CommitError::Stopped(stopped)) => Some(stopped.clone().into()),
            Self::Commit(CommitError::Unsettled { version, source }) => {
                Some(Reopen::Stopped(Stopped::Unsettled {
                    version: *version,
                    reason: source.to_string(),
                }))
            }
            Self::Commit(CommitError::Halted { version, source }) => {
                Some(Reopen::Stopped(Stopped::Halted {
                    version: *version,
                    reason: source.to_string(),
                }))
            }
            // A commit that found a group in the log which the session
            // never published has stopped the session as well.
            Self::Commit(CommitError::Failed(gap @ Failure::VersionGap { latest, .. })) => {
                Some(Reopen::Stopped(Stopped::Halted {
                    version: latest.next().unwrap_or(*latest),
                    reason: gap.to_string(),
                }))
            }
            Self::Commit(_) => None,
        }
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit(error) => write!(f, "{error}"),
            Self::Reopen(reopen) => write!(f, "{reopen}"),
        }
    }
}

impl Error for WriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Commit(error) => Some(error),
            Self::Reopen(reopen) => Some(reopen),
        }
    }
}

impl From<CommitError> for WriteError {
    fn from(error: CommitError) -> Self {
        Self::Commit(error)
    }
}

/// Why a call of the facade that is no write failed: a flush, and a close.
#[derive(Debug)]
#[non_exhaustive]
pub enum DbError {
    /// Write-back failed: a sync of the log, a table checkpoint, the
    /// closing of the session's files. A close answers with this, and has
    /// finished nothing in the directory, where a replacement of the
    /// manifest had been left in doubt and the manifest could not be
    /// opened again.
    Flush(FlushError),
    /// The cleaner could not close the files it was moving blocks and
    /// values into. What it had moved stands, and the rest of the close
    /// was made.
    Clean(CleanerError),
    /// A thread of the background work panicked. Its work is not done, and
    /// what it held is as a crash would leave it: the next open recovers.
    Panicked(&'static str),
    /// The directory was removed while the database was open, or holds
    /// another database now, or the close could not find out that it is
    /// this database's still. The close finished nothing in it and made
    /// nothing durable (P-18).
    Removed {
        /// The directory the database was opened in.
        dir: PathBuf,
    },
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flush(source) => write!(f, "write-back: {source}"),
            Self::Clean(source) => write!(f, "the cleaner: {source}"),
            Self::Panicked(thread) => write!(f, "the {thread} thread panicked"),
            Self::Removed { dir } => write!(
                f,
                "{} was not found to hold this database any more: it was removed or replaced \
                 while the database was open, and the close finished nothing in it",
                dir.display()
            ),
        }
    }
}

impl Error for DbError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Flush(source) => Some(source),
            Self::Clean(source) => Some(source),
            Self::Panicked(_) | Self::Removed { .. } => None,
        }
    }
}

impl From<FlushError> for DbError {
    fn from(error: FlushError) -> Self {
        Self::Flush(error)
    }
}
