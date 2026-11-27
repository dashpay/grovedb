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

/// A page version that a segment holds is spent once the version after
/// it is in a segment too. It stays for the readers that can ask for it,
/// then a reclaim takes it out, with no eviction, and a reader that asks
/// for it all the same reads it from its segment.
#[test]
fn a_spent_page_version_leaves_the_pool_once_no_reader_can_ask_for_it() {
    let pool = pool(4);
    let mut disk = Disk::default();
    let pins = TestPins::at(1);
    let first = create(&pool, 1, 1);
    disk.write(content(1, 1, 0));
    pool.mark_written(&first).expect("owed");
    drop(first);
    let second = commit(&pool, 1, 1);

    // Until version 2 is in a segment, version 1 is the page that
    // write-back applies the run of version 2 to. It is not spent,
    // whoever can ask for it, and a reclaim asks nothing.
    pins.set_oldest(2);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pins.asked(), 0);
    assert!(pool.get_version(page(1), version(1)).is_some());

    // Version 2 is written. A reader at version 1 can still ask.
    disk.write(content(1, 2, 0));
    pool.mark_written(&second).expect("owed");
    drop(second);
    pins.set_oldest(1);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(
        Content::read(&pool.get(page(1), version(1)).expect("kept")),
        content(1, 1, 0)
    );
    assert_eq!(pool.stats().releases, 0);

    // That reader goes. An operation that was running then may still ask.
    pins.set_oldest(2);
    pins.set_oldest_guard(0);
    pool.reclaim(&pins);
    assert!(pool.get(page(1), version(1)).is_ok());
    assert_eq!(pool.stats().releases, 0);
    pins.set_oldest_guard(u64::MAX);
    assert_eq!(pool.reclaim(&pins), 0, "no page version was held");
    let stats = pool.stats();
    assert_eq!((stats.releases, stats.evictions), (1, 0));
    assert!(pool.get(page(1), version(1)).is_err());
    assert!(pool.get(page(1), version(2)).is_ok());
    assert_eq!(free_frames(&pool), 3);

    // Nothing is listed any more, and a reclaim asks nothing.
    let asked = pins.asked();
    pool.reclaim(&pins);
    assert_eq!(pins.asked(), asked);

    // A reader that asks for it afterwards reads it from its segment.
    assert_eq!(
        Content::read(&fetch(&pool, &disk, 1, 1).expect("in a segment")),
        content(1, 1, 0)
    );
    check(&pool, 0);
}

/// A reader that holds a spent page version keeps it: a reclaim takes out
/// no frame that has a pin, as an eviction takes none. The version is
/// listed no more, and the clock has it from there on.
#[test]
fn a_reader_that_holds_a_spent_page_version_keeps_it() {
    let pool = pool(4);
    let pins = TestPins::at(1);
    let first = create(&pool, 1, 1);
    pool.mark_written(&first).expect("owed");
    drop(first);
    let reader = pool.get(page(1), version(1)).expect("in the pool");
    let second = commit(&pool, 1, 1);
    pool.mark_written(&second).expect("owed");
    drop(second);

    pins.set_oldest(2);
    let asked = pins.asked();
    pool.reclaim(&pins);
    assert!(pins.asked() > asked, "it was listed");
    assert_eq!(pool.stats().releases, 0);
    assert_eq!(Content::read(&reader), content(1, 1, 0));
    assert!(pool.get(page(1), version(1)).is_ok());
    check(&pool, 0);
    drop(reader);
    let asked = pins.asked();
    pool.reclaim(&pins);
    assert_eq!(pins.asked(), asked, "it is listed no more");
    assert!(pool.get(page(1), version(1)).is_ok());
    check(&pool, 0);
}

