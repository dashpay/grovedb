//! Segment files, `seg/<n>.seg`: blocks of page images and page deltas, and
//! the footer index of a sealed segment.
//!
//! This module is the specification of the segment format in format version
//! 1, on top of the shared rules in `docs/lignindb/format.md` (cited below as
//! B1 to B11, C1 to C7, R1 to R7 and H1 to H5).
//!
//! # File layout
//!
//! ```text
//! file header     20 bytes: kind 3 (segment), sequence = the file id
//! block           zero or more, one after another with no gap
//! index record    sealed segments only
//! trailer         sealed segments only: the last 20 valid bytes
//! ```
//!
//! The first block starts at offset 20 ([`DATA_START`]). A segment is written
//! by appending blocks. Sealing it appends the index record and the trailer,
//! which together are the footer, and nothing is appended after that.
//!
//! A segment does not say how long it is or whether it is sealed. The
//! manifest does ([`crate::manifest`]): it gives the segment's valid length
//! and its state. Bytes past the valid length are not part of the segment,
//! whatever they hold. That is what lets a checkpoint share a file that the
//! live database is still appending to.
//!
//! # Record frame
//!
//! Every record after the file header has the same 8-byte frame.
//!
//! ```text
//! offset  size  field
//!      0     4  tag, a u32: bits 0 to 26 are `len`, the body length in bytes;
//!               bits 27 to 31 are the record kind
//!      4     4  checksum of bytes 0 to 3 followed by the body
//!      8   len  body
//! ```
//!
//! This is the default record frame with one change. A default frame's
//! length is at most 2^27 - 4, so the top five bits of its length field are
//! always zero. Here they carry the record kind. The body is at most 2^27 - 4
//! bytes ([`MAX_BODY_LEN`]), which keeps the checksum within C5.
//!
//! | Kind | Record | Body |
//! | --- | --- | --- |
//! | 1 | base image, raw | the payload |
//! | 2 | base image, LZ4 | `raw_len` (u32), then an LZ4 block |
//! | 3 | delta run, raw | the payload |
//! | 4 | delta run, LZ4 | `raw_len` (u32), then an LZ4 block |
//! | 5 | index | 8 bytes for each block |
//! | 6 | trailer | 12 bytes |
//!
//! Kind 0 is invalid (B6), so neither zero-filled bytes nor a record in the
//! default frame is a segment record. Every other code is an error
//! ([`FormatError::UnknownRecordKind`]).
//!
//! A reader checks a record in this order: 8 bytes are there; `len` is within
//! the caller's maximum and the bytes available; the checksum; the kind; then
//! the fields of the body. The kind is checked after the checksum so that a
//! flipped bit in it is reported as damage.
//!
//! # Blocks
//!
//! A block holds one payload: the base image of one page, or one run of
//! deltas of one page. The payload is opaque here. Its layout belongs to the
//! page and delta formats, and it is stored at its actual length with no
//! padding.
//!
//! - **The block is the record.** Its checksum is the only one over the
//!   payload, and it covers the bytes as stored, so a reader verifies a block
//!   before it decompresses it (C7).
//! - **Raw.** The body is the payload. The block is the payload plus 8 bytes.
//! - **LZ4.** The body is `raw_len`, the payload's length once decompressed,
//!   followed by the payload compressed in the LZ4 block format, with no
//!   dictionary and no frame.
//! - **LZ4 only when it is smaller.** An LZ4 block is valid only if its body
//!   is shorter than `raw_len`, that is, only if the whole block is smaller
//!   than the same payload stored raw. A reader rejects any other
//!   ([`FormatError::CompressedBlockNotSmaller`]). So no block is more than 8
//!   bytes longer than its payload. A writer may store any payload raw.
//! - **What a reader accepts as LZ4.** Any stream of LZ4 sequences that uses
//!   every stored byte and decompresses to exactly `raw_len` bytes. The rules
//!   the LZ4 specification gives a compressor for the end of a block are not
//!   checked.
//! - **One exception to B8.** Different LZ4 streams can decompress to the
//!   same payload. Which one a writer produces is not part of the format, and
//!   it may change with the compressor. Everything else in a segment has one
//!   encoding.
//! - **The block does not name its page or its place** (section 5 of the
//!   shared rules: file space is never reused). The pointer that leads to a
//!   block, a file id, an offset and a length, comes from the page table. The
//!   page id of each block is in the footer index.
//!
//! A reader that follows a pointer has exactly the block's bytes and uses
//! [`Block::decode`]. A reader that walks a segment uses [`Block::read`]:
//! the frame gives each block's length, so a segment with no footer can be
//! walked from offset 20 to its valid length.
//!
//! # Footer
//!
//! The index record (kind 5) has one 8-byte entry for each block, in file
//! order:
//!
//! ```text
//! offset  size  field
//!      0     4  page id of the block's page (never 0)
//!      4     4  the block's tag, as in its frame
//! ```
//!
//! Offsets are not stored. Block 0 starts at offset 20, and each block starts
//! where the one before it ends, 8 bytes plus its `len` further on. The index
//! record starts where the last block ends.
//!
//! The trailer (kind 6) is the last 20 bytes of a sealed segment. Its body:
//!
//! ```text
//! offset  size  field
//!      0     8  index offset: the file offset of the index record, u64
//!      8     4  block count, u32
//! ```
//!
//! A reader checks all of this, so there is one footer for a given run of
//! blocks:
//!
//! - index offset + 8 + 8 x block count + 20 is the segment's valid length
//! - the index body is 8 x block count bytes
//! - every entry has a page id and the tag of a block
//! - 20 plus the lengths of all blocks is the index offset
//!
//! One index record holds at most 2^24 - 1 entries ([`MAX_BLOCKS`]), which is
//! the most blocks a segment may have. A writer seals a segment before that.
//!
//! # Example
//!
//! ```
//! use lignin_format::{
//!     FileHeader, FileId, FileKind, FormatVersion, PageId,
//!     segment::{self, Block, BlockContent, Compression, IndexEntry, SegmentIndex},
//! };
//!
//! # fn main() -> Result<(), lignin_format::FormatError> {
//! let id = FileId::new(310).expect("not zero");
//! let page = PageId::new(7).expect("not zero");
//! let header = FileHeader::new(FileKind::Segment, FormatVersion::CURRENT, id.get())?;
//! let mut file = header.encode().to_vec();
//!
//! // Append a page image. The tag goes into the footer index later.
//! let image = vec![0x5A; 4096];
//! let offset = file.len();
//! let tag = segment::write_block(BlockContent::Base, Compression::Lz4, &image, &mut file)?;
//! assert!(tag.block_len() < image.len());
//!
//! // Follow a pointer (offset and length) to the block.
//! let pointed = &file[offset..offset + tag.block_len()];
//! let block = Block::decode(FormatVersion::CURRENT, pointed, image.len())?;
//! assert_eq!(block.payload()?, image.as_slice());
//!
//! // Seal the segment, then read the index back from its last bytes.
//! let index_offset = file.len() as u64;
//! segment::write_footer(&[IndexEntry { page, tag }], index_offset, &mut file)?;
//! let version = segment::decode_file_header(&file, id)?;
//! let index = SegmentIndex::decode_tail(version, &file, file.len() as u64)?;
//! let listed: Vec<_> = index.blocks().map(|block| (block.page, block.offset)).collect();
//! assert_eq!(listed, [(page, segment::DATA_START)]);
//! # Ok(())
//! # }
//! ```

use std::borrow::Cow;

use crate::{
    checksum::Checksum,
    error::FormatError,
    header::{FileHeader, FileKind, FormatVersion, RecordHeader},
    ids::{FileId, PageId},
    wire::take,
};

/// Bytes of the frame in front of every record body.
pub const FRAME_LEN: usize = 8;

/// The longest body a record may have, and so the longest payload of a block.
pub const MAX_BODY_LEN: usize = RecordHeader::MAX_PAYLOAD_LEN;

/// The most blocks one segment may hold.
pub const MAX_BLOCKS: usize = MAX_BODY_LEN / IndexEntry::ENCODED_LEN;

/// The file offset of a segment's first block.
pub const DATA_START: u64 = FileHeader::ENCODED_LEN as u64;

const KIND_SHIFT: u32 = 27;
const LEN_MASK: u32 = (1 << KIND_SHIFT) - 1;
const RAW_LEN_FIELD: usize = 4;
const MIN_LZ4_BODY_LEN: usize = RAW_LEN_FIELD + 1;
const TRAILER_BODY_LEN: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum RecordKind {
    BaseRaw = 1,
    BaseLz4 = 2,
    DeltaRaw = 3,
    DeltaLz4 = 4,
    Index = 5,
    Trailer = 6,
}

impl RecordKind {
    const fn of_tag(tag: u32, version: FormatVersion) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => match tag >> KIND_SHIFT {
                1 => Ok(Self::BaseRaw),
                2 => Ok(Self::BaseLz4),
                3 => Ok(Self::DeltaRaw),
                4 => Ok(Self::DeltaLz4),
                5 => Ok(Self::Index),
                6 => Ok(Self::Trailer),
                code => Err(FormatError::UnknownRecordKind { code }),
            },
        }
    }

    const fn tag(self, body_len: u32) -> u32 {
        (self as u32) << KIND_SHIFT | body_len
    }
}

/// What a block's payload is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockContent {
    /// The base image of a page.
    Base,
    /// A run of deltas of one page.
    Delta,
}

/// How a block's payload is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compression {
    /// As it is.
    Raw,
    /// Compressed in the LZ4 block format.
    Lz4,
}

/// The tag of a block: what it holds, how it is stored and how long its body
/// is.
///
/// The same four bytes stand in the block's frame and in its footer index
/// entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockTag {
    content: BlockContent,
    compression: Compression,
    body_len: u32,
}

