//! The read path, the load path, the clock and the frames beside the fixed
//! ones.

use lignin_format::PageSize;

use super::*;

#[test]
fn a_pool_is_as_many_frames_as_its_capacity_holds() {
    let config = PoolConfig {
        page_size: PageSize::DEFAULT,
        capacity: 1 << 20,
    };
    let pool = BufferPool::new(config).expect("a megabyte holds frames");
    assert_eq!(pool.frame_len(), 32_768);
    assert_eq!(pool.capacity(), 1 << 20);
    assert!((29..=31).contains(&pool.frames()), "{}", pool.frames());
    assert!(pool.fixed_memory() <= pool.capacity());
    assert!(pool.fixed_memory() > pool.frames() * pool.frame_len());
    assert_eq!(pool.memory(), pool.fixed_memory());
    assert_eq!(
        pool.overflow_cost() - pool.frame_len(),
        pool.overflow_cost() % 4096
    );
    assert_eq!(pool.stats(), PoolStats::default());
    assert_eq!(pool.census(), Census::default());
    check(&pool, 0);

    let mut blank = pool.blank();
    assert_eq!(blank.len(), 32_768);
    assert_eq!(blank.as_ptr() as usize % 4096, 0, "a frame is page-aligned");
    blank.fill(7);
    drop(blank);
    check(&pool, 0);
}

#[test]
fn every_page_size_of_a_store_is_served() {
    for size in [8_192, 16_384, 32_768, 65_536] {
        let config = PoolConfig {
            page_size: PageSize::new(size).expect("a page size"),
            capacity: 4 * size + 4_096,
        };
        let pool = BufferPool::new(config).expect("holds frames");
        assert_eq!(pool.frame_len(), size);
        assert_eq!(pool.frames(), 4);
        let mut blank = pool.blank();
        assert_eq!(blank.as_ptr() as usize % 4096, 0);
        blank.fill(0xA5);
        assert!(blank.iter().all(|&byte| byte == 0xA5));
    }
}

#[test]
fn a_capacity_below_one_frame_is_refused() {
    let config = PoolConfig {
        page_size: PageSize::MIN,
        capacity: 8_192,
    };
    let Err(error) = BufferPool::new(config) else {
        panic!("a pool of less than one frame was made");
    };
    let CacheError::CapacityTooSmall { capacity, frame } = error else {
        panic!("{error}");
    };
    assert_eq!(capacity, 8_192);
    assert!(frame > 8_192);
    let exact = PoolConfig {
        page_size: PageSize::MIN,
        capacity: frame,
    };
    assert_eq!(BufferPool::new(exact).expect("one frame").frames(), 1);
}

#[test]
fn a_capacity_no_allocation_can_have_is_refused() {
    let config = PoolConfig {
        page_size: PageSize::MIN,
        capacity: usize::MAX,
    };
    assert_eq!(
        BufferPool::new(config).err(),
        Some(CacheError::CapacityTooLarge)
    );
    assert_eq!(
        BufferPool::with_frames(usize::MAX / 2, 8, 1, 0).err(),
        Some(CacheError::CapacityTooLarge)
    );
}

#[test]
fn a_miss_is_loaded_and_then_hit() {
    let pool = pool(2);
    let miss = pool.get(page(7), version(4)).expect_err("an empty pool");
    assert_eq!(miss.page(), page(7));
    assert_eq!(miss.at(), version(4));

    let mut blank = pool.blank();
    assert_eq!(blank.len(), FRAME);
    content(7, 3, 0).write(&mut blank);
    let first = loaded(pool.install_loaded(blank, miss, version(3), None));
    assert_eq!(first.page(), page(7));
    assert_eq!(first.born(), version(3));
    assert_eq!(Content::read(&first), content(7, 3, 0));
    assert_eq!(first.as_ref().len(), FRAME);

    let hit = pool.get(page(7), version(4)).expect("a hit");
    assert_eq!(hit.as_ptr(), first.as_ptr());
    let later = pool.get(page(7), version(900)).expect("still current");
    assert_eq!(later.as_ptr(), first.as_ptr());
    assert!(pool.get(page(7), version(2)).is_err(), "before it was born");
    assert!(pool.get(page(8), version(4)).is_err(), "another page");

    assert_eq!(
        pool.stats(),
        PoolStats {
            hits: 2,
            misses: 3,
            evictions: 0,
            overflows: 0
        }
    );
    let census = pool.census();
    assert_eq!((census.published, census.pinned), (1, 1));
    assert_eq!(census.unevictable, 1);
    drop((first, hit, later));
    assert_eq!(pool.census().unevictable, 0);
    check(&pool, 0);
}

