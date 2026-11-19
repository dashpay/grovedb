//! What a load refuses that a checksum cannot see: blocks that are whole
//! and are not what the page table says they are.

use lignin_format::{
    delta::DeltaError,
    flat_page::{FlatBranchBuilder, FlatPageError},
    segment::SegmentError,
    tree_page::TreePageError,
};

use super::*;
use crate::segment::tests::faulty::INJECTED;

const RAW: Compression = Compression::Raw;
const LZ4: Compression = Compression::Lz4;

#[test]
fn a_block_of_the_other_kind_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let run = Written::filled(db.size, page(7), 3, 10).change(&[Delta::Remove { slot: slot(1) }]);

    let stray = db.append(page(7), BlockContent::Delta, RAW, &run);
    db.table
        .push(page(7), Link::base(version(1), stray))
        .unwrap();
    match load(&db, page(7), 1).unwrap_err() {
        LoadError::Content {
            page: of,
            location,
            expected: BlockContent::Base,
            found: BlockContent::Delta,
        } => assert_eq!((of, location), (page(7), stray)),
        other => panic!("{other:?}"),
    }

    let image = Written::filled(db.size, page(8), 3, 10).image();
    db.base(page(8), 1, RAW, &image);
    let stray = db.append(page(8), BlockContent::Base, LZ4, &image);
    db.table
        .push(page(8), Link::delta(version(2), stray))
        .unwrap();
    match load(&db, page(8), 2).unwrap_err() {
        LoadError::Content {
            page: of,
            location,
            expected: BlockContent::Delta,
            found: BlockContent::Base,
        } => assert_eq!((of, location), (page(8), stray)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_image_or_a_run_of_another_page_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let mut nine = Written::filled(db.size, page(9), 3, 10);
    let mut seven = Written::filled(db.size, page(7), 3, 10);
    let wrong = DeltaError::WrongPage {
        found: page(9),
        expected: page(7),
    };

    let block = db.base(page(7), 1, LZ4, &nine.image());
    assert_eq!(
        payload_error(load(&db, page(7), 1).unwrap_err()),
        (block, wrong.into())
    );

    db.base(page(7), 2, RAW, &seven.image());
    let run = nine.change(&[Delta::Remove { slot: slot(1) }]);
    let block = db.delta(page(7), 3, RAW, &run);
    assert_eq!(
        payload_error(load(&db, page(7), 3).unwrap_err()),
        (block, wrong.into())
    );
    assert_eq!(tree_image(&load(&db, page(7), 2).unwrap()), seven.image());

    let run = seven.change(&[Delta::Remove { slot: slot(1) }]);
    db.base(
        page(5),
        1,
        RAW,
        &Written::filled(db.size, page(5), 3, 10).image(),
    );
    let block = db.delta(page(5), 2, RAW, &run);
    assert_eq!(
        payload_error(load(&db, page(5), 2).unwrap_err()),
        (
            block,
            DeltaError::WrongPage {
                found: page(7),
                expected: page(5),
            }
            .into()
        )
    );

    let leaf = flat_image(db.size, page(9), 4, 1);
    let branch = FlatBranchBuilder::new(page(9), b"", db.size.image_capacity(), 1, page(40))
        .expect("a branch")
        .finish();
    for (born, image) in [(4, leaf), (5, branch)] {
        let block = db.base(page(7), born, LZ4, &image);
        assert_eq!(
            payload_error(load(&db, page(7), born).unwrap_err()),
            (block, wrong.into())
        );
        db.base(page(9), born, RAW, &image);
        assert_eq!(load(&db, page(9), born).unwrap()[..image.len()], image);
    }
}

/// A page version that a segment holds has no pending value
/// (`page-delta.md` PD16, `flat-page.md` P15): not its base image, and not
/// the page as the last record of a run leaves it.
#[test]
fn a_page_version_in_a_segment_holds_no_pending_value() {
    let mut db = Db::new(PageSize::MIN);
    let waiting = PendingValue::new([7, 0, 0, 0, 0, 0, 0, 1], 5_000).expect("a pending value");
    let placed = ValuePointer::new(FileId::MIN, 64, 5_000).expect("a pointer");
    let pending = |db: &Db, of: u32, at: u64| match load(db, page(of), at).unwrap_err() {
        LoadError::PendingValue {
            page: found,
            location,
        } => {
            assert_eq!(found, page(of));
            location
        }
        other => panic!("{other:?}"),
    };
    let make_pending = Delta::Value {
        slot: slot(2),
        value: CellValue::Pending(waiting),
    };
    let place = Delta::Value {
        slot: slot(2),
        value: CellValue::OutOfLine(placed),
    };
    let hash = Delta::Hash {
        slot: slot(1),
        field: HashField::Value,
        hash: &[0xD4; 32],
    };

    let mut written = Written::filled(db.size, page(7), 3, 10);
    let clean = written.image();
    written.apply(&make_pending);
    let base = db.base(page(7), 1, RAW, &written.image());
    assert_eq!(pending(&db, 7, 1), base);

    db.base(page(7), 2, RAW, &clean);
    let left_pending = written.change(&[hash, make_pending]);
    let run = db.delta(page(7), 3, RAW, &left_pending);
    assert_eq!(pending(&db, 7, 3), run);
    assert_eq!(tree_image(&load(&db, page(7), 2).unwrap()), clean);

    let placing = written.change(&[place]);
    db.delta(page(7), 4, RAW, &placing);
    assert_eq!(pending(&db, 7, 4), run);

    let mut written = Written::filled(db.size, page(8), 3, 10);
    db.base(page(8), 1, RAW, &written.image());
    let placed_in_the_run = written.change(&[make_pending, hash, place]);
    db.delta(page(8), 2, LZ4, &placed_in_the_run);
    let frame = load(&db, page(8), 2).unwrap();
    assert_eq!(tree_image(&frame), written.image());
    let loaded = TreePage::from_validated(&frame, FRAME_FORMAT).unwrap();
    assert_eq!(
        loaded.cell(slot(2)).unwrap().value(),
        CellValue::OutOfLine(placed)
    );

    let capacity = db.size.image_capacity();
    let entry = |place: ValuePlace| {
        let mut leaf = FlatLeafBuilder::new(page(9), b"", capacity).expect("a leaf");
        leaf.push(b"key", FlatValue::OutOfLine(&place.encode()))
            .expect("the entry fits");
        leaf.finish()
    };
    let base = db.base(page(9), 1, LZ4, &entry(ValuePlace::Pending(waiting)));
    assert_eq!(pending(&db, 9, 1), base);
    let image = entry(ValuePlace::Placed(placed));
    db.base(page(9), 2, RAW, &image);
    assert_eq!(load(&db, page(9), 2).unwrap()[..image.len()], image);
}

#[test]
fn bytes_behind_an_image_are_refused() {
    let mut db = Db::new(PageSize::MIN);
    let tree = Written::filled(db.size, page(7), 3, 10).image();
    let flat = flat_image(db.size, page(8), 5, 1);
    for (of, image) in [(page(7), tree), (page(8), flat)] {
        let mut longer = image.clone();
        longer.push(0);
        let block = db.base(of, 1, RAW, &longer);
        match load(&db, of, 1).unwrap_err() {
            LoadError::TrailingBytes {
                page: found,
                location,
                image_len,
                payload_len,
            } => assert_eq!(
                (found, location, image_len, payload_len),
                (of, block, image.len(), image.len() + 1)
            ),
            other => panic!("{other:?}"),
        }
        db.base(of, 2, RAW, &image);
        assert_eq!(load(&db, of, 2).unwrap()[..image.len()], image);
    }
}

#[test]
fn what_is_not_a_page_image_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let tree = Written::filled(db.size, page(1), 3, 10).image();
    let flat = flat_image(db.size, page(7), 5, 1);
    let mut not_compact = tree.clone();
    not_compact[12] = 1;
    let mut reserved = tree.clone();
    reserved[1] = 1;
    let mut level = flat.clone();
    level[1] = 1;
    let cases: [(&[u8], FormatError); 7] = [
        (
            &[],
            FormatError::Truncated {
                needed: TreePage::HEADER_LEN,
                available: 0,
            },
        ),
        (&[9; 40], FormatError::UnknownRecordKind { code: 9 }),
        (&[0; 40], FormatError::UnknownRecordKind { code: 0 }),
        (
            &tree[..tree.len() - 1],
            FormatError::Truncated {
                needed: tree.len(),
                available: tree.len() - 1,
            },
        ),
        (&not_compact, TreePageError::NotCompact { unused: 1 }.into()),
        (&reserved, FormatError::NonZeroReserved),
        (
            &level,
            FlatPageError::InvalidLevel {
                kind: FlatPageKind::Leaf,
                level: 1,
            }
            .into(),
        ),
    ];
    for (raw, (payload, expected)) in cases.into_iter().enumerate() {
        let of = page(1 + raw as u32);
        let block = db.base(of, 1, RAW, payload);
        assert_eq!(
            payload_error(load(&db, of, 1).unwrap_err()),
            (block, expected),
            "case {raw}"
        );
    }
    let block = db.base(page(20), 1, RAW, &flat[..flat.len() - 1]);
    let (location, error) = payload_error(load(&db, page(20), 1).unwrap_err());
    assert_eq!(location, block);
    assert!(matches!(error, FormatError::Truncated { .. }), "{error:?}");
}

