use super::*;
use crate::{checksum::Checksum, test_util::SplitMix64};

const V1: FormatVersion = FormatVersion::V1;
const HEADER_LEN: usize = FileHeader::ENCODED_LEN;
const FRAME: usize = RecordHeader::ENCODED_LEN;
const NO_VERSION: u64 = u64::MAX;

const GOLDEN_EMPTY: &str = "4c49474e494e4442050001000100000009a97190\
    2d0000009c7289fb\
    020000000000000000ffffffffffffffff0000000000000000\
    0000000000000000000000000000000000000000";
const GOLDEN_SMALL: &str = "4c49474e494e4442050001007a0000000ba8756f\
    550000004594b567\
    01\
    07000000081000103601000014000000000000002800000000000000\
    0700000038000020360100001c100000000000002900000000000000\
    09000000000000300000000000000000000000002600000000000000\
    310000008dbbb428\
    03\
    050000000000000003000000000000000010000000000000\
    080000000100000000000000000000000000000000000000\
    2d000000600ae0e4\
    02290000000000000029000000000000000300000000000000\
    1100000014400100000000000200000000000000";

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
    FileId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn block(file: u32, offset: u64, len: u32) -> BlockPointer {
    BlockPointer::new(id(file), offset, len).expect("a block")
}

fn link(page: u32, raw_version: u64, kind: LinkKind) -> Link {
    Link {
        page: PageId::new(page).expect("not zero"),
        version: version(raw_version),
        kind,
    }
}

fn base(page: u32, raw_version: u64) -> Link {
    link(
        page,
        raw_version,
        LinkKind::Base(block(3, 20 + u64::from(page) * 100, 64)),
    )
}

fn delta(page: u32, raw_version: u64) -> Link {
    link(
        page,
        raw_version,
        LinkKind::Delta(block(4, 20 + raw_version % 1000 * 10, 9)),
    )
}

fn free(page: u32, raw_version: u64) -> Link {
    link(page, raw_version, LinkKind::Free)
}

fn small_links() -> [Link; 3] {
    [
        link(7, 40, LinkKind::Base(block(310, 20, 4104))),
        link(7, 41, LinkKind::Delta(block(310, 4124, 56))),
        free(9, 38),
    ]
}

/// One item of a links record, from its raw fields.
fn item(page: u32, kind: u32, len: u32, file: u32, offset: u64, raw_version: u64) -> Vec<u8> {
    let mut bytes = page.to_le_bytes().to_vec();
    bytes.extend_from_slice(&(kind << 28 | len).to_le_bytes());
    bytes.extend_from_slice(&file.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());
    bytes.extend_from_slice(&raw_version.to_le_bytes());
    bytes
}

fn base_item(page: u32, raw_version: u64) -> Vec<u8> {
    item(page, BASE, 64, 3, 20, raw_version)
}

fn delta_item(page: u32, raw_version: u64) -> Vec<u8> {
    item(page, DELTA, 9, 4, 84, raw_version)
}

fn free_item(page: u32, raw_version: u64) -> Vec<u8> {
    item(page, FREE, 0, 0, 0, raw_version)
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_record(payload, &mut out).expect("a record");
    out
}

fn links_record(items: &[Vec<u8>]) -> Vec<u8> {
    let mut payload = vec![LINKS_RECORD];
    payload.extend(items.iter().flatten());
    framed(&payload)
}

/// An end record with no place in the log and no counts.
fn end_record(covers: u64, reaches: u64, links: u64) -> Vec<u8> {
    end_record_with(covers, reaches, links, (0, 0), 0)
}

/// An end record from its raw fields. `place` is a log file id and an
/// offset.
fn end_record_with(
    covers: u64,
    reaches: u64,
    links: u64,
    place: (u32, u64),
    value_counts: u64,
) -> Vec<u8> {
    let mut payload = vec![END_RECORD];
    for field in [covers, reaches, links] {
        payload.extend_from_slice(&field.to_le_bytes());
    }
    payload.extend_from_slice(&place.0.to_le_bytes());
    payload.extend_from_slice(&place.1.to_le_bytes());
    payload.extend_from_slice(&value_counts.to_le_bytes());
    framed(&payload)
}

/// One item of a counts record, from its raw fields.
fn count_item(file: u32, flags: u32, values: u64, bytes: u64) -> Vec<u8> {
    let mut item = file.to_le_bytes().to_vec();
    item.extend_from_slice(&flags.to_le_bytes());
    item.extend_from_slice(&values.to_le_bytes());
    item.extend_from_slice(&bytes.to_le_bytes());
    item
}

fn counts_record(items: &[Vec<u8>]) -> Vec<u8> {
    let mut payload = vec![VALUE_COUNTS_RECORD];
    payload.extend(items.iter().flatten());
    framed(&payload)
}

fn count(file: u32, values: u64) -> ValueCount {
    ValueCount {
        file: id(file),
        values,
        bytes: values * 100,
        miscounted: values % 7 == 3,
    }
}

fn place(file: u32, offset: u64) -> LogPlace {
    LogPlace::new(id(file), offset).expect("a place")
}

/// The checkpoint file `id` with `links` and an empty sample of the counts.
fn encode_links(
    id: FileId,
    covers: CommitVersion,
    links: impl IntoIterator<Item = Link>,
) -> Result<Vec<u8>, FormatError> {
    encode(id, covers, links, None, [])
}

fn small_counts() -> [ValueCount; 2] {
    [
        ValueCount {
            file: id(5),
            values: 3,
            bytes: 4096,
            miscounted: false,
        },
        ValueCount {
            file: id(8),
            values: 0,
            bytes: 0,
            miscounted: true,
        },
    ]
}

/// A whole file with the id 1 from its records.
fn file(records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = file_header(id(1)).encode().to_vec();
    bytes.extend(records.iter().flatten());
    bytes
}

/// A file with one links record of `items` and an end record that agrees
/// with them.
fn file_of(items: &[Vec<u8>]) -> Vec<u8> {
    let reaches = items
        .iter()
        .map(|item| u64::from_le_bytes(item[20..].try_into().unwrap()))
        .max()
        .unwrap_or(NO_VERSION);
    file(&[
        links_record(items),
        end_record(0, reaches, items.len() as u64),
    ])
}

fn decoded(bytes: &[u8]) -> Result<Checkpoint, FormatError> {
    decode(bytes, id(1))
}

fn table_error(error: TableError) -> FormatError {
    FormatError::Table(error)
}

