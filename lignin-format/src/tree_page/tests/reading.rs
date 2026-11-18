//! Reading: what `decode`, `check` and the accessors accept and reject.

use super::*;

const SLOT_1_AT: usize = 28;
const SLOT_3_AT: usize = 171;
const DIRECTORY_AT: usize = 22;

fn image() -> Vec<u8> {
    let image = example().assemble();
    assert_eq!(image.len(), 247);
    image
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn decode(bytes: &[u8]) -> Result<TreePage<'_>, FormatError> {
    TreePage::decode(bytes, V1, TreePageSize::DEFAULT)
}

fn decode_error(bytes: &[u8]) -> FormatError {
    decode(bytes).expect_err("not an image")
}

/// A page of `cells` cells of about 100 to 400 bytes.
fn random_model(rng: &mut Rng, id: u32, cells: usize) -> ModelPage {
    let mut model = ModelPage::new(id, rng.below(1000) as u64);
    for _ in 0..cells {
        let raw = match rng.chance(80) {
            true => model.lowest_free_slot(),
            false => model.slot_count() as u16 + 1 + rng.below(3) as u16,
        };
        model.cells.insert(raw, rng.cell(model.id, 200));
    }
    model
}

/// Calls every accessor. Nothing here may panic, whatever the bytes are.
fn exercise(bytes: &[u8]) {
    for size in [TreePageSize::MIN, TreePageSize::MAX] {
        if let Ok(page) = TreePage::decode(bytes, V1, size) {
            assert_eq!(page.to_image(), page.as_bytes());
            page.check(size).expect("an image is a valid page");
        }
    }
    let Ok(page) = TreePage::from_validated(bytes, V1) else {
        return;
    };
    let _ = page.check(TreePageSize::MAX);
    let _ = page.check_fragments();
    assert!(page.to_image().len() <= TreePage::MAX_LEN);
    let _ = page.lowest_free_slot();
    let _ = page.fragment_roots().count();
    let _ = page.find_value_pointer(&pointer(1, 20, 1));
    let _ = (
        page.id(),
        page.departures(),
        page.cell_count(),
        page.is_empty(),
    );
    let _ = (page.free_bytes(TreePageSize::MIN), page.used_bytes());
    let mut seen = 0;
    for (slot, cell) in page.cells().chain(page.cells().rev()) {
        seen += 1;
        assert!(slot.index() < page.slot_count());
        let _ = (cell.key(), cell.feature(), cell.value(), cell.value_len());
        let _ = (cell.value_hash(), cell.kv_hash(), cell.is_fragment_root());
        let _ = cell.hosted_roots().count() + cell.hosted_root_count();
        for field in HashField::ALL {
            let _ = cell.hash(field);
        }
        for side in Side::BOTH {
            let _ = cell.link(side).map(|link| link.encoded_len());
        }
        let _ = TreeCell::decode(cell.as_bytes(), V1);
    }
    assert!(seen <= 2 * page.slot_count());
    for raw in [1, 2, 3, u16::MAX] {
        let _ = page.cell(slot(raw));
        let _ = page.cell_by_address(CellAddress::new(page.id(), slot(raw)));
    }
}

#[test]
fn an_image_reads_back_and_later_bytes_are_ignored() {
    let image = image();
    let mut frame = image.clone();
    frame.resize(1000, 0xFF);
    for bytes in [&image, &frame] {
        let page = decode(bytes).expect("valid");
        assert_eq!(page.as_bytes(), image);
        example().assert_is(page, TreePageSize::DEFAULT);
        example().assert_is(view(bytes), TreePageSize::DEFAULT);
    }
}

#[test]
fn every_shorter_input_is_truncated() {
    let image = image();
    for len in 0..image.len() {
        let needed = if len < 22 { 22 } else { image.len() };
        let truncated = FormatError::Truncated {
            needed,
            available: len,
        };
        assert_eq!(decode_error(&image[..len]), truncated);
        assert_eq!(
            TreePage::from_validated(&image[..len], V1).expect_err("too short"),
            truncated
        );
    }
}

#[test]
fn zero_filled_bytes_are_not_a_page() {
    for len in 0..100 {
        let expected = match len < 22 {
            true => FormatError::Truncated {
                needed: 22,
                available: len,
            },
            false => FormatError::UnknownRecordKind { code: 0 },
        };
        assert_eq!(decode_error(&vec![0; len]), expected);
    }
}

