//! A loaded page is its base image with the runs after it applied in
//! order, whatever the chain went through.
//!
//! The commits of the model reach the page table and no pool, so a pool
//! is used only while the table stays as it is.

use super::*;

/// A change to a tree page, with what it borrows.
enum Change {
    Insert(SlotId, Vec<u8>),
    Remove(SlotId),
    Hash(SlotId, HashField, [u8; 32]),
    Value(SlotId, Vec<u8>),
}

impl Change {
    fn delta(&self) -> Delta<'_> {
        match self {
            Self::Insert(slot, bytes) => Delta::Insert {
                slot: *slot,
                cell: cell(bytes),
            },
            Self::Remove(slot) => Delta::Remove { slot: *slot },
            Self::Hash(slot, field, hash) => Delta::Hash {
                slot: *slot,
                field: *field,
                hash,
            },
            Self::Value(slot, value) => Delta::Value {
                slot: *slot,
                value: CellValue::Inline(value),
            },
        }
    }

    /// A change that `written` takes.
    fn random(written: &Written, serial: &mut u32, rng: &mut SimRng) -> Self {
        let page = written.page();
        let slots: Vec<SlotId> = page.cells().map(|(slot, _)| slot).collect();
        let free = page.free_bytes(written.size);
        let any = |rng: &mut SimRng| slots[rng.below(slots.len() as u64) as usize];
        let hash = junk_hash(rng);
        match rng.below(10) {
            0..=2 if free > 400 => {
                *serial += 1;
                let key = format!("k{serial:06}");
                let value = vec![*serial as u8; rng.below(200) as usize];
                let left = rng.below(3) == 0;
                Self::Insert(
                    page.lowest_free_slot(),
                    cell_bytes(
                        key.as_bytes(),
                        &value,
                        hash[0],
                        left.then(|| link(900 + *serial, &hash)),
                    ),
                )
            }
            3 | 4 if slots.len() > 1 => Self::Remove(any(rng)),
            5 if free > 400 => Self::Value(any(rng), vec![hash[1]; rng.below(300) as usize]),
            6 => Self::Hash(any(rng), HashField::KeyValue, hash),
            _ => Self::Hash(any(rng), HashField::Value, hash),
        }
    }
}

/// Makes one to four changes to `written` and returns the run that
/// records them.
fn random_run(written: &mut Written, serial: &mut u32, rng: &mut SimRng) -> Vec<u8> {
    let mut run = Vec::new();
    let mut records = DeltaRunWriter::new(written.id, None, &mut run);
    for _ in 0..1 + rng.below(4) {
        let change = Change::random(written, serial, rng);
        let delta = change.delta();
        written.apply(&delta);
        records.push(&delta).unwrap();
    }
    records.finish().unwrap();
    run
}

/// One page of the model: what each commit that changed it left of it.
struct Page {
    id: PageId,
    flat: bool,
    written: Option<Written>,
    /// The blocks the page's chain got since the page was last created,
    /// each with the image the chain up to it gives.
    links: Vec<(BlockLocation, Vec<u8>)>,
    /// The version of each commit that changed the page, with the image it
    /// left, or `None` for the commit that freed it.
    history: Vec<(u64, Option<Vec<u8>>)>,
}

impl Page {
    fn at(&self, at: u64) -> Option<&(u64, Option<Vec<u8>>)> {
        self.history.iter().rfind(|(born, _)| *born <= at)
    }

    fn free(&mut self, db: &Db, born: u64) {
        db.table.push(self.id, Link::free(version(born))).unwrap();
        self.written = None;
        self.links.clear();
        self.history.push((born, None));
    }

    fn linked(&mut self, location: BlockLocation, born: u64, image: Vec<u8>) {
        self.links.push((location, image.clone()));
        match self.history.last_mut() {
            Some((newest, left)) if *newest == born => *left = Some(image),
            _ => self.history.push((born, Some(image))),
        }
    }
}

fn compression(rng: &mut SimRng) -> Compression {
    if rng.below(2) == 0 {
        Compression::Raw
    } else {
        Compression::Lz4
    }
}

