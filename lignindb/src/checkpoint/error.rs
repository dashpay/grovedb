//! Why a checkpoint was not made, or not deleted.

use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{wal::LogPlace, CommitVersion, FileId, FormatError};
use lignin_store::{flush::FlushError, segment::SegmentStoreError, wal::LogError};

use crate::commit::Stopped;

/// Why [`Checkpointer::create`](super::Checkpointer::create) or
/// [`delete`](super::delete) failed. Each of the two says what is left
/// then.
///
/// Whatever the error, the live database holds what it held. What a
/// creation does to it is write-back's work before a directory is made,
/// a catch-up and the settling of its manifest, and a log that is durable
/// further (the [module documentation](super)). After [`Self::Sync`] its
/// log makes nothing more durable until the database is opened again, as
/// after any sync of the log that fails.
#[derive(Debug)]
pub enum CheckpointError {
    /// The target is there already, as a directory or as anything else.
    /// The creation renamed nothing onto it and removed nothing of it
    /// (P-14 of `platform-requirements.md`).
    TargetExists {
        /// The target.
        target: PathBuf,
    },
    /// The target has no directory above it to be made in, or no name of
    /// its own there.
    NoParent {
        /// The target.
        target: PathBuf,
    },
    /// The target has a name that a creation builds under
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)), in whatever case of
    /// its letters, so no checkpoint is made at it. Nothing was touched.
    ReservedName {
        /// The target.
        target: PathBuf,
    },
    /// The directory is one that a creation builds a checkpoint in
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)), and its lock is
    /// held, as by a creation that is building in it now. Nothing in it
    /// was removed.
    Creating {
        /// The directory that is being built in.
        dir: PathBuf,
    },
    /// A look at the path of a directory that checkpoints are built in
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)) did not find what
    /// the call looked for. Under a lock it has just taken, a creation,
    /// or a deletion of what a creation left, looks for a directory of
    /// its own in the directory above, with the lock file it holds. Right
    /// before the rename, a creation looks for that directory holding
    /// what the creation put into it and nothing else. The call does not
    /// know why it was not found so. Nothing was given the name of the
    /// target.
    Replaced {
        /// The path of the directory.
        dir: PathBuf,
    },
    /// A directory that checkpoints are built in
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)) has no lock file of
    /// its own to take the lock in. It lists, under the name of its lock
    /// file, something that is not a file, such as a link or a directory;
    /// or it lists nothing under that name, and a new lock file could not
    /// be made there because the file system finds the name taken all the
    /// same. Its lock is not taken, so a creation does not build in it
    /// and [`delete`](super::delete) does not remove it. Nothing of it
    /// was touched.
    NotALock {
        /// The path of the lock file.
        path: PathBuf,
    },
    /// A file that the checkpoint was to carry beside the store's own is
    /// not named by one plain name inside the database directory.
    CarriedName {
        /// The name that was given.
        name: String,
    },
    /// The session takes no commit any more, so what its log holds is not
    /// known to be what it has published. The database is opened again
    /// first.
    Stopped(Stopped),
    /// The log holds a commit that the session has not published, as after
    /// a commit that panicked between its append and its publication. A
    /// checkpoint would land on a version that no reader of this session
    /// has seen.
    LogAhead {
        /// The version of the last group of the log.
        log: CommitVersion,
        /// The latest published version.
        latest: CommitVersion,
    },
    /// Write-back, which the creation asks to settle a manifest that a
    /// replacement left in doubt before it makes a directory
    /// (`lignin_store::flush::WriteBack::settle_manifest`), answered this
    /// error. No directory was made for the checkpoint. The call can be
    /// made again. Where the manifest could not be opened again, it is in
    /// doubt still; where it was opened and a store that was told of it
    /// answered the error, that doubt is settled.
    Settle(FlushError),
    /// The manifest could not be read at the sampling step: a replacement
    /// left it in doubt after the creation had asked for it to be
    /// settled. The call can be made again.
    Manifest(SegmentStoreError),
    /// The log could not be made durable through the version of the
    /// checkpoint. A sync of the log that fails stays failed for the
    /// session (`lignin_store::wal`): no commit of it becomes durable, and
    /// no checkpoint is made, before the database is opened again.
    Sync(FlushError),
    /// The log refused to be held, or to say what its files are.
    Log(LogError),
    /// The log's sync returned without covering the place the checkpoint
    /// ends at.
    NotDurable {
        /// Where the checkpoint's log ends.
        end: LogPlace,
        /// How far the log is durable.
        durable: Option<LogPlace>,
    },
    /// A log file before the end of the checkpoint's log is not what the
    /// log has to make of it by then: closed and durable to its end, or,
    /// for the file of the end itself, named and durable through the end.
    LogFile {
        /// The file.
        file: FileId,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The checkpoint's manifest could not be written as the format has
    /// it.
    Format(FormatError),
    /// The file layer failed.
    Io {
        /// What was being done.
        action: &'static str,
        /// To what.
        path: PathBuf,
        /// The error of the file layer.
        source: io::Error,
    },
    /// The directory above the path does not list it, under the name
    /// given, as a directory of its own: it lists nothing of that name,
    /// or a file, or something else, such as a link to a directory, which
    /// is not followed. [`delete`](super::delete) was given the path, and
    /// where nothing is there at all, nothing that a creation left was
    /// found beside it either; or it is the name a creation was to build
    /// under. Nothing was removed.
    NotADirectory {
        /// The path.
        dir: PathBuf,
    },
    /// [`delete`](super::delete): the directory has no manifest of this
    /// engine, so it is no checkpoint and no database of it. Nothing was
    /// removed.
    NotADatabase {
        /// The directory.
        dir: PathBuf,
        /// Why its manifest could not be read.
        source: SegmentStoreError,
    },
    /// [`delete`](super::delete): a handle has the directory open. Nothing
    /// was removed.
    Open {
        /// The directory.
        dir: PathBuf,
    },
}