#[test]
fn each_check_of_the_header_has_its_error() {
    for code in [0, 1, 2, 4, 255] {
        let mut bytes = image();
        bytes[0] = code;
        assert_eq!(
            decode_error(&bytes),
            FormatError::UnknownRecordKind { code: code.into() }
        );
    }

    let mut bytes = image();
    bytes[1] = 1;
    assert_eq!(decode_error(&bytes), FormatError::NonZeroReserved);

    for len in [0, 21] {
        let mut bytes = image();
        put16(&mut bytes, 2, len);
        assert_eq!(
            decode_error(&bytes),
            TreePageError::PageTooShort { len: len.into() }.into()
        );
    }

    let mut bytes = image();
    bytes[4..8].fill(0);
    assert_eq!(decode_error(&bytes), FormatError::MissingId);

    let mut bytes = image();
    put16(&mut bytes, 8, 200);
    assert_eq!(
        decode_error(&bytes),
        TreePageError::DirectoryOutOfBounds {
            needed: 422,
            len: 247
        }
        .into()
    );

    let mut bytes = image();
    put16(&mut bytes, 12, 4);
    assert_eq!(
        decode_error(&bytes),
        TreePageError::NotCompact { unused: 4 }.into()
    );

    let mut bytes = image();
    put16(&mut bytes, 10, 3);
    assert_eq!(
        decode_error(&bytes),
        TreePageError::CellCountMismatch {
            stated: 3,
            found: 2
        }
        .into()
    );
}

#[test]
fn a_page_longer_than_its_store_s_pages_is_refused() {
    let mut rng = Rng::new(0x10F6);
    let model = random_model(&mut rng, 5, 60);
    let image = model.assemble();
    assert!(image.len() > TreePageSize::MIN.get());
    model.assert_is(decode(&image).expect("valid"), TreePageSize::DEFAULT);
    let too_long: FormatError = TreePageError::PageTooLong {
        len: image.len(),
        capacity: 8_192,
    }
    .into();
    assert_eq!(
        TreePage::decode(&image, V1, TreePageSize::MIN).expect_err("too long"),
        too_long
    );
    assert_eq!(
        view(&image).check(TreePageSize::MIN).expect_err("too long"),
        too_long
    );
    let mut frame = image.clone();
    frame.resize(TreePageSize::DEFAULT.get(), 0);
    assert_eq!(
        TreePageMut::open(&mut frame, TreePageSize::MIN, V1).expect_err("too long"),
        too_long
    );
}

#[test]
fn each_check_of_the_slots_has_its_error() {
    let out_of_order = |raw| FormatError::from(TreePageError::CellsOutOfOrder { slot: slot(raw) });

    // The two cells under each other's slot.
    let mut bytes = image();
    put16(&mut bytes, DIRECTORY_AT, SLOT_3_AT as u16);
    put16(&mut bytes, DIRECTORY_AT + 4, SLOT_1_AT as u16);
    assert_eq!(decode_error(&bytes), out_of_order(1));

    // A slot that points into the directory, and one past the end.
    for offset in [1, 24, 27, 29, 247, 60_000] {
        let mut bytes = image();
        put16(&mut bytes, DIRECTORY_AT, offset);
        assert_eq!(decode_error(&bytes), out_of_order(1));
    }

    // The free slot in the middle pointing at the cell of the slot after it.
    let mut bytes = image();
    put16(&mut bytes, DIRECTORY_AT + 2, SLOT_3_AT as u16);
    put16(&mut bytes, 10, 3);
    assert_eq!(decode_error(&bytes), out_of_order(3));

    // The last cell says it is one byte longer than the image has room for.
    let mut bytes = image();
    bytes[SLOT_3_AT + 4] = 2;
    assert_eq!(
        decode_error(&bytes),
        TreePageError::InvalidSlot { slot: slot(3) }.into()
    );

    // The image ends one byte after its last cell.
    let mut bytes = image();
    bytes.push(0);
    put16(&mut bytes, 2, 248);
    assert_eq!(
        decode_error(&bytes),
        TreePageError::UnusedBytes {
            used: 247,
            len: 248
        }
        .into()
    );

    // The last slot of the directory is free.
    let mut bytes = image();
    bytes.truncate(SLOT_3_AT);
    put16(&mut bytes, 2, SLOT_3_AT as u16);
    put16(&mut bytes, DIRECTORY_AT + 4, 0);
    put16(&mut bytes, 10, 1);
    assert_eq!(decode_error(&bytes), TreePageError::TrailingFreeSlot.into());
    assert_eq!(
        view(&bytes)
            .check(TreePageSize::DEFAULT)
            .expect_err("a free last slot"),
        TreePageError::TrailingFreeSlot.into()
    );
}

/// The cell of slot 1 of the example, with the offsets of its parts.
mod at {
    pub(super) const FLAGS: usize = super::SLOT_1_AT;
    pub(super) const VALUE_LEN: usize = FLAGS + 4;
    pub(super) const LINK: usize = FLAGS + 72;
    pub(super) const HOSTED: usize = LINK + 43;
    pub(super) const LOCATION: usize = HOSTED + 12 + 2 + 2;
}

