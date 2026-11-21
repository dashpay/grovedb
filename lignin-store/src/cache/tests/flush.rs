//! The unwritten lists, write-back, and what is held for readers.

use super::*;

fn unwritten(pool: &BufferPool) -> Vec<(u64, Vec<u32>)> {
    let mut versions = Vec::new();
    pool.unwritten_versions(&mut versions);
    versions
        .into_iter()
        .map(|version| {
            let mut pages = Vec::new();
            pool.unwritten_pages(version, &mut pages);
            let mut pages: Vec<u32> = pages.into_iter().map(PageId::get).collect();
            pages.sort_unstable();
            (version.get(), pages)
        })
        .collect()
}

#[test]
fn each_commit_has_a_list_of_the_pages_it_bore() {
    let pool = pool(8);
    assert_eq!(pool.oldest_unwritten(), None);
    drop(create(&pool, 1, 1));
    drop(create(&pool, 2, 1));
    drop(commit(&pool, 1, 1));
    drop(create(&pool, 3, 4));
    assert_eq!(
        unwritten(&pool),
        [(1, vec![1, 2]), (2, vec![1]), (4, vec![3])]
    );
    assert_eq!(pool.oldest_unwritten(), Some(version(1)));
    let mut none = Vec::new();
    pool.unwritten_pages(version(3), &mut none);
    assert!(none.is_empty());

    // Write-back takes the oldest commit first.
    let first = pool.get_version(page(1), version(1)).expect("kept");
    pool.mark_written(&first).expect("owed");
    assert_eq!(unwritten(&pool), [(1, vec![2]), (2, vec![1]), (4, vec![3])]);
    let second = pool.get_version(page(2), version(1)).expect("kept");
    pool.mark_written(&second).expect("owed");
    assert_eq!(unwritten(&pool), [(2, vec![1]), (4, vec![3])]);
    assert_eq!(pool.oldest_unwritten(), Some(version(2)));
    assert_eq!(
        pool.mark_written(&second).err(),
        Some(CacheError::NotUnwritten {
            page: page(2),
            born: version(1)
        })
    );
    assert_eq!(pool.census().unwritten, 2);
    check(&pool, 0);
}

#[test]
fn a_page_that_was_loaded_is_not_owed() {
    let pool = pool(2);
    let loaded = seed(&pool, content(1, 3, 0), Some(5));
    assert_eq!(
        pool.mark_written(&loaded).err(),
        Some(CacheError::NotUnwritten {
            page: page(1),
            born: version(3)
        })
    );
    assert_eq!(
        pool.hold_for_readers(&loaded).err(),
        Some(CacheError::NotUnwritten {
            page: page(1),
            born: version(3)
        })
    );
}

#[test]
fn an_unwritten_page_stays_in_memory_until_it_is_written_back() {
    let pool = pool(2);
    let mut disk = Disk::default();
    drop(create(&pool, 1, 1));
    drop(create(&pool, 2, 1));
    for raw in 3..=6 {
        disk.write(content(raw, 1, 0));
    }

    // Other pages come and go beside the fixed frames. Nothing evicts the
    // two that only the pool holds.
    for raw in 3..=6 {
        let other = fetch(&pool, &disk, raw, 1).expect("on disk");
        assert_eq!(pool.overflow_frames(), 1);
        drop(other);
        assert_eq!(pool.overflow_frames(), 0);
    }
    assert_eq!(pool.stats().evictions, 0);
    for raw in 1..=2 {
        let kept = pool.get(page(raw), version(1)).expect("kept");
        assert_eq!(Content::read(&kept), content(raw, 1, 0));
    }

    // Once it is in a segment, a page can be evicted.
    let written = pool.get_version(page(1), version(1)).expect("kept");
    disk.write(Content::read(&written));
    pool.mark_written(&written).expect("owed");
    drop(written);
    drop(fetch(&pool, &disk, 3, 1));
    drop(fetch(&pool, &disk, 4, 1));
    assert_eq!(pool.stats().evictions, 2);
    assert!(pool.get(page(1), version(1)).is_err(), "evicted");
    assert!(pool.get(page(2), version(1)).is_ok(), "still owed");
    assert_eq!(
        Content::read(&fetch(&pool, &disk, 1, 1).expect("on disk")),
        content(1, 1, 0)
    );
    check(&pool, 0);
}

#[test]
fn a_superseded_version_can_be_held_for_its_readers_instead() {
    let pool = pool(2);
    let pins = TestPins::at(1);
    let copy = pool.new_private(owner(1), page(1)).expect("a new page");
    {
        let mut latch = copy.write().expect("private");
        content(1, 1, 0).write(&mut latch);
        latch.attach(name(7), value(b"pending"));
    }
    let first = pool.install(&copy, version(1)).expect("a commit");
    drop(copy);
    assert_eq!(
        pool.hold_for_readers(&first).err(),
        Some(CacheError::StillCurrent {
            page: page(1),
            born: version(1)
        })
    );
    drop(commit(&pool, 1, 1));
    assert_eq!(pool.attached_bytes(), 14, "the copy carried the value on");

    pool.hold_for_readers(&first).expect("superseded and owed");
    assert_eq!(unwritten(&pool), [(2, vec![1])]);
    let census = pool.census();
    assert_eq!((census.unwritten, census.held), (1, 1));
    assert_eq!(
        pool.hold_for_readers(&first).err(),
        Some(CacheError::NotUnwritten {
            page: page(1),
            born: version(1)
        })
    );
    drop(first);
    check(&pool, 0);

    // It is still there for a reader at version 1, value and all, and
    // nothing takes it while that reader can ask.
    assert_eq!(pool.reclaim(&pins), 0);
    let seen = pool.get(page(1), version(1)).expect("held");
    assert_eq!(seen.attached(name(7)), Some(&b"pending"[..]));
    drop(seen);

    pins.set_oldest(2);
    assert_eq!(pool.reclaim(&pins), 1);
    assert!(pool.get(page(1), version(1)).is_err());
    assert_eq!(pool.attached_bytes(), 7);
    assert_eq!(pool.census().held, 0);
    check(&pool, 0);
}

