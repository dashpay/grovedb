//! Page-table checkpoints, `table/<n>.pt`: the links of every page's chain,
//! as the page table held them when the checkpoint was written, and a sample
//! of the count of live values of every value-log file.
//!
//! See section 3 of `docs/lignindb/page-table.md`.
//!
//! ```text
//! file header     20 bytes: kind 5 (page table), sequence = the file id
//! links record    zero or more, each with 4096 links except the last
//! counts record   zero or more, each with 4096 counts except the last
//! end record      the totals, the commit version the checkpoint covers, and
//!                 the place in the log of the sample of the counts
//! ```
//!
//! Every record is in the default frame, and its payload starts with the
//! record kind:
//!
//! ```text
//! kind  record  payload after the kind byte
//!    1  links   28 bytes for each link
//!    2  end     covers (u64), newest version (u64), link count (u64),
//!               log file id (u32) and offset (u64) of the sample's place,
//!               count of counts (u64)
//!    3  counts  24 bytes for each value-log file
//! ```
//!
//! ```text
//! offset  size  field of a link
//!      0     4  page id
//!      4     4  bits 0..28 block length, bits 28..30 link kind, the rest 0
//!      8     4  segment file id, or 0 in a free link
//!     12     8  block offset, or 0 in a free link
//!     20     8  commit version
//! ```
//!
//! Links are in ascending order of page id, and the links of one page are in
//! the order of its chain, oldest first.
//!
//! ```text
//! offset  size  field of a count
//!      0     4  value-log file id
//!      4     4  bit 0: the count is known to be wrong, the rest 0
//!      8     8  live values
//!     16     8  bytes the live values take
//! ```
//!
//! Counts are in ascending order of file id, one for a file.
//!
//! # Example
//!
//! ```
//! use lignin_format::{
//!     table::{self, BlockPointer, Link, LinkKind, LogPlace, ValueCount},
//!     CommitVersion, FileId, PageId,
//! };
//!
//! # fn main() -> Result<(), lignin_format::FormatError> {
//! let id = FileId::new(122).expect("not zero");
//! let segment = FileId::new(310).expect("not zero");
//! let version = |raw| CommitVersion::new(raw).expect("a version");
//! let link = |page, raw, kind| Link {
//!     page: PageId::new(page).expect("not zero"),
//!     version: version(raw),
//!     kind,
//! };
//! let links = [
//!     link(7, 40, LinkKind::Base(BlockPointer::new(segment, 20, 4104)?)),
//!     link(7, 41, LinkKind::Delta(BlockPointer::new(segment, 4124, 56)?)),
//!     link(9, 38, LinkKind::Free),
//! ];
//!
//! // The sample of the counts: one value-log file with three live values,
//! // taken when the log ended at offset 81940 of `wal/00000017.log`.
//! let place = LogPlace::new(FileId::new(17).expect("not zero"), 81_940)?;
//! let counts = [ValueCount {
//!     file: FileId::new(5).expect("not zero"),
//!     values: 3,
//!     bytes: 4096,
//!     miscounted: false,
//! }];
//!
//! // The bytes of the whole checkpoint file, and back.
//! let bytes = table::encode(id, version(41), links, Some(place), counts)?;
//! let checkpoint = table::decode(&bytes, id)?;
//! assert_eq!(checkpoint.links, links);
//! assert_eq!(checkpoint.value_counts, counts);
//! assert_eq!(checkpoint.end.covers, version(41));
//! assert_eq!(checkpoint.end.reaches, Some(version(41)));
//! assert_eq!(checkpoint.end.place, Some(place));
//! # Ok(())
//! # }
//! ```

use std::fmt;

pub use crate::wal::LogPlace;
use crate::{
    error::FormatError,
    header::{read_record, write_record, FileHeader, FileKind, FormatVersion, RecordHeader},
    ids::{CommitVersion, FileId, PageId},
    segment::{DATA_START, FRAME_LEN, MAX_BODY_LEN},
    wal::{CHECKPOINT_MARK_LEN, FIRST_ITEM_OFFSET},
};