#[test]
fn each_check_of_a_cell_has_its_error() {
    let cases: [(&[(usize, u8)], FormatError); 11] = [
        (&[(at::FLAGS, 0x1D)], FormatError::NonZeroReserved),
        (&[(at::FLAGS, 0x8D)], FormatError::NonZeroReserved),
        // The left child's page id, then its slot.
        (&[(at::LINK, 0)], FormatError::MissingId),
        (&[(at::LINK + 4, 0)], FormatError::MissingId),
        // A hosted root with a slot and no page, and one with a page and no
        // slot.
        (&[(at::HOSTED, 0)], FormatError::MissingId),
        (&[(at::HOSTED + 6, 1)], FormatError::MissingId),
        (&[(at::HOSTED + 10, 1)], FormatError::MissingId),
        // The value pointer: no file, an offset inside the file header, no
        // length.
        (&[(at::LOCATION, 0)], FormatError::MissingId),
        (
            &[(at::LOCATION + 4, 19)],
            ValueLogError::InvalidOffset { offset: 19 }.into(),
        ),
        (
            &[(at::LOCATION + 11, 0x80)],
            ValueLogError::InvalidOffset {
                offset: 20 | 1 << 63,
            }
            .into(),
        ),
        (
            &[(at::VALUE_LEN, 0), (at::VALUE_LEN + 1, 0)],
            ValueLogError::EmptyValue.into(),
        ),
    ];
    for (changes, error) in cases {
        let mut bytes = image();
        for &(at, byte) in changes {
            bytes[at] = byte;
        }
        assert_eq!(decode_error(&bytes), error, "{changes:?}");
        assert_eq!(
            view(&bytes)
                .check(TreePageSize::DEFAULT)
                .expect_err("a damaged cell"),
            error
        );
        let cell = &bytes[SLOT_1_AT..SLOT_3_AT];
        assert_eq!(TreeCell::decode(cell, V1).expect_err("damaged"), error);
    }
}

#[test]
fn the_checks_run_in_their_documented_order() {
    // Each change breaks one check. With all later changes applied as well,
    // the error is that of the earliest.
    type Change = (fn(&mut Vec<u8>), FormatError);
    let changes: [Change; 8] = [
        (
            |bytes| bytes[0] = 9,
            FormatError::UnknownRecordKind { code: 9 },
        ),
        (|bytes| bytes[1] = 9, FormatError::NonZeroReserved),
        (|bytes| bytes[4..8].fill(0), FormatError::MissingId),
        (
            |bytes| put16(bytes, 12, 7),
            TreePageError::NotCompact { unused: 7 }.into(),
        ),
        (
            |bytes| bytes[SLOT_1_AT] |= 0x40,
            FormatError::NonZeroReserved,
        ),
        (
            |bytes| bytes[SLOT_3_AT + 4] = 200,
            TreePageError::InvalidSlot { slot: slot(3) }.into(),
        ),
        (
            |bytes| {
                bytes.push(0);
                put16(bytes, 2, 248);
            },
            TreePageError::UnusedBytes {
                used: 247,
                len: 248,
            }
            .into(),
        ),
        (
            |bytes| put16(bytes, 10, 9),
            TreePageError::CellCountMismatch {
                stated: 9,
                found: 2,
            }
            .into(),
        ),
    ];
    for first in 0..changes.len() {
        let mut bytes = image();
        for (change, _) in changes[first..].iter().rev() {
            change(&mut bytes);
        }
        assert_eq!(
            decode_error(&bytes),
            changes[first].1,
            "from change {first}"
        );
    }
}

#[test]
fn a_cell_reads_back_from_its_own_bytes() {
    let mut rng = Rng::new(0xCE11_0001);
    for _ in 0..2_000 {
        let cell = rng.cell(page_id(3), 300);
        let bytes = cell.content().encode().expect("valid");
        assert_eq!(bytes, cell.assemble());
        cell.assert_is(TreeCell::decode(&bytes, V1).expect("valid"));

        let mut appended = vec![7; 5];
        cell.content().encode_into(&mut appended).expect("valid");
        assert_eq!(appended[..5], [7; 5]);
        assert_eq!(appended[5..], bytes);

        let longer = [bytes.as_slice(), &[0]].concat();
        assert_eq!(
            TreeCell::decode(&longer, V1).expect_err("one byte too many"),
            TreePageError::CellLengthMismatch {
                expected: bytes.len(),
                found: longer.len()
            }
            .into()
        );
    }
}

