//! The manifest, `MANIFEST`: which files are live, how much of each is valid,
//! the page-table checkpoint and the database format version.
//!
//! This module is the specification of the manifest in format version 1, on
//! top of the shared rules in `docs/lignindb/format.md` (cited below as B1 to
//! B11, R1 to R7, H1 to H5, V1 to V5 and U1 to U10).
//!
//! # File layout
//!
//! ```text
//! file header   20 bytes: kind 1 (manifest), sequence 0
//! one record    in the default frame; its payload is the manifest body
//! ```
//!
//! Nothing follows the record. The manifest is never appended to: the store
//! replaces the whole file (B11).
//!
//! - **The format version in the file header is the database format
//!   version** (V3). The body has no version field of its own.
//! - **The sequence field of the file header is 0.** The manifest is not
//!   numbered and nothing counts its replacements. A reader rejects any other
//!   value ([`FormatError::WrongFileSequence`]).
//!
//! # Body
//!
//! ```text
//! offset  size  field
//!      0     4  last file id used for a log file, or 0
//!      4     4  last file id used for a segment, or 0
//!      8     4  last file id used for a value-log file, or 0
//!     12     4  last file id used for a page-table checkpoint, or 0
//!     16     4  page-table checkpoint: file id, or 0 for none
//!     20     8  page-table checkpoint: valid length, u64
//!     28     8  page-table checkpoint: commit version it covers, u64
//!     36    16  one entry for each live file
//! ```
//!
//! The number of entries is not stored: it is the rest of the body divided by
//! 16. A manifest names at most 2^20 files ([`MAX_LIVE_FILES`]).
//!
//! ## Last file ids
//!
//! For each numbered kind, the highest id this database directory has ever
//! given to a file of that kind, or 0 if it never created one. A file id is
//! never reused, even after its file is deleted, and this is what remembers
//! the ids of deleted files. Every id in the manifest is at most the last id
//! of its kind. A store gives a new file a higher id than the last one.
//!
//! ## Page-table checkpoint
//!
//! The manifest names one page-table checkpoint, `table/<n>.pt`, by its file
//! id, the length of the file that is valid, and the commit version it
//! covers. What "covers" means for replay, and the layout of the file, are
//! the page table's to define.
//!
//! A database that has no checkpoint yet stores file id 0, valid length 0 and
//! the "no version" pattern, all ones.
//!
//! ## Live files
//!
//! ```text
//! offset  size  field
//!      0     2  file kind, as in the file header: 2 log, 3 segment, 4 value log
//!      2     1  state: 1 open, 2 closed, 3 sealed
//!      3     1  reserved, 0
//!      4     4  file id (never 0)
//!      8     8  valid length, u64
//! ```
//!
//! Entries are in strictly ascending order of kind code, then file id. So a
//! file is named once, and a set of files has one encoding.
//!
//! **Valid length.** Every byte of the file below its valid length belongs to
//! a whole record (R1). It is at least 20: a manifest names a file only once
//! its header is written (H5).
//!
//! **State** says what the bytes past the valid length are.
//!
//! - **Open (1).** The session that wrote the manifest was still appending to
//!   the file, so whole records may follow the valid length. Whether a later
//!   reader uses them is the rule of the file's own format and of the store:
//!   a log is replayed up to its last complete group.
//! - **Closed (2).** The file ends at its valid length for this database.
//!   Bytes past it are not part of the database, whatever they hold. A
//!   checkpoint names the files it shares with the live database this way,
//!   and recovery names the files of an earlier session this way.
//! - **Sealed (3).** A segment only. As closed, and the valid bytes end with
//!   the segment's footer index ([`crate::segment`]). The valid length is at
//!   least 48: a file header, an empty index record and a trailer.
//!
//! A segment says nowhere in itself whether it has a footer, because a
//! segment that a checkpoint shares may gain one later, past the checkpoint's
//! valid length. Only the manifest can say.
//!
//! The page-table checkpoint is not in this list and has no state. It is
//! written whole before the manifest names it.
//!
//! # What a reader checks
//!
//! In this order: the file header (H4); the kind is manifest; the sequence is
//! 0; the record's length and checksum; that nothing follows the record; then
//! the body, by the format version in the header. Every rule above is
//! checked, so a manifest has exactly one encoding (B8).
//!
//! # Example
//!
//! ```
//! use lignin_format::{
//!     CommitVersion, FileId, FileKind,
//!     manifest::{FileState, LastFileIds, LiveFile, Manifest, TableCheckpoint},
//! };
//!
//! # fn main() -> Result<(), lignin_format::FormatError> {
//! let id = |raw| FileId::new(raw).expect("not zero");
//! let last_file_ids = LastFileIds {
//!     wal: Some(id(45)),
//!     segment: Some(id(310)),
//!     value_log: None,
//!     page_table: Some(id(122)),
//! };
//! let covers = CommitVersion::new(9000).expect("a version");
//! let table = TableCheckpoint::new(id(122), 4116, covers)?;
//! let files = vec![
//!     LiveFile::new(FileKind::Segment, id(310), FileState::Open, 123_456)?,
//!     LiveFile::new(FileKind::Segment, id(309), FileState::Sealed, 67_108_864)?,
//!     LiveFile::new(FileKind::Wal, id(45), FileState::Open, 20)?,
//! ];
//! let manifest = Manifest::new(last_file_ids, Some(table), files)?;
//!
//! // The bytes of the whole `MANIFEST` file, and back.
//! let bytes = manifest.encode()?;
//! assert_eq!(Manifest::decode(&bytes)?, manifest);
//! assert_eq!(manifest.files()[0].kind(), FileKind::Wal);
//! # Ok(())
//! # }
//! ```

