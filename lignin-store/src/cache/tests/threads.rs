//! Threads on one pool: readers on snapshots of different ages, a committer
//! that changes, creates and frees pages, write-back, page steps, and a
//! watcher of the pool's memory, all at once.
//!
//! Every read is compared with the history of commits, and every page that
//! is not in the pool is loaded from a disk that holds only what write-back
//! put there.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, RwLock,
    },
    thread,
};

use super::*;

const PAGES: u32 = 8;

#[derive(Default)]
struct Registry {
    latest: u64,
    pins: BTreeMap<u64, usize>,
}

/// What the threads share beside the pool.
#[derive(Default)]
struct World {
    disk: Mutex<Disk>,
    /// For each page, every version a commit bore: its born version, and the
    /// version that superseded it or `u64::MAX`. A commit adds its versions
    /// before it is published.
    history: RwLock<BTreeMap<u32, Vec<(u64, u64)>>>,
    registry: Mutex<Registry>,
    epoch: AtomicU64,
    done: AtomicBool,
    reads: AtomicU64,
    loads: AtomicU64,
    steps: AtomicU64,
    written: AtomicU64,
}

impl World {
    /// Pins the latest version, as a snapshot does.
    fn pin(&self) -> u64 {
        let mut registry = self.registry.lock().expect("no panic");
        let latest = registry.latest;
        *registry.pins.entry(latest).or_default() += 1;
        latest
    }

    fn unpin(&self, at: u64) {
        let mut registry = self.registry.lock().expect("no panic");
        let count = registry.pins.get_mut(&at).expect("pinned");
        *count -= 1;
        if *count == 0 {
            registry.pins.remove(&at);
        }
    }

    fn publish(&self, version: u64) {
        self.registry.lock().expect("no panic").latest = version;
    }

    fn born(&self, page: u32, at: u64) -> Option<u64> {
        let history = self.history.read().expect("no panic");
        history
            .get(&page)?
            .iter()
            .find(|&&(born, superseded)| born <= at && at < superseded)
            .map(|&(born, _)| born)
    }

    /// Reads a page at a pinned version and checks it against the history.
    fn read<'p>(&self, pool: &'p BufferPool, page: u32, at: u64) -> Option<PageRef<'p>> {
        let found = match pool.get(self::page(page), version(at)) {
            Ok(found) => Some(found),
            Err(miss) => {
                self.loads.fetch_add(1, Ordering::Relaxed);
                load_with(pool, miss, |page, at| {
                    self.disk.lock().expect("no panic").lookup(page, at)
                })
            }
        };
        self.reads.fetch_add(1, Ordering::Relaxed);
        let expected = self.born(page, at);
        match &found {
            Some(found) => {
                let content = Content::read(found);
                assert_eq!(content.page, page);
                assert_eq!(Some(content.born), expected, "page {page} at {at}");
                assert_eq!(found.born(), version(content.born));
            }
            None => assert_eq!(expected, None, "page {page} at {at} is missing"),
        }
        found
    }
}