#[test]
fn an_image_longer_than_a_page_of_the_store_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let large = Written::filled(PageSize::DEFAULT, page(7), 60, 200).image();
    assert!(large.len() > db.size.get());
    let too_long = FormatError::RecordTooLong {
        len: large.len() as u64,
        max: db.size.image_capacity() as u64,
    };
    for (of, compression) in [(page(7), RAW), (page(8), LZ4)] {
        let block = db.base(of, 1, compression, &large);
        assert_eq!(
            block_error(load(&db, of, 1).unwrap_err()),
            (block, too_long),
            "{compression:?}"
        );
    }

    let mut db = Db::new(PageSize::MAX);
    let whole = vec![3; PageSize::MAX.get()];
    let block = db.base(page(7), 1, LZ4, &whole);
    assert_eq!(
        block_error(load(&db, page(7), 1).unwrap_err()),
        (
            block,
            FormatError::RecordTooLong {
                len: 65_536,
                max: 65_535,
            }
        )
    );
}

#[test]
fn a_payload_that_does_not_decompress_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 12, 60);
    let base = db.base(page(7), 1, LZ4, &written.image());
    let cells = [
        cell_bytes(b"x-1", &[0x55; 120], 1, None),
        cell_bytes(b"x-2", &[0x55; 120], 2, None),
    ];
    let run = written.change(&[
        Delta::Insert {
            slot: slot(13),
            cell: cell(&cells[0]),
        },
        Delta::Insert {
            slot: slot(14),
            cell: cell(&cells[1]),
        },
    ]);
    let delta = db.delta(page(7), 2, LZ4, &run);
    assert!((delta.len as usize) < run.len());
    assert_eq!(tree_image(&load(&db, page(7), 2).unwrap()), written.image());

    let original = db.stored(delta);
    db.rewrite(delta, |body| body[4..].fill(0xFF));
    assert_eq!(
        payload_error(load(&db, page(7), 2).unwrap_err()),
        (delta, SegmentError::Decompression.into())
    );
    db.segment_file()
        .write_all_at(&original, delta.offset)
        .unwrap();

    db.rewrite(base, |body| body[4..].fill(0xFF));
    assert_eq!(
        payload_error(load(&db, page(7), 2).unwrap_err()),
        (base, SegmentError::Decompression.into())
    );
}