use crate::{
    error::FormatError,
    header::{read_record, write_record, FileHeader, FileKind, FormatVersion},
    ids::{CommitVersion, FileId},
    segment::{SegmentTrailer, FRAME_LEN},
    wire::take,
};

/// The most live files one manifest may name.
pub const MAX_LIVE_FILES: usize = 1 << 20;

const FIXED_LEN: usize = 36;
const ENTRY_LEN: usize = 16;
const MAX_BODY_LEN: usize = FIXED_LEN + MAX_LIVE_FILES * ENTRY_LEN;
const MIN_VALID_LEN: u64 = FileHeader::ENCODED_LEN as u64;
const MIN_SEALED_LEN: u64 = MIN_VALID_LEN + (FRAME_LEN + SegmentTrailer::ENCODED_LEN) as u64;

const HEADER: FileHeader = match FileHeader::new(FileKind::Manifest, FormatVersion::CURRENT, 0) {
    Ok(header) => header,
    Err(_) => panic!("the manifest header is valid"),
};

/// What the bytes of a live file past its valid length are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FileState {
    /// The session that wrote the manifest was still appending to the file.
    /// Whole records may follow the valid length.
    Open = 1,
    /// The file ends at its valid length for this database. Bytes past it
    /// are not part of the database.
    Closed = 2,
    /// A segment that ends at its valid length, with its footer index as the
    /// last valid bytes.
    Sealed = 3,
}

impl FileState {
    /// The state with this code in a manifest of format `version`.
    pub const fn from_code(code: u8, version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => match code {
                1 => Ok(Self::Open),
                2 => Ok(Self::Closed),
                3 => Ok(Self::Sealed),
                code => Err(FormatError::UnknownFileState { code }),
            },
        }
    }

    /// The code stored in the manifest.
    pub const fn code(self) -> u8 {
        self as u8
    }
}

/// One live file as the manifest names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LiveFile {
    kind: FileKind,
    id: FileId,
    state: FileState,
    valid_len: u64,
}

impl LiveFile {
    /// A live file of `kind` with this id, state and valid length.
    ///
    /// The kind is a log, a segment or a value log. Only a segment can be
    /// sealed. The valid length is at least 20, and at least 48 for a sealed
    /// segment. Anything else is [`FormatError::InvalidManifestEntry`].
    pub const fn new(
        kind: FileKind,
        id: FileId,
        state: FileState,
        valid_len: u64,
    ) -> Result<Self, FormatError> {
        let min_len = match (kind, state) {
            (FileKind::Segment, FileState::Sealed) => MIN_SEALED_LEN,
            (
                FileKind::Wal | FileKind::Segment | FileKind::ValueLog,
                FileState::Open | FileState::Closed,
            ) => MIN_VALID_LEN,
            _ => return Err(FormatError::InvalidManifestEntry),
        };
        if valid_len < min_len {
            return Err(FormatError::InvalidManifestEntry);
        }
        Ok(Self {
            kind,
            id,
            state,
            valid_len,
        })
    }

    /// The file's kind: a log, a segment or a value log.
    pub const fn kind(&self) -> FileKind {
        self.kind
    }

    /// The file's id, the number in its name.
    pub const fn id(&self) -> FileId {
        self.id
    }

    /// What the bytes past the valid length are.
    pub const fn state(&self) -> FileState {
        self.state
    }

    /// How many bytes of the file, from its start, are valid.
    pub const fn valid_len(&self) -> u64 {
        self.valid_len
    }

    const fn order(&self) -> (u16, FileId) {
        (self.kind.code(), self.id)
    }
}

/// The page-table checkpoint a manifest names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableCheckpoint {
    file: FileId,
    valid_len: u64,
    covers: CommitVersion,
}

impl TableCheckpoint {
    /// The checkpoint in `table/<file>.pt`, of which `valid_len` bytes are
    /// valid, covering commit version `covers`.
    ///
    /// The valid length is at least 20, or the error is
    /// [`FormatError::InvalidManifestEntry`].
    pub const fn new(
        file: FileId,
        valid_len: u64,
        covers: CommitVersion,
    ) -> Result<Self, FormatError> {
        if valid_len < MIN_VALID_LEN {
            return Err(FormatError::InvalidManifestEntry);
        }
        Ok(Self {
            file,
            valid_len,
            covers,
        })
    }

    /// The id of the checkpoint file.
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// How many bytes of the checkpoint file, from its start, are valid.
    pub const fn valid_len(&self) -> u64 {
        self.valid_len
    }

    /// The commit version the checkpoint covers.
    pub const fn covers(&self) -> CommitVersion {
        self.covers
    }
}

/// For each numbered file kind, the highest id the database directory has
/// ever given to a file of that kind. `None` if it never created one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LastFileIds {
    /// The last id used for a log file.
    pub wal: Option<FileId>,
    /// The last id used for a segment.
    pub segment: Option<FileId>,
    /// The last id used for a value-log file.
    pub value_log: Option<FileId>,
    /// The last id used for a page-table checkpoint.
    pub page_table: Option<FileId>,
}

impl LastFileIds {
    /// The last id used for a file of `kind`. `None` for the manifest, which
    /// is not numbered.
    pub const fn of(&self, kind: FileKind) -> Option<FileId> {
        match kind {
            FileKind::Manifest => None,
            FileKind::Wal => self.wal,
            FileKind::Segment => self.segment,
            FileKind::ValueLog => self.value_log,
            FileKind::PageTable => self.page_table,
        }
    }
}

