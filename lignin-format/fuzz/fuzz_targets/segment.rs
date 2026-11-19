//! Segment files: blocks, the footer index and the trailer
//! (`segment-manifest.md` section 2).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    segment::{
        self, Block, BlockTag, Compression, IndexEntry, SegmentIndex, SegmentTrailer, DATA_START,
        MAX_BODY_LEN,
    },
    FileId, FormatVersion,
};
use lignin_format_fuzz::{header_file_id, ok, page_size, sealed_file, segment_frame_len, Input};

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let file = sealed_file(&input, segment_frame_len);
    let max_payload_len = match input.control >> 1 & 7 {
        7 => MAX_BODY_LEN,
        6 => usize::MAX,
        choice => page_size(choice).get(),
    };
    tags(&file);

    let id = header_file_id(&file);
    let id = match input.bit(4) {
        true => FileId::new(id.get() % 0xFFFF_FFFD + 1).expect("a file id"),
        false => id,
    };
    let version = ok(segment::decode_file_header(&file, id)).unwrap_or(FormatVersion::V1);
    walk(&file, version, max_payload_len);
    footer(&file, version, max_payload_len, input.control);
});

fn tags(file: &[u8]) {
    for word in file.chunks_exact(BlockTag::ENCODED_LEN).take(256) {
        let word: [u8; 4] = word.try_into().unwrap();
        if let Some(tag) = ok(BlockTag::decode(word, FormatVersion::V1)) {
            assert_eq!(tag.encode(), word);
            assert!(tag.body_len() <= MAX_BODY_LEN);
            assert_eq!(tag.block_len(), segment::FRAME_LEN + tag.body_len());
            let _ = (tag.content(), tag.compression());
        }
    }
}

/// Reads the blocks from the first one on, as a reader of an open segment
/// does.
fn walk(file: &[u8], version: FormatVersion, max_payload_len: usize) {
    let mut rest = file.get(DATA_START as usize..).unwrap_or_default();
    while !rest.is_empty() {
        let Some((block, after)) = ok(Block::read(version, rest, max_payload_len)) else {
            return;
        };
        let len = rest.len() - after.len();
        assert_eq!(len, block.tag().block_len());
        let alone = ok(Block::decode(version, &rest[..len], max_payload_len));
        assert_eq!(alone, Some(block));
        check_block(&block, &rest[..len], max_payload_len);
        rest = after;
    }
}

fn check_block(block: &Block<'_>, bytes: &[u8], max_payload_len: usize) {
    assert!(block.payload_len() <= max_payload_len.min(MAX_BODY_LEN));
    assert_eq!(block.content(), block.tag().content());
    assert_eq!(block.compression(), block.tag().compression());
    let payload = ok(block.payload());
    let mut copied = vec![0; block.payload_len()];
    let copy = ok(block.copy_payload_to(&mut copied));
    assert_eq!(payload.is_some(), copy.is_some());
    let _ = ok(block.copy_payload_to(&mut copied[..block.payload_len().saturating_sub(1)]));
    let Some(payload) = payload else {
        return;
    };
    assert_eq!(*payload, copied);
    assert_eq!(payload.len(), block.payload_len());

    let mut written = Vec::new();
    let tag = segment::write_block(block.content(), block.compression(), &payload, &mut written)
        .expect("a payload that was read");
    assert!(tag.block_len() <= payload.len() + segment::FRAME_LEN);
    if block.compression() == Compression::Raw {
        assert_eq!(written, bytes);
    }
    let back = Block::decode(FormatVersion::V1, &written, payload.len()).expect("a written block");
    assert_eq!(*back.payload().expect("a written block"), *payload);
}

/// Reads the footer from the end of the valid part, as the manifest states
/// its length. Bit 7 of `control` states one that is 8 bytes too long, and
/// bits 5 and 6 pick where the bytes handed to the reader start.
fn footer(file: &[u8], version: FormatVersion, max_payload_len: usize, control: u8) {
    let stated = file.len() as u64 + u64::from(control >> 7) * 8;
    let tail_at = match control >> 5 & 3 {
        0 => 0,
        1 => file.len().saturating_sub(SegmentTrailer::ENCODED_LEN),
        2 => (DATA_START as usize).min(file.len()),
        _ => file.len() / 2,
    };
    let trailer_at = file.len().checked_sub(SegmentTrailer::ENCODED_LEN);
    let trailer =
        trailer_at.and_then(|at| ok(SegmentTrailer::decode(version, &file[at..], stated)));
    let tail = ok(SegmentIndex::decode_tail(version, &file[tail_at..], stated));
    if let (Some(trailer), Some(at)) = (trailer, trailer_at) {
        assert!(trailer.block_count() <= segment::MAX_BLOCKS);
        let index_at = usize::try_from(trailer.index_offset()).unwrap_or(usize::MAX);
        let direct = file
            .get(index_at..at)
            .and_then(|bytes| ok(SegmentIndex::decode(version, &trailer, bytes)));
        if stated == file.len() as u64 {
            assert_eq!(tail.is_some(), tail_at <= index_at && direct.is_some());
            if let Some(tail) = tail {
                assert_eq!(direct, Some(tail));
            }
        }
    }
    let Some(index) = tail.filter(|_| stated == file.len() as u64) else {
        return;
    };
    assert_eq!(index.is_empty(), index.len() == 0);
    let listed: Vec<_> = index.blocks().collect();
    assert_eq!(listed.len(), index.len());
    let entries: Vec<IndexEntry> = listed
        .iter()
        .map(|block| IndexEntry {
            page: block.page,
            tag: block.tag,
        })
        .collect();
    let index_offset = listed.last().map_or(DATA_START, |block| {
        block.offset + block.tag.block_len() as u64
    });
    let mut written = Vec::new();
    segment::write_footer(&entries, index_offset, &mut written).expect("a footer that was read");
    assert_eq!(written, file[index_offset as usize..]);

    for listed in &listed {
        let start = listed.offset as usize;
        let bytes = &file[start..start + listed.tag.block_len()];
        if let Some(block) = ok(Block::decode(version, bytes, max_payload_len)) {
            assert_eq!(block.tag(), listed.tag);
        }
    }
}
