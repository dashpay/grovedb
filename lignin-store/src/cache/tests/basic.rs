//! The read path, the load path, the clock and the frames beside the fixed
//! ones.

use lignin_format::PageSize;

use super::*;
use crate::cache::frame::Moment;

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
    assert_eq!(free_frames(&pool), 3, "every refused frame came back");
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
    assert_eq!(free_frames(&pool), 2);
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
    assert_eq!(free_frames(&pool), 0);

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

#[test]
fn a_hit_in_a_fixed_frame_is_pinned_without_a_count() {
    let pool = pool(2);
    drop(seed(&pool, content(1, 1, 0), None));
    let found = pool.get(page(1), version(1)).expect("in the pool");
    assert_eq!(found.frame().pins(), 0, "the pin is announced");
    let census = pool.census();
    assert_eq!((census.pinned, census.unevictable), (1, 1));

    // The frame is not taken for another page while it is held.
    let second = seed(&pool, content(2, 1, 0), None);
    let third = seed(&pool, content(3, 1, 0), None);
    assert_eq!(pool.overflow_frames(), 1);
    assert_eq!(Content::read(&found), content(1, 1, 0));
    drop((second, third));
    assert_eq!(pool.overflow_frames(), 0);

    // A second pin is a count, and it keeps the frame when the first is
    // gone: with the other frame held as well, a page that comes in gets a
    // frame beside the two.
    let again = found.clone();
    assert_eq!(again.frame().pins(), 1);
    drop(found);
    assert_eq!(pool.census().pinned, 1);
    let second = pool.get(page(2), version(1)).expect("in the pool");
    let fourth = seed(&pool, content(4, 1, 0), None);
    assert_eq!(pool.overflow_frames(), 1);
    assert_eq!(Content::read(&again), content(1, 1, 0));
    assert_eq!(Content::read(&second), content(2, 1, 0));
    drop((second, fourth, again));
    assert_eq!(pool.census().pinned, 0);
    assert_eq!(pool.stats().hits, 2);
    check(&pool, 0);
}

#[test]
fn a_thread_with_no_cell_left_counts_its_pins() {
    let pool = pool(2);
    drop(seed(&pool, content(1, 1, 0), None));
    let held: Vec<_> = (0..100)
        .map(|_| pool.get(page(1), version(1)).expect("in the pool"))
        .collect();
    assert_eq!(
        held[0].frame().pins() as usize,
        held.len() - crate::cache::lanes::CELLS,
        "the pins beyond the cells of a lane are counted"
    );
    assert_eq!(pool.census().pinned, 1);
    assert_eq!(pool.stats().hits, 100);

    // Whatever way a pin was taken, the frame stays while one is left, and
    // a cell that was given back is used again.
    let mut held = held;
    while held.len() > 1 {
        held.swap_remove(held.len() / 2);
        drop(seed(&pool, content(2 + held.len() as u32, 1, 0), None));
        assert_eq!(Content::read(&held[0]), content(1, 1, 0));
    }
    let again = pool.get(page(1), version(1)).expect("in the pool");
    assert_eq!(again.frame().pins(), held[0].frame().pins());
    drop((held, again));
    assert_eq!(pool.census().pinned, 0);
    check(&pool, 0);
}

#[test]
fn a_hit_in_a_frame_beside_the_fixed_ones_is_counted() {
    let pool = pool(1);
    let fixed = seed(&pool, content(1, 1, 0), None);
    let beside = seed(&pool, content(2, 1, 0), None);
    assert_eq!(pool.overflow_frames(), 1);
    let found = pool.get(page(2), version(1)).expect("in the pool");
    assert_eq!(found.frame().pins(), 2);
    drop(beside);
    assert_eq!(pool.overflow_frames(), 1);
    drop(found);
    assert_eq!(pool.overflow_frames(), 0);
    drop(fixed);
    check(&pool, 0);
}

