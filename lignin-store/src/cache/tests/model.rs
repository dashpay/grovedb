//! A random life of a pool, checked against a model of the pages: commits
//! that change, create and free pages, a transaction that is rolled back,
//! snapshots of older versions, write-back, page steps and memory pressure.
//!
//! After every step the pool's own invariants are checked (`check`), and
//! every read is compared with what the model says that version of the page
//! holds. A page that is not in the pool is loaded from the model's disk,
//! which holds only what write-back put there, so a page version that the
//! pool let go before it was written back cannot be read.

use std::collections::BTreeMap;

use super::*;

const PAGES: u32 = 6;

type Values = BTreeMap<u32, Vec<u8>>;

#[derive(Debug, Clone)]
struct Version {
    born: u64,
    superseded: Option<u64>,
    revision: u32,
    attached: Values,
}

struct Copy<'p> {
    page: u32,
    handle: PrivateRef<'p>,
    attached: Values,
}

struct Life<'p> {
    pool: &'p BufferPool,
    rng: Rng,
    latest: u64,
    pages: BTreeMap<u32, Vec<Version>>,
    disk: Disk,
    snapshots: Vec<u64>,
    pins: TestPins,
    owners: u64,
    copies: Vec<Copy<'p>>,
    reads: u64,
    refused_steps: u64,
    held: u64,
}

fn values_of<'a>(attachments: impl Iterator<Item = (u32, &'a [u8])>) -> Values {
    attachments
        .map(|(tag, bytes)| (tag, bytes.to_vec()))
        .collect()
}

impl<'p> Life<'p> {
    fn new(pool: &'p BufferPool, seed: u64) -> Self {
        Self {
            pool,
            rng: Rng::new(seed),
            latest: 0,
            pages: BTreeMap::new(),
            disk: Disk::default(),
            snapshots: Vec::new(),
            pins: TestPins::at(0),
            owners: 0,
            copies: Vec::new(),
            reads: 0,
            refused_steps: 0,
            held: 0,
        }
    }

    fn visible(&self, page: u32, at: u64) -> Option<&Version> {
        self.pages.get(&page)?.iter().find(|version| {
            version.born <= at && version.superseded.is_none_or(|superseded| at < superseded)
        })
    }

    fn current_mut(&mut self, page: u32) -> Option<&mut Version> {
        self.pages
            .get_mut(&page)?
            .last_mut()
            .filter(|version| version.superseded.is_none())
    }

    fn is_live(&self, page: u32) -> bool {
        self.visible(page, self.latest).is_some()
    }

