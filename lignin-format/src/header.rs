//! The header every LigninDB file starts with, the format version it carries,
//! and the default record frame.
//!
//! See "File header", "Format version" and "Records" in
//! `docs/lignindb/format.md`.

use crate::{
    checksum::{Checksum, MAX_CHECKSUMMED_LEN},
    error::FormatError,
    ids::FileId,
};

/// A version of the LigninDB on-disk format.
///
/// A file's kind and format version together fix the layout of every byte in
/// it. Each variant is a version this build can read, so a decoder that
/// matches on it has to say what it does for every readable version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum FormatVersion {
    /// The first format.
    V1 = 1,
}

impl FormatVersion {
    /// Bytes the version takes on disk.
    pub const ENCODED_LEN: usize = 2;
    /// The version this build writes.
    pub const CURRENT: Self = Self::V1;
    /// The oldest version this build reads.
    pub const OLDEST_READABLE: Self = Self::V1;

    /// The version with this number, if this build can read it.
    pub const fn new(raw: u16) -> Result<Self, FormatError> {
        match raw {
            1 => Ok(Self::V1),
            found => Err(FormatError::UnsupportedFormatVersion { found }),
        }
    }

    /// The version's number.
    pub const fn get(self) -> u16 {
        self as u16
    }
}

/// What a file holds. The code is stored in the file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum FileKind {
    /// `MANIFEST`: the live files and the page-table checkpoint.
    Manifest = 1,
    /// `wal/<n>.log`: the write-ahead log.
    Wal = 2,
    /// `seg/<n>.seg`: page images and deltas.
    Segment = 3,
    /// `val/<n>.val`: values above the inline threshold.
    ValueLog = 4,
    /// `table/<n>.pt`: a page-table checkpoint.
    PageTable = 5,
}

impl FileKind {
    /// Every file kind of the current format version.
    pub const ALL: [Self; 5] = [
        Self::Manifest,
        Self::Wal,
        Self::Segment,
        Self::ValueLog,
        Self::PageTable,
    ];

    /// The kind with this code in a file of format `version`.
    pub const fn from_code(code: u16, version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => match code {
                1 => Ok(Self::Manifest),
                2 => Ok(Self::Wal),
                3 => Ok(Self::Segment),
                4 => Ok(Self::ValueLog),
                5 => Ok(Self::PageTable),
                code => Err(FormatError::UnknownFileKind { code }),
            },
        }
    }

    /// The code stored in the file header.
    pub const fn code(self) -> u16 {
        self as u16
    }

    /// Whether files of this kind carry a [`FileId`] in their name and header.
    pub const fn is_numbered(self) -> bool {
        !matches!(self, Self::Manifest)
    }
}

/// The 20 bytes every LigninDB file starts with.
///
/// ```text
/// offset  size  field
///      0     8  magic "LIGNINDB"
///      8     2  file kind
///     10     2  format version
///     12     4  sequence
///     16     4  checksum of bytes 0..16
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHeader {
    kind: FileKind,
    version: FormatVersion,
    sequence: u32,
}

impl FileHeader {
    /// Bytes the header takes on disk.
    pub const ENCODED_LEN: usize = 20;
    /// The first eight bytes of every LigninDB file.
    pub const MAGIC: [u8; 8] = *b"LIGNINDB";

    const KIND_AT: usize = 8;
    const VERSION_AT: usize = 10;
    const SEQUENCE_AT: usize = 12;
    const CHECKSUM_AT: usize = 16;

    /// A header for a file of `kind` written in format `version`.
    ///
    /// `sequence` is the file's [`FileId`] for a numbered kind and must be
    /// one there: not zero, and not [`FileId::RESERVED`].
    pub const fn new(
        kind: FileKind,
        version: FormatVersion,
        sequence: u32,
    ) -> Result<Self, FormatError> {
        if kind.is_numbered() && FileId::new(sequence).is_none() {
            return Err(FormatError::InvalidFileSequence { kind });
        }
        Ok(Self {
            kind,
            version,
            sequence,
        })
    }