/// Chains drawn from `rng` for pages in ascending order: `pages` pages with
/// at least one link, and some ids left out between them.
fn random_links(rng: &mut SplitMix64, pages: usize) -> Vec<Link> {
    let mut links = Vec::new();
    let mut page = 0;
    for _ in 0..pages {
        page += 1 + (rng.next_u64() % 3) as u32;
        let mut raw_version = rng.next_u64() % 50;
        let mut open = false;
        for _ in 0..1 + rng.next_u64() % 6 {
            raw_version += rng.next_u64() % 3;
            let pointer = block(
                1 + (rng.next_u64() % 9) as u32,
                20 + rng.next_u64() % (1 << 40),
                MIN_BLOCK_LEN + (rng.next_u64() % 5000) as u32,
            );
            let kind = match rng.next_u64() % 8 {
                0 if open => LinkKind::Free,
                1..=4 if open => LinkKind::Delta(pointer),
                _ => LinkKind::Base(pointer),
            };
            open = kind != LinkKind::Free;
            links.push(link(page, raw_version, kind));
        }
    }
    links
}

#[test]
fn golden_bytes() {
    let empty = encode_links(id(1), CommitVersion::EMPTY, []).unwrap();
    assert_eq!(hex(&empty), GOLDEN_EMPTY);
    assert_eq!(empty.len(), HEADER_LEN + END_RECORD_LEN);
    let checkpoint = decode(&empty, id(1)).unwrap();
    assert_eq!(
        checkpoint,
        Checkpoint {
            version: V1,
            end: End {
                covers: CommitVersion::EMPTY,
                reaches: None,
                links: 0,
                place: None,
                value_counts: 0,
            },
            links: Vec::new(),
            value_counts: Vec::new(),
        }
    );

    let log_end = place(17, 81_940);
    let small = encode(
        id(122),
        version(41),
        small_links(),
        Some(log_end),
        small_counts(),
    )
    .unwrap();
    assert_eq!(hex(&small), GOLDEN_SMALL);
    assert_eq!(small.len(), 223);
    let checkpoint = decode(&unhex(GOLDEN_SMALL), id(122)).unwrap();
    assert_eq!(checkpoint.links, small_links());
    assert_eq!(checkpoint.value_counts, small_counts());
    assert_eq!(
        checkpoint.end,
        End {
            covers: version(41),
            reaches: Some(version(41)),
            links: 3,
            place: Some(log_end),
            value_counts: 2,
        }
    );
}

#[test]
fn a_link_is_28_bytes_and_a_checkpoint_of_a_million_pages_is_28_megabytes() {
    let pages: usize = 1_000_000;
    let records = pages.div_ceil(LINKS_PER_RECORD);
    let links = (1..=pages as u32).map(|page| base(page, u64::from(page) % 977));
    let bytes = encode_links(id(1), version(977), links).unwrap();
    assert_eq!(
        bytes.len(),
        HEADER_LEN + records * (FRAME + KIND_LEN) + pages * LINK_LEN + END_RECORD_LEN
    );
    assert_eq!(bytes.len(), 28_002_278);
    assert!(bytes.len() < 32_000_000);

    let checkpoint = decoded(&bytes).unwrap();
    assert_eq!(checkpoint.links.len(), pages);
    assert_eq!(checkpoint.end.reaches, Some(version(976)));
}

#[test]
fn checkpoints_round_trip_in_both_directions() {
    let mut rng = SplitMix64::new(0x7AB1E);
    for pages in [0, 1, 2, 30, 700, 1500, 3000] {
        let links = random_links(&mut rng, pages);
        let covers = version(rng.next_u64() % 100);
        let bytes = encode_links(id(9), covers, links.iter().copied()).unwrap();
        let checkpoint = decode(&bytes, id(9)).unwrap();
        assert_eq!(checkpoint.links, links);
        assert_eq!(checkpoint.version, FormatVersion::CURRENT);
        assert_eq!(checkpoint.end.covers, covers);
        assert_eq!(checkpoint.end.links, links.len() as u64);
        assert_eq!(
            checkpoint.end.reaches,
            links.iter().map(|link| link.version).max()
        );
        assert_eq!(
            encode_links(id(9), covers, checkpoint.links).unwrap(),
            bytes
        );
    }
}

#[test]
fn every_record_but_the_last_holds_exactly_4096_links() {
    assert_eq!((LINK_LEN, LINKS_PER_RECORD), (28, 4096));
    assert_eq!((MAX_PAYLOAD_LEN, MAX_RECORD_LEN), (114_689, 114_697));
    assert_eq!(END_RECORD_LEN, 53);
    let two_records =
        encode_links(id(1), version(1), (1..=4097).map(|page| base(page, 1))).unwrap();
    assert_eq!(two_records.len(), 20 + 114_697 + 37 + 53);
    for (count, lens) in [
        (LINKS_PER_RECORD - 1, vec![LINKS_PER_RECORD - 1]),
        (LINKS_PER_RECORD, vec![LINKS_PER_RECORD]),
        (LINKS_PER_RECORD + 1, vec![LINKS_PER_RECORD, 1]),
        (2 * LINKS_PER_RECORD, vec![LINKS_PER_RECORD; 2]),
        (
            2 * LINKS_PER_RECORD + 5,
            vec![LINKS_PER_RECORD, LINKS_PER_RECORD, 5],
        ),
    ] {
        let links: Vec<_> = (1..=count as u32).map(|page| base(page, 1)).collect();
        let bytes = encode_links(id(1), version(1), links.iter().copied()).unwrap();

        let mut decoder = Decoder::new(V1);
        let mut rest = &bytes[HEADER_LEN..];
        let mut found = Vec::new();
        let mut read = Vec::new();
        while !decoder.is_finished() {
            let (record, after) = decoder.read(rest).unwrap();
            assert!(rest.len() - after.len() <= MAX_RECORD_LEN);
            rest = after;
            if let Record::Links(record) = record {
                found.push(record.len());
                read.extend(record);
            }
        }
        assert!(rest.is_empty());
        assert_eq!(found, lens, "{count} links");
        assert_eq!(read, links);
    }
}

#[test]
fn the_encoder_hands_out_each_record_as_it_is_complete() {
    let mut encoder = Encoder::default();
    let mut out = Vec::new();
    for page in 1..LINKS_PER_RECORD as u32 {
        encoder.push(base(page, 5), &mut out).unwrap();
        assert!(out.is_empty());
    }
    encoder
        .push(base(LINKS_PER_RECORD as u32, 5), &mut out)
        .unwrap();
    assert_eq!(out.len(), MAX_RECORD_LEN);
    out.clear();
    encoder.push(free(5000, 7), &mut out).unwrap();
    assert!(out.is_empty());
    let end = encoder.finish(version(6), None, &mut out).unwrap();
    assert_eq!(out.len(), FRAME + KIND_LEN + LINK_LEN + END_RECORD_LEN);
    assert_eq!(
        end,
        End {
            covers: version(6),
            reaches: Some(version(7)),
            links: LINKS_PER_RECORD as u64 + 1,
            place: None,
            value_counts: 0,
        }
    );
}

