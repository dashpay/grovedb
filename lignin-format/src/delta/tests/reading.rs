//! Reading a run: every check, and every byte string.

use super::*;

fn refused(bytes: &[u8]) -> FormatError {
    DeltaRun::decode(bytes, V1).expect_err("not a run")
}

/// A run of page 7 that holds `record` and nothing else.
fn with_record(record: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7, 0, 0, 0];
    bytes.extend(record);
    bytes
}

fn truncated(needed: usize, available: usize) -> FormatError {
    FormatError::Truncated { needed, available }
}

/// The bytes of a run that holds what `run` holds, written again.
fn rewritten(run: &DeltaRun<'_>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = DeltaRunWriter::new(run.page(), run.created(), &mut bytes);
    for delta in run.deltas() {
        writer.push(&delta).expect("what was read is written");
    }
    writer.finish().expect("a run");
    bytes
}

#[test]
fn each_check_of_a_run_has_its_error() {
    // The page id.
    for len in 0..4 {
        assert_eq!(refused(&[7, 0, 0, 0][..len]), truncated(4, len));
    }
    assert_eq!(refused(&[0, 0, 0, 0, 4, 1, 0]), FormatError::MissingId);
    assert_eq!(refused(&[0; 64]), FormatError::MissingId);

    // At least one record.
    assert_eq!(delta_error(refused(&[7, 0, 0, 0])), DeltaError::EmptyRun);

    // A create record is the first of its run.
    let create = [1, 5, 0, 0, 0, 0, 0, 0, 0];
    let remove = [4, 1, 0];
    assert_eq!(decode(&with_record(&create)).created(), Some(5));
    for len in 1..create.len() {
        assert_eq!(
            refused(&with_record(&create[..len])),
            truncated(13, 4 + len)
        );
    }
    assert_eq!(
        delta_error(refused(&with_record(
            &[remove.as_slice(), &create].concat()
        ))),
        DeltaError::MisplacedCreate
    );
    assert_eq!(
        delta_error(refused(&with_record(&[create, create].concat()))),
        DeltaError::MisplacedCreate
    );
    assert_eq!(
        delta_error(refused(&with_record(
            &[create.as_slice(), &remove, &create].concat()
        ))),
        DeltaError::MisplacedCreate
    );

    // What follows the last record is read as a record.
    let trailing = with_record(&[remove.as_slice(), &[0]].concat());
    assert_eq!(
        refused(&trailing),
        FormatError::UnknownRecordKind { code: 0 }
    );
    let trailing = with_record(&[remove.as_slice(), &[4, 1]].concat());
    assert_eq!(refused(&trailing), truncated(10, 9));
}

#[test]
fn every_kind_code_is_a_record_or_an_error() {
    for code in 0..=u8::MAX {
        let mut record = vec![code];
        record.extend([0; 80]);
        let bytes = with_record(&record);
        let result = DeltaRun::decode(&bytes, V1);
        let unknown = FormatError::UnknownRecordKind {
            code: u32::from(code),
        };
        match code {
            // The kind is checked before the slot, which is 0 here.
            2..=21 => assert_eq!(result, Err(FormatError::MissingId)),
            // The create record takes 8 bytes, and the next kind is 0.
            1 => assert_eq!(result, Err(FormatError::UnknownRecordKind { code: 0 })),
            _ => {
                assert_eq!(result, Err(unknown));
                // Also with nothing behind it.
                assert_eq!(refused(&with_record(&[code])), unknown);
            }
        }
    }
}

