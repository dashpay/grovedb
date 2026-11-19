//! The error the log returns.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use lignin_format::{
    wal::{LogAddress, LogPlace},
    FileId, FormatError, FormatVersion,
};

use crate::segment::SegmentStoreError;

/// Why an operation of the log failed.
#[derive(Debug)]
pub enum LogError {
    /// The file layer reported an error, and nothing of the log changed.
    Io {
        /// What was being done.
        op: &'static str,
        /// The file or directory it was done to.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The bytes of a log file are not what the format allows.
    Format {
        /// The file.
        path: PathBuf,
        /// The file offset of the record that was being read.
        offset: u64,
        /// What is wrong with the bytes.
        source: FormatError,
    },
    /// A log file fails below the valid length its manifest gives it, where
    /// the manifest vouches for whole items: the file is damaged, and what
    /// follows is not taken for the end of the log.
    Damaged {
        /// The file.
        path: PathBuf,
        /// The file offset of the first item that is not whole.
        offset: u64,
        /// The valid length in the manifest.
        valid_len: u64,
        /// What is wrong there.
        source: FormatError,
    },
    /// The manifest could not be read or replaced.
    Manifest(SegmentStoreError),
    /// A manifest of another directory was passed.
    WrongDirectory {
        /// The directory of the log.
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
    /// A log file that the manifest does not name, with an id above the last
    /// one the manifest records, holds more than a file header. No session
    /// of this database wrote it, so the manifest does not belong to these
    /// files. Nothing was removed.
    UnnamedFileHoldsItems {
        /// The file.
        path: PathBuf,
        /// The file's length.
        len: u64,
    },
    /// The end of the log that was passed is not the end of the log this
    /// manifest names.
    WrongLogEnd {
        /// The first file on which the two disagree.
        file: FileId,
    },
    /// The manifest no longer names a dropped file, but the file could not be
    /// deleted. Everything else the call had to do was done. The deletion is
    /// tried again whenever the log is told of a manifest, and by the next
    /// session that opens the directory.
    NotDeleted {
        /// The file.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// Every log file id has been used.
    FileIdsExhausted,
    /// The log already has a file to turn to.
    SpareExists {
        /// That file.
        file: FileId,
    },
    /// The log has no file to turn to: create one and replace the manifest.
    NoSpare,
    /// No durable manifest names the file the log would turn to.
    SpareNotNamed {
        /// That file.
        file: FileId,
    },
    /// The log has no file that takes items: it has not turned to its first
    /// file yet, or an append to its file failed. Turn to the next file.
    NoActiveFile,
    /// The log no longer ends where the group was written for, so the group
    /// was not appended. Ask for the place again and write the group for it.
    PlaceTaken {
        /// The place the group was written for.
        expected: LogPlace,
        /// Where the log ends now, if it has a file that takes items.
        found: Option<LogPlace>,
    },
    /// The bytes offered are not an item that may stand at the end of the
    /// log. Nothing was appended.
    Refused {
        /// The offset in the bytes of the record that is refused.
        offset: usize,
        /// What recovery would say of it.
        source: FormatError,
    },
    /// The bytes offered are more than one item, or an item and something
    /// else. Nothing was appended.
    NotOneItem {
        /// The bytes offered.
        len: usize,
        /// The length of the first item in them.
        item_len: usize,
    },
    /// The bytes offered are an item of another kind than the call appends.
    /// Nothing was appended.
    WrongItem {
        /// What the call appends.
        expected: &'static str,
    },
    /// The file layer failed the append. The item is not in the log, and no
    /// crash brings it back. The log file takes no more items: turn to the
    /// next one.
    NotAppended {
        /// The file.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The file layer failed an append, and the log could not find out
    /// whether the item is in the file, or could not keep a crash from
    /// bringing it back. The log takes nothing more in this session. The next
    /// open settles it.
    Unsettled {
        /// The file.
        path: PathBuf,
        /// The error that left it open.
        source: io::Error,
    },
    /// A sync of a log file failed, so nothing that was appended after the
    /// last sync that succeeded can be made durable in this session.
    SyncFailed {
        /// The file.
        path: PathBuf,
        /// The error the file layer returned.
        source: io::Error,
    },
    /// The log is not durable up to the place from which it was to be
    /// kept, so no file before that place was dropped: a crash could take
    /// the place away, and with it the beginning of what is left of the
    /// log.
    NotDurable {
        /// The place the log was to be kept from.
        place: LogPlace,
        /// How far the log is durable.
        durable: Option<LogPlace>,
    },
    /// A place was passed to a log it does not come from, or to the log of a
    /// later session.
    WrongStore,
    /// The log holds no file with this id, or the file takes part in
    /// nothing that was asked.
    UnknownFile {
        /// The id asked for.
        file: FileId,
    },
    /// No log file in use holds this address.
    UnknownAddress {
        /// The address asked for.
        address: LogAddress,
    },
    /// A value is longer than this machine can hold in memory.
    ValueTooLong {
        /// The bytes the value's records take.
        extent: u64,
    },
}

impl LogError {
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
            Self::Io { source, .. }
            | Self::NotDeleted { source, .. }
            | Self::NotAppended { source, .. }
            | Self::Unsettled { source, .. }
            | Self::SyncFailed { source, .. } => Some(source),
            Self::Manifest(SegmentStoreError::Io { source, .. }) => Some(source),
            _ => None,
        }
    }
}

impl From<SegmentStoreError> for LogError {
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

impl fmt::Display for LogError {
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
            Self::Damaged {
                path,
                offset,
                valid_len,
                source,
            } => write!(
                f,
                "{} is damaged at offset {offset}, below its valid length of {valid_len}: {source}",
                path.display()
            ),
            Self::Manifest(error) => write!(f, "{error}"),
            Self::WrongDirectory { expected, found } => write!(
                f,
                "manifest of {} passed to the log of {}",
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
                "the header of {} says it is log file {found}",
                path.display()
            ),
            Self::UnnamedFileHoldsItems { path, len } => write!(
                f,
                "{} is {len} bytes long, but the manifest does not name it and records no id that \
                 high",
                path.display()
            ),
            Self::WrongLogEnd { file } => write!(
                f,
                "the end of the log that was passed and the manifest disagree on log file {file}"
            ),
            Self::NotDeleted { path, source } => write!(
                f,
                "the manifest no longer names {}, but it could not be deleted: {source}",
                path.display()
            ),
            Self::FileIdsExhausted => write!(f, "every log file id has been used"),
            Self::SpareExists { file } => {
                write!(f, "the log already has file {file} to turn to")
            }
            Self::NoSpare => write!(f, "the log has no file to turn to"),
            Self::SpareNotNamed { file } => {
                write!(f, "no manifest names log file {file} yet")
            }
            Self::NoActiveFile => write!(f, "the log has no file that takes items"),
            Self::PlaceTaken { expected, found } => {
                write!(
                    f,
                    "a group written for offset {} of log file {}, ",
                    expected.offset(),
                    expected.file()
                )?;
                match found {
                    Some(found) => write!(
                        f,
                        "where the log now ends at offset {} of file {}",
                        found.offset(),
                        found.file()
                    ),
                    None => write!(f, "where the log now has no file that takes items"),
                }
            }
            Self::Refused { offset, source } => write!(
                f,
                "the record at offset {offset} of what was offered may not stand at the end of \
                 the log: {source}"
            ),
            Self::NotOneItem { len, item_len } => write!(
                f,
                "{len} bytes were offered as one item, and the first item in them is {item_len} \
                 bytes long"
            ),
            Self::WrongItem { expected } => {
                write!(f, "what was offered is not {expected}")
            }
            Self::NotAppended { path, source } => write!(
                f,
                "an append to {} failed, and the item is not in the log: {source}",
                path.display()
            ),
            Self::Unsettled { path, source } => write!(
                f,
                "an append to {} failed, and it is not known whether the item is in the log: \
                 {source}",
                path.display()
            ),
            Self::SyncFailed { path, source } => write!(
                f,
                "a sync of {} failed, so the log cannot be made durable in this session: {source}",
                path.display()
            ),
            Self::NotDurable { place, durable } => {
                write!(
                    f,
                    "the log is to be kept from offset {} of file {}, and is durable ",
                    place.offset(),
                    place.file()
                )?;
                match durable {
                    Some(durable) => write!(
                        f,
                        "up to offset {} of file {}",
                        durable.offset(),
                        durable.file()
                    ),
                    None => write!(f, "nowhere"),
                }
            }
            Self::WrongStore => write!(f, "a place of another log, or of an earlier session"),
            Self::UnknownFile { file } => write!(f, "no log file {file}"),
            Self::UnknownAddress { address } => {
                write!(f, "no log file in use holds the address {address}")
            }
            Self::ValueTooLong { extent } => write!(
                f,
                "a value whose records take {extent} bytes does not fit in memory here"
            ),
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. }
            | Self::NotDeleted { source, .. }
            | Self::NotAppended { source, .. }
            | Self::Unsettled { source, .. }
            | Self::SyncFailed { source, .. } => Some(source),
            Self::Format { source, .. }
            | Self::Damaged { source, .. }
            | Self::Refused { source, .. } => Some(source),
            Self::Manifest(error) => Some(error),
            _ => None,
        }
    }
}