#[test]
fn what_was_never_written_back_of_a_freed_page_is_kept_for_its_readers() {
    let pool = pool(2);
    let mut disk = Disk::default();
    let pins = TestPins::at(1);
    drop(create(&pool, 1, 1));
    drop(commit(&pool, 1, 1));
    assert_eq!(unwritten(&pool), [(1, vec![1]), (2, vec![1])]);

    // Commit 3 frees the page. Write-back owes nothing of it any more.
    pool.free(page(1), version(3)).expect("a later commit");
    disk.free(1, 3);
    assert_eq!(unwritten(&pool), []);
    assert_eq!(pool.oldest_unwritten(), None);
    let census = pool.census();
    assert_eq!(
        (census.unwritten, census.held, census.unevictable),
        (0, 2, 2)
    );
    assert_eq!(
        pool.free(page(1), version(2)).err(),
        Some(CacheError::VersionNotAfter {
            page: page(1),
            version: version(2),
            current: Some(version(2))
        })
    );
    check(&pool, 0);

    // Snapshots at versions 1 and 2 keep reading it, under memory pressure,
    // and a reader at version 3 finds no page.
    for raw in 2..=5 {
        disk.write(content(raw, 1, 0));
        drop(fetch(&pool, &disk, raw, 3));
    }
    assert_eq!(pool.stats().evictions, 0);
    assert_eq!(
        Content::read(&pool.get(page(1), version(1)).expect("held")),
        content(1, 1, 0)
    );
    assert_eq!(
        Content::read(&pool.get(page(1), version(2)).expect("held")),
        content(1, 2, 0)
    );
    assert!(fetch(&pool, &disk, 1, 3).is_none());

    // The snapshot at 1 goes: version 1 of the page, superseded at 2, can
    // go. An operation that was running then may still ask for it.
    pins.set_oldest(2);
    pins.set_oldest_guard(0);
    assert_eq!(pool.reclaim(&pins), 0);
    assert!(pool.get(page(1), version(1)).is_ok());
    pins.set_oldest_guard(u64::MAX);
    assert_eq!(pool.reclaim(&pins), 1);
    assert!(pool.get(page(1), version(1)).is_err());
    assert!(pool.get(page(1), version(2)).is_ok());

    // A reader that still holds the last version keeps it past the reclaim
    // that drops it from the pool.
    let reader = pool.get(page(1), version(2)).expect("held");
    pins.set_oldest(3);
    assert_eq!(pool.reclaim(&pins), 1);
    assert!(pool.get(page(1), version(2)).is_err());
    assert_eq!(Content::read(&reader), content(1, 2, 0));
    check(&pool, 1);
    drop(reader);
    assert_eq!(pool.census(), Census::default());
    assert_eq!(pool.reclaim(&pins), 0);
    check(&pool, 0);
}

#[test]
fn reclaim_asks_nothing_while_nothing_is_held() {
    let pool = pool(2);
    let pins = TestPins::at(5);
    drop(create(&pool, 1, 1));
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pins.asked(), 0);
    assert_eq!(pins.epoch.load(Ordering::SeqCst), 0);
}

#[test]
fn a_page_that_is_not_in_the_pool_can_be_freed() {
    let pool = pool(2);
    pool.free(page(1), version(4)).expect("nothing to mark");
    assert_eq!(
        pool.free(page(1), version(0)).err(),
        Some(CacheError::VersionNotAfter {
            page: page(1),
            version: version(0),
            current: None
        })
    );
    // A page that was written back and freed leaves a clean frame, which
    // the clock takes like any other.
    let loaded = seed(&pool, content(2, 3, 0), None);
    pool.free(page(2), version(5)).expect("a later commit");
    assert_eq!(loaded.frame().superseded(), 5);
    assert!(pool.get(page(2), version(4)).is_ok());
    assert!(pool.get(page(2), version(5)).is_err());
    assert_eq!(pool.census().held, 0);
    check(&pool, 0);
}

#[test]
fn the_id_of_a_freed_page_names_a_new_page_beside_the_old_frames() {
    let pool = pool(4);
    let pins = TestPins::at(1);
    drop(create(&pool, 1, 1));
    pool.free(page(1), version(2)).expect("a later commit");
    // The id is used again while a snapshot at version 1 still reads the
    // old page.
    drop(create(&pool, 1, 3));
    assert_eq!(
        Content::read(&pool.get(page(1), version(1)).expect("the old page")),
        content(1, 1, 0)
    );
    assert!(
        pool.get(page(1), version(2)).is_err(),
        "no page at version 2"
    );
    assert_eq!(
        Content::read(&pool.get(page(1), version(3)).expect("the new page")),
        content(1, 3, 0)
    );
    check(&pool, 0);
    pins.set_oldest(3);
    assert_eq!(pool.reclaim(&pins), 1);
    assert!(pool.get(page(1), version(3)).is_ok());
    check(&pool, 0);
}
