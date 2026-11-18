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
    /// The bytes are not a run of page deltas, or a run does not apply.
    Delta(crate::delta::DeltaError),
    /// The bytes are not a flat page image, or a flat page cannot be built.
    FlatPage(crate::flat_page::FlatPageError),
    /// The bytes are not a segment record, or a segment cannot be written.
    Segment(crate::segment::SegmentError),
    /// The bytes are not a manifest, or a manifest cannot be built.
    Manifest(crate::manifest::ManifestError),
    /// The bytes are not a page-table checkpoint, or one cannot be written.
    Table(crate::table::TableError),
    /// The bytes are not a tree page or a tree page cell, or a tree page
    /// cannot take what was written to it.
    TreePage(crate::tree_page::TreePageError),
    /// The bytes are not a valid value-log encoding, or a value cannot be
    /// encoded.
    ValueLog(crate::value_log::ValueLogError),
    /// A page size that is not 8,192, 16,384, 32,768 or 65,536 bytes.
    InvalidPageSize {
        /// The page size asked for, in bytes.
        page_size: usize,
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
            Self::Delta(error) => write!(f, "page delta: {error}"),
            Self::FlatPage(error) => write!(f, "flat page: {error}"),
            Self::Segment(error) => write!(f, "segment: {error}"),
            Self::Manifest(error) => write!(f, "manifest: {error}"),
            Self::Table(error) => write!(f, "page-table checkpoint: {error}"),
            Self::TreePage(error) => write!(f, "tree page: {error}"),
            Self::ValueLog(error) => write!(f, "value log: {error}"),
            Self::InvalidPageSize { page_size } => write!(
                f,
                "page size {page_size} is not 8192, 16384, 32768 or 65536"
            ),
            Self::MissingId => write!(f, "identifier field holds no identifier"),
            Self::NonZeroReserved => write!(f, "reserved field is not zero"),
            Self::UnknownRecordKind { code } => write!(f, "unknown record kind code {code}"),
        }
    }
}

impl std::error::Error for FormatError {}
