//! The races of taking a pin and of taking a guard (`transactions.md` PN4,
//! PN5, PN6; test T-PN1), each written once and run two ways.
//!
//! - **Under loom**, which runs the code of [`Versions`] under the
//!   interleavings of each model:
//!   `RUSTFLAGS="--cfg loom" cargo test -p lignindb --release --lib snapshot::tests::loom`.
//! - **On real threads**, in every normal test run, each race many times.
//!
//! A model has a committer, a reclaimer and a reader. The committer makes
//! version 2, which supersedes the one page version of version 1. The
//! reclaimer asks the three questions as the page id allocator and the
//! buffer pool do, and reclaims that page version when the answers allow
//! it. The reader then must never read version 1 once it is reclaimed.
//!
//! The same models are run against readers that break a rule, and must
//! then fail: that is what shows that they can.

#[cfg(loom)]
use ::loom as rt;
#[cfg(not(loom))]
use std as rt;

use lignin_store::alloc::Pins;
use rt::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
};

use super::*;

#[cfg(loom)]
fn model(body: impl Fn() + Sync + Send + 'static) {
    bounded_model(None, body);
}

/// A model that loom explores up to `preemptions` preemptions, or without
/// a bound. `LOOM_MAX_PREEMPTIONS` sets another bound for all of them.
#[cfg(loom)]
fn bounded_model(preemptions: Option<usize>, body: impl Fn() + Sync + Send + 'static) {
    let mut builder = ::loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = preemptions;
    }
    builder.max_branches = 100_000;
    builder.check(body);
}

#[cfg(not(loom))]
fn bounded_model(_preemptions: Option<usize>, body: impl Fn() + Sync + Send + 'static) {
    model(body);
}

/// Without loom the race is run on real threads, `LIGNIN_RACE_ROUNDS` times
/// or a few thousand.
#[cfg(not(loom))]
fn model(body: impl Fn() + Sync + Send + 'static) {
    let rounds = std::env::var("LIGNIN_RACE_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(if cfg!(miri) { 20 } else { 3_000 });
    for _ in 0..rounds {
        body();
    }
}

/// A flag that is not set, stored the way every later value is: loom gives
/// a new atomic its first value as a weaker store, which a `SeqCst` load
/// could still return after a later `SeqCst` store.
fn flag() -> AtomicBool {
    let flag = AtomicBool::new(false);
    flag.store(false, Ordering::SeqCst);
    flag
}

fn counter(value: u64) -> AtomicU64 {
    let counter = AtomicU64::new(value);
    counter.store(value, Ordering::SeqCst);
    counter
}

/// A database at version 1 with one page, which commit 2 supersedes.
struct World {
    versions: Versions,
    /// Whether the page version that version 1 sees has been reclaimed.
    reclaimed: AtomicBool,
    /// The version the superseded page version was retired at, once the
    /// commit has installed, or 0.
    retired_at: AtomicU64,
}

impl World {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            versions: Versions::new(version(1)),
            reclaimed: flag(),
            retired_at: counter(0),
        })
    }

    /// Commit 2: install, which retires the old page version, then publish
    /// (CM3 steps 7 and 8).
    fn commit(&self) {
        self.retired_at.store(2, Ordering::SeqCst);
        self.versions.publish(version(2)).expect("the next version");
    }

    /// One reclaim, by the three questions in their order.
    fn reclaim(&self) {
        let oldest_pin = self.versions.oldest_pin();
        let epoch = self.versions.next_epoch();
        let oldest_guard = self.versions.oldest_guard();
        let retired_at = self.retired_at.load(Ordering::SeqCst);
        if retired_at != 0 && retired_at <= oldest_pin.get() && epoch <= oldest_guard {
            self.reclaimed.store(true, Ordering::SeqCst);
        }
    }

    /// A read of the page at the version `at`.
    fn read(&self, at: CommitVersion) {
        if at < version(2) {
            assert!(
                !self.reclaimed.load(Ordering::SeqCst),
                "a reader of version {at} read a reclaimed page version"
            );
        }
    }
}

