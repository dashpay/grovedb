//! The allocator under threads: writers that allocate, commit and roll
//! back, readers on snapshots and in operations, and reclaims, all at once.
//! First an id is freed under a reader on a snapshot and another under a
//! reader in an operation, on purpose; then the threads run free for a
//! number of rounds.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    thread,
};

use super::*;

const IDS: u32 = 48;
const THREADS: u64 = 4;
/// The rounds of each writer.
const ROUNDS: u64 = if cfg!(miri) { 20 } else { 10_000 };

/// Counts a writer out when it ends, also when it panics.
struct Leaving<'a>(&'a AtomicU64);

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Tells the other threads to stop if the thread that holds it panics.
struct Failed<'a>(&'a AtomicBool);

impl Drop for Failed<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

struct Shared {
    ids: PageAllocator,
    pins: Registry,
    /// The commit lock, around the page each id names in the latest
    /// version.
    committed: Mutex<BTreeMap<u32, u64>>,
    /// The page each id was last handed out for.
    named: Vec<AtomicU64>,
    /// Whether a transaction or a committed page has the id.
    held: Vec<AtomicBool>,
    next_page: AtomicU64,
    reused: AtomicU64,
    checked: AtomicU64,
    started: AtomicBool,
    writing: AtomicU64,
    failed: AtomicBool,
}

impl Shared {
    fn committed(&self) -> std::sync::MutexGuard<'_, BTreeMap<u32, u64>> {
        self.committed.lock().expect("no thread panicked")
    }

    fn give_up(&self, raw: u32) {
        assert!(self.held[raw as usize].swap(false, Ordering::SeqCst));
    }

    fn reclaim(&self) -> Vec<PageId> {
        let mut released = Vec::new();
        self.ids.reclaim(&self.pins, &mut released);
        self.reused
            .fetch_add(released.len() as u64, Ordering::Relaxed);
        released
    }

    /// Waits until every thread is started. False if one failed first.
    fn start(&self) -> bool {
        while !self.started.load(Ordering::SeqCst) {
            if self.failed.load(Ordering::SeqCst) {
                return false;
            }
            thread::yield_now();
        }
        true
    }

    fn progress(&self) -> (u64, u64) {
        (
            self.reused.load(Ordering::Relaxed),
            self.checked.load(Ordering::Relaxed),
        )
    }

    fn write(&self, seed: u64) {
        let _leaving = Leaving(&self.writing);
        let _failed = Failed(&self.failed);
        if !self.start() {
            return;
        }
        let mut rng = Rng::new(seed);
        for _ in 0..ROUNDS {
            if self.failed.load(Ordering::SeqCst) {
                break;
            }
            thread::yield_now();
            let mut private = Vec::new();
            for _ in 0..rng.below(4) {
                let Ok(id) = self.ids.allocate() else { break };
                let raw = id.get();
                assert!(
                    !self.held[raw as usize].swap(true, Ordering::SeqCst),
                    "id {raw} was handed out twice"
                );
                let new = self.next_page.fetch_add(1, Ordering::SeqCst);
                self.named[raw as usize].store(new, Ordering::SeqCst);
                private.push((raw, new));
            }
            if rng.below(4) == 0 {
                for (raw, _) in private {
                    self.give_up(raw);
                    self.ids.release(page(raw)).expect("it is private");
                }
                continue;
            }
            {
                let mut committed = self.committed();
                let commit = self.pins.latest() + 1;
                for _ in 0..rng.below(4) {
                    let count = committed.len() as u64;
                    if count == 0 {
                        break;
                    }
                    let raw = *committed
                        .keys()
                        .nth(rng.below(count) as usize)
                        .expect("in range");
                    committed.remove(&raw);
                    self.give_up(raw);
                    self.ids
                        .free(page(raw), version(commit))
                        .expect("it is live");
                }
                for (raw, new) in private {
                    self.ids.publish(page(raw)).expect("it is private");
                    committed.insert(raw, new);
                }
                self.pins.publish(commit);
            }
            self.reclaim();
        }
    }

    /// Checks, again and again, that every id of `view` still names the
    /// page it named when the view was taken.
    fn look(&self, view: &BTreeMap<u32, u64>, rng: &mut Rng) {
        for _ in 0..1 + rng.below(6) {
            for (raw, seen) in view {
                let now = self.named[*raw as usize].load(Ordering::SeqCst);
                assert_eq!(now, *seen, "id {raw} was handed out under a reader");
            }
            self.checked.fetch_add(view.len() as u64, Ordering::Relaxed);
            thread::yield_now();
        }
    }

    fn read(&self, seed: u64) {
        let _failed = Failed(&self.failed);
        if !self.start() {
            return;
        }
        let mut rng = Rng::new(seed);
        while self.writing.load(Ordering::SeqCst) > 0 {
            if rng.below(2) == 0 {
                let (pinned, view) = {
                    let committed = self.committed();
                    (self.pins.pin(), committed.clone())
                };
                self.look(&view, &mut rng);
                self.pins.unpin(pinned);
            } else {
                let guard = self.pins.guard();
                let view = self.committed().clone();
                self.look(&view, &mut rng);
                self.pins.unguard(guard);
            }
            if rng.below(3) == 0 {
                self.reclaim();
            }
        }
    }
}