#[test]
fn the_versions_of_one_page_are_told_apart() {
    let pool = pool(4);
    let old = seed(&pool, content(1, 3, 0), Some(7));
    let new = seed(&pool, content(1, 7, 0), None);
    assert_ne!(old.as_ptr(), new.as_ptr());

    for at in 3..7 {
        let seen = pool.get(page(1), version(at)).expect("the old version");
        assert_eq!(Content::read(&seen), content(1, 3, 0));
    }
    for at in [7, 8, 1_000] {
        let seen = pool.get(page(1), version(at)).expect("the new version");
        assert_eq!(Content::read(&seen), content(1, 7, 0));
    }
    let miss = pool.get(page(1), version(2)).expect_err("before both");
    assert_eq!(miss.newer, 3, "the miss names the oldest newer version");

    assert_eq!(
        pool.get_version(page(1), version(3))
            .map(|seen| seen.born()),
        Some(version(3))
    );
    assert_eq!(
        pool.get_version(page(1), version(7))
            .map(|seen| seen.born()),
        Some(version(7))
    );
    assert!(pool.get_version(page(1), version(5)).is_none());
    assert!(pool.get_version(page(2), version(3)).is_none());
    check(&pool, 0);
}

#[test]
fn a_newer_version_in_the_pool_bounds_a_loaded_one() {
    let pool = pool(4);
    let _new = seed(&pool, content(1, 7, 0), None);
    let miss = pool.get(page(1), version(5)).expect_err("only the new one");
    let mut blank = pool.blank();
    content(1, 3, 0).write(&mut blank);
    let old = loaded(pool.install_loaded(blank, miss, version(3), None));
    assert_eq!(old.frame().superseded(), 7);
    assert_eq!(
        Content::read(&pool.get(page(1), version(6)).expect("old")),
        content(1, 3, 0)
    );
    assert_eq!(
        Content::read(&pool.get(page(1), version(7)).expect("new")),
        content(1, 7, 0)
    );
    check(&pool, 0);
}

#[test]
fn a_load_that_cannot_be_is_refused() {
    let pool = pool(4);
    let miss = pool.get(page(1), version(5)).expect_err("empty");

    let blank = pool.blank();
    assert_eq!(
        pool.install_loaded(blank, miss, version(6), None).err(),
        Some(CacheError::LoadedTooNew {
            page: page(1),
            born: version(6),
            at: version(5)
        })
    );
    let blank = pool.blank();
    assert_eq!(
        pool.install_loaded(blank, miss, version(3), Some(version(5)))
            .err(),
        Some(CacheError::LoadedSuperseded {
            page: page(1),
            born: version(3),
            superseded: version(5),
            at: version(5)
        })
    );
    let mut blank = pool.blank();
    blank.attach(name(1), value(b"pending"));
    assert_eq!(
        pool.install_loaded(blank, miss, version(3), None).err(),
        Some(CacheError::AttachedValues {
            page: page(1),
            born: version(3)
        })
    );
    assert_eq!(pool.attached_bytes(), 0);

    let _between = seed(&pool, content(1, 4, 0), Some(9));
    let blank = pool.blank();
    assert_eq!(
        pool.install_loaded(blank, miss, version(3), None).err(),
        Some(CacheError::LoadedSuperseded {
            page: page(1),
            born: version(3),
            superseded: version(4),
            at: version(5)
        })
    );
    assert_eq!(pool.census().published, 1);
    assert_eq!(lock(&pool.free).len(), 3, "every refused frame came back");
    check(&pool, 0);
}

#[test]
fn a_second_load_of_one_page_version_gives_the_first_frame() {
    let pool = pool(3);
    let first_miss = pool.get(page(1), version(5)).expect_err("empty");
    let second_miss = pool.get(page(1), version(6)).expect_err("empty");
    let mut first = pool.blank();
    let mut second = pool.blank();
    content(1, 3, 0).write(&mut first);
    content(1, 3, 0).write(&mut second);
    let first = loaded(pool.install_loaded(first, first_miss, version(3), None));
    let second = loaded(pool.install_loaded(second, second_miss, version(3), None));
    assert_eq!(first.as_ptr(), second.as_ptr());
    assert_eq!(pool.census().published, 1);
    assert_eq!(lock(&pool.free).len(), 2);
    check(&pool, 0);
}