#[test]
fn the_encoder_refuses_a_link_that_cannot_follow_and_goes_on() {
    let out_of_order = table_error(TableError::LinksOutOfOrder);
    let no_chain = table_error(TableError::InvalidChain);
    let cases = [
        (vec![base(5, 3)], base(4, 9), out_of_order),
        (vec![base(5, 3)], delta(5, 2), out_of_order),
        (vec![base(5, 3)], free(5, 2), out_of_order),
        (vec![], delta(5, 2), no_chain),
        (vec![base(4, 1)], delta(5, 2), no_chain),
        (vec![free(5, 2)], delta(5, 3), no_chain),
        (vec![base(5, 1), free(5, 2)], free(5, 3), no_chain),
    ];
    for (accepted, refused, error) in cases {
        let mut encoder = Encoder::new();
        let mut out = Vec::new();
        for link in &accepted {
            encoder.push(*link, &mut out).unwrap();
        }
        assert_eq!(encoder.push(refused, &mut out), Err(error), "{refused:?}");
        encoder.push(base(6, 0), &mut out).unwrap();
        encoder.finish(version(0), None, &mut out).unwrap();

        let mut expected = accepted.clone();
        expected.push(base(6, 0));
        assert_eq!(
            out,
            encode_links(id(1), version(0), expected).unwrap()[HEADER_LEN..]
        );
    }
    assert_eq!(
        encode_links(id(1), version(0), [delta(1, 1)]),
        Err(table_error(TableError::InvalidChain))
    );
}

#[test]
fn every_chain_a_page_can_have_is_accepted() {
    let chains: [&[Link]; 8] = [
        &[base(1, 5)],
        &[free(1, 5)],
        &[base(1, 5), delta(1, 5), delta(1, 6)],
        &[base(1, 5), base(1, 5), delta(1, 7), base(1, 7)],
        &[base(1, 5), free(1, 5), base(1, 5)],
        &[base(1, 5), delta(1, 6), free(1, 7), base(1, 9), delta(1, 9)],
        &[free(1, 5), base(1, 6), free(1, 7)],
        &[base(1, 0), delta(1, u64::MAX - 1)],
    ];
    for chain in chains {
        let mut links = chain.to_vec();
        links.push(base(u32::MAX, 0));
        let bytes = encode_links(id(1), version(0), links.iter().copied()).unwrap();
        assert_eq!(decoded(&bytes).unwrap().links, links);
    }
}

#[test]
fn block_pointers_are_checked() {
    let invalid = Err(table_error(TableError::InvalidLink));
    let file = id(1);
    for (offset, len) in [
        (20, MIN_BLOCK_LEN),
        (20, MAX_BLOCK_LEN),
        (u64::MAX - u64::from(MAX_BLOCK_LEN), MAX_BLOCK_LEN),
        (u64::MAX - 8, 8),
    ] {
        let pointer = BlockPointer::new(file, offset, len).unwrap();
        assert_eq!(
            (pointer.file(), pointer.offset(), pointer.len()),
            (file, offset, len)
        );
        assert_eq!(pointer.end(), offset + u64::from(len));
        let kind = LinkKind::Delta(pointer);
        assert_eq!(kind.block(), Some(pointer));
        let links = [base(1, 1), link(1, 2, kind)];
        let bytes = encode_links(id(1), version(2), links).unwrap();
        assert_eq!(decoded(&bytes).unwrap().links, links);
    }
    assert_eq!(LinkKind::Free.block(), None);
    for (offset, len) in [
        (20, MIN_BLOCK_LEN - 1),
        (20, 0),
        (20, MAX_BLOCK_LEN + 1),
        (20, u32::MAX),
        (19, 64),
        (0, 64),
        (u64::MAX - 7, 8),
        (u64::MAX, 64),
    ] {
        assert_eq!(BlockPointer::new(file, offset, len), invalid);
    }
    assert_eq!(MIN_BLOCK_LEN, 8);
    assert_eq!(MAX_BLOCK_LEN, (1 << 27) + 4);
}

#[test]
fn a_forged_link_is_rejected_by_the_rule_it_breaks() {
    let invalid = table_error(TableError::InvalidLink);
    let cases: [(Vec<u8>, FormatError); 18] = [
        (item(0, BASE, 64, 3, 20, 1), FormatError::MissingId),
        (item(1, BASE, 64, 0, 20, 1), FormatError::MissingId),
        (item(1, DELTA, 64, 0, 20, 1), FormatError::MissingId),
        (item(1, BASE, 64, 3, 20, NO_VERSION), FormatError::MissingId),
        (item(1, FREE, 0, 0, 0, NO_VERSION), FormatError::MissingId),
        (
            item(1, 0, 64, 3, 20, 1),
            table_error(TableError::UnknownLinkKind { code: 0 }),
        ),
        (
            item(1, 0, 0, 0, 0, 0),
            table_error(TableError::UnknownLinkKind { code: 0 }),
        ),
        (
            item(1, BASE | 0b100, 64, 3, 20, 1),
            FormatError::NonZeroReserved,
        ),
        (
            item(1, FREE | 0b1000, 0, 0, 0, 1),
            FormatError::NonZeroReserved,
        ),
        (item(1, BASE, 7, 3, 20, 1), invalid),
        (item(1, BASE, MAX_BLOCK_LEN + 1, 3, 20, 1), invalid),
        (item(1, DELTA, 64, 3, 19, 1), invalid),
        (item(1, BASE, 64, 3, u64::MAX - 63, 1), invalid),
        (item(1, FREE, 8, 0, 0, 1), invalid),
        (item(1, FREE, 0, 3, 0, 1), invalid),
        (item(1, FREE, 0, 0, 20, 1), invalid),
        (
            item(1, BASE, 64, u32::MAX, 20, 1),
            FormatError::ReservedFileId,
        ),
        (
            item(1, FREE, 0, u32::MAX, 0, 1),
            FormatError::ReservedFileId,
        ),
    ];
    for (forged, error) in cases {
        assert_eq!(
            decoded(&file_of(std::slice::from_ref(&forged))),
            Err(error),
            "{forged:?}"
        );
        let after_a_base = [base_item(1, 0), forged];
        assert_eq!(decoded(&file_of(&after_a_base)), Err(error));
    }
    assert!(decoded(&file_of(&[item(1, BASE, 8, 3, u64::MAX - 8, 1)])).is_ok());
}