impl BlockTag {
    /// Bytes a tag takes on disk.
    pub const ENCODED_LEN: usize = 4;

    /// What the block's payload is.
    pub const fn content(self) -> BlockContent {
        self.content
    }

    /// How the block's payload is stored.
    pub const fn compression(self) -> Compression {
        self.compression
    }

    /// The length of the block's body: the bytes after its frame.
    pub const fn body_len(self) -> usize {
        self.body_len as usize
    }

    /// The length of the whole block in the file, frame included.
    pub const fn block_len(self) -> usize {
        FRAME_LEN + self.body_len()
    }

    /// The four bytes written to disk.
    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let kind = match (self.content, self.compression) {
            (BlockContent::Base, Compression::Raw) => RecordKind::BaseRaw,
            (BlockContent::Base, Compression::Lz4) => RecordKind::BaseLz4,
            (BlockContent::Delta, Compression::Raw) => RecordKind::DeltaRaw,
            (BlockContent::Delta, Compression::Lz4) => RecordKind::DeltaLz4,
        };
        kind.tag(self.body_len).to_le_bytes()
    }

    /// Reads a tag in a segment of format `version`.
    ///
    /// Rejects the kind of a record that is not a block, a body longer than
    /// [`MAX_BODY_LEN`], and an LZ4 body too short to hold its length field
    /// and one byte of compressed data.
    pub const fn decode(
        bytes: [u8; Self::ENCODED_LEN],
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        let tag = u32::from_le_bytes(bytes);
        let (content, compression) = match RecordKind::of_tag(tag, version) {
            Ok(RecordKind::BaseRaw) => (BlockContent::Base, Compression::Raw),
            Ok(RecordKind::BaseLz4) => (BlockContent::Base, Compression::Lz4),
            Ok(RecordKind::DeltaRaw) => (BlockContent::Delta, Compression::Raw),
            Ok(RecordKind::DeltaLz4) => (BlockContent::Delta, Compression::Lz4),
            Ok(RecordKind::Index | RecordKind::Trailer) => {
                return Err(FormatError::UnexpectedRecordKind {
                    code: tag >> KIND_SHIFT,
                });
            }
            Err(error) => return Err(error),
        };
        let body_len = tag & LEN_MASK;
        if body_len as usize > MAX_BODY_LEN {
            return Err(FormatError::RecordTooLong {
                len: body_len as u64,
                max: MAX_BODY_LEN as u64,
            });
        }
        if matches!(compression, Compression::Lz4) && (body_len as usize) < MIN_LZ4_BODY_LEN {
            return Err(FormatError::Truncated {
                needed: FRAME_LEN + MIN_LZ4_BODY_LEN,
                available: FRAME_LEN + body_len as usize,
            });
        }
        Ok(Self {
            content,
            compression,
            body_len,
        })
    }
}

struct Frame<'a> {
    tag: [u8; 4],
    body: &'a [u8],
}

impl<'a> Frame<'a> {
    fn read(bytes: &'a [u8], max_body_len: usize) -> Result<(Self, &'a [u8]), FormatError> {
        let mut fields = bytes;
        let (Some(tag), Some(checksum)) = (take(&mut fields), take(&mut fields)) else {
            return Err(FormatError::Truncated {
                needed: FRAME_LEN,
                available: bytes.len(),
            });
        };
        let body_len = (u32::from_le_bytes(tag) & LEN_MASK) as usize;
        let max = max_body_len.min(MAX_BODY_LEN);
        if body_len > max {
            return Err(FormatError::RecordTooLong {
                len: body_len as u64,
                max: max as u64,
            });
        }
        let (body, rest) = fields
            .split_at_checked(body_len)
            .ok_or(FormatError::Truncated {
                needed: FRAME_LEN + body_len,
                available: bytes.len(),
            })?;
        Checksum::decode(checksum).verify(&[&tag, body])?;
        Ok((Self { tag, body }, rest))
    }

    fn read_exact(bytes: &'a [u8], max_body_len: usize) -> Result<Self, FormatError> {
        let (frame, rest) = Self::read(bytes, max_body_len)?;
        if rest.is_empty() {
            Ok(frame)
        } else {
            Err(FormatError::InconsistentLength {
                expected: (bytes.len() - rest.len()) as u64,
                found: bytes.len() as u64,
            })
        }
    }

    fn kind(&self, version: FormatVersion) -> Result<RecordKind, FormatError> {
        RecordKind::of_tag(u32::from_le_bytes(self.tag), version)
    }

    fn expect(&self, kind: RecordKind, version: FormatVersion) -> Result<(), FormatError> {
        if self.kind(version)? == kind {
            Ok(())
        } else {
            Err(FormatError::UnexpectedRecordKind {
                code: u32::from_le_bytes(self.tag) >> KIND_SHIFT,
            })
        }
    }
}

fn begin_frame(out: &mut Vec<u8>) -> usize {
    let start = out.len();
    out.extend_from_slice(&[0; FRAME_LEN]);
    start
}

fn finish_frame(tag: [u8; 4], start: usize, out: &mut [u8]) {
    let (frame, body) = out[start..].split_at_mut(FRAME_LEN);
    let checksum = Checksum::of(&[&tag, body]);
    frame[..4].copy_from_slice(&tag);
    frame[4..].copy_from_slice(&checksum.encode());
}

fn body_len_u32(len: usize) -> Result<u32, FormatError> {
    u32::try_from(len)
        .ok()
        .filter(|_| len <= MAX_BODY_LEN)
        .ok_or(FormatError::RecordTooLong {
            len: len as u64,
            max: MAX_BODY_LEN as u64,
        })
}

fn append_lz4_body(payload: &[u8], raw_len: u32, out: &mut Vec<u8>) -> Option<u32> {
    let body_at = out.len();
    let stream_at = body_at + RAW_LEN_FIELD;
    out.extend_from_slice(&raw_len.to_le_bytes());
    out.resize(
        stream_at + lz4_flex::block::get_maximum_output_size(payload.len()),
        0,
    );
    let body_len = lz4_flex::block::compress_into(payload, &mut out[stream_at..])
        .ok()
        .map(|stream_len| RAW_LEN_FIELD + stream_len)
        .filter(|&body_len| body_len < payload.len())
        .and_then(|body_len| u32::try_from(body_len).ok());
    out.truncate(body_at + body_len.map_or(0, |body_len| body_len as usize));
    body_len
}

/// Appends `payload` to `out` as one block and returns the block's tag.
///
/// With [`Compression::Lz4`] the payload is compressed, and the block is
/// stored that way only if that makes it smaller than storing it raw.
/// Otherwise, and with [`Compression::Raw`], the block is raw. Either way the
/// block is at most [`FRAME_LEN`] bytes longer than `payload`.
///
/// The caller keeps the returned tag, with the page id, for the footer index.
pub fn write_block(
    content: BlockContent,
    compression: Compression,
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<BlockTag, FormatError> {
    let raw_len = body_len_u32(payload.len())?;
    let start = begin_frame(out);
    let lz4_len = match compression {
        Compression::Raw => None,
        Compression::Lz4 => append_lz4_body(payload, raw_len, out),
    };
    let tag = match lz4_len {
        Some(body_len) => BlockTag {
            content,
            compression: Compression::Lz4,
            body_len,
        },
        None => {
            out.extend_from_slice(payload);
            BlockTag {
                content,
                compression: Compression::Raw,
                body_len: raw_len,
            }
        }
    };
    finish_frame(tag.encode(), start, out);
    Ok(tag)
}

/// One block of a segment, with its checksum verified.
///
/// The payload of an LZ4 block is not decompressed until it is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Block<'a> {
    tag: BlockTag,
    stored: &'a [u8],
    payload_len: u32,
}

impl<'a> Block<'a> {
    /// Reads the block that `bytes` is: what a pointer to a block leads to.
    ///
    /// `bytes` must be the whole block and nothing else. `max_payload_len` is
    /// the longest payload the caller accepts, which bounds what
    /// [`Self::payload`] allocates.
    pub fn decode(
        version: FormatVersion,
        bytes: &'a [u8],
        max_payload_len: usize,
    ) -> Result<Self, FormatError> {
        Self::of_frame(
            version,
            Frame::read_exact(bytes, max_payload_len)?,
            max_payload_len,
        )
    }

    /// Reads the block at the start of `bytes`, and returns it with the bytes
    /// after it. This is how a segment is walked block by block.
    pub fn read(
        version: FormatVersion,
        bytes: &'a [u8],
        max_payload_len: usize,
    ) -> Result<(Self, &'a [u8]), FormatError> {
        let (frame, rest) = Frame::read(bytes, max_payload_len)?;
        Ok((Self::of_frame(version, frame, max_payload_len)?, rest))
    }

    fn of_frame(
        version: FormatVersion,
        frame: Frame<'a>,
        max_payload_len: usize,
    ) -> Result<Self, FormatError> {
        let tag = BlockTag::decode(frame.tag, version)?;
        let mut stored = frame.body;
        let payload_len = match tag.compression {
            Compression::Raw => tag.body_len,
            Compression::Lz4 => {
                let raw_len =
                    u32::from_le_bytes(take(&mut stored).ok_or(FormatError::Truncated {
                        needed: FRAME_LEN + MIN_LZ4_BODY_LEN,
                        available: tag.block_len(),
                    })?);
                let max = max_payload_len.min(MAX_BODY_LEN);
                if raw_len as usize > max {
                    return Err(FormatError::RecordTooLong {
                        len: u64::from(raw_len),
                        max: max as u64,
                    });
                }
                if tag.body_len >= raw_len {
                    return Err(FormatError::CompressedBlockNotSmaller {
                        stored: tag.body_len,
                        raw: raw_len,
                    });
                }
                raw_len
            }
        };
        Ok(Self {
            tag,
            stored,
            payload_len,
        })
    }