#[test]
fn a_load_that_a_commit_overtook_is_made_again() {
    let pool = pool(4);
    let miss = pool.get(page(1), version(5)).expect_err("empty");

    // Another page of the same part of the table is committed meanwhile.
    // The pool can no longer tell that what was read of page 1 is still
    // the newest, so it takes nothing.
    drop(create(&pool, 2, 6));
    let mut blank = pool.blank();
    content(1, 3, 0).write(&mut blank);
    let again = pool
        .install_loaded(blank, miss, version(3), None)
        .expect("nothing wrong with the load");
    let Loaded::Again(blank) = again else {
        panic!("a load that a commit overtook was installed");
    };
    assert_eq!(
        Content::read(&blank),
        content(1, 3, 0),
        "the frame comes back"
    );
    assert_eq!(pool.census().published, 1);

    // Asked again, it misses again, and the second load goes in.
    let miss = pool.get(page(1), version(5)).expect_err("still not there");
    let current = loaded(pool.install_loaded(blank, miss, version(3), None));
    assert_eq!(current.frame().superseded(), NO_VERSION);
    assert!(pool.get(page(1), version(1_000)).is_ok());
    check(&pool, 0);
}

#[test]
fn a_load_that_a_freeing_commit_overtook_is_made_again() {
    let pool = pool(4);
    let mut disk = Disk::default();
    disk.write(content(1, 3, 0));
    let miss = pool.get(page(1), version(5)).expect_err("empty");
    let (old, next) = disk.lookup(1, 5).expect("on disk");
    assert_eq!(next, None);

    // Commit 6 frees the page: the page table first, then the pool.
    disk.free(1, 6);
    pool.free(page(1), version(6)).expect("a later commit");

    let mut blank = pool.blank();
    old.write(&mut blank);
    let again = pool
        .install_loaded(blank, miss, version(3), None)
        .expect("nothing wrong with the load");
    assert!(matches!(again, Loaded::Again(_)));
    drop(again);

    // The reader of version 5 still gets the page, now bounded by the free
    // link, and a reader of version 6 gets none.
    let seen = fetch(&pool, &disk, 1, 5).expect("on disk");
    assert_eq!(seen.frame().superseded(), 6);
    assert!(fetch(&pool, &disk, 1, 6).is_none());
    check(&pool, 0);
}

#[test]
fn a_load_that_a_page_step_overtook_is_made_again() {
    let pool = pool(1);
    let mut disk = Disk::default();
    disk.write(content(1, 3, 0));
    disk.write(content(2, 3, 0));

    // A reader misses page 1 and reads it from disk as it is there.
    let miss = pool.get(page(1), version(5)).expect_err("empty");
    let (stale, next) = disk.lookup(1, 5).expect("on disk");

    // Background work changes the page in a page step, write-back puts the
    // change into a segment, and the frame is evicted.
    let looked = fetch(&pool, &disk, 1, 5).expect("on disk");
    let mut blank = pool.blank();
    blank.copy_from(&looked);
    content(1, 3, 1).write(&mut blank);
    let step = pool.page_step(page(1));
    let changed = step.replace(&looked, blank).expect("nothing in the way");
    drop((step, looked));
    disk.write(Content::read(&changed));
    pool.mark_written(&changed).expect("owed");
    drop(changed);
    drop(fetch(&pool, &disk, 2, 5));
    assert!(pool.get(page(1), version(5)).is_err(), "evicted");

    // What the reader read is the page before the step. It is not taken.
    let mut blank = pool.blank();
    stale.write(&mut blank);
    let again = pool
        .install_loaded(blank, miss, version(stale.born), next.map(version))
        .expect("nothing wrong with the load");
    assert!(matches!(again, Loaded::Again(_)));
    drop(again);
    assert_eq!(
        Content::read(&fetch(&pool, &disk, 1, 5).expect("on disk")),
        content(1, 3, 1)
    );
    check(&pool, 0);
}

#[test]
fn the_clock_gives_a_used_page_a_second_chance() {
    let pool = pool(3);
    for raw in 1..=3 {
        drop(seed(&pool, content(raw, 1, 0), None));
    }
    assert_eq!(lock(&pool.free).len(), 0);

    // Every page was used when it was loaded. The hand takes each mark off,
    // comes round, and evicts the first frame.
    drop(seed(&pool, content(4, 1, 0), None));
    assert!(pool.get(page(1), version(1)).is_err(), "page 1 was evicted");
    assert_eq!(pool.stats().evictions, 1);

    // Page 2 is used again and page 3 is not, so the hand passes page 2.
    drop(pool.get(page(2), version(1)).expect("still there"));
    drop(seed(&pool, content(5, 1, 0), None));
    assert!(
        pool.get(page(2), version(1)).is_ok(),
        "page 2 was passed over"
    );
    assert!(pool.get(page(3), version(1)).is_err(), "page 3 was evicted");
    assert_eq!(pool.stats().evictions, 2);
    assert_eq!(pool.overflow_frames(), 0);
    assert_eq!(pool.census().published, 3);
    check(&pool, 0);
}

