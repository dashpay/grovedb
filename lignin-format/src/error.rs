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
    /// The file is of another kind than the one the decoder reads.
    WrongFileKind {
        /// The kind the decoder reads.
        expected: FileKind,
        /// The kind in the file header.
        found: FileKind,
    },
    /// The sequence in the file header is not the one the file must carry.
    WrongFileSequence {
        /// The sequence the file must carry.
        expected: u32,
        /// The sequence in the file header.
        found: u32,
    },
    /// The record kind is defined, but not allowed where it was found.
    UnexpectedRecordKind {
        /// The code found in the record.
        code: u32,
    },
    /// A length or an offset does not agree with the bytes or the fields it
    /// describes.
    InconsistentLength {
        /// The value the other bytes and fields require.
        expected: u64,
        /// The value found.
        found: u64,
    },
    /// A compressed block is not smaller than the same block stored raw.
    CompressedBlockNotSmaller {
        /// The body length of the compressed block.
        stored: u32,
        /// The length it says its payload has once decompressed.
        raw: u32,
    },
    /// Compressed bytes do not decompress to the length the block states.
    Decompression,
    /// The file state code is zero or not defined in the manifest's format
    /// version.
    UnknownFileState {
        /// The code found in the manifest.
        code: u8,
    },
    /// A manifest entry breaks a rule for its file kind or state.
    InvalidManifestEntry,
    /// The files in a manifest are not in strictly ascending order of kind
    /// and id.
    ManifestEntriesOutOfOrder,
    /// A record's payload is longer than its format allows.
    RecordTooLong {
        /// The payload length.
        len: u64,
        /// The largest length allowed.
        max: u64,
    },
    /// The bytes are not a flat page image, or a flat page cannot be built.
    FlatPage(crate::flat_page::FlatPageError),
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
            Self::WrongFileKind { expected, found } => {
                write!(f, "expected a {expected:?} file, found a {found:?} file")
            }
            Self::WrongFileSequence { expected, found } => {
                write!(f, "file header sequence is {found}, expected {expected}")
            }
            Self::UnexpectedRecordKind { code } => {
                write!(f, "record kind code {code} is not allowed here")
            }
            Self::InconsistentLength { expected, found } => {
                write!(
                    f,
                    "inconsistent length or offset: found {found}, expected {expected}"
                )
            }
            Self::CompressedBlockNotSmaller { stored, raw } => write!(
                f,
                "compressed block of {stored} bytes is not smaller than its {raw} raw bytes"
            ),
            Self::Decompression => {
                write!(f, "compressed bytes do not decompress to the stated length")
            }
            Self::UnknownFileState { code } => write!(f, "unknown file state code {code}"),
            Self::InvalidManifestEntry => {
                write!(f, "manifest entry breaks a rule for its file kind or state")
            }
            Self::ManifestEntriesOutOfOrder => {
                write!(
                    f,
                    "manifest files are not in ascending order of kind and id"
                )
            }
            Self::RecordTooLong { len, max } => {
                write!(
                    f,
                    "record payload of {len} bytes exceeds the maximum of {max}"
                )
            }
            Self::FlatPage(error) => write!(f, "flat page: {error}"),
            Self::MissingId => write!(f, "identifier field holds no identifier"),
            Self::NonZeroReserved => write!(f, "reserved field is not zero"),
            Self::UnknownRecordKind { code } => write!(f, "unknown record kind code {code}"),
        }
    }
}

impl std::error::Error for FormatError {}