#[test]
fn every_shorter_cell_is_truncated() {
    let mut rng = Rng::new(0xCE11_0002);
    for _ in 0..40 {
        let bytes = rng.cell(page_id(3), 60).assemble();
        for len in 0..bytes.len() {
            match TreeCell::decode(&bytes[..len], V1).expect_err("too short") {
                FormatError::Truncated { needed, available } => {
                    assert_eq!(available, len);
                    assert!(len < needed && needed <= bytes.len());
                }
                other => panic!("expected truncated, got {other:?}"),
            }
        }
    }
}

#[test]
fn a_part_longer_than_its_length_field_is_refused() {
    let long = vec![0; 256];
    let roots = vec![None; 256];
    let base = ModelCell::leaf(b"k", b"v");
    let link = |aggregate| TreeLink {
        child: address(1, 1),
        hash: &[0; 32],
        heights: (0, 0),
        child_key_len: 0,
        aggregate,
    };
    let too_long = |field| {
        FormatError::from(TreePageError::FieldTooLong {
            field,
            len: 256,
            max: 255,
        })
    };
    let cases = [
        (
            CellContent {
                key: &long,
                ..base.content()
            },
            too_long(CellField::Key),
        ),
        (
            CellContent {
                feature: &long,
                ..base.content()
            },
            too_long(CellField::Feature),
        ),
        (
            CellContent {
                left: Some(link(&long)),
                ..base.content()
            },
            too_long(CellField::Aggregate),
        ),
        (
            CellContent {
                right: Some(link(&long)),
                ..base.content()
            },
            too_long(CellField::Aggregate),
        ),
        (
            CellContent {
                hosted_roots: &roots,
                ..base.content()
            },
            too_long(CellField::HostedRoots),
        ),
    ];
    let size = TreePageSize::DEFAULT;
    let mut frame = frame_of(&example(), size);
    let before = frame.clone();
    for (content, error) in cases {
        assert_eq!(content.encode().expect_err("too long"), error);
        let mut out = vec![1, 2, 3];
        assert_eq!(content.encode_into(&mut out).expect_err("too long"), error);
        assert_eq!(out, [1, 2, 3]);
        let mut page = open(&mut frame, size);
        assert_eq!(page.insert(slot(2), &content).expect_err("too long"), error);
        assert_eq!(
            page.replace(slot(1), &content).expect_err("too long"),
            error
        );
    }
    let mut page = open(&mut frame, size);
    assert_eq!(
        page.set_feature(slot(1), &long).expect_err("too long"),
        too_long(CellField::Feature)
    );
    assert_eq!(
        page.set_link(slot(1), Side::Right, Some(&link(&long)))
            .expect_err("too long"),
        too_long(CellField::Aggregate)
    );
    assert_eq!(
        page.set_hosted_roots(slot(1), &roots)
            .expect_err("too long"),
        too_long(CellField::HostedRoots)
    );
    assert_eq!(frame, before);

    // The longest of every part is accepted.
    let longest = vec![0x5A; 255];
    let mut cell = base;
    cell.key = longest.clone();
    cell.feature = longest.clone();
    cell.hosted = vec![Some(address(4, 4)); 255];
    for side in Side::BOTH {
        *cell.link_mut(side) = Some(ModelLink {
            child: address(8, 8),
            hash: [3; 32],
            heights: (255, 255),
            child_key_len: 255,
            aggregate: longest.clone(),
        });
    }
    cell.value = ModelValue::OutOfLine(pointer(u32::MAX, i64::MAX as u64, u32::MAX));
    let bytes = cell.content().encode().expect("valid");
    assert_eq!(bytes.len(), TreeCell::MAX_OUT_OF_LINE_LEN);
    cell.assert_is(TreeCell::decode(&bytes, V1).expect("valid"));
}

#[test]
fn a_value_in_a_cell_is_at_most_what_its_length_field_can_say() {
    use super::super::cell::inline_value_len;

    assert_eq!(inline_value_len(0), Ok(0));
    assert_eq!(inline_value_len(u32::MAX as usize), Ok(u32::MAX));
    #[cfg(target_pointer_width = "64")]
    assert_eq!(
        inline_value_len(u32::MAX as usize + 1),
        Err(TreePageError::FieldTooLong {
            field: CellField::InlineValue,
            len: u32::MAX as usize + 1,
            max: u32::MAX as usize,
        }
        .into())
    );
}

