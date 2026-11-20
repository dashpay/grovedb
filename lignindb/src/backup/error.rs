//! Why a database directory was not copied.

use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{FileId, FileKind};
use lignin_store::{segment::SegmentStoreError, wal::LogError};

/// Why [`Backup::copy`](super::Backup::copy) made no copy.
///
/// Whatever the error, the source is as it was: a copy reads it and takes
/// its lock, and writes nothing to it. What the copy had made of the
/// target is taken away again as far as the file layer lets it, and what
/// is left has no manifest unless every file that manifest names is there
/// ("What a copy that fails leaves" in the
/// [module documentation](super)).
#[derive(Debug)]
#[non_exhaustive]
pub enum BackupError {
    /// The target is there already, as a directory or as anything else.
    /// Nothing was touched.
    TargetExists {
        /// The target.
        target: PathBuf,
    },
    /// The target has no directory above it to be made in.
    NoParent {
        /// The target.
        target: PathBuf,
    },
    /// The path of the target is inside the path of the source: within
    /// one file layer the copy would be made in the directory it reads.
    /// The paths are compared as they are given, whatever the file layers.
    /// Nothing was touched.
    TargetInSource {
        /// The source.
        dir: PathBuf,
        /// The target.
        target: PathBuf,
    },
    /// The source holds no lock file, so nothing keeps a handle from
    /// opening it while it is copied. A database has its `LOCK` from the
    /// open that made it, and a checkpoint from its creation: the file is
    /// missing where a copy left it out or someone removed it, or the
    /// source is no database directory at all. Nothing was made.
    NoLockFile {
        /// The source.
        dir: PathBuf,
    },
    /// A handle has the source open. A copy is made of a directory that
    /// nobody writes: a checkpoint that was not opened, or a database that
    /// is closed. Nothing was made.
    Open {
        /// The source.
        dir: PathBuf,
    },
    /// The source has no manifest of this engine: it is no database, or a
    /// creation or a deletion left it without one. Nothing was made.
    NotADatabase {
        /// The source.
        dir: PathBuf,
        /// Why its manifest could not be read.
        source: SegmentStoreError,
    },
    /// A file that the source's manifest names is not there, or holds
    /// fewer bytes than the manifest vouches for. The source is damaged.
    Short {
        /// The file.
        path: PathBuf,
        /// The bytes the manifest vouches for.
        valid_len: u64,
        /// The bytes the file has, or `None` if it is not there.
        len: Option<u64>,
    },
    /// The manifest of the source names a file of a kind that no database
    /// directory holds under a number.
    Kind {
        /// The kind.
        kind: FileKind,
        /// The file.
        file: FileId,
    },
    /// A file that the copy was to carry beside the store's own is not
    /// named by one plain name of a file inside the source.
    CarriedName {
        /// The name that was given.
        name: String,
    },
    /// The log of the copy does not read to an end: the source's log is
    /// damaged below what its manifest vouches for, or the copy is not
    /// what was read.
    Log(LogError),
    /// The manifest of the copy could not be made durable.
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
}

impl BackupError {
    pub(super) fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self + use<> {
        let path = path.to_path_buf();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetExists { target } => {
                write!(f, "{} is there already", target.display())
            }
            Self::NoParent { target } => write!(
                f,
                "{} has no directory above it to be made in",
                target.display()
            ),
            Self::TargetInSource { dir, target } => write!(
                f,
                "{} is inside {}, the directory that is to be copied",
                target.display(),
                dir.display()
            ),
            Self::NoLockFile { dir } => write!(
                f,
                "{} holds no LOCK file, so it is no database directory, or one that a handle \
                 could open while it is copied",
                dir.display()
            ),
            Self::Open { dir } => write!(
                f,
                "{} is open in a handle, and a copy is made of a directory that nobody writes",
                dir.display()
            ),
            Self::NotADatabase { dir, source } => {
                write!(f, "{} is no database: {source}", dir.display())
            }
            Self::Short {
                path,
                valid_len,
                len: Some(len),
            } => write!(
                f,
                "{} holds {len} bytes, and the manifest vouches for {valid_len}",
                path.display()
            ),
            Self::Short {
                path,
                valid_len,
                len: None,
            } => write!(
                f,
                "{} is not there, and the manifest vouches for {valid_len} bytes of it",
                path.display()
            ),
            Self::Kind { kind, file } => write!(
                f,
                "the manifest names file {file} of the kind {kind:?}, which no directory of a \
                 database holds"
            ),
            Self::CarriedName { name } => write!(
                f,
                "{name:?} is not the plain name of a file in the source that a copy can carry"
            ),
            Self::Log(source) => write!(f, "the log of the copy: {source}"),
            Self::Install(source) => write!(f, "the manifest of the copy: {source}"),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
        }
    }
}

impl Error for BackupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotADatabase { source, .. } | Self::Install(source) => Some(source),
            Self::Log(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::TargetExists { .. }
            | Self::NoParent { .. }
            | Self::TargetInSource { .. }
            | Self::NoLockFile { .. }
            | Self::Open { .. }
            | Self::Short { .. }
            | Self::Kind { .. }
            | Self::CarriedName { .. } => None,
        }
    }
}
