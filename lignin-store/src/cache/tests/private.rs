//! Private copies, their latches, and the commit that installs them.

use super::*;

#[test]
fn a_private_copy_is_made_from_the_version_its_transaction_reads() {
    let pool = pool(4);
    let base = seed(&pool, content(1, 3, 0), None);
    let copy = pool
        .make_private(owner(1), page(1), version(5))
        .expect("in the pool");
    assert_eq!(copy.page(), page(1));
    assert_eq!(copy.owner(), owner(1));
    assert_eq!(copy.base(), Some(version(3)));
    assert_eq!(Content::read(&copy.read()), content(1, 3, 0));
    assert_eq!(pool.page_step(page(1)).private_copies(), 1);

    // Changing the copy changes nothing a reader sees.
    content(1, 6, 0).write(&mut copy.write().expect("private"));
    assert_eq!(Content::read(&copy.read()), content(1, 6, 0));
    assert_eq!(Content::read(&base), content(1, 3, 0));
    assert_eq!(
        Content::read(&pool.get(page(1), version(100)).expect("committed")),
        content(1, 3, 0)
    );
    let census = pool.census();
    assert_eq!((census.published, census.private), (1, 1));
    assert_eq!(census.unevictable, 2);
    check(&pool, 0);
}

#[test]
fn a_page_is_made_private_once_for_an_owner() {
    let pool = pool(4);
    drop(seed(&pool, content(1, 3, 0), None));
    let first = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    content(1, 4, 0).write(&mut first.write().expect("private"));

    let second = pool
        .make_private(owner(1), page(1), version(3))
        .expect("the same copy");
    assert_eq!(Content::read(&second.read()), content(1, 4, 0));
    assert_eq!(second.read().as_ptr(), first.read().as_ptr());
    let third = pool.private(owner(1), page(1)).expect("the same copy");
    assert_eq!(third.read().as_ptr(), first.read().as_ptr());
    let fourth = first.clone();
    assert_eq!(fourth.read().as_ptr(), first.read().as_ptr());
    assert_eq!(pool.page_step(page(1)).private_copies(), 1);
    assert_eq!(lock(&pool.free).len(), 2, "the spare frames came back");

    // Another owner gets a copy of its own, of the committed page.
    let other = pool
        .make_private(owner(2), page(1), version(3))
        .expect("in the pool");
    assert_ne!(other.read().as_ptr(), first.read().as_ptr());
    assert_eq!(Content::read(&other.read()), content(1, 3, 0));
    assert_eq!(pool.page_step(page(1)).private_copies(), 2);
    assert!(pool.private(owner(3), page(1)).is_none());
    assert!(pool.private(owner(1), page(2)).is_none());
    check(&pool, 0);
}

#[test]
fn a_page_that_is_not_in_the_pool_is_loaded_before_it_is_made_private() {
    let pool = pool(2);
    let mut disk = Disk::default();
    disk.write(content(1, 3, 0));
    let miss = pool
        .make_private(owner(1), page(1), version(5))
        .expect_err("not in the pool");
    assert_eq!((miss.page(), miss.at()), (page(1), version(5)));
    assert_eq!(lock(&pool.free).len(), 2, "the spare frame came back");
    drop(load(&pool, &disk, miss).expect("on disk"));
    let copy = pool
        .make_private(owner(1), page(1), version(5))
        .expect("loaded");
    assert_eq!(Content::read(&copy.read()), content(1, 3, 0));
    check(&pool, 0);
}

#[test]
fn a_copy_can_be_made_of_a_version_that_is_not_the_newest() {
    let pool = pool(4);
    drop(seed(&pool, content(1, 3, 0), Some(7)));
    drop(seed(&pool, content(1, 7, 0), None));
    let copy = pool
        .make_private(owner(1), page(1), version(5))
        .expect("in the pool");
    assert_eq!(copy.base(), Some(version(3)));
    assert_eq!(
        pool.install(copy, version(8)).err(),
        Some(CacheError::BaseNotCurrent {
            page: page(1),
            base: Some(version(3)),
            current: Some(version(7))
        })
    );
    assert_eq!(pool.page_step(page(1)).private_copies(), 1, "still private");
    check(&pool, 0);
}