    /// The block's tag.
    pub const fn tag(&self) -> BlockTag {
        self.tag
    }

    /// What the payload is.
    pub const fn content(&self) -> BlockContent {
        self.tag.content
    }

    /// How the payload is stored.
    pub const fn compression(&self) -> Compression {
        self.tag.compression
    }

    /// The length of the payload, decompressed.
    pub const fn payload_len(&self) -> usize {
        self.payload_len as usize
    }

    /// The payload, borrowed from the block if it is raw and decompressed
    /// into a new buffer if it is LZ4.
    pub fn payload(&self) -> Result<Cow<'a, [u8]>, FormatError> {
        match self.tag.compression {
            Compression::Raw => Ok(Cow::Borrowed(self.stored)),
            Compression::Lz4 => {
                let mut payload = vec![0; self.payload_len()];
                self.decompress_to(&mut payload)?;
                Ok(Cow::Owned(payload))
            }
        }
    }

    /// Writes the payload to `out`, which must be exactly
    /// [`Self::payload_len`] bytes long. Nothing is allocated.
    pub fn copy_payload_to(&self, out: &mut [u8]) -> Result<(), FormatError> {
        if out.len() != self.payload_len() {
            return Err(FormatError::InconsistentLength {
                expected: u64::from(self.payload_len),
                found: out.len() as u64,
            });
        }
        match self.tag.compression {
            Compression::Raw => {
                out.copy_from_slice(self.stored);
                Ok(())
            }
            Compression::Lz4 => self.decompress_to(out),
        }
    }

    fn decompress_to(&self, out: &mut [u8]) -> Result<(), FormatError> {
        match lz4_flex::block::decompress_into(self.stored, out) {
            Ok(written) if written == out.len() => Ok(()),
            _ => Err(FormatError::Decompression),
        }
    }
}

/// What the footer index records for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndexEntry {
    /// The page the block belongs to.
    pub page: PageId,
    /// The block's tag, as [`write_block`] returned it.
    pub tag: BlockTag,
}

impl IndexEntry {
    /// Bytes an entry takes on disk.
    pub const ENCODED_LEN: usize = 8;

    fn decode(
        bytes: &[u8; Self::ENCODED_LEN],
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        let [p0, p1, p2, p3, t0, t1, t2, t3] = *bytes;
        Ok(Self {
            page: PageId::decode([p0, p1, p2, p3]).ok_or(FormatError::MissingId)?,
            tag: BlockTag::decode([t0, t1, t2, t3], version)?,
        })
    }
}

fn blocks_end(blocks: impl IntoIterator<Item = BlockTag>) -> Option<u64> {
    blocks.into_iter().try_fold(DATA_START, |end, tag| {
        end.checked_add(tag.block_len() as u64)
    })
}

/// Appends the footer of a segment to `out`: the index record and the
/// trailer. This seals the segment.
///
/// `blocks` are the segment's blocks in file order. `index_offset` is the file
/// offset the footer is written at. It must be where the last block ends,
/// which this checks.
pub fn write_footer(
    blocks: &[IndexEntry],
    index_offset: u64,
    out: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let index_len = blocks.len().saturating_mul(IndexEntry::ENCODED_LEN);
    let block_count = match u32::try_from(blocks.len()) {
        Ok(count) if blocks.len() <= MAX_BLOCKS => count,
        _ => {
            return Err(FormatError::RecordTooLong {
                len: index_len as u64,
                max: MAX_BODY_LEN as u64,
            });
        }
    };
    let expected = blocks_end(blocks.iter().map(|block| block.tag));
    if expected != Some(index_offset) {
        return Err(FormatError::InconsistentLength {
            expected: expected.unwrap_or(u64::MAX),
            found: index_offset,
        });
    }

    out.reserve(FRAME_LEN + index_len + SegmentTrailer::ENCODED_LEN);
    let start = begin_frame(out);
    for block in blocks {
        out.extend_from_slice(&block.page.encode());
        out.extend_from_slice(&block.tag.encode());
    }
    let index_tag = RecordKind::Index.tag(block_count * IndexEntry::ENCODED_LEN as u32);
    finish_frame(index_tag.to_le_bytes(), start, out);

    let start = begin_frame(out);
    out.extend_from_slice(&index_offset.to_le_bytes());
    out.extend_from_slice(&block_count.to_le_bytes());
    let trailer_tag = RecordKind::Trailer.tag(TRAILER_BODY_LEN as u32);
    finish_frame(trailer_tag.to_le_bytes(), start, out);
    Ok(())
}

/// The last record of a sealed segment. It says where the index record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmentTrailer {
    index_offset: u64,
    block_count: u32,
}

impl SegmentTrailer {
    /// Bytes the trailer takes on disk, frame included.
    pub const ENCODED_LEN: usize = FRAME_LEN + TRAILER_BODY_LEN;

    /// Reads the trailer from `bytes`, the last [`Self::ENCODED_LEN`] bytes of
    /// a sealed segment whose valid length is `valid_len`.
    pub fn decode(
        version: FormatVersion,
        bytes: &[u8],
        valid_len: u64,
    ) -> Result<Self, FormatError> {
        let frame = Frame::read_exact(bytes, TRAILER_BODY_LEN)?;
        frame.expect(RecordKind::Trailer, version)?;
        let mut body = frame.body;
        let (Some(index_offset), Some(block_count), []) = (take(&mut body), take(&mut body), body)
        else {
            return Err(FormatError::InconsistentLength {
                expected: TRAILER_BODY_LEN as u64,
                found: frame.body.len() as u64,
            });
        };
        let trailer = Self {
            index_offset: u64::from_le_bytes(index_offset),
            block_count: u32::from_le_bytes(block_count),
        };
        if trailer.block_count() > MAX_BLOCKS {
            return Err(FormatError::RecordTooLong {
                len: u64::from(trailer.block_count) * IndexEntry::ENCODED_LEN as u64,
                max: MAX_BODY_LEN as u64,
            });
        }
        let footer_len = (trailer.index_len() + Self::ENCODED_LEN) as u64;
        match valid_len.checked_sub(footer_len) {
            Some(expected) if expected >= DATA_START => {
                if trailer.index_offset == expected {
                    Ok(trailer)
                } else {
                    Err(FormatError::InconsistentLength {
                        expected,
                        found: trailer.index_offset,
                    })
                }
            }
            _ => Err(FormatError::InconsistentLength {
                expected: DATA_START + footer_len,
                found: valid_len,
            }),
        }
    }

    /// The file offset of the index record, which is where the last block
    /// ends.
    pub const fn index_offset(&self) -> u64 {
        self.index_offset
    }

    /// How many blocks the segment holds.
    pub const fn block_count(&self) -> usize {
        self.block_count as usize
    }

    /// The length of the index record in the file, frame included. The index
    /// record is the `index_len` bytes that start at [`Self::index_offset`].
    pub const fn index_len(&self) -> usize {
        FRAME_LEN + self.block_count() * IndexEntry::ENCODED_LEN
    }
}

/// The footer index of a sealed segment: every block's page, tag and offset.
///
/// It borrows the index record and holds nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmentIndex<'a> {
    entries: &'a [u8],
    version: FormatVersion,
}

impl<'a> SegmentIndex<'a> {
    /// Reads the index record `bytes` that `trailer` points to.
    pub fn decode(
        version: FormatVersion,
        trailer: &SegmentTrailer,
        bytes: &'a [u8],
    ) -> Result<Self, FormatError> {
        let entries_len = trailer.block_count() * IndexEntry::ENCODED_LEN;
        let frame = Frame::read_exact(bytes, entries_len)?;
        frame.expect(RecordKind::Index, version)?;
        let (entries, []) = frame.body.as_chunks() else {
            return Err(FormatError::InconsistentLength {
                expected: entries_len as u64,
                found: frame.body.len() as u64,
            });
        };
        if entries.len() != trailer.block_count() {
            return Err(FormatError::InconsistentLength {
                expected: entries_len as u64,
                found: frame.body.len() as u64,
            });
        }
        let mut end = Some(DATA_START);
        for entry in entries {
            let entry = IndexEntry::decode(entry, version)?;
            end = end.and_then(|end| end.checked_add(entry.tag.block_len() as u64));
        }
        if end != Some(trailer.index_offset) {
            return Err(FormatError::InconsistentLength {
                expected: end.unwrap_or(u64::MAX),
                found: trailer.index_offset,
            });
        }
        Ok(Self {
            entries: frame.body,
            version,
        })
    }

    /// Reads the footer from `tail`, the last bytes of a sealed segment whose
    /// valid length is `valid_len`.
    ///
    /// `tail` may be any number of bytes that end at the valid length. If it
    /// is too short to hold the footer, the error is
    /// [`FormatError::Truncated`], and its `needed` is how many bytes from the
    /// end the footer takes. A reader can so try a fixed amount first and
    /// read again only for a large index.
    pub fn decode_tail(
        version: FormatVersion,
        tail: &'a [u8],
        valid_len: u64,
    ) -> Result<Self, FormatError> {
        let truncated = |needed| FormatError::Truncated {
            needed,
            available: tail.len(),
        };
        let trailer_at = tail
            .len()
            .checked_sub(SegmentTrailer::ENCODED_LEN)
            .ok_or(truncated(SegmentTrailer::ENCODED_LEN))?;
        let (before, trailer) = tail.split_at(trailer_at);
        let trailer = SegmentTrailer::decode(version, trailer, valid_len)?;
        let index_at = before
            .len()
            .checked_sub(trailer.index_len())
            .ok_or(truncated(trailer.index_len() + SegmentTrailer::ENCODED_LEN))?;
        Self::decode(version, &trailer, &before[index_at..])
    }

