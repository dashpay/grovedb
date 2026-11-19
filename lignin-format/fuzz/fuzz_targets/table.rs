//! Page-table checkpoints (`page-table.md` section 3): `table::decode`, and
//! `Decoder::{read, record}` record by record and on payloads without their
//! frame.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    table::{self, Checkpoint, Decoder, Record, LINKS_PER_RECORD, LINK_LEN, MAX_PAYLOAD_LEN},
    FileHeader, FileId, FormatVersion,
};
use lignin_format_fuzz::{default_frame_len, ok, sealed_file, take_u16, take_up_to, Input};

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    match input.control >> 1 & 3 {
        0 | 1 => file(&input, sealed_file(&input, default_frame_len)),
        2 => payloads(input.bytes),
        _ => {
            let expanded = expand(input.bytes);
            file(&input, expanded);
        }
    }
});

fn sequence(file: &[u8]) -> FileId {
    file.get(12..16)
        .and_then(|sequence| FileId::new(u32::from_le_bytes(sequence.try_into().unwrap())))
        .unwrap_or(FileId::MIN)
}

fn file(input: &Input<'_>, file: Vec<u8>) {
    let id = if input.bit(3) {
        FileId::MIN
    } else {
        sequence(&file)
    };
    let decoded = ok(table::decode(&file, id));
    let stepped = step(&file, id);
    assert_eq!(decoded.is_some(), stepped.is_some());
    let Some(checkpoint) = decoded else {
        return;
    };
    assert_eq!(stepped.as_ref(), Some(&checkpoint));
    let encoded = table::encode(
        id,
        checkpoint.end.covers,
        checkpoint.links.iter().copied(),
        checkpoint.end.place,
        checkpoint.value_counts.iter().copied(),
    );
    assert_eq!(ok(encoded).as_deref(), Some(file.as_slice()));
    for link in &checkpoint.links {
        if let Some(block) = link.kind.block() {
            assert!(block.offset() <= block.end());
            let _ = (block.file(), block.len());
        }
    }
}

/// Reads the file record by record, as a reader that streams it does.
fn step(file: &[u8], id: FileId) -> Option<Checkpoint> {
    let version = ok(table::decode_file_header(file, id))?;
    let mut decoder = Decoder::new(version);
    let mut links = Vec::new();
    let mut value_counts = Vec::new();
    let mut rest = &file[FileHeader::ENCODED_LEN..];
    loop {
        assert!(!decoder.is_finished());
        let (record, after) = ok(decoder.read(rest))?;
        rest = after;
        match record {
            Record::Links(record) => {
                let len = record.len();
                links.extend(record);
                assert!(len <= LINKS_PER_RECORD);
            }
            Record::ValueCounts(record) => value_counts.extend(record),
            Record::End(end) => {
                assert!(decoder.is_finished());
                return rest.is_empty().then_some(Checkpoint {
                    version,
                    end,
                    links,
                    value_counts,
                });
            }
        }
    }
}

/// Hands payloads to the decoder without their frame. Each is a `u16`
/// length, with the top bit asking for the length times 64, and that many
/// bytes.
fn payloads(mut bytes: &[u8]) {
    let mut decoder = Decoder::new(FormatVersion::V1);
    while let Some(len) = take_u16(&mut bytes) {
        let len = if len & 0x8000 != 0 {
            usize::from(len & 0x7FFF) * 64
        } else {
            usize::from(len)
        };
        let payload = take_up_to(&mut bytes, len);
        let payload = repeat_to(payload, len);
        match ok(decoder.record(&payload)) {
            Some(Record::Links(links)) => {
                assert!(payload.len() <= MAX_PAYLOAD_LEN);
                assert!(links.len() <= LINKS_PER_RECORD);
                links.for_each(drop);
            }
            Some(Record::ValueCounts(counts)) => counts.for_each(drop),
            Some(Record::End(_)) => assert!(decoder.is_finished()),
            None => {}
        }
    }
}

fn repeat_to(bytes: &[u8], len: usize) -> Vec<u8> {
    if bytes.is_empty() {
        return vec![0; len];
    }
    bytes.iter().copied().cycle().take(len).collect()
}

/// Builds a large checkpoint from a few bytes, then changes some of its
/// bytes and fixes its checksums, so that records of 4096 links and counts
/// and the boundaries around them are reached.
///
/// The input is: the number of links (`u16`) and of counts (`u16`), the
/// step between page ids and between file ids, then a list of changes, each
/// a `u32` offset and a byte to put there.
fn expand(mut bytes: &[u8]) -> Vec<u8> {
    use lignin_format::{
        table::{BlockPointer, Link, LinkKind, ValueCount},
        CommitVersion, PageId,
    };

    let links = take_u16(&mut bytes).unwrap_or(0) as usize % (2 * LINKS_PER_RECORD + 2);
    let counts = take_u16(&mut bytes).unwrap_or(0) as usize % (2 * LINKS_PER_RECORD + 2);
    let page_step = take_u16(&mut bytes).unwrap_or(1).max(1) as u32;
    let file_step = take_u16(&mut bytes).unwrap_or(1).max(1) as u32;
    let id = FileId::new(7).unwrap();
    let block = BlockPointer::new(FileId::new(3).unwrap(), 20, 64).unwrap();
    let link = |index: usize| {
        let page = PageId::new(1 + (index as u32 / 2) * page_step)?;
        let kind = match index % 6 {
            5 => LinkKind::Free,
            1 | 3 => LinkKind::Delta(block),
            _ => LinkKind::Base(block),
        };
        Some(Link {
            page,
            version: CommitVersion::new(index as u64).unwrap(),
            kind,
        })
    };
    let count = |index: usize| {
        Some(ValueCount {
            file: FileId::new(1 + index as u32 * file_step)?,
            values: index as u64,
            bytes: index as u64 * 100,
            miscounted: index % 3 == 0,
        })
    };
    let covers = CommitVersion::new(links as u64).unwrap();
    let encoded = table::encode(
        id,
        covers,
        (0..links).map_while(link),
        None,
        (0..counts).map_while(count),
    );
    let Some(mut file) = ok(encoded) else {
        return FileHeader::new(lignin_format::FileKind::PageTable, FormatVersion::V1, 7)
            .unwrap()
            .encode()
            .to_vec();
    };
    let mut changed = false;
    while let (Some(low), Some(high), Some(&byte)) =
        (take_u16(&mut bytes), take_u16(&mut bytes), bytes.first())
    {
        bytes = &bytes[1..];
        let at = (usize::from(high) << 16 | usize::from(low)) % (file.len() + LINK_LEN);
        match file.get_mut(at) {
            Some(slot) => *slot = byte,
            None => file.truncate(at % file.len().max(1)),
        }
        changed = true;
    }
    if changed {
        lignin_format_fuzz::seal_file_header(&mut file);
        lignin_format_fuzz::seal_frames(&mut file, FileHeader::ENCODED_LEN, default_frame_len);
    }
    file
}