#[test]
fn every_single_bit_flip_is_rejected_or_reads_as_what_it_says() {
    let mut rng = Rng::new(0xF11B);
    for image in [image(), random_model(&mut rng, 77, 6).assemble()] {
        let mut accepted = 0;
        for bit in 0..image.len() * 8 {
            let mut flipped = image.clone();
            flipped[bit / 8] ^= 1 << (bit % 8);
            exercise(&flipped);
            if let Ok(page) = decode(&flipped) {
                // One encoding per content: what was read is exactly what
                // these bytes say, and it encodes back to them.
                assert_eq!(page.to_image(), flipped);
                page.check(TreePageSize::DEFAULT).expect("valid");
                accepted += 1;
            }
        }
        // Keys, values, hashes and counts are content, so most flips give
        // another valid page. A flip in a length or an offset never does.
        assert!(accepted > image.len() * 4);
        for at in [2, 3, 8, 9, 10, 11, 12, 13, DIRECTORY_AT, DIRECTORY_AT + 1] {
            for bit in 0..8 {
                let mut flipped = image.clone();
                flipped[at] ^= 1 << bit;
                decode(&flipped).expect_err("a damaged length or offset");
            }
        }
    }
}

#[test]
fn arbitrary_bytes_are_an_error_or_a_page_and_never_a_panic() {
    let mut rng = Rng::new(0xB10);
    let model = random_model(&mut rng, 21, 8);
    let valid = model.assemble();
    for round in 0..60_000 {
        let mut bytes = match round % 3 {
            // Arbitrary bytes.
            0 => {
                let len = rng.below(400);
                rng.bytes(len)
            }
            // Arbitrary bytes behind a header that passes.
            1 => {
                let len = 22 + rng.below(600);
                let mut bytes = rng.bytes(len);
                bytes[0] = 3;
                bytes[1] = 0;
                let stated = 22 + rng.below(len - 21);
                put16(&mut bytes, 2, stated as u16);
                let slots = rng.below((stated - 22) / 2 + 1).min(rng.below(12));
                put16(&mut bytes, 8, slots as u16);
                let shared = 22 + 2 * slots + rng.below(stated);
                for index in 0..slots {
                    if rng.chance(70) {
                        let offset = match rng.chance(30) {
                            true => shared,
                            false => 22 + 2 * slots + rng.below(stated),
                        };
                        put16(&mut bytes, 22 + 2 * index, offset as u16);
                    }
                }
                bytes
            }
            // A valid image with a few bytes changed.
            _ => {
                let mut bytes = valid.clone();
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(bytes.len());
                    bytes[at] = rng.below(256) as u8;
                }
                bytes
            }
        };
        exercise(&bytes);
        let _ = TreeCell::decode(&bytes, V1);
        if bytes.len() >= 72 {
            bytes[0] &= 0x0F;
            if let Ok(cell) = TreeCell::decode(&bytes, V1) {
                ModelCell::read(cell).assert_is(cell);
            }
        }
    }
}

#[test]
fn a_page_whose_slots_share_a_cell_is_written_out_within_a_page() {
    // Bytes that were never checked: many slots lead to one large cell.
    // Writing each slot's cell out would pass the longest page many times.
    for (slots, value_len) in [(100, 900), (16_000, 30_000), (16_378, 32_679)] {
        let cell = ModelCell::leaf(b"shared", &vec![0x5A; value_len]).assemble();
        let cells_at = 22 + 2 * slots;
        let mut bytes = ModelPage::new(4, 0).assemble();
        bytes.resize(cells_at, 0);
        bytes.extend(&cell);
        assert!(bytes.len() <= TreePage::MAX_LEN);
        put16(&mut bytes, 2, (cells_at + cell.len()) as u16);
        put16(&mut bytes, 8, slots as u16);
        put16(&mut bytes, 10, slots as u16);
        for index in 0..slots {
            put16(&mut bytes, 22 + 2 * index, cells_at as u16);
        }
        TreePage::decode(&bytes, V1, TreePageSize::MAX).expect_err("not an image");
        let page = view(&bytes);
        page.check(TreePageSize::MAX).expect_err("not a valid page");
        if slots == 100 {
            assert_eq!(
                decode_error(&bytes),
                TreePageError::CellsOutOfOrder { slot: slot(2) }.into()
            );
            assert_eq!(
                page.check(TreePageSize::MAX),
                Err(TreePageError::OverlappingCells { slot: slot(2) }.into())
            );
        }
        assert_eq!(page.cells().count(), slots);
        let image = page.to_image();
        assert!(image.len() <= TreePage::MAX_LEN);
        assert!(image.len() >= cells_at + cell.len());
        exercise(&bytes);
    }
}

