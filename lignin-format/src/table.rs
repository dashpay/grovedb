//! Page-table checkpoints, `table/<n>.pt`: the links of every page's chain,
//! as the page table held them when the checkpoint was written.
//!
//! See section 3 of `docs/lignindb/page-table.md`.
//!
//! ```text
//! file header     20 bytes: kind 5 (page table), sequence = the file id
//! links record    zero or more, each with 4096 links except the last
//! end record      the totals, and the commit version the checkpoint covers
//! ```
//!
//! Every record is in the default frame, and its payload starts with the
//! record kind:
//!
//! ```text
//! kind  record  payload after the kind byte
//!    1  links   28 bytes for each link
//!    2  end     covers (u64), newest version (u64), link count (u64)
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
//! # Example
//!
//! ```
//! use lignin_format::{
//!     table::{self, BlockPointer, Link, LinkKind},
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
//! // The bytes of the whole checkpoint file, and back.
//! let bytes = table::encode(id, version(41), links)?;
//! let checkpoint = table::decode(&bytes, id)?;
//! assert_eq!(checkpoint.links, links);
//! assert_eq!(checkpoint.end.covers, version(41));
//! assert_eq!(checkpoint.end.reaches, Some(version(41)));
//! # Ok(())
//! # }
//! ```

use std::fmt;

use crate::{
    error::FormatError,
    header::{read_record, write_record, FileHeader, FileKind, FormatVersion, RecordHeader},
    ids::{CommitVersion, FileId, PageId},
    segment::{DATA_START, FRAME_LEN, MAX_BODY_LEN},
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
    /// record after the end record, or a links record after one that was not
    /// full.
    UnexpectedRecordKind {
        /// The code found in the record.
        code: u32,
    },
    /// A length does not agree with the bytes or the fields it describes: a
    /// links record that is empty or not whole links, an end record of
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

/// The longest payload of a record: the kind byte and a full run of links.
pub const MAX_PAYLOAD_LEN: usize = KIND_LEN + LINKS_PER_RECORD * LINK_LEN;

/// The longest record of a checkpoint, its frame included.
pub const MAX_RECORD_LEN: usize = RecordHeader::ENCODED_LEN + MAX_PAYLOAD_LEN;

/// Bytes of the end record, its frame included: the last bytes of a
/// checkpoint.
pub const END_RECORD_LEN: usize = RecordHeader::ENCODED_LEN + KIND_LEN + END_BODY_LEN;

const KIND_LEN: usize = 1;
const END_BODY_LEN: usize = 24;
const LINKS_RECORD: u8 = 1;
const END_RECORD: u8 = 2;

const KIND_SHIFT: u32 = 28;
const LEN_MASK: u32 = (1 << KIND_SHIFT) - 1;
const KIND_MASK: u32 = 0b11;
const RESERVED_SHIFT: u32 = 30;
const BASE: u32 = 1;
const DELTA: u32 = 2;
const FREE: u32 = 3;

const _: () = assert!(MAX_BLOCK_LEN <= LEN_MASK);
const _: () = assert!(MAX_PAYLOAD_LEN <= RecordHeader::MAX_PAYLOAD_LEN);

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
        let file = FileId::decode(array(&bytes[8..12]));
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
}

impl End {
    fn encode(&self) -> [u8; KIND_LEN + END_BODY_LEN] {
        let mut payload = [0; KIND_LEN + END_BODY_LEN];
        payload[0] = END_RECORD;
        payload[1..9].copy_from_slice(&self.covers.encode());
        payload[9..17].copy_from_slice(&CommitVersion::encode_optional(self.reaches));
        payload[17..].copy_from_slice(&self.links.to_le_bytes());
        payload
    }