#[test]
fn each_check_of_a_record_has_its_error() {
    // The slot, for every kind that has one.
    for code in 2..=21 {
        assert_eq!(refused(&with_record(&[code])), truncated(7, 5));
        assert_eq!(refused(&with_record(&[code, 1])), truncated(7, 6));
        assert_eq!(refused(&with_record(&[code, 0, 0])), FormatError::MissingId);
    }

    // A hash is 32 bytes.
    for code in 5..=8 {
        let mut record = vec![code, 1, 0];
        record.extend([9; 31]);
        assert_eq!(refused(&with_record(&record)), truncated(39, 38));
        record.push(9);
        assert_eq!(decode(&with_record(&record)).deltas().len(), 1);
    }

    // A child's address is a whole address.
    for code in [9, 10] {
        let address = |bytes: [u8; 6]| with_record(&[[code, 1, 0].as_slice(), &bytes].concat());
        assert_eq!(refused(&address([0; 6])), FormatError::MissingId);
        assert_eq!(
            refused(&address([7, 0, 0, 0, 0, 0])),
            FormatError::MissingId
        );
        assert_eq!(
            refused(&address([0, 0, 0, 0, 2, 0])),
            FormatError::MissingId
        );
        assert_eq!(
            refused(&address([7, 0, 0, 0, 2, 0])[..12]),
            truncated(13, 12)
        );
        assert_eq!(decode(&address([7, 0, 0, 0, 2, 0])).deltas().len(), 1);
    }

    // A link: the address, the hash, the four bytes behind it, the
    // aggregate.
    for code in [11, 12] {
        let link = |address: [u8; 6], aggregate_len: u8, aggregate: &[u8]| {
            let mut record = vec![code, 1, 0];
            record.extend(address);
            record.extend([3; 32]);
            record.extend([1, 2, 3, aggregate_len]);
            record.extend(aggregate);
            with_record(&record)
        };
        let child = [7, 0, 0, 0, 2, 0];
        assert_eq!(refused(&link([0; 6], 0, &[])), FormatError::MissingId);
        assert_eq!(
            refused(&link([7, 0, 0, 0, 0, 0], 0, &[])),
            FormatError::MissingId
        );
        let whole = link(child, 2, &[5, 6]);
        assert_eq!(whole.len(), 4 + 3 + 42 + 2);
        assert_eq!(decode(&whole).deltas().len(), 1);
        assert_eq!(refused(&link(child, 2, &[5])), truncated(51, 50));
        assert_eq!(refused(&link(child, 255, &[5; 254])), truncated(304, 303));
        for len in 7..whole.len() {
            let needed = match len {
                7..13 => 13,
                13..45 => 45,
                45..49 => 49,
                _ => 51,
            };
            assert_eq!(refused(&whole[..len]), truncated(needed, len));
        }
    }

    // One hosted root: a position, and an address or none.
    let root = |bytes: [u8; 6]| with_record(&[[17, 1, 0, 3].as_slice(), &bytes].concat());
    assert_eq!(decode(&root([0; 6])).deltas().len(), 1);
    assert_eq!(decode(&root([7, 0, 0, 0, 2, 0])).deltas().len(), 1);
    assert_eq!(refused(&root([7, 0, 0, 0, 0, 0])), FormatError::MissingId);
    assert_eq!(refused(&root([0, 0, 0, 0, 2, 0])), FormatError::MissingId);
    assert_eq!(refused(&with_record(&[17, 1, 0])), truncated(8, 7));
    assert_eq!(refused(&root([0; 6])[..13]), truncated(14, 13));

    // All hosted roots: a count, and that many.
    let roots =
        |count: u8, fields: &[u8]| with_record(&[[18, 1, 0, count].as_slice(), fields].concat());
    assert_eq!(decode(&roots(0, &[])).deltas().len(), 1);
    assert_eq!(refused(&with_record(&[18, 1, 0])), truncated(8, 7));
    assert_eq!(refused(&roots(2, &[0; 11])), truncated(20, 19));
    assert_eq!(decode(&roots(2, &[0; 12])).deltas().len(), 1);
    let half = [0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0];
    assert_eq!(refused(&roots(2, &half)), FormatError::MissingId);
    assert_eq!(refused(&roots(255, &[0; 1529])), truncated(1538, 1537));

    // A feature: a length, and that many bytes.
    let feature =
        |len: u8, bytes: &[u8]| with_record(&[[19, 1, 0, len].as_slice(), bytes].concat());
    assert_eq!(decode(&feature(0, &[])).deltas().len(), 1);
    assert_eq!(refused(&with_record(&[19, 1, 0])), truncated(8, 7));
    assert_eq!(refused(&feature(5, &[1; 4])), truncated(13, 12));
    assert_eq!(refused(&feature(255, &[1; 254])), truncated(263, 262));

    // A value: a length, and that many bytes. No cell holds a value above
    // the longest value of the longest cell, and that is checked before the
    // bytes are asked for.
    let value = |len: usize, bytes: &[u8]| {
        let mut record = vec![20, 1, 0];
        record.extend((len as u16).to_le_bytes());
        record.extend(bytes);
        with_record(&record)
    };
    assert_eq!(decode(&value(0, &[])).deltas().len(), 1);
    assert_eq!(refused(&with_record(&[20, 1, 0, 0])), truncated(9, 8));
    assert_eq!(refused(&value(3, &[1; 2])), truncated(12, 11));
    let longest = vec![1; MAX_INLINE_VALUE_LEN];
    assert_eq!(decode(&value(longest.len(), &longest)).deltas().len(), 1);
    assert_eq!(
        refused(&value(longest.len(), &longest[1..])),
        truncated(9 + longest.len(), 8 + longest.len())
    );
    for len in [MAX_INLINE_VALUE_LEN + 1, usize::from(u16::MAX)] {
        assert_eq!(
            refused(&value(len, &[1; 2])),
            TreePageError::FieldTooLong {
                field: CellField::InlineValue,
                len,
                max: MAX_INLINE_VALUE_LEN,
            }
            .into()
        );
    }

    // A value pointer: file id, offset, value length.
    let install = |file: u32, offset: u64, value_len: u32| {
        let mut record = vec![21, 1, 0];
        record.extend(file.to_le_bytes());
        record.extend(offset.to_le_bytes());
        record.extend(value_len.to_le_bytes());
        with_record(&record)
    };
    assert_eq!(decode(&install(13, 64, 300)).deltas().len(), 1);
    assert_eq!(refused(&install(13, 64, 300)[..22]), truncated(23, 22));
    assert_eq!(refused(&install(0, 64, 300)), FormatError::MissingId);
    assert_eq!(refused(&install(0, 0, 0)), FormatError::MissingId);
    assert_eq!(
        refused(&install(13, 19, 0)),
        ValueLogError::InvalidOffset { offset: 19 }.into()
    );
    assert_eq!(
        refused(&install(13, 1 << 63, 300)),
        ValueLogError::InvalidOffset { offset: 1 << 63 }.into()
    );
    assert_eq!(
        refused(&install(13, 64, 0)),
        ValueLogError::EmptyValue.into()
    );

    // The same record with the reserved file id carries a pending value:
    // the eight bytes are its id, which is any number but zero.
    for id in [1, 19, 64, 1 << 63, u64::MAX] {
        let bytes = install(u32::MAX, id, 300);
        let run = decode(&bytes);
        assert_eq!(
            run.deltas().next(),
            Some(Delta::Value {
                slot: slot(1),
                value: CellValue::Pending(pending(id, 300)),
            })
        );
    }
    assert_eq!(refused(&install(u32::MAX, 0, 300)), FormatError::MissingId);
    assert_eq!(refused(&install(u32::MAX, 0, 0)), FormatError::MissingId);
    assert_eq!(
        refused(&install(u32::MAX, 64, 0)),
        ValueLogError::EmptyValue.into()
    );
    assert_eq!(
        refused(&install(u32::MAX, 64, 300)[..22]),
        truncated(23, 22)
    );
}

