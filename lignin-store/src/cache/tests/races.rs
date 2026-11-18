//! Races on the pool: two or three threads that meet on one frame, one page
//! or one private copy, each written once and run three ways.
//!
//! - **Under loom**, which runs the pool's own code under the interleavings
//!   its partial-order reduction selects, up to the preemption bound of each
//!   model, and checks every access to a frame's bytes against the others
//!   (`sync::Tracked`): a write beside a read or another write fails the
//!   model, and so does one that no lock or atomic orders after the accesses
//!   before it.
//!   `RUSTFLAGS="--cfg loom" cargo test -p lignin-store --release --lib cache::tests::races`.
//!   `LOOM_MAX_PREEMPTIONS` sets another bound for all of them.
//! - **On real threads**, in every normal test run, each race many times.
//! - **Under Miri**, whose scheduler preempts at random points and which
//!   reports a data race, a use after free or a leak:
//!   `MIRIFLAGS="-Zmiri-many-seeds=0..64" cargo +nightly miri test -p lignin-store --lib cache::tests::races`.
//!
//! Loom's reduction keeps the last access to each atomic, not every
//! conflicting one, so it does not reach every interleaving. The other two
//! ways are there for what it leaves out.

#[cfg(loom)]
use ::loom::{
    sync::{atomic::AtomicU64 as SharedU64, Arc},
    thread,
};
#[cfg(not(loom))]
use std::{
    sync::{atomic::AtomicU64 as SharedU64, Arc},
    thread,
};

use super::*;

#[cfg(loom)]
fn model(preemptions: usize, body: impl Fn() + Sync + Send + 'static) {
    let mut builder = ::loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = Some(preemptions);
    }
    builder.max_branches = 100_000;
    builder.check(body);
}

/// Without loom the race is run on real threads, `LIGNIN_RACE_ROUNDS` times
/// or a few hundred.
#[cfg(not(loom))]
fn model(_preemptions: usize, body: impl Fn() + Sync + Send + 'static) {
    let rounds = std::env::var("LIGNIN_RACE_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(if cfg!(miri) { 2 } else { 300 });
    for _ in 0..rounds {
        body();
    }
}

/// Every page is on disk as version 1, and nothing follows it.
fn version_one(page: u32, _at: u64) -> Option<(Content, Option<u64>)> {
    Some((content(page, 1, 0), None))
}

fn fetch_one<'p>(pool: &'p BufferPool, page: u32, at: u64) -> PageRef<'p> {
    let found = match pool.get(self::page(page), version(at)) {
        Ok(found) => Some(found),
        Err(miss) => load_with(pool, miss, version_one),
    };
    found.expect("on disk")
}

fn join(threads: Vec<thread::JoinHandle<()>>) {
    for thread in threads {
        thread.join().expect("the thread did not panic");
    }
}

/// Pin, evict and reload: two readers, each of its own page, with one
/// frame between them. A reader's page is never the other's, a pinned frame
/// is never taken, and the frame beside the fixed one goes when it is
/// unpinned.
#[test]
fn two_readers_share_one_frame() {
    model(3, || {
        let pool = Arc::new(pool(1));
        let threads = [1, 2]
            .into_iter()
            .map(|page| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for _ in 0..2 {
                        let found = fetch_one(&pool, page, 5);
                        assert_eq!(Content::read(&found), content(page, 1, 0));
                    }
                })
            })
            .collect();
        join(threads);
        assert_eq!(pool.overflow_frames(), 0);
        assert_eq!(pool.census().published, 1);
        check(&pool, 0);
    });
}

/// Pin against evict, on one page: two readers of page 1 and one of page 2,
/// with one frame. Whoever holds page 1 keeps reading it while the frame is
/// wanted for page 2.
#[test]
fn a_page_is_read_while_its_frame_is_wanted() {
    model(2, || {
        let pool = Arc::new(pool(1));
        let threads = [1, 1, 2]
            .into_iter()
            .map(|page| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    let found = fetch_one(&pool, page, 5);
                    assert_eq!(Content::read(&found), content(page, 1, 0));
                    let again = found.clone();
                    drop(found);
                    assert_eq!(Content::read(&again), content(page, 1, 0));
                })
            })
            .collect();
        join(threads);
        assert_eq!(pool.overflow_frames(), 0);
        check(&pool, 0);
    });
}