    /// What the file holds.
    pub const fn kind(&self) -> FileKind {
        self.kind
    }

    /// The format version the file was written in.
    pub const fn version(&self) -> FormatVersion {
        self.version
    }

    /// The sequence field as stored.
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// The file's id, for a numbered kind.
    pub const fn file_id(&self) -> Option<FileId> {
        if self.kind.is_numbered() {
            FileId::new(self.sequence)
        } else {
            None
        }
    }

    /// The bytes written at the start of the file.
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut bytes = [0; Self::ENCODED_LEN];
        bytes[..Self::KIND_AT].copy_from_slice(&Self::MAGIC);
        bytes[Self::KIND_AT..Self::VERSION_AT].copy_from_slice(&self.kind.code().to_le_bytes());
        bytes[Self::VERSION_AT..Self::SEQUENCE_AT]
            .copy_from_slice(&self.version.get().to_le_bytes());
        bytes[Self::SEQUENCE_AT..Self::CHECKSUM_AT].copy_from_slice(&self.sequence.to_le_bytes());
        let checksum = Checksum::of(&[&bytes[..Self::CHECKSUM_AT]]);
        bytes[Self::CHECKSUM_AT..].copy_from_slice(&checksum.encode());
        bytes
    }

    /// Reads the header from the first [`Self::ENCODED_LEN`] bytes of a file.
    ///
    /// Later bytes are ignored. The checks run in a fixed order: length,
    /// magic, checksum, format version, file kind, sequence.
    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        let bytes: &[u8; Self::ENCODED_LEN] = first_chunk(bytes)?;
        let (covered, checksum) = bytes.split_at(Self::CHECKSUM_AT);
        if covered[..Self::KIND_AT] != Self::MAGIC {
            return Err(FormatError::BadMagic);
        }
        Checksum::decode(array(checksum)).verify(&[covered])?;

        let version = FormatVersion::new(u16::from_le_bytes(array(
            &covered[Self::VERSION_AT..Self::SEQUENCE_AT],
        )))?;
        let kind = FileKind::from_code(
            u16::from_le_bytes(array(&covered[Self::KIND_AT..Self::VERSION_AT])),
            version,
        )?;
        let sequence = u32::from_le_bytes(array(&covered[Self::SEQUENCE_AT..]));
        Self::new(kind, version, sequence)
    }
}

/// The default frame of a record: its payload length and its checksum.
///
/// ```text
/// offset  size  field
///      0     4  payload length
///      4     4  checksum of bytes 0..4 followed by the payload
///      8   len  payload
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordHeader {
    payload_len: u32,
    checksum: Checksum,
}

impl RecordHeader {
    /// Bytes the header takes on disk.
    pub const ENCODED_LEN: usize = 8;
    /// The longest payload a framed record may carry.
    pub const MAX_PAYLOAD_LEN: usize = MAX_CHECKSUMMED_LEN - Self::CHECKSUM_AT;

    const CHECKSUM_AT: usize = 4;

    /// The header that frames `payload`.
    pub fn for_payload(payload: &[u8]) -> Result<Self, FormatError> {
        let payload_len = checked_payload_len(payload.len() as u64, Self::MAX_PAYLOAD_LEN)?;
        Ok(Self {
            payload_len,
            checksum: Checksum::of(&[&payload_len.to_le_bytes(), payload]),
        })
    }

    /// The payload length in bytes.
    pub const fn payload_len(&self) -> usize {
        self.payload_len as usize
    }

    /// The stored checksum.
    pub const fn checksum(&self) -> Checksum {
        self.checksum
    }