#[test]
fn the_latch_lets_one_writer_or_many_readers_in() {
    let pool = pool(2);
    let copy = pool.new_private(owner(1), page(1)).expect("a new page");
    assert!(
        copy.read().iter().all(|&byte| byte == 0),
        "a new page is zero"
    );
    assert_eq!(copy.base(), None);

    let reader = copy.read();
    let second = copy.try_read().expect("readers share");
    assert!(copy.try_write().expect("private").is_none());
    drop((reader, second));

    let mut writer = copy.try_write().expect("private").expect("free");
    writer[0] = 9;
    assert_eq!(writer[0], 9);
    assert!(copy.try_read().is_none());
    assert!(copy.try_write().expect("private").is_none());
    drop(writer);
    assert_eq!(copy.read()[0], 9);
}

#[test]
fn a_new_page_is_private_once() {
    let pool = pool(2);
    let _copy = pool.new_private(owner(1), page(1)).expect("a new page");
    assert_eq!(
        pool.new_private(owner(1), page(1)).err(),
        Some(CacheError::AlreadyPrivate {
            page: page(1),
            owner: owner(1)
        })
    );
    assert!(pool.new_private(owner(2), page(1)).is_ok());
    assert_eq!(lock(&pool.free).len(), 0);
    check(&pool, 0);
}