/// The contents of `MANIFEST`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Manifest {
    version: FormatVersion,
    last_file_ids: LastFileIds,
    table_checkpoint: Option<TableCheckpoint>,
    files: Vec<LiveFile>,
}

impl Manifest {
    /// A manifest at the format version this build writes.
    ///
    /// `files` may be in any order. Rejects a file named twice
    /// ([`FormatError::ManifestEntriesOutOfOrder`]), more than
    /// [`MAX_LIVE_FILES`] files ([`FormatError::RecordTooLong`]), and a file or
    /// a checkpoint whose id is above the last id of its kind
    /// ([`FormatError::InvalidManifestEntry`]).
    pub fn new(
        last_file_ids: LastFileIds,
        table_checkpoint: Option<TableCheckpoint>,
        mut files: Vec<LiveFile>,
    ) -> Result<Self, FormatError> {
        files.sort_unstable_by_key(LiveFile::order);
        Self::checked(
            FormatVersion::CURRENT,
            last_file_ids,
            table_checkpoint,
            files,
        )
    }

    fn checked(
        version: FormatVersion,
        last_file_ids: LastFileIds,
        table_checkpoint: Option<TableCheckpoint>,
        files: Vec<LiveFile>,
    ) -> Result<Self, FormatError> {
        if files.len() > MAX_LIVE_FILES {
            return Err(FormatError::RecordTooLong {
                len: (FIXED_LEN as u64).saturating_add(files.len() as u64 * ENTRY_LEN as u64),
                max: MAX_BODY_LEN as u64,
            });
        }
        if !files.is_sorted_by(|a, b| a.order() < b.order()) {
            return Err(FormatError::ManifestEntriesOutOfOrder);
        }
        let named = files
            .iter()
            .map(|file| (file.kind, file.id))
            .chain(table_checkpoint.map(|table| (FileKind::PageTable, table.file)));
        for (kind, id) in named {
            if last_file_ids.of(kind) < Some(id) {
                return Err(FormatError::InvalidManifestEntry);
            }
        }
        Ok(Self {
            version,
            last_file_ids,
            table_checkpoint,
            files,
        })
    }

    /// The database format version: the version in the manifest's file
    /// header.
    pub const fn format_version(&self) -> FormatVersion {
        self.version
    }

    /// The last file id used for each numbered kind.
    pub const fn last_file_ids(&self) -> LastFileIds {
        self.last_file_ids
    }

    /// The page-table checkpoint, if the database has one.
    pub const fn table_checkpoint(&self) -> Option<TableCheckpoint> {
        self.table_checkpoint
    }

    /// The live files, in ascending order of kind code and then id.
    pub fn files(&self) -> &[LiveFile] {
        &self.files
    }

    /// The whole `MANIFEST` file, written at [`FormatVersion::CURRENT`]
    /// whatever version this manifest was read at (U1, U3).
    pub fn encode(&self) -> Result<Vec<u8>, FormatError> {
        let mut body = Vec::with_capacity(FIXED_LEN + self.files.len() * ENTRY_LEN);
        for kind in [
            FileKind::Wal,
            FileKind::Segment,
            FileKind::ValueLog,
            FileKind::PageTable,
        ] {
            body.extend_from_slice(&FileId::encode_optional(self.last_file_ids.of(kind)));
        }
        let table = self.table_checkpoint;
        body.extend_from_slice(&FileId::encode_optional(table.map(|table| table.file)));
        body.extend_from_slice(&table.map_or(0, |table| table.valid_len).to_le_bytes());
        body.extend_from_slice(&CommitVersion::encode_optional(
            table.map(|table| table.covers),
        ));
        for file in &self.files {
            body.extend_from_slice(&file.kind.code().to_le_bytes());
            body.extend_from_slice(&[file.state.code(), 0]);
            body.extend_from_slice(&file.id.encode());
            body.extend_from_slice(&file.valid_len.to_le_bytes());
        }

        let mut out = Vec::with_capacity(FileHeader::ENCODED_LEN + FRAME_LEN + body.len());
        out.extend_from_slice(&HEADER.encode());
        write_record(&body, &mut out)?;
        Ok(out)
    }