/// Why a byte string is not a page-table checkpoint, or why one cannot be
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    /// The file header is of another kind than page table.
    NotAPageTable {
        /// The kind in the file header.
        found: FileKind,
    },
    /// The file header carries another id than the file's name.
    WrongFileId {
        /// The id in the file's name.
        expected: u32,
        /// The id in the file header.
        found: u32,
    },
    /// The record kind is defined, but not allowed where it was found: a
    /// record after the end record, a links or counts record after one of
    /// its kind that was not full, or a links record after a counts record.
    UnexpectedRecordKind {
        /// The code found in the record.
        code: u32,
    },
    /// A length does not agree with the bytes or the fields it describes: a
    /// links or counts record that is empty or not whole items, a counts
    /// record with more counts than a record holds, an end record of
    /// another size, or bytes after the end record.
    LengthMismatch {
        /// The length the other bytes and fields require.
        expected: u64,
        /// The length found.
        found: u64,
    },
    /// The link kind code is zero or not defined in the file's format
    /// version.
    UnknownLinkKind {
        /// The code found in the link.
        code: u8,
    },
    /// A link breaks a rule for its kind: a block length no block has, a
    /// block that starts before the first block of a segment or ends past
    /// the largest offset, or a free link that names a block.
    InvalidLink,
    /// A link's page id is lower than the one before it, or its version is
    /// lower than the version of the link before it in the same chain.
    LinksOutOfOrder,
    /// The kinds of a page's links are in an order no chain has: a chain
    /// that starts with a delta, a delta or a free link after a free link.
    InvalidChain,
    /// The end record's link count is not the number of links before it.
    LinkCountMismatch {
        /// The number of links in the records.
        expected: u64,
        /// The count in the end record.
        found: u64,
    },
    /// The end record's newest version is not the newest version of the
    /// links before it.
    ReachMismatch,
    /// A count's file id is not above the file id of the count before it.
    CountsOutOfOrder,
    /// The end record's count of counts is not the number of counts before
    /// it.
    ValueCountMismatch {
        /// The number of counts in the records.
        expected: u64,
        /// The count in the end record.
        found: u64,
    },
    /// The place in the log is not the end of a checkpoint mark: an offset
    /// before the end of the first record a log file can hold behind its
    /// start, past the largest offset, or with no log file.
    InvalidLogPlace,
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAPageTable { found } => {
                write!(f, "a {found:?} file, not a page-table checkpoint")
            }
            Self::WrongFileId { expected, found } => {
                write!(f, "file header carries id {found}, expected {expected}")
            }
            Self::UnexpectedRecordKind { code } => {
                write!(f, "record kind code {code} is not allowed here")
            }
            Self::LengthMismatch { expected, found } => {
                write!(f, "length is {found}, expected {expected}")
            }
            Self::UnknownLinkKind { code } => write!(f, "unknown link kind code {code}"),
            Self::InvalidLink => write!(f, "link breaks a rule for its kind"),
            Self::LinksOutOfOrder => {
                write!(f, "links are not in ascending order of page id and version")
            }
            Self::InvalidChain => write!(f, "the kinds of a page's links do not form a chain"),
            Self::LinkCountMismatch { expected, found } => {
                write!(
                    f,
                    "end record counts {found} links, the records hold {expected}"
                )
            }
            Self::ReachMismatch => write!(
                f,
                "end record's newest version is not the newest version of the links"
            ),
            Self::CountsOutOfOrder => {
                write!(f, "counts are not in ascending order of file id")
            }
            Self::ValueCountMismatch { expected, found } => {
                write!(
                    f,
                    "end record counts {found} counts, the records hold {expected}"
                )
            }
            Self::InvalidLogPlace => {
                write!(
                    f,
                    "the place in the log is not one a checkpoint mark ends at"
                )
            }
        }
    }
}

impl From<TableError> for FormatError {
    fn from(error: TableError) -> Self {
        Self::Table(error)
    }
}

/// Bytes one link takes in a links record.
pub const LINK_LEN: usize = 28;

/// The number of links in every links record except the last.
pub const LINKS_PER_RECORD: usize = 4096;

/// The shortest block a link may point to: a frame with an empty body.
pub const MIN_BLOCK_LEN: u32 = FRAME_LEN as u32;

/// The longest block a link may point to.
pub const MAX_BLOCK_LEN: u32 = (FRAME_LEN + MAX_BODY_LEN) as u32;

/// Bytes one count takes in a counts record.
pub const VALUE_COUNT_LEN: usize = 24;

/// The number of counts in every counts record except the last.
pub const VALUE_COUNTS_PER_RECORD: usize = 4096;

/// The longest payload of a record: the kind byte and a full run of links.
pub const MAX_PAYLOAD_LEN: usize = KIND_LEN + LINKS_PER_RECORD * LINK_LEN;

