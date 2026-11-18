//! The error the value log returns.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{value_log::ValuePointer, FileId, FormatError, FormatVersion};

use crate::segment::SegmentStoreError;

/// What the layers above answer with when they cannot look at a page or take
/// a pointer rewrite: [`ValueOwners`](super::ValueOwners) returns it and
/// [`ValueStoreError::Owners`] carries it.
pub type OwnersError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Why a pointer could not be counted as it was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miscount {
    /// The store holds no value-log file with the pointer's file id.
    UnknownFile,
    /// The pointer ends past the bytes the store has of its file.
    OutOfBounds,
    /// A cell was reported to stop holding the pointer, but the file's count
    /// of live values was already zero.
    CountIsZero,
    /// No durable manifest gives the pointer's file a valid length that
    /// reaches the end of the value, so a crash could take the value away.
    NotCovered,
    /// A pointer rewrite was counted, but the page step it was counted in
    /// did not report the pointer installed. The file of the old pointer is
    /// kept from now on.
    NotInstalled,
}

impl fmt::Display for Miscount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFile => write!(f, "there is no such value-log file"),
            Self::OutOfBounds => write!(f, "it ends past the end of its file"),
            Self::CountIsZero => write!(f, "its file's count of live values is already zero"),
            Self::NotCovered => write!(f, "no durable manifest covers it"),
            Self::NotInstalled => {
                write!(f, "its rewrite was counted and not reported as installed")
            }
        }
    }
}

