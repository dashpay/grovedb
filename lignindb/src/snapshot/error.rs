//! The errors of snapshots and of version publication.

use std::{error::Error, fmt};

use lignin_format::CommitVersion;

/// Why a call through a snapshot was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    /// A snapshot is read-only (`transactions.md` RD1): a write or a commit
    /// through it is refused.
    ReadOnly,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnly => f.write_str("a snapshot is read-only"),
        }
    }
}

impl Error for SnapshotError {}

/// Why a version was not published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    /// The version is not the one after the latest published version. A
    /// commit takes that one, under the commit lock (CV1, CV5).
    NotNext {
        /// The latest published version.
        latest: CommitVersion,
        /// The version that was to be published.
        version: CommitVersion,
    },
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotNext { latest, version } => write!(
                f,
                "version {version} cannot be published: the latest version is {latest}"
            ),
        }
    }
}

impl Error for PublishError {}