/// The longest record of a checkpoint, its frame included.
pub const MAX_RECORD_LEN: usize = RecordHeader::ENCODED_LEN + MAX_PAYLOAD_LEN;

/// Bytes of the end record, its frame included: the last bytes of a
/// checkpoint.
pub const END_RECORD_LEN: usize = RecordHeader::ENCODED_LEN + KIND_LEN + END_BODY_LEN;

/// The smallest offset the place of a sample has: the end of a checkpoint
/// mark that is the first item of its log file.
pub const FIRST_SAMPLE_OFFSET: u64 = FIRST_ITEM_OFFSET + CHECKPOINT_MARK_LEN as u64;

const KIND_LEN: usize = 1;
const END_BODY_LEN: usize = 44;
const LINKS_RECORD: u8 = 1;
const END_RECORD: u8 = 2;
const VALUE_COUNTS_RECORD: u8 = 3;
const LINKS_PAYLOAD_LEN: usize = KIND_LEN + LINKS_PER_RECORD * LINK_LEN;
const VALUE_COUNTS_PAYLOAD_LEN: usize = KIND_LEN + VALUE_COUNTS_PER_RECORD * VALUE_COUNT_LEN;
const MISCOUNTED: u32 = 1;

const KIND_SHIFT: u32 = 28;
const LEN_MASK: u32 = (1 << KIND_SHIFT) - 1;
const KIND_MASK: u32 = 0b11;
const RESERVED_SHIFT: u32 = 30;
const BASE: u32 = 1;
const DELTA: u32 = 2;
const FREE: u32 = 3;

const _: () = assert!(MAX_BLOCK_LEN <= LEN_MASK);
const _: () = assert!(MAX_PAYLOAD_LEN <= RecordHeader::MAX_PAYLOAD_LEN);
const _: () = assert!(VALUE_COUNTS_PAYLOAD_LEN <= MAX_PAYLOAD_LEN);

/// Where a block is: its segment, the file offset it starts at and its
/// length in the file, frame included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockPointer {
    file: FileId,
    offset: u64,
    len: u32,
}

impl BlockPointer {
    /// The block of `len` bytes at `offset` of the segment `file`.
    ///
    /// The length is from [`MIN_BLOCK_LEN`] to [`MAX_BLOCK_LEN`], the offset
    /// is at or after the first block of a segment, and the block ends at or
    /// before the largest offset. Anything else is
    /// [`TableError::InvalidLink`].
    pub const fn new(file: FileId, offset: u64, len: u32) -> Result<Self, FormatError> {
        if len < MIN_BLOCK_LEN
            || len > MAX_BLOCK_LEN
            || offset < DATA_START
            || offset.checked_add(len as u64).is_none()
        {
            return Err(FormatError::Table(TableError::InvalidLink));
        }
        Ok(Self { file, offset, len })
    }

    /// The segment that holds the block.
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The file offset of the block's first byte.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// The block's length in the file, its frame included.
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> u32 {
        self.len
    }

    /// The file offset just past the block's last byte.
    pub const fn end(&self) -> u64 {
        self.offset + self.len as u64
    }
}

/// What one link of a chain is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// The base image of the page as its version left it.
    Base(BlockPointer),
    /// A run of deltas that brings the page from the link before it to its
    /// version.
    Delta(BlockPointer),
    /// The page was freed at its version.
    Free,
}

impl LinkKind {
    /// The block the link points to, if it has one.
    pub const fn block(&self) -> Option<BlockPointer> {
        match self {
            Self::Base(block) | Self::Delta(block) => Some(*block),
            Self::Free => None,
        }
    }
}

/// One link of one page's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Link {
    /// The page.
    pub page: PageId,
    /// The commit version the link brings the page to.
    pub version: CommitVersion,
    /// What the link is.
    pub kind: LinkKind,
}

impl Link {
    fn encode(&self) -> [u8; LINK_LEN] {
        let (code, block) = match self.kind {
            LinkKind::Base(block) => (BASE, Some(block)),
            LinkKind::Delta(block) => (DELTA, Some(block)),
            LinkKind::Free => (FREE, None),
        };
        let word = code << KIND_SHIFT | block.map_or(0, |block| block.len);
        let mut bytes = [0; LINK_LEN];
        bytes[..4].copy_from_slice(&self.page.encode());
        bytes[4..8].copy_from_slice(&word.to_le_bytes());
        bytes[8..12].copy_from_slice(&FileId::encode_optional(block.map(|block| block.file)));
        bytes[12..20].copy_from_slice(&block.map_or(0, |block| block.offset).to_le_bytes());
        bytes[20..].copy_from_slice(&self.version.encode());
        bytes
    }