/// Has the commit `born` change `page` and stores what write-back makes of
/// it: a run or a new image. Before that, sometimes, a link the chain
/// already has is consolidated into an image (`page-table.md` T9). After
/// it, sometimes, a page step changes the page under the same version
/// (`transactions.md` PV6).
fn commit(db: &mut Db, page: &mut Page, born: u64, serial: &mut u32, rng: &mut SimRng) {
    let size = db.size;
    if page.flat {
        if !page.links.is_empty() && rng.below(5) == 0 {
            page.free(db, born);
            return;
        }
        let image = flat_image(
            size,
            page.id,
            1 + rng.below(40) as usize,
            rng.next_u64() as u8,
        );
        let location = db.base(page.id, born, compression(rng), &image);
        page.linked(location, born, image);
        return;
    }
    let Some(written) = &mut page.written else {
        let written = Written::filled(
            size,
            page.id,
            1 + rng.below(6) as u16,
            rng.below(90) as usize,
        );
        let image = written.image();
        let location = db.base(page.id, born, compression(rng), &image);
        page.written = Some(written);
        page.linked(location, born, image);
        return;
    };
    if rng.below(8) == 0 {
        page.free(db, born);
        return;
    }
    if rng.below(4) == 0 {
        let (through, image) = &page.links[rng.below(page.links.len() as u64) as usize];
        let base = db.append(page.id, BlockContent::Base, compression(rng), image);
        db.table.consolidate(page.id, *through, base).unwrap();
    }
    let run = random_run(written, serial, rng);
    let image = written.image();
    let location = if rng.below(6) == 0 {
        db.base(page.id, born, compression(rng), &image)
    } else {
        db.delta(page.id, born, compression(rng), &run)
    };
    let step = (rng.below(5) == 0).then(|| (random_run(written, serial, rng), written.image()));
    page.linked(location, born, image);
    if let Some((run, image)) = step {
        let location = db.delta(page.id, born, compression(rng), &run);
        page.linked(location, born, image);
    }
}

/// Loads `page` at `at` and checks it against the model.
fn check(db: &Db, pool: &BufferPool, buffers: &mut LoadBuffers, page: &Page, at: u64) -> bool {
    let loaded = db.loader(pool).get(page.id, version(at), buffers);
    let mut blocks = Vec::new();
    let in_table = db.table.read(page.id, version(at), &mut blocks);
    match page.at(at) {
        Some((born, Some(image))) => {
            let frame = loaded.expect("a page version in a segment");
            assert_eq!((frame.page(), frame.born()), (page.id, version(*born)));
            assert_eq!(in_table, Some(version(*born)));
            if page.flat {
                assert_eq!(frame[..image.len()], image[..]);
                assert!(frame[image.len()..].iter().all(|&byte| byte == 0));
            } else {
                assert_eq!(&tree_image(&frame), image, "page {} at {at}", page.id);
            }
            true
        }
        other => {
            let freed = other.map(|(born, _)| version(*born));
            assert_eq!(in_table, None);
            match loaded.unwrap_err() {
                LoadError::NoPage {
                    page: of,
                    at: asked,
                    freed: found,
                } => assert_eq!((of, asked, found), (page.id, version(at), freed)),
                other => panic!("{other:?}"),
            }
            false
        }
    }
}

#[test]
fn a_page_is_its_base_image_and_the_runs_after_it_in_order() {
    const VERSIONS: u64 = 160;
    for (size, seed) in [
        (PageSize::MIN, 1),
        (PageSize::DEFAULT, 2),
        (PageSize::MAX, 3),
    ] {
        let mut db = Db::new(size);
        let mut rng = rng(seed);
        let mut pages: Vec<Page> = (1..=6)
            .map(|raw| Page {
                id: page(raw),
                flat: raw > 4,
                written: None,
                links: Vec::new(),
                history: Vec::new(),
            })
            .collect();
        let mut buffers = LoadBuffers::new();
        let mut serial = 0;
        let (mut loaded, mut hits, mut evictions) = (0, 0, 0);
        for born in 1..=VERSIONS {
            let changed = rng.below(pages.len() as u64) as usize;
            commit(&mut db, &mut pages[changed], born, &mut serial, &mut rng);
            let pool = db.pool(2);
            for _ in 0..8 {
                let page = &pages[rng.below(pages.len() as u64) as usize];
                let at = 1 + rng.below(born + 1);
                loaded += u64::from(check(&db, &pool, &mut buffers, page, at));
            }
            let stats = pool.stats();
            hits += stats.hits;
            evictions += stats.evictions;
        }
        assert!(
            hits > 20 && evictions > 100,
            "{hits} hits, {evictions} evictions"
        );
        assert!(loaded > VERSIONS * 3);

        let cold = db.pool(1);
        for page in &pages {
            for at in 1..=VERSIONS + 1 {
                check(&db, &cold, &mut buffers, page, at);
            }
        }
        let stats = cold.stats();
        assert!(stats.evictions > 100, "{stats:?}");
    }
}