#[test]
fn a_run_that_does_not_apply_is_refused_and_leaves_nothing_behind() {
    let mut db = Db::new(PageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 4, 10);
    db.base(page(7), 1, RAW, &written.image());
    let before_good = written.image();
    let good = written.change(&[Delta::Hash {
        slot: slot(2),
        field: HashField::KeyValue,
        hash: &[0xC3; 32],
    }]);
    let after_good = written.image();
    assert_ne!(before_good, after_good);

    let mut bad = Vec::new();
    let mut records = DeltaRunWriter::new(page(7), None, &mut bad);
    records.push(&Delta::Remove { slot: slot(1) }).unwrap();
    records.push(&Delta::Remove { slot: slot(9) }).unwrap();
    records.push(&Delta::Remove { slot: slot(2) }).unwrap();
    records.finish().unwrap();
    let refused = db.delta(page(7), 2, RAW, &bad);

    let pool = db.pool(1);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let (location, error) =
        payload_error(loader.get(page(7), version(2), &mut buffers).unwrap_err());
    assert_eq!(location, refused);
    assert_eq!(error, TreePageError::SlotFree { slot: slot(9) }.into());
    assert_eq!(pool.census(), Default::default());
    assert!(pool.get(page(7), version(2)).is_err());

    db.base(page(7), 3, RAW, &before_good);
    db.delta(page(7), 4, RAW, &good);
    let loader = db.loader(&pool);
    loader.get(page(7), version(2), &mut buffers).unwrap_err();
    let loaded = loader.get(page(7), version(4), &mut buffers).unwrap();
    assert_eq!(tree_image(&loaded), after_good);
    assert_eq!(loaded.born(), version(4));
}