    fn decode(bytes: &[u8; LINK_LEN], version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => Self::decode_v1(bytes),
        }
    }

    fn decode_v1(bytes: &[u8; LINK_LEN]) -> Result<Self, FormatError> {
        let page = PageId::decode(array(&bytes[..4])).ok_or(FormatError::MissingId)?;
        let word = u32::from_le_bytes(array(&bytes[4..8]));
        if word >> RESERVED_SHIFT != 0 {
            return Err(FormatError::NonZeroReserved);
        }
        let len = word & LEN_MASK;
        let file = FileId::decode(array(&bytes[8..12]))?;
        let offset = u64::from_le_bytes(array(&bytes[12..20]));
        let version = CommitVersion::decode(array(&bytes[20..])).ok_or(FormatError::MissingId)?;
        let kind = match (word >> KIND_SHIFT) & KIND_MASK {
            FREE if len == 0 && file.is_none() && offset == 0 => LinkKind::Free,
            FREE => return Err(FormatError::Table(TableError::InvalidLink)),
            code @ (BASE | DELTA) => {
                let file = file.ok_or(FormatError::MissingId)?;
                let block = BlockPointer::new(file, offset, len)?;
                if code == BASE {
                    LinkKind::Base(block)
                } else {
                    LinkKind::Delta(block)
                }
            }
            code => {
                return Err(FormatError::Table(TableError::UnknownLinkKind {
                    code: code as u8,
                }));
            }
        };
        Ok(Self {
            page,
            version,
            kind,
        })
    }
}

/// The place in the log of a sample of the counts: the end of the
/// checkpoint mark that was written when the sample was taken.
///
/// A mark is a record of its log file, behind the file's first item at
/// the earliest. An earlier offset is [`TableError::InvalidLogPlace`].
fn sample_place(place: LogPlace) -> Result<LogPlace, FormatError> {
    if place.offset() < FIRST_SAMPLE_OFFSET {
        return Err(FormatError::Table(TableError::InvalidLogPlace));
    }
    Ok(place)
}

/// The live values of one value-log file, in a sample of the counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueCount {
    /// The value-log file.
    pub file: FileId,
    /// How many live values the file holds.
    pub values: u64,
    /// The bytes those values take in the file.
    pub bytes: u64,
    /// The count is known to be wrong.
    pub miscounted: bool,
}

impl ValueCount {
    fn encode(&self) -> [u8; VALUE_COUNT_LEN] {
        let flags = if self.miscounted { MISCOUNTED } else { 0 };
        let mut bytes = [0; VALUE_COUNT_LEN];
        bytes[..4].copy_from_slice(&self.file.encode());
        bytes[4..8].copy_from_slice(&flags.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.values.to_le_bytes());
        bytes[16..].copy_from_slice(&self.bytes.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8; VALUE_COUNT_LEN], version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => Self::decode_v1(bytes),
        }
    }

    fn decode_v1(bytes: &[u8; VALUE_COUNT_LEN]) -> Result<Self, FormatError> {
        let file = FileId::decode(array(&bytes[..4]))?.ok_or(FormatError::MissingId)?;
        let flags = u32::from_le_bytes(array(&bytes[4..8]));
        if flags & !MISCOUNTED != 0 {
            return Err(FormatError::NonZeroReserved);
        }
        Ok(Self {
            file,
            values: u64::from_le_bytes(array(&bytes[8..16])),
            bytes: u64::from_le_bytes(array(&bytes[16..])),
            miscounted: flags & MISCOUNTED != 0,
        })
    }
}

/// The end record of a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct End {
    /// The commit version the checkpoint covers: replay starts with the
    /// commit after it.
    pub covers: CommitVersion,
    /// The newest version of any link in the checkpoint, or `None` if it has
    /// no links.
    pub reaches: Option<CommitVersion>,
    /// The number of links in the checkpoint.
    pub links: u64,
    /// The place in the log of the sample of the counts: the end of the
    /// checkpoint mark that was written when the sample was taken. The
    /// counts include every change whose record is before it and none whose
    /// record is after it. `None` is the place before every record of the
    /// log.
    pub place: Option<LogPlace>,
    /// The number of counts in the checkpoint.
    pub value_counts: u64,
}