#[test]
fn each_check_of_a_cell_in_a_record_has_its_error() {
    let cell = ModelCell {
        hosted: vec![Some(address(9, 1))],
        left: Some(ModelLink {
            child: address(7, 3),
            hash: [3; 32],
            heights: (1, 0),
            child_key_len: 3,
            aggregate: vec![5],
        }),
        ..ModelCell::leaf(b"key", b"value")
    };
    let cell_bytes = cell.assemble();
    let cell_len = cell_bytes.len();
    assert_eq!(cell_len, 129);
    for code in [2, 3] {
        let record = |stated: usize, cell: &[u8]| {
            let mut record = vec![code, 1, 0];
            record.extend((stated as u16).to_le_bytes());
            record.extend(cell);
            with_record(&record)
        };
        let whole = record(cell_len, &cell_bytes);
        assert_eq!(decode(&whole).deltas().len(), 1);

        // The length field, then the bytes it states.
        assert_eq!(refused(&whole[..8]), truncated(9, 8));
        assert_eq!(refused(&whole[..9]), truncated(9 + cell_len, 9));
        assert_eq!(
            refused(&whole[..whole.len() - 1]),
            truncated(whole.len(), whole.len() - 1)
        );

        // No page takes a cell above the longest cell of the largest page,
        // and that is checked before the bytes are asked for.
        assert_eq!(
            decode(&record(MAX_CELL_LEN, &{
                let mut long = ModelCell::leaf(b"", &[]);
                long.value = ModelValue::Inline(vec![6; MAX_CELL_LEN - 72]);
                long.assemble()
            }))
            .deltas()
            .len(),
            1
        );
        assert_eq!(
            refused(&record(MAX_CELL_LEN + 1, &[])),
            TreePageError::CellTooLarge {
                len: MAX_CELL_LEN + 1,
                max: MAX_CELL_LEN,
            }
            .into()
        );
        assert_eq!(
            refused(&record(usize::from(u16::MAX), &[])),
            TreePageError::CellTooLarge {
                len: 65_535,
                max: MAX_CELL_LEN,
            }
            .into()
        );

        // The length the record states is the cell's own.
        let mut longer = cell_bytes.clone();
        longer.push(0);
        assert_eq!(
            refused(&record(cell_len + 1, &longer)),
            TreePageError::CellLengthMismatch {
                expected: cell_len,
                found: cell_len + 1,
            }
            .into()
        );
        assert_eq!(
            refused(&record(cell_len - 1, &cell_bytes[..cell_len - 1])),
            TreePageError::CellLengthMismatch {
                expected: cell_len,
                found: cell_len - 1,
            }
            .into()
        );
        assert_eq!(
            refused(&record(10, &cell_bytes[..10])),
            TreePageError::CellLengthMismatch {
                expected: 72,
                found: 10,
            }
            .into()
        );
        assert_eq!(
            refused(&record(0, &[])),
            TreePageError::CellLengthMismatch {
                expected: 72,
                found: 0,
            }
            .into()
        );
        // A following record does not lend its bytes to a cell.
        let mut two = record(cell_len - 1, &cell_bytes);
        two.extend([4, 1, 0]);
        assert_eq!(
            refused(&two),
            TreePageError::CellLengthMismatch {
                expected: cell_len,
                found: cell_len - 1,
            }
            .into()
        );

        // The cell's own checks.
        let damaged = |at: usize, byte: u8| {
            let mut cell = cell_bytes.clone();
            cell[at] = byte;
            refused(&record(cell_len, &cell))
        };
        assert_eq!(damaged(0, 0x11), FormatError::NonZeroReserved);
        // The left link's address, of which the slot is at 76.
        assert_eq!(damaged(76, 0), FormatError::MissingId);
        // The hosted root, of which the slot is at 119.
        assert_eq!(damaged(119, 0), FormatError::MissingId);
        // Marked as out of line, the value would be 12 bytes where the
        // record has 5.
        assert_eq!(
            damaged(0, 0x05),
            TreePageError::CellLengthMismatch {
                expected: cell_len + 7,
                found: cell_len,
            }
            .into()
        );

        // A value out of line whose place is no place.
        let mut placed = cell.clone();
        placed.value = ModelValue::OutOfLine(pointer(12, 20, 300));
        let placed = placed.assemble();
        let at_place = placed.len() - 12;
        let damaged = |at: usize, byte: u8| {
            let mut cell = placed.clone();
            cell[at] = byte;
            refused(&record(placed.len(), &cell))
        };
        assert_eq!(decode(&record(placed.len(), &placed)).deltas().len(), 1);
        assert_eq!(damaged(at_place, 0), FormatError::MissingId);
        assert_eq!(
            damaged(at_place + 4, 19),
            ValueLogError::InvalidOffset { offset: 19 }.into()
        );
    }
}