#[test]
fn a_run_in_a_segment_does_not_create_its_page() {
    let mut db = Db::new(PageSize::MIN);
    let written = Written::filled(db.size, page(7), 2, 10);
    db.base(page(7), 1, RAW, &written.image());
    let bytes = cell_bytes(b"k", b"v", 1, None);
    let mut run = Vec::new();
    let mut records = DeltaRunWriter::new(page(7), Some(0), &mut run);
    records
        .push(&Delta::Insert {
            slot: slot(1),
            cell: cell(&bytes),
        })
        .unwrap();
    records.finish().unwrap();
    let block = db.delta(page(7), 2, RAW, &run);
    match load(&db, page(7), 2).unwrap_err() {
        LoadError::CreatedInSegment { page: of, location } => {
            assert_eq!((of, location), (page(7), block));
        }
        other => panic!("{other:?}"),
    }

    db.base(
        page(8),
        1,
        RAW,
        &Written::filled(db.size, page(8), 2, 10).image(),
    );
    let block = db.delta(page(8), 2, RAW, &run);
    assert_eq!(
        payload_error(load(&db, page(8), 2).unwrap_err()),
        (
            block,
            DeltaError::WrongPage {
                found: page(7),
                expected: page(8),
            }
            .into()
        )
    );
}

/// The block of a run says its payload is 100 MiB long, in 60 stored bytes.
#[test]
fn a_run_that_cannot_be_as_long_as_it_says_takes_no_memory() {
    let mut db = Db::new(PageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 3, 10);
    db.base(page(7), 1, RAW, &written.image());
    let bytes = cell_bytes(b"x", &[0x55; 2_000], 1, None);
    let run = written.change(&[Delta::Insert {
        slot: slot(4),
        cell: cell(&bytes),
    }]);
    let delta = db.delta(page(7), 2, LZ4, &run);
    assert!(delta.len < 100);
    db.rewrite(delta, |body| {
        body[..4].copy_from_slice(&(100u32 << 20).to_le_bytes());
    });

    let pool = db.pool(1);
    let mut buffers = LoadBuffers::new();
    let error = db
        .loader(&pool)
        .get(page(7), version(2), &mut buffers)
        .unwrap_err();
    assert_eq!(
        payload_error(error),
        (delta, SegmentError::Decompression.into())
    );
    assert!(buffers.run.capacity() < 1 << 20);
}