/// Two loads of one page version at once: both readers end with the page,
/// one frame holds it, and the other frame is given back.
#[test]
fn two_loads_of_one_page_end_in_one_frame() {
    model(3, || {
        let pool = Arc::new(pool(2));
        let threads = (0..2)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    let found = fetch_one(&pool, 1, 5);
                    assert_eq!(Content::read(&found), content(1, 1, 0));
                })
            })
            .collect();
        join(threads);
        assert_eq!(pool.census().published, 1);
        check(&pool, 0);
    });
}

/// A frame beside the fixed one: the only fixed frame is pinned, and two
/// readers load another page. The frames they are given are freed exactly
/// once, whichever of them drops its pin last.
#[test]
fn a_frame_beside_the_fixed_one_is_freed_once() {
    model(3, || {
        let pool = Arc::new(pool(1));
        let held = seed(&pool, content(1, 1, 0), None);
        let threads = (0..2)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    let found = fetch_one(&pool, 2, 5);
                    assert_eq!(Content::read(&found), content(2, 1, 0));
                    let census = pool.census();
                    assert_eq!(census.overflow, census.overflow_unevictable);
                })
            })
            .collect();
        join(threads);
        assert_eq!(Content::read(&held), content(1, 1, 0));
        assert_eq!(pool.overflow_frames(), 0);
        drop(held);
        check(&pool, 0);
    });
}

/// Latch, and a page made private once (LT5): two operations of one owner
/// make the same page private at the same moment and each changes it under
/// the exclusive latch. One copy is made, and neither change is lost.
#[test]
fn two_operations_of_one_owner_share_one_private_copy() {
    model(3, || {
        let pool = Arc::new(pool(2));
        drop(seed(&pool, content(1, 1, 0), None));
        let threads = (0..2)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    let copy = pool
                        .make_private(owner(1), page(1), version(1))
                        .expect("in the pool");
                    {
                        let mut latch = copy.write().expect("private");
                        let before = Content::read(&latch);
                        content(1, 1, before.revision + 1).write(&mut latch);
                    }
                    let latch = copy.read();
                    assert!(Content::read(&latch).revision >= 1);
                })
            })
            .collect();
        join(threads);
        assert_eq!(pool.page_step(page(1)).private_copies(), 1);
        let copy = pool.private(owner(1), page(1)).expect("the copy");
        assert_eq!(Content::read(&copy.read()), content(1, 1, 2));
        drop(copy);
        check(&pool, 0);
    });
}

/// A commit installs a page version and publishes its version while two
/// readers read: a reader gets the version it asked at, whole, and never
/// the other one.
#[test]
fn a_reader_gets_the_version_it_asked_at() {
    model(3, || {
        let pool = Arc::new(pool(2));
        let latest = Arc::new(SharedU64::new(1));
        drop(create(&pool, 1, 1));
        let mut threads = Vec::new();
        {
            let (pool, latest) = (Arc::clone(&pool), Arc::clone(&latest));
            threads.push(thread::spawn(move || {
                drop(commit(&pool, 1, 1));
                latest.store(2, Ordering::SeqCst);
            }));
        }
        for _ in 0..2 {
            let (pool, latest) = (Arc::clone(&pool), Arc::clone(&latest));
            threads.push(thread::spawn(move || {
                let at = latest.load(Ordering::SeqCst);
                let found = pool
                    .get(page(1), version(at))
                    .expect("an unwritten page version stays");
                assert_eq!(Content::read(&found), content(1, at, 0));
            }));
        }
        join(threads);
        check(&pool, 0);
    });
}

/// A commit against another operation of its transaction that still holds
/// the copy's latch: the commit waits for the latch, and from the install
/// on nothing writes the frame, so a reader never sees it change.
#[test]
fn a_commit_waits_for_the_latch_of_the_copy_it_installs() {
    model(3, || {
        let pool = Arc::new(pool(2));
        let copy = pool.new_private(owner(1), page(1)).expect("a new page");
        content(1, 1, 0).write(&mut copy.write().expect("private"));
        drop(copy);
        let writer = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let Some(copy) = pool.private(owner(1), page(1)) else {
                    return;
                };
                match copy.write() {
                    Ok(mut latch) => content(1, 1, 1).write(&mut latch),
                    Err(error) => assert_eq!(error, CacheError::NotPrivate { page: page(1) }),
                };
            })
        };
        let committer = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let copy = pool.private(owner(1), page(1)).expect("the copy");
                let installed = pool.install(copy, version(1)).expect("a commit");
                let seen = Content::read(&installed);
                thread::yield_now();
                assert_eq!(Content::read(&installed), seen);
            })
        };
        let reader = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                if let Ok(found) = pool.get(page(1), version(1)) {
                    let seen = Content::read(&found);
                    thread::yield_now();
                    assert_eq!(Content::read(&found), seen);
                }
            })
        };
        join(vec![writer, committer, reader]);
        assert!(pool.private(owner(1), page(1)).is_none());
        check(&pool, 0);
    });
}

