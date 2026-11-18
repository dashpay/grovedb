//! The allocator under threads: writers that allocate, commit and roll
//! back, readers on snapshots and in operations, and reclaims, all at once.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use super::*;

const IDS: u32 = 48;
const THREADS: u64 = 4;
/// What the threads go on for: ids that came back, and looks at an id
/// through a view.
const ENOUGH: (u64, u64) = (40_000, 400_000);
/// How long the writers go on if that never comes.
const AT_MOST: Duration = Duration::from_secs(120);

/// Counts a writer out when it ends, also when it panics.
struct Leaving<'a>(&'a AtomicU64);

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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
    start: Barrier,
    writing: AtomicU64,
}

impl Shared {
    fn committed(&self) -> std::sync::MutexGuard<'_, BTreeMap<u32, u64>> {
        self.committed.lock().expect("no thread panicked")
    }

    fn give_up(&self, raw: u32) {
        assert!(self.held[raw as usize].swap(false, Ordering::SeqCst));
    }

    fn reclaim(&self) {
        let mut released = Vec::new();
        self.ids.reclaim(&self.pins, &mut released);
        self.reused
            .fetch_add(released.len() as u64, Ordering::Relaxed);
    }

    fn progress(&self) -> (u64, u64) {
        (
            self.reused.load(Ordering::Relaxed),
            self.checked.load(Ordering::Relaxed),
        )
    }

    fn write(&self, seed: u64) {
        let _leaving = Leaving(&self.writing);
        self.start.wait();
        let mut rng = Rng::new(seed);
        let started = Instant::now();
        while started.elapsed() < AT_MOST {
            let (reused, checked) = self.progress();
            if reused >= ENOUGH.0 && checked >= ENOUGH.1 {
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
        self.start.wait();
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
        start: Barrier::new(2 * THREADS as usize),
        writing: AtomicU64::new(THREADS),
    };
    thread::scope(|scope| {
        for seed in 1..=THREADS {
            let shared = &shared;
            scope.spawn(move || shared.write(seed));
            scope.spawn(move || shared.read(100 + seed));
        }
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
    assert!(
        reused >= ENOUGH.0 && checked >= ENOUGH.1,
        "{reused} ids came back, {checked} looks"
    );
}