/// Cut anywhere, a run is a shorter run or an error that says how many
/// bytes were missing.
#[test]
fn every_prefix_of_a_run_is_a_shorter_run_or_an_error() {
    let (bytes, ops) = golden();
    let mut ends = vec![];
    let mut end = DeltaRun::HEADER_LEN;
    for op in &ops {
        end += op.with_delta(|delta| delta.encoded_len());
        ends.push(end);
    }
    assert_eq!(end, bytes.len());

    for len in 0..bytes.len() {
        let prefix = &bytes[..len];
        match ends.iter().position(|&end| end == len) {
            Some(last) => {
                let run = decode(prefix);
                assert_eq!(run.deltas().len(), last + 1);
                for (delta, op) in run.deltas().zip(&ops) {
                    op.with_delta(|expected| assert_eq!(delta, expected));
                }
            }
            None if len == DeltaRun::HEADER_LEN => {
                assert_eq!(delta_error(refused(prefix)), DeltaError::EmptyRun)
            }
            None => {
                let FormatError::Truncated { needed, available } = refused(prefix) else {
                    panic!("{len} bytes of a run are not reported as cut short")
                };
                assert_eq!(available, len);
                assert!(needed > len);
                // Nothing asks for more than the record it is in.
                let record_end = ends.iter().find(|&&end| end > len).expect("in a record");
                assert!(needed <= *record_end || len < DeltaRun::HEADER_LEN);
            }
        }
    }
}