    fn decode(body: &[u8]) -> Result<Self, FormatError> {
        let Ok(body) = <&[u8; END_BODY_LEN]>::try_from(body) else {
            return Err(FormatError::Table(TableError::LengthMismatch {
                expected: END_BODY_LEN as u64,
                found: body.len() as u64,
            }));
        };
        Ok(Self {
            covers: CommitVersion::decode(array(&body[..8])).ok_or(FormatError::MissingId)?,
            reaches: CommitVersion::decode(array(&body[8..16])),
            links: u64::from_le_bytes(array(&body[16..])),
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

/// The rules on the order of links, applied to one link after another.
#[derive(Debug, Clone, Copy, Default)]
struct Order {
    last: Option<(PageId, CommitVersion, Chain)>,
    links: u64,
    reaches: Option<CommitVersion>,
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

    fn end(&self, covers: CommitVersion) -> End {
        End {
            covers,
            reaches: self.reaches,
            links: self.links,
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

/// Writes the records of a checkpoint, one link after another.
///
/// The file header is not part of it: write [`file_header`] first. Links are
/// pushed in the order the file holds them, and a record is appended to the
/// output each time [`LINKS_PER_RECORD`] links are complete, so a caller can
/// write the output to a file as it fills.
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
    /// ([`TableError::LinksOutOfOrder`], [`TableError::InvalidChain`]). The
    /// encoder is then as it was.
    pub fn push(&mut self, link: Link, out: &mut Vec<u8>) -> Result<(), FormatError> {
        self.order.accept(&link)?;
        self.payload.extend_from_slice(&link.encode());
        if self.payload.len() == MAX_PAYLOAD_LEN {
            self.flush(out)?;
        }
        Ok(())
    }

    /// Appends the last links record, if links are left over, and the end
    /// record, to `out`. `covers` is the commit version the checkpoint
    /// covers.
    pub fn finish(mut self, covers: CommitVersion, out: &mut Vec<u8>) -> Result<End, FormatError> {
        if self.payload.len() > KIND_LEN {
            self.flush(out)?;
        }
        let end = self.order.end(covers);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Any,
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
    /// its kind, its place among the records, and then every link in it. A
    /// record that fails leaves the decoder as it was.
    pub fn read<'a>(&mut self, bytes: &'a [u8]) -> Result<(Record<'a>, &'a [u8]), FormatError> {
        let (payload, rest) = read_record(bytes, MAX_PAYLOAD_LEN)?;
        Ok((self.record(payload)?, rest))
    }

    /// Reads one record from its payload, which the caller has taken out of
    /// its frame with its checksum verified ([`RecordHeader`]).
    pub fn record<'a>(&mut self, payload: &'a [u8]) -> Result<Record<'a>, FormatError> {
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
        if kind == END_RECORD {
            let end = End::decode(body)?;
            let found = self.order.end(end.covers);
            if end.links != found.links {
                return Err(FormatError::Table(TableError::LinkCountMismatch {
                    expected: found.links,
                    found: end.links,
                }));
            }
            if end.reaches != found.reaches {
                return Err(FormatError::Table(TableError::ReachMismatch));
            }
            self.expect = Expect::Nothing;
            return Ok(Record::End(end));
        }

        let (items, rest) = body.as_chunks::<LINK_LEN>();
        if items.is_empty() || !rest.is_empty() {
            return Err(FormatError::Table(TableError::LengthMismatch {
                expected: (items.len().max(1) * LINK_LEN) as u64,
                found: body.len() as u64,
            }));
        }
        let mut order = self.order;
        for item in items {
            order.accept(&Link::decode(item, self.version)?)?;
        }
        self.order = order;
        if items.len() < LINKS_PER_RECORD {
            self.expect = Expect::End;
        }
        Ok(Record::Links(Links {
            version: self.version,
            items: items.iter(),
        }))
    }
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
}

/// The bytes of the whole checkpoint file `id` that covers the commit
/// version `covers` and holds `links`, which must be in file order.
pub fn encode(
    id: FileId,
    covers: CommitVersion,
    links: impl IntoIterator<Item = Link>,
) -> Result<Vec<u8>, FormatError> {
    let mut out = file_header(id).encode().to_vec();
    let mut encoder = Encoder::new();
    for link in links {
        encoder.push(link, &mut out)?;
    }
    encoder.finish(covers, &mut out)?;
    Ok(out)
}

/// Reads a checkpoint from `bytes`, the valid bytes of the file named `id`.
///
/// Nothing may follow the end record ([`TableError::LengthMismatch`]).
pub fn decode(bytes: &[u8], id: FileId) -> Result<Checkpoint, FormatError> {
    let version = decode_file_header(bytes, id)?;
    let mut decoder = Decoder::new(version);
    let mut links = Vec::new();
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
            Record::End(end) if rest.is_empty() => {
                return Ok(Checkpoint {
                    version,
                    end,
                    links,
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