#[test]
fn a_frame_that_was_taken_out_under_a_hit_is_free_when_the_hit_is_dropped() {
    let pool = pool(3);
    drop(create(&pool, 1, 1));
    let found = pool.get(page(1), version(1)).expect("unwritten");
    assert_eq!(found.frame().pins(), 0, "the pin is announced");

    // A page step replaces the frame, and a commit frees the page: the
    // version the reader holds is held for readers, and then reclaimed.
    step_once(&pool, 1);
    assert_eq!(free_frames(&pool), 1, "the reader holds it");
    assert_eq!(Content::read(&found), content(1, 1, 0));

    drop(found);
    assert_eq!(free_frames(&pool), 2, "the old frame is free");
    check(&pool, 0);

    let held = pool.get(page(1), version(1)).expect("unwritten");
    pool.free(page(1), version(2)).expect("a later commit");
    assert_eq!(pool.reclaim(&TestPins::at(2)), 1);
    assert_eq!(free_frames(&pool), 2, "the reader holds it");
    assert_eq!(Content::read(&held), content(1, 1, 1));
    drop(held);
    assert_eq!(free_frames(&pool), 3);
    assert_eq!(pool.census(), Census::default());
    check(&pool, 0);
}

#[test]
fn a_pool_has_at_most_a_lane_for_every_two_frames_and_no_more_than_it_may() {
    for (frames, lanes) in [
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 2),
        (9, 4),
        (200, 64),
        (300, 128),
        (5_000, 128),
    ] {
        let pool = BufferPool::with_frames(frames, 8, 1, 0).expect("a small pool");
        assert_eq!(pool.arena.lanes().len(), lanes, "{frames} frames");
    }
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn a_frame_takes_less_than_512_bytes_beside_its_page() {
    use crate::cache::{
        frame::{Arena, OwnedFrame},
        lanes::Lanes,
        pool::Bucket,
    };

    // Miri is given a few small pools: it takes long over a large one.
    let sizes: &[usize] = if cfg!(miri) {
        &[8_192]
    } else {
        &[8_192, 16_384, 32_768, 65_536]
    };
    let counts: Vec<usize> = if cfg!(miri) {
        vec![1, 2, 3, 9]
    } else {
        (1..=40)
            .chain([63, 64, 65, 255, 256, 257, 1_000, 4_097])
            .collect()
    };
    for &size in sizes {
        for &frames in &counts {
            let config = PoolConfig {
                page_size: PageSize::new(size).expect("a page size"),
                capacity: frames * (size + 512),
            };
            let pool = BufferPool::new(config).expect("holds frames");
            assert!(pool.frames() >= frames, "{frames} frames of {size}");
            assert!(pool.fixed_memory() <= pool.capacity());
            // What a few frames save of their 512 bytes is not another
            // frame.
            if frames <= 32 {
                assert_eq!(pool.frames(), frames, "{frames} frames of {size}");
            }
            // One frame more does not fit.
            let more = pool.frames() + 1;
            let cost = more * (size + Arena::HEADER_LEN + size_of::<OwnedFrame>())
                + (2 * more).next_power_of_two() * size_of::<Bucket>()
                + Lanes::count_for(more) * Lanes::LANE_LEN;
            assert!(cost > pool.capacity(), "{frames} frames of {size}");
        }
    }
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn a_part_of_the_table_is_a_cache_line_of_its_own() {
    use crate::cache::pool::Bucket;
    assert_eq!((size_of::<Bucket>(), align_of::<Bucket>()), (64, 64));
}

#[test]
fn a_hit_gives_way_to_a_change_of_its_part_of_the_table() {
    use crate::cache::frame::Unlocked;

    let pool = pool(2);
    drop(seed(&pool, content(1, 1, 0), None));
    let chain = &pool.buckets[0].chain;
    let pin = || chain.pin_unlocked(&pool.arena, |frame| frame.page_raw() == 1, |_| {});

    // While the part is being changed nothing is pinned without the lock.
    let change = chain.write();
    assert!(matches!(pin(), Unlocked::Not));
    drop(change);
    let Unlocked::Found(found) = pin() else {
        panic!("nothing changes the table now");
    };
    assert_eq!(pool.census().pinned, 1);
    pool.unpin(found.into_pin());

    // A change that comes between the announcement and the second look is
    // seen: the pin is taken back, and the page is pinned under the lock.
    let found = pool
        .get_while(page(1), version(1), |moment| {
            if moment == Moment::Announced {
                drop(seed(&pool, content(2, 1, 0), None));
            }
        })
        .expect("in the pool");
    assert_eq!(found.frame().pins(), 1, "the pin is counted");
    assert_eq!(Content::read(&found), content(1, 1, 0));
    drop(found);
    assert_eq!(pool.census().pinned, 0);
    assert_eq!(pool.stats().hits, 1);
    check(&pool, 0);
}

#[test]
fn a_read_with_no_lock_takes_page_versions_only() {
    use crate::cache::frame::Unlocked;

    let pool = pool(2);
    let copy = pool.new_private(owner(1), page(1)).expect("a new page");
    let chain = &pool.buckets[0].chain;

    // Whatever frame the caller asks for, a private copy is not pinned as
    // a page version: its bytes can still change.
    let pinned = chain.pin_unlocked(&pool.arena, |frame| frame.page_raw() == 1, |_| {});
    assert!(matches!(pinned, Unlocked::Not));
    assert_eq!(
        pool.census().pinned,
        1,
        "the copy's handle and nothing else"
    );
    pool.discard(&copy).expect("a private copy");
    drop(copy);
    check(&pool, 0);
}

#[test]
fn a_dead_frame_whose_last_pin_went_is_taken_before_one_is_allocated() {
    use crate::cache::frame::Unlocked;

    let pool = pool(2);
    drop(create(&pool, 1, 1));
    let chain = &pool.buckets[0].chain;
    let pinned = chain.pin_unlocked(&pool.arena, |frame| frame.page_raw() == 1, |_| {});
    let Unlocked::Found(found) = pinned else {
        panic!("nothing changes the table now");
    };

    // A page step replaces the frame under the reader, into the last free
    // frame. The reader then gives its pin back, and has not yet gone
    // through the dead frames.
    step_once(&pool, 1);
    assert!(found.into_pin().unpin(), "the frame is dead");
    assert_eq!(free_frames(&pool), 0);

    // A frame is wanted, none is free and the other one is unwritten: the
    // dead frame is taken, and none is allocated beside the two.
    let blank = pool.blank();
    assert_eq!(pool.overflow_frames(), 0);
    drop(blank);
    assert_eq!(free_frames(&pool), 1);
    check(&pool, 0);
}

/// A frame is a slice of bytes from its first moment, before anything was
/// written to it. So the pool's memory, and that of every frame beside the
/// fixed ones, has to be memory that was written once: it is allocated
/// zeroed. Miri fails this test if it is not.
#[test]
fn a_frame_can_be_read_before_anything_is_written_to_it() {
    let pool = pool(1);
    let fixed = pool.blank();
    let beside = pool.blank();
    assert_eq!(pool.overflow_frames(), 1);
    for blank in [&fixed, &beside] {
        assert_eq!(blank.len(), FRAME);
        assert!(blank.iter().all(|&byte| byte == 0), "a new frame is zeroed");
        assert_eq!(blank.attached(name(1)), None);
    }
}

/// A pool is shared by every thread of a store, and what it hands out goes
/// where the work goes: a page that was read to another thread, a latch to
/// the threads that read under it. The compiler lets them only because the
/// frames, their pointers and the arena are declared safe to send and to
/// share (`frame.rs`), which this test would not build without. A model
/// keeps what it checks in the handles, and they stay on their threads.
#[cfg(not(loom))]
#[test]
fn the_handles_of_the_pool_can_go_to_other_threads() {
    fn sent<T: Send>() {}
    fn shared<T: Sync>() {}
    sent::<BufferPool>();
    shared::<BufferPool>();
    sent::<PageRef<'_>>();
    shared::<PageRef<'_>>();
    sent::<PrivateRef<'_>>();
    shared::<PrivateRef<'_>>();
    sent::<Blank<'_>>();
    shared::<Blank<'_>>();
    shared::<SharedLatch<'_>>();
    shared::<ExclusiveLatch<'_>>();
    shared::<PageStep<'_>>();

    let pool = pool(2);
    let found = seed(&pool, content(1, 1, 0), None);
    let copy = pool
        .make_private(owner(1), page(1), version(1))
        .expect("in the pool");
    let latch = copy.read();
    std::thread::scope(|scope| {
        scope.spawn(|| assert_eq!(Content::read(&latch), content(1, 1, 0)));
        scope.spawn(|| assert_eq!(Content::read(&found.clone()), content(1, 1, 0)));
        let blank = pool.blank();
        scope.spawn(move || assert_eq!(blank.len(), FRAME));
    });
    drop(latch);
    pool.discard(&copy).expect("a private copy");
    drop((copy, found));
    check(&pool, 0);
}

/// Two frames that were taken out while readers held them wait together.
/// The one whose reader goes first is made free, though it waits behind the
/// other, and the other waits on.
#[test]
fn a_dead_frame_is_made_free_while_another_still_waits() {
    let pool = pool(4);
    drop(create(&pool, 1, 1));
    drop(create(&pool, 2, 1));
    let first = pool.get(page(1), version(1)).expect("in the pool");
    let second = pool.get(page(2), version(1)).expect("in the pool");

    // A page step on each page takes its frame out under the reader. The
    // frame of page 2 is the last to join those that wait.
    step_once(&pool, 1);
    step_once(&pool, 2);
    assert_eq!(free_frames(&pool), 0);

    drop(first);
    assert_eq!(free_frames(&pool), 1);
    assert_eq!(Content::read(&second), content(2, 1, 0));
    drop(second);
    assert_eq!(free_frames(&pool), 2);
    assert_eq!(pool.overflow_frames(), 0);
    check(&pool, 0);
}

#[test]
fn a_frame_that_was_given_another_page_meanwhile_is_not_pinned() {
    let pool = pool(2);
    drop(seed(&pool, content(1, 1, 0), None));

    // The reader has picked the frame of page 1 and holds nothing yet.
    // Page 1 is evicted, its frame is given to page 3, and page 1 is loaded
    // again into the other frame. The reader then announces a frame that
    // holds another page, finds that the table was changed, and takes the
    // frame page 1 is in now, under the lock.
    let found = pool
        .get_while(page(1), version(1), |moment| {
            if moment == Moment::Picked {
                let second = seed(&pool, content(2, 1, 0), None);
                let third = seed(&pool, content(3, 1, 0), None);
                assert_eq!(pool.overflow_frames(), 0, "page 1 was evicted");
                drop(second);
                drop(seed(&pool, content(1, 1, 0), None));
                drop(third);
            }
        })
        .expect("in the pool");
    assert_eq!(Content::read(&found), content(1, 1, 0));
    assert_eq!(found.frame().pins(), 1, "the pin is counted");
    assert_eq!(pool.stats().evictions, 2);
    drop(found);
    assert_eq!(pool.census().pinned, 0);
    check(&pool, 0);
}

#[test]
fn an_announcement_that_was_no_pin_leaves_no_dead_frame_behind() {
    let pool = pool(3);
    drop(create(&pool, 1, 1));

    // A page step replaces the frame between the reader's announcement and
    // its second look. The frame is dead and waits for the announcement to
    // go, and the reader, who takes it back, is the one who frees it.
    let found = pool
        .get_while(page(1), version(1), |moment| {
            if moment == Moment::Announced {
                step_once(&pool, 1);
                assert_eq!(free_frames(&pool), 1, "the announcement keeps the frame");
            }
        })
        .expect("unwritten");
    assert_eq!(Content::read(&found), content(1, 1, 1));
    assert_eq!(free_frames(&pool), 2, "the old frame is free");
    drop(found);
    check(&pool, 0);
}

#[test]
fn a_frame_the_clock_could_not_take_keeps_its_mark() {
    let pool = pool(2);
    drop(seed(&pool, content(1, 1, 0), None));
    drop(seed(&pool, content(2, 1, 0), None));
    let held = pool.get(page(1), version(1)).expect("in the pool");

    // The clock takes the marks off both frames, comes to the first one
    // again, finds it pinned, and evicts the second.
    drop(seed(&pool, content(3, 1, 0), None));
    assert_eq!(pool.stats().evictions, 1);
    assert!(pool.get(page(2), version(1)).is_err());

    // The pinned frame has its mark again, as a frame with a counted pin
    // keeps it: the hand passes it once more before it tries it again.
    assert!(held.frame().take_referenced());
    assert_eq!(Content::read(&held), content(1, 1, 0));
    drop(held);
    check(&pool, 0);
}