#[test]
fn links_out_of_order_and_chains_that_cannot_be_are_rejected() {
    let out_of_order = table_error(TableError::LinksOutOfOrder);
    let no_chain = table_error(TableError::InvalidChain);
    let cases: [(Vec<Vec<u8>>, FormatError); 9] = [
        (vec![base_item(2, 1), base_item(1, 1)], out_of_order),
        (vec![base_item(2, 5), base_item(2, 4)], out_of_order),
        (vec![base_item(2, 5), delta_item(2, 4)], out_of_order),
        (vec![base_item(2, 5), free_item(2, 4)], out_of_order),
        (vec![delta_item(1, 1)], no_chain),
        (vec![base_item(1, 1), delta_item(2, 1)], no_chain),
        (vec![free_item(1, 1), delta_item(1, 1)], no_chain),
        (vec![free_item(1, 1), free_item(1, 2)], no_chain),
        (
            vec![base_item(1, 1), free_item(1, 1), delta_item(1, 2)],
            no_chain,
        ),
    ];
    for (items, error) in cases {
        assert_eq!(decoded(&file_of(&items)), Err(error), "{items:?}");
    }

    let across_records = file(&[
        links_record(&vec![base_item(9, 4); LINKS_PER_RECORD]),
        links_record(&[delta_item(9, 3)]),
        end_record(0, 4, LINKS_PER_RECORD as u64 + 1),
    ]);
    assert_eq!(decoded(&across_records), Err(out_of_order));
    let across_records = file(&[links_record(&vec![free_item(9, 4); 1]), end_record(0, 4, 1)]);
    assert!(decoded(&across_records).is_ok());
}

#[test]
fn records_that_are_misplaced_or_of_the_wrong_length_are_rejected() {
    let unexpected = |code| table_error(TableError::UnexpectedRecordKind { code });
    let length = |expected, found| table_error(TableError::LengthMismatch { expected, found });
    let full = links_record(&vec![base_item(1, 1); LINKS_PER_RECORD]);
    let full_count = LINKS_PER_RECORD as u64;
    let one = links_record(&[base_item(2, 1)]);
    let mut odd = vec![LINKS_RECORD];
    odd.extend(base_item(1, 1));
    odd.push(0);
    let mut short_end = vec![END_RECORD];
    short_end.extend([0; END_BODY_LEN - 1]);
    let mut long_end = vec![END_RECORD];
    long_end.extend([0; END_BODY_LEN + 1]);
    let mut too_many = vec![LINKS_RECORD];
    too_many.extend(base_item(1, 1).repeat(LINKS_PER_RECORD + 1));
    let too_many = {
        let len = (too_many.len() as u32).to_le_bytes();
        let mut record = len.to_vec();
        record.extend(Checksum::of(&[&len, &too_many]).encode());
        record.extend(too_many);
        record
    };

    let cases: [(Vec<Vec<u8>>, FormatError); 15] = [
        (
            vec![one.clone(), one.clone(), end_record(0, 1, 2)],
            unexpected(1),
        ),
        (vec![full.clone(), one.clone(), one.clone()], unexpected(1)),
        (
            vec![end_record(0, NO_VERSION, 0), one.clone()],
            length(73, 110),
        ),
        (
            vec![end_record(0, NO_VERSION, 0), end_record(0, NO_VERSION, 0)],
            length(73, 126),
        ),
        (vec![framed(&[LINKS_RECORD])], length(28, 0)),
        (vec![framed(&odd)], length(28, 29)),
        (vec![framed(&odd[..20])], length(28, 19)),
        (vec![framed(&[])], length(1, 0)),
        (vec![framed(&short_end)], length(44, 43)),
        (vec![framed(&long_end)], length(44, 45)),
        (vec![framed(&[END_RECORD])], length(44, 0)),
        (
            vec![framed(&[0; 25])],
            FormatError::UnknownRecordKind { code: 0 },
        ),
        (
            vec![framed(&[4; 29])],
            FormatError::UnknownRecordKind { code: 4 },
        ),
        (
            vec![too_many],
            FormatError::RecordTooLong {
                len: MAX_PAYLOAD_LEN as u64 + LINK_LEN as u64,
                max: MAX_PAYLOAD_LEN as u64,
            },
        ),
        (
            vec![full.clone(), full.clone(), end_record(0, 1, full_count)],
            table_error(TableError::LinkCountMismatch {
                expected: 2 * full_count,
                found: full_count,
            }),
        ),
    ];
    for (records, error) in cases {
        assert_eq!(decoded(&file(&records)), Err(error));
    }
    assert!(decoded(&file(&[full, one, end_record(0, 1, full_count + 1)])).is_ok());
}

#[test]
fn a_payload_is_bounded_whether_or_not_it_comes_in_a_frame() {
    let mut full = vec![LINKS_RECORD];
    full.extend(base_item(1, 1).repeat(LINKS_PER_RECORD));
    let mut too_many = full.clone();
    too_many.extend(base_item(1, 1));
    let too_long = FormatError::RecordTooLong {
        len: MAX_PAYLOAD_LEN as u64 + LINK_LEN as u64,
        max: MAX_PAYLOAD_LEN as u64,
    };

    let mut decoder = Decoder::new(V1);
    assert_eq!(decoder.record(&too_many).unwrap_err(), too_long);
    let Record::Links(links) = decoder.record(&full).unwrap() else {
        panic!("a links record");
    };
    assert_eq!(links.len(), LINKS_PER_RECORD);
    let mut one_byte_more = full.clone();
    one_byte_more.push(0);
    assert!(matches!(
        decoder.record(&one_byte_more).unwrap_err(),
        FormatError::RecordTooLong { .. }
    ));
    let mut long_end = vec![END_RECORD];
    long_end.resize(MAX_PAYLOAD_LEN + 1, 0);
    assert!(matches!(
        decoder.record(&long_end).unwrap_err(),
        FormatError::RecordTooLong { .. }
    ));
}

#[test]
fn a_count_is_24_bytes_and_every_counts_record_but_the_last_holds_exactly_4096() {
    assert_eq!((VALUE_COUNT_LEN, VALUE_COUNTS_PER_RECORD), (24, 4096));
    assert_eq!(KIND_LEN + VALUE_COUNTS_PER_RECORD * VALUE_COUNT_LEN, 98_305);
    let full = VALUE_COUNTS_PER_RECORD;
    let log_end = Some(place(3, 77_000));
    for (links, counted, lens) in [
        (0, 0, vec![]),
        (0, 1, vec![1]),
        (3, 1, vec![1]),
        (LINKS_PER_RECORD, 2, vec![2]),
        (5, full - 1, vec![full - 1]),
        (5, full, vec![full]),
        (0, full + 1, vec![full, 1]),
        (LINKS_PER_RECORD + 1, 2 * full, vec![full, full]),
        (2, 2 * full + 5, vec![full, full, 5]),
    ] {
        let links: Vec<_> = (1..=links as u32).map(|page| base(page, 1)).collect();
        let counts: Vec<_> = (1..=counted as u32)
            .map(|file| count(file, u64::from(file)))
            .collect();
        let bytes = encode(
            id(1),
            version(1),
            links.iter().copied(),
            log_end,
            counts.iter().copied(),
        )
        .unwrap();
        let records = links.len().div_ceil(LINKS_PER_RECORD) + lens.len();
        assert_eq!(
            bytes.len(),
            HEADER_LEN
                + records * (FRAME + KIND_LEN)
                + links.len() * LINK_LEN
                + counted * VALUE_COUNT_LEN
                + END_RECORD_LEN
        );

        let mut decoder = Decoder::new(V1);
        let mut rest = &bytes[HEADER_LEN..];
        let (mut found, mut read, mut end) = (Vec::new(), Vec::new(), None);
        while !decoder.is_finished() {
            let (record, after) = decoder.read(rest).unwrap();
            rest = after;
            match record {
                Record::Links(_) => assert!(found.is_empty(), "links after counts"),
                Record::ValueCounts(record) => {
                    assert_eq!(record.size_hint(), (record.len(), Some(record.len())));
                    found.push(record.len());
                    read.extend(record);
                }
                Record::End(record) => end = Some(record),
            }
        }
        assert!(rest.is_empty());
        assert_eq!(found, lens, "{counted} counts");
        assert_eq!(read, counts);
        let end = end.expect("an end record");
        assert_eq!((end.place, end.value_counts), (log_end, counted as u64));

        let checkpoint = decoded(&bytes).unwrap();
        assert_eq!(checkpoint.links, links);
        assert_eq!(checkpoint.value_counts, counts);
    }
}

