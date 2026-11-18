//! The error the segment store returns.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{tree_page::TreePageSize, FileId, FileKind, FormatError, FormatVersion};

use super::store::BlockLocation;

/// A change that a manifest must not make to the manifest it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestChange {
    /// The page size is not the one the store was created with.
    PageSizeChanged {
        /// The page size of the manifest in force.
        from: TreePageSize,
        /// The page size of the manifest offered.
        to: TreePageSize,
    },
    /// The last file id of a kind is lower than it was.
    LastFileIdLowered {
        /// The kind whose last id went down.
        kind: FileKind,
    },
    /// A file that was not named before has an id that is not above the last
    /// id its kind had.
    FileIdReused {
        /// The file's kind.
        kind: FileKind,
        /// The file's id.
        id: FileId,
    },
    /// A file's valid length is lower than it was.
    FileShortened {
        /// The file's kind.
        kind: FileKind,
        /// The file's id.
        id: FileId,
    },
    /// A file that was closed or sealed has another state or another valid
    /// length.
    FinishedFileChanged {
        /// The file's kind.
        kind: FileKind,
        /// The file's id.
        id: FileId,
    },
    /// The page-table checkpoint is the same file with another valid length
    /// or another commit version.
    TableCheckpointChanged {
        /// The checkpoint's file id.
        id: FileId,
    },
}

impl fmt::Display for ManifestChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageSizeChanged { from, to } => write!(
                f,
                "the page size is {}, and the store was created with {}",
                to.get(),
                from.get()
            ),
            Self::LastFileIdLowered { kind } => {
                write!(f, "the last {kind:?} file id is lower than it was")
            }
            Self::FileIdReused { kind, id } => write!(
                f,
                "{kind:?} file {id} is new, but its id is not above the last id of its kind"
            ),
            Self::FileShortened { kind, id } => {
                write!(f, "{kind:?} file {id} has a lower valid length than it had")
            }
            Self::FinishedFileChanged { kind, id } => write!(
                f,
                "{kind:?} file {id} was closed or sealed and has another state or length"
            ),
            Self::TableCheckpointChanged { id } => write!(
                f,
                "page-table checkpoint {id} has another length or commit version than it had"
            ),
        }
    }
}