#[test]
fn every_hash_in_a_frame_is_the_hash_a_block_held() {
    let mut db = Db::new(PageSize::DEFAULT);
    let mut rng = rng(7);
    let hashes: Vec<[u8; 32]> = (0..40).map(|_| junk_hash(&mut rng)).collect();
    let mut written = Written::new(db.size, page(7));
    for n in 0..10usize {
        let key = format!("node-{n}");
        let bytes = CellContent {
            key: key.as_bytes(),
            feature: &[],
            value_hash: &hashes[4 * n],
            kv_hash: &hashes[4 * n + 1],
            left: Some(link(50, &hashes[4 * n + 2])),
            right: Some(link(60, &hashes[4 * n + 3])),
            hosted_roots: &[],
            value: CellValue::Inline(b"a value that no hash here is the hash of"),
            fragment_root: n == 0,
        }
        .encode()
        .unwrap();
        written.apply(&Delta::Insert {
            slot: slot(1 + n as u16),
            cell: cell(&bytes),
        });
    }
    db.base(page(7), 1, Compression::Lz4, &written.image());
    let changed: Vec<[u8; 32]> = (0..4).map(|_| junk_hash(&mut rng)).collect();
    let fields = [
        HashField::Value,
        HashField::KeyValue,
        HashField::Child(Side::Left),
        HashField::Child(Side::Right),
    ];
    let changes: Vec<Delta<'_>> = fields
        .iter()
        .zip(&changed)
        .map(|(&field, hash)| Delta::Hash {
            slot: slot(3),
            field,
            hash,
        })
        .collect();
    let run = written.change(&changes);
    db.delta(page(7), 2, Compression::Raw, &run);

    for at in [1, 2] {
        let frame = load(&db, page(7), at).unwrap();
        let loaded = TreePage::from_validated(&frame, FRAME_FORMAT).unwrap();
        assert_eq!(loaded.cell_count(), 10);
        for (n, (found, cell)) in loaded.cells().enumerate() {
            assert_eq!(found, slot(1 + n as u16));
            let stored = |field: usize| {
                if at == 2 && n == 2 {
                    &changed[field]
                } else {
                    &hashes[4 * n + field]
                }
            };
            for (index, &field) in fields.iter().enumerate() {
                assert_eq!(
                    cell.hash(field),
                    Some(stored(index)),
                    "{field:?} of cell {n}"
                );
            }
            assert_eq!(
                cell.value(),
                CellValue::Inline(b"a value that no hash here is the hash of")
            );
        }
    }
}

#[test]
fn a_loaded_frame_is_the_image_and_zeros_behind_it() {
    let mut db = Db::new(PageSize::MIN);
    let full = Written::filled(db.size, page(1), 19, 280).image();
    assert!(full.len() > 7_000);
    let small = Written::filled(db.size, page(2), 1, 3).image();
    let flat = flat_image(db.size, page(3), 2, 9);
    db.base(page(1), 1, Compression::Raw, &full);
    db.base(page(2), 1, Compression::Lz4, &small);
    db.base(page(3), 1, Compression::Raw, &flat);

    let pool = db.pool(1);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    for (raw, image) in [(1, &full), (2, &small), (1, &full), (3, &flat)] {
        let frame = loader.get(page(raw), version(1), &mut buffers).unwrap();
        assert_eq!(frame.len(), db.size.get());
        assert_eq!(frame[..image.len()], image[..]);
        assert!(frame[image.len()..].iter().all(|&byte| byte == 0));
    }
    assert_eq!(pool.stats().evictions, 3);
    assert_eq!(pool.overflow_frames(), 0);
}