/// A run of exactly `len` bytes for the page in `written`: changes to the
/// value hash of its first cell, and the removal of some of its last
/// cells to make up the length.
fn run_of_len(written: &mut Written, len: usize) -> Vec<u8> {
    let hash = [0x5A; 32];
    let change = Delta::Hash {
        slot: slot(1),
        field: HashField::Value,
        hash: &hash,
    };
    let cells = written.page().cells().count() as u16;
    let removal = |n: u16| Delta::Remove {
        slot: slot(cells - n),
    };
    let (per_change, per_removal) = (change.encoded_len(), removal(0).encoded_len());
    let records = len - PageId::ENCODED_LEN;
    let removals = (0..cells - 1)
        .find(|&n| (records - usize::from(n) * per_removal) % per_change == 0)
        .expect("some number of removals makes up the length");
    let changes = (records - usize::from(removals) * per_removal) / per_change;
    let mut deltas = vec![change; changes];
    deltas.extend((0..removals).map(removal));
    let run = written.change(&deltas);
    assert_eq!(run.len(), len);
    run
}

/// **A delta block whose payload is above one page size is refused**
/// (`flush::max_run_len`): write-back writes the page as a base image
/// instead, so no chain has one. A run of exactly that length is read.
#[test]
fn a_run_longer_than_a_page_is_refused() {
    let size = PageSize::MIN;
    let max = crate::flush::max_run_len(size);
    assert_eq!(max, size.get());
    let filled = |id| Written::filled(size, page(id), 40, 10);

    // At the limit: stored as it is, and compressed.
    for compression in [RAW, LZ4] {
        let mut db = Db::new(size);
        let mut written = filled(7);
        db.base(page(7), 1, RAW, &written.image());
        let run = run_of_len(&mut written, max);
        db.delta(page(7), 2, compression, &run);
        let loaded = load(&db, page(7), 2).expect("a run of one page size is read");
        let loaded = TreePage::from_validated(&loaded, V1).expect("a tree page");
        assert_eq!(loaded.to_image(), written.image());
    }

    // One byte more. Stored as it is, the block is longer than any run can
    // be stored as, and is not read: the bytes of its body say nothing.
    let mut db = Db::new(size);
    let mut written = filled(7);
    db.base(page(7), 1, RAW, &written.image());
    let run = run_of_len(&mut written, max + 1);
    let delta = db.delta(page(7), 2, RAW, &run);
    db.rewrite(delta, |body| body.fill(0xEE));
    let pool = db.pool(1);
    let mut buffers = LoadBuffers::new();
    let error = db
        .loader(&pool)
        .get(page(7), version(2), &mut buffers)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "the block at offset {} of segment {} is a run of {} bytes for page 7, and a run of \
             this store has at most {max}",
            delta.offset,
            delta.file,
            max + 1
        )
    );
    assert!(std::error::Error::source(&error).is_none());
    match error {
        LoadError::RunTooLong {
            page: of,
            location,
            len,
            max: most,
        } => assert_eq!((of, location, len, most), (page(7), delta, max + 1, max)),
        other => panic!("{other}"),
    }
    assert!(buffers.block.capacity() <= max, "the block was not read");
    assert_eq!(pool.census().published, 0, "nothing was put into the pool");

    // Compressed, the block is short, and the run it states is too long:
    // no memory is taken for it.
    let mut db = Db::new(size);
    let mut written = filled(8);
    db.base(page(8), 1, RAW, &written.image());
    let run = run_of_len(&mut written, max + 1);
    let delta = db.delta(page(8), 2, LZ4, &run);
    assert!((delta.len as usize) < max / 4, "{} bytes stored", delta.len);
    let pool = db.pool(1);
    let mut buffers = LoadBuffers::new();
    let error = db
        .loader(&pool)
        .get(page(8), version(2), &mut buffers)
        .unwrap_err();
    match error {
        LoadError::RunTooLong {
            page: of,
            location,
            len,
            max: most,
        } => assert_eq!((of, location, len, most), (page(8), delta, max + 1, max)),
        other => panic!("{other}"),
    }
    assert_eq!(buffers.run.capacity(), 0, "no memory for the run");

    // The page under the run is still read at its own version.
    assert!(load(&db, page(8), 1).is_ok());
}