/// A page step on a page version that a commit is just installing: the
/// step finds no current version yet, or it replaces the one the commit
/// installed, and the commit still hands out a whole page.
#[test]
fn a_page_step_follows_a_commit_at_once() {
    model(3, || {
        let pool = Arc::new(pool(2));
        let copy = pool.new_private(owner(1), page(1)).expect("a new page");
        content(1, 1, 0).write(&mut copy.write().expect("private"));
        drop(copy);
        let committer = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let copy = pool.private(owner(1), page(1)).expect("the copy");
                let installed = pool.install(copy, version(1)).expect("a commit");
                assert_eq!(Content::read(&installed), content(1, 1, 0));
            })
        };
        let background = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let step = pool.page_step(page(1));
                let Some(current) = step.current() else {
                    return;
                };
                let mut blank = pool.blank();
                blank.copy_from(&current);
                content(1, 1, 1).write(&mut blank);
                drop(step.replace(&current, blank).expect("nothing in the way"));
            })
        };
        join(vec![committer, background]);
        let current = pool.get(page(1), version(1)).expect("installed");
        assert!(Content::read(&current).revision <= 1);
        assert_eq!(pool.oldest_unwritten(), Some(version(1)));
        drop(current);
        check(&pool, 0);
    });
}

/// A load against a commit: the page is on disk as version 1, a commit
/// makes version 2 of it, and a reader of version 1 loads the page while it
/// does. The reader gets version 1, and what it loaded never passes for the
/// page a later reader of version 2 gets.
#[test]
fn a_load_never_passes_for_a_newer_version() {
    model(3, || {
        let pool = Arc::new(pool(2));
        let committer = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let held = fetch_one(&pool, 1, 1);
                let copy = pool
                    .make_private(owner(2), page(1), version(1))
                    .expect("held");
                drop(held);
                content(1, 2, 0).write(&mut copy.write().expect("private"));
                drop(pool.install(copy, version(2)).expect("a commit"));
            })
        };
        let reader = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let found = fetch_one(&pool, 1, 1);
                assert_eq!(Content::read(&found), content(1, 1, 0));
            })
        };
        join(vec![committer, reader]);
        let new = pool.get(page(1), version(2)).expect("unwritten");
        assert_eq!(Content::read(&new), content(1, 2, 0));
        let old = fetch_one(&pool, 1, 1);
        assert_eq!(Content::read(&old), content(1, 1, 0));
        drop((new, old));
        check(&pool, 0);
    });
}

/// The page step against the making of a private copy (PV7): either the
/// step is refused, or the copy is made of what the step installed. A copy
/// of the page as it was before a step that went through would bring the
/// old bytes back at its commit.
#[test]
fn a_page_step_and_a_private_copy_agree() {
    model(3, || {
        let pool = Arc::new(pool(3));
        drop(seed(&pool, content(1, 1, 0), None));
        let stepped = Arc::new(SharedU64::new(0));
        let copied = Arc::new(SharedU64::new(0));
        let background = {
            let (pool, stepped) = (Arc::clone(&pool), Arc::clone(&stepped));
            thread::spawn(move || {
                let step = pool.page_step(page(1));
                let current = step.current().expect("in the pool");
                let mut blank = pool.blank();
                blank.copy_from(&current);
                content(1, 1, 1).write(&mut blank);
                match step.replace(&current, blank) {
                    Ok(new) => {
                        assert_eq!(Content::read(&new), content(1, 1, 1));
                        stepped.store(1, Ordering::SeqCst);
                    }
                    Err(refusal) => assert_eq!(refusal, StepRefusal::PrivateCopy),
                }
            })
        };
        let writer = {
            let (pool, copied) = (Arc::clone(&pool), Arc::clone(&copied));
            thread::spawn(move || {
                let copy = pool
                    .make_private(owner(1), page(1), version(1))
                    .expect("in the pool");
                let revision = Content::read(&copy.read()).revision;
                copied.store(u64::from(revision) + 1, Ordering::SeqCst);
            })
        };
        join(vec![background, writer]);
        let stepped = stepped.load(Ordering::SeqCst) == 1;
        let copied = copied.load(Ordering::SeqCst) - 1;
        assert_eq!(copied, u64::from(stepped), "the copy and the step disagree");
        check(&pool, 0);
    });
}

