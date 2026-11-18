//! STORE-05's acceptance test: a page with any single corrupted byte is
//! rejected with a typed error.

use std::collections::BTreeMap;

use lignin_format::segment::SegmentError;

use super::*;

/// One page version in segments: the blocks of its chain, and the page a
/// load of it gives.
struct Stored {
    of: PageId,
    at: u64,
    flat: bool,
    blocks: Vec<BlockLocation>,
    expected: Vec<u8>,
}

/// A tree page with a compressed base image, a raw run of two changed
/// hashes and a compressed run that adds cells; a tree page with a raw
/// image; and a flat page with a raw image and one stored with compression. With `full`, the pages are as long as the
/// store's pages get and their values do not compress.
fn stored_pages(db: &mut Db, full: bool) -> Vec<Stored> {
    let size = db.size;
    let tree = page(7);
    let cells = if full {
        ((size.get() - 700) / 347) as u16
    } else {
        4
    };
    let mut written = if full {
        Written::incompressible(size, tree, cells, 250)
    } else {
        Written::filled(size, tree, cells, 16)
    };
    let first_image = written.image();
    let base = db.base(tree, 1, Compression::Lz4, &first_image);
    let hashes = written.change(&[
        Delta::Hash {
            slot: slot(1),
            field: HashField::Value,
            hash: &[0xE1; 32],
        },
        Delta::Hash {
            slot: slot(3),
            field: HashField::Child(Side::Left),
            hash: &[0xE2; 32],
        },
    ]);
    let first = db.delta(tree, 2, Compression::Raw, &hashes);
    let added = [
        cell_bytes(b"new-1", &[0x11; 90], 0x31, None),
        cell_bytes(b"new-2", &[0x22; 90], 0x32, None),
    ];
    let cells_added = written.change(&[
        Delta::Insert {
            slot: slot(cells + 1),
            cell: cell(&added[0]),
        },
        Delta::Insert {
            slot: slot(cells + 2),
            cell: cell(&added[1]),
        },
        Delta::Remove { slot: slot(2) },
    ]);
    let second = db.delta(tree, 3, Compression::Lz4, &cells_added);

    let plain = Written::filled(size, page(10), cells.min(6), 12).image();
    let plain_base = db.base(page(10), 1, Compression::Raw, &plain);

    let (raw, packed) = (page(8), page(9));
    let leaf = |id| {
        if full {
            full_flat_image(size, id)
        } else {
            flat_image(size, id, 4, 0x40)
        }
    };
    let (raw_leaf, packed_leaf) = (leaf(raw), leaf(packed));
    let raw_base = db.base(raw, 1, Compression::Raw, &raw_leaf);
    let packed_base = db.base(packed, 1, Compression::Lz4, &packed_leaf);

    assert!(
        (second.len as usize) < cells_added.len(),
        "the run is compressed"
    );
    assert_eq!(first.len as usize, hashes.len() + 8, "the run is raw");
    if full {
        let capacity = size.image_capacity();
        assert!(first_image.len() > capacity * 85 / 100 && raw_leaf.len() > capacity - 250);
        assert!(base.len as usize > capacity / 2);
    }
    assert!(
        (base.len as usize) < first_image.len(),
        "the tree image is compressed"
    );
    assert!(packed_base.len < raw_base.len || full);
    let stored = |of, at, flat, blocks: &[BlockLocation], expected| Stored {
        of,
        at,
        flat,
        blocks: blocks.to_vec(),
        expected,
    };
    vec![
        stored(tree, 3, false, &[base, first, second], written.image()),
        stored(page(10), 1, false, &[plain_base], plain),
        stored(raw, 1, true, &[raw_base], raw_leaf),
        stored(packed, 1, true, &[packed_base], packed_leaf),
    ]
}

/// The page of `stored` as a load into `pool` gives it.
fn loaded(db: &Db, pool: &BufferPool, stored: &Stored) -> Result<Vec<u8>, LoadError> {
    let frame = db
        .loader(pool)
        .get(stored.of, version(stored.at), &mut LoadBuffers::new())?;
    Ok(if stored.flat {
        frame[..stored.expected.len()].to_vec()
    } else {
        tree_image(&frame)
    })
}

