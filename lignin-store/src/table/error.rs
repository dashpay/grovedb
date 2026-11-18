//! The errors the page table returns.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{CommitVersion, FileId, FormatError, FormatVersion, PageId};

use crate::segment::{BlockLocation, SegmentStoreError};

/// Why a change to one page's chain was refused. The chain is as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// The location is not one a block can have: its length is not the
    /// length of a block, or it starts before the first block of a segment.
    InvalidLocation {
        /// The location offered.
        location: BlockLocation,
    },
    /// The link is older than the newest link of the chain.
    OutOfOrder {
        /// The version of the chain's newest link.
        newest: CommitVersion,
        /// The version of the link offered.
        version: CommitVersion,
    },
    /// A delta was offered for a page that has no link, or whose newest
    /// link says it was freed.
    DeltaWithoutBase,
    /// The page's newest link already says it was freed.
    AlreadyFree,
    /// No link of the chain points to this location: it was never there, or
    /// it has been trimmed.
    NotInChain {
        /// The location looked for.
        location: BlockLocation,
    },
    /// The table holds as many links as it can.
    TableFull,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLocation { location } => write!(
                f,
                "{} bytes at offset {} of segment {} cannot be a block",
                location.len, location.offset, location.file
            ),
            Self::OutOfOrder { newest, version } => write!(
                f,
                "a link of version {version} cannot follow the chain's newest, of version {newest}"
            ),
            Self::DeltaWithoutBase => write!(f, "a delta needs a base image beneath it"),
            Self::AlreadyFree => write!(f, "the page is already freed"),
            Self::NotInChain { location } => write!(
                f,
                "no link of the chain points to offset {} of segment {}",
                location.offset, location.file
            ),
            Self::TableFull => write!(f, "the page table holds as many links as it can"),
        }
    }
}

impl std::error::Error for LinkError {}

/// Why an operation on a page-table checkpoint failed.
#[derive(Debug)]
pub enum PageTableError {
    /// The file layer reported an error.
    Io {
        /// What was being done.
        op: &'static str,
        /// The file or directory it was done to.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The bytes of a checkpoint are not what its format allows.
    Format {
        /// The file.
        path: PathBuf,
        /// The file offset of the record that was being read or written.
        offset: u64,
        /// What is wrong with the bytes.
        source: FormatError,
    },
    /// The manifest or the segment store failed.
    Store(SegmentStoreError),
    /// A checkpoint holds a link its page's chain cannot take.
    Link {
        /// The page.
        page: PageId,
        /// Why the link was refused.
        source: LinkError,
    },
    /// A checkpoint is shorter than the valid length its manifest gives it.
    FileTooShort {
        /// The file.
        path: PathBuf,
        /// The valid length in the manifest.
        valid_len: u64,
        /// The file's length.
        len: u64,
    },
    /// A checkpoint has a newer format version than the manifest that names
    /// it.
    FileNewerThanManifest {
        /// The file.
        path: PathBuf,
        /// The file's format version.
        file: FormatVersion,
        /// The manifest's format version.
        manifest: FormatVersion,
    },
    /// A checkpoint says it covers another commit version than the manifest
    /// that names it says.
    CoversMismatch {
        /// The file.
        path: PathBuf,
        /// The version in the manifest.
        manifest: CommitVersion,
        /// The version in the file.
        file: CommitVersion,
    },
    /// A checkpoint points to a block that the manifest does not vouch for:
    /// the manifest does not name the block's segment, or gives it a valid
    /// length that ends before the block does.
    NotCovered {
        /// The segment.
        segment: FileId,
        /// Where the last block the checkpoint points to in it ends.
        needed: u64,
        /// The segment's valid length in the manifest, if it names the
        /// segment.
        valid_len: Option<u64>,
    },
    /// The checkpoint offered covers an earlier commit version than the one
    /// the manifest names.
    CoversWentBack {
        /// The version the named checkpoint covers.
        current: CommitVersion,
        /// The version the offered checkpoint covers.
        offered: CommitVersion,
    },
    /// The manifest no longer names a checkpoint, but its file could not be
    /// deleted. Everything else the call had to do was done. The deletion is
    /// tried again whenever the checkpoints are told of a manifest, and by
    /// the next session that opens the directory.
    NotDeleted {
        /// The file.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// Every checkpoint id has been used.
    FileIdsExhausted,
}

impl PageTableError {
    pub(super) fn io<'a>(
        op: &'static str,
        path: &'a Path,
    ) -> impl FnOnce(io::Error) -> Self + use<'a> {
        move |source| Self::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(super) fn format<'a>(
        path: &'a Path,
        offset: u64,
    ) -> impl FnOnce(FormatError) -> Self + use<'a> {
        move |source| Self::Format {
            path: path.to_path_buf(),
            offset,
            source,
        }
    }
}

impl From<SegmentStoreError> for PageTableError {
    fn from(error: SegmentStoreError) -> Self {
        Self::Store(error)
    }
}

impl fmt::Display for PageTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { op, path, source } => {
                write!(f, "cannot {op} {}: {source}", path.display())
            }
            Self::Format {
                path,
                offset,
                source,
            } => write!(f, "{} at offset {offset}: {source}", path.display()),
            Self::Store(error) => write!(f, "{error}"),
            Self::Link { page, source } => write!(f, "page {page}: {source}"),
            Self::FileTooShort {
                path,
                valid_len,
                len,
            } => write!(
                f,
                "{} is {len} bytes long, shorter than its valid length of {valid_len}",
                path.display()
            ),
            Self::FileNewerThanManifest {
                path,
                file,
                manifest,
            } => write!(
                f,
                "{} has format version {}, newer than the manifest's {}",
                path.display(),
                file.get(),
                manifest.get()
            ),
            Self::CoversMismatch {
                path,
                manifest,
                file,
            } => write!(
                f,
                "{} covers version {file}, but the manifest says {manifest}",
                path.display()
            ),
            Self::NotCovered {
                segment,
                needed,
                valid_len: Some(valid_len),
            } => write!(
                f,
                "the checkpoint points to a block that ends at offset {needed} of segment \
                 {segment}, whose valid length is {valid_len}"
            ),
            Self::NotCovered {
                segment,
                valid_len: None,
                ..
            } => write!(
                f,
                "the checkpoint points into segment {segment}, which the manifest does not name"
            ),
            Self::CoversWentBack { current, offered } => write!(
                f,
                "a checkpoint that covers version {offered} cannot replace one that covers \
                 {current}"
            ),
            Self::NotDeleted { path, source } => write!(
                f,
                "the manifest no longer names {}, but it could not be deleted: {source}",
                path.display()
            ),
            Self::FileIdsExhausted => write!(f, "every checkpoint id has been used"),
        }
    }
}

impl std::error::Error for PageTableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::NotDeleted { source, .. } => Some(source),
            Self::Format { source, .. } => Some(source),
            Self::Store(source) => Some(source),
            Self::Link { source, .. } => Some(source),
            _ => None,
        }
    }
}