#[test]
fn the_encoder_takes_counts_after_the_links_and_no_link_after_a_count() {
    let mut encoder = Encoder::new();
    let mut out = Vec::new();
    encoder.push(base(1, 5), &mut out).unwrap();
    encoder.push(delta(1, 6), &mut out).unwrap();
    assert!(out.is_empty());

    // The first count ends the links: their record is handed out.
    encoder.push_value_count(count(4, 9), &mut out).unwrap();
    assert_eq!(out.len(), FRAME + KIND_LEN + 2 * LINK_LEN);
    assert_eq!(
        encoder.push(base(2, 7), &mut out).unwrap_err(),
        table_error(TableError::UnexpectedRecordKind { code: 1 })
    );
    for refused in [4, 3, 1] {
        assert_eq!(
            encoder
                .push_value_count(count(refused, 1), &mut out)
                .unwrap_err(),
            table_error(TableError::CountsOutOfOrder)
        );
    }
    assert_eq!(out.len(), FRAME + KIND_LEN + 2 * LINK_LEN);
    encoder.push_value_count(count(5, 2), &mut out).unwrap();
    let end = encoder
        .finish(version(6), Some(place(1, 20)), &mut out)
        .unwrap();
    assert_eq!(
        end,
        End {
            covers: version(6),
            reaches: Some(version(6)),
            links: 2,
            place: Some(place(1, 20)),
            value_counts: 2,
        }
    );
    let mut bytes = file_header(id(1)).encode().to_vec();
    bytes.extend(out);
    let checkpoint = decoded(&bytes).unwrap();
    assert_eq!(checkpoint.links, [base(1, 5), delta(1, 6)]);
    assert_eq!(checkpoint.value_counts, [count(4, 9), count(5, 2)]);

    // Counts with no link before them, and a record handed out when full.
    let mut encoder = Encoder::new();
    let mut out = Vec::new();
    for file in 1..VALUE_COUNTS_PER_RECORD as u32 {
        encoder.push_value_count(count(file, 1), &mut out).unwrap();
        assert!(out.is_empty());
    }
    encoder
        .push_value_count(count(VALUE_COUNTS_PER_RECORD as u32, 1), &mut out)
        .unwrap();
    assert_eq!(
        out.len(),
        FRAME + KIND_LEN + VALUE_COUNTS_PER_RECORD * VALUE_COUNT_LEN
    );
    out.clear();
    encoder.finish(version(0), None, &mut out).unwrap();
    assert_eq!(out.len(), END_RECORD_LEN);
}

#[test]
fn a_forged_count_is_rejected_by_the_rule_it_breaks() {
    let with_counts = |items: &[Vec<u8>]| {
        decoded(&file(&[
            counts_record(items),
            end_record_with(0, NO_VERSION, 0, (0, 0), items.len() as u64),
        ]))
    };
    let out_of_order = table_error(TableError::CountsOutOfOrder);
    let good = with_counts(&[
        count_item(1, 0, 0, 0),
        count_item(2, 1, u64::MAX, u64::MAX),
        count_item(u32::MAX - 1, 0, 7, 1),
    ])
    .unwrap();
    assert_eq!(
        with_counts(&[count_item(1, 0, 0, 0), count_item(u32::MAX, 0, 7, 1)]),
        Err(FormatError::ReservedFileId)
    );
    assert_eq!(good.value_counts.len(), 3);
    assert!(good.value_counts[1].miscounted && !good.value_counts[0].miscounted);
    assert_eq!(
        (good.value_counts[1].values, good.value_counts[1].bytes),
        (u64::MAX, u64::MAX)
    );

    assert_eq!(
        with_counts(&[count_item(0, 0, 1, 1)]),
        Err(FormatError::MissingId)
    );
    for bit in 1..32 {
        for flags in [1 << bit, 1 << bit | 1] {
            assert_eq!(
                with_counts(&[count_item(3, flags, 1, 1)]),
                Err(FormatError::NonZeroReserved),
                "flags {flags:#x}"
            );
        }
    }
    for (first, second) in [(3, 3), (3, 2), (u32::MAX - 1, 1)] {
        assert_eq!(
            with_counts(&[count_item(first, 0, 1, 1), count_item(second, 0, 1, 1)]),
            Err(out_of_order)
        );
    }
    let full: Vec<_> = (1..=VALUE_COUNTS_PER_RECORD as u32)
        .map(|file| count_item(file, 0, 1, 1))
        .collect();
    let last = VALUE_COUNTS_PER_RECORD as u32;
    let across = |next: u32| {
        decoded(&file(&[
            counts_record(&full),
            counts_record(&[count_item(next, 0, 1, 1)]),
            end_record_with(0, NO_VERSION, 0, (0, 0), u64::from(last) + 1),
        ]))
    };
    assert_eq!(across(last), Err(out_of_order));
    assert!(across(last + 1).is_ok());
}