/// Why an operation of the value log failed.
#[derive(Debug)]
pub enum ValueStoreError {
    /// The file layer reported an error.
    Io {
        /// What was being done.
        op: &'static str,
        /// The file or directory it was done to.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The bytes of a file are not what its format allows, or a value cannot
    /// be encoded.
    Format {
        /// The file.
        path: PathBuf,
        /// The file offset of the value that was being read or written.
        offset: u64,
        /// What is wrong with the bytes.
        source: FormatError,
    },
    /// The manifest could not be read or replaced.
    Manifest(SegmentStoreError),
    /// A manifest of another directory was passed.
    WrongDirectory {
        /// The directory of the value log.
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
    /// A file's header carries another id than the file's name.
    WrongFileId {
        /// The file.
        path: PathBuf,
        /// The id in the header.
        found: u32,
    },
    /// A value-log file that the manifest does not name, with an id above the
    /// last one the manifest records, holds more than a file header. No
    /// session of this database wrote it, so the manifest does not belong to
    /// these files. Nothing was removed.
    UnnamedFileHoldsValues {
        /// The file.
        path: PathBuf,
        /// The file's length.
        len: u64,
    },
    /// The counts that were recovered say a file holds live values, but the
    /// manifest does not name that file. The values are lost, or the counts
    /// and the manifest are not of the same database.
    LiveValuesInMissingFile {
        /// The file.
        file: FileId,
        /// The live values the counts give it.
        values: u64,
    },
    /// The counts that were recovered show that the count of a file was too
    /// low, so a cell may still point into it, and the manifest does not
    /// name that file although it is still in `val/`. Nothing was removed.
    MiscountedFileNotNamed {
        /// The file.
        file: FileId,
    },
    /// The manifest no longer names a dropped file, but the file could not be
    /// deleted. Everything else the call had to do was done. The deletion is
    /// tried again whenever the store is told of a manifest, and by the next
    /// session that opens the directory.
    NotDeleted {
        /// The file.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The store holds no value-log file with this id.
    UnknownFile {
        /// The id asked for.
        file: FileId,
    },
    /// A pointer does not lie inside the values of its file.
    OutOfBounds {
        /// The pointer.
        pointer: ValuePointer,
        /// Where the file's readable bytes end.
        end: u64,
    },
    /// A pointer was to be installed in a page, but no durable manifest gives
    /// its file a valid length that reaches the end of the value.
    NotCovered {
        /// The pointer.
        pointer: ValuePointer,
    },
    /// A value is longer than this machine can hold in memory.
    ValueTooLong {
        /// The bytes the value's records take.
        extent: u64,
    },
    /// The file takes no values until a manifest names it.
    NotNamed {
        /// The file.
        file: FileId,
    },
    /// The file is closed and takes no more values.
    NotOpen {
        /// The file.
        file: FileId,
    },
    /// The file is still being written, so it cannot be cleaned.
    NotClosed {
        /// The file.
        file: FileId,
    },
    /// A sync of the file failed, so nothing written after the last sync that
    /// succeeded can be made durable in it.
    FileFailed {
        /// The file.
        file: FileId,
    },
    /// Every value-log file id has been used.
    FileIdsExhausted,
    /// The value log was opened read-only.
    ReadOnly,
    /// A writer, a set of copied values or a mark was passed to a store it
    /// does not come from, or to the store of a later session.
    WrongStore,
    /// A pointer was reported that the counts cannot take. The rest of the
    /// change was applied. If a file's count is now known to be wrong, the
    /// file is kept whatever its count says.
    Miscounted {
        /// The pointer.
        pointer: ValuePointer,
        /// Why it could not be counted.
        why: Miscount,
    },
    /// The layers above could not look at a page or take a pointer rewrite.
    Owners(OwnersError),
}

impl ValueStoreError {
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

    /// The error of the file layer that this error reports, if it reports
    /// one.
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            Self::Io { source, .. } | Self::NotDeleted { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<SegmentStoreError> for ValueStoreError {
    fn from(error: SegmentStoreError) -> Self {
        match error {
            SegmentStoreError::Io { op, path, source } => Self::Io { op, path, source },
            SegmentStoreError::Format {
                path,
                offset,
                source,
            } => Self::Format {
                path,
                offset,
                source,
            },
            other => Self::Manifest(other),
        }
    }
}

impl fmt::Display for ValueStoreError {
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
            Self::Manifest(error) => write!(f, "{error}"),
            Self::WrongDirectory { expected, found } => write!(
                f,
                "manifest of {} passed to the value log of {}",
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
            Self::WrongFileId { path, found } => write!(
                f,
                "the header of {} says it is value-log file {found}",
                path.display()
            ),
            Self::UnnamedFileHoldsValues { path, len } => write!(
                f,
                "{} is {len} bytes long, but the manifest does not name it and records no id that \
                 high",
                path.display()
            ),
            Self::LiveValuesInMissingFile { file, values } => write!(
                f,
                "value-log file {file} has {values} live values, but the manifest does not name it"
            ),
            Self::MiscountedFileNotNamed { file } => write!(
                f,
                "the count of value-log file {file} was too low, and the manifest does not name \
                 the file"
            ),
            Self::NotDeleted { path, source } => write!(
                f,
                "the manifest no longer names {}, but it could not be deleted: {source}",
                path.display()
            ),
            Self::UnknownFile { file } => write!(f, "no value-log file {file}"),
            Self::OutOfBounds { pointer, end } => write!(
                f,
                "a value of {} bytes at offset {} of value-log file {} is not inside its values, \
                 which end at {end}",
                pointer.value_len(),
                pointer.offset(),
                pointer.file()
            ),
            Self::NotCovered { pointer } => write!(
                f,
                "no durable manifest covers the value at offset {} of value-log file {}",
                pointer.offset(),
                pointer.file()
            ),
            Self::ValueTooLong { extent } => write!(
                f,
                "a value whose records take {extent} bytes does not fit in memory here"
            ),
            Self::NotNamed { file } => write!(f, "no manifest names value-log file {file} yet"),
            Self::NotOpen { file } => write!(f, "value-log file {file} takes no more values"),
            Self::NotClosed { file } => write!(f, "value-log file {file} is still being written"),
            Self::FileFailed { file } => write!(f, "a sync of value-log file {file} failed"),
            Self::FileIdsExhausted => write!(f, "every value-log file id has been used"),
            Self::ReadOnly => write!(f, "the value log is read-only"),
            Self::WrongStore => write!(
                f,
                "a writer, copies or a mark of another value log, or of an earlier session"
            ),
            Self::Miscounted { pointer, why } => write!(
                f,
                "the value at offset {} of value-log file {} cannot be counted: {why}",
                pointer.offset(),
                pointer.file()
            ),
            Self::Owners(error) => write!(f, "the owner of a value could not be asked: {error}"),
        }
    }
}

impl std::error::Error for ValueStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::NotDeleted { source, .. } => Some(source),
            Self::Format { source, .. } => Some(source),
            Self::Manifest(error) => Some(error),
            Self::Owners(error) => Some(&**error),
            _ => None,
        }
    }
}