#[test]
fn a_commit_installs_a_private_copy_as_the_next_version() {
    let pool = pool(4);
    let old = seed(&pool, content(1, 3, 0), None);
    let copy = pool
        .make_private(owner(1), page(1), version(5))
        .expect("in the pool");
    content(1, 6, 0).write(&mut copy.write().expect("private"));
    let stale = copy.clone();

    let new = pool.install(copy, version(6)).expect("a commit");
    assert_eq!(new.born(), version(6));
    assert_eq!(new.page(), page(1));
    assert_eq!(Content::read(&new), content(1, 6, 0));
    assert_eq!(pool.page_step(page(1)).private_copies(), 0);

    // A reader below the commit keeps the old version, and one at it or
    // after it gets the new one.
    assert_eq!(Content::read(&old), content(1, 3, 0));
    for at in [3, 5] {
        let seen = pool.get(page(1), version(at)).expect("the old version");
        assert_eq!(seen.as_ptr(), old.as_ptr());
    }
    for at in [6, 7, 500] {
        let seen = pool.get(page(1), version(at)).expect("the new version");
        assert_eq!(seen.as_ptr(), new.as_ptr());
    }

    // Write-back owes the new version, and a handle left over from the
    // transaction can read it and no longer change it.
    assert_eq!(pool.oldest_unwritten(), Some(version(6)));
    assert_eq!(
        stale.write().err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    assert_eq!(
        stale.try_write().err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    assert_eq!(Content::read(&stale.read()), content(1, 6, 0));
    assert_eq!(
        pool.install(stale.clone(), version(7)).err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    assert_eq!(
        pool.discard(stale).err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    let census = pool.census();
    assert_eq!(
        (census.published, census.private, census.unwritten),
        (2, 0, 1)
    );
    check(&pool, 0);
}

#[test]
fn a_commit_that_cannot_follow_the_page_is_refused() {
    let pool = pool(4);
    drop(seed(&pool, content(1, 3, 0), None));
    let copy = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    for bad in [0, 2, 3] {
        assert_eq!(
            pool.install(copy.clone(), version(bad)).err(),
            Some(CacheError::VersionNotAfter {
                page: page(1),
                version: version(bad),
                current: Some(version(3))
            })
        );
    }
    let fresh = pool.new_private(owner(2), page(2)).expect("a new page");
    assert_eq!(
        pool.install(fresh.clone(), version(0)).err(),
        Some(CacheError::VersionNotAfter {
            page: page(2),
            version: version(0),
            current: None
        })
    );
    assert_eq!(pool.census().private, 2, "both are still private");
    assert!(pool.install(copy, version(4)).is_ok());
    assert!(pool.install(fresh, version(4)).is_ok());
    check(&pool, 0);
}

#[test]
fn of_two_copies_of_one_version_only_the_first_is_installed() {
    let pool = pool(4);
    drop(seed(&pool, content(1, 3, 0), None));
    let first = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    let second = pool
        .make_private(owner(2), page(1), version(3))
        .expect("in the pool");
    drop(pool.install(first, version(4)).expect("the first commit"));
    assert_eq!(
        pool.install(second, version(5)).err(),
        Some(CacheError::BaseNotCurrent {
            page: page(1),
            base: Some(version(3)),
            current: Some(version(4))
        })
    );

    // A page that a transaction created cannot go over a page that exists.
    let fresh = pool.new_private(owner(3), page(1)).expect("a copy");
    assert_eq!(
        pool.install(fresh, version(5)).err(),
        Some(CacheError::BaseNotCurrent {
            page: page(1),
            base: None,
            current: Some(version(4))
        })
    );
    check(&pool, 0);
}

#[test]
fn a_copy_is_installed_when_the_version_it_was_made_from_was_evicted() {
    let pool = pool(2);
    let mut disk = Disk::default();
    disk.write(content(1, 3, 0));
    drop(fetch(&pool, &disk, 1, 3));
    let copy = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    content(1, 4, 0).write(&mut copy.write().expect("private"));

    // Other pages push the base out. The copy stays.
    disk.write(content(2, 1, 0));
    disk.write(content(3, 1, 0));
    drop(fetch(&pool, &disk, 2, 3));
    drop(fetch(&pool, &disk, 3, 3));
    assert!(
        pool.get(page(1), version(3)).is_err(),
        "the base was evicted"
    );

    drop(pool.install(copy, version(4)).expect("a commit"));
    // The base comes back from disk for an old reader, bounded by the new
    // version, which is in the pool.
    let old = fetch(&pool, &disk, 1, 3).expect("on disk");
    assert_eq!(Content::read(&old), content(1, 3, 0));
    assert_eq!(old.frame().superseded(), 4);
    let new = fetch(&pool, &disk, 1, 4).expect("in the pool");
    assert_eq!(Content::read(&new), content(1, 4, 0));
    check(&pool, 0);
}

#[test]
fn a_discarded_copy_goes_with_its_last_handle() {
    let pool = pool(2);
    drop(seed(&pool, content(1, 3, 0), None));
    let copy = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    content(1, 9, 0).write(&mut copy.write().expect("private"));
    let stale = copy.clone();
    assert_eq!(lock(&pool.free).len(), 0);

    pool.discard(copy).expect("a private copy");
    assert_eq!(pool.page_step(page(1)).private_copies(), 0);
    assert!(pool.private(owner(1), page(1)).is_none());
    assert_eq!(lock(&pool.free).len(), 0, "a handle still pins the frame");
    check(&pool, 1);

    // The handle that is left reads what the copy held, and changes nothing.
    assert_eq!(Content::read(&stale.read()), content(1, 9, 0));
    assert_eq!(
        stale.write().err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    assert_eq!(
        pool.discard(stale.clone()).err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    assert_eq!(
        pool.transfer(&stale, owner(2)).err(),
        Some(CacheError::NotPrivate { page: page(1) })
    );
    drop(stale);
    assert_eq!(lock(&pool.free).len(), 1, "the frame is free");
    assert_eq!(
        Content::read(&pool.get(page(1), version(3)).expect("committed")),
        content(1, 3, 0)
    );
    check(&pool, 0);
}

#[test]
fn a_copy_beside_the_fixed_frames_is_freed_when_it_is_discarded() {
    let pool = pool(1);
    let base = seed(&pool, content(1, 3, 0), None);
    let copy = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    assert_eq!(pool.overflow_frames(), 1);
    let stale = copy.clone();
    pool.discard(copy).expect("a private copy");
    assert_eq!(pool.overflow_frames(), 1);
    drop(stale);
    assert_eq!(pool.overflow_frames(), 0);
    drop(base);
    check(&pool, 0);
}

#[test]
fn a_scope_hands_its_copy_to_its_transaction() {
    let pool = pool(4);
    drop(seed(&pool, content(1, 3, 0), None));
    let scope = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");
    pool.transfer(&scope, owner(7))
        .expect("nobody else has one");
    assert_eq!(scope.owner(), owner(7));
    pool.transfer(&scope, owner(7)).expect("already there");
    assert!(pool.private(owner(1), page(1)).is_none());
    assert!(pool.private(owner(7), page(1)).is_some());

    // A second scope's copy of the same page cannot join it: the caller
    // reconciles the two and discards one.
    let second = pool
        .make_private(owner(2), page(1), version(3))
        .expect("in the pool");
    assert_eq!(
        pool.transfer(&second, owner(7)).err(),
        Some(CacheError::AlreadyPrivate {
            page: page(1),
            owner: owner(7)
        })
    );
    assert_eq!(second.owner(), owner(2));
    pool.discard(second).expect("a private copy");
    check(&pool, 0);
}

#[test]
fn attached_values_travel_with_the_copy_and_the_commit() {
    let pool = pool(4);
    let copy = pool.new_private(owner(1), page(1)).expect("a new page");
    {
        let mut latch = copy.write().expect("private");
        content(1, 1, 0).write(&mut latch);
        assert!(latch.attach(4, value(b"four")).is_none());
        assert!(latch.attach(5, value(b"fives")).is_none());
        assert_eq!(
            latch.attach(4, value(b"FOUR!!")).as_deref(),
            Some(&b"four"[..])
        );
        assert_eq!(latch.attached(4), Some(&b"FOUR!!"[..]));
        assert_eq!(latch.attached(6), None);
        assert_eq!(latch.attachments().count(), 2);
        assert!(latch.detach(6).is_none());
    }
    assert_eq!(pool.attached_bytes(), 11);
    assert_eq!(copy.read().attached(5), Some(&b"fives"[..]));
    assert_eq!(copy.read().attachments().count(), 2);

    let committed = pool.install(copy, version(1)).expect("a commit");
    assert_eq!(committed.attached(4), Some(&b"FOUR!!"[..]));
    assert_eq!(committed.attached(5), Some(&b"fives"[..]));
    assert_eq!(committed.attached(6), None);
    let mut tags: Vec<u32> = committed.attachments().map(|(tag, _)| tag).collect();
    tags.sort_unstable();
    assert_eq!(tags, [4, 5]);

    // The next transaction's copy shares the values, and can drop them.
    let next = pool
        .make_private(owner(2), page(1), version(1))
        .expect("in the pool");
    assert_eq!(pool.attached_bytes(), 22, "counted once for each frame");
    {
        let mut latch = next.write().expect("private");
        assert_eq!(latch.detach(4).as_deref(), Some(&b"FOUR!!"[..]));
        assert_eq!(latch.detach(5).as_deref(), Some(&b"fives"[..]));
        assert_eq!(latch.attachments().count(), 0);
    }
    assert_eq!(pool.attached_bytes(), 11);
    let second = pool.install(next, version(2)).expect("a commit");
    assert_eq!(second.attachments().count(), 0);
    assert_eq!(
        committed.attached(5),
        Some(&b"fives"[..]),
        "the old version"
    );
    check(&pool, 0);

    // A discarded copy gives its values up.
    let third = pool
        .make_private(owner(3), page(1), version(2))
        .expect("in the pool");
    third.write().expect("private").attach(1, value(&[0; 40]));
    assert_eq!(pool.attached_bytes(), 51);
    pool.discard(third).expect("a private copy");
    assert_eq!(pool.attached_bytes(), 11);
    check(&pool, 0);
}