/// One id is freed while a reader holds a view of it, under a snapshot pin
/// if `pinned` and under an operation guard if not: no reclaim gives it
/// back and no allocation hands it out before the reader has looked and
/// let go, and a reclaim gives it back after.
fn an_id_freed_under_a_reader(shared: &Shared, pinned: bool) {
    let raw = shared.ids.allocate().expect("an id is free").get();
    assert!(!shared.held[raw as usize].swap(true, Ordering::SeqCst));
    let new = shared.next_page.fetch_add(1, Ordering::SeqCst);
    shared.named[raw as usize].store(new, Ordering::SeqCst);
    shared.ids.publish(page(raw)).expect("it is private");
    shared.committed().insert(raw, new);
    let commit = shared.pins.latest() + 1;
    shared.pins.publish(commit);

    let (holding, release) = (AtomicBool::new(false), AtomicBool::new(false));
    thread::scope(|scope| {
        let _failed = Failed(&shared.failed);
        scope.spawn(|| {
            let _failed = Failed(&shared.failed);
            let (hold, view) = {
                let committed = shared.committed();
                let hold = match pinned {
                    true => shared.pins.pin(),
                    false => shared.pins.guard(),
                };
                (hold, committed.clone())
            };
            assert_eq!(view.get(&raw), Some(&new));
            holding.store(true, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                if shared.failed.load(Ordering::SeqCst) {
                    return;
                }
                thread::yield_now();
            }
            shared.look(&view, &mut Rng::new(1));
            match pinned {
                true => shared.pins.unpin(hold),
                false => shared.pins.unguard(hold),
            }
        });
        while !holding.load(Ordering::SeqCst) {
            assert!(!shared.failed.load(Ordering::SeqCst), "the reader failed");
            thread::yield_now();
        }
        {
            let _committed = shared.committed().remove(&raw);
            shared.give_up(raw);
            let freed_at = version(commit + 1);
            shared.ids.free(page(raw), freed_at).expect("it is live");
            shared.pins.publish(commit + 1);
        }
        for _ in 0..3 {
            let released = shared.reclaim();
            assert!(
                !released.contains(&page(raw)),
                "id {raw} came back under its reader"
            );
        }
        let mut handed = Vec::new();
        while let Ok(id) = shared.ids.allocate() {
            handed.push(id);
            assert!(handed.len() < IDS as usize, "more ids than there are");
        }
        assert!(
            !handed.contains(&page(raw)),
            "id {raw} was handed out under its reader"
        );
        for id in handed {
            shared.ids.release(id).expect("it is private");
        }
        release.store(true, Ordering::SeqCst);
    });
    let came_back: Vec<_> = (0..3).flat_map(|_| shared.reclaim()).collect();
    assert!(
        came_back.contains(&page(raw)),
        "id {raw} did not come back: {came_back:?}"
    );
}

#[test]
fn no_reader_sees_an_id_change_hands_while_threads_allocate_commit_and_reclaim() {
    let shared = Shared {
        ids: PageAllocator::with_limit(page(IDS)),
        pins: Registry::default(),
        committed: Mutex::new(BTreeMap::new()),
        named: (0..=IDS).map(|_| AtomicU64::new(0)).collect(),
        held: (0..=IDS).map(|_| AtomicBool::new(false)).collect(),
        next_page: AtomicU64::new(1),
        reused: AtomicU64::new(0),
        checked: AtomicU64::new(0),
        started: AtomicBool::new(false),
        writing: AtomicU64::new(THREADS),
        failed: AtomicBool::new(false),
    };
    an_id_freed_under_a_reader(&shared, true);
    an_id_freed_under_a_reader(&shared, false);
    thread::scope(|scope| {
        let _failed = Failed(&shared.failed);
        for seed in 1..=THREADS {
            let shared = &shared;
            scope.spawn(move || shared.write(seed));
            scope.spawn(move || shared.read(100 + seed));
        }
        shared.started.store(true, Ordering::SeqCst);
    });

    shared.reclaim();
    let committed = shared.committed();
    let census = shared.ids.census();
    assert_eq!(
        (census.private, census.live, census.retired),
        (0, committed.len() as u64, 0)
    );
    let live: Vec<_> = committed
        .keys()
        .map(|raw| (page(*raw), PageState::Live))
        .collect();
    assert_eq!(in_use(&shared.ids), live);
    let (reused, checked) = shared.progress();
    println!("{reused} ids came back, {checked} looks");
}