/// Changes the byte at each place of each block of `stored` to each of
/// `values(place, byte)` in turn, where a place is counted from the block's
/// first byte, and checks that every load is refused by the block's
/// checksum or its length, with nothing put into the pool. Returns how
/// many changed bytes were tried, by the error.
fn corrupt_bytes(
    db: &Db,
    stored: &[Stored],
    mut values: impl FnMut(usize, u8) -> Vec<u8>,
) -> BTreeMap<&'static str, u64> {
    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let file = db.segment_file();
    let mut refused = BTreeMap::new();
    for page in stored {
        for &location in &page.blocks {
            let original = db.stored(location);
            for (at, &byte) in original.iter().enumerate() {
                let offset = location.offset + at as u64;
                for value in values(at, byte) {
                    assert_ne!(value, byte);
                    file.write_all_at(&[value], offset)
                        .expect("the byte is changed");
                    let error = loader
                        .get(page.of, version(page.at), &mut buffers)
                        .expect_err("a page with a changed byte");
                    assert!(
                        matches!(error, LoadError::Block { page: of, .. } if of == page.of),
                        "{error:?}"
                    );
                    let (block, error) = block_error(error);
                    assert_eq!(block, location);
                    let kind = match error {
                        FormatError::ChecksumMismatch { .. } => "checksum",
                        FormatError::Truncated { .. } if at < 4 => "longer than its location",
                        FormatError::RecordTooLong { .. } if at < 4 => "longer than a page",
                        FormatError::Segment(SegmentError::LengthMismatch { .. }) if at < 4 => {
                            "shorter than its location"
                        }
                        other => panic!("byte {at} of {location:?} as {value:#04x}: {other:?}"),
                    };
                    *refused.entry(kind).or_default() += 1;
                }
                file.write_all_at(&[byte], offset)
                    .expect("the byte is restored");
            }
            assert_eq!(db.stored(location), original);
        }
        assert_eq!(pool.census(), Default::default(), "nothing was installed");
        assert_eq!(
            loaded(db, &db.pool(2), page).expect("the page as it was written"),
            page.expected
        );
    }
    refused
}

/// How many blocks the chains of `stored` have, and how many bytes they
/// take in the segment.
fn blocks_of(stored: &[Stored]) -> (u64, u64) {
    let blocks = stored.iter().flat_map(|page| &page.blocks);
    (
        blocks.clone().count() as u64,
        blocks.map(|location| u64::from(location.len)).sum(),
    )
}

#[test]
fn a_page_with_any_single_corrupted_byte_is_rejected_with_a_typed_error() {
    let mut db = Db::new(PageSize::MIN);
    let stored = stored_pages(&mut db, false);
    for page in &stored {
        let pool = db.pool(2);
        assert_eq!(
            loaded(&db, &pool, page).expect("an intact page"),
            page.expected
        );
    }
    let (blocks, bytes) = blocks_of(&stored);
    assert_eq!(blocks, 6);

    let refused = corrupt_bytes(&db, &stored, |_, byte| {
        (0..=u8::MAX).filter(|&value| value != byte).collect()
    });
    println!("{bytes} bytes in {blocks} blocks, each changed to its 255 other values: {refused:?}");
    assert_eq!(refused.values().sum::<u64>(), bytes * 255);
    assert!(refused["checksum"] >= (bytes - blocks * 4) * 255);
}

/// Pages as long as a page gets, of each of the four page sizes. In the
/// smallest size every byte of every block is changed, to a value with one
/// bit flipped and to one other value. In the larger sizes the 8 bytes of
/// each block's frame and every 23rd byte behind them are.
#[test]
fn a_changed_byte_is_found_in_full_pages_of_every_size() {
    for (bytes, stride) in [(8_192, 1), (16_384, 23), (32_768, 23), (65_536, 23)] {
        let size = PageSize::new(bytes).expect("a page size");
        let mut db = Db::new(size);
        let stored = stored_pages(&mut db, true);
        let (blocks, bytes) = blocks_of(&stored);
        assert!(bytes > 5 * size.image_capacity() as u64 / 2);
        let mut rng = rng(size.get() as u64);
        let refused = corrupt_bytes(&db, &stored, |place, byte| {
            if place >= 8 && place % stride != 0 {
                return Vec::new();
            }
            let flipped = byte ^ 1 << rng.below(8);
            let other = byte ^ (1 + rng.below(255) as u8);
            if other == flipped {
                vec![other]
            } else {
                vec![flipped, other]
            }
        });
        let tried = refused.values().sum::<u64>();
        println!(
            "{} byte pages: {bytes} bytes in {blocks} blocks, {tried} changed bytes tried: \
             {refused:?}",
            size.get()
        );
        assert!(tried >= bytes / stride as u64);
    }
}