impl End {
    fn encode(&self) -> [u8; KIND_LEN + END_BODY_LEN] {
        let mut payload = [0; KIND_LEN + END_BODY_LEN];
        payload[0] = END_RECORD;
        payload[1..9].copy_from_slice(&self.covers.encode());
        payload[9..17].copy_from_slice(&CommitVersion::encode_optional(self.reaches));
        payload[17..25].copy_from_slice(&self.links.to_le_bytes());
        let file = self.place.map(|place| place.file());
        payload[25..29].copy_from_slice(&FileId::encode_optional(file));
        let offset = self.place.map_or(0, |place| place.offset());
        payload[29..37].copy_from_slice(&offset.to_le_bytes());
        payload[37..].copy_from_slice(&self.value_counts.to_le_bytes());
        payload
    }

    fn decode(body: &[u8]) -> Result<Self, FormatError> {
        let Ok(body) = <&[u8; END_BODY_LEN]>::try_from(body) else {
            return Err(FormatError::Table(TableError::LengthMismatch {
                expected: END_BODY_LEN as u64,
                found: body.len() as u64,
            }));
        };
        let offset = u64::from_le_bytes(array(&body[28..36]));
        let place = match FileId::decode(array(&body[24..28]))? {
            Some(file) => Some(sample_place(
                LogPlace::new(file, offset).map_err(|_| TableError::InvalidLogPlace)?,
            )?),
            None if offset == 0 => None,
            None => return Err(FormatError::Table(TableError::InvalidLogPlace)),
        };
        Ok(Self {
            covers: CommitVersion::decode(array(&body[..8])).ok_or(FormatError::MissingId)?,
            reaches: CommitVersion::decode(array(&body[8..16])),
            links: u64::from_le_bytes(array(&body[16..24])),
            place,
            value_counts: u64::from_le_bytes(array(&body[36..])),
        })
    }
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut array = [0; N];
    array.copy_from_slice(bytes);
    array
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chain {
    Open,
    Freed,
}

/// The rules on the order of links and of counts, applied to one after
/// another.
#[derive(Debug, Clone, Copy, Default)]
struct Order {
    last: Option<(PageId, CommitVersion, Chain)>,
    links: u64,
    reaches: Option<CommitVersion>,
    last_counted: Option<FileId>,
    value_counts: u64,
}

impl Order {
    fn accept(&mut self, link: &Link) -> Result<(), FormatError> {
        let chain = match self.last {
            Some((page, _, _)) if link.page < page => {
                return Err(FormatError::Table(TableError::LinksOutOfOrder));
            }
            Some((page, version, chain)) if link.page == page => {
                if link.version < version {
                    return Err(FormatError::Table(TableError::LinksOutOfOrder));
                }
                Some(chain)
            }
            _ => None,
        };
        let next = match (chain, link.kind) {
            (_, LinkKind::Base(_)) | (Some(Chain::Open), LinkKind::Delta(_)) => Chain::Open,
            (None | Some(Chain::Open), LinkKind::Free) => Chain::Freed,
            (None, LinkKind::Delta(_)) | (Some(Chain::Freed), _) => {
                return Err(FormatError::Table(TableError::InvalidChain));
            }
        };
        self.last = Some((link.page, link.version, next));
        self.links += 1;
        self.reaches = self.reaches.max(Some(link.version));
        Ok(())
    }

    fn accept_count(&mut self, count: &ValueCount) -> Result<(), FormatError> {
        if self.last_counted >= Some(count.file) {
            return Err(FormatError::Table(TableError::CountsOutOfOrder));
        }
        self.last_counted = Some(count.file);
        self.value_counts += 1;
        Ok(())
    }

