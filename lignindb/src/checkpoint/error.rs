//! Why a checkpoint was not made, or not deleted.

use std::{
    error::Error,
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{wal::LogPlace, CommitVersion, FileId, FormatError};
use lignin_store::{flush::FlushError, segment::SegmentStoreError, wal::LogError};

use crate::commit::Stopped;

/// Why [`Checkpointer::create`](super::Checkpointer::create),
/// [`delete`](super::delete) or [`finish`](super::finish) failed. Each of
/// the three says what is left then, and what is said of
/// [`delete`](super::delete) under an error holds for
/// [`finish`](super::finish) where it does what a deletion does.
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
    /// its ASCII letters: the target is refused. Nothing was touched.
    ReservedName {
        /// The target.
        target: PathBuf,
    },
    /// The directory is the staging directory of a creation
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)), and its lock is
    /// held, as by a creation that is building in it now. The call did
    /// nothing more once it found the lock held.
    Creating {
        /// The directory that is being built in.
        dir: PathBuf,
    },
    /// A look at the path of a creation's staging directory
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)), of the directory
    /// a checkpoint is built in inside it
    /// ([`BUILDING_DIR_NAME`](super::BUILDING_DIR_NAME)), or of a
    /// database directory that [`delete`](super::delete) or
    /// [`finish`](super::finish) has locked, did not find the directory
    /// whose lock is held, or not as
    /// the call looked for it. Under a lock it has just taken, and
    /// again before it removes what was built there, a creation, or a
    /// deletion of what a creation left, looks for a directory of its own
    /// in the directory above, with the lock file it holds. Right before
    /// the rename, a creation looks for both directories, the one that
    /// was built in holding what the creation put into it and nothing
    /// else. The call does not know why it was not found so. Nothing was
    /// given the name of the target, and nothing was removed from what
    /// was not found.
    Replaced {
        /// The path of the directory.
        dir: PathBuf,
    },
    /// A staging directory
    /// ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)) did not list a file
    /// under the name of its lock file, byte for byte, and the call made
    /// none there: it lists nothing under that name, and the call found,
    /// twice, that the name leads to something all the same, as it can
    /// where a file system takes several spellings for one name. The lock
    /// of the directory was not taken, and the call went no further: a
    /// creation does not build in such a directory, and
    /// [`delete`](super::delete) does not go on to remove it. One that
    /// lists something else than a file under that name is refused
    /// before, as [`Self::NotStaging`].
    ///
    /// Or [`delete`](super::delete) or [`finish`](super::finish) was to
    /// take the lock of a database directory, and that directory lists
    /// something else than a file under the name of its lock file, such
    /// as a link: the lock was not taken, and nothing was removed.
    NotALock {
        /// The path of the lock file.
        path: PathBuf,
    },
    /// A directory under a name that creations give their staging
    /// directories ([`CREATING_SUFFIX`](super::CREATING_SUFFIX)) lists an
    /// entry that no creation puts there: one of another name than the
    /// lock file's and
    /// [`BUILDING_DIR_NAME`](super::BUILDING_DIR_NAME), one of these that
    /// is of another kind, or a lock file that holds a byte. So it is not
    /// what a creation left, whatever it is: a creation at the target
    /// beside it does not build in it, and [`delete`](super::delete),
    /// given the directory itself, does not remove it. Nothing in it was
    /// removed. Where the first look found it so, no lock was taken in it
    /// and no lock file was made in it either; one that came under the
    /// name, or came to list the entry, after that look was refused by a
    /// look under the lock (the [module documentation](super), "What the
    /// looks do not tell"). A [`delete`](super::delete) of the target
    /// beside it does not fail with this: it passes the directory over.
    ///
    /// What a creation of a build before this rule left there, with a
    /// part of a checkpoint directly in it, is refused in the same way:
    /// whoever knows it for that removes the directory.
    NotStaging {
        /// The directory.
        dir: PathBuf,
        /// The name of the first entry that no creation puts there.
        found: OsString,
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
    /// found beside it either: no directory, or one that no creation
    /// left, which is as it was. Or it is the name of the staging
    /// directory a creation was to make. Nothing was removed.
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
                "{} is locked, as a directory that a checkpoint is being built in",
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
                "{} is not listed as a file, and none was made there: the lock of its directory \
                 was not taken",
                path.display()
            ),
            Self::NotStaging { dir, found } => write!(
                f,
                "{} holds {found:?}, which no creation of a checkpoint puts into its staging \
                 directory: the directory is not taken for what a creation left, and nothing \
                 in it was removed",
                dir.display()
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
            | Self::NotStaging { .. }
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
