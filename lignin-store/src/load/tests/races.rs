//! Loads beside other loads, commits and write-back.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::*;

/// The hash a test gives the first cell of a page at the commit `born`.
fn mark(born: u64) -> [u8; 32] {
    let mut hash = [born as u8; 32];
    hash[..8].copy_from_slice(&born.to_le_bytes());
    hash
}

fn marked(hash: &[u8; 32]) -> Delta<'_> {
    Delta::Hash {
        slot: slot(1),
        field: HashField::Value,
        hash,
    }
}

fn mark_of(frame: &[u8]) -> [u8; 32] {
    let page = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
    *page.cell(slot(1)).expect("the first cell").value_hash()
}

/// Has the commit `born` change `of` in the pool: a private copy of the
/// page as a reader at `before` sees it, changed and installed.
fn commit(size: PageSize, pool: &BufferPool, of: PageId, before: u64, born: u64) -> PageRef<'_> {
    let copy = pool
        .make_private(OwnerId::new(born), of, version(before))
        .expect("the page is in the pool");
    {
        let mut latch = copy.write().expect("a private copy");
        let mut page = TreePageMut::open(&mut latch, size, FRAME_FORMAT).expect("a page");
        marked(&mark(born))
            .apply(&mut page)
            .expect("the change is made");
    }
    pool.install(&copy, version(born))
        .expect("the commit installs")
}

fn one_page(db: &mut Db) -> Written {
    let mut written = Written::filled(db.size, page(7), 3, 10);
    written.apply(&marked(&mark(1)));
    db.base(page(7), 1, Compression::Lz4, &written.image());
    written
}

#[test]
fn two_loads_of_one_page_version_give_one_frame() {
    let mut db = Db::new(PageSize::MIN);
    one_page(&mut db);
    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let first = pool.get(page(7), version(1)).unwrap_err();
    let second = pool.get(page(7), version(4)).unwrap_err();
    assert_eq!((first.page(), first.at()), (page(7), version(1)));

    let one = loader.load(first, &mut buffers).unwrap();
    let other = loader.load(second, &mut buffers).unwrap();
    assert_eq!(one.as_ptr(), other.as_ptr());
    assert_eq!((one.born(), other.born()), (version(1), version(1)));
    assert_eq!(pool.census().published, 1);
    let stats = pool.stats();
    assert_eq!((stats.hits, stats.misses), (0, 2));
}

#[test]
fn a_load_that_a_commit_overtook_asks_the_pool_again() {
    let mut db = Db::new(PageSize::MIN);
    one_page(&mut db);
    let pool = db.pool(4);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let before = pool.get(page(7), version(1)).unwrap_err();

    let held = loader.get(page(7), version(1), &mut buffers).unwrap();
    let new = commit(db.size, &pool, page(7), 1, 2);
    let hits = pool.stats().hits;

    let loaded = loader.load(before, &mut buffers).unwrap();
    assert_eq!(loaded.as_ptr(), held.as_ptr());
    assert_eq!((loaded.born(), mark_of(&loaded)), (version(1), mark(1)));
    assert_eq!(pool.stats().hits, hits + 1);
    assert_eq!(mark_of(&new), mark(2));
    assert_eq!(pool.census().published, 2);
}

#[test]
fn a_load_that_a_commit_overtook_is_made_again_if_the_page_is_still_not_there() {
    let mut db = Db::new(PageSize::MIN);
    one_page(&mut db);
    let pool = db.pool(2);
    let loader = db.loader(&pool);
    let mut buffers = LoadBuffers::new();
    let before = pool.get(page(7), version(1)).unwrap_err();

    let held = loader.get(page(7), version(1), &mut buffers).unwrap();
    let new = commit(db.size, &pool, page(7), 1, 2);
    drop((held, new));
    drop(pool.blank());
    assert!(pool.get_version(page(7), version(1)).is_none());
    assert!(pool.get_version(page(7), version(2)).is_some());
    let misses = pool.stats().misses;

    let loaded = loader.load(before, &mut buffers).unwrap();
    assert_eq!((loaded.born(), mark_of(&loaded)), (version(1), mark(1)));
    assert_eq!(pool.stats().misses, misses + 1);
    assert!(pool.get(page(7), version(1)).is_ok());
    assert_eq!(mark_of(&pool.get(page(7), version(2)).unwrap()), mark(2));
    assert_eq!(mark_of(&pool.get(page(7), version(9)).unwrap()), mark(2));
}

/// A commit frees the page after the load has read the page table and
/// before it reaches the pool: the load is made again, and what it installs
/// ends at the freeing version.
#[test]
fn a_page_freed_while_it_is_loaded_ends_at_the_free() {
    let mut db = Db::new(PageSize::MIN);
    one_page(&mut db);
    let pool = db.pool(2);
    let loader = db.loader(&pool);
    db.vfs.take_calls();
    let at_the_block = db.vfs.stop_after(0, Op::Read, SEGMENT);

    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let frame = loader.get(page(7), version(1), &mut LoadBuffers::new())?;
            Ok::<_, LoadError>((frame.born(), mark_of(&frame)))
        });
        at_the_block.wait();
        db.table.push(page(7), Link::free(version(2))).unwrap();
        pool.free(page(7), version(2)).unwrap();
        at_the_block.go_on();
        assert_eq!(reader.join().unwrap().unwrap(), (version(1), mark(1)));
    });

    assert_eq!(pool.stats().misses, 2);
    let block_reads = db.vfs.take_calls();
    assert_eq!(
        block_reads,
        [
            (Op::Read, SEGMENT.to_owned()),
            (Op::Read, SEGMENT.to_owned())
        ],
        "the block is read again for the second fill"
    );
    assert!(pool.get(page(7), version(1)).is_ok());
    assert!(pool.get(page(7), version(2)).is_err());
    assert!(matches!(
        loader.get(page(7), version(2), &mut LoadBuffers::new()).unwrap_err(),
        LoadError::NoPage { freed: Some(freed), .. } if freed == version(2)
    ));
    assert_eq!(pool.census().published, 1);
}