/// The two directions of "one encoding": what is written reads back as
/// what was written, and what was read writes the same bytes.
#[test]
fn what_was_written_reads_back_and_what_was_read_is_written_the_same() {
    let size = PageSize::DEFAULT;
    let mut rng = Rng::new(0xB8);
    let mut kinds = [0; Op::KINDS];
    for round in 0..3_000 {
        let mut model = ModelPage::new(1 + rng.below(1 << 30) as u32, 0);
        for raw in 1..=rng.below(6) as u16 {
            model.cells.insert(raw, rng.cell(model.id, 100));
        }
        let ops: Vec<Op> = (0..1 + rng.below(12))
            .map(|_| Op::random(&mut rng, &model, size))
            // A cell that no page takes has no record.
            .filter(|op| op.with_delta(|delta| delta.encode_into(&mut Vec::new()).is_ok()))
            .collect();
        let created = (round % 3 == 0).then(|| rng.below(1 << 20) as u64);
        if ops.is_empty() && created.is_none() {
            continue;
        }
        let bytes = run_of(model.id, created, &ops);
        let lens: usize = ops
            .iter()
            .map(|op| op.with_delta(|delta| delta.encoded_len()))
            .sum();
        assert_eq!(
            bytes.len(),
            DeltaRun::HEADER_LEN + lens + created.map_or(0, |_| 9)
        );

        let run = decode(&bytes);
        assert_eq!(run.page(), model.id);
        assert_eq!(run.created(), created);
        assert_eq!(run.deltas().len(), ops.len());
        for (delta, op) in run.deltas().zip(&ops) {
            op.with_delta(|written| assert_eq!(delta, written));
            kinds[op.kind()] += 1;
        }
        assert_eq!(rewritten(&run), bytes);
        assert_eq!(decode(&bytes), run);
    }
    assert!(kinds.iter().all(|&count| count >= 300), "{kinds:?}");
}

