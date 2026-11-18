use std::collections::BTreeMap;

use super::*;
use crate::{
    flat_page::FlatPageKind,
    test_util::SplitMix64,
    value_log::{ValueLogError, ValuePointer},
    CellAddress, FileId, FormatError, FormatVersion, PageId,
};

pub(crate) mod model;
mod reading;
mod writing;

use model::{ModelCell, ModelLink, ModelPage, ModelValue, Rng};

const V1: FormatVersion = FormatVersion::V1;
const SIZES: [TreePageSize; 3] = [TreePageSize::MIN, TreePageSize::DEFAULT, TreePageSize::MAX];

fn page_id(raw: u32) -> PageId {
    PageId::new(raw).expect("non-zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("non-zero")
}

fn address(page: u32, raw_slot: u16) -> CellAddress {
    CellAddress::new(page_id(page), slot(raw_slot))
}

fn pointer(file: u32, offset: u64, value_len: u32) -> ValuePointer {
    ValuePointer::new(FileId::new(file).expect("non-zero"), offset, value_len).expect("valid")
}

fn tree_page_error(error: FormatError) -> TreePageError {
    match error {
        FormatError::TreePage(error) => error,
        other => panic!("expected a tree page error, got {other:?}"),
    }
}

/// A frame of `size` holding the page of `model`, built cell by cell.
fn frame_of(model: &ModelPage, size: TreePageSize) -> Vec<u8> {
    let mut frame = vec![0xEE; size.get()];
    let mut page = TreePageMut::init(&mut frame, size, model.id, model.departures).expect("fits");
    for (&raw, cell) in &model.cells {
        page.insert(slot(raw), &cell.content()).expect("fits");
    }
    frame
}

fn open(frame: &mut [u8], size: TreePageSize) -> TreePageMut<'_> {
    TreePageMut::open(frame, size, V1).expect("a page")
}

fn view(frame: &[u8]) -> TreePage<'_> {
    TreePage::from_validated(frame, V1).expect("a page")
}

/// The page of the documentation's example: three slots, the second free.
fn example() -> ModelPage {
    let mut page = ModelPage::new(7, 2);
    page.cells.insert(
        1,
        ModelCell {
            key: b"ab".to_vec(),
            feature: vec![0x03, 0x2A],
            value_hash: [0x11; 32],
            kv_hash: [0x22; 32],
            left: Some(ModelLink {
                child: address(7, 3),
                hash: [0x33; 32],
                heights: (1, 0),
                child_key_len: 3,
                aggregate: vec![0x05],
            }),
            right: None,
            hosted: vec![Some(address(9, 1)), None],
            value: ModelValue::OutOfLine(pointer(12, 20, 300)),
            fragment_root: true,
        },
    );
    page.cells.insert(
        3,
        ModelCell {
            key: b"abc".to_vec(),
            feature: Vec::new(),
            value_hash: [0x44; 32],
            kv_hash: [0x55; 32],
            left: None,
            right: None,
            hosted: Vec::new(),
            value: ModelValue::Inline(b"v".to_vec()),
            fragment_root: false,
        },
    );
    page
}