#[test]
fn a_page_that_is_not_there_takes_no_frame() {
    let mut db = Db::new(PageSize::MIN);
    let image = Written::filled(db.size, page(7), 3, 10).image();
    db.base(page(7), 1, RAW, &image);
    db.table.push(page(8), Link::free(version(1))).unwrap();
    let pool = db.pool(1);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    drop(loader.get(page(7), version(1), &mut buffers).unwrap());

    for absent in [page(8), page(9)] {
        assert!(matches!(
            loader.get(absent, version(1), &mut buffers).unwrap_err(),
            LoadError::NoPage { .. }
        ));
    }
    assert!(pool.get(page(7), version(1)).is_ok());
    let stats = pool.stats();
    assert_eq!((stats.evictions, stats.overflows), (0, 0));
}

#[test]
fn a_read_the_file_layer_fails_is_its_error_and_the_next_load_goes_through() {
    let mut db = Db::new(PageSize::MIN);
    let image = Written::filled(db.size, page(7), 3, 10).image();
    let block = db.base(page(7), 1, RAW, &image);
    let pool = db.pool(1);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();

    db.vfs.fail(Op::Read, SEGMENT);
    match loader.get(page(7), version(1), &mut buffers).unwrap_err() {
        LoadError::Block {
            page: of,
            location,
            source: SegmentStoreError::Io { source, .. },
        } => {
            assert_eq!((of, location), (page(7), block));
            assert_eq!(source.kind(), INJECTED);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(pool.census(), Default::default());
    let loaded = loader.get(page(7), version(1), &mut buffers).unwrap();
    assert_eq!(tree_image(&loaded), image);
}

#[test]
fn a_flat_page_takes_no_run() {
    let mut db = Db::new(PageSize::MIN);
    let branch = FlatBranchBuilder::new(page(8), b"", db.size.image_capacity(), 1, page(40))
        .expect("a branch")
        .finish();
    for (raw, image) in [(7, flat_image(db.size, page(7), 6, 1)), (8, branch)] {
        db.base(page(raw), 1, RAW, &image);
        let run =
            Written::filled(db.size, page(raw), 1, 4).change(&[Delta::Remove { slot: slot(1) }]);
        let block = db.delta(page(raw), 2, RAW, &run);
        match load(&db, page(raw), 2).unwrap_err() {
            LoadError::FlatPageDelta { page: of, location } => {
                assert_eq!((of, location), (page(raw), block));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(load(&db, page(raw), 1).unwrap()[..image.len()], image);
    }
}

#[test]
fn what_is_not_a_run_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 2, 10);
    let image = written.image();
    let run = written.change(&[Delta::Remove { slot: slot(1) }]);
    let mut longer = run.clone();
    longer.push(0);
    let cases: [(&[u8], FormatError); 4] = [
        (
            &[],
            FormatError::Truncated {
                needed: 4,
                available: 0,
            },
        ),
        (&run[..4], DeltaError::EmptyRun.into()),
        (&longer, FormatError::UnknownRecordKind { code: 0 }),
        (&[0; 12], FormatError::MissingId),
    ];
    for (raw, (payload, expected)) in cases.into_iter().enumerate() {
        let of = page(7 + raw as u32);
        let own = Written::filled(db.size, of, 2, 10).image();
        db.base(of, 1, RAW, &own);
        let block = db.delta(of, 2, RAW, payload);
        assert_eq!(
            payload_error(load(&db, of, 2).unwrap_err()),
            (block, expected),
            "case {raw}"
        );
    }
    assert_eq!(tree_image(&load(&db, page(7), 1).unwrap()), image);
}

#[test]
fn a_block_that_is_not_in_the_store_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let image = Written::filled(db.size, page(7), 2, 10).image();
    let block = db.base(page(7), 1, RAW, &image);
    let elsewhere = BlockLocation {
        file: FileId::new(9).unwrap(),
        ..block
    };
    let past_the_end = BlockLocation {
        offset: block.offset + 1,
        ..block
    };
    let shorter = BlockLocation {
        len: block.len - 1,
        ..block
    };
    for (raw, location) in [(8, elsewhere), (9, past_the_end), (10, shorter)] {
        db.table
            .push(page(raw), Link::base(version(1), location))
            .unwrap();
        match load(&db, page(raw), 1).unwrap_err() {
            LoadError::Block {
                page: of,
                location: found,
                source,
            } => {
                assert_eq!((of, found), (page(raw), location));
                match (raw, source) {
                    (8, SegmentStoreError::UnknownSegment { file }) => {
                        assert_eq!(file, elsewhere.file);
                    }
                    (9, SegmentStoreError::OutOfBounds { .. }) => {}
                    (10, SegmentStoreError::Format { source, .. }) => {
                        assert!(matches!(source, FormatError::Truncated { .. }));
                    }
                    (_, other) => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    db.base(
        page(11),
        1,
        RAW,
        &Written::filled(db.size, page(11), 2, 10).image(),
    );
    db.table
        .push(page(11), Link::delta(version(2), elsewhere))
        .unwrap();
    assert!(matches!(
        load(&db, page(11), 2).unwrap_err(),
        LoadError::Block {
            source: SegmentStoreError::UnknownSegment { .. },
            location,
            ..
        } if location == elsewhere
    ));
}

#[test]
fn a_page_that_no_segment_holds_at_a_version_is_no_page() {
    let mut db = Db::new(PageSize::MIN);
    let first = Written::filled(db.size, page(7), 2, 10).image();
    let second = Written::filled(db.size, page(7), 3, 10).image();
    let no_page = |db: &Db, of: u32, at: u64| match load(db, page(of), at).unwrap_err() {
        LoadError::NoPage {
            page: found,
            at: asked,
            freed,
        } => {
            assert_eq!((found, asked), (page(of), version(at)));
            freed
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(no_page(&db, 7, 1), None);

    db.base(page(7), 5, RAW, &first);
    assert_eq!(no_page(&db, 7, 4), None);
    assert_eq!(no_page(&db, 8, 5), None);
    assert_eq!(load(&db, page(7), 5).unwrap()[..first.len()], first);

    db.table.push(page(7), Link::free(version(7))).unwrap();
    assert_eq!(load(&db, page(7), 6).unwrap()[..first.len()], first);
    assert_eq!(no_page(&db, 7, 7), Some(version(7)));
    assert_eq!(no_page(&db, 7, 8), Some(version(7)));

    db.base(page(7), 9, LZ4, &second);
    assert_eq!(no_page(&db, 7, 8), Some(version(7)));
    assert_eq!(load(&db, page(7), 9).unwrap()[..second.len()], second);
    assert_eq!(load(&db, page(7), 6).unwrap()[..first.len()], first);
}

#[test]
fn a_loader_takes_a_pool_of_its_page_size() {
    let db = Db::new(PageSize::MIN);
    let pool = db.pool(1);
    match PageLoader::new(&pool, &db.table, &db.segments, PageSize::DEFAULT).unwrap_err() {
        LoadError::FrameSize {
            page_size,
            frame_len,
        } => assert_eq!((page_size, frame_len), (32_768, 8_192)),
        other => panic!("{other:?}"),
    }
}

/// A commit's page version is marked as written although no link of it
/// was pushed, which breaks the pool's rule. A reader that then misses
/// the page finds an older version in the page table than the pool knows.
#[test]
fn a_page_the_table_is_behind_the_pool_on_is_refused() {
    let mut db = Db::new(PageSize::MIN);
    let image = Written::filled(db.size, page(7), 2, 10).image();
    db.base(page(7), 1, RAW, &image);
    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let owner = OwnerId::new(1);

    let first = loader.get(page(7), version(1), &mut buffers).unwrap();
    let copy = pool.make_private(owner, page(7), version(1)).unwrap();
    let second = pool.install(&copy, version(2)).unwrap();
    let copy = pool.make_private(owner, page(7), version(2)).unwrap();
    let third = pool.install(&copy, version(3)).unwrap();
    pool.mark_written(&third).unwrap();
    drop((first, second, third, copy));
    let blanks: Vec<_> = (0..4).map(|_| pool.blank()).collect();
    drop(blanks);
    assert!(pool.get_version(page(7), version(2)).is_some());
    assert!(pool.get_version(page(7), version(3)).is_none());

    match loader.get(page(7), version(3), &mut buffers).unwrap_err() {
        LoadError::Cache(CacheError::LoadedSuperseded {
            page: of,
            born,
            superseded,
            at,
        }) => assert_eq!(
            (of, born, superseded, at),
            (page(7), version(1), version(2), version(3))
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_error_has_its_message() {
    use std::error::Error;

    let location = BlockLocation {
        file: FileId::new(3).unwrap(),
        offset: 20,
        len: 64,
    };
    let of = page(7);
    let cases = [
        (
            LoadError::FrameSize {
                page_size: 32_768,
                frame_len: 8_192,
            },
            "the store's pages are 32768 bytes and the pool's frames 8192",
            false,
        ),
        (
            LoadError::NoPage {
                page: of,
                at: version(4),
                freed: None,
            },
            "no segment holds a version of page 7 at 4",
            false,
        ),
        (
            LoadError::NoPage {
                page: of,
                at: version(4),
                freed: Some(version(3)),
            },
            "page 7 has no version at 4: the commit of version 3 freed it",
            false,
        ),
        (
            LoadError::Block {
                page: of,
                location,
                source: SegmentStoreError::UnknownSegment {
                    file: location.file,
                },
            },
            "the block at offset 20 of segment 3 could not be read for page 7: no segment 3",
            true,
        ),
        (
            LoadError::Content {
                page: of,
                location,
                expected: BlockContent::Base,
                found: BlockContent::Delta,
            },
            "the block at offset 20 of segment 3 holds a run of deltas where the chain of page 7 \
             has a base image",
            false,
        ),
        (
            LoadError::Payload {
                page: of,
                location,
                source: FormatError::MissingId,
            },
            "the block at offset 20 of segment 3 does not give page 7: identifier field holds no \
             identifier",
            true,
        ),
        (
            LoadError::TrailingBytes {
                page: of,
                location,
                image_len: 100,
                payload_len: 101,
            },
            "the block at offset 20 of segment 3 holds 101 bytes for page 7, whose image is 100 \
             long",
            false,
        ),
        (
            LoadError::FlatPageDelta { page: of, location },
            "the block at offset 20 of segment 3 is a run of deltas for page 7, which is a flat \
             page",
            false,
        ),
        (
            LoadError::CreatedInSegment { page: of, location },
            "the block at offset 20 of segment 3 is a run that creates page 7, in a segment",
            false,
        ),
        (
            LoadError::PendingValue { page: of, location },
            "the block at offset 20 of segment 3 leaves page 7 with a value that is not in the \
             value log",
            false,
        ),
        (
            LoadError::from(CacheError::WrongPool),
            "the buffer pool refused the page: ",
            true,
        ),
    ];
    for (error, message, has_source) in cases {
        let shown = error.to_string();
        assert!(shown.starts_with(message), "{shown}");
        assert_eq!(error.source().is_some(), has_source, "{shown}");
        if let Some(source) = error.source() {
            assert!(shown.ends_with(&source.to_string()), "{shown}");
        }
    }
}
