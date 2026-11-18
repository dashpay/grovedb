//! The typed error every decoder in this crate returns.

use std::fmt;

use crate::header::FileKind;

/// Why a byte string is not a valid encoding.
///
/// Decoders never panic on untrusted bytes. They return one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// Fewer bytes are available than the encoding needs. Both counts start
    /// at the first byte the decoder was given.
    Truncated {
        /// Bytes the encoding needs.
        needed: usize,
        /// Bytes that were available.
        available: usize,
    },
    /// The file does not start with the LigninDB magic.
    BadMagic,
    /// The stored checksum does not match the bytes it covers.
    ChecksumMismatch {
        /// The checksum read from the record, in its stored (masked) form.
        stored: u32,
        /// The checksum computed over the covered bytes, in the same form.
        computed: u32,
    },
    /// The format version is zero, or one this build cannot read.
    UnsupportedFormatVersion {
        /// The version found in the file header.
        found: u16,
    },
    /// The file kind code is not defined in the file's format version.
    UnknownFileKind {
        /// The code found in the file header.
        code: u16,
    },
    /// A numbered file kind carries sequence number zero.
    InvalidFileSequence {
        /// The kind of the file.
        kind: FileKind,
    },
    /// A record's payload is longer than its format allows.
    RecordTooLong {
        /// The payload length.
        len: u64,
        /// The largest length allowed.
        max: u64,
    },
    /// A field that must hold an identifier holds the "none" pattern.
    MissingId,
    /// A reserved field or bit is not zero.
    NonZeroReserved,
    /// The record kind code is zero or not defined in the file's format
    /// version.
    UnknownRecordKind {
        /// The code found in the record.
        code: u32,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { needed, available } => {
                write!(f, "truncated: need {needed} bytes, have {available}")
            }
            Self::BadMagic => write!(f, "not a LigninDB file: bad magic"),
            Self::ChecksumMismatch { stored, computed } => write!(
                f,
                "checksum mismatch: stored {stored:#010x}, computed {computed:#010x}"
            ),
            Self::UnsupportedFormatVersion { found } => {
                write!(f, "unsupported format version {found}")
            }
            Self::UnknownFileKind { code } => write!(f, "unknown file kind code {code}"),
            Self::InvalidFileSequence { kind } => {
                write!(f, "{kind:?} file with sequence number zero")
            }
            Self::RecordTooLong { len, max } => {
                write!(
                    f,
                    "record payload of {len} bytes exceeds the maximum of {max}"
                )
            }
            Self::MissingId => write!(f, "identifier field holds no identifier"),
            Self::NonZeroReserved => write!(f, "reserved field is not zero"),
            Self::UnknownRecordKind { code } => write!(f, "unknown record kind code {code}"),
        }
    }
}

impl std::error::Error for FormatError {}