    /// Reads a manifest from `bytes`, the whole `MANIFEST` file.
    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        let header = FileHeader::decode(bytes)?;
        if header.kind() != FileKind::Manifest {
            return Err(FormatError::WrongFileKind {
                expected: FileKind::Manifest,
                found: header.kind(),
            });
        }
        if header.sequence() != HEADER.sequence() {
            return Err(FormatError::WrongFileSequence {
                expected: HEADER.sequence(),
                found: header.sequence(),
            });
        }
        let record = bytes.get(FileHeader::ENCODED_LEN..).unwrap_or_default();
        let (body, rest) = read_record(record, MAX_BODY_LEN).map_err(|error| match error {
            FormatError::Truncated { needed, available } => FormatError::Truncated {
                needed: needed + FileHeader::ENCODED_LEN,
                available: available + FileHeader::ENCODED_LEN,
            },
            error => error,
        })?;
        if !rest.is_empty() {
            return Err(FormatError::InconsistentLength {
                expected: (bytes.len() - rest.len()) as u64,
                found: bytes.len() as u64,
            });
        }
        match header.version() {
            FormatVersion::V1 => Self::decode_v1(body),
        }
    }

    fn decode_v1(body: &[u8]) -> Result<Self, FormatError> {
        let version = FormatVersion::V1;
        let mut fields = body;
        let short = FormatError::InconsistentLength {
            expected: FIXED_LEN as u64,
            found: body.len() as u64,
        };
        let last_file_ids = LastFileIds {
            wal: FileId::decode(take(&mut fields).ok_or(short)?),
            segment: FileId::decode(take(&mut fields).ok_or(short)?),
            value_log: FileId::decode(take(&mut fields).ok_or(short)?),
            page_table: FileId::decode(take(&mut fields).ok_or(short)?),
        };
        let table_file = FileId::decode(take(&mut fields).ok_or(short)?);
        let table_valid_len = u64::from_le_bytes(take(&mut fields).ok_or(short)?);
        let table_covers = CommitVersion::decode(take(&mut fields).ok_or(short)?);
        let table_checkpoint = match (table_file, table_covers) {
            (Some(file), Some(covers)) => {
                Some(TableCheckpoint::new(file, table_valid_len, covers)?)
            }
            (Some(_), None) => return Err(FormatError::MissingId),
            (None, None) if table_valid_len == 0 => None,
            (None, _) => return Err(FormatError::InvalidManifestEntry),
        };

        let (entries, []) = fields.as_chunks::<ENTRY_LEN>() else {
            return Err(FormatError::InconsistentLength {
                expected: (body.len() - fields.len() % ENTRY_LEN) as u64,
                found: body.len() as u64,
            });
        };
        let mut files = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut entry = entry.as_slice();
            let (Some(kind), Some([state, reserved]), Some(id), Some(valid_len)) = (
                take(&mut entry),
                take(&mut entry),
                take(&mut entry),
                take(&mut entry),
            ) else {
                return Err(short);
            };
            let kind = FileKind::from_code(u16::from_le_bytes(kind), version)?;
            let state = FileState::from_code(state, version)?;
            if reserved != 0 {
                return Err(FormatError::NonZeroReserved);
            }
            let id = FileId::decode(id).ok_or(FormatError::MissingId)?;
            files.push(LiveFile::new(
                kind,
                id,
                state,
                u64::from_le_bytes(valid_len),
            )?);
        }
        Self::checked(version, last_file_ids, table_checkpoint, files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::SplitMix64;

    const GOLDEN_EMPTY: &str = "4c49474e494e444201000100000000005931c85e\
        2400000044320b72\
        00000000000000000000000000000000\
        000000000000000000000000ffffffffffffffff";
    const GOLDEN_FULL: &str = "4c49474e494e444201000100000000005931c85e\
        740000008287d324\
        2d000000360100000c0000007a000000\
        7a00000014100000000000002823000000000000\
        020002002c0000000000100000000000\
        020001002d0000001400000000000000\
        03000300350100000000000400000000\
        030001003601000040e2010000000000\
        040002000c00000000f2052a01000000";

    const NO_VERSION: u64 = u64::MAX;
    const LAST: [u32; 4] = [45, 310, 12, 122];
    const TABLE: (u32, u64, u64) = (122, 4116, 9000);
    const NO_TABLE: (u32, u64, u64) = (0, 0, NO_VERSION);

    type Entry = (u16, u8, u8, u32, u64);
    const ENTRIES: [Entry; 5] = [
        (2, 2, 0, 44, 1_048_576),
        (2, 1, 0, 45, 20),
        (3, 3, 0, 309, 67_108_864),
        (3, 1, 0, 310, 123_456),
        (4, 2, 0, 12, 5_000_000_000),
    ];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn id(raw: u32) -> FileId {
        FileId::new(raw).expect("non-zero")
    }

    fn version(raw: u64) -> CommitVersion {
        CommitVersion::new(raw).expect("a version")
    }

    fn file(kind: FileKind, raw_id: u32, state: FileState, valid_len: u64) -> LiveFile {
        LiveFile::new(kind, id(raw_id), state, valid_len).expect("valid entry")
    }

    fn last_ids(last: [u32; 4]) -> LastFileIds {
        let [wal, segment, value_log, page_table] = last.map(FileId::new);
        LastFileIds {
            wal,
            segment,
            value_log,
            page_table,
        }
    }

    fn full() -> Manifest {
        let table = TableCheckpoint::new(id(122), 4116, version(9000)).expect("valid");
        let files = vec![
            file(FileKind::ValueLog, 12, FileState::Closed, 5_000_000_000),
            file(FileKind::Segment, 310, FileState::Open, 123_456),
            file(FileKind::Wal, 45, FileState::Open, 20),
            file(FileKind::Segment, 309, FileState::Sealed, 67_108_864),
            file(FileKind::Wal, 44, FileState::Closed, 1_048_576),
        ];
        Manifest::new(last_ids(LAST), Some(table), files).expect("valid manifest")
    }

    fn body(last: [u32; 4], table: (u32, u64, u64), entries: &[Entry]) -> Vec<u8> {
        let mut body: Vec<u8> = last.iter().flat_map(|id| id.to_le_bytes()).collect();
        body.extend_from_slice(&table.0.to_le_bytes());
        body.extend_from_slice(&table.1.to_le_bytes());
        body.extend_from_slice(&table.2.to_le_bytes());
        for (kind, state, reserved, id, valid_len) in entries {
            body.extend_from_slice(&kind.to_le_bytes());
            body.extend_from_slice(&[*state, *reserved]);
            body.extend_from_slice(&id.to_le_bytes());
            body.extend_from_slice(&valid_len.to_le_bytes());
        }
        body
    }

    fn forge_file(header: FileHeader, body: &[u8]) -> Vec<u8> {
        let mut bytes = header.encode().to_vec();
        write_record(body, &mut bytes).expect("fits");
        bytes
    }

    fn forge(body: &[u8]) -> Vec<u8> {
        forge_file(HEADER, body)
    }

    fn decode_entries(entries: &[Entry]) -> Result<Manifest, FormatError> {
        Manifest::decode(&forge(&body(LAST, TABLE, entries)))
    }

    fn decode_with(change: impl FnOnce(&mut Entry)) -> Result<Manifest, FormatError> {
        let mut entries = ENTRIES;
        change(&mut entries[2]);
        decode_entries(&entries)
    }

    #[test]
    fn limits_are_the_ones_the_format_states() {
        assert_eq!(MAX_LIVE_FILES, 1 << 20);
        assert_eq!(MIN_VALID_LEN, 20);
        assert_eq!(MIN_SEALED_LEN, 48);
        assert_eq!(HEADER.kind(), FileKind::Manifest);
        assert_eq!(HEADER.version(), FormatVersion::CURRENT);
        assert_eq!(HEADER.sequence(), 0);
        assert_eq!(body([0; 4], NO_TABLE, &[]).len(), FIXED_LEN);
        assert_eq!(body(LAST, TABLE, &ENTRIES).len(), FIXED_LEN + 5 * ENTRY_LEN);
    }

    #[test]
    fn manifests_match_the_golden_bytes() {
        let empty = Manifest::new(LastFileIds::default(), None, Vec::new()).expect("valid");
        assert_eq!(hex(&empty.encode().expect("fits")), GOLDEN_EMPTY);
        assert_eq!(Manifest::decode(&unhex(GOLDEN_EMPTY)), Ok(empty));
        assert_eq!(unhex(GOLDEN_EMPTY), forge(&body([0; 4], NO_TABLE, &[])));

        assert_eq!(hex(&full().encode().expect("fits")), GOLDEN_FULL);
        assert_eq!(unhex(GOLDEN_FULL), forge(&body(LAST, TABLE, &ENTRIES)));
        let decoded = Manifest::decode(&unhex(GOLDEN_FULL)).expect("valid manifest");
        assert_eq!(decoded, full());
        assert_eq!(decoded.format_version(), FormatVersion::V1);
        assert_eq!(decoded.last_file_ids(), last_ids(LAST));
        for (kind, last) in FileKind::ALL.into_iter().zip([0, 45, 310, 12, 122]) {
            assert_eq!(decoded.last_file_ids().of(kind), FileId::new(last));
        }
        let table = decoded.table_checkpoint().expect("has a checkpoint");
        assert_eq!(
            (table.file().get(), table.valid_len(), table.covers().get()),
            TABLE
        );
        let files: Vec<Entry> = decoded
            .files()
            .iter()
            .map(|file| {
                let (kind, state) = (file.kind().code(), file.state().code());
                (kind, state, 0, file.id().get(), file.valid_len())
            })
            .collect();
        assert_eq!(files, ENTRIES);
    }

    #[test]
    fn file_state_round_trips_and_rejects_every_other_code() {
        for code in 0..=u8::MAX {
            match FileState::from_code(code, FormatVersion::V1) {
                Ok(state) => {
                    assert!((1..=3).contains(&code));
                    assert_eq!(state.code(), code);
                }
                Err(error) => {
                    assert!(!(1..=3).contains(&code));
                    assert_eq!(error, FormatError::UnknownFileState { code });
                }
            }
        }
        assert_eq!(
            [FileState::Open, FileState::Closed, FileState::Sealed].map(FileState::code),
            [1, 2, 3],
            "file state codes are part of the format"
        );
    }

    #[test]
    fn a_live_file_obeys_the_rules_of_its_kind_and_state() {
        let states = [FileState::Open, FileState::Closed, FileState::Sealed];
        for kind in FileKind::ALL {
            for state in states {
                for valid_len in [0, 19, 20, 47, 48, u64::MAX] {
                    let listed =
                        matches!(kind, FileKind::Wal | FileKind::Segment | FileKind::ValueLog);
                    let sealed = state == FileState::Sealed;
                    let valid = listed
                        && (!sealed || kind == FileKind::Segment)
                        && valid_len >= if sealed { 48 } else { 20 };
                    match LiveFile::new(kind, id(7), state, valid_len) {
                        Ok(file) => {
                            assert!(valid, "{kind:?} {state:?} {valid_len}");
                            assert_eq!(file.kind(), kind);
                            assert_eq!(file.id(), id(7));
                            assert_eq!(file.state(), state);
                            assert_eq!(file.valid_len(), valid_len);
                        }
                        Err(error) => {
                            assert!(!valid, "{kind:?} {state:?} {valid_len}");
                            assert_eq!(error, FormatError::InvalidManifestEntry);
                        }
                    }
                }
            }
        }
        for valid_len in [0, 19] {
            assert_eq!(
                TableCheckpoint::new(id(1), valid_len, CommitVersion::EMPTY),
                Err(FormatError::InvalidManifestEntry)
            );
        }
        let table = TableCheckpoint::new(id(1), 20, CommitVersion::EMPTY).expect("valid");
        assert_eq!(table.covers(), CommitVersion::EMPTY);
        assert!(TableCheckpoint::new(id(1), u64::MAX, CommitVersion::MAX).is_ok());
    }

    #[test]
    fn a_manifest_names_each_file_once_and_no_id_above_the_last_of_its_kind() {
        let closed = |kind, raw_id| file(kind, raw_id, FileState::Closed, 20);
        let last = last_ids([5, 5, 5, 5]);

        let files = vec![closed(FileKind::Wal, 3), closed(FileKind::Wal, 3)];
        assert_eq!(
            Manifest::new(last, None, files),
            Err(FormatError::ManifestEntriesOutOfOrder)
        );
        let files = vec![
            closed(FileKind::Segment, 3),
            file(FileKind::Segment, 3, FileState::Sealed, 48),
        ];
        assert_eq!(
            Manifest::new(last, None, files),
            Err(FormatError::ManifestEntriesOutOfOrder)
        );
        let same_id =
            [FileKind::Wal, FileKind::Segment, FileKind::ValueLog].map(|kind| closed(kind, 3));
        assert!(Manifest::new(last, None, same_id.to_vec()).is_ok());

        for kind in [FileKind::Wal, FileKind::Segment, FileKind::ValueLog] {
            assert!(Manifest::new(last, None, vec![closed(kind, 5)]).is_ok());
            assert_eq!(
                Manifest::new(last, None, vec![closed(kind, 6)]),
                Err(FormatError::InvalidManifestEntry)
            );
            assert_eq!(
                Manifest::new(LastFileIds::default(), None, vec![closed(kind, 1)]),
                Err(FormatError::InvalidManifestEntry)
            );
        }
        let table = |raw_id| TableCheckpoint::new(id(raw_id), 20, version(1)).ok();
        assert!(Manifest::new(last, table(5), Vec::new()).is_ok());
        assert_eq!(
            Manifest::new(last, table(6), Vec::new()),
            Err(FormatError::InvalidManifestEntry)
        );
        assert_eq!(
            Manifest::new(LastFileIds::default(), table(1), Vec::new()),
            Err(FormatError::InvalidManifestEntry)
        );
    }

    #[test]
    fn manifests_round_trip() {
        let mut rng = SplitMix64::new(0x3A21);
        let kinds = [FileKind::Wal, FileKind::Segment, FileKind::ValueLog];
        for round in 0..2_000 {
            let last = [0; 4].map(|_| match rng.next_u64() % 8 {
                0 => 0,
                1 => u32::MAX,
                _ => (rng.next_u64() % 60) as u32,
            });
            let last = last_ids(last);
            let mut files = Vec::new();
            for kind in kinds {
                let Some(last) = last.of(kind) else { continue };
                for raw_id in 1..=last.get().min(60) {
                    if rng.next_u64().is_multiple_of(3) {
                        continue;
                    }
                    let state = match rng.next_u64() % 3 {
                        0 => FileState::Open,
                        1 if kind == FileKind::Segment => FileState::Sealed,
                        _ => FileState::Closed,
                    };
                    let valid_len = match rng.next_u64() % 4 {
                        0 => 48,
                        1 => u64::MAX,
                        _ => 48 + rng.next_u64() % (1 << 40),
                    };
                    files.push(file(kind, raw_id, state, valid_len));
                }
            }
            for at in (1..files.len()).rev() {
                files.swap(at, (rng.next_u64() as usize) % (at + 1));
            }
            let table = last.page_table.filter(|_| round % 5 != 0).map(|last| {
                let file = id((rng.next_u64() as u32) % last.get() + 1);
                let covers =
                    version(rng.next_u64() >> (rng.next_u64() % 64)).min(CommitVersion::MAX);
                TableCheckpoint::new(file, 20 + rng.next_u64() % 9_000, covers).expect("valid")
            });

            let manifest = Manifest::new(last, table, files.clone()).expect("valid manifest");
            assert_eq!(manifest.format_version(), FormatVersion::CURRENT);
            assert_eq!(manifest.last_file_ids(), last);
            assert_eq!(manifest.table_checkpoint(), table);
            assert_eq!(manifest.files().len(), files.len());
            assert!(manifest.files().is_sorted_by(|a, b| a.order() < b.order()));
            assert!(files.iter().all(|file| manifest.files().contains(file)));

            let bytes = manifest.encode().expect("fits");
            assert_eq!(bytes.len(), 20 + 8 + 36 + 16 * files.len());
            let decoded = Manifest::decode(&bytes).expect("valid manifest");
            assert_eq!(decoded, manifest);
            assert_eq!(decoded.encode().expect("fits"), bytes);
        }
    }

    #[test]
    fn the_file_must_be_a_manifest_of_a_readable_version_with_sequence_zero() {
        let body = body(LAST, TABLE, &ENTRIES);
        for kind in FileKind::ALL.into_iter().filter(|kind| kind.is_numbered()) {
            let header = FileHeader::new(kind, FormatVersion::V1, 1).expect("valid");
            assert_eq!(
                Manifest::decode(&forge_file(header, &body)),
                Err(FormatError::WrongFileKind {
                    expected: FileKind::Manifest,
                    found: kind,
                })
            );
        }
        for sequence in [1, 310, u32::MAX] {
            let header =
                FileHeader::new(FileKind::Manifest, FormatVersion::V1, sequence).expect("valid");
            assert_eq!(
                Manifest::decode(&forge_file(header, &body)),
                Err(FormatError::WrongFileSequence {
                    expected: 0,
                    found: sequence,
                })
            );
        }

        let with_version = |found: u16| {
            let mut bytes = unhex(GOLDEN_FULL);
            bytes[10..12].copy_from_slice(&found.to_le_bytes());
            let checksum = crate::Checksum::of(&[&bytes[..16]]);
            bytes[16..20].copy_from_slice(&checksum.encode());
            bytes
        };
        assert_eq!(Manifest::decode(&with_version(1)), Ok(full()));
        for found in [0, 2, u16::MAX] {
            assert_eq!(
                Manifest::decode(&with_version(found)),
                Err(FormatError::UnsupportedFormatVersion { found })
            );
        }
        let mut other_engine = unhex(GOLDEN_FULL);
        other_engine[..8].copy_from_slice(b"MANIFEST");
        assert_eq!(Manifest::decode(&other_engine), Err(FormatError::BadMagic));
    }

    #[test]
    fn a_manifest_is_the_whole_file() {
        let bytes = unhex(GOLDEN_FULL);
        for available in 0..bytes.len() {
            let needed = match available {
                0..20 => 20,
                20..28 => 28,
                _ => bytes.len(),
            };
            assert_eq!(
                Manifest::decode(&bytes[..available]),
                Err(FormatError::Truncated { needed, available })
            );
        }
        for extra in [1, 8, 16, 144] {
            let mut longer = bytes.clone();
            longer.extend_from_slice(&bytes[..extra]);
            assert_eq!(
                Manifest::decode(&longer),
                Err(FormatError::InconsistentLength {
                    expected: 144,
                    found: 144 + extra as u64,
                })
            );
        }
    }

    #[test]
    fn the_body_is_the_fixed_fields_and_whole_entries() {
        let whole = body(LAST, TABLE, &ENTRIES);
        for len in 0..whole.len() {
            let expected = match len {
                0..36 => 36,
                len => (36 + (len - 36) / 16 * 16) as u64,
            };
            let decoded = Manifest::decode(&forge(&whole[..len]));
            if expected == len as u64 {
                assert_eq!(
                    decoded.map(|manifest| manifest.files().len()),
                    Ok((len - 36) / 16)
                );
            } else {
                assert_eq!(
                    decoded,
                    Err(FormatError::InconsistentLength {
                        expected,
                        found: len as u64,
                    })
                );
            }
        }
    }

    #[test]
    fn a_forged_entry_is_rejected() {
        assert_eq!(decode_entries(&ENTRIES), Ok(full()));
        let invalid = Err(FormatError::InvalidManifestEntry);

        for code in [0, 6, 300, u16::MAX] {
            assert_eq!(
                decode_with(|entry| entry.0 = code),
                Err(FormatError::UnknownFileKind { code })
            );
        }
        for kind in [1, 5] {
            let mut entries = ENTRIES.to_vec();
            entries.insert(if kind == 1 { 0 } else { 5 }, (kind, 2, 0, 1, 20));
            assert_eq!(decode_entries(&entries), invalid);
        }
        for code in [0, 4, u8::MAX] {
            assert_eq!(
                decode_with(|entry| entry.1 = code),
                Err(FormatError::UnknownFileState { code })
            );
        }
        for at in [0, 4] {
            let mut entries = ENTRIES;
            entries[at].1 = 3;
            assert_eq!(decode_entries(&entries), invalid);
        }
        for reserved in [1, 0x80, u8::MAX] {
            assert_eq!(
                decode_with(|entry| entry.2 = reserved),
                Err(FormatError::NonZeroReserved)
            );
        }
        assert_eq!(
            decode_with(|entry| entry.3 = 0),
            Err(FormatError::MissingId)
        );

        assert_eq!(decode_with(|entry| entry.4 = 47), invalid);
        assert!(decode_with(|entry| entry.4 = 48).is_ok());
        assert_eq!(decode_with(|entry| *entry = (3, 2, 0, 309, 19)), invalid);
        assert!(decode_with(|entry| *entry = (3, 2, 0, 309, 20)).is_ok());

        let mut above_last = ENTRIES;
        above_last[3].3 = 311;
        assert_eq!(decode_entries(&above_last), invalid);
        assert_eq!(
            decode_with(|entry| entry.3 = 310),
            Err(FormatError::ManifestEntriesOutOfOrder)
        );
        for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 4), (0, 4)] {
            let mut entries = ENTRIES;
            entries.swap(a, b);
            assert_eq!(
                decode_entries(&entries),
                Err(FormatError::ManifestEntriesOutOfOrder)
            );
        }
        let mut entries = ENTRIES.to_vec();
        entries.push(ENTRIES[4]);
        assert_eq!(
            decode_entries(&entries),
            Err(FormatError::ManifestEntriesOutOfOrder)
        );
    }

    #[test]
    fn forged_fixed_fields_are_rejected() {
        let decode = |last, table| Manifest::decode(&forge(&body(last, table, &ENTRIES)));
        let invalid = Err(FormatError::InvalidManifestEntry);
        assert_eq!(decode(LAST, TABLE), Ok(full()));

        for (at, last) in [(0, 44), (1, 309), (2, 11), (3, 121), (0, 0), (3, 0)] {
            let mut ids = LAST;
            ids[at] = last;
            assert_eq!(decode(ids, TABLE), invalid);
        }
        assert!(decode([u32::MAX; 4], TABLE).is_ok());

        assert!(decode(LAST, NO_TABLE).is_ok());
        assert!(decode([45, 310, 12, 0], NO_TABLE).is_ok());
        assert_eq!(
            decode(LAST, (122, 4116, NO_VERSION)),
            Err(FormatError::MissingId)
        );
        assert_eq!(decode(LAST, (0, 4116, NO_VERSION)), invalid);
        assert_eq!(decode(LAST, (0, 0, 9000)), invalid);
        assert_eq!(decode(LAST, (0, 0, 0)), invalid);
        assert_eq!(decode(LAST, (0, 4116, 9000)), invalid);
        assert_eq!(decode(LAST, (122, 19, 9000)), invalid);
        assert_eq!(decode(LAST, (122, 0, 9000)), invalid);
        assert!(decode(LAST, (122, 20, 0)).is_ok());
        assert!(decode(LAST, (1, u64::MAX, u64::MAX - 1)).is_ok());
        assert_eq!(decode(LAST, (123, 4116, 9000)), invalid);
    }

    #[test]
    fn manifest_rejects_every_single_bit_flip() {
        for golden in [GOLDEN_EMPTY, GOLDEN_FULL] {
            let bytes = unhex(golden);
            for bit in 0..bytes.len() * 8 {
                let mut damaged = bytes.clone();
                damaged[bit / 8] ^= 1 << (bit % 8);
                let error = Manifest::decode(&damaged).expect_err("damaged manifest");
                let payload_len = bytes.len() - 28;
                let damaged_len = if (160..192).contains(&bit) {
                    payload_len ^ (1 << (bit - 160))
                } else {
                    payload_len
                };
                if bit < 64 {
                    assert_eq!(error, FormatError::BadMagic);
                } else if damaged_len > MAX_BODY_LEN {
                    assert_eq!(
                        error,
                        FormatError::RecordTooLong {
                            len: damaged_len as u64,
                            max: MAX_BODY_LEN as u64,
                        }
                    );
                } else if damaged_len > payload_len {
                    assert_eq!(
                        error,
                        FormatError::Truncated {
                            needed: 28 + damaged_len,
                            available: bytes.len(),
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
        assert_eq!(Manifest::decode(&[0; 144]), Err(FormatError::BadMagic));
    }

    #[test]
    fn a_manifest_names_at_most_the_stated_number_of_files() {
        let last = last_ids([0, u32::MAX, 0, 0]);
        let mut files: Vec<LiveFile> = (1..=MAX_LIVE_FILES as u32 + 1)
            .map(|raw_id| file(FileKind::Segment, raw_id, FileState::Closed, 20))
            .collect();
        assert_eq!(
            Manifest::new(last, None, files.clone()),
            Err(FormatError::RecordTooLong {
                len: MAX_BODY_LEN as u64 + 16,
                max: MAX_BODY_LEN as u64,
            })
        );
        let mut entries: Vec<Entry> = (1..=MAX_LIVE_FILES as u32 + 1)
            .map(|raw_id| (3, 2, 0, raw_id, 20))
            .collect();
        let forged = forge(&body([0, u32::MAX, 0, 0], NO_TABLE, &entries));
        assert_eq!(
            Manifest::decode(&forged),
            Err(FormatError::RecordTooLong {
                len: MAX_BODY_LEN as u64 + 16,
                max: MAX_BODY_LEN as u64,
            })
        );

        files.pop();
        entries.pop();
        let manifest = Manifest::new(last, None, files).expect("fits");
        let bytes = manifest.encode().expect("fits");
        assert_eq!(bytes.len(), 28 + MAX_BODY_LEN);
        assert!(bytes == forge(&body([0, u32::MAX, 0, 0], NO_TABLE, &entries)));
        assert!(Manifest::decode(&bytes).as_ref() == Ok(&manifest));
    }

    #[test]
    fn a_manifest_that_decodes_is_the_one_its_value_encodes_to() {
        let mut rng = SplitMix64::new(0xB8);
        let (mut accepted, mut rejected) = (0, 0);
        for _ in 0..60_000 {
            let last = [0; 4].map(|_| (rng.next_u64() % 4) as u32);
            let table = match rng.next_u64() % 4 {
                0 => NO_TABLE,
                _ => (
                    (rng.next_u64() % 4) as u32,
                    18 + rng.next_u64() % 4,
                    rng.next_u64() % 3,
                ),
            };
            let entries: Vec<Entry> = (0..rng.next_u64() % 4)
                .map(|at| {
                    (
                        (2 + at.min(rng.next_u64() % 3)) as u16,
                        1 + (rng.next_u64() % 3) as u8,
                        u8::from(rng.next_u64().is_multiple_of(20)),
                        (rng.next_u64() % 4) as u32,
                        18 + rng.next_u64() % 32,
                    )
                })
                .collect();
            let mut body = body(last, table, &entries);
            if rng.next_u64().is_multiple_of(10) {
                let cut = (rng.next_u64() as usize) % (body.len() + 1);
                body.truncate(cut);
            }
            let bytes = forge(&body);
            match Manifest::decode(&bytes) {
                Ok(manifest) => {
                    accepted += 1;
                    assert_eq!(manifest.encode().expect("fits"), bytes);
                    let again = Manifest::new(
                        manifest.last_file_ids(),
                        manifest.table_checkpoint(),
                        manifest.files().to_vec(),
                    );
                    assert_eq!(again, Ok(manifest));
                }
                Err(_) => rejected += 1,
            }
        }
        assert!(
            accepted > 1_000 && rejected > 10_000,
            "{accepted} {rejected}"
        );
    }

    #[test]
    fn decode_returns_an_error_on_arbitrary_bytes() {
        let mut rng = SplitMix64::new(0xFA23);
        let golden = unhex(GOLDEN_FULL);
        for round in 0..40_000 {
            let bytes = if round % 2 == 0 {
                let len = (rng.next_u64() % 200) as usize;
                rng.bytes(len)
            } else {
                let mut bytes = golden.clone();
                for _ in 0..=rng.next_u64() % 3 {
                    let at = (rng.next_u64() as usize) % bytes.len();
                    bytes[at] = rng.next_u64() as u8;
                }
                let keep = (rng.next_u64() as usize) % (bytes.len() + 1);
                if round % 4 == 1 {
                    bytes.truncate(keep);
                }
                bytes
            };
            if let Ok(manifest) = Manifest::decode(&bytes) {
                assert_eq!(manifest.encode().expect("fits"), bytes);
            }
        }
    }
}