    fn end(&self, covers: CommitVersion, place: Option<LogPlace>) -> End {
        End {
            covers,
            reaches: self.reaches,
            links: self.links,
            place,
            value_counts: self.value_counts,
        }
    }
}

/// The header of the checkpoint file `id`, at the format version this build
/// writes.
pub const fn file_header(id: FileId) -> FileHeader {
    match FileHeader::new(FileKind::PageTable, FormatVersion::CURRENT, id.get()) {
        Ok(header) => header,
        Err(_) => panic!("a file id is not zero"),
    }
}

/// Reads the file header at the start of the checkpoint file named `id`, and
/// returns the format version the rest of the file is in.
///
/// Rejects a file of another kind, and a checkpoint whose header carries
/// another id than its name (H3).
pub fn decode_file_header(bytes: &[u8], id: FileId) -> Result<FormatVersion, FormatError> {
    let header = FileHeader::decode(bytes)?;
    if header.kind() != FileKind::PageTable {
        return Err(FormatError::Table(TableError::NotAPageTable {
            found: header.kind(),
        }));
    }
    if header.sequence() != id.get() {
        return Err(FormatError::Table(TableError::WrongFileId {
            expected: id.get(),
            found: header.sequence(),
        }));
    }
    Ok(header.version())
}

/// Writes the records of a checkpoint, one link after another and then one
/// count after another.
///
/// The file header is not part of it: write [`file_header`] first. Links are
/// pushed in the order the file holds them, then counts, and a record is
/// appended to the output each time [`LINKS_PER_RECORD`] links or
/// [`VALUE_COUNTS_PER_RECORD`] counts are complete, so a caller can write
/// the output to a file as it fills.
#[derive(Debug, Clone)]
pub struct Encoder {
    order: Order,
    payload: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// An encoder for the records of one checkpoint.
    pub fn new() -> Self {
        Self {
            order: Order::default(),
            payload: vec![LINKS_RECORD],
        }
    }

    /// Adds `link` after the links pushed so far. If that completes a
    /// record, the record is appended to `out`.
    ///
    /// Rejects a link that may not follow the one before it
    /// ([`TableError::LinksOutOfOrder`], [`TableError::InvalidChain`]), and
    /// any link once a count was pushed
    /// ([`TableError::UnexpectedRecordKind`]). The encoder is then as it
    /// was.
    pub fn push(&mut self, link: Link, out: &mut Vec<u8>) -> Result<(), FormatError> {
        if self.payload[0] != LINKS_RECORD {
            return Err(FormatError::Table(TableError::UnexpectedRecordKind {
                code: u32::from(LINKS_RECORD),
            }));
        }
        self.order.accept(&link)?;
        self.payload.extend_from_slice(&link.encode());
        if self.payload.len() == LINKS_PAYLOAD_LEN {
            self.flush(out)?;
        }
        Ok(())
    }

    /// Adds `count` after the counts pushed so far. The first count ends
    /// the links: the last links record, if links are left over, is
    /// appended to `out`. If the count completes a record, that record is
    /// appended as well.
    ///
    /// Rejects a count whose file is not above the file of the count before
    /// it ([`TableError::CountsOutOfOrder`]). The encoder is then as it was.
    pub fn push_value_count(
        &mut self,
        count: ValueCount,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        self.order.accept_count(&count)?;
        if self.payload[0] == LINKS_RECORD {
            if self.payload.len() > KIND_LEN {
                self.flush(out)?;
            }
            self.payload[0] = VALUE_COUNTS_RECORD;
        }
        self.payload.extend_from_slice(&count.encode());
        if self.payload.len() == VALUE_COUNTS_PAYLOAD_LEN {
            self.flush(out)?;
        }
        Ok(())
    }

    /// Appends the last links or counts record, if items are left over, and
    /// the end record, to `out`. `covers` is the commit version the
    /// checkpoint covers, and `place` the place in the log of the sample
    /// the counts are.
    pub fn finish(
        mut self,
        covers: CommitVersion,
        place: Option<LogPlace>,
        out: &mut Vec<u8>,
    ) -> Result<End, FormatError> {
        let place = place.map(sample_place).transpose()?;
        if self.payload.len() > KIND_LEN {
            self.flush(out)?;
        }
        let end = self.order.end(covers, place);
        write_record(&end.encode(), out)?;
        Ok(end)
    }

    fn flush(&mut self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        write_record(&self.payload, out)?;
        self.payload.truncate(KIND_LEN);
        Ok(())
    }
}

/// One record of a checkpoint.
#[derive(Debug, Clone)]
pub enum Record<'a> {
    /// A run of links.
    Links(Links<'a>),
    /// A run of counts.
    ValueCounts(ValueCounts<'a>),
    /// The end record. Nothing follows it.
    End(End),
}

/// The links of one links record, in file order.
#[derive(Debug, Clone)]
pub struct Links<'a> {
    version: FormatVersion,
    items: std::slice::Iter<'a, [u8; LINK_LEN]>,
}

impl Iterator for Links<'_> {
    type Item = Link;