#[test]
fn counts_records_that_are_misplaced_or_of_the_wrong_length_are_rejected() {
    let unexpected = |code| table_error(TableError::UnexpectedRecordKind { code });
    let length = |expected, found| table_error(TableError::LengthMismatch { expected, found });
    let end = |links: u64, counts: u64| {
        let reaches = if links == 0 { NO_VERSION } else { 1 };
        end_record_with(0, reaches, links, (0, 0), counts)
    };
    let full_links = links_record(&vec![base_item(1, 1); LINKS_PER_RECORD]);
    let one_link = links_record(&[base_item(2, 1)]);
    let full: Vec<_> = (1..=VALUE_COUNTS_PER_RECORD as u32)
        .map(|file| count_item(file, 0, 1, 1))
        .collect();
    let full_counts = counts_record(&full);
    let one_count = |file: u32| counts_record(&[count_item(file, 0, 1, 1)]);
    let per_record = VALUE_COUNTS_PER_RECORD as u64;
    let mut too_many = full.clone();
    too_many.push(count_item(5000, 0, 1, 1));
    let mut odd = vec![VALUE_COUNTS_RECORD];
    odd.extend(count_item(1, 0, 1, 1));
    odd.push(0);

    let accepted: [Vec<Vec<u8>>; 5] = [
        vec![one_count(1), end(0, 1)],
        vec![one_link.clone(), one_count(1), end(1, 1)],
        vec![full_links.clone(), one_count(1), end(per_record, 1)],
        vec![full_counts.clone(), end(0, per_record)],
        vec![full_counts.clone(), one_count(9000), end(0, per_record + 1)],
    ];
    for records in accepted {
        assert!(decoded(&file(&records)).is_ok());
    }

    let cases: [(Vec<Vec<u8>>, FormatError); 10] = [
        (
            vec![one_count(1), one_link.clone(), end(1, 1)],
            unexpected(1),
        ),
        (
            vec![full_counts.clone(), one_link.clone(), end(1, per_record)],
            unexpected(1),
        ),
        (vec![one_count(1), one_count(2), end(0, 2)], unexpected(3)),
        (vec![end(0, 0), one_count(1)], length(73, 73 + 8 + 1 + 24)),
        (vec![framed(&[VALUE_COUNTS_RECORD])], length(24, 0)),
        (vec![framed(&odd)], length(24, 25)),
        (vec![framed(&odd[..20])], length(24, 19)),
        (
            vec![counts_record(&too_many)],
            length(per_record * 24, (per_record + 1) * 24),
        ),
        (
            vec![one_count(1), end(0, 0)],
            table_error(TableError::ValueCountMismatch {
                expected: 1,
                found: 0,
            }),
        ),
        (
            vec![full_counts.clone(), full_counts.clone()],
            table_error(TableError::CountsOutOfOrder),
        ),
    ];
    for (records, error) in cases {
        assert_eq!(decoded(&file(&records)), Err(error));
    }

    // A decoder that is handed a record after the counts have ended, or
    // after the end record, refuses it by its kind.
    let mut decoder = Decoder::new(V1);
    decoder.read(&one_count(1)).unwrap();
    for (record, code) in [(one_count(2), 3), (one_link.clone(), 1)] {
        assert_eq!(decoder.read(&record).unwrap_err(), unexpected(code));
    }
    decoder.read(&end(0, 1)).unwrap();
    assert_eq!(decoder.read(&one_count(2)).unwrap_err(), unexpected(3));
}

#[test]
fn the_end_record_must_agree_with_the_counts_and_name_a_place_in_the_log() {
    let items = [count_item(2, 0, 5, 500), count_item(7, 1, 0, 9)];
    let with_end = |log_end: (u32, u64), counts: u64| {
        decoded(&file(&[
            counts_record(&items),
            end_record_with(0, NO_VERSION, 0, log_end, counts),
        ]))
    };
    assert_eq!(with_end((0, 0), 2).unwrap().end.place, None);
    assert_eq!(with_end((9, 20), 2).unwrap().end.place, Some(place(9, 20)));
    assert_eq!(
        with_end((u32::MAX - 1, u64::MAX), 2).unwrap().end.place,
        Some(place(u32::MAX - 1, u64::MAX))
    );
    for offset in [0, 20, u64::MAX] {
        assert_eq!(
            with_end((u32::MAX, offset), 2),
            Err(FormatError::ReservedFileId)
        );
    }
    let invalid = table_error(TableError::InvalidLogPlace);
    for log_end in [(0, 1), (0, 20), (0, u64::MAX), (9, 0), (9, 19)] {
        assert_eq!(with_end(log_end, 2), Err(invalid), "{log_end:?}");
    }
    for counts in [0, 1, 3, u64::MAX] {
        assert_eq!(
            with_end((0, 0), counts),
            Err(table_error(TableError::ValueCountMismatch {
                expected: 2,
                found: counts,
            }))
        );
    }
    let none = decoded(&file(&[end_record_with(0, NO_VERSION, 0, (0, 0), 1)]));
    assert_eq!(
        none,
        Err(table_error(TableError::ValueCountMismatch {
            expected: 0,
            found: 1,
        }))
    );

    assert_eq!(LogPlace::new(id(3), 19), Err(invalid));
    let made = LogPlace::new(id(3), 20).unwrap();
    assert_eq!((made.file(), made.offset()), (id(3), 20));
    let mut places = [place(2, 900), place(3, 20), place(1, 5000), place(2, 899)];
    places.sort();
    assert_eq!(
        places,
        [place(1, 5000), place(2, 899), place(2, 900), place(3, 20)]
    );
    assert!(None < Some(place(1, 20)));
}

#[test]
fn a_counts_record_that_fails_leaves_the_decoder_as_it_was() {
    let mut decoder = Decoder::new(V1);
    decoder.read(&links_record(&[base_item(3, 5)])).unwrap();
    let bad = [count_item(4, 0, 1, 1), count_item(4, 0, 1, 1)];
    decoder.read(&counts_record(&bad)).unwrap_err();
    decoder
        .read(&counts_record(&[
            count_item(6, 0, 1, 1),
            count_item(0, 0, 1, 1),
        ]))
        .unwrap_err();
    let mut payload = vec![VALUE_COUNTS_RECORD];
    payload.extend(count_item(4, 1, 8, 80));
    let Record::ValueCounts(counts) = decoder.record(&payload).unwrap() else {
        panic!("a counts record");
    };
    assert_eq!(
        counts.collect::<Vec<_>>(),
        [ValueCount {
            file: id(4),
            values: 8,
            bytes: 80,
            miscounted: true,
        }]
    );
    decoder
        .read(&end_record_with(0, 5, 1, (0, 0), 2))
        .unwrap_err();
    decoder
        .read(&end_record_with(0, 5, 1, (0, 7), 1))
        .unwrap_err();
    assert!(!decoder.is_finished());
    decoder.read(&end_record_with(0, 5, 1, (0, 0), 1)).unwrap();
    assert!(decoder.is_finished());
}

#[test]
fn a_record_after_the_end_record_is_refused_by_the_decoder() {
    let mut decoder = Decoder::new(V1);
    let end = end_record(7, NO_VERSION, 0);
    assert!(!decoder.is_finished());
    let (record, rest) = decoder.read(&end).unwrap();
    assert!(matches!(record, Record::End(end) if end.covers == version(7)));
    assert!(rest.is_empty() && decoder.is_finished());
    for (record, code) in [(end, 2), (links_record(&[base_item(1, 1)]), 1)] {
        assert_eq!(
            decoder.read(&record).unwrap_err(),
            table_error(TableError::UnexpectedRecordKind { code })
        );
    }
    assert_eq!(
        decoder.read(&framed(&[9])).unwrap_err(),
        FormatError::UnknownRecordKind { code: 9 }
    );
}