/// The blocks of a chain were written at different times and lie apart,
/// so the file layer is told of all of them before the first is read:
/// each block by its place and its length, in the order of the chain. A
/// page version that is one block is read with no hint.
#[test]
fn the_blocks_of_a_chain_are_named_to_the_file_layer_before_the_first_is_read() {
    let mut db = Db::new(PageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 3, 10);
    let base = db.base(page(7), 1, Compression::Raw, &written.image());
    let run = written.change(&[Delta::Remove { slot: slot(2) }]);
    let first = db.delta(page(7), 3, Compression::Raw, &run);
    let run = written.change(&[Delta::Hash {
        slot: slot(1),
        field: HashField::Value,
        hash: &[9; 32],
    }]);
    let second = db.delta(page(7), 4, Compression::Raw, &run);
    let alone = Written::filled(db.size, page(8), 1, 3).image();
    db.base(page(8), 1, Compression::Raw, &alone);

    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    db.vfs.take_calls();

    // A page of one block, and a version of the other page that is its
    // base image alone: a read each, and no hint.
    drop(loader.get(page(8), version(1), &mut buffers).unwrap());
    drop(loader.get(page(7), version(2), &mut buffers).unwrap());
    assert_eq!(db.vfs.take_hints(), []);
    assert_eq!(db.vfs.take_calls().len(), 2);

    // The version of three blocks: three hints, and then the three reads.
    let whole = loader.get(page(7), version(4), &mut buffers).unwrap();
    assert_eq!(tree_image(&whole), written.image());
    let named = |location: BlockLocation| Hint {
        after_calls: 0,
        name: SEGMENT.to_owned(),
        offset: location.offset,
        len: u64::from(location.len),
    };
    assert_eq!(
        db.vfs.take_hints(),
        [named(base), named(first), named(second)]
    );
    let read = (Op::Read, SEGMENT.to_owned());
    assert_eq!(db.vfs.take_calls(), [read.clone(), read.clone(), read]);

    // What a hint names must lie in the blocks of a segment of the store:
    // a location of no segment, and one past the end of its segment, are
    // passed over, and one block alone is not named.
    let nowhere = BlockLocation {
        file: FileId::new(9).expect("an id"),
        ..base
    };
    let past = BlockLocation {
        offset: u64::MAX - 5,
        ..second
    };
    db.segments.will_read(&[nowhere, base, past]);
    db.segments.will_read(&[first]);
    assert_eq!(db.vfs.take_hints(), [named(base)]);
}

/// A page version that is read from its segments is one load, whoever
/// asked for it. A writer's first copy of a page that the pool does not
/// hold asks the pool for a copy and not for the page, so the pool counts
/// no miss for it, and the load is counted all the same. An ask for a
/// page that no segment holds is a miss, and nothing is read for it.
#[test]
fn a_page_that_is_read_is_counted_as_a_load_whoever_asked() {
    let mut db = Db::new(PageSize::MIN);
    let image = Written::filled(db.size, page(7), 1, 3).image();
    db.base(page(7), 1, Compression::Raw, &image);
    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let owner = OwnerId::new(2);
    let counted = || {
        let stats = pool.stats();
        (stats.misses, stats.loads)
    };

    let miss = pool
        .make_private(owner, page(7), version(1))
        .expect_err("not in the pool");
    assert_eq!(counted(), (0, 0));
    let loaded = loader.load(miss, &mut buffers).expect("a load");
    let copy = pool
        .make_private(owner, page(7), version(1))
        .expect("the page is in the pool");
    assert_eq!(counted(), (0, 1));
    drop((loaded, copy));

    let refused = loader.get(page(9), version(1), &mut buffers);
    assert!(matches!(refused, Err(LoadError::NoPage { .. })));
    assert_eq!(counted(), (1, 1));
}

#[test]
fn a_loaded_page_stands_for_the_versions_up_to_the_next_link() {
    let mut db = Db::new(PageSize::MIN);
    let mut written = Written::filled(db.size, page(7), 3, 10);
    let first = written.image();
    db.base(page(7), 1, Compression::Raw, &first);
    let run = written.change(&[Delta::Remove { slot: slot(2) }]);
    db.delta(page(7), 3, Compression::Raw, &run);
    db.table.push(page(7), Link::free(version(5))).unwrap();

    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let old = loader.get(page(7), version(2), &mut buffers).unwrap();
    assert_eq!((old.born(), tree_image(&old)), (version(1), first));
    assert_eq!(
        pool.get(page(7), version(1)).unwrap().as_ptr(),
        old.as_ptr()
    );
    assert!(pool.get(page(7), version(3)).is_err());

    let new = loader.get(page(7), version(4), &mut buffers).unwrap();
    assert_eq!(
        (new.born(), tree_image(&new)),
        (version(3), written.image())
    );
    assert_eq!(
        pool.get(page(7), version(3)).unwrap().as_ptr(),
        new.as_ptr()
    );
    assert!(pool.get(page(7), version(5)).is_err());
    assert!(matches!(
        loader.get(page(7), version(5), &mut buffers).unwrap_err(),
        LoadError::NoPage { freed: Some(freed), .. } if freed == version(5)
    ));
    let stats = pool.stats();
    assert_eq!((stats.hits, stats.misses), (2, 5));
    assert_eq!(pool.census().published, 2);
}