#[test]
fn golden_bytes() {
    let mut expected = vec![
        0x03, 0x00, // kind: tree page; reserved
        0xF7, 0x00, // length 247
        0x07, 0x00, 0x00, 0x00, // page 7
        0x03, 0x00, // 3 slots
        0x02, 0x00, // 2 cells
        0x00, 0x00, // no unused bytes
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 2 departures
        0x1C, 0x00, // slot 1: the cell at 28
        0x00, 0x00, // slot 2: free
        0xAB, 0x00, // slot 3: the cell at 171
    ];
    // The cell of slot 1.
    expected.extend([0x0D, 0x02, 0x02, 0x02]); // left link, out of line, fragment root; lengths
    expected.extend([0x2C, 0x01, 0x00, 0x00]); // value length 300
    expected.extend([0x11; 32]); // value hash
    expected.extend([0x22; 32]); // key-value hash
    expected.extend([0x07, 0x00, 0x00, 0x00, 0x03, 0x00]); // left child: page 7, slot 3
    expected.extend([0x33; 32]); // its hash
    expected.extend([0x01, 0x00, 0x03, 0x01, 0x05]); // heights, key length, aggregate
    expected.extend([0x09, 0x00, 0x00, 0x00, 0x01, 0x00]); // hosted root: page 9, slot 1
    expected.extend([0x00; 6]); // hosted root: none
    expected.extend([0x03, 0x2A]); // feature
    expected.extend(b"ab"); // key
    expected.extend([0x0C, 0x00, 0x00, 0x00]); // value in file 12
    expected.extend([0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // at offset 20
                                                                       // The cell of slot 3.
    expected.extend([0x00, 0x03, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
    expected.extend([0x44; 32]);
    expected.extend([0x55; 32]);
    expected.extend(b"abc");
    expected.extend(b"v");
    assert_eq!(expected.len(), 247);

    let model = example();
    assert_eq!(model.assemble(), expected);

    let size = TreePageSize::DEFAULT;
    let mut frame = vec![0; size.get()];
    let mut page = TreePageMut::init(&mut frame, size, page_id(7), 1).expect("fits");
    for raw in 1..=3 {
        let cell = model.cells.get(&raw).unwrap_or(&model.cells[&3]);
        page.insert(slot(raw), &cell.content()).expect("fits");
    }
    page.remove(slot(2)).expect("occupied");
    assert_eq!(page.page().to_image(), expected);

    let page = TreePage::decode(&expected, V1, size).expect("valid");
    model.assert_is(page, size);
    assert_eq!(
        page.find_value_pointer(&pointer(12, 20, 300)),
        Some(slot(1))
    );
    assert_eq!(page.fragment_roots().collect::<Vec<_>>(), [slot(1)]);
    page.check_fragments().expect("consistent");
}

#[test]
fn the_empty_page_is_its_header() {
    for size in SIZES {
        let mut frame = vec![0xEE; size.get()];
        let page = TreePageMut::init(&mut frame, size, page_id(0x0102_0304), 9).expect("fits");
        assert_eq!(page.free_bytes(), size.cell_capacity());
        let image = page.page().to_image();
        assert_eq!(
            image,
            [3, 0, 22, 0, 4, 3, 2, 1, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0]
        );
        let page = TreePage::decode(&image, V1, size).expect("valid");
        ModelPage::new(0x0102_0304, 9).assert_is(page, size);
        assert!(page.is_empty());
        assert_eq!(page.lowest_free_slot(), SlotId::MIN);
        assert!(page.cells().next().is_none());
    }
}

#[test]
fn sizes_are_the_documented_ones() {
    assert_eq!(TreePage::KIND_CODE, 3);
    assert!(FlatPageKind::ALL
        .iter()
        .all(|kind| kind.code() != TreePage::KIND_CODE));
    assert_eq!(TreePage::HEADER_LEN, 22);
    assert_eq!(TreePage::SLOT_LEN, 2);
    assert_eq!(TreePage::MAX_LEN, 65_535);
    assert_eq!(TreeCell::HEADER_LEN, 8);
    assert_eq!(TreeCell::FIXED_LEN, 72);
    assert_eq!(TreeLink::FIXED_LEN, 42);
    assert_eq!(TreeCell::HOSTED_ROOT_LEN, 6);
    assert_eq!(TreeCell::VALUE_LOCATION_LEN, 12);
    assert_eq!(HASH_LEN, 32);
    assert_eq!(TreeCell::MAX_OUT_OF_LINE_LEN, 2_718);

    // A leaf with a one-byte key and a one-byte value, and the same node
    // with two children, no aggregates and its value out of line.
    assert_eq!(TreeCell::len_of(1, 0, None, None, 0, 1), 74);
    assert_eq!(TreeCell::len_of(1, 0, Some(0), Some(0), 0, 12), 169);
    assert_eq!(TreeCell::len_of(0, 0, None, None, 1, 0), 78);
    assert_eq!(
        TreeCell::len_of(usize::MAX, 1, Some(usize::MAX), None, usize::MAX, 1),
        usize::MAX
    );
}

#[test]
fn a_page_size_is_8_to_64_kib() {
    assert_eq!(TreePageSize::MIN.get(), 8_192);
    assert_eq!(TreePageSize::DEFAULT.get(), 32_768);
    assert_eq!(TreePageSize::MAX.get(), 65_536);
    assert_eq!(TreePageSize::default(), TreePageSize::DEFAULT);
    for bytes in [0, 1, 4_096, 8_191, 65_537, usize::MAX] {
        assert_eq!(
            TreePageSize::new(bytes),
            Err(TreePageError::PageSizeOutOfRange { page_size: bytes }.into())
        );
    }
    for bytes in [8_192, 8_193, 10_000, 32_768, 65_535, 65_536] {
        let size = TreePageSize::new(bytes).expect("in range");
        assert_eq!(size.get(), bytes);
        assert_eq!(size.image_capacity(), bytes.min(65_535));
        assert_eq!(size.cell_capacity(), bytes.min(65_535) - 22);
        // The longest cell is the longest that, with its slot, is smaller
        // than half of what cells can use.
        let max = size.max_cell_len();
        assert!(2 * (max + TreePage::SLOT_LEN) < size.cell_capacity());
        assert!(2 * (max + 1 + TreePage::SLOT_LEN) >= size.cell_capacity());
        assert_eq!(size.max_inline_value_len(100), max - 100);
        assert_eq!(size.max_inline_value_len(max), 0);
        assert_eq!(size.max_inline_value_len(max + 1), 0);
    }
    assert_eq!(TreePageSize::DEFAULT.max_cell_len(), 16_370);
    assert_eq!(TreePageSize::MIN.max_cell_len(), 4_082);
    assert!(TreePageSize::MIN < TreePageSize::DEFAULT);
}

#[test]
fn every_error_has_its_message() {
    let slot = slot(4);
    let cases: [(TreePageError, &str); 27] = [
        (
            TreePageError::PageSizeOutOfRange { page_size: 512 },
            "page size 512 is outside 8192 to 65536",
        ),
        (
            TreePageError::BufferTooSmall {
                len: 10,
                needed: 8_192,
            },
            "buffer of 10 bytes for a page of 8192",
        ),
        (
            TreePageError::PageTooShort { len: 5 },
            "page length 5 is below the 22-byte header",
        ),
        (
            TreePageError::PageTooLong {
                len: 9_000,
                capacity: 8_192,
            },
            "page length 9000 exceeds the capacity of 8192",
        ),
        (
            TreePageError::DirectoryOutOfBounds {
                needed: 30,
                len: 22,
            },
            "slot directory ends at 30, past the page length 22",
        ),
        (
            TreePageError::TrailingFreeSlot,
            "the last slot of the directory is free",
        ),
        (
            TreePageError::InvalidSlot { slot },
            "slot 4 does not lead to a cell",
        ),
        (
            TreePageError::CellsOutOfOrder { slot },
            "the cell of slot 4 does not start where the cell before it ends",
        ),
        (
            TreePageError::UnusedBytes { used: 90, len: 99 },
            "cells end at 90, before the image length 99",
        ),
        (
            TreePageError::NotCompact { unused: 3 },
            "3 unused bytes among the cells of an image",
        ),
        (
            TreePageError::UnusedBytesMismatch {
                stated: 3,
                found: 4,
            },
            "page states 3 unused bytes among its cells, and has 4",
        ),
        (
            TreePageError::CellCountMismatch {
                stated: 3,
                found: 4,
            },
            "page states 3 cells, and has 4",
        ),
        (
            TreePageError::OverlappingCells { slot },
            "the cell of slot 4 starts inside another cell",
        ),
        (
            TreePageError::CellTooLarge {
                len: 5_000,
                max: 4_082,
            },
            "cell of 5000 bytes exceeds the maximum of 4082",
        ),
        (
            TreePageError::FieldTooLong {
                field: CellField::Key,
                len: 256,
                max: 255,
            },
            "key of 256 bytes exceeds the maximum of 255",
        ),
        (
            TreePageError::CellLengthMismatch {
                expected: 74,
                found: 75,
            },
            "cell of 74 bytes given as 75 bytes",
        ),
        (
            TreePageError::PageFull {
                needed: 80,
                available: 79,
            },
            "page full: need 80 bytes, 79 are free",
        ),
        (
            TreePageError::SlotOccupied { slot },
            "slot 4 already holds a cell",
        ),
        (TreePageError::SlotFree { slot }, "slot 4 holds no cell"),
        (
            TreePageError::NoSuchLink {
                slot,
                side: Side::Right,
            },
            "the cell of slot 4 has no right link",
        ),
        (
            TreePageError::NoSuchHostedRoot { slot, index: 2 },
            "the cell of slot 4 has no hosted-root pointer 2",
        ),
        (
            TreePageError::DanglingAddress { slot },
            "the cell of slot 4 points at no cell of its own page",
        ),
        (
            TreePageError::CellNamedTwice { slot },
            "two cells of the page point at the cell of slot 4",
        ),
        (
            TreePageError::ChildKeyLengthMismatch { slot },
            "a link of the cell of slot 4 states a wrong length for its child's key",
        ),
        (
            TreePageError::KeyChanged { slot },
            "the cell of slot 4 cannot be replaced by another key's cell",
        ),
        (
            TreePageError::FragmentRootMismatch { slot },
            "the fragment-root mark of slot 4 disagrees with the page's links",
        ),
        (
            TreePageError::FieldTooLong {
                field: CellField::InlineValue,
                len: 9,
                max: 8,
            },
            "inline value of 9 bytes exceeds the maximum of 8",
        ),
    ];
    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
        assert_eq!(
            FormatError::from(error).to_string(),
            format!("tree page: {message}")
        );
        let boxed: Box<dyn std::error::Error> = Box::new(error);
        assert_eq!(boxed.to_string(), message);
    }
    assert_eq!(Side::Left.to_string(), "left");
    assert_eq!(CellField::Feature.to_string(), "feature");
    assert_eq!(CellField::Aggregate.to_string(), "aggregate");
    assert_eq!(CellField::HostedRoots.to_string(), "hosted-root list");
}