#[test]
fn a_page_with_unused_bytes_is_valid_but_is_not_an_image() {
    let image = image();
    let mut bytes = image[..SLOT_3_AT].to_vec();
    bytes.extend([0xEE; 4]);
    bytes.extend(&image[SLOT_3_AT..]);
    put16(&mut bytes, 2, 251);
    put16(&mut bytes, DIRECTORY_AT + 4, SLOT_3_AT as u16 + 4);
    put16(&mut bytes, 12, 4);
    let page = view(&bytes);
    example().assert_is(page, TreePageSize::DEFAULT);
    assert_eq!(page.unused_bytes(), 4);
    assert_eq!(page.encoded_len(), 251);
    assert_eq!(page.to_image(), image);
    assert_eq!(
        decode_error(&bytes),
        TreePageError::NotCompact { unused: 4 }.into()
    );

    put16(&mut bytes, 12, 0);
    assert_eq!(
        decode_error(&bytes),
        TreePageError::CellsOutOfOrder { slot: slot(3) }.into()
    );
    assert_eq!(
        view(&bytes)
            .check(TreePageSize::DEFAULT)
            .expect_err("a wrong count"),
        TreePageError::UnusedBytesMismatch {
            stated: 0,
            found: 4
        }
        .into()
    );
}

#[test]
fn each_check_of_a_page_in_a_buffer_has_its_error() {
    let check = |bytes: &[u8]| {
        view(bytes)
            .check(TreePageSize::DEFAULT)
            .expect_err("not a valid page")
    };

    // Two slots with the same cell.
    let mut bytes = image();
    put16(&mut bytes, DIRECTORY_AT + 2, SLOT_1_AT as u16);
    put16(&mut bytes, 10, 3);
    assert_eq!(
        check(&bytes),
        TreePageError::OverlappingCells { slot: slot(2) }.into()
    );

    // A slot that points into the header or the directory.
    for offset in [1, 21, 27] {
        let mut bytes = image();
        put16(&mut bytes, DIRECTORY_AT + 2, offset);
        assert_eq!(
            check(&bytes),
            TreePageError::InvalidSlot { slot: slot(2) }.into()
        );
        assert!(view(&bytes).cell(slot(2)).is_none());
    }

    // A slot whose cell would run past the end of the page.
    for offset in [246, 247, 9_000] {
        let mut bytes = image();
        put16(&mut bytes, DIRECTORY_AT + 2, offset);
        assert_eq!(
            check(&bytes),
            TreePageError::InvalidSlot { slot: slot(2) }.into()
        );
        assert!(view(&bytes).cell(slot(2)).is_none());
    }

    let mut bytes = image();
    put16(&mut bytes, 10, 1);
    assert_eq!(
        check(&bytes),
        TreePageError::CellCountMismatch {
            stated: 1,
            found: 2
        }
        .into()
    );

    let mut bytes = image();
    put16(&mut bytes, 12, 9);
    assert_eq!(
        check(&bytes),
        TreePageError::UnusedBytesMismatch {
            stated: 9,
            found: 0
        }
        .into()
    );
}

/// The key length a link of `cell` states for the child at `address`, when
/// it is a link and not a hosted-root pointer that names it first.
fn key_len_of(cell: &ModelCell, address: CellAddress) -> Option<u8> {
    [&cell.left, &cell.right]
        .into_iter()
        .flatten()
        .find(|link| link.child == address)
        .map(|link| link.child_key_len)
}

/// What `check_fragments` must say about `model`, worked out from the model.
fn expected_fragment_check(model: &ModelPage) -> Result<(), TreePageError> {
    let mut named = Vec::new();
    for (&raw, cell) in &model.cells {
        let children = [&cell.left, &cell.right]
            .into_iter()
            .flatten()
            .map(|link| link.child);
        for address in children.chain(cell.hosted.iter().flatten().copied()) {
            if address.page() != model.id {
                continue;
            }
            let target = address.slot().get();
            if target == raw || !model.cells.contains_key(&target) {
                return Err(TreePageError::DanglingAddress { slot: slot(raw) });
            }
            let key_len = model.cells[&target].key.len();
            if key_len_of(cell, address).is_some_and(|len| usize::from(len) != key_len) {
                return Err(TreePageError::ChildKeyLengthMismatch { slot: slot(raw) });
            }
            if named.contains(&target) {
                return Err(TreePageError::CellNamedTwice { slot: slot(target) });
            }
            named.push(target);
        }
    }
    for (&raw, cell) in &model.cells {
        if cell.fragment_root == named.contains(&raw) {
            return Err(TreePageError::FragmentRootMismatch { slot: slot(raw) });
        }
    }
    Ok(())
}

/// A page of four cells in two fragments, with links that leave the page.
fn fragments() -> ModelPage {
    let mut model = ModelPage::new(6, 0);
    let link = |child| {
        Some(ModelLink {
            child,
            hash: [9; 32],
            heights: (1, 1),
            child_key_len: 1,
            aggregate: Vec::new(),
        })
    };
    let mut top = ModelCell::leaf(b"m", b"1");
    top.left = link(address(6, 2));
    top.right = link(address(40, 3));
    top.fragment_root = true;
    let mut host = ModelCell::leaf(b"d", b"2");
    host.hosted = vec![Some(address(6, 3)), None, Some(address(41, 1))];
    let hosted = ModelCell::leaf(b"a", b"3");
    let mut alone = ModelCell::leaf(b"z", b"4");
    alone.fragment_root = true;
    model
        .cells
        .extend([(1, top), (2, host), (3, hosted), (5, alone)]);
    model
}