    /// The bytes written in front of the payload.
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut bytes = [0; Self::ENCODED_LEN];
        bytes[..Self::CHECKSUM_AT].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes[Self::CHECKSUM_AT..].copy_from_slice(&self.checksum.encode());
        bytes
    }

    /// Reads a record header from the first [`Self::ENCODED_LEN`] bytes.
    ///
    /// The length is checked against `max_payload_len` (and
    /// [`Self::MAX_PAYLOAD_LEN`]) here, before the caller reads or allocates
    /// anything of that size. The payload is not checked until
    /// [`Self::verified_payload`].
    pub fn decode(bytes: &[u8], max_payload_len: usize) -> Result<Self, FormatError> {
        let bytes: &[u8; Self::ENCODED_LEN] = first_chunk(bytes)?;
        let (payload_len, checksum) = bytes.split_at(Self::CHECKSUM_AT);
        let payload_len = checked_payload_len(
            u64::from(u32::from_le_bytes(array(payload_len))),
            max_payload_len.min(Self::MAX_PAYLOAD_LEN),
        )?;
        Ok(Self {
            payload_len,
            checksum: Checksum::decode(array(checksum)),
        })
    }

    /// The payload at the start of `following`, the bytes after this header,
    /// once its checksum has been verified.
    pub fn verified_payload<'a>(&self, following: &'a [u8]) -> Result<&'a [u8], FormatError> {
        let payload = following
            .get(..self.payload_len())
            .ok_or(FormatError::Truncated {
                needed: self.payload_len(),
                available: following.len(),
            })?;
        self.checksum
            .verify(&[&self.payload_len.to_le_bytes(), payload])?;
        Ok(payload)
    }
}

/// Appends `payload` to `out` as one framed record.
pub fn write_record(payload: &[u8], out: &mut Vec<u8>) -> Result<(), FormatError> {
    let header = RecordHeader::for_payload(payload)?;
    out.reserve(RecordHeader::ENCODED_LEN + payload.len());
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(payload);
    Ok(())
}

/// Reads the framed record at the start of `bytes`.
///
/// Returns its verified payload and the bytes after the record.
pub fn read_record(bytes: &[u8], max_payload_len: usize) -> Result<(&[u8], &[u8]), FormatError> {
    let header = RecordHeader::decode(bytes, max_payload_len)?;
    let record_len = RecordHeader::ENCODED_LEN + header.payload_len();
    let (record, rest) = bytes
        .split_at_checked(record_len)
        .ok_or(FormatError::Truncated {
            needed: record_len,
            available: bytes.len(),
        })?;
    let payload = header.verified_payload(&record[RecordHeader::ENCODED_LEN..])?;
    Ok((payload, rest))
}

fn checked_payload_len(len: u64, max: usize) -> Result<u32, FormatError> {
    let max = max as u64;
    if len > max {
        return Err(FormatError::RecordTooLong { len, max });
    }
    u32::try_from(len).map_err(|_| FormatError::RecordTooLong { len, max })
}