fn race(reader: impl Fn(&World) + Send + Sync + Copy + 'static) {
    model(move || {
        let world = World::new();
        let threads = [
            thread::spawn({
                let world = world.clone();
                move || world.commit()
            }),
            thread::spawn({
                let world = world.clone();
                move || world.reclaim()
            }),
            thread::spawn({
                let world = world.clone();
                move || reader(&world)
            }),
        ];
        for thread in threads {
            thread.join().expect("the thread ends");
        }
        world.reclaim();
        world.reclaim();
        assert!(world.reclaimed.load(Ordering::SeqCst));
    });
}

#[test]
fn a_reader_that_pins_never_reads_a_reclaimed_page_version() {
    race(|world| {
        let pin = world.versions.pin();
        world.read(pin.version());
        drop(pin);
    });
}

#[test]
fn an_operation_under_a_guard_never_reads_a_reclaimed_page_version() {
    race(|world| {
        let guard = world.versions.guard();
        world.read(guard.latest());
        drop(guard);
    });
}

#[test]
fn a_snapshot_and_its_operation_never_read_a_reclaimed_page_version() {
    race(|world| {
        let pin = world.versions.pin();
        let guard = world.versions.guard();
        world.read(pin.version());
        drop(pin);
        world.read(guard.latest());
        drop(guard);
    });
}

/// Two readers beside the commit: each cell is claimed, announced in and
/// given back while the other reader and the publication are under way.
#[test]
fn two_readers_pin_beside_a_commit() {
    model(|| {
        let world = World::new();
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let world = world.clone();
                thread::spawn(move || {
                    let pin = world.versions.pin();
                    let oldest = world.versions.oldest_pin();
                    assert!(oldest <= pin.version());
                    world.read(pin.version());
                    drop(pin);
                })
            })
            .collect();
        world.commit();
        world.reclaim();
        for reader in readers {
            reader.join().expect("the thread ends");
        }
        assert_eq!(world.versions.oldest_pin(), version(2));
        assert_eq!(world.versions.pins_held(), 0);
    });
}

/// The reader's pin is in a chunk that it links itself, because every cell
/// of the first chunk is claimed. The reclaimer's scan has to see that
/// chunk whenever the pin is one it has to respect.
#[test]
fn a_pin_that_makes_the_table_grow_is_seen_by_the_reclaimer() {
    race(|world| {
        world.versions.fill_first_chunk_of_pins();
        let pin = world.versions.pin();
        world.read(pin.version());
        drop(pin);
    });
}

/// Counts the reclaims that run, and fails if two run at once.
struct Counted {
    inside: AtomicBool,
    runs: AtomicU64,
    /// Whether each caller had asked when the last reclaim began.
    asked: [AtomicBool; 3],
    seen: [AtomicBool; 3],
}

struct CountedHook(Arc<Counted>);