    fn next(&mut self) -> Option<Link> {
        let version = self.version;
        self.items
            .by_ref()
            .find_map(|item| Link::decode(item, version).ok())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}

impl ExactSizeIterator for Links<'_> {}

/// The counts of one counts record, in file order.
#[derive(Debug, Clone)]
pub struct ValueCounts<'a> {
    version: FormatVersion,
    items: std::slice::Iter<'a, [u8; VALUE_COUNT_LEN]>,
}

impl Iterator for ValueCounts<'_> {
    type Item = ValueCount;

    fn next(&mut self) -> Option<ValueCount> {
        let version = self.version;
        self.items
            .by_ref()
            .find_map(|item| ValueCount::decode(item, version).ok())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}

impl ExactSizeIterator for ValueCounts<'_> {}

/// Which records may come next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A links record, a counts record or the end record.
    Any,
    /// A counts record or the end record: the links have ended.
    CountsOrEnd,
    /// The end record: the counts have ended.
    End,
    Nothing,
}

/// Reads the records of a checkpoint, one after another, and checks every
/// rule on them and on the order of their links.
#[derive(Debug, Clone)]
pub struct Decoder {
    version: FormatVersion,
    order: Order,
    expect: Expect,
}

impl Decoder {
    /// A decoder for the records of a checkpoint whose file header carries
    /// the format `version`.
    pub fn new(version: FormatVersion) -> Self {
        Self {
            version,
            order: Order::default(),
            expect: Expect::Any,
        }
    }

    /// Whether the end record has been read.
    pub fn is_finished(&self) -> bool {
        self.expect == Expect::Nothing
    }

    /// Reads the record at the start of `bytes`. Returns it and the bytes
    /// after it.
    ///
    /// The record is checked in this order: its length against
    /// [`MAX_PAYLOAD_LEN`] and against the bytes there are, its checksum,
    /// its kind, its place among the records, and then every link or count
    /// in it. A record that fails leaves the decoder as it was.
    pub fn read<'a>(&mut self, bytes: &'a [u8]) -> Result<(Record<'a>, &'a [u8]), FormatError> {
        let (payload, rest) = read_record(bytes, MAX_PAYLOAD_LEN)?;
        Ok((self.record(payload)?, rest))
    }

    /// Reads one record from its payload, which the caller has taken out of
    /// its frame with its checksum verified ([`RecordHeader`]).
    ///
    /// A payload longer than [`MAX_PAYLOAD_LEN`] is refused as
    /// [`Self::read`] refuses it, so both give the same answer for the same
    /// record.
    pub fn record<'a>(&mut self, payload: &'a [u8]) -> Result<Record<'a>, FormatError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(FormatError::RecordTooLong {
                len: payload.len() as u64,
                max: MAX_PAYLOAD_LEN as u64,
            });
        }
        match self.version {
            FormatVersion::V1 => self.record_v1(payload),
        }
    }

    fn record_v1<'a>(&mut self, payload: &'a [u8]) -> Result<Record<'a>, FormatError> {
        let Some((&kind, body)) = payload.split_first() else {
            return Err(FormatError::Table(TableError::LengthMismatch {
                expected: KIND_LEN as u64,
                found: 0,
            }));
        };
        let allowed = match kind {
            LINKS_RECORD => self.expect == Expect::Any,
            VALUE_COUNTS_RECORD => matches!(self.expect, Expect::Any | Expect::CountsOrEnd),
            END_RECORD => self.expect != Expect::Nothing,
            code => {
                return Err(FormatError::UnknownRecordKind {
                    code: u32::from(code),
                });
            }
        };
        if !allowed {
            return Err(FormatError::Table(TableError::UnexpectedRecordKind {
                code: u32::from(kind),
            }));
        }
        match kind {
            END_RECORD => self.end_v1(body).map(Record::End),
            VALUE_COUNTS_RECORD => self.value_counts_v1(body).map(Record::ValueCounts),
            _ => self.links_v1(body).map(Record::Links),
        }
    }

    fn end_v1(&mut self, body: &[u8]) -> Result<End, FormatError> {
        let end = End::decode(body)?;
        let found = self.order.end(end.covers, end.place);
        if end.links != found.links {
            return Err(FormatError::Table(TableError::LinkCountMismatch {
                expected: found.links,
                found: end.links,
            }));
        }
        if end.reaches != found.reaches {
            return Err(FormatError::Table(TableError::ReachMismatch));
        }
        if end.value_counts != found.value_counts {
            return Err(FormatError::Table(TableError::ValueCountMismatch {
                expected: found.value_counts,
                found: end.value_counts,
            }));
        }
        self.expect = Expect::Nothing;
        Ok(end)
    }

    fn value_counts_v1<'a>(&mut self, body: &'a [u8]) -> Result<ValueCounts<'a>, FormatError> {
        let items = whole_items::<VALUE_COUNT_LEN>(body)?;
        if items.len() > VALUE_COUNTS_PER_RECORD {
            return Err(FormatError::Table(TableError::LengthMismatch {
                expected: (VALUE_COUNTS_PER_RECORD * VALUE_COUNT_LEN) as u64,
                found: body.len() as u64,
            }));
        }
        let mut order = self.order;
        for item in items {
            order.accept_count(&ValueCount::decode(item, self.version)?)?;
        }
        self.order = order;
        self.expect = if items.len() < VALUE_COUNTS_PER_RECORD {
            Expect::End
        } else {
            Expect::CountsOrEnd
        };
        Ok(ValueCounts {
            version: self.version,
            items: items.iter(),
        })
    }

    fn links_v1<'a>(&mut self, body: &'a [u8]) -> Result<Links<'a>, FormatError> {
        let items = whole_items::<LINK_LEN>(body)?;
        let mut order = self.order;
        for item in items {
            order.accept(&Link::decode(item, self.version)?)?;
        }
        self.order = order;
        if items.len() < LINKS_PER_RECORD {
            self.expect = Expect::CountsOrEnd;
        }
        Ok(Links {
            version: self.version,
            items: items.iter(),
        })
    }
}