#[test]
fn the_fragment_roots_are_the_cells_no_cell_of_the_page_points_at() {
    let model = fragments();
    let image = model.assemble();
    let page = decode(&image).expect("valid");
    page.check_fragments().expect("consistent");
    assert_eq!(model.unnamed(), [1, 5]);
    assert_eq!(
        page.fragment_roots().map(SlotId::get).collect::<Vec<_>>(),
        model.unnamed()
    );
    assert!(page.cell_by_address(address(6, 3)).is_some());
    assert!(page.cell_by_address(address(6, 4)).is_none());
    assert!(page.cell_by_address(address(7, 3)).is_none());

    let check = |model: &ModelPage| {
        let image = model.assemble();
        let result = decode(&image)
            .expect("the image is valid whatever its links say")
            .check_fragments()
            .map_err(tree_page_error);
        assert_eq!(result, expected_fragment_check(model));
        result.expect_err("inconsistent")
    };
    let mismatch = |raw| TreePageError::FragmentRootMismatch { slot: slot(raw) };
    let dangling = |raw| TreePageError::DanglingAddress { slot: slot(raw) };

    // A mark too many, and a mark missing.
    for raw in [1, 2, 3, 5] {
        let mut changed = model.clone();
        let cell = changed.cells.get_mut(&raw).expect("a cell");
        cell.fragment_root = !cell.fragment_root;
        assert_eq!(check(&changed), mismatch(raw));
    }

    // A link to a free slot, to a slot past the directory, and to itself.
    for target in [4, 6, 400, u16::MAX, 1] {
        let mut changed = model.clone();
        let top = changed.cells.get_mut(&1).expect("a cell");
        top.left.as_mut().expect("a link").child = address(6, target);
        assert_eq!(check(&changed), dangling(1));
    }
    let mut changed = model.clone();
    changed.cells.get_mut(&2).expect("a cell").hosted[1] = Some(address(6, 2));
    assert_eq!(check(&changed), dangling(2));

    // A link that states another length than its child's key has. The
    // length of a child in another page cannot be checked here.
    for key_len in [0, 2, 255] {
        let mut changed = model.clone();
        let top = changed.cells.get_mut(&1).expect("a cell");
        top.left.as_mut().expect("a link").child_key_len = key_len;
        assert_eq!(
            check(&changed),
            TreePageError::ChildKeyLengthMismatch { slot: slot(1) }
        );
    }
    let mut changed = model.clone();
    let top = changed.cells.get_mut(&1).expect("a cell");
    top.right.as_mut().expect("a link").child_key_len = 77;
    decode(&changed.assemble())
        .expect("valid")
        .check_fragments()
        .expect("consistent");

    // A cell that two cells point at.
    let mut changed = model.clone();
    let top = changed.cells.get_mut(&1).expect("a cell");
    top.right.as_mut().expect("a link").child = address(6, 3);
    assert_eq!(
        check(&changed),
        TreePageError::CellNamedTwice { slot: slot(3) }
    );
    let mut changed = model.clone();
    changed.cells.get_mut(&2).expect("a cell").hosted[2] = Some(address(6, 3));
    assert_eq!(
        check(&changed),
        TreePageError::CellNamedTwice { slot: slot(3) }
    );
}

