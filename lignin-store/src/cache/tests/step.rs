//! The page step of background work.

use super::*;

/// A copy of `old` with one more revision, for a page step to install.
fn revised<'p>(pool: &'p BufferPool, old: &PageRef<'_>) -> Blank<'p> {
    let mut blank = pool.blank();
    blank.copy_from(old);
    let before = Content::read(&blank);
    content(before.page, before.born, before.revision + 1).write(&mut blank);
    blank
}

#[test]
fn a_page_step_puts_a_new_frame_in_place_of_the_current_version() {
    let pool = pool(4);
    let old = seed(&pool, content(1, 3, 0), None);
    let reader = old.clone();

    let step = pool.page_step(page(1));
    assert_eq!(step.page(), page(1));
    assert_eq!(step.private_copies(), 0);
    let current = step.current().expect("in the pool");
    assert_eq!(current.as_ptr(), old.as_ptr());
    assert!(step.is_current(&old));
    let new = step
        .replace(&current, revised(&pool, &current))
        .expect("nothing is in the way");
    assert!(!step.is_current(&old));
    assert!(step.is_current(&new));
    drop(step);

    // The page version is the same one, stored anew.
    assert_eq!(new.born(), version(3));
    assert_eq!(Content::read(&new), content(1, 3, 1));
    assert_eq!(
        pool.get(page(1), version(50)).expect("current").as_ptr(),
        new.as_ptr()
    );
    assert_eq!(
        pool.get_version(page(1), version(3))
            .expect("there")
            .as_ptr(),
        new.as_ptr()
    );
    // A reader that held the old frame still reads it, unchanged.
    assert_eq!(Content::read(&reader), content(1, 3, 0));
    assert_eq!(Content::read(&old), content(1, 3, 0));
    check(&pool, 1);

    // The content was in a segment before, and is owed to one now.
    let mut pages = Vec::new();
    pool.unwritten_pages(version(3), &mut pages);
    assert_eq!(pages, [page(1)]);
    assert_eq!(
        pool.mark_written(&old).err(),
        Some(CacheError::Replaced {
            page: page(1),
            born: version(3)
        })
    );
    assert_eq!(
        pool.hold_for_readers(&old).err(),
        Some(CacheError::Replaced {
            page: page(1),
            born: version(3)
        })
    );
    pool.mark_written(&new).expect("it is owed");

    drop((old, reader, current));
    assert_eq!(lock(&pool.turnover.free).len(), 3, "the old frame is free");
    check(&pool, 0);
}

#[test]
fn a_page_step_on_an_unwritten_version_keeps_it_on_its_list_once() {
    let pool = pool(4);
    let committed = create(&pool, 1, 5);
    let step = pool.page_step(page(1));
    let new = step
        .replace(&committed, revised(&pool, &committed))
        .expect("nothing is in the way");
    drop(step);
    let mut pages = Vec::new();
    pool.unwritten_pages(version(5), &mut pages);
    assert_eq!(pages, [page(1)]);
    assert_eq!(pool.census().unwritten, 1);
    drop(committed);
    check(&pool, 0);
    pool.mark_written(&new).expect("it is owed");
    assert_eq!(pool.oldest_unwritten(), None);
    check(&pool, 0);
}

#[test]
fn a_page_step_is_refused_under_a_private_copy() {
    let pool = pool(4);
    let old = seed(&pool, content(1, 3, 0), None);
    let copy = pool
        .make_private(owner(1), page(1), version(3))
        .expect("in the pool");

    let step = pool.page_step(page(1));
    assert_eq!(step.private_copies(), 1);
    assert!(step.is_current(&old));
    assert_eq!(
        step.replace(&old, revised(&pool, &old)).err(),
        Some(StepRefusal::PrivateCopy)
    );
    drop(step);
    assert_eq!(
        lock(&pool.turnover.free).len(),
        2,
        "the refused frame came back"
    );

    // Once the copy is given up, the step goes through.
    pool.discard(&copy).expect("a private copy");
    drop(copy);
    let step = pool.page_step(page(1));
    assert_eq!(step.private_copies(), 0);
    assert!(step.replace(&old, revised(&pool, &old)).is_ok());
    drop(step);
    check(&pool, 1);
}

