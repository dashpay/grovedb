//! `lignin segments DIR [--blocks]`: each segment the manifest names,
//! block by block.
//!
//! Like `manifest`, it reads files only, each up to the valid length the
//! manifest gives it, and so reads a checkpoint whose segments the live
//! database goes on appending to. Every block's checksum is verified. A
//! sealed segment is read by its footer index, which names each block's
//! page; a segment that is being filled, or was closed unsealed, is walked
//! block by block from its header, and each block's page is read from its
//! payload.

use std::{collections::BTreeSet, fs::File, io::Read, io::Write, path::Path};

use lignin_format::{
    delta::DeltaRun,
    manifest::{FileState, LiveFile},
    segment::{self, Block, BlockContent, Compression, SegmentIndex, MAX_BODY_LEN},
    FileKind, FormatError, FormatVersion, PageId,
};

use crate::{
    manifest::{measured, read, relative, state},
    Outcome, ToolError,
};

/// What one segment holds.
#[derive(Debug, Default)]
struct Totals {
    blocks: u64,
    images: u64,
    runs: u64,
    compressed: u64,
    stored_bytes: u64,
    raw_bytes: u64,
    pages: BTreeSet<PageId>,
}

/// One block as the listing shows it.
struct Listed {
    offset: u64,
    page: Option<PageId>,
    content: BlockContent,
    compression: Compression,
    stored: usize,
    raw: usize,
}

/// The page a block's payload is of: a page image and a run of deltas
/// both name it.
fn page_of(version: FormatVersion, block: &Block<'_>) -> Result<PageId, FormatError> {
    let payload = block.payload()?;
    match block.content() {
        BlockContent::Delta => Ok(DeltaRun::decode(&payload, version)?.page()),
        BlockContent::Base => {
            // Both kinds of page keep their id at byte 4 of the image.
            let id = payload
                .get(4..8)
                .and_then(|id| <[u8; 4]>::try_from(id).ok());
            id.and_then(PageId::decode).ok_or(FormatError::MissingId)
        }
    }
}

/// Reads the blocks of the segment `bytes`, the first `file.valid_len()`
/// bytes of the file, and calls `each` with every one.
fn blocks(
    file: &LiveFile,
    bytes: &[u8],
    each: &mut dyn FnMut(Listed),
) -> Result<(), (u64, FormatError)> {
    let version = segment::decode_file_header(bytes, file.id()).map_err(|error| (0, error))?;
    let listed = |offset: u64, block: &Block<'_>, page| Listed {
        offset,
        page,
        content: block.content(),
        compression: block.compression(),
        stored: block.tag().block_len(),
        raw: block.payload_len(),
    };
    if file.state() == FileState::Sealed {
        let index = SegmentIndex::decode_tail(version, bytes, bytes.len() as u64);
        let index = index.map_err(|error| (bytes.len() as u64, error))?;
        for indexed in index.blocks() {
            let at = |error| (indexed.offset, error);
            let start = usize::try_from(indexed.offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(indexed.tag.block_len());
            let block_bytes = bytes.get(start..end).ok_or(at(FormatError::Truncated {
                needed: end,
                available: bytes.len(),
            }))?;
            let block = Block::decode(version, block_bytes, MAX_BODY_LEN).map_err(at)?;
            each(listed(indexed.offset, &block, Some(indexed.page)));
        }
        return Ok(());
    }
    let mut rest = &bytes[segment::DATA_START as usize..];
    while !rest.is_empty() {
        let offset = (bytes.len() - rest.len()) as u64;
        let at = |error| (offset, error);
        let (block, after) = Block::read(version, rest, MAX_BODY_LEN).map_err(at)?;
        let page = page_of(version, &block).map_err(at)?;
        each(listed(offset, &block, Some(page)));
        rest = after;
    }
    Ok(())
}

/// Writes each segment of `dir`, and with `all_blocks` each of its blocks,
/// to `out`.
pub fn run(dir: &Path, all_blocks: bool, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let manifest = read(dir)?;
    let segments = manifest.files().iter();
    let segments: Vec<&LiveFile> = segments
        .filter(|file| file.kind() == FileKind::Segment)
        .collect();
    writeln!(out, "segments: {}", segments.len())?;
    let mut first_damage = None;
    for file in segments {
        let (path, len) = measured(dir, file);
        let name = relative(dir, &path);
        if len.is_none_or(|len| len < file.valid_len()) {
            writeln!(
                out,
                "{name}: missing or shorter than its {} valid bytes",
                file.valid_len()
            )?;
            first_damage.get_or_insert(ToolError::Format {
                path,
                offset: len.unwrap_or(0),
                source: FormatError::Truncated {
                    needed: file.valid_len() as usize,
                    available: len.unwrap_or(0) as usize,
                },
            });
            continue;
        }
        let mut bytes = Vec::new();
        let valid = File::open(&path)?
            .take(file.valid_len())
            .read_to_end(&mut bytes);
        valid?;
        // The file can have shrunk since its length was looked at.
        if (bytes.len() as u64) < file.valid_len() {
            writeln!(
                out,
                "{name}: {} bytes read of its {} valid bytes",
                bytes.len(),
                file.valid_len()
            )?;
            first_damage.get_or_insert(ToolError::Format {
                path,
                offset: bytes.len() as u64,
                source: FormatError::Truncated {
                    needed: file.valid_len() as usize,
                    available: bytes.len(),
                },
            });
            continue;
        }
        let mut totals = Totals::default();
        let mut listing = Vec::new();
        let read = blocks(file, &bytes, &mut |block| {
            totals.blocks += 1;
            match block.content {
                BlockContent::Base => totals.images += 1,
                BlockContent::Delta => totals.runs += 1,
            }
            totals.compressed += u64::from(block.compression == Compression::Lz4);
            totals.stored_bytes += block.stored as u64;
            totals.raw_bytes += block.raw as u64;
            totals.pages.extend(block.page);
            if all_blocks {
                listing.push(block);
            }
        });
        writeln!(
            out,
            "{name} {} {} bytes: {} blocks ({} page images, {} delta runs; {} LZ4), {} bytes \
             stored, {} raw, {} pages",
            state(file.state()),
            file.valid_len(),
            totals.blocks,
            totals.images,
            totals.runs,
            totals.compressed,
            totals.stored_bytes,
            totals.raw_bytes,
            totals.pages.len()
        )?;
        for block in listing {
            writeln!(
                out,
                "  at {} page {} {} {} {} bytes ({} raw)",
                block.offset,
                block.page.map_or(0, PageId::get),
                match block.content {
                    BlockContent::Base => "image",
                    BlockContent::Delta => "deltas",
                },
                match block.compression {
                    Compression::Raw => "raw",
                    Compression::Lz4 => "lz4",
                },
                block.stored,
                block.raw
            )?;
        }
        if let Err((offset, source)) = read {
            writeln!(out, "  DAMAGED at byte {offset}: {source}")?;
            first_damage.get_or_insert(ToolError::Format {
                path,
                offset,
                source,
            });
        }
    }
    match first_damage {
        Some(error) => Err(error),
        None => Ok(Outcome::Done),
    }
}