#[test]
fn every_single_bit_flip_is_rejected_or_reads_as_what_it_says() {
    let (bytes, _) = golden();
    let (mut rejected, mut read) = (0, 0);
    for bit in 0..bytes.len() * 8 {
        let mut flipped = bytes.clone();
        flipped[bit / 8] ^= 1 << (bit % 8);
        match DeltaRun::decode(&flipped, V1) {
            Err(_) => rejected += 1,
            Ok(run) => {
                assert_eq!(rewritten(&run), flipped);
                read += 1;
            }
        }
    }
    // A run has no checksum of its own: a flipped bit in a hash or a value
    // reads as another hash or value. The record around the run catches it.
    assert!(rejected > 300, "{rejected} rejected");
    assert!(read > 3_000, "{read} read");
}

#[test]
fn arbitrary_bytes_are_an_error_or_a_run_and_never_a_panic() {
    let size = PageSize::MIN;
    let mut rng = Rng::new(0xF022);
    let (golden, _) = golden();
    let (page, model) = full_page(0xF023, size);
    let (mut runs, mut applied) = (0, 0);
    for round in 0..60_000 {
        let bytes = match round % 3 {
            0 => {
                let len = rng.below(120);
                rng.bytes(len)
            }
            1 => {
                // Records of every kind with arbitrary fields, for the page
                // they are applied to.
                let mut bytes = model.id.encode().to_vec();
                for _ in 0..1 + rng.below(4) {
                    bytes.push(1 + rng.below(22) as u8);
                    bytes.extend((rng.below(model.slot_count() + 3) as u16).to_le_bytes());
                    let len = match rng.below(4) {
                        0 => 0,
                        1 => 6,
                        2 => 32,
                        _ => rng.below(90),
                    };
                    let mut fields = rng.bytes(len);
                    if rng.chance(50) {
                        // Small numbers where a length or an id may stand.
                        for byte in fields.iter_mut().take(8) {
                            *byte %= 4;
                        }
                    }
                    bytes.extend(fields);
                }
                bytes
            }
            _ => {
                let mut bytes = golden.clone();
                bytes[..4].copy_from_slice(&model.id.encode());
                for _ in 0..1 + rng.below(3) {
                    let at = rng.below(bytes.len());
                    match rng.below(3) {
                        0 => bytes[at] = rng.below(256) as u8,
                        1 => bytes.truncate(at),
                        _ => bytes.insert(at, rng.below(256) as u8),
                    }
                    if bytes.is_empty() {
                        break;
                    }
                }
                bytes
            }
        };
        let Ok(run) = DeltaRun::decode(&bytes, V1) else {
            continue;
        };
        runs += 1;
        assert_eq!(rewritten(&run), bytes);
        assert_eq!(run.deltas().count(), run.deltas().len());

        // Whatever a run that was read asks of a valid page, the page is
        // valid afterwards.
        let mut frame = page.clone();
        if run.apply(&mut frame, size, V1).is_ok() {
            applied += 1;
        }
        let page = TreePage::from_validated(&frame, V1).expect("a page");
        page.check(size).expect("a valid page");
        let image = page.to_image();
        TreePage::decode(&image, V1, size).expect("an image");
    }
    assert!(runs > 3_000, "{runs} runs");
    assert!(applied > 300, "{applied} applied");
}