#[test]
fn a_page_step_is_refused_once_the_page_has_moved_on() {
    let pool = pool(8);
    let old = seed(&pool, content(1, 3, 0), None);

    // A commit superseded the page.
    let newer = commit(&pool, 1, 3);
    let step = pool.page_step(page(1));
    assert!(!step.is_current(&old));
    assert_eq!(
        step.replace(&old, revised(&pool, &old)).err(),
        Some(StepRefusal::NotCurrent)
    );
    // Another step replaced the frame.
    let replaced = step
        .replace(&newer, revised(&pool, &newer))
        .expect("the current version");
    assert_eq!(
        step.replace(&newer, revised(&pool, &newer)).err(),
        Some(StepRefusal::NotCurrent)
    );
    drop(step);

    // A frame of another page is not this page's current version.
    let other = seed(&pool, content(2, 1, 0), None);
    let step = pool.page_step(page(1));
    assert!(!step.is_current(&other));
    assert_eq!(
        step.replace(&other, revised(&pool, &replaced)).err(),
        Some(StepRefusal::NotCurrent)
    );
    drop(step);

    // A commit freed the page.
    pool.free(page(1), version(9)).expect("a later commit");
    let step = pool.page_step(page(1));
    assert!(step.current().is_none());
    assert_eq!(
        step.replace(&replaced, revised(&pool, &replaced)).err(),
        Some(StepRefusal::NotCurrent)
    );
    drop(step);
    assert_eq!(Content::read(&replaced), content(1, 4, 1));
    check(&pool, 1);
}

#[test]
fn a_page_step_finds_no_current_version_of_a_page_that_is_not_in_the_pool() {
    let pool = pool(2);
    let step = pool.page_step(page(1));
    assert!(step.current().is_none());
    assert_eq!(step.private_copies(), 0);
}

#[test]
fn a_pending_value_is_placed_in_a_page_step() {
    let pool = pool(4);
    let copy = pool.new_private(owner(1), page(1)).expect("a new page");
    {
        let mut latch = copy.write().expect("private");
        content(1, 1, 0).write(&mut latch);
        latch.attach(name(1), value(&[1; 30]));
        latch.attach(name(2), value(&[2; 50]));
    }
    let committed = pool.install(&copy, version(1)).expect("a commit");
    drop(copy);
    assert_eq!(pool.attached_bytes(), 80);
    assert_eq!(
        pool.mark_written(&committed).err(),
        Some(CacheError::AttachedValues {
            page: page(1),
            born: version(1)
        })
    );

    // Write-back places value 1: the new frame holds the pointer in its
    // bytes, and no longer the value.
    let step = pool.page_step(page(1));
    let mut blank = revised(&pool, &committed);
    assert_eq!(blank.attached(name(1)), Some(&[1; 30][..]));
    assert_eq!(blank.detach(name(1)).as_deref(), Some(&[1; 30][..]));
    assert!(blank.detach(name(1)).is_none());
    let placed = step.replace(&committed, blank).expect("nothing in the way");
    drop(step);
    assert_eq!(placed.attached(name(1)), None);
    assert_eq!(placed.attached(name(2)), Some(&[2; 50][..]));
    assert_eq!(
        committed.attached(name(1)),
        Some(&[1; 30][..]),
        "the old frame"
    );
    assert_eq!(
        pool.attached_bytes(),
        130,
        "both frames, until the old goes"
    );
    drop(committed);
    assert_eq!(pool.attached_bytes(), 50);

    // With the last value placed, the page can be written back.
    let step = pool.page_step(page(1));
    let mut blank = revised(&pool, &placed);
    blank.detach(name(2));
    let done = step.replace(&placed, blank).expect("nothing in the way");
    drop((step, placed));
    assert_eq!(pool.attached_bytes(), 0);
    pool.mark_written(&done).expect("no value is left");
    assert_eq!(Content::read(&done), content(1, 1, 2));
    assert_eq!(pool.census().unevictable, 1, "only the pin keeps it");
    check(&pool, 0);
}

#[test]
fn a_frame_that_is_copied_over_loses_what_was_attached_to_it() {
    let pool = pool(4);
    let plain = seed(&pool, content(1, 1, 0), None);
    let mut blank = pool.blank();
    blank.attach(name(9), value(&[9; 20]));
    assert_eq!(pool.attached_bytes(), 20);
    blank.copy_from(&plain);
    assert_eq!(blank.attached(name(9)), None);
    assert_eq!(pool.attached_bytes(), 0);
    assert_eq!(Content::read(&blank), content(1, 1, 0));
    blank.attach(name(3), value(&[3; 5]));
    drop(blank);
    assert_eq!(
        pool.attached_bytes(),
        0,
        "a dropped blank gives its values up"
    );
}
