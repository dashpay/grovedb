//! STORE-05's acceptance test: a page with any single corrupted byte is
//! rejected with a typed error.

use std::collections::BTreeMap;

use super::*;

/// One page version in segments: the blocks of its chain, and the page a
/// load of it gives.
struct Stored {
    of: PageId,
    at: u64,
    blocks: Vec<BlockLocation>,
    expected: Vec<u8>,
}

/// A tree page with a base image, a raw run of two changed hashes and a
/// compressed run that adds cells, and a flat page with a raw image and
/// one stored with compression. With `full`, the pages are as long as the
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
    let base = db.base(tree, 1, Compression::Lz4, &written.image());
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

    let (raw, packed) = (page(8), page(9));
    let leaf = if full {
        full_flat_image(size, 5)
    } else {
        flat_image(size, 4, 0x40)
    };
    let raw_base = db.base(raw, 1, Compression::Raw, &leaf);
    let packed_base = db.base(packed, 1, Compression::Lz4, &leaf);

    assert!(
        (second.len as usize) < cells_added.len(),
        "the run is compressed"
    );
    assert_eq!(first.len as usize, hashes.len() + 8, "the run is raw");
    if full {
        let capacity = size.image_capacity();
        assert!(written.image().len() > capacity * 85 / 100 && leaf.len() > capacity - 250);
        assert!(base.len as usize > capacity / 2);
    } else {
        assert!((base.len as usize) < written.image().len());
        assert!(packed_base.len < raw_base.len);
    }
    vec![
        Stored {
            of: tree,
            at: 3,
            blocks: vec![base, first, second],
            expected: written.image(),
        },
        Stored {
            of: raw,
            at: 1,
            blocks: vec![raw_base],
            expected: leaf.clone(),
        },
        Stored {
            of: packed,
            at: 1,
            blocks: vec![packed_base],
            expected: leaf,
        },
    ]
}

/// The page of `stored` as a load into `pool` gives it.
fn loaded(db: &Db, pool: &BufferPool, stored: &Stored) -> Result<Vec<u8>, LoadError> {
    let frame = db
        .loader(pool)
        .get(stored.of, version(stored.at), &mut LoadBuffers::new())?;
    Ok(if stored.blocks.len() > 1 {
        tree_image(&frame)
    } else {
        frame[..stored.expected.len()].to_vec()
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
    let mut tried = 0;
    for page in stored {
        for &location in &page.blocks {
            let original = db.stored(location);
            for (at, &byte) in original.iter().enumerate() {
                let offset = location.offset + at as u64;
                for value in values(at, byte) {
                    assert_ne!(value, byte);
                    file.write_all_at(&[value], offset)
                        .expect("the byte is changed");
                    tried += 1;
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
    assert_eq!(refused.values().sum::<u64>(), tried);
    refused
}

fn block_bytes(stored: &[Stored]) -> u64 {
    stored
        .iter()
        .flat_map(|page| &page.blocks)
        .map(|location| u64::from(location.len))
        .sum()
}

#[test]
fn a_page_with_any_single_corrupted_byte_is_rejected_with_a_typed_error() {
    let mut db = Db::new(TreePageSize::MIN);
    let stored = stored_pages(&mut db, false);
    for page in &stored {
        let pool = db.pool(2);
        assert_eq!(
            loaded(&db, &pool, page).expect("an intact page"),
            page.expected
        );
    }
    let bytes = block_bytes(&stored);

    let refused = corrupt_bytes(&db, &stored, |_, byte| {
        (0..=u8::MAX).filter(|&value| value != byte).collect()
    });
    println!("{bytes} bytes in 5 blocks, each changed to its 255 other values: {refused:?}");
    assert_eq!(refused.values().sum::<u64>(), bytes * 255);
    assert!(refused["checksum"] >= (bytes - 5 * 4) * 255);
}

/// Pages as long as a page gets, of every page size. In the smallest size
/// every byte of every block is changed, to a value with one bit flipped
/// and to one other value. In the larger sizes the 8 bytes of each block's
/// frame and every 23rd byte behind them are.
#[test]
fn a_changed_byte_is_found_in_full_pages_of_every_size() {
    for (size, stride) in [
        (TreePageSize::MIN, 1),
        (TreePageSize::DEFAULT, 23),
        (TreePageSize::MAX, 23),
    ] {
        let mut db = Db::new(size);
        let stored = stored_pages(&mut db, true);
        let bytes = block_bytes(&stored);
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
            "{} byte pages: {bytes} bytes in 5 blocks, {tried} changed bytes tried: {refused:?}",
            size.get()
        );
        assert!(tried >= bytes / stride as u64);
    }
}