impl Reclaim for CountedHook {
    fn reclaim(&self, _: &Versions) {
        let counted = &self.0;
        assert!(!counted.inside.swap(true, Ordering::SeqCst), "two at once");
        counted.runs.fetch_add(1, Ordering::SeqCst);
        for (seen, asked) in counted.seen.iter().zip(&counted.asked) {
            seen.store(asked.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        counted.inside.store(false, Ordering::SeqCst);
    }
}

/// `callers` threads call for a reclaim at once. One runs at a time, none
/// waits, and with two callers the last one that runs began after both
/// had asked. With three, word can be left for the next call, which the
/// model makes at its end.
fn callers_ask_for_a_reclaim(callers: usize, preemptions: Option<usize>) {
    bounded_model(preemptions, move || {
        let counted = Arc::new(Counted {
            inside: flag(),
            runs: counter(0),
            asked: [flag(), flag(), flag()],
            seen: [flag(), flag(), flag()],
        });
        let versions = Arc::new(Versions::with_reclaim(
            version(1),
            Box::new(CountedHook(counted.clone())),
        ));
        let threads: Vec<_> = (0..callers)
            .map(|caller| {
                let (versions, counted) = (versions.clone(), counted.clone());
                thread::spawn(move || {
                    counted.asked[caller].store(true, Ordering::SeqCst);
                    versions.reclaim();
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("the thread ends");
        }
        let seen_all = |counted: &Counted| {
            counted.seen[..callers]
                .iter()
                .all(|seen| seen.load(Ordering::SeqCst))
        };
        let runs = counted.runs.load(Ordering::SeqCst);
        assert!((1..=callers as u64).contains(&runs), "{runs} reclaims ran");
        if callers > 2 && !seen_all(&counted) {
            versions.reclaim();
        }
        assert!(seen_all(&counted));
    });
}

#[test]
fn a_reclaim_starts_after_each_of_two_calls_that_ask() {
    callers_ask_for_a_reclaim(2, None);
}

/// A call that cannot begin, because one is running, leaves the running
/// one's mark alone: the third caller does not run beside the first. Loom
/// explores this one up to three preemptions; without a bound it does not
/// end in an hour.
#[test]
fn three_calls_never_run_two_reclaims_at_once() {
    callers_ask_for_a_reclaim(3, Some(3));
}

/// What the last reclaim was answered, and whether a reader has said since
/// it began that one is due.
struct Told {
    oldest_pin: AtomicU64,
    woken: AtomicBool,
}

struct ToldHook(Arc<Told>);

/// The flag is swapped, not stored: loom orders two writes of one atomic
/// from two threads only when each reads what the other wrote.
impl Reclaim for ToldHook {
    fn reclaim(&self, pins: &Versions) {
        self.0.woken.swap(false, Ordering::SeqCst);
        let oldest = pins.oldest_pin().get();
        self.0.oldest_pin.store(oldest, Ordering::SeqCst);
    }

    fn wake(&self) {
        self.0.woken.swap(true, Ordering::SeqCst);
    }
}

/// A reader holds back the reclaim that follows a commit, with a pin on
/// the version before it or with an announcement of that version that it
/// then withdraws. Either way it says that a reclaim is due: nothing it
/// held back is left with nobody told.
#[test]
fn a_reader_that_held_back_a_reclaim_says_that_one_is_due() {
    model(|| {
        let told = Arc::new(Told {
            oldest_pin: counter(0),
            woken: flag(),
        });
        let versions = Arc::new(Versions::with_reclaim(
            version(1),
            Box::new(ToldHook(told.clone())),
        ));
        let reader = thread::spawn({
            let versions = versions.clone();
            move || drop(versions.pin())
        });
        versions.publish(version(2)).expect("the next version");
        versions.reclaim();
        reader.join().expect("the thread ends");
        let answered = told.oldest_pin.load(Ordering::SeqCst);
        assert!(
            answered == 2 || told.woken.load(Ordering::SeqCst),
            "the reclaim was answered {answered} and nobody was told"
        );
    });
}

/// The models find a reader that breaks the rules. These run under loom
/// only: on real threads the windows are a few instructions wide.
#[cfg(loom)]
mod broken {
    use super::*;

    /// PN4's forbidden design: the reader reads the latest version, and
    /// relies on it without looking again once its pin is announced. Here
    /// the pin it then takes is on a newer version than the one it reads.
    #[test]
    #[should_panic(expected = "read a reclaimed page version")]
    fn a_reader_that_reads_the_version_before_it_pins_is_found() {
        race(|world| {
            let at = world.versions.latest();
            let pin = world.versions.pin();
            world.read(at);
            drop(pin);
        });
    }

    /// PN5: an operation that reads the latest version before it takes its
    /// guard.
    #[test]
    #[should_panic(expected = "read a reclaimed page version")]
    fn an_operation_that_reads_the_version_before_its_guard_is_found() {
        race(|world| {
            let at = world.versions.latest();
            let guard = world.versions.guard();
            world.read(at);
            drop(guard);
        });
    }

    /// A reader that holds nothing at all.
    #[test]
    #[should_panic(expected = "read a reclaimed page version")]
    fn a_reader_that_holds_nothing_is_found() {
        race(|world| {
            let at = world.versions.latest();
            world.read(at);
        });
    }
}