fn first_chunk<const N: usize>(bytes: &[u8]) -> Result<&[u8; N], FormatError> {
    bytes.first_chunk().ok_or(FormatError::Truncated {
        needed: N,
        available: bytes.len(),
    })
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut array = [0; N];
    array.copy_from_slice(bytes);
    array
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::SplitMix64;

    fn with_checksum(mut bytes: [u8; FileHeader::ENCODED_LEN]) -> [u8; FileHeader::ENCODED_LEN] {
        let checksum = Checksum::of(&[&bytes[..16]]);
        bytes[16..].copy_from_slice(&checksum.encode());
        bytes
    }

    fn sample_header() -> FileHeader {
        FileHeader::new(FileKind::Segment, FormatVersion::V1, 310).expect("valid")
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn format_version_round_trips_and_rejects_every_other_number() {
        assert_eq!(FormatVersion::CURRENT, FormatVersion::V1);
        assert!(FormatVersion::OLDEST_READABLE <= FormatVersion::CURRENT);
        assert_eq!(FormatVersion::ENCODED_LEN, 2);
        for raw in 0..=u16::MAX {
            match FormatVersion::new(raw) {
                Ok(version) => {
                    assert_eq!(version.get(), raw);
                    assert!(
                        (FormatVersion::OLDEST_READABLE.get()..=FormatVersion::CURRENT.get())
                            .contains(&raw)
                    );
                }
                Err(error) => {
                    assert_eq!(error, FormatError::UnsupportedFormatVersion { found: raw });
                }
            }
        }
        assert_eq!(FormatVersion::new(1), Ok(FormatVersion::V1));
        assert!(FormatVersion::new(0).is_err());
        assert!(FormatVersion::new(2).is_err());
    }

    #[test]
    fn file_kind_round_trips_and_rejects_every_other_code() {
        let mut known = 0;
        for code in 0..=u16::MAX {
            match FileKind::from_code(code, FormatVersion::V1) {
                Ok(kind) => {
                    assert_eq!(kind.code(), code);
                    assert!(FileKind::ALL.contains(&kind));
                    known += 1;
                }
                Err(error) => assert_eq!(error, FormatError::UnknownFileKind { code }),
            }
        }
        assert_eq!(known, FileKind::ALL.len());
        assert!(FileKind::from_code(0, FormatVersion::V1).is_err());
        assert_eq!(
            FileKind::ALL.map(FileKind::code),
            [1, 2, 3, 4, 5],
            "file kind codes are part of the format"
        );
        assert_eq!(
            FileKind::ALL.map(FileKind::is_numbered),
            [false, true, true, true, true]
        );
    }

    #[test]
    fn file_header_matches_the_golden_bytes() {
        let golden = [
            (
                FileKind::Manifest,
                0,
                "4c49474e494e444201000100000000005931c85e",
            ),
            (
                FileKind::Wal,
                45,
                "4c49474e494e4442020001002d000000dde85e0d",
            ),
            (
                FileKind::Segment,
                310,
                "4c49474e494e44420300010036010000d2c72fa6",
            ),
            (
                FileKind::ValueLog,
                12,
                "4c49474e494e4442040001000c000000e6767d89",
            ),
            (
                FileKind::PageTable,
                122,
                "4c49474e494e4442050001007a0000000ba8756f",
            ),
        ];
        for (kind, sequence, expected) in golden {
            let header = FileHeader::new(kind, FormatVersion::V1, sequence).expect("valid");
            assert_eq!(hex(&header.encode()), expected);
        }
    }

    #[test]
    fn file_header_round_trips() {
        let mut rng = SplitMix64::new(0xF11E);
        let sequences: Vec<u32> = [0, 1, 2, 99_999_999, 100_000_000, u32::MAX - 1, u32::MAX]
            .into_iter()
            .chain((0..2_000).map(|_| rng.next_u64() as u32))
            .collect();
        for kind in FileKind::ALL {
            for &sequence in &sequences {
                let header = match FileHeader::new(kind, FormatVersion::CURRENT, sequence) {
                    Ok(header) => header,
                    Err(error) => {
                        assert_eq!(error, FormatError::InvalidFileSequence { kind });
                        assert!(kind.is_numbered() && [0, u32::MAX].contains(&sequence));
                        continue;
                    }
                };
                assert_eq!(header.kind(), kind);
                assert_eq!(header.version(), FormatVersion::CURRENT);
                assert_eq!(header.sequence(), sequence);
                assert_eq!(
                    header.file_id(),
                    FileId::new(sequence).filter(|_| kind.is_numbered())
                );

                let bytes = header.encode();
                let decoded = FileHeader::decode(&bytes).expect("decodes");
                assert_eq!(decoded, header);
                assert_eq!(decoded.encode(), bytes);
            }
        }
    }

    #[test]
    fn file_header_decode_ignores_what_follows_it() {
        let mut bytes = sample_header().encode().to_vec();
        bytes.extend_from_slice(&[0xAB; 100]);
        assert_eq!(FileHeader::decode(&bytes), Ok(sample_header()));
    }

    #[test]
    fn file_header_rejects_every_shorter_input() {
        let bytes = sample_header().encode();
        for available in 0..bytes.len() {
            assert_eq!(
                FileHeader::decode(&bytes[..available]),
                Err(FormatError::Truncated {
                    needed: 20,
                    available,
                })
            );
        }
    }

    #[test]
    fn file_header_decodes_only_bytes_it_would_encode() {
        let mut rng = SplitMix64::new(0xDEC1);
        let (mut accepted, mut rejected) = (0, 0);
        for _ in 0..20_000 {
            let code = (rng.next_u64() % 8) as u16;
            let version = (rng.next_u64() % 4) as u16;
            let sequence = match rng.next_u64() % 4 {
                0 => 0,
                _ => rng.next_u64() as u32,
            };
            let mut bytes = [0; FileHeader::ENCODED_LEN];
            bytes[..8].copy_from_slice(&FileHeader::MAGIC);
            bytes[8..10].copy_from_slice(&code.to_le_bytes());
            bytes[10..12].copy_from_slice(&version.to_le_bytes());
            bytes[12..16].copy_from_slice(&sequence.to_le_bytes());
            let bytes = with_checksum(bytes);

            let valid = version == 1 && (code == 1 || ((2..=5).contains(&code) && sequence != 0));
            match FileHeader::decode(&bytes) {
                Ok(header) => {
                    assert!(valid);
                    assert_eq!(header.encode(), bytes);
                    accepted += 1;
                }
                Err(_) => {
                    assert!(!valid);
                    rejected += 1;
                }
            }
        }
        assert!(accepted > 1_000 && rejected > 1_000);
    }

    #[test]
    fn file_header_checks_run_in_the_specified_order() {
        assert_eq!(
            FileHeader::decode(&[0; 19]),
            Err(FormatError::Truncated {
                needed: 20,
                available: 19,
            })
        );

        let mut bytes = sample_header().encode();
        bytes[0] ^= 1;
        bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(FileHeader::decode(&bytes), Err(FormatError::BadMagic));

        let mut bytes = sample_header().encode();
        bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            FileHeader::decode(&bytes),
            Err(FormatError::ChecksumMismatch { .. })
        ));

        let mut bytes = sample_header().encode();
        bytes[8..10].copy_from_slice(&900u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&[0; 4]);
        assert_eq!(
            FileHeader::decode(&with_checksum(bytes)),
            Err(FormatError::UnsupportedFormatVersion { found: 2 })
        );

        let mut bytes = sample_header().encode();
        bytes[8..10].copy_from_slice(&900u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&[0; 4]);
        assert_eq!(
            FileHeader::decode(&with_checksum(bytes)),
            Err(FormatError::UnknownFileKind { code: 900 })
        );

        let mut bytes = sample_header().encode();
        bytes[12..16].copy_from_slice(&[0; 4]);
        assert_eq!(
            FileHeader::decode(&with_checksum(bytes)),
            Err(FormatError::InvalidFileSequence {
                kind: FileKind::Segment
            })
        );
    }

    #[test]
    fn file_header_rejects_every_single_bit_flip() {
        for kind in FileKind::ALL {
            let bytes = FileHeader::new(kind, FormatVersion::V1, 7)
                .expect("valid")
                .encode();
            for bit in 0..bytes.len() * 8 {
                let mut damaged = bytes;
                damaged[bit / 8] ^= 1 << (bit % 8);
                let error = FileHeader::decode(&damaged).expect_err("damaged header");
                if bit < 64 {
                    assert_eq!(error, FormatError::BadMagic);
                } else {
                    assert!(matches!(error, FormatError::ChecksumMismatch { .. }));
                }
            }
        }
    }

    #[test]
    fn file_header_rejects_zero_filled_bytes() {
        assert_eq!(FileHeader::decode(&[0; 64]), Err(FormatError::BadMagic));
        let mut zero_after_magic = [0; FileHeader::ENCODED_LEN];
        zero_after_magic[..8].copy_from_slice(&FileHeader::MAGIC);
        assert!(matches!(
            FileHeader::decode(&zero_after_magic),
            Err(FormatError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn file_header_rejects_an_unreadable_version_with_a_valid_checksum() {
        for found in (0..=u16::MAX).filter(|&raw| FormatVersion::new(raw).is_err()) {
            let mut bytes = sample_header().encode();
            bytes[10..12].copy_from_slice(&found.to_le_bytes());
            assert_eq!(
                FileHeader::decode(&with_checksum(bytes)),
                Err(FormatError::UnsupportedFormatVersion { found })
            );
        }
    }

    #[test]
    fn file_header_rejects_an_unknown_kind_with_a_valid_checksum() {
        for code in (0..=u16::MAX).filter(|&code| !(1..=5).contains(&code)) {
            let mut bytes = sample_header().encode();
            bytes[8..10].copy_from_slice(&code.to_le_bytes());
            assert_eq!(
                FileHeader::decode(&with_checksum(bytes)),
                Err(FormatError::UnknownFileKind { code })
            );
        }
    }

    #[test]
    fn file_header_rejects_a_numbered_file_whose_sequence_is_no_file_id() {
        for kind in FileKind::ALL.into_iter().filter(|kind| kind.is_numbered()) {
            for no_id in [0, FileId::RESERVED] {
                assert_eq!(
                    FileHeader::new(kind, FormatVersion::V1, no_id),
                    Err(FormatError::InvalidFileSequence { kind })
                );
                let mut bytes = FileHeader::new(kind, FormatVersion::V1, 1)
                    .expect("valid")
                    .encode();
                bytes[12..16].copy_from_slice(&no_id.to_le_bytes());
                assert_eq!(
                    FileHeader::decode(&with_checksum(bytes)),
                    Err(FormatError::InvalidFileSequence { kind })
                );
            }
            let last = FileHeader::new(kind, FormatVersion::V1, FileId::MAX.get()).expect("valid");
            assert_eq!(last.file_id(), Some(FileId::MAX));
            assert_eq!(FileHeader::decode(&last.encode()), Ok(last));
        }
        for sequence in [0, FileId::RESERVED] {
            let manifest =
                FileHeader::new(FileKind::Manifest, FormatVersion::V1, sequence).expect("valid");
            assert_eq!(manifest.file_id(), None);
            assert_eq!(FileHeader::decode(&manifest.encode()), Ok(manifest));
        }
    }

    #[test]
    fn record_matches_the_golden_bytes() {
        let mut out = Vec::new();
        write_record(b"", &mut out).expect("fits");
        assert_eq!(hex(&out), "00000000a67b113a");
        out.clear();
        write_record(b"lignin", &mut out).expect("fits");
        assert_eq!(hex(&out), "060000002d50d4416c69676e696e");
    }

    #[test]
    fn records_round_trip_one_after_another() {
        let mut rng = SplitMix64::new(0x4EC0);
        let payloads: Vec<Vec<u8>> = (0..200)
            .chain([4095, 4096, 4097, 70_000])
            .map(|len| rng.bytes(len))
            .collect();
        let mut file = Vec::new();
        for payload in &payloads {
            let before = file.len();
            write_record(payload, &mut file).expect("fits");
            assert_eq!(
                file.len() - before,
                RecordHeader::ENCODED_LEN + payload.len()
            );

            let header = RecordHeader::for_payload(payload).expect("fits");
            assert_eq!(header.payload_len(), payload.len());
            assert_eq!(file[before..before + 8], header.encode());
            assert_eq!(
                RecordHeader::decode(&header.encode(), payload.len()),
                Ok(header)
            );
            assert_eq!(header.verified_payload(payload), Ok(payload.as_slice()));
        }

        let mut rest = file.as_slice();
        for payload in &payloads {
            let (read, after) = read_record(rest, 70_000).expect("valid record");
            assert_eq!(read, payload.as_slice());
            rest = after;
        }
        assert!(rest.is_empty());
        assert_eq!(
            read_record(rest, 70_000),
            Err(FormatError::Truncated {
                needed: 8,
                available: 0,
            })
        );
    }

    #[test]
    fn record_rejects_every_shorter_input() {
        let mut record = Vec::new();
        write_record(b"twelve bytes", &mut record).expect("fits");
        for available in 0..record.len() {
            let needed = if available < 8 { 8 } else { 20 };
            assert_eq!(
                read_record(&record[..available], 12),
                Err(FormatError::Truncated { needed, available })
            );
        }

        let header = RecordHeader::decode(&record, 12).expect("valid header");
        assert_eq!(
            header.verified_payload(&record[8..19]),
            Err(FormatError::Truncated {
                needed: 12,
                available: 11,
            })
        );
    }

    #[test]
    fn record_rejects_every_single_bit_flip() {
        const MAX: usize = 1 << 20;
        let mut rng = SplitMix64::new(0xB17);
        for len in [0, 1, 40, 300] {
            let mut record = Vec::new();
            write_record(&rng.bytes(len), &mut record).expect("fits");
            for bit in 0..record.len() * 8 {
                let mut damaged = record.clone();
                damaged[bit / 8] ^= 1 << (bit % 8);
                let error = read_record(&damaged, MAX).expect_err("damaged record");

                let damaged_len = if bit < 32 { len ^ (1 << bit) } else { len };
                if damaged_len > MAX {
                    assert_eq!(
                        error,
                        FormatError::RecordTooLong {
                            len: damaged_len as u64,
                            max: MAX as u64,
                        }
                    );
                } else if damaged_len > len {
                    assert_eq!(
                        error,
                        FormatError::Truncated {
                            needed: 8 + damaged_len,
                            available: record.len(),
                        }
                    );
                } else {
                    assert!(
                        matches!(error, FormatError::ChecksumMismatch { .. }),
                        "bit {bit}: {error}"
                    );
                }
            }
        }
    }

    #[test]
    fn record_header_decodes_only_bytes_it_would_encode() {
        let mut rng = SplitMix64::new(0xDEC0);
        for _ in 0..20_000 {
            let mut bytes = rng.next_u64().to_le_bytes();
            if rng.next_u64().is_multiple_of(2) {
                bytes[2..4].fill(0);
            }
            let len = u32::from_le_bytes(array(&bytes[..4])) as usize;
            let max = (rng.next_u64() % (1 << 17)) as usize;
            match RecordHeader::decode(&bytes, max) {
                Ok(header) => {
                    assert!(len <= max);
                    assert_eq!(header.payload_len(), len);
                    assert_eq!(header.checksum().encode(), bytes[4..]);
                    assert_eq!(header.encode(), bytes);
                }
                Err(error) => {
                    assert!(len > max);
                    assert_eq!(
                        error,
                        FormatError::RecordTooLong {
                            len: len as u64,
                            max: max as u64,
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn record_rejects_zero_filled_bytes() {
        for len in (8..200).chain([4096, 1 << 20, 1 << 24]) {
            assert!(matches!(
                read_record(&vec![0; len], 1 << 20),
                Err(FormatError::ChecksumMismatch { .. })
            ));
        }
    }

    #[test]
    fn record_length_is_bounded_before_the_payload_is_read() {
        let header = RecordHeader::for_payload(&[7; 100]).expect("fits");
        assert_eq!(RecordHeader::decode(&header.encode(), 100), Ok(header));
        assert_eq!(
            RecordHeader::decode(&header.encode(), 99),
            Err(FormatError::RecordTooLong { len: 100, max: 99 })
        );

        let max = RecordHeader::MAX_PAYLOAD_LEN;
        assert_eq!(max, (1 << 27) - 4);
        let mut bytes = [0; RecordHeader::ENCODED_LEN];
        bytes[..4].copy_from_slice(&(max as u32).to_le_bytes());
        assert_eq!(
            RecordHeader::decode(&bytes, usize::MAX).map(|header| header.payload_len()),
            Ok(max)
        );
        bytes[..4].copy_from_slice(&(max as u32 + 1).to_le_bytes());
        assert_eq!(
            RecordHeader::decode(&bytes, usize::MAX),
            Err(FormatError::RecordTooLong {
                len: max as u64 + 1,
                max: max as u64,
            })
        );
        bytes[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            RecordHeader::decode(&bytes, usize::MAX),
            Err(FormatError::RecordTooLong { .. })
        ));

        let too_long = vec![0; max + 1];
        assert_eq!(
            RecordHeader::for_payload(&too_long),
            Err(FormatError::RecordTooLong {
                len: max as u64 + 1,
                max: max as u64,
            })
        );
        let mut out = Vec::new();
        assert!(write_record(&too_long, &mut out).is_err());
        assert!(out.is_empty());
    }
}