/// The version of a page that a segment holds is spent when a later one
/// is written, however many commits lie between them: the version that
/// was held for readers between them goes as a held one does.
#[test]
fn the_written_version_before_the_one_that_is_written_is_spent() {
    let pool = pool(4);
    let pins = TestPins::at(3);
    let first = create(&pool, 1, 1);
    pool.mark_written(&first).expect("owed");
    drop(first);
    let second = commit(&pool, 1, 1);
    let third = commit(&pool, 1, 2);
    pool.hold_for_readers(&second).expect("superseded and owed");
    drop(second);

    // Version 1 is still the page under the run that takes it to version
    // 3: only the held version goes.
    assert_eq!(pool.reclaim(&pins), 1);
    assert_eq!(pool.stats().releases, 0);
    assert!(pool.get_version(page(1), version(1)).is_some());

    pool.mark_written(&third).expect("owed");
    drop(third);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pool.stats().releases, 1);
    assert!(pool.get_version(page(1), version(1)).is_none());
    assert_eq!(free_frames(&pool), 3);
    check(&pool, 0);
}

/// The list of spent page versions is bounded: it never holds more than
/// its bound, a full one loses its older half before it takes another,
/// and those frames are the clock's.
#[test]
fn the_list_of_spent_page_versions_is_bounded() {
    use super::super::pool::{Held, Unwritten};

    let one = |born: u64| Held {
        page: page(1),
        born,
        superseded: born + 1,
        epoch: None,
    };
    let bound = Unwritten::SPENT_BOUND as u64;
    let mut unwritten = Unwritten::default();
    for born in 0..bound {
        unwritten.spend([one(born)]);
    }
    assert_eq!(unwritten.spent.len() as u64, bound);
    assert_eq!(unwritten.spent[0].born, 0);
    unwritten.spend([one(bound)]);
    assert_eq!(unwritten.spent.len() as u64, bound / 2 + 1);
    assert_eq!(unwritten.spent[0].born, bound / 2);
    assert_eq!(unwritten.spent.last().map(|last| last.born), Some(bound));

    // The versions of one call, many of them, as the written versions of a
    // page that is freed: in their order, and never more than the bound.
    let mut unwritten = Unwritten::default();
    unwritten.spend((0..3 * bound).map(one));
    assert!(unwritten.spent.len() as u64 <= bound);
    assert!(unwritten.spent.capacity() as u64 <= bound);
    let listed: Vec<u64> = unwritten.spent.iter().map(|spent| spent.born).collect();
    assert!(listed.windows(2).all(|pair| pair[0] + 1 == pair[1]));
    assert_eq!(listed.last(), Some(&(3 * bound - 1)));
}

/// Of several versions of a page that a segment holds, the one that is
/// listed when a later version is written is the newest of them: the
/// others were listed when that one was written. So a page that is
/// written again and again while a reader keeps every version in the pool
/// is listed once for each write, and not once for each pair of them.
#[test]
fn a_page_version_is_listed_once_however_many_versions_are_written_after_it() {
    let pool = pool(8);
    let pins = TestPins::at(1);
    let first = create(&pool, 1, 1);
    pool.mark_written(&first).expect("owed");
    drop(first);
    for at in 1..5 {
        let next = commit(&pool, 1, at);
        pool.mark_written(&next).expect("owed");
    }
    // The reader on version 1 keeps all of them: nothing goes, and four
    // are listed, the versions 1 to 4.
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pool.stats().releases, 0);
    assert_eq!(spent_listed(&pool), 4);

    pins.set_oldest(5);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pool.stats().releases, 4);
    assert_eq!(spent_listed(&pool), 0);
    assert_eq!(free_frames(&pool), 7);
    check(&pool, 0);
}