#[test]
fn a_record_that_fails_leaves_the_decoder_as_it_was() {
    let mut decoder = Decoder::new(V1);
    let good = [base_item(3, 5), delta_item(3, 6)];
    let bad = [base_item(4, 9), delta_item(2, 9)];
    decoder.read(&links_record(&bad)).unwrap_err();
    let mut payload = vec![LINKS_RECORD];
    payload.extend(good.iter().flatten());
    let Record::Links(links) = decoder.record(&payload).unwrap() else {
        panic!("a links record");
    };
    assert_eq!(links.size_hint(), (2, Some(2)));
    assert_eq!(links.collect::<Vec<_>>().len(), 2);
    decoder.read(&end_record(0, 9, 2)).unwrap_err();
    decoder.read(&end_record(0, 6, 3)).unwrap_err();
    assert!(!decoder.is_finished());
    decoder.read(&end_record(0, 6, 2)).unwrap();
    assert!(decoder.is_finished());
}

#[test]
fn the_end_record_must_agree_with_the_links() {
    let items = [base_item(1, 4), delta_item(1, 9), base_item(2, 6)];
    let with_end = |covers, reaches, links| {
        decoded(&file(&[
            links_record(&items),
            end_record(covers, reaches, links),
        ]))
    };
    assert!(with_end(0, 9, 3).is_ok());
    assert!(with_end(77, 9, 3).is_ok());
    assert_eq!(with_end(NO_VERSION, 9, 3), Err(FormatError::MissingId));
    for reaches in [6, 8, 10, NO_VERSION] {
        assert_eq!(
            with_end(0, reaches, 3),
            Err(table_error(TableError::ReachMismatch))
        );
    }
    for links in [0, 2, 4, u64::MAX] {
        assert_eq!(
            with_end(0, 9, links),
            Err(table_error(TableError::LinkCountMismatch {
                expected: 3,
                found: links,
            }))
        );
    }
    let no_links = |reaches, links| decoded(&file(&[end_record(0, reaches, links)]));
    assert!(no_links(NO_VERSION, 0).is_ok());
    assert_eq!(no_links(0, 0), Err(table_error(TableError::ReachMismatch)));
    assert_eq!(
        no_links(NO_VERSION, 1),
        Err(table_error(TableError::LinkCountMismatch {
            expected: 0,
            found: 1,
        }))
    );
}

#[test]
fn the_file_header_is_checked_against_the_name() {
    let bytes = unhex(GOLDEN_SMALL);
    assert_eq!(decode_file_header(&bytes, id(122)), Ok(V1));
    assert_eq!(file_header(id(122)).encode(), bytes[..HEADER_LEN]);
    assert_eq!(
        decode(&bytes, id(121)),
        Err(table_error(TableError::WrongFileId {
            expected: 121,
            found: 122,
        }))
    );
    for kind in FileKind::ALL {
        let header = FileHeader::new(kind, V1, 122).unwrap();
        let mut other = bytes.clone();
        other[..HEADER_LEN].copy_from_slice(&header.encode());
        let result = decode(&other, id(122));
        if kind == FileKind::PageTable {
            assert!(result.is_ok());
        } else {
            assert_eq!(
                result,
                Err(table_error(TableError::NotAPageTable { found: kind }))
            );
        }
    }
    let mut damaged = bytes.clone();
    damaged[0] ^= 1;
    assert_eq!(decode(&damaged, id(122)), Err(FormatError::BadMagic));
}

#[test]
fn every_shorter_input_is_truncated_and_every_longer_one_is_too_long() {
    let bytes = unhex(GOLDEN_SMALL);
    let record_end = HEADER_LEN + FRAME + KIND_LEN + 3 * LINK_LEN;
    let counts_end = record_end + FRAME + KIND_LEN + 2 * VALUE_COUNT_LEN;
    assert_eq!(bytes.len(), counts_end + END_RECORD_LEN);
    for len in 0..bytes.len() {
        let needed = match len {
            len if len < HEADER_LEN => HEADER_LEN,
            len if len < HEADER_LEN + FRAME => HEADER_LEN + FRAME,
            len if len < record_end => record_end,
            len if len < record_end + FRAME => record_end + FRAME,
            len if len < counts_end => counts_end,
            len if len < counts_end + FRAME => counts_end + FRAME,
            _ => bytes.len(),
        };
        assert_eq!(
            decode(&bytes[..len], id(122)),
            Err(FormatError::Truncated {
                needed,
                available: len,
            }),
            "{len} bytes"
        );
    }
    for extra in [1, 8, 33, 500] {
        let mut longer = bytes.clone();
        longer.resize(bytes.len() + extra, 0);
        assert_eq!(
            decode(&longer, id(122)),
            Err(table_error(TableError::LengthMismatch {
                expected: bytes.len() as u64,
                found: longer.len() as u64,
            }))
        );
    }
    let mut twice = bytes.clone();
    twice.extend_from_slice(&bytes[HEADER_LEN..]);
    assert!(decode(&twice, id(122)).is_err());
}

#[test]
fn every_single_bit_flip_is_rejected() {
    for golden in [GOLDEN_EMPTY, GOLDEN_SMALL] {
        let bytes = unhex(golden);
        let file_id = decode_file_header(&bytes, id(1)).map_or(id(122), |_| id(1));
        let frames: Vec<usize> = if golden == GOLDEN_EMPTY {
            vec![HEADER_LEN]
        } else {
            let counts = HEADER_LEN + FRAME + KIND_LEN + 3 * LINK_LEN;
            let end = counts + FRAME + KIND_LEN + 2 * VALUE_COUNT_LEN;
            vec![HEADER_LEN, counts, end]
        };
        for bit in 0..bytes.len() * 8 {
            let mut flipped = bytes.clone();
            flipped[bit / 8] ^= 1 << (bit % 8);
            let error = decode(&flipped, file_id).expect_err("a flipped bit is found");
            let at = bit / 8;
            let in_len_field = frames.iter().any(|frame| (*frame..frame + 4).contains(&at));
            if at < 8 {
                assert_eq!(error, FormatError::BadMagic);
            } else if at < HEADER_LEN || !in_len_field {
                assert!(
                    matches!(error, FormatError::ChecksumMismatch { .. }),
                    "bit {bit}: {error}"
                );
            } else {
                assert!(
                    matches!(
                        error,
                        FormatError::ChecksumMismatch { .. }
                            | FormatError::Truncated { .. }
                            | FormatError::RecordTooLong { .. }
                    ),
                    "bit {bit}: {error}"
                );
            }
        }
    }
}

#[test]
fn zero_filled_bytes_are_not_a_record() {
    let mut decoder = Decoder::new(V1);
    for len in [8, 9, 36, 37, 1000] {
        let error = decoder.read(&vec![0; len]).unwrap_err();
        assert!(
            matches!(error, FormatError::ChecksumMismatch { .. }),
            "{error}"
        );
    }
    assert_eq!(
        decoder.record(&[0; 29]).unwrap_err(),
        FormatError::UnknownRecordKind { code: 0 }
    );
    let mut zero_link = vec![LINKS_RECORD];
    zero_link.extend([0; LINK_LEN]);
    assert_eq!(
        decoder.record(&zero_link).unwrap_err(),
        FormatError::MissingId
    );
}