/// The items of `N` bytes that the body of a links or counts record is made
/// of: at least one, and nothing left over.
fn whole_items<const N: usize>(body: &[u8]) -> Result<&[[u8; N]], FormatError> {
    let (items, rest) = body.as_chunks::<N>();
    if items.is_empty() || !rest.is_empty() {
        return Err(FormatError::Table(TableError::LengthMismatch {
            expected: (items.len().max(1) * N) as u64,
            found: body.len() as u64,
        }));
    }
    Ok(items)
}

/// A whole checkpoint, read into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// The format version of the file.
    pub version: FormatVersion,
    /// The end record.
    pub end: End,
    /// Every link, in file order.
    pub links: Vec<Link>,
    /// Every count, in file order.
    pub value_counts: Vec<ValueCount>,
}

/// The bytes of the whole checkpoint file `id` that covers the commit
/// version `covers` and holds `links` and then `value_counts`, each of which
/// must be in file order. `place` is the place in the log of the sample the
/// counts are.
pub fn encode(
    id: FileId,
    covers: CommitVersion,
    links: impl IntoIterator<Item = Link>,
    place: Option<LogPlace>,
    value_counts: impl IntoIterator<Item = ValueCount>,
) -> Result<Vec<u8>, FormatError> {
    let mut out = file_header(id).encode().to_vec();
    let mut encoder = Encoder::new();
    for link in links {
        encoder.push(link, &mut out)?;
    }
    for count in value_counts {
        encoder.push_value_count(count, &mut out)?;
    }
    encoder.finish(covers, place, &mut out)?;
    Ok(out)
}

/// Reads a checkpoint from `bytes`, the valid bytes of the file named `id`.
///
/// Nothing may follow the end record ([`TableError::LengthMismatch`]).
pub fn decode(bytes: &[u8], id: FileId) -> Result<Checkpoint, FormatError> {
    let version = decode_file_header(bytes, id)?;
    let mut decoder = Decoder::new(version);
    let mut links = Vec::new();
    let mut value_counts = Vec::new();
    let mut rest = bytes.get(FileHeader::ENCODED_LEN..).unwrap_or_default();
    loop {
        let at = bytes.len() - rest.len();
        let (record, after) = decoder.read(rest).map_err(|error| match error {
            FormatError::Truncated { needed, available } => FormatError::Truncated {
                needed: needed + at,
                available: available + at,
            },
            error => error,
        })?;
        rest = after;
        match record {
            Record::Links(record) => links.extend(record),
            Record::ValueCounts(record) => value_counts.extend(record),
            Record::End(end) if rest.is_empty() => {
                return Ok(Checkpoint {
                    version,
                    end,
                    links,
                    value_counts,
                });
            }
            Record::End(_) => {
                return Err(FormatError::Table(TableError::LengthMismatch {
                    expected: (bytes.len() - rest.len()) as u64,
                    found: bytes.len() as u64,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests;