#[test]
fn a_change_that_would_not_read_back_is_not_written() {
    let size = PageSize::MAX;
    let mut out = vec![0xA5; 3];
    let mut refuse = |delta: Delta<'_>, expected: FormatError| {
        assert_eq!(delta.encode_into(&mut out), Err(expected));
        assert_eq!(out, [0xA5; 3]);
    };

    // A cell that no page takes.
    let mut long = ModelCell::leaf(b"key", &[]);
    long.value = ModelValue::Inline(vec![1; MAX_CELL_LEN - 74]);
    let bytes = long.assemble();
    assert_eq!(bytes.len(), MAX_CELL_LEN + 1);
    let cell = TreeCell::decode(&bytes, V1).expect("a cell");
    let too_large = TreePageError::CellTooLarge {
        len: MAX_CELL_LEN + 1,
        max: MAX_CELL_LEN,
    };
    refuse(
        Delta::Insert {
            slot: slot(1),
            cell,
        },
        too_large.into(),
    );
    refuse(
        Delta::Replace {
            slot: slot(1),
            cell,
        },
        too_large.into(),
    );

    // A cell from a page that was never checked, with half an address
    // among its hosted roots.
    let mut model = ModelPage::new(7, 0);
    let cell = ModelCell {
        hosted: vec![Some(address(9, 1))],
        ..ModelCell::leaf(b"key", b"value")
    };
    model.cells.insert(1, cell);
    let mut image = model.assemble();
    let slot_of_root = 22 + 2 + 72 + 4;
    assert_eq!(image[slot_of_root..slot_of_root + 2], [1, 0]);
    image[slot_of_root] = 0;
    assert_eq!(
        TreePage::decode(&image, V1, size).map(drop),
        Err(FormatError::MissingId)
    );
    let unchecked = TreePage::from_validated(&image, V1).expect("a header");
    let cell = unchecked.cell(slot(1)).expect("a cell");
    refuse(
        Delta::Insert {
            slot: slot(1),
            cell,
        },
        FormatError::MissingId,
    );

    // A value that no cell holds.
    let long = vec![1; MAX_INLINE_VALUE_LEN + 1];
    refuse(
        Delta::Value {
            slot: slot(1),
            value: crate::tree_page::CellValue::Inline(&long),
        },
        TreePageError::FieldTooLong {
            field: CellField::InlineValue,
            len: MAX_INLINE_VALUE_LEN + 1,
            max: MAX_INLINE_VALUE_LEN,
        }
        .into(),
    );

    // A run takes no record that was refused, and is not a run without
    // one.
    let mut bytes = vec![0xA5; 3];
    let mut writer = DeltaRunWriter::new(page_id(7), None, &mut bytes);
    assert_eq!(writer.encoded_len(), 4);
    let long = [0; 256];
    assert!(writer
        .push(&Delta::Feature {
            slot: slot(1),
            feature: &long,
        })
        .is_err());
    assert_eq!(writer.encoded_len(), 4);
    assert_eq!(
        writer.finish().map_err(delta_error),
        Err(DeltaError::EmptyRun)
    );
    assert_eq!(bytes, [0xA5; 3]);

    let mut writer = DeltaRunWriter::new(page_id(7), None, &mut bytes);
    writer
        .push(&Delta::Remove { slot: slot(1) })
        .expect("a record");
    assert!(writer
        .push(&Delta::Feature {
            slot: slot(1),
            feature: &long,
        })
        .is_err());
    assert_eq!(writer.encoded_len(), 7);
    assert_eq!(writer.finish(), Ok(7));
    assert_eq!(bytes, [0xA5, 0xA5, 0xA5, 7, 0, 0, 0, 4, 1, 0]);
}

#[test]
fn changes_are_compared_by_what_they_change() {
    let (_, ops) = golden();
    for (index, op) in ops.iter().enumerate() {
        for (other_index, other) in ops.iter().enumerate() {
            op.with_delta(|delta| {
                other.with_delta(|other| assert_eq!(delta == other, index == other_index))
            });
        }
        // The same change to another slot is another change.
        let mut moved = op.clone();
        match &mut moved {
            Op::Insert(raw, _)
            | Op::Replace(raw, _)
            | Op::Remove(raw)
            | Op::Hash(raw, ..)
            | Op::Child(raw, ..)
            | Op::Link(raw, ..)
            | Op::FragmentRoot(raw, _)
            | Op::HostedRoot(raw, ..)
            | Op::HostedRoots(raw, _)
            | Op::Feature(raw, _)
            | Op::Value(raw, _) => *raw += 100,
        }
        op.with_delta(|delta| {
            moved.with_delta(|moved| {
                assert_ne!(delta, moved);
                assert_eq!(moved.slot().get(), delta.slot().get() + 100);
            })
        });
    }
}