/// Forged files with valid checksums: what decodes must encode back to the
/// same bytes, so every value has one encoding.
#[test]
fn a_forged_file_decodes_only_to_what_encodes_back_to_it() {
    let mut rng = SplitMix64::new(0xF0_46ED);
    let (mut accepted, mut rejected, mut with_counts) = (0, 0, 0);
    for round in 0..40_000 {
        let count = 1 + rng.next_u64() % 4;
        let mut page = 1 + (rng.next_u64() % 3) as u32;
        let mut raw_version = rng.next_u64() % 4;
        let mut items = Vec::new();
        for _ in 0..count {
            let draw = rng.next_u64();
            if draw.is_multiple_of(3) {
                page += (draw >> 8) as u32 % 3;
            }
            raw_version += (draw >> 16) % 2;
            let kind = (draw >> 24) as u32 % 4;
            let mut fields = if kind == FREE {
                item(page, kind, 0, 0, 0, raw_version)
            } else {
                item(page, kind, 64, 3, 20, raw_version)
            };
            if round % 4 == 0 {
                let at = (draw >> 32) as usize % LINK_LEN;
                fields[at] ^= 1 << ((draw >> 40) % 8);
            }
            items.push(fields);
        }
        let reaches = items
            .iter()
            .map(|item| u64::from_le_bytes(item[20..].try_into().unwrap()))
            .max()
            .unwrap();
        let reaches = if round % 7 == 0 { reaches + 1 } else { reaches };
        let links = if round % 11 == 0 { count + 1 } else { count };

        let mut records = vec![links_record(&items)];
        let mut counted = rng.next_u64() % 4;
        let mut file_id = (rng.next_u64() % 3) as u32;
        let mut counts = Vec::new();
        for _ in 0..counted {
            let draw = rng.next_u64();
            file_id += u32::from(!draw.is_multiple_of(4)) * (1 + (draw >> 8) as u32 % 2);
            let flags = if (draw >> 16).is_multiple_of(9) {
                (draw >> 24) as u32
            } else {
                (draw >> 24) as u32 & 1
            };
            counts.push(count_item(file_id, flags, draw >> 40, draw >> 3));
        }
        if !counts.is_empty() {
            records.push(counts_record(&counts));
            with_counts += 1;
        }
        if round % 13 == 0 {
            counted += 1;
        }
        let places = [(0, 0), (0, 0), (4, 20), (4, 900), (4, 19), (0, 20)];
        let log_end = places[(rng.next_u64() % places.len() as u64) as usize];
        records.push(end_record_with(3, reaches, links, log_end, counted));
        let bytes = file(&records);
        match decoded(&bytes) {
            Ok(checkpoint) => {
                accepted += 1;
                let again = encode(
                    id(1),
                    checkpoint.end.covers,
                    checkpoint.links,
                    checkpoint.end.place,
                    checkpoint.value_counts,
                )
                .unwrap();
                assert_eq!(again, bytes);
            }
            Err(_) => rejected += 1,
        }
    }
    println!("{accepted} accepted, {rejected} rejected, {with_counts} with counts");
    assert!(
        accepted > 1_000 && rejected > 10_000 && with_counts > 20_000,
        "{accepted} {rejected} {with_counts}"
    );
}

#[test]
fn arbitrary_and_mutated_bytes_give_an_error_and_never_a_panic() {
    let mut rng = SplitMix64::new(0xB10);
    let valid = encode(
        id(1),
        version(60),
        random_links(&mut SplitMix64::new(4), 12),
        Some(place(2, 400)),
        (1..9).map(|file| count(file, u64::from(file) * 3)),
    )
    .unwrap();
    for _ in 0..20_000 {
        let len = (rng.next_u64() % 200) as usize;
        let _ = decoded(&rng.bytes(len));
        let mut prefixed = valid[..HEADER_LEN].to_vec();
        prefixed.extend(rng.bytes(len));
        assert!(decoded(&prefixed).is_err());

        let mut mutated = valid.clone();
        for _ in 0..1 + rng.next_u64() % 3 {
            let at = (rng.next_u64() % mutated.len() as u64) as usize;
            mutated[at] = rng.next_u64() as u8;
        }
        if mutated != valid {
            assert!(decoded(&mutated).is_err());
        }
        let cut = (rng.next_u64() % valid.len() as u64) as usize;
        assert!(decoded(&valid[..cut]).is_err());

        let mut decoder = Decoder::new(V1);
        let _ = decoder.record(&rng.bytes(len));
        let mut payload = rng.bytes(1 + LINK_LEN * (len % 5));
        payload[0] = 1 + (len % 2) as u8;
        let _ = decoder.record(&payload);
        let mut payload = rng.bytes(1 + VALUE_COUNT_LEN * (len % 5));
        payload[0] = VALUE_COUNTS_RECORD;
        let _ = decoder.record(&payload);
        let mut payload = rng.bytes(1 + END_BODY_LEN);
        payload[0] = END_RECORD;
        let _ = decoder.record(&payload);
    }
}

#[test]
fn errors_say_what_is_wrong() {
    let cases = [
        (
            TableError::NotAPageTable {
                found: FileKind::Segment,
            },
            "a Segment file, not a page-table checkpoint",
        ),
        (
            TableError::WrongFileId {
                expected: 7,
                found: 9,
            },
            "file header carries id 9, expected 7",
        ),
        (
            TableError::UnexpectedRecordKind { code: 1 },
            "record kind code 1 is not allowed here",
        ),
        (
            TableError::LengthMismatch {
                expected: 28,
                found: 29,
            },
            "length is 29, expected 28",
        ),
        (
            TableError::UnknownLinkKind { code: 0 },
            "unknown link kind code 0",
        ),
        (TableError::InvalidLink, "link breaks a rule for its kind"),
        (
            TableError::LinksOutOfOrder,
            "links are not in ascending order of page id and version",
        ),
        (
            TableError::InvalidChain,
            "the kinds of a page's links do not form a chain",
        ),
        (
            TableError::LinkCountMismatch {
                expected: 3,
                found: 4,
            },
            "end record counts 4 links, the records hold 3",
        ),
        (
            TableError::ReachMismatch,
            "end record's newest version is not the newest version of the links",
        ),
        (
            TableError::CountsOutOfOrder,
            "counts are not in ascending order of file id",
        ),
        (
            TableError::ValueCountMismatch {
                expected: 3,
                found: 4,
            },
            "end record counts 4 counts, the records hold 3",
        ),
        (
            TableError::InvalidLogPlace,
            "the place in the log is not one a record has",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        assert_eq!(
            FormatError::from(error).to_string(),
            format!("page-table checkpoint: {text}")
        );
    }
}