    /// How many blocks the segment holds.
    pub const fn len(&self) -> usize {
        self.entries.len() / IndexEntry::ENCODED_LEN
    }

    /// Whether the segment holds no block.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The segment's blocks in file order, each with its offset.
    pub fn blocks(&self) -> impl Iterator<Item = IndexedBlock> + 'a {
        let version = self.version;
        self.entries
            .as_chunks()
            .0
            .iter()
            .scan(DATA_START, move |offset, entry| {
                let IndexEntry { page, tag } = IndexEntry::decode(entry, version).ok()?;
                let block = IndexedBlock {
                    page,
                    tag,
                    offset: *offset,
                };
                *offset += tag.block_len() as u64;
                Some(block)
            })
    }
}

/// One block as the footer index lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndexedBlock {
    /// The page the block belongs to.
    pub page: PageId,
    /// The block's tag. `tag.block_len()` is the block's length in the file.
    pub tag: BlockTag,
    /// The file offset the block starts at.
    pub offset: u64,
}

/// Reads the file header at the start of the segment file named `id`, and
/// returns the format version the rest of the file is in.
///
/// Rejects a file of another kind, and a segment whose header carries another
/// id than its name (H3).
pub fn decode_file_header(bytes: &[u8], id: FileId) -> Result<FormatVersion, FormatError> {
    let header = FileHeader::decode(bytes)?;
    if header.kind() != FileKind::Segment {
        return Err(FormatError::WrongFileKind {
            expected: FileKind::Segment,
            found: header.kind(),
        });
    }
    if header.sequence() != id.get() {
        return Err(FormatError::WrongFileSequence {
            expected: id.get(),
            found: header.sequence(),
        });
    }
    Ok(header.version())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::SplitMix64;

    const V1: FormatVersion = FormatVersion::V1;
    const MAX: usize = 1 << 20;
    const CONTENTS: [BlockContent; 2] = [BlockContent::Base, BlockContent::Delta];
    const COMPRESSIONS: [Compression; 2] = [Compression::Raw, Compression::Lz4];

    const GOLDEN_RAW_BASE: &str = "06000008e6e37a086c69676e696e";
    const GOLDEN_RAW_DELTA_EMPTY: &str = "000000189990524b";
    const GOLDEN_LZ4_STREAM: &str = "4f61626364040004506461626364";
    const GOLDEN_LZ4_BASE: &str = "120000104316a897200000004f61626364040004506461626364";
    const GOLDEN_LZ4_DELTA: &str = "12000020d3793ff9200000004f61626364040004506461626364";
    const GOLDEN_SEGMENT: &str = "4c49474e494e44420300010036010000d2c72fa6\
        06000008e6e37a086c69676e696e\
        020000186e7c60c56431\
        120000104316a897200000004f61626364040004506461626364\
        1800002863fe5481070000000600000807000000020000180900000012000010\
        0c000030ea3cf000460000000000000003000000";
    const GOLDEN_EMPTY_FOOTER: &str = "00000028dfae30de0c0000306593e40e140000000000000000000000";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn page(raw: u32) -> PageId {
        PageId::new(raw).expect("non-zero")
    }

    fn tag_word(kind: u32, body_len: usize) -> u32 {
        kind << 27 | body_len as u32
    }

    fn forge(kind: u32, body: &[u8]) -> Vec<u8> {
        let tag = tag_word(kind, body.len()).to_le_bytes();
        let mut record = tag.to_vec();
        record.extend_from_slice(&Checksum::of(&[&tag, body]).encode());
        record.extend_from_slice(body);
        record
    }

    fn forge_lz4(kind: u32, raw_len: u32, stream: &[u8]) -> Vec<u8> {
        let mut body = raw_len.to_le_bytes().to_vec();
        body.extend_from_slice(stream);
        forge(kind, &body)
    }

    fn forge_footer(
        entries: &[(u32, u32)],
        index_kind: u32,
        trailer_kind: u32,
        index_offset: u64,
        block_count: u32,
    ) -> Vec<u8> {
        let index: Vec<u8> = entries
            .iter()
            .flat_map(|(page, tag)| [page.to_le_bytes(), tag.to_le_bytes()].concat())
            .collect();
        let mut trailer = index_offset.to_le_bytes().to_vec();
        trailer.extend_from_slice(&block_count.to_le_bytes());
        [forge(index_kind, &index), forge(trailer_kind, &trailer)].concat()
    }

    fn reference_lz4_decode(stream: &[u8]) -> Option<Vec<u8>> {
        fn length(nibble: u8, stream: &[u8], at: &mut usize) -> Option<usize> {
            let mut length = usize::from(nibble);
            if nibble == 15 {
                loop {
                    let byte = *stream.get(*at)?;
                    *at += 1;
                    length += usize::from(byte);
                    if byte != 255 {
                        break;
                    }
                }
            }
            Some(length)
        }

        let (mut out, mut at) = (Vec::new(), 0);
        loop {
            let token = *stream.get(at)?;
            at += 1;
            let literals = length(token >> 4, stream, &mut at)?;
            out.extend_from_slice(stream.get(at..at.checked_add(literals)?)?);
            at += literals;
            if at == stream.len() {
                return Some(out);
            }
            let offset = usize::from(u16::from_le_bytes([*stream.get(at)?, *stream.get(at + 1)?]));
            at += 2;
            if offset == 0 || offset > out.len() {
                return None;
            }
            let matched = length(token & 15, stream, &mut at)? + 4;
            for _ in 0..matched {
                out.push(out[out.len() - offset]);
            }
        }
    }

    fn compressible(rng: &mut SplitMix64, len: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(len + 64);
        while data.len() < len {
            let count = (rng.next_u64() % 48) as usize + 1;
            match rng.next_u64() % 4 {
                0 => data.extend(std::iter::repeat_n(rng.next_u64() as u8, count)),
                1 if data.len() >= 16 => {
                    let from = (rng.next_u64() as usize) % (data.len() - 15);
                    for at in 0..count {
                        data.push(data[from + at % 16]);
                    }
                }
                2 => data.extend_from_slice(&rng.bytes(count % 6 + 1)),
                _ => data.extend_from_slice(b"lignin page "),
            }
        }
        data.truncate(len);
        data
    }

    fn sample_payloads(rng: &mut SplitMix64) -> Vec<Vec<u8>> {
        let lens = (0..=300).chain([1000, 4095, 4096, 4097, 8192, 65_535, 65_536, 70_000]);
        lens.flat_map(|len| [rng.bytes(len), compressible(rng, len), vec![0; len]])
            .collect()
    }

    struct Built {
        file: Vec<u8>,
        payloads: Vec<Vec<u8>>,
        entries: Vec<IndexEntry>,
        index_offset: u64,
    }

    impl Built {
        fn footer(&self) -> &[u8] {
            &self.file[self.index_offset as usize..]
        }

        fn valid_len(&self) -> u64 {
            self.file.len() as u64
        }
    }

    fn build_segment(rng: &mut SplitMix64, blocks: usize) -> Built {
        let header = FileHeader::new(FileKind::Segment, V1, 310).expect("valid");
        let mut file = header.encode().to_vec();
        let (mut payloads, mut entries) = (Vec::new(), Vec::new());
        for _ in 0..blocks {
            let len = (rng.next_u64() % 400) as usize;
            let payload = if rng.next_u64().is_multiple_of(2) {
                compressible(rng, len)
            } else {
                rng.bytes(len)
            };
            let content = CONTENTS[(rng.next_u64() % 2) as usize];
            let compression = COMPRESSIONS[(rng.next_u64() % 2) as usize];
            let tag = write_block(content, compression, &payload, &mut file).expect("fits");
            entries.push(IndexEntry {
                page: page(rng.next_u64() as u32 | 1),
                tag,
            });
            payloads.push(payload);
        }
        let index_offset = file.len() as u64;
        write_footer(&entries, index_offset, &mut file).expect("consistent");
        Built {
            file,
            payloads,
            entries,
            index_offset,
        }
    }

    #[test]
    fn limits_are_the_ones_the_format_states() {
        assert_eq!(FRAME_LEN, 8);
        assert_eq!(MAX_BODY_LEN, (1 << 27) - 4);
        assert_eq!(MAX_BLOCKS, (1 << 24) - 1);
        assert_eq!(DATA_START, 20);
        assert_eq!(SegmentTrailer::ENCODED_LEN, 20);
        assert_eq!(IndexEntry::ENCODED_LEN, 8);
        assert_eq!(BlockTag::ENCODED_LEN, 4);
        assert_eq!(LEN_MASK as usize, MAX_BODY_LEN + 3);
    }

    #[test]
    fn blocks_match_the_golden_bytes() {
        let mut out = Vec::new();
        let tag = write_block(BlockContent::Base, Compression::Raw, b"lignin", &mut out);
        assert_eq!(hex(&out), GOLDEN_RAW_BASE);
        assert_eq!(tag.expect("fits").encode(), [0x06, 0x00, 0x00, 0x08]);

        out.clear();
        for compression in COMPRESSIONS {
            write_block(BlockContent::Delta, compression, b"", &mut out).expect("fits");
        }
        assert_eq!(hex(&out), GOLDEN_RAW_DELTA_EMPTY.repeat(2));

        let payload = b"abcd".repeat(8);
        assert_eq!(
            reference_lz4_decode(&unhex(GOLDEN_LZ4_STREAM)),
            Some(payload.clone())
        );
        for (golden, content) in [GOLDEN_LZ4_BASE, GOLDEN_LZ4_DELTA]
            .into_iter()
            .zip(CONTENTS)
        {
            let bytes = unhex(golden);
            let block = Block::decode(V1, &bytes, 32).expect("valid block");
            assert_eq!(block.content(), content);
            assert_eq!(block.compression(), Compression::Lz4);
            assert_eq!(block.payload_len(), 32);
            assert_eq!(block.tag().body_len(), 18);
            assert_eq!(block.tag().block_len(), bytes.len());
            assert_eq!(block.tag().encode(), bytes[..4]);
            assert_eq!(block.payload().expect("valid stream"), payload.as_slice());
        }
    }

    #[test]
    fn a_sealed_segment_matches_the_golden_bytes() {
        let golden = unhex(GOLDEN_SEGMENT);
        let id = FileId::new(310).expect("non-zero");

        let header = FileHeader::new(FileKind::Segment, V1, id.get()).expect("valid");
        let mut file = header.encode().to_vec();
        let base = write_block(BlockContent::Base, Compression::Raw, b"lignin", &mut file);
        let delta = write_block(BlockContent::Delta, Compression::Raw, b"d1", &mut file);
        let lz4 = unhex(GOLDEN_LZ4_BASE);
        file.extend_from_slice(&lz4);
        let entries = [
            IndexEntry {
                page: page(7),
                tag: base.expect("fits"),
            },
            IndexEntry {
                page: page(7),
                tag: delta.expect("fits"),
            },
            IndexEntry {
                page: page(9),
                tag: Block::decode(V1, &lz4, MAX).expect("valid block").tag(),
            },
        ];
        assert_eq!(file.len(), 70);
        write_footer(&entries, 70, &mut file).expect("consistent");
        assert_eq!(hex(&file), hex(&golden));

        assert_eq!(decode_file_header(&golden, id), Ok(V1));
        let index = SegmentIndex::decode_tail(V1, &golden, 122).expect("valid footer");
        assert_eq!(index.len(), 3);
        let blocks: Vec<IndexedBlock> = index.blocks().collect();
        assert_eq!(
            blocks
                .iter()
                .map(|block| (block.page.get(), block.offset, block.tag.block_len()))
                .collect::<Vec<_>>(),
            [(7, 20, 14), (7, 34, 10), (9, 44, 26)]
        );
        let payloads: [&[u8]; 3] = [b"lignin", b"d1", &b"abcd".repeat(8)];
        for (indexed, payload) in blocks.iter().zip(payloads) {
            let at = indexed.offset as usize;
            let block = Block::decode(V1, &golden[at..at + indexed.tag.block_len()], MAX);
            let block = block.expect("valid block");
            assert_eq!(block.tag(), indexed.tag);
            assert_eq!(block.payload().expect("valid payload"), payload);
        }
    }

    #[test]
    fn an_empty_sealed_segment_matches_the_golden_bytes() {
        let mut footer = Vec::new();
        write_footer(&[], DATA_START, &mut footer).expect("consistent");
        assert_eq!(hex(&footer), GOLDEN_EMPTY_FOOTER);
        let index = SegmentIndex::decode_tail(V1, &footer, 48).expect("valid footer");
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.blocks().count(), 0);
    }

    #[test]
    fn blocks_round_trip() {
        let mut rng = SplitMix64::new(0x5E6);
        let (mut raw, mut lz4) = (0, 0);
        for payload in sample_payloads(&mut rng) {
            for content in CONTENTS {
                for compression in COMPRESSIONS {
                    let mut out = vec![0xEE; 3];
                    let tag = write_block(content, compression, &payload, &mut out).expect("fits");
                    let bytes = &out[3..];
                    assert_eq!(tag.content(), content);
                    assert_eq!(tag.block_len(), bytes.len());
                    assert_eq!(tag.encode(), bytes[..4]);
                    assert_eq!(BlockTag::decode(tag.encode(), V1), Ok(tag));

                    let block = Block::decode(V1, bytes, payload.len()).expect("valid block");
                    assert_eq!(block.tag(), tag);
                    assert_eq!(block.content(), content);
                    assert_eq!(block.compression(), tag.compression());
                    assert_eq!(block.payload_len(), payload.len());
                    assert_eq!(block.payload().expect("valid payload"), payload.as_slice());
                    let mut copy = vec![0xEE; payload.len()];
                    block.copy_payload_to(&mut copy).expect("right length");
                    assert_eq!(copy, payload);

                    let mut longer = bytes.to_vec();
                    longer.extend_from_slice(b"next");
                    let (read, rest) = Block::read(V1, &longer, MAX).expect("valid block");
                    assert_eq!(read, block);
                    assert_eq!(rest, b"next");

                    match tag.compression() {
                        Compression::Raw => {
                            raw += 1;
                            assert_eq!(&bytes[FRAME_LEN..], payload.as_slice());
                            assert!(matches!(block.payload(), Ok(Cow::Borrowed(_))));
                            let mut again = Vec::new();
                            let again_tag =
                                write_block(content, Compression::Raw, &payload, &mut again);
                            assert_eq!(again_tag, Ok(tag));
                            assert_eq!(again, bytes);
                        }
                        Compression::Lz4 => {
                            lz4 += 1;
                            assert_eq!(compression, Compression::Lz4);
                            let stream = &bytes[FRAME_LEN + RAW_LEN_FIELD..];
                            assert_eq!(reference_lz4_decode(stream).as_ref(), Some(&payload));
                        }
                    }
                }
            }
        }
        assert!(raw > 1_000 && lz4 > 500, "raw {raw}, lz4 {lz4}");
    }

    #[test]
    fn an_incompressible_block_costs_at_most_eight_bytes_of_overhead() {
        let mut rng = SplitMix64::new(0xB10C);
        let lens = (0..=600).chain([4095, 4096, 4097, 8192, 65_535, 65_536, 65_537, MAX + 1]);
        for len in lens {
            let payload = rng.bytes(len);
            for content in CONTENTS {
                for compression in COMPRESSIONS {
                    let mut out = Vec::new();
                    let tag = write_block(content, compression, &payload, &mut out).expect("fits");
                    assert_eq!(tag.compression(), Compression::Raw, "{len} bytes");
                    assert_eq!(out.len() - payload.len(), 8, "{len} bytes");
                    assert_eq!(&out[8..], payload.as_slice());
                }
            }
        }
    }

    #[test]
    fn no_block_is_more_than_eight_bytes_longer_than_its_payload() {
        let mut rng = SplitMix64::new(0x0EAD);
        let mut smaller = 0;
        for payload in sample_payloads(&mut rng) {
            let mut raw = Vec::new();
            write_block(BlockContent::Base, Compression::Raw, &payload, &mut raw).expect("fits");
            assert_eq!(raw.len(), payload.len() + 8);

            let mut out = Vec::new();
            let tag = write_block(BlockContent::Base, Compression::Lz4, &payload, &mut out);
            match tag.expect("fits").compression() {
                Compression::Raw => assert_eq!(out, raw),
                Compression::Lz4 => {
                    smaller += 1;
                    assert!(out.len() < raw.len());
                }
            }
            assert!(out.len() <= payload.len() + 8);
        }
        assert!(smaller > 500, "{smaller}");
    }

    #[test]
    fn lz4_is_valid_only_when_the_block_is_smaller_than_the_raw_one() {
        let stream = |matches: usize| {
            let mut stream = b"\x40abcd\x04\x00".to_vec();
            stream.extend_from_slice(&b"\x00\x04\x00".repeat(matches - 1));
            stream.push(0x00);
            stream
        };
        let payload = |matches: usize| b"abcd".repeat(matches + 1);
        for matches in 1..=12 {
            let (stream, payload) = (stream(matches), payload(matches));
            assert_eq!(reference_lz4_decode(&stream), Some(payload.clone()));
            let body_len = (RAW_LEN_FIELD + stream.len()) as u32;
            let raw_len = payload.len() as u32;
            let bytes = forge_lz4(2, raw_len, &stream);
            let block = Block::decode(V1, &bytes, MAX);
            if body_len < raw_len {
                assert!(matches >= 6);
                let block = block.expect("smaller than raw");
                assert_eq!(block.payload().expect("valid stream"), payload.as_slice());
                assert!(bytes.len() < payload.len() + FRAME_LEN);
            } else {
                assert!(matches <= 5);
                assert_eq!(
                    block,
                    Err(FormatError::CompressedBlockNotSmaller {
                        stored: body_len,
                        raw: raw_len,
                    })
                );
            }
        }
        assert_eq!(stream(6).len() + RAW_LEN_FIELD + 1, payload(6).len());
        assert_eq!(stream(5).len() + RAW_LEN_FIELD, payload(5).len());
    }

    #[test]
    fn a_forged_lz4_block_is_rejected() {
        for len in 0..RAW_LEN_FIELD + 1 {
            assert_eq!(
                Block::decode(V1, &forge(2, &[0x55; 8][..len]), MAX),
                Err(FormatError::Truncated {
                    needed: 13,
                    available: 8 + len,
                })
            );
        }

        let stream = unhex(GOLDEN_LZ4_STREAM);
        assert!(Block::decode(V1, &forge_lz4(4, 32, &stream), 32).is_ok());
        assert_eq!(
            Block::decode(V1, &forge_lz4(4, 32, &stream), 31),
            Err(FormatError::RecordTooLong { len: 32, max: 31 })
        );
        assert_eq!(
            Block::decode(V1, &forge_lz4(4, u32::MAX, &stream), usize::MAX),
            Err(FormatError::RecordTooLong {
                len: u64::from(u32::MAX),
                max: MAX_BODY_LEN as u64,
            })
        );

        for raw_len in [19, 31, 33, 1000] {
            let bytes = forge_lz4(2, raw_len, &stream);
            let block = Block::decode(V1, &bytes, MAX).expect("the frame is valid");
            assert_eq!(block.payload_len(), raw_len as usize);
            assert_eq!(block.payload(), Err(FormatError::Decompression));
            let mut out = vec![0; raw_len as usize];
            assert_eq!(
                block.copy_payload_to(&mut out),
                Err(FormatError::Decompression)
            );
        }
        for stream in [&[0xF0, 0xFF, 0xFF][..], &[0x00, 0x01, 0x00, 0x00], &[0x10]] {
            let block = forge_lz4(2, 64, stream);
            let block = Block::decode(V1, &block, MAX).expect("the frame is valid");
            assert_eq!(block.payload(), Err(FormatError::Decompression));
        }

        let bytes = unhex(GOLDEN_LZ4_BASE);
        let block = Block::decode(V1, &bytes, MAX).expect("valid block");
        for len in [0, 31, 33] {
            assert_eq!(
                block.copy_payload_to(&mut vec![0; len]),
                Err(FormatError::InconsistentLength {
                    expected: 32,
                    found: len as u64,
                })
            );
        }
        let raw = unhex(GOLDEN_RAW_BASE);
        let raw = Block::decode(V1, &raw, MAX).expect("valid block");
        assert_eq!(
            raw.copy_payload_to(&mut [0; 7]),
            Err(FormatError::InconsistentLength {
                expected: 6,
                found: 7,
            })
        );
    }

    #[test]
    fn lz4_decoding_agrees_with_a_reference_decoder_on_damaged_streams() {
        let mut rng = SplitMix64::new(0x124);
        let (mut accepted, mut rejected) = (0, 0);
        for round in 0..30_000 {
            let len = (rng.next_u64() % 600) as usize + 40;
            let payload = compressible(&mut rng, len);
            let mut block = Vec::new();
            let tag = write_block(BlockContent::Base, Compression::Lz4, &payload, &mut block);
            if tag.expect("fits").compression() == Compression::Raw {
                continue;
            }
            let mut stream = block[FRAME_LEN + RAW_LEN_FIELD..].to_vec();
            for _ in 0..rng.next_u64() % 3 {
                let at = (rng.next_u64() as usize) % stream.len();
                match rng.next_u64() % 4 {
                    0 => stream[at] = rng.next_u64() as u8,
                    1 => stream[at] ^= 1 << (rng.next_u64() % 8),
                    2 => stream.truncate(at.max(1)),
                    _ => stream.insert(at, rng.next_u64() as u8),
                }
            }
            let raw_len = match round % 8 {
                0 => payload.len() - 1,
                1 => payload.len() + 1,
                _ => payload.len(),
            };
            if RAW_LEN_FIELD + stream.len() >= raw_len {
                continue;
            }

            let bytes = forge_lz4(2, raw_len as u32, &stream);
            let block = Block::decode(V1, &bytes, MAX).expect("the frame is valid");
            let expected = reference_lz4_decode(&stream).filter(|out| out.len() == raw_len);
            match expected {
                Some(expected) => {
                    accepted += 1;
                    assert_eq!(block.payload().expect("valid stream"), expected.as_slice());
                    let mut out = vec![0xEE; raw_len];
                    block.copy_payload_to(&mut out).expect("valid stream");
                    assert_eq!(out, expected);
                }
                None => {
                    rejected += 1;
                    assert_eq!(block.payload(), Err(FormatError::Decompression));
                }
            }
        }
        assert!(
            accepted > 5_000 && rejected > 5_000,
            "{accepted} {rejected}"
        );
    }

    #[test]
    fn block_rejects_every_single_bit_flip() {
        let mut rng = SplitMix64::new(0xF11B);
        let mut records = Vec::new();
        for payload in [
            rng.bytes(0),
            rng.bytes(1),
            rng.bytes(300),
            compressible(&mut rng, 300),
        ] {
            let mut record = Vec::new();
            write_block(BlockContent::Delta, Compression::Lz4, &payload, &mut record)
                .expect("fits");
            records.push(record);
        }
        assert_eq!(
            Block::decode(V1, &records[3], MAX).map(|block| block.compression()),
            Ok(Compression::Lz4)
        );
        for record in records {
            let len = record.len() - FRAME_LEN;
            for bit in 0..record.len() * 8 {
                let mut damaged = record.clone();
                damaged[bit / 8] ^= 1 << (bit % 8);
                let error = Block::decode(V1, &damaged, MAX).expect_err("damaged block");
                assert_eq!(Block::read(V1, &damaged, MAX), Err(error));

                let damaged_len = if bit < 27 { len ^ (1 << bit) } else { len };
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
                            needed: FRAME_LEN + damaged_len,
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
    fn block_rejects_every_shorter_and_longer_input() {
        let block = unhex(GOLDEN_RAW_BASE);
        for available in 0..block.len() {
            let needed = if available < FRAME_LEN { FRAME_LEN } else { 14 };
            let error = FormatError::Truncated { needed, available };
            assert_eq!(Block::decode(V1, &block[..available], MAX), Err(error));
            assert_eq!(Block::read(V1, &block[..available], MAX), Err(error));
        }
        let mut longer = block.clone();
        longer.push(0);
        assert_eq!(
            Block::decode(V1, &longer, MAX),
            Err(FormatError::InconsistentLength {
                expected: 14,
                found: 15,
            })
        );
        assert!(Block::read(V1, &longer, MAX).is_ok());
    }

    #[test]
    fn block_rejects_zero_filled_bytes() {
        for len in (FRAME_LEN..200).chain([4096, MAX]) {
            assert!(matches!(
                Block::read(V1, &vec![0; len], MAX),
                Err(FormatError::ChecksumMismatch { .. })
            ));
        }
    }

    #[test]
    fn block_length_is_bounded_before_the_payload_is_used() {
        let payload = [7; 100];
        for compression in COMPRESSIONS {
            let mut block = Vec::new();
            let tag = write_block(BlockContent::Base, compression, &payload, &mut block);
            assert_eq!(tag.expect("fits").compression(), compression);
            assert!(Block::decode(V1, &block, 100).is_ok());
            let len = match compression {
                Compression::Raw => 100,
                Compression::Lz4 => block.len() - FRAME_LEN,
            };
            assert_eq!(
                Block::decode(V1, &block, len - 1),
                Err(FormatError::RecordTooLong {
                    len: len as u64,
                    max: len as u64 - 1,
                })
            );
            if compression == Compression::Lz4 {
                assert_eq!(
                    Block::decode(V1, &block, 99),
                    Err(FormatError::RecordTooLong { len: 100, max: 99 })
                );
            }
        }

        let mut header = tag_word(1, MAX_BODY_LEN + 1).to_le_bytes().to_vec();
        header.extend_from_slice(&[0; 4]);
        assert_eq!(
            Block::read(V1, &header, usize::MAX),
            Err(FormatError::RecordTooLong {
                len: MAX_BODY_LEN as u64 + 1,
                max: MAX_BODY_LEN as u64,
            })
        );
    }

    #[test]
    fn the_longest_payload_fits_one_block_and_a_longer_one_does_not() {
        let mut payload = vec![0; MAX_BODY_LEN + 1];
        let mut out = Vec::new();
        assert_eq!(
            write_block(BlockContent::Base, Compression::Lz4, &payload, &mut out),
            Err(FormatError::RecordTooLong {
                len: MAX_BODY_LEN as u64 + 1,
                max: MAX_BODY_LEN as u64,
            })
        );
        assert!(out.is_empty());

        payload.truncate(MAX_BODY_LEN);
        for compression in COMPRESSIONS {
            out.clear();
            let tag = write_block(BlockContent::Base, compression, &payload, &mut out);
            assert_eq!(tag.expect("fits").compression(), compression);
            let block = Block::decode(V1, &out, usize::MAX).expect("valid block");
            assert_eq!(block.payload_len(), MAX_BODY_LEN);
            assert!(block.payload().expect("valid payload") == payload.as_slice());
        }
    }

    #[test]
    fn every_record_kind_code_is_a_block_or_an_error() {
        let stream = [100, 0, 0, 0, 0xF0, 0xFF, 0xFF, 0xFF];
        for code in 0..32 {
            let bytes = forge(code, &stream);
            let block = Block::decode(V1, &bytes, MAX);
            let tag = BlockTag::decode([8, 0, 0, (code << 3) as u8], V1);
            assert_eq!(block.map(|block| block.tag()), tag);
            match code {
                1 | 3 => {
                    let block = block.expect("a raw block");
                    assert_eq!(block.compression(), Compression::Raw);
                    assert_eq!(block.content(), CONTENTS[(code / 2) as usize]);
                    assert_eq!(block.payload().expect("raw"), &stream[..]);
                }
                2 | 4 => {
                    let block = block.expect("an LZ4 block");
                    assert_eq!(block.compression(), Compression::Lz4);
                    assert_eq!(block.content(), CONTENTS[(code / 2 - 1) as usize]);
                    assert_eq!(block.payload_len(), 100);
                    assert_eq!(block.payload(), Err(FormatError::Decompression));
                }
                5 | 6 => assert_eq!(block, Err(FormatError::UnexpectedRecordKind { code })),
                _ => assert_eq!(block, Err(FormatError::UnknownRecordKind { code })),
            }
        }
    }

    #[test]
    fn a_tag_decodes_only_from_the_bytes_it_encodes_to() {
        let mut rng = SplitMix64::new(0x7A6);
        let (mut accepted, mut rejected) = (0, 0);
        for round in 0..200_000 {
            let mut word = rng.next_u64() as u32;
            if round % 2 == 0 {
                word &= 0x3800_00FF;
            }
            let (code, len) = (word >> 27, (word & LEN_MASK) as usize);
            let valid = match code {
                1 | 3 => len <= MAX_BODY_LEN,
                2 | 4 => (MIN_LZ4_BODY_LEN..=MAX_BODY_LEN).contains(&len),
                _ => false,
            };
            match BlockTag::decode(word.to_le_bytes(), V1) {
                Ok(tag) => {
                    assert!(valid);
                    accepted += 1;
                    assert_eq!(tag.encode(), word.to_le_bytes());
                    assert_eq!(tag.body_len(), len);
                    assert_eq!(tag.block_len(), len + 8);
                    assert_eq!(tag.content(), CONTENTS[((code - 1) / 2) as usize]);
                    assert_eq!(tag.compression(), COMPRESSIONS[((code - 1) % 2) as usize]);
                }
                Err(_) => {
                    assert!(!valid);
                    rejected += 1;
                }
            }
        }
        assert!(accepted > 10_000 && rejected > 10_000);
    }

    #[test]
    fn footers_round_trip() {
        let mut rng = SplitMix64::new(0xF007);
        for blocks in [0, 1, 2, 3, 17, 300] {
            let built = build_segment(&mut rng, blocks);
            let (file, valid_len) = (&built.file, built.valid_len());
            let footer_len = built.footer().len();
            assert_eq!(footer_len, 8 + 8 * blocks + 20);

            let trailer_at = file.len() - SegmentTrailer::ENCODED_LEN;
            let trailer = SegmentTrailer::decode(V1, &file[trailer_at..], valid_len);
            let trailer = trailer.expect("valid trailer");
            assert_eq!(trailer.index_offset(), built.index_offset);
            assert_eq!(trailer.block_count(), blocks);
            assert_eq!(trailer.index_len(), footer_len - 20);
            let index_at = trailer.index_offset() as usize;
            let index_bytes = &file[index_at..index_at + trailer.index_len()];
            let index = SegmentIndex::decode(V1, &trailer, index_bytes).expect("valid index");
            assert_eq!(index.len(), blocks);
            assert_eq!(index.is_empty(), blocks == 0);

            for tail_len in [footer_len, footer_len + 1, file.len()] {
                let tail = &file[file.len() - tail_len.min(file.len())..];
                assert_eq!(SegmentIndex::decode_tail(V1, tail, valid_len), Ok(index));
            }

            let listed: Vec<IndexedBlock> = index.blocks().collect();
            assert_eq!(listed.len(), blocks);
            let mut rest = &file[DATA_START as usize..index_at];
            for ((listed, entry), payload) in listed.iter().zip(&built.entries).zip(&built.payloads)
            {
                assert_eq!((listed.page, listed.tag), (entry.page, entry.tag));
                assert_eq!(listed.offset, (index_at - rest.len()) as u64);
                let (walked, after) = Block::read(V1, rest, MAX).expect("valid block");
                rest = after;

                let at = listed.offset as usize;
                let pointed = Block::decode(V1, &file[at..at + listed.tag.block_len()], MAX);
                assert_eq!(pointed, Ok(walked));
                assert_eq!(walked.tag(), listed.tag);
                assert_eq!(walked.payload().expect("valid payload"), payload.as_slice());
            }
            assert!(rest.is_empty());

            let entries: Vec<IndexEntry> = listed
                .iter()
                .map(|block| IndexEntry {
                    page: block.page,
                    tag: block.tag,
                })
                .collect();
            let mut again = Vec::new();
            write_footer(&entries, trailer.index_offset(), &mut again).expect("consistent");
            assert_eq!(again, built.footer());
        }
    }

    #[test]
    fn a_tail_too_short_for_the_footer_says_how_much_is_needed() {
        let mut rng = SplitMix64::new(0x7A11);
        let built = build_segment(&mut rng, 5);
        let footer_len = built.footer().len();
        assert_eq!(footer_len, 68);
        for available in 0..footer_len {
            let tail = &built.file[built.file.len() - available..];
            let needed = if available < 20 { 20 } else { footer_len };
            assert_eq!(
                SegmentIndex::decode_tail(V1, tail, built.valid_len()),
                Err(FormatError::Truncated { needed, available })
            );
        }
    }

    #[test]
    fn write_footer_checks_where_the_blocks_end() {
        let mut rng = SplitMix64::new(0x0FF5);
        let built = build_segment(&mut rng, 4);
        for wrong in [
            0,
            DATA_START,
            built.index_offset - 1,
            built.index_offset + 1,
        ] {
            let mut out = Vec::new();
            assert_eq!(
                write_footer(&built.entries, wrong, &mut out),
                Err(FormatError::InconsistentLength {
                    expected: built.index_offset,
                    found: wrong,
                })
            );
            assert!(out.is_empty());
        }
    }

    #[test]
    fn footer_rejects_every_single_bit_flip() {
        let mut rng = SplitMix64::new(0xF1F0);
        for blocks in [0, 1, 6] {
            let built = build_segment(&mut rng, blocks);
            let footer_at = built.index_offset as usize;
            for bit in footer_at * 8..built.file.len() * 8 {
                let mut damaged = built.file.clone();
                damaged[bit / 8] ^= 1 << (bit % 8);
                assert!(
                    SegmentIndex::decode_tail(V1, &damaged, built.valid_len()).is_err(),
                    "bit {bit}"
                );
            }
            for cut in 1..built.footer().len() {
                let shorter = &built.file[..built.file.len() - cut];
                assert!(SegmentIndex::decode_tail(V1, shorter, shorter.len() as u64).is_err());
                assert!(SegmentIndex::decode_tail(V1, shorter, built.valid_len()).is_err());
            }
        }
    }

    #[test]
    fn a_forged_footer_is_rejected() {
        let base = tag_word(1, 6);
        let delta = tag_word(3, 0);
        let good = [(7, base), (9, delta)];
        fn decode(footer: &[u8], valid_len: u64) -> Result<SegmentIndex<'_>, FormatError> {
            SegmentIndex::decode_tail(V1, footer, valid_len)
        }
        let inconsistent =
            |expected, found| Err(FormatError::InconsistentLength { expected, found });

        let footer = forge_footer(&good, 5, 6, 42, 2);
        assert_eq!(footer.len(), 44);
        let index = decode(&footer, 86).expect("valid footer");
        assert_eq!(
            index
                .blocks()
                .map(|block| (block.page.get(), block.offset))
                .collect::<Vec<_>>(),
            [(7, 20), (9, 34)]
        );

        assert_eq!(decode(&footer, 85), inconsistent(41, 42));
        assert_eq!(decode(&footer, 87), inconsistent(43, 42));
        assert_eq!(decode(&footer, 63), inconsistent(64, 63));
        assert_eq!(decode(&footer, 0), inconsistent(64, 0));
        assert_eq!(decode(&footer, u64::MAX), inconsistent(u64::MAX - 44, 42));

        assert_eq!(
            decode(&forge_footer(&good, 5, 6, 43, 2), 87),
            inconsistent(42, 43)
        );
        assert_eq!(
            decode(&forge_footer(&good, 5, 6, 41, 2), 85),
            inconsistent(42, 41)
        );
        assert_eq!(
            decode(&forge_footer(&[], 5, 6, 21, 0), 49),
            inconsistent(20, 21)
        );
        assert_eq!(
            decode(&forge_footer(&[], 5, 6, 19, 0), 47),
            inconsistent(48, 47)
        );

        assert_eq!(
            decode(&forge_footer(&good, 5, 6, 42, 3), 94),
            Err(FormatError::Truncated {
                needed: 52,
                available: 44,
            })
        );
        let three = forge_footer(&good, 5, 6, 42, 3);
        let trailer = SegmentTrailer::decode(V1, &three[24..], 94).expect("valid trailer");
        assert_eq!(trailer.block_count(), 3);
        assert_eq!(
            SegmentIndex::decode(V1, &trailer, &three[..24]),
            inconsistent(24, 16)
        );
        let one = forge_footer(&good, 5, 6, 42, 1);
        let trailer = SegmentTrailer::decode(V1, &one[24..], 78).expect("valid trailer");
        assert_eq!(
            SegmentIndex::decode(V1, &trailer, &one[..24]),
            Err(FormatError::RecordTooLong { len: 16, max: 8 })
        );
        let ragged = forge(5, &[0; 12]);
        let trailer = SegmentTrailer::decode(V1, &one[24..], 78).expect("valid trailer");
        assert_eq!(
            SegmentIndex::decode(V1, &trailer, &ragged),
            Err(FormatError::RecordTooLong { len: 12, max: 8 })
        );
        let trailer = SegmentTrailer::decode(V1, &footer[24..], 86).expect("valid trailer");
        assert_eq!(
            SegmentIndex::decode(V1, &trailer, &ragged),
            inconsistent(16, 12)
        );

        let entry_errors = [
            ((0, base), FormatError::MissingId),
            (
                (7, tag_word(5, 6)),
                FormatError::UnexpectedRecordKind { code: 5 },
            ),
            (
                (7, tag_word(6, 6)),
                FormatError::UnexpectedRecordKind { code: 6 },
            ),
            (
                (7, tag_word(0, 6)),
                FormatError::UnknownRecordKind { code: 0 },
            ),
            (
                (7, tag_word(9, 6)),
                FormatError::UnknownRecordKind { code: 9 },
            ),
            (
                (7, tag_word(2, 4)),
                FormatError::Truncated {
                    needed: 13,
                    available: 12,
                },
            ),
            (
                (7, tag_word(1, MAX_BODY_LEN + 1)),
                FormatError::RecordTooLong {
                    len: MAX_BODY_LEN as u64 + 1,
                    max: MAX_BODY_LEN as u64,
                },
            ),
        ];
        for (entry, error) in entry_errors {
            assert_eq!(
                decode(&forge_footer(&[entry, good[1]], 5, 6, 42, 2), 86),
                Err(error)
            );
        }
        assert_eq!(
            decode(
                &forge_footer(&[(7, tag_word(1, 7)), good[1]], 5, 6, 42, 2),
                86
            ),
            inconsistent(43, 42)
        );
        assert!(decode(
            &forge_footer(&[(7, tag_word(2, 6)), good[1]], 5, 6, 42, 2),
            86
        )
        .is_ok());

        for (index_kind, trailer_kind, error) in [
            (6, 6, FormatError::UnexpectedRecordKind { code: 6 }),
            (1, 6, FormatError::UnexpectedRecordKind { code: 1 }),
            (0, 6, FormatError::UnknownRecordKind { code: 0 }),
            (7, 6, FormatError::UnknownRecordKind { code: 7 }),
            (5, 5, FormatError::UnexpectedRecordKind { code: 5 }),
            (5, 3, FormatError::UnexpectedRecordKind { code: 3 }),
            (5, 0, FormatError::UnknownRecordKind { code: 0 }),
            (5, 31, FormatError::UnknownRecordKind { code: 31 }),
        ] {
            let footer = forge_footer(&good, index_kind, trailer_kind, 42, 2);
            assert_eq!(decode(&footer, 86), Err(error));
        }
    }

    #[test]
    fn a_forged_trailer_is_rejected() {
        let decode = |bytes: &[u8], valid_len| SegmentTrailer::decode(V1, bytes, valid_len);
        let good = forge(6, &[&20u64.to_le_bytes()[..], &0u32.to_le_bytes()].concat());
        assert_eq!(
            decode(&good, 48).map(|trailer| trailer.index_offset()),
            Ok(20)
        );

        for available in 0..good.len() {
            let needed = if available < 8 { 8 } else { 20 };
            assert_eq!(
                decode(&good[..available], 48),
                Err(FormatError::Truncated { needed, available })
            );
        }
        let mut longer = good.clone();
        longer.push(0);
        assert_eq!(
            decode(&longer, 48),
            Err(FormatError::InconsistentLength {
                expected: 20,
                found: 21,
            })
        );
        assert_eq!(
            decode(&forge(6, &[0; 8]), 48),
            Err(FormatError::InconsistentLength {
                expected: 12,
                found: 8,
            })
        );
        assert_eq!(
            decode(&forge(6, &[0; 13]), 48),
            Err(FormatError::RecordTooLong { len: 13, max: 12 })
        );

        let too_many = MAX_BLOCKS as u32 + 1;
        let body = [&20u64.to_le_bytes()[..], &too_many.to_le_bytes()].concat();
        assert_eq!(
            decode(&forge(6, &body), u64::MAX),
            Err(FormatError::RecordTooLong {
                len: 1 << 27,
                max: MAX_BODY_LEN as u64,
            })
        );
        let body = [&20u64.to_le_bytes()[..], &u32::MAX.to_le_bytes()].concat();
        assert!(matches!(
            decode(&forge(6, &body), u64::MAX),
            Err(FormatError::RecordTooLong { .. })
        ));
        assert!(matches!(
            decode(&[0; 20], 48),
            Err(FormatError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn the_largest_index_fits_one_record_and_a_larger_one_does_not() {
        let empty = BlockTag {
            content: BlockContent::Delta,
            compression: Compression::Raw,
            body_len: 0,
        };
        let entry = IndexEntry {
            page: page(1),
            tag: empty,
        };
        let mut entries = Vec::with_capacity(MAX_BLOCKS + 1);
        entries.resize(MAX_BLOCKS, entry);
        let index_offset = DATA_START + (MAX_BLOCKS * FRAME_LEN) as u64;

        let mut footer = Vec::new();
        write_footer(&entries, index_offset, &mut footer).expect("fits");
        assert_eq!(footer.len(), FRAME_LEN + MAX_BODY_LEN - 4 + 20);
        let valid_len = index_offset + footer.len() as u64;
        let index = SegmentIndex::decode_tail(V1, &footer, valid_len).expect("valid footer");
        assert_eq!(index.len(), MAX_BLOCKS);
        assert_eq!(
            index.blocks().last().map(|block| block.offset),
            Some(index_offset - 8)
        );

        entries.push(entry);
        footer.clear();
        assert_eq!(
            write_footer(&entries, index_offset + 8, &mut footer),
            Err(FormatError::RecordTooLong {
                len: 1 << 27,
                max: MAX_BODY_LEN as u64,
            })
        );
        assert!(footer.is_empty());
    }

    #[test]
    fn the_file_header_must_be_a_segment_with_the_id_of_its_name() {
        let id = FileId::new(310).expect("non-zero");
        let other = FileId::new(311).expect("non-zero");
        let header = |kind| FileHeader::new(kind, V1, 310).expect("valid").encode();

        let mut file = header(FileKind::Segment).to_vec();
        file.extend_from_slice(b"blocks follow");
        assert_eq!(decode_file_header(&file, id), Ok(V1));
        assert_eq!(
            decode_file_header(&file, other),
            Err(FormatError::WrongFileSequence {
                expected: 311,
                found: 310,
            })
        );
        for kind in [FileKind::Wal, FileKind::ValueLog, FileKind::PageTable] {
            assert_eq!(
                decode_file_header(&header(kind), id),
                Err(FormatError::WrongFileKind {
                    expected: FileKind::Segment,
                    found: kind,
                })
            );
        }
        assert_eq!(
            decode_file_header(&file[..19], id),
            Err(FormatError::Truncated {
                needed: 20,
                available: 19,
            })
        );
        assert_eq!(decode_file_header(&[0; 20], id), Err(FormatError::BadMagic));
    }

    #[test]
    fn a_record_that_decodes_is_the_one_its_value_encodes_to() {
        let mut rng = SplitMix64::new(0xA2B);
        let (mut blocks, mut trailers, mut indexes) = (0, 0, 0);
        for _ in 0..60_000 {
            let kind = (rng.next_u64() % 8) as u32;
            let len = (rng.next_u64() % 40) as usize;
            let body = match rng.next_u64() % 3 {
                0 => rng.bytes(len),
                1 => {
                    let count = rng.next_u64() % 4;
                    let entries = (0..count).flat_map(|_| {
                        let page = (rng.next_u64() % 6) as u32;
                        let tag =
                            tag_word((rng.next_u64() % 6) as u32, (rng.next_u64() % 9) as usize);
                        [page.to_le_bytes(), tag.to_le_bytes()].concat()
                    });
                    entries.collect()
                }
                _ => {
                    let offset = 20 + rng.next_u64() % 4;
                    let count = (rng.next_u64() % 3) as u32;
                    [&offset.to_le_bytes()[..], &count.to_le_bytes()].concat()
                }
            };
            let bytes = forge(kind, &body);

            if let Ok(block) = Block::decode(V1, &bytes, MAX) {
                blocks += 1;
                assert_eq!(Block::read(V1, &bytes, MAX), Ok((block, &[][..])));
                if let Ok(payload) = block.payload() {
                    assert_eq!(payload.len(), block.payload_len());
                    if block.compression() == Compression::Raw {
                        let mut again = Vec::new();
                        write_block(block.content(), Compression::Raw, &payload, &mut again)
                            .expect("fits");
                        assert_eq!(again, bytes);
                    }
                }
            }

            let valid_len = 40 + rng.next_u64() % 30;
            if let Ok(trailer) = SegmentTrailer::decode(V1, &bytes, valid_len) {
                trailers += 1;
                assert_eq!(kind, 6);
                assert_eq!(
                    trailer.index_offset() + (trailer.index_len() + 20) as u64,
                    valid_len
                );
            }

            let blocks_end = body.as_chunks::<8>().0.iter().fold(20, |end, entry| {
                end + 8 + u64::from(u32::from_le_bytes([entry[4], entry[5], entry[6], 0]))
            });
            let offset = blocks_end + u64::from(rng.next_u64().is_multiple_of(4));
            let count = (body.len() / 8) as u32;
            let trailer = [&offset.to_le_bytes()[..], &count.to_le_bytes()].concat();
            let footer = [bytes.clone(), forge(6, &trailer)].concat();
            let valid_len = offset + footer.len() as u64;
            if let Ok(index) = SegmentIndex::decode_tail(V1, &footer, valid_len) {
                indexes += 1;
                let entries: Vec<IndexEntry> = index
                    .blocks()
                    .map(|block| IndexEntry {
                        page: block.page,
                        tag: block.tag,
                    })
                    .collect();
                assert_eq!(entries.len(), index.len());
                let mut again = Vec::new();
                write_footer(&entries, offset, &mut again).expect("consistent");
                assert_eq!(again, footer);
            }
        }
        assert!(
            blocks > 5_000 && trailers > 20 && indexes > 500,
            "{blocks} {trailers} {indexes}"
        );
    }

    #[test]
    fn decoders_return_an_error_on_arbitrary_bytes() {
        let mut rng = SplitMix64::new(0xFA22);
        let built = build_segment(&mut rng, 12);
        let id = FileId::new(310).expect("non-zero");
        for round in 0..40_000 {
            let bytes = if round % 2 == 0 {
                let len = (rng.next_u64() % 80) as usize;
                rng.bytes(len)
            } else {
                let mut bytes = built.file.clone();
                for _ in 0..=rng.next_u64() % 3 {
                    let at = (rng.next_u64() as usize) % bytes.len();
                    bytes[at] = rng.next_u64() as u8;
                }
                let from = (rng.next_u64() as usize) % bytes.len();
                bytes.split_off(from)
            };
            let valid_len = rng.next_u64() % 2_000;
            let _ = decode_file_header(&bytes, id);
            let _ = Block::decode(V1, &bytes, MAX).map(|block| block.payload());
            let _ = Block::read(V1, &bytes, MAX).map(|(block, _)| block.payload());
            let _ = SegmentTrailer::decode(V1, &bytes, valid_len);
            let _ = SegmentIndex::decode_tail(V1, &bytes, valid_len)
                .map(|index| index.blocks().count());
            let _ = SegmentIndex::decode_tail(V1, &bytes, bytes.len() as u64);
        }
    }
}