/// Why an operation of the segment store failed.
#[derive(Debug)]
pub enum SegmentStoreError {
    /// The file layer reported an error.
    Io {
        /// What was being done.
        op: &'static str,
        /// The file or directory it was done to.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The bytes of a file are not what its format allows.
    Format {
        /// The file.
        path: PathBuf,
        /// The file offset of the record that was being read or written.
        offset: u64,
        /// What is wrong with the bytes.
        source: FormatError,
    },
    /// The directory has no `MANIFEST`.
    NoManifest {
        /// The database directory.
        dir: PathBuf,
    },
    /// The directory already has a `MANIFEST`.
    ManifestExists {
        /// The database directory.
        dir: PathBuf,
    },
    /// A replacement of the manifest failed after its rename began, so it is
    /// not known which manifest a crash would leave. Open the manifest again.
    ManifestInDoubt {
        /// The database directory.
        dir: PathBuf,
    },
    /// The manifest offered as a replacement breaks a rule against the one
    /// it would replace.
    ManifestChange(ManifestChange),
    /// The store was created with another page size than the one it is
    /// opened with.
    PageSizeMismatch {
        /// The database directory.
        dir: PathBuf,
        /// The page size in the manifest.
        stored: TreePageSize,
        /// The page size the store was opened with.
        requested: TreePageSize,
    },
    /// A manifest of another directory was passed.
    WrongDirectory {
        /// The directory of the segment store.
        expected: PathBuf,
        /// The directory of the manifest.
        found: PathBuf,
    },
    /// A file is shorter than the valid length its manifest gives it.
    FileTooShort {
        /// The file.
        path: PathBuf,
        /// The valid length in the manifest.
        valid_len: u64,
        /// The file's length.
        len: u64,
    },
    /// A file has a newer format version than the manifest that names it.
    FileNewerThanManifest {
        /// The file.
        path: PathBuf,
        /// The file's format version.
        file: FormatVersion,
        /// The manifest's format version.
        manifest: FormatVersion,
    },
    /// A segment file that the manifest does not name, with an id above the
    /// last one the manifest records, holds more than a file header. No
    /// session of this database wrote it, so the manifest does not belong to
    /// these files. Nothing was removed.
    UnnamedSegmentHoldsBlocks {
        /// The file.
        path: PathBuf,
        /// The file's length.
        len: u64,
    },
    /// The manifest no longer names a dropped segment, but its file could not
    /// be deleted. Everything else the call had to do was done. The deletion
    /// is tried again whenever the store is told of a manifest, and by the
    /// next session that opens the directory.
    NotDeleted {
        /// The file.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The store holds no segment with this id.
    UnknownSegment {
        /// The id asked for.
        file: FileId,
    },
    /// A location does not lie inside the blocks of its segment.
    OutOfBounds {
        /// The location asked for.
        location: BlockLocation,
        /// Where the segment's readable bytes end.
        end: u64,
    },
    /// The segment has no footer, because it is not sealed.
    NotSealed {
        /// The segment.
        file: FileId,
    },
    /// The segment takes no blocks until a manifest names it.
    NotNamed {
        /// The segment.
        file: FileId,
    },
    /// The segment is closed or sealed and takes no more blocks.
    NotOpen {
        /// The segment.
        file: FileId,
    },
    /// A sync of the segment failed, so nothing written after the last sync
    /// that succeeded can be made durable in it.
    SegmentFailed {
        /// The segment.
        file: FileId,
    },
    /// The segment holds as many blocks as one segment may.
    SegmentFull {
        /// The segment.
        file: FileId,
    },
    /// The segment still has a writer.
    SegmentBusy {
        /// The segment.
        file: FileId,
    },
    /// Every segment id has been used.
    FileIdsExhausted,
    /// The store was opened read-only.
    ReadOnly,
}

impl SegmentStoreError {
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

impl fmt::Display for SegmentStoreError {
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
            Self::NoManifest { dir } => write!(f, "{} has no MANIFEST", dir.display()),
            Self::ManifestExists { dir } => {
                write!(f, "{} already has a MANIFEST", dir.display())
            }
            Self::ManifestInDoubt { dir } => write!(
                f,
                "a replacement of the manifest of {} did not finish; open it again",
                dir.display()
            ),
            Self::ManifestChange(change) => write!(f, "invalid manifest replacement: {change}"),
            Self::PageSizeMismatch {
                dir,
                stored,
                requested,
            } => write!(
                f,
                "{} has pages of {} bytes and was opened with pages of {}",
                dir.display(),
                stored.get(),
                requested.get()
            ),
            Self::WrongDirectory { expected, found } => write!(
                f,
                "manifest of {} passed to the segment store of {}",
                found.display(),
                expected.display()
            ),
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
            Self::UnnamedSegmentHoldsBlocks { path, len } => write!(
                f,
                "{} is {len} bytes long, but the manifest does not name it and records no id that \
                 high",
                path.display()
            ),
            Self::NotDeleted { path, source } => write!(
                f,
                "the manifest no longer names {}, but it could not be deleted: {source}",
                path.display()
            ),
            Self::UnknownSegment { file } => write!(f, "no segment {file}"),
            Self::OutOfBounds { location, end } => write!(
                f,
                "{} bytes at offset {} of segment {} are not inside its blocks, which end at {end}",
                location.len, location.offset, location.file
            ),
            Self::NotSealed { file } => write!(f, "segment {file} is not sealed"),
            Self::NotNamed { file } => write!(f, "no manifest names segment {file} yet"),
            Self::NotOpen { file } => write!(f, "segment {file} takes no more blocks"),
            Self::SegmentFailed { file } => write!(f, "a sync of segment {file} failed"),
            Self::SegmentFull { file } => write!(f, "segment {file} is full"),
            Self::SegmentBusy { file } => write!(f, "segment {file} still has a writer"),
            Self::FileIdsExhausted => write!(f, "every segment id has been used"),
            Self::ReadOnly => write!(f, "the segment store is read-only"),
        }
    }
}

impl std::error::Error for SegmentStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Format { source, .. } => Some(source),
            Self::NotDeleted { source, .. } => Some(source),
            _ => None,
        }
    }
}