/// Says that the commits are over when it is dropped, also by a panic.
struct Over<'a>(&'a AtomicBool);

impl Drop for Over<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// One thread commits changes to three pages and writes each back, as a
/// run or as a new image, while readers ask for the pages at the versions
/// that were published, in a pool too small to keep them. A load counts as
/// beside the commits when it asked at a version a commit of this test
/// published and ended before the commits did. The commits go on until 500
/// loads were, and one commit more.
#[test]
fn readers_load_while_commits_and_write_back_go_on() {
    const PAGES: u64 = 3;
    const COMMITS: u64 = 800;
    const MOST_COMMITS: u64 = 40_000;
    const BESIDE: u64 = 500;
    const READERS: u64 = 4;

    /// The commit that bore the version of page `index` a reader at `at`
    /// sees: commit `v` changes page `v % PAGES`, and commit 1 wrote all.
    fn born_at(index: u64, at: u64) -> u64 {
        at.checked_sub((at + PAGES - index) % PAGES)
            .filter(|&last| last >= 2)
            .unwrap_or(1)
    }

    let mut db = Db::new(PageSize::MIN);
    let ids: Vec<PageId> = (0..PAGES).map(|index| page(10 + index as u32)).collect();
    for &id in &ids {
        let mut written = Written::filled(db.size, id, 4, 30);
        written.apply(&marked(&mark(1)));
        db.base(id, 1, Compression::Raw, &written.image());
    }
    let pool = db.pool(4);
    let Db {
        table,
        segments,
        writer,
        size,
        ..
    } = &mut db;
    let (table, segments, size) = (&*table, &*segments, *size);
    let loader = PageLoader::new(&pool, table, segments, size).unwrap();
    let published = AtomicU64::new(1);
    let over = AtomicBool::new(false);
    let beside = AtomicU64::new(0);

    let last = std::thread::scope(|scope| {
        let commits_end = Over(&over);
        for reader in 0..READERS {
            let (ids, published, over, beside) = (&ids, &published, &over, &beside);
            scope.spawn(move || {
                let mut rng = SimRng::derive(11, reader, 0);
                let mut buffers = LoadBuffers::new();
                while !over.load(Ordering::Acquire) {
                    let latest = published.load(Ordering::Acquire);
                    let at = latest - rng.below(latest.min(12));
                    let index = rng.below(PAGES);
                    let frame = loader
                        .get(ids[index as usize], version(at), &mut buffers)
                        .expect("a published page version");
                    let born = born_at(index, at);
                    assert_eq!(frame.born(), version(born), "page {index} at {at}");
                    assert_eq!(mark_of(&frame), mark(born), "page {index} at {at}");
                    if latest >= 2 && !over.load(Ordering::Acquire) {
                        beside.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }

        let mut buffers = LoadBuffers::new();
        let mut born = 1;
        let mut enough_at = None;
        loop {
            if enough_at.is_none() && beside.load(Ordering::Relaxed) >= BESIDE {
                enough_at = Some(born);
            }
            let committed_since = enough_at.is_some_and(|enough_at| born > enough_at);
            if (born >= COMMITS && committed_since) || born >= MOST_COMMITS {
                break;
            }
            born += 1;
            let index = born % PAGES;
            let id = ids[index as usize];
            let before = born_at(index, born - 1);
            let current = loader.get(id, version(born - 1), &mut buffers).unwrap();
            assert_eq!(current.born(), version(before));
            let new = commit(size, &pool, id, born - 1, born);
            drop(current);
            published.store(born, Ordering::Release);

            let link = if born % 16 < PAGES {
                let image = tree_image(&new);
                let block = writer
                    .append(id, BlockContent::Base, Compression::Lz4, &image)
                    .unwrap();
                Link::base(version(born), block)
            } else {
                let mut run = Vec::new();
                let mut records = DeltaRunWriter::new(id, None, &mut run);
                records.push(&marked(&mark(born))).unwrap();
                records.finish().unwrap();
                let block = writer
                    .append(id, BlockContent::Delta, Compression::Raw, &run)
                    .unwrap();
                Link::delta(version(born), block)
            };
            table.push(id, link).unwrap();
            pool.mark_written(&new).unwrap();
        }
        drop(commits_end);
        assert!(enough_at.is_some_and(|enough_at| born > enough_at));
        born
    });

    let stats = pool.stats();
    let beside = beside.load(Ordering::Relaxed);
    println!("{last} commits, {beside} loads by readers beside them, {stats:?}");
    assert!(beside >= BESIDE, "{beside} loads beside {last} commits");
    assert!(stats.evictions > 100, "{stats:?}");
    let mut buffers = LoadBuffers::new();
    for index in 0..PAGES {
        for at in 1..=last {
            let frame = loader
                .get(ids[index as usize], version(at), &mut buffers)
                .unwrap();
            assert_eq!(mark_of(&frame), mark(born_at(index, at)));
        }
    }
}