#[test]
fn the_fragment_check_agrees_with_the_model_on_random_pages() {
    let mut rng = Rng::new(0xF4A6);
    let (mut consistent, mut inconsistent) = (0, 0);
    for round in 0..3_000 {
        let cells = 1 + rng.below(9);
        let mut model = ModelPage::new(6, 0);
        for raw in 1..=cells as u16 {
            let mut cell = ModelCell::leaf(&[raw as u8], b"v");
            if rng.chance(20) {
                cell.hosted = vec![None; 2];
            }
            model.cells.insert(raw + (raw > 4) as u16, cell);
        }
        // Hang every cell but the first on an earlier one, where a place is
        // free. Sometimes point somewhere else instead.
        let slots: Vec<u16> = model.cells.keys().copied().collect();
        for (position, &raw) in slots.iter().enumerate().skip(1) {
            let parent = slots[rng.below(position)];
            let child = match rng.below(40) {
                0 => address(6, 1 + rng.below(12) as u16),
                1 => address(9, raw),
                _ => address(6, raw),
            };
            let cell = model.cells.get_mut(&parent).expect("a cell");
            let link = ModelLink {
                child,
                hash: rng.hash(),
                heights: (0, 0),
                child_key_len: 1 + u8::from(rng.chance(3)),
                aggregate: Vec::new(),
            };
            if cell.left.is_none() {
                cell.left = Some(link);
            } else if cell.right.is_none() {
                cell.right = Some(link);
            } else if let Some(root) = cell.hosted.iter_mut().find(|root| root.is_none()) {
                *root = Some(child);
            }
        }
        for raw in model.unnamed() {
            model.cells.get_mut(&raw).expect("a cell").fragment_root = true;
        }
        if round % 4 == 0 {
            let raw = slots[rng.below(slots.len())];
            let cell = model.cells.get_mut(&raw).expect("a cell");
            cell.fragment_root = !cell.fragment_root;
        }
        let image = model.assemble();
        let result = decode(&image)
            .expect("valid")
            .check_fragments()
            .map_err(tree_page_error);
        assert_eq!(result, expected_fragment_check(&model));
        match result {
            Ok(()) => consistent += 1,
            Err(_) => inconsistent += 1,
        }
    }
    assert!(consistent > 500 && inconsistent > 500);
}

#[test]
fn the_cell_that_holds_a_value_pointer_is_found_by_the_pointer() {
    let mut model = ModelPage::new(8, 0);
    let pointers = [
        pointer(3, 20, 500),
        pointer(3, 539, 500),
        pointer(4, 20, 500),
        pointer(3, 1 << 40, 1),
    ];
    for (index, pointer) in pointers.iter().enumerate() {
        let mut cell = ModelCell::leaf(&[index as u8], b"");
        cell.value = ModelValue::OutOfLine(*pointer);
        model.cells.insert(2 * index as u16 + 1, cell);
    }
    // A value in its cell whose bytes are those a cell stores for a value
    // pointer, and one that is a whole pointer.
    let mut location = pointers[0].encode().to_vec();
    model.cells.insert(2, ModelCell::leaf(b"whole", &location));
    location.truncate(12);
    model.cells.insert(4, ModelCell::leaf(b"place", &location));

    let image = model.assemble();
    let page = decode(&image).expect("valid");
    for (index, pointer) in pointers.iter().enumerate() {
        assert_eq!(
            page.find_value_pointer(pointer),
            Some(slot(2 * index as u16 + 1))
        );
    }
    for absent in [
        pointer(3, 20, 499),
        pointer(3, 21, 500),
        pointer(5, 20, 500),
        pointer(4, 539, 500),
    ] {
        assert_eq!(page.find_value_pointer(&absent), None);
    }
    let empty = ModelPage::new(8, 0).assemble();
    assert_eq!(view(&empty).find_value_pointer(&pointers[0]), None);
}

#[test]
fn a_hash_is_read_from_bytes_that_are_not_the_key_or_the_value() {
    let mut rng = Rng::new(0x4A54);
    for size in SIZES {
        let model = random_model(&mut rng, 31, 20);
        let frame = frame_of(&model, size);
        let page = view(&frame);

        // The same page with every key and every value in a cell overwritten.
        let mut scrambled = frame.clone();
        for (slot, cell) in page.cells() {
            let start = page.cell_range(slot).expect("a cell").start;
            let (value_start, value_end) = cell.value_extent();
            let key_start = value_start - cell.key().len();
            let end = match cell.value() {
                CellValue::Inline(_) => value_end,
                CellValue::OutOfLine(_) => value_start,
            };
            for byte in &mut scrambled[start + key_start..start + end] {
                *byte = !*byte;
            }
        }
        let other = view(&scrambled);

        for (slot, cell) in page.cells() {
            let start = page.cell_range(slot).expect("a cell").start;
            let (value_start, _) = cell.value_extent();
            let twin = other.cell(slot).expect("still a cell");
            for field in HashField::ALL {
                let Ok(at) = cell.hash_at(field) else {
                    assert_eq!(cell.hash(field), None);
                    assert_eq!(twin.hash(field), None);
                    continue;
                };
                // The hash is 32 bytes at an offset that the cell's header
                // and links fix, before the key and the value.
                assert!(at + HASH_LEN <= value_start - cell.key().len());
                let stored = &frame[start + at..start + at + HASH_LEN];
                assert_eq!(cell.hash(field).expect("stored").as_slice(), stored);
                assert_eq!(twin.hash(field), cell.hash(field));
            }
            assert_eq!(twin.value_hash(), cell.value_hash());
            assert_eq!(twin.kv_hash(), cell.kv_hash());
            for side in Side::BOTH {
                assert_eq!(twin.link(side), cell.link(side));
            }
        }
    }
}