#[test]
fn a_pinned_page_is_not_evicted_and_a_frame_beside_the_fixed_ones_serves() {
    let pool = pool(2);
    let one = seed(&pool, content(1, 1, 0), None);
    let two = seed(&pool, content(2, 1, 0), None);
    assert_eq!(pool.memory(), pool.fixed_memory());

    let three = seed(&pool, content(3, 1, 0), None);
    assert_eq!(pool.overflow_frames(), 1);
    assert_eq!(pool.stats().overflows, 1);
    assert_eq!(pool.memory(), pool.fixed_memory() + pool.overflow_cost());
    let census = pool.census();
    assert_eq!(
        (census.published, census.pinned, census.unevictable),
        (3, 3, 3)
    );
    assert_eq!((census.overflow, census.overflow_unevictable), (1, 1));
    check(&pool, 0);

    // Each is still the page it was.
    assert_eq!(Content::read(&one), content(1, 1, 0));
    assert_eq!(Content::read(&two), content(2, 1, 0));
    assert_eq!(Content::read(&three), content(3, 1, 0));

    // A second pin keeps the frame, and the last one lets it go at once.
    let again = three.clone();
    drop(three);
    assert_eq!(pool.overflow_frames(), 1);
    assert_eq!(Content::read(&again), content(3, 1, 0));
    drop(again);
    assert_eq!(pool.overflow_frames(), 0);
    assert_eq!(pool.memory(), pool.fixed_memory());
    assert!(pool.get(page(3), version(1)).is_err(), "it was not kept");
    assert_eq!(pool.census().published, 2);
    check(&pool, 0);

    // The fixed frames stay cached when their pins go.
    drop((one, two));
    assert!(pool.get(page(1), version(1)).is_ok());
    assert!(pool.get(page(2), version(1)).is_ok());
    check(&pool, 0);
}

#[test]
fn a_blank_beside_the_fixed_frames_is_given_back_when_dropped() {
    let pool = pool(1);
    let held = seed(&pool, content(1, 1, 0), None);
    let blank = pool.blank();
    assert_eq!(pool.overflow_frames(), 1);
    check(&pool, 1);
    drop(blank);
    assert_eq!(pool.overflow_frames(), 0);
    drop(held);
    check(&pool, 0);
}

#[test]
fn a_pool_that_is_dropped_frees_what_is_beside_its_fixed_frames() {
    let pool = pool(1);
    let copy = pool.new_private(owner(1), page(1)).expect("a new page");
    let other = pool.new_private(owner(1), page(2)).expect("a new page");
    other
        .write()
        .expect("private")
        .attach(name(1), value(&[0; 100]));
    assert_eq!(pool.overflow_frames(), 1);
    assert_eq!(pool.attached_bytes(), 100);
    drop((copy, other));
    assert_eq!(pool.overflow_frames(), 1, "a private copy stays");
    check(&pool, 0);
    drop(pool);
}

#[test]
fn the_handles_cross_threads() {
    fn send_and_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_and_sync::<BufferPool>();
    send_and_sync::<Miss>();
    #[cfg(not(loom))]
    {
        send_and_sync::<PageRef<'_>>();
        send_and_sync::<PrivateRef<'_>>();
        send::<Blank<'_>>();
    }
    let _ = send::<()>;
}

#[test]
fn everything_can_be_shown() {
    let pool = pool(2);
    let shown = seed(&pool, content(1, 3, 0), None);
    let copy = pool
        .make_private(owner(9), page(1), version(3))
        .expect("in the pool");
    let texts = [
        format!("{pool:?}"),
        format!("{shown:?}"),
        format!("{copy:?}"),
        format!("{:?}", copy.read()),
        format!("{:?}", copy.write().expect("private")),
        format!("{:?}", pool.blank()),
        format!("{:?}", pool.page_step(page(1))),
        format!("{:?}", shown.frame()),
        format!("{:?}", shown.pointer()),
        format!("{:?}", pool.arena),
        format!("{:?}", pool.get(page(5), version(1))),
        format!("{:?}", Loaded::Again(pool.blank())),
        format!("{:?} {:?}", pool.stats(), pool.census()),
        format!(
            "{:?}",
            PoolConfig {
                page_size: PageSize::DEFAULT,
                capacity: 1
            }
        ),
    ];
    for (text, expected) in texts.iter().zip([
        "BufferPool",
        "PageRef",
        "PrivateRef",
        "SharedLatch",
        "ExclusiveLatch",
        "Blank",
        "PageStep",
        "Frame",
        "FramePtr",
        "Arena",
        "Miss",
        "Again",
        "PoolStats",
        "PoolConfig",
    ]) {
        assert!(text.contains(expected), "{text}");
    }
    assert_eq!(owner(9).to_string(), "owner 9");
    assert_eq!(owner(9).get(), 9);
}
