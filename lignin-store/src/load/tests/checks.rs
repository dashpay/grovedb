//! What a load refuses that a checksum cannot see: blocks that are whole
//! and are not what the page table says they are.

use lignin_format::{
    delta::DeltaError, flat_page::FlatPageError, segment::SegmentError, tree_page::TreePageError,
};

use super::*;

const RAW: Compression = Compression::Raw;
const LZ4: Compression = Compression::Lz4;

#[test]
fn a_block_of_the_other_kind_is_refused() {
    let mut db = Db::new(TreePageSize::MIN);
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
    let mut db = Db::new(TreePageSize::MIN);
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
}

#[test]
fn bytes_behind_an_image_are_refused() {
    let mut db = Db::new(TreePageSize::MIN);
    let tree = Written::filled(db.size, page(7), 3, 10).image();
    let flat = flat_image(db.size, 5, 1);
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
    let mut db = Db::new(TreePageSize::MIN);
    let tree = Written::filled(db.size, page(1), 3, 10).image();
    let flat = flat_image(db.size, 5, 1);
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
    let mut db = Db::new(TreePageSize::MIN);
    let large = Written::filled(TreePageSize::DEFAULT, page(7), 60, 200).image();
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

    let mut db = Db::new(TreePageSize::MAX);
    let whole = vec![3; TreePageSize::MAX.get()];
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
    let mut db = Db::new(TreePageSize::MIN);
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
    let mut db = Db::new(TreePageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 4, 10);
    db.base(page(7), 1, RAW, &written.image());
    let good = written.change(&[Delta::Hash {
        slot: slot(2),
        field: HashField::KeyValue,
        hash: &[0xC3; 32],
    }]);
    let after_good = written.image();

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

    db.base(page(7), 3, RAW, &written.image());
    db.delta(page(7), 4, RAW, &good);
    let pool = db.pool(1);
    let loader = db.loader(&pool);
    loader.get(page(7), version(2), &mut buffers).unwrap_err();
    let loaded = loader.get(page(7), version(4), &mut buffers).unwrap();
    assert_eq!(tree_image(&loaded), after_good);
    assert_eq!(loaded.born(), version(4));
}

#[test]
fn a_run_in_a_segment_does_not_create_its_page() {
    let mut db = Db::new(TreePageSize::MIN);
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
}

#[test]
fn a_flat_page_takes_no_run() {
    let mut db = Db::new(TreePageSize::MIN);
    for (raw, kind) in [(7, 0), (8, 1)] {
        let image = if kind == 0 {
            flat_image(db.size, 6, 1)
        } else {
            lignin_format::flat_page::FlatBranchBuilder::new(
                b"",
                db.size.image_capacity(),
                1,
                page(40),
            )
            .expect("a branch")
            .finish()
        };
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
    let mut db = Db::new(TreePageSize::MIN);
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
    let mut db = Db::new(TreePageSize::MIN);
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
    let mut db = Db::new(TreePageSize::MIN);
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
    let db = Db::new(TreePageSize::MIN);
    let pool = db.pool(1);
    match PageLoader::new(&pool, &db.table, &db.segments, TreePageSize::DEFAULT).unwrap_err() {
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
    let mut db = Db::new(TreePageSize::MIN);
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
            LoadError::NoBase {
                page: of,
                born: version(4),
            },
            "the chain of page 7 up to version 4 has no base image",
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