/// A page step against a reader and against write-back: the reader holds
/// one of the two frames whole, the old frame is freed once, and write-back
/// never marks the replaced frame as written.
#[test]
fn a_page_step_retires_the_frame_it_replaces() {
    model(2, || {
        let pool = Arc::new(pool(2));
        drop(create(&pool, 1, 1));
        let background = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let step = pool.page_step(page(1));
                let current = step.current().expect("in the pool");
                let mut blank = pool.blank();
                blank.copy_from(&current);
                content(1, 1, 1).write(&mut blank);
                drop(step.replace(&current, blank).expect("nothing in the way"));
            })
        };
        let reader = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let found = pool.get(page(1), version(1)).expect("unwritten");
                let seen = Content::read(&found);
                assert!(seen == content(1, 1, 0) || seen == content(1, 1, 1));
            })
        };
        let marked = Arc::new(SharedU64::new(u64::MAX));
        let flusher = {
            let (pool, marked) = (Arc::clone(&pool), Arc::clone(&marked));
            thread::spawn(move || {
                let frame = pool.get_version(page(1), version(1)).expect("unwritten");
                let written = Content::read(&frame);
                match pool.mark_written(&frame) {
                    Ok(()) => marked.store(u64::from(written.revision), Ordering::SeqCst),
                    Err(error) => {
                        assert!(matches!(error, CacheError::Replaced { .. }));
                        assert_eq!(written, content(1, 1, 0));
                    }
                }
            })
        };
        join(vec![background, reader, flusher]);
        // The new frame is owed to a segment unless it is the one that
        // write-back wrote: having written the old one does not count.
        let current = pool.get(page(1), version(1)).expect("there");
        assert_eq!(Content::read(&current), content(1, 1, 1));
        let owed = marked.load(Ordering::SeqCst) != 1;
        assert_eq!(current.frame().has(flag::UNWRITTEN), owed);
        assert_eq!(pool.oldest_unwritten().is_some(), owed);
        drop(current);
        check(&pool, 0);
    });
}

/// A reclaim against the last unpin: a page version that is held for
/// readers is dropped while a reader gives its pin back. The frame is freed
/// once, by one of the two.
#[test]
fn a_held_frame_is_freed_once() {
    model(3, || {
        let pool = Arc::new(pool(2));
        drop(create(&pool, 1, 1));
        pool.free(page(1), version(2)).expect("a later commit");
        let threads = (0..2)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    if let Ok(found) = pool.get(page(1), version(1)) {
                        assert_eq!(Content::read(&found), content(1, 1, 0));
                    }
                })
            })
            .chain([{
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    assert_eq!(pool.reclaim(&TestPins::at(2)), 1);
                })
            }])
            .collect();
        join(threads);
        assert_eq!(pool.census(), Census::default());
        check(&pool, 0);
    });
}

/// A discard against a writer of the same copy: the writer either changes
/// the copy before it goes or is told that it is gone, and the frame is
/// freed once.
#[test]
fn a_discarded_copy_is_not_written() {
    model(3, || {
        let pool = Arc::new(pool(2));
        let copy = pool.new_private(owner(1), page(1)).expect("a new page");
        content(1, 1, 0).write(&mut copy.write().expect("private"));
        drop(copy);
        let writer = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let Some(copy) = pool.private(owner(1), page(1)) else {
                    return;
                };
                let written = match copy.write() {
                    Ok(mut latch) => {
                        content(1, 1, 1).write(&mut latch);
                        1
                    }
                    Err(error) => {
                        assert_eq!(error, CacheError::NotPrivate { page: page(1) });
                        0
                    }
                };
                assert_eq!(Content::read(&copy.read()), content(1, 1, written));
            })
        };
        let discarder = {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                let copy = pool.private(owner(1), page(1)).expect("the copy");
                pool.discard(copy).expect("private");
            })
        };
        join(vec![writer, discarder]);
        assert_eq!(pool.census(), Census::default());
        check(&pool, 0);
    });
}