    fn fetch(&self, page: u32, at: u64) -> Option<PageRef<'p>> {
        fetch(self.pool, &self.disk, page, at)
    }

    fn repin(&self) {
        let oldest = self.snapshots.iter().copied().min().unwrap_or(self.latest);
        self.pins.set_oldest(oldest.min(self.latest));
        self.pool.reclaim(&self.pins);
    }

    fn read(&mut self, page: u32, at: u64) {
        self.reads += 1;
        let found = self.fetch(page, at);
        match (self.visible(page, at), found) {
            (None, None) => {}
            (Some(expected), Some(found)) => {
                assert_eq!(
                    Content::read(&found),
                    content(page, expected.born, expected.revision),
                    "page {page} at version {at}"
                );
                assert_eq!(found.born(), version(expected.born));
                assert_eq!(values_of(found.attachments()), expected.attached);
            }
            (expected, found) => panic!(
                "page {page} at version {at}: the model has {expected:?} and the pool {found:?}"
            ),
        }
    }

    fn read_some(&mut self) {
        let page = 1 + self.rng.below(u64::from(PAGES)) as u32;
        let at = match self.snapshots.len() as u64 {
            0 => self.latest,
            count => match self.rng.below(count + 1) {
                index if index == count => self.latest,
                index => self.snapshots[index as usize],
            },
        };
        self.read(page, at);
    }

    /// Makes one more page private to the open transaction.
    fn touch(&mut self) {
        let page = 1 + self.rng.below(u64::from(PAGES)) as u32;
        if self.copies.iter().any(|copy| copy.page == page) {
            return;
        }
        if self.copies.is_empty() {
            self.owners += 1;
        }
        let owner = owner(self.owners);
        let (handle, mut attached) = match self.visible(page, self.latest) {
            Some(base) => {
                let attached = base.attached.clone();
                let born = base.born;
                let mut loaded = None;
                let handle = loop {
                    match self
                        .pool
                        .make_private(owner, self::page(page), version(self.latest))
                    {
                        Ok(handle) => break handle,
                        Err(miss) => {
                            assert!(loaded.is_none(), "a page that is held was missed");
                            loaded = load(self.pool, &self.disk, miss);
                            assert!(loaded.is_some(), "a live page can be loaded");
                        }
                    }
                };
                drop(loaded);
                assert_eq!(handle.base(), Some(version(born)));
                (handle, attached)
            }
            None => {
                let handle = self
                    .pool
                    .new_private(owner, self::page(page))
                    .expect("no copy yet");
                (handle, Values::new())
            }
        };
        assert_eq!(values_of(handle.read().attachments()), attached);
        let mut latch = handle.write().expect("private");
        if self.rng.chance(3) {
            let tag = 1 + self.rng.below(3) as u32;
            let bytes = vec![tag as u8; 1 + self.rng.below(40) as usize];
            latch.attach(tag, value(&bytes));
            attached.insert(tag, bytes);
        }
        if self.rng.chance(3) {
            let tag = 1 + self.rng.below(3) as u32;
            assert_eq!(latch.detach(tag).is_some(), attached.remove(&tag).is_some());
        }
        drop(latch);
        self.copies.push(Copy {
            page,
            handle,
            attached,
        });
    }

    fn rollback(&mut self) {
        for copy in self.copies.drain(..) {
            self.pool.discard(copy.handle).expect("a private copy");
        }
    }

    /// Commits the open transaction, and frees some pages it did not touch.
    fn commit(&mut self) {
        let next = self.latest + 1;
        let copies = std::mem::take(&mut self.copies);
        let mut freed = Vec::new();
        for page in 1..=PAGES {
            let untouched = copies.iter().all(|copy| copy.page != page);
            if untouched && self.is_live(page) && self.rng.chance(8) {
                freed.push(page);
            }
        }
        if copies.is_empty() && freed.is_empty() {
            return;
        }
        for copy in copies {
            content(copy.page, next, 0).write(&mut copy.handle.write().expect("private"));
            let installed = self
                .pool
                .install(copy.handle, version(next))
                .expect("a commit");
            assert_eq!(installed.born(), version(next));
            if let Some(before) = self.current_mut(copy.page) {
                before.superseded = Some(next);
            }
            self.pages.entry(copy.page).or_default().push(Version {
                born: next,
                superseded: None,
                revision: 0,
                attached: copy.attached,
            });
        }
        for page in freed {
            self.disk.free(page, next);
            self.pool
                .free(self::page(page), version(next))
                .expect("a commit");
            self.current_mut(page).expect("live").superseded = Some(next);
        }
        self.latest = next;
        self.repin();
    }

    fn snapshot(&mut self) {
        if self.snapshots.len() < 4 && self.rng.chance(2) {
            self.snapshots.push(self.latest);
        } else if !self.snapshots.is_empty() {
            let index = self.rng.below(self.snapshots.len() as u64) as usize;
            self.snapshots.swap_remove(index);
        }
        self.repin();
    }

    fn model_version(&mut self, page: u32, born: u64) -> &mut Version {
        self.pages
            .get_mut(&page)
            .and_then(|versions| versions.iter_mut().find(|version| version.born == born))
            .expect("the pool lists a page version the model has")
    }

    /// One round of write-back over everything that is owed, oldest commit
    /// first. `all` says whether to take every page or to leave some.
    fn write_back(&mut self, all: bool) {
        let mut versions = Vec::new();
        self.pool.unwritten_versions(&mut versions);
        assert!(versions.is_sorted());
        let mut blocked = Vec::new();
        for born in versions {
            let mut pages = Vec::new();
            self.pool.unwritten_pages(born, &mut pages);
            for page in pages {
                if blocked.contains(&page) || (!all && self.rng.chance(4)) {
                    blocked.push(page);
                    continue;
                }
                if !self.write_back_page(page.get(), born.get()) {
                    blocked.push(page);
                }
            }
        }
    }

    /// Writes one page version back, placing its attached values first.
    /// `false` if it has to wait for the open transaction.
    fn write_back_page(&mut self, page: u32, born: u64) -> bool {
        let pool = self.pool;
        let mut frame = pool
            .get_version(self::page(page), version(born))
            .expect("an unwritten page version is in the pool");
        let superseded = self.model_version(page, born).superseded.is_some();
        let tags: Vec<u32> = frame.attachments().map(|(tag, _)| tag).collect();
        if superseded && !tags.is_empty() {
            pool.hold_for_readers(&frame).expect("superseded and owed");
            self.held += 1;
            return true;
        }
        for tag in tags {
            let step = pool.page_step(self::page(page));
            if step.private_copies() > 0 {
                assert!(self.copies.iter().any(|copy| copy.page == page));
                return false;
            }
            let mut blank = pool.blank();
            blank.copy_from(&frame);
            let before = Content::read(&blank);
            content(page, born, before.revision + 1).write(&mut blank);
            assert!(blank.detach(tag).is_some());
            frame = step.replace(&frame, blank).expect("checked under the step");
            let version = self.model_version(page, born);
            version.revision += 1;
            assert!(version.attached.remove(&tag).is_some());
        }
        self.disk.write(Content::read(&frame));
        pool.mark_written(&frame)
            .expect("owed, and no value is left");
        true
    }

    /// Background work changes how the current version of a page is stored.
    fn page_step(&mut self) {
        let page = 1 + self.rng.below(u64::from(PAGES)) as u32;
        let pool = self.pool;
        let kept = self.fetch(page, self.latest);
        let step = pool.page_step(self::page(page));
        let current = step.current();
        assert_eq!(current.is_some(), self.is_live(page));
        let Some(current) = current else {
            return;
        };
        assert_eq!(
            current.as_ptr(),
            kept.as_ref().expect("a live page").as_ptr()
        );
        let mut blank = pool.blank();
        blank.copy_from(&current);
        let before = Content::read(&blank);
        content(page, before.born, before.revision + 1).write(&mut blank);
        let copied = self.copies.iter().any(|copy| copy.page == page);
        match step.replace(&current, blank) {
            Ok(new) => {
                assert!(!copied, "a page step went through under a private copy");
                assert_eq!(new.born(), current.born());
                self.current_mut(page).expect("live").revision += 1;
            }
            Err(refusal) => {
                assert_eq!(refusal, StepRefusal::PrivateCopy);
                assert!(copied);
                self.refused_steps += 1;
            }
        }
    }

    fn pressure(&mut self) {
        for _ in 0..PAGES {
            self.read_some();
        }
    }

    fn step(&mut self) {
        match self.rng.below(100) {
            0..30 => self.read_some(),
            30..48 => self.touch(),
            48..60 => self.commit(),
            60..64 => self.rollback(),
            64..74 => self.snapshot(),
            74..86 => self.write_back(false),
            86..95 => self.page_step(),
            _ => self.pressure(),
        }
        check(self.pool, 0);
        let census = self.pool.census();
        assert_eq!(census.private, self.copies.len());
        assert!(
            self.pool.memory()
                <= self.pool.fixed_memory()
                    + census.unevictable * self.pool.overflow_cost()
                    + self.pool.attached_bytes()
        );
    }

    /// Ends everything that keeps a page in memory, and checks that the
    /// pool is left with nothing it cannot evict.
    fn finish(mut self) -> (u64, u64, u64) {
        self.commit();
        self.rollback();
        self.snapshots.clear();
        self.repin();
        self.write_back(true);
        self.repin();
        check(self.pool, 0);
        let census = self.pool.census();
        assert_eq!(
            (census.private, census.unwritten, census.held, census.pinned),
            (0, 0, 0, 0)
        );
        assert_eq!(census.unevictable, 0);
        assert_eq!(self.pool.overflow_frames(), 0);
        assert_eq!(self.pool.attached_bytes(), 0);
        assert_eq!(self.pool.memory(), self.pool.fixed_memory());
        assert!(census.published <= self.pool.frames());
        for page in 1..=PAGES {
            self.read(page, self.latest);
        }
        (self.reads, self.refused_steps, self.held)
    }
}

fn live(frames: usize, seed: u64, steps: usize) -> (u64, u64, u64) {
    let pool = if seed.is_multiple_of(2) {
        pool(frames)
    } else {
        wide_pool(frames)
    };
    let mut life = Life::new(&pool, seed);
    for _ in 0..steps {
        life.step();
    }
    let latest = life.latest;
    let done = life.finish();
    assert!(latest > steps as u64 / 20, "the life made commits");
    done
}

#[test]
fn a_random_life_reads_what_the_model_holds() {
    let steps = if cfg!(miri) { 150 } else { 3_000 };
    let mut reads = 0;
    let mut refused = 0;
    let mut held = 0;
    for frames in [1, 2, 3, 8] {
        for seed in 0..if cfg!(miri) { 2 } else { 6 } {
            let (life_reads, life_refused, life_held) =
                live(frames, seed * 31 + frames as u64, steps);
            reads += life_reads;
            refused += life_refused;
            held += life_held;
        }
    }
    assert!(reads > 0);
    if !cfg!(miri) {
        assert!(refused > 10, "page steps met private copies: {refused}");
        assert!(held > 10, "superseded versions were held: {held}");
    }
}