impl CheckpointError {
    pub(super) fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self + use<> {
        let path = path.to_path_buf();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let place =
            |place: &LogPlace| format!("offset {} of log file {}", place.offset(), place.file());
        match self {
            Self::TargetExists { target } => write!(
                f,
                "{} is there already, and a checkpoint is made in a new directory",
                target.display()
            ),
            Self::NoParent { target } => write!(
                f,
                "{} has no directory above it to make the checkpoint in, or no name there",
                target.display()
            ),
            Self::ReservedName { target } => write!(
                f,
                "{} has a name that checkpoints are built under, and none is made there",
                target.display()
            ),
            Self::Creating { dir } => write!(
                f,
                "{} is locked, as a directory that a checkpoint is being built in, and nothing \
                 in it was removed",
                dir.display()
            ),
            Self::Replaced { dir } => write!(
                f,
                "{} was not found to be the directory whose lock is held, with what was put \
                 into it, and nothing was given the name of the target",
                dir.display()
            ),
            Self::NotALock { path } => write!(
                f,
                "{} is no lock file of its directory's own, and the lock of the directory was \
                 not taken",
                path.display()
            ),
            Self::CarriedName { name } => write!(
                f,
                "{name:?} is not the plain name of a file in the database directory"
            ),
            Self::Stopped(stopped) => write!(
                f,
                "the session takes no commit, and no checkpoint is made of it: {stopped}"
            ),
            Self::LogAhead { log, latest } => write!(
                f,
                "the log holds commit {log}, and the latest published version is {latest}"
            ),
            Self::Settle(source) => write!(
                f,
                "the manifest was in doubt, and settling it answered an error: {source}"
            ),
            Self::Manifest(source) => write!(f, "the manifest could not be read: {source}"),
            Self::Sync(source) => write!(f, "the log could not be made durable: {source}"),
            Self::Log(source) => write!(f, "the log: {source}"),
            Self::NotDurable { end, durable } => write!(
                f,
                "the checkpoint's log ends at {}, and the log is durable {}",
                place(end),
                match durable {
                    Some(durable) => format!("up to {}", place(durable)),
                    None => "nowhere".to_string(),
                }
            ),
            Self::LogFile { file, reason } => write!(f, "log file {file} {reason}"),
            Self::Format(source) => write!(
                f,
                "the manifest of the checkpoint could not be written: {source}"
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "could not {action} {}: {source}", path.display()),
            Self::NotADirectory { dir } => write!(
                f,
                "{} is not a directory of its own, and nothing was removed",
                dir.display()
            ),
            Self::NotADatabase { dir, source } => write!(
                f,
                "{} is no checkpoint, and nothing of it was removed: {source}",
                dir.display()
            ),
            Self::Open { dir } => write!(
                f,
                "{} is open, and nothing of it was removed",
                dir.display()
            ),
        }
    }
}

impl Error for CheckpointError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Settle(source) => Some(source),
            Self::Manifest(source) => Some(source),
            Self::NotADatabase { source, .. } => Some(source),
            Self::Sync(source) => Some(source),
            Self::Log(source) => Some(source),
            Self::Format(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::TargetExists { .. }
            | Self::NoParent { .. }
            | Self::ReservedName { .. }
            | Self::Creating { .. }
            | Self::Replaced { .. }
            | Self::NotALock { .. }
            | Self::CarriedName { .. }
            | Self::Stopped(_)
            | Self::LogAhead { .. }
            | Self::NotDurable { .. }
            | Self::LogFile { .. }
            | Self::NotADirectory { .. }
            | Self::Open { .. } => None,
        }
    }
}

impl From<LogError> for CheckpointError {
    fn from(source: LogError) -> Self {
        Self::Log(source)
    }
}

impl From<FormatError> for CheckpointError {
    fn from(source: FormatError) -> Self {
        Self::Format(source)
    }
}