impl Pins for World {
    fn oldest_pin(&self) -> CommitVersion {
        let registry = self.registry.lock().expect("no panic");
        let oldest = registry.pins.keys().next().copied();
        version(oldest.unwrap_or(registry.latest).min(registry.latest))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.epoch.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// Every operation of this test holds a version pin, so no guard is
    /// ever older than the current epoch.
    fn oldest_guard(&self) -> Epoch {
        Epoch::new(self.epoch.load(Ordering::SeqCst))
    }
}

fn committer(pool: &BufferPool, world: &World, commits: u64, seed: u64) {
    let mut rng = Rng::new(seed);
    for next in 1..=commits {
        let at = next - 1;
        let pin = world.pin();
        assert_eq!(pin, at);
        let mut pages = vec![1 + rng.below(u64::from(PAGES)) as u32];
        let second = 1 + rng.below(u64::from(PAGES)) as u32;
        if second != pages[0] {
            pages.push(second);
        }
        let mut copies = Vec::new();
        for &page in &pages {
            let copy = if world.born(page, at).is_some() {
                // A page step can replace the page that was loaded, and
                // write-back and the clock can then take the new frame, so
                // the page is loaded for as long as it is missed.
                let mut held = None;
                loop {
                    match pool.make_private(owner(next), self::page(page), version(at)) {
                        Ok(copy) => break copy,
                        Err(_) => {
                            drop(held.take());
                            held = world.read(pool, page, at);
                            assert!(held.is_some(), "a live page can be loaded");
                        }
                    }
                }
            } else {
                pool.new_private(owner(next), self::page(page))
                    .expect("no copy yet")
            };
            content(page, next, 0).write(&mut copy.write().expect("private"));
            copies.push((page, copy));
        }
        let freed = (1..=PAGES)
            .find(|page| !pages.contains(page) && world.born(*page, at).is_some())
            .filter(|_| rng.chance(6));

        if rng.chance(3) {
            thread::yield_now();
        }
        for (_, copy) in copies {
            drop(pool.install(&copy, version(next)).expect("a commit"));
        }
        if let Some(page) = freed {
            world.disk.lock().expect("no panic").free(page, next);
            pool.free(self::page(page), version(next))
                .expect("a commit");
        }
        let mut history = world.history.write().expect("no panic");
        for &page in pages.iter().chain(&freed) {
            let versions = history.entry(page).or_default();
            if let Some(last) = versions.last_mut().filter(|last| last.1 == u64::MAX) {
                last.1 = next;
            }
            if Some(page) != freed {
                versions.push((next, u64::MAX));
            }
        }
        drop(history);
        world.publish(next);
        world.unpin(pin);
        pool.reclaim(world);
    }
    world.done.store(true, Ordering::SeqCst);
}

fn reader(pool: &BufferPool, world: &World, seed: u64) {
    let mut rng = Rng::new(seed);
    while !world.done.load(Ordering::SeqCst) {
        let at = world.pin();
        let mut held = Vec::new();
        for _ in 0..1 + rng.below(12) {
            let page = 1 + rng.below(u64::from(PAGES)) as u32;
            if let Some(found) = world.read(pool, page, at)
                && rng.chance(3)
            {
                held.push(found);
            }
            if rng.chance(4) {
                thread::yield_now();
            }
        }
        for found in &held {
            let content = Content::read(found);
            assert_eq!(world.born(content.page, at), Some(content.born));
        }
        drop(held);
        world.unpin(at);
        pool.reclaim(world);
    }
}

/// Writes back what is owed, oldest commit first. A page version whose page
/// was freed meanwhile is left to the pool, as the page table would refuse
/// its link (`page-table.md` T18).
fn flusher(pool: &BufferPool, world: &World) {
    let mut versions = Vec::new();
    let mut pages = Vec::new();
    loop {
        let done = world.done.load(Ordering::SeqCst);
        versions.clear();
        pool.unwritten_versions(&mut versions);
        for &born in &versions {
            pages.clear();
            pool.unwritten_pages(born, &mut pages);
            for &page in &pages {
                let Some(frame) = pool.get_version(page, born) else {
                    continue;
                };
                let content = Content::read(&frame);
                let mut disk = world.disk.lock().expect("no panic");
                let chain = disk.chains.entry(content.page).or_default();
                let fits = chain.last().is_none_or(|last| match last {
                    Link::Page(last) => last.born <= content.born,
                    Link::Free(freed) => *freed < content.born,
                });
                if !fits {
                    // The page table refuses a link behind a free link, and
                    // write-back then leaves the page version to the pool:
                    // the commit that freed the page is about to tell it.
                    continue;
                }
                disk.write(content);
                drop(disk);
                match pool.mark_written(&frame) {
                    Ok(()) => {
                        world.written.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(CacheError::NotUnwritten { .. } | CacheError::Replaced { .. }) => {}
                    Err(other) => panic!("{other}"),
                }
            }
        }
        if done {
            break;
        }
        thread::yield_now();
    }
}

/// Changes how the current version of a page is stored, as the cleaner does
/// when it moves a value.
fn stepper(pool: &BufferPool, world: &World, seed: u64) {
    let mut rng = Rng::new(seed);
    while !world.done.load(Ordering::SeqCst) {
        let page = 1 + rng.below(u64::from(PAGES)) as u32;
        let at = world.pin();
        let looked = world.read(pool, page, at);
        world.unpin(at);
        let Some(looked) = looked else {
            continue;
        };
        let mut blank = pool.blank();
        blank.copy_from(&looked);
        let before = Content::read(&blank);
        content(page, before.born, before.revision + 1).write(&mut blank);
        let step = pool.page_step(self::page(page));
        if step.is_current(&looked) && step.private_copies() == 0 {
            let new = step
                .replace(&looked, blank)
                .expect("checked under the step");
            assert_eq!(new.born(), looked.born());
            world.steps.fetch_add(1, Ordering::Relaxed);
        }
        drop(step);
        thread::yield_now();
    }
}

/// Watches that every frame beside the fixed ones is in use.
fn watcher(pool: &BufferPool, world: &World) {
    while !world.done.load(Ordering::SeqCst) {
        let census = pool.census();
        assert_eq!(census.overflow, census.overflow_unevictable);
        thread::yield_now();
    }
}

fn run(frames: usize, commits: u64, readers: u64) {
    let pool = wide_pool(frames);
    let world = World::default();
    thread::scope(|scope| {
        scope.spawn(|| committer(&pool, &world, commits, 1));
        scope.spawn(|| flusher(&pool, &world));
        scope.spawn(|| stepper(&pool, &world, 2));
        scope.spawn(|| watcher(&pool, &world));
        for seed in 0..readers {
            let (pool, world) = (&pool, &world);
            scope.spawn(move || reader(pool, world, 10 + seed));
        }
    });

    assert!(world.reads.load(Ordering::Relaxed) > 0);
    assert_eq!(pool.census().private, 0);
    flusher(&pool, &world);
    pool.reclaim(&world);
    check(&pool, 0);
    let census = pool.census();
    assert_eq!((census.pinned, census.held), (0, 0));
    assert_eq!(pool.overflow_frames(), 0);
    assert_eq!(pool.memory(), pool.fixed_memory());
    let latest = world.registry.lock().expect("no panic").latest;
    assert_eq!(latest, commits);
    for page in 1..=PAGES {
        drop(world.read(&pool, page, latest));
    }
}

#[test]
fn threads_read_what_was_committed() {
    if cfg!(miri) {
        run(2, 12, 2);
    } else {
        for frames in [1, 3, 16] {
            run(frames, 3_000, 4);
        }
    }
}