/// A listing outlives its frame. The clock takes the frame of a spent
/// version, a reader loads the version again into another frame, and the
/// reclaim that comes to the stale entry takes that frame out as it would
/// have taken the first: it holds a version no reader can ask for. Read
/// again after that, the version is what it was.
#[test]
fn a_spent_page_version_that_was_loaded_again_is_taken_out_like_any_other() {
    let pool = pool(2);
    let mut disk = Disk::default();
    let pins = TestPins::at(1);
    let first = create(&pool, 1, 1);
    let listed_at = first.as_ptr();
    disk.write(content(1, 1, 0));
    pool.mark_written(&first).expect("owed");
    drop(first);
    let second = commit(&pool, 1, 1);
    disk.write(content(1, 2, 0));
    pool.mark_written(&second).expect("owed");
    drop(second);
    assert_eq!(spent_listed(&pool), 1);

    // Two other pages take both frames: the clock has taken version 1.
    disk.write(content(2, 1, 0));
    disk.write(content(3, 1, 0));
    let other = fetch(&pool, &disk, 2, 1).expect("in a segment");
    let another = fetch(&pool, &disk, 3, 1).expect("in a segment");
    assert!(pool.get_version(page(1), version(1)).is_none());
    assert_eq!(pool.stats().evictions, 2);
    assert_eq!(spent_listed(&pool), 1);

    // The reader on version 1 loads it again while the frame that the
    // version was listed in is held for another page: it comes into the
    // other frame. Then the reader goes.
    let (holds_it, lets_go) = match other.as_ptr() == listed_at {
        true => (other, another),
        false => (another, other),
    };
    assert_eq!(holds_it.as_ptr(), listed_at);
    drop(lets_go);
    let again = fetch(&pool, &disk, 1, 1).expect("in a segment");
    assert_ne!(again.as_ptr(), listed_at);
    assert_eq!(Content::read(&again), content(1, 1, 0));
    drop((again, holds_it));
    pins.set_oldest(2);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pool.stats().releases, 1);
    assert!(pool.get_version(page(1), version(1)).is_none());
    assert_eq!(spent_listed(&pool), 0);
    assert_eq!(
        Content::read(&fetch(&pool, &disk, 1, 1).expect("in a segment")),
        content(1, 1, 0)
    );
    check(&pool, 0);
}

/// A listing names a page version by the id of its page and the commit
/// that bore it. When the id of a freed page names a new page, the entry
/// of the old page's version takes the old frame, and nothing of the page
/// that has the id now.
#[test]
fn a_spent_version_of_a_freed_page_takes_nothing_of_the_page_that_has_its_id_now() {
    let pool = pool(4);
    let pins = TestPins::at(1);
    let old = create(&pool, 1, 1);
    pool.mark_written(&old).expect("owed");
    drop(old);
    pool.free(page(1), version(2)).expect("a later commit");
    assert_eq!(spent_listed(&pool), 1);
    let new = create(&pool, 1, 3);

    pins.set_oldest(3);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pool.stats().releases, 1);
    assert!(
        pool.get(page(1), version(1)).is_err(),
        "the old page is gone"
    );
    assert_eq!(Content::read(&new), content(1, 3, 0));
    assert_eq!(pool.census().unwritten, 1);

    // Written, the new page's version is the current one: nothing is
    // listed of it, and a reclaim leaves it.
    pool.mark_written(&new).expect("owed");
    drop(new);
    assert_eq!(spent_listed(&pool), 0);
    pool.reclaim(&pins);
    assert_eq!(pool.stats().releases, 1);
    assert_eq!(
        Content::read(&pool.get(page(1), version(3)).expect("the new page")),
        content(1, 3, 0)
    );
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
    // It is spent, and a reclaim takes it out before the clock comes by,
    // once no reader is on a version before the free.
    drop(loaded);
    let pins = TestPins::at(4);
    assert_eq!(pool.reclaim(&pins), 0);
    assert!(pool.get(page(2), version(4)).is_ok());
    pins.set_oldest(5);
    assert_eq!(pool.reclaim(&pins), 0);
    assert_eq!(pool.stats().releases, 1);
    assert!(pool.get(page(2), version(4)).is_err());
    assert_eq!(pool.census(), Census::default());
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
