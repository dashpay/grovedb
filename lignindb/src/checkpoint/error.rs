//! Why a checkpoint was not made, or not deleted.

use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{wal::LogPlace, CommitVersion, FileId, FormatError};
use lignin_store::{flush::FlushError, segment::SegmentStoreError, wal::LogError};

use crate::commit::Stopped;

/// Why [`Checkpointer::create`](super::Checkpointer::create) made no
/// checkpoint, or [`delete`](super::delete) deleted none.
///
/// Whatever the error, the live database holds what it held: a creation
/// changes nothing of it but how far its log is durable. After
/// [`Self::Sync`] its log makes nothing more durable until the database is
/// opened again, as after any sync of the log that fails.
#[derive(Debug)]
pub enum CheckpointError {
    /// The target is there already, as a directory or as anything else.
    /// Nothing was touched (P-14 of `platform-requirements.md`).
    TargetExists {
        /// The target.
        target: PathBuf,
    },
    /// The target has no directory above it to be made in.
    NoParent {
        /// The target.
        target: PathBuf,
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
    /// The manifest could not be read: a replacement left it in doubt, and
    /// write-back has not settled it yet. The call can be made again.
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
    /// The first manifest of the checkpoint could not be made durable.
    Install(SegmentStoreError),
    /// The file layer failed.
    Io {
        /// What was being done.
        action: &'static str,
        /// To what.
        path: PathBuf,
        /// The error of the file layer.
        source: io::Error,
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
                "{} has no directory above it to make the checkpoint in",
                target.display()
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
            Self::Install(source) => write!(
                f,
                "the manifest of the checkpoint was not made durable: {source}"
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "could not {action} {}: {source}", path.display()),
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
            Self::Manifest(source) | Self::Install(source) => Some(source),
            Self::NotADatabase { source, .. } => Some(source),
            Self::Sync(source) => Some(source),
            Self::Log(source) => Some(source),
            Self::Format(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::TargetExists { .. }
            | Self::NoParent { .. }
            | Self::CarriedName { .. }
            | Self::Stopped(_)
            | Self::LogAhead { .. }
            | Self::NotDurable { .. }
            | Self::LogFile { .. }
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
