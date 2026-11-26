//! What [`Versions`] says and does on one thread, and on real threads that
//! pin, guard, publish and reclaim at once.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
};

use lignin_store::alloc::{Epoch, Pins};

use super::*;
use crate::snapshot::slots::Slots;

fn published(latest: u64) -> Versions {
    let versions = Versions::new(CommitVersion::EMPTY);
    for raw in 1..=latest {
        versions.publish(version(raw)).expect("the next version");
    }
    versions
}

#[test]
fn a_pin_is_on_the_latest_version_and_stays_there() {
    let versions = Versions::new(version(7));
    assert_eq!(versions.latest(), version(7));
    let pin = versions.pin();
    assert_eq!(pin.version(), version(7));

    versions.publish(version(8)).expect("the next version");
    let later = versions.pin();
    assert_eq!((pin.version(), later.version()), (version(7), version(8)));
    assert_eq!(versions.pins_held(), 2);
    drop(pin);
    assert_eq!(versions.pins_held(), 1);
    drop(later);
    assert_eq!(versions.pins_held(), 0);
}

#[test]
fn only_the_next_version_is_published() {
    let versions = published(2);
    for wrong in [0, 1, 2, 4, u64::MAX - 1] {
        let refused = versions.publish(version(wrong));
        assert_eq!(
            refused,
            Err(PublishError::NotNext {
                latest: version(2),
                version: version(wrong)
            })
        );
        assert_eq!(versions.latest(), version(2));
    }
    assert_eq!(versions.publish(version(3)), Ok(()));
    assert_eq!(versions.latest(), version(3));

    let last = Versions::new(CommitVersion::MAX);
    assert!(last.publish(CommitVersion::MAX).is_err());
    assert!(last.publish(CommitVersion::EMPTY).is_err());
}

#[test]
fn the_errors_say_what_was_refused() {
    let refused = PublishError::NotNext {
        latest: version(2),
        version: version(5),
    };
    assert_eq!(
        refused.to_string(),
        "version 5 cannot be published: the latest version is 2"
    );
    assert_eq!(
        SnapshotError::ReadOnly.to_string(),
        "a snapshot is read-only"
    );
    let errors: [&dyn std::error::Error; 2] = [&refused, &SnapshotError::ReadOnly];
    assert!(errors.iter().all(|error| error.source().is_none()));
}

#[test]
fn the_oldest_pin_is_the_lowest_pinned_version_or_the_latest() {
    let versions = published(1);
    assert_eq!(versions.oldest_pin(), version(1));
    let first = versions.pin();
    versions.publish(version(2)).expect("the next version");
    let second = versions.pin();
    versions.publish(version(3)).expect("the next version");
    let third = versions.pin();
    versions.publish(version(4)).expect("the next version");
    assert_eq!(versions.oldest_pin(), version(1));

    drop(second);
    assert_eq!(versions.oldest_pin(), version(1));
    drop(first);
    assert_eq!(versions.oldest_pin(), version(3));
    drop(third);
    assert_eq!(versions.oldest_pin(), version(4));
}

#[test]
fn a_guard_is_of_the_epoch_it_was_taken_in() {
    let versions = published(3);
    assert_eq!(versions.epoch(), Epoch::new(0));
    assert_eq!(versions.oldest_guard(), Epoch::new(0));

    let early = versions.guard();
    assert_eq!((early.epoch(), early.latest()), (Epoch::new(0), version(3)));
    assert_eq!(versions.next_epoch(), Epoch::new(1));
    let late = versions.guard();
    assert_eq!(late.epoch(), Epoch::new(1));
    assert_eq!(versions.next_epoch(), Epoch::new(2));
    assert_eq!(versions.epoch(), Epoch::new(2));
    assert_eq!(versions.guards_held(), 2);

    assert_eq!(versions.oldest_guard(), Epoch::new(0));
    drop(early);
    assert_eq!(versions.oldest_guard(), Epoch::new(1));
    drop(late);
    assert_eq!(versions.oldest_guard(), Epoch::new(2));
    assert_eq!(versions.guards_held(), 0);

    versions.publish(version(4)).expect("the next version");
    assert_eq!(versions.guard().latest(), version(4));
}

#[test]
fn the_handles_print_what_they_hold() {
    let versions = published(2);
    let pin = versions.pin();
    let guard = versions.guard();
    assert_eq!(format!("{pin:?}"), "VersionPin(v2)");
    assert_eq!(format!("{guard:?}"), "OperationGuard(Epoch(0))");
    assert_eq!(
        format!("{versions:?}"),
        "Versions { latest: v2, oldest_pin: v2, pins: 1, epoch: Epoch(0), guards: 1, .. }"
    );
}

#[test]
fn the_handles_go_to_other_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<Versions>();
    shared::<VersionPin<'_>>();
    shared::<OperationGuard<'_>>();
    shared::<Snapshot<'_>>();
    shared::<Operation<'_>>();

    let versions = published(1);
    let pin = versions.pin();
    versions.publish(version(2)).expect("the next version");
    thread::scope(|scope| {
        scope.spawn(|| {
            assert_eq!(pin.version(), version(1));
            drop(pin);
        });
    });
    assert_eq!(versions.oldest_pin(), version(2));
}

#[test]
fn the_table_grows_when_every_cell_is_claimed_and_cells_are_used_again() {
    let slots = Slots::new();
    let first = slots.capacity();
    assert_eq!((slots.min(), slots.claimed()), (u64::MAX, 0));

    let mut held: Vec<_> = (0..first as u64).map(|at| slots.claim(1000 + at)).collect();
    assert_eq!((slots.capacity(), slots.claimed()), (first, first));
    held.push(slots.claim(7));
    assert_eq!((slots.capacity(), slots.claimed()), (3 * first, first + 1));
    held.extend((0..2 * first as u64).map(|at| slots.claim(2000 + at)));
    assert_eq!(slots.capacity(), 7 * first);
    assert_eq!((slots.min(), slots.claimed()), (7, 3 * first + 1));

    held[first].announce(9);
    assert_eq!(slots.min(), 9);
    held[first].vacate();
    assert_eq!((slots.min(), slots.claimed()), (1000, 3 * first));
    for slot in &held[..first] {
        slot.vacate();
    }
    assert_eq!((slots.min(), slots.claimed()), (2000, 2 * first));

    let again: Vec<_> = (0..first as u64 + 1)
        .map(|at| slots.claim(1 + at))
        .collect();
    assert_eq!(slots.capacity(), 7 * first);
    assert_eq!((slots.min(), slots.claimed()), (1, 3 * first + 1));
    drop((held, again));
}

/// Each of the first threads of a process starts its look for a vacant
/// cell at a cell of its own: at least 8 cells from the next for 32
/// threads, 4 for 33 to 64, and one for as many threads as the first
/// chunk has cells.
#[test]
fn the_first_threads_start_at_cells_of_their_own() {
    use crate::snapshot::slots::{home_of, FIRST_CHUNK};

    assert_eq!(FIRST_CHUNK, 256);
    for (threads, apart) in [
        (1, 256),
        (2, 128),
        (32, 8),
        (48, 4),
        (64, 4),
        (200, 1),
        (256, 1),
    ] {
        let mut homes: Vec<usize> = (0..threads).map(home_of).collect();
        homes.sort_unstable();
        assert!(homes.iter().all(|home| *home < FIRST_CHUNK));
        let nearest = (homes.windows(2).map(|pair| pair[1] - pair[0]))
            .chain([FIRST_CHUNK - homes[threads - 1] + homes[0]])
            .min();
        assert_eq!(nearest, Some(apart), "{threads} threads");
    }
    // The thread after those starts where the first did.
    assert_eq!((home_of(0), home_of(FIRST_CHUNK)), (0, 0));
    assert_eq!(home_of(FIRST_CHUNK + 1), home_of(1));
}

#[test]
fn any_number_of_pins_and_guards_are_held_at_once() {
    let versions = published(1);
    let mut pins = Vec::new();
    let mut guards = Vec::new();
    for raw in 2..=700 {
        pins.push(versions.pin());
        guards.push(versions.guard());
        versions.publish(version(raw)).expect("the next version");
        versions.next_epoch();
    }
    assert_eq!((versions.pins_held(), versions.guards_held()), (699, 699));
    for (at, (pin, guard)) in pins.iter().zip(&guards).enumerate() {
        assert_eq!(pin.version(), version(1 + at as u64));
        assert_eq!(guard.epoch(), Epoch::new(at as u64));
    }
    for at in 0..699 {
        assert_eq!(versions.oldest_pin(), version(1 + at));
        assert_eq!(versions.oldest_guard(), Epoch::new(at));
        pins.remove(0);
        guards.remove(0);
    }
    assert_eq!(versions.oldest_pin(), version(700));
}

/// Counts the reclaims it runs, with what the oldest pin was at each, and
/// the times it is told that one is due.
#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
    wakes: AtomicUsize,
    oldest: Mutex<Vec<u64>>,
}

impl Reclaim for Arc<Counting> {
    fn reclaim(&self, pins: &Versions) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let oldest = pins.oldest_pin().get();
        self.oldest.lock().expect("no panic").push(oldest);
    }

    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_pin_before_the_latest_says_that_a_reclaim_is_due_and_runs_none() {
    let counting = Arc::new(Counting::default());
    let versions = Versions::with_reclaim(version(1), Box::new(counting.clone()));
    let counts = || {
        (
            counting.calls.load(Ordering::SeqCst),
            counting.wakes.load(Ordering::SeqCst),
        )
    };

    // A pin on the latest version holds nothing back, and neither does a
    // guard that goes.
    drop(versions.pin());
    drop(versions.guard());
    assert_eq!(counts(), (0, 0));

    // The committer reclaims after it has published.
    let old = versions.pin();
    let older_still = versions.pin();
    versions.publish(version(2)).expect("the next version");
    assert_eq!(counts(), (0, 0));
    versions.reclaim();
    assert_eq!(counts(), (1, 0));

    // A pin before the latest version says so when it goes. The reader
    // runs no reclaim: the one that is run later finds the pin gone.
    drop(old);
    assert_eq!(counts(), (1, 1));
    drop(older_still);
    assert_eq!(counts(), (1, 2));
    versions.reclaim();
    assert_eq!(counts(), (2, 2));
    assert_eq!(*counting.oldest.lock().expect("no panic"), [1, 2]);

    // With nobody to ask or tell, nothing happens.
    let alone = published(1);
    let pin = alone.pin();
    alone.publish(version(2)).expect("the next version");
    drop(pin);
    alone.reclaim();
}

/// A reclaimer that does not act on being told.
struct Deaf;

impl Reclaim for Deaf {
    fn reclaim(&self, _: &Versions) {}
}

#[test]
fn a_reclaim_need_not_act_on_being_told() {
    let versions = Versions::with_reclaim(version(1), Box::new(Deaf));
    let pin = versions.pin();
    versions.publish(version(2)).expect("the next version");
    drop(pin);
    versions.reclaim();
    assert_eq!(versions.oldest_pin(), version(2));
}

/// Asks for another reclaim from inside the first one it runs.
struct AsksAgain(Arc<AtomicUsize>);

impl Reclaim for AsksAgain {
    fn reclaim(&self, pins: &Versions) {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            pins.reclaim();
        }
    }
}

#[test]
fn a_reclaim_that_is_asked_for_while_one_runs_follows_it() {
    let calls = Arc::new(AtomicUsize::new(0));
    let versions = Versions::with_reclaim(version(1), Box::new(AsksAgain(calls.clone())));
    versions.reclaim();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    versions.reclaim();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// Stops inside its first reclaim until it is let go, and fails if two
/// run at once.
struct Stops {
    calls: AtomicUsize,
    inside_now: AtomicBool,
    inside: Barrier,
    go_on: Barrier,
}

impl Reclaim for Arc<Stops> {
    fn reclaim(&self, _: &Versions) {
        assert!(!self.inside_now.swap(true, Ordering::SeqCst), "two at once");
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.inside.wait();
            self.go_on.wait();
        }
        self.inside_now.store(false, Ordering::SeqCst);
    }
}

/// While one reclaim runs, a second and a third call return at once and
/// start none: a call that could not begin leaves the mark of the one
/// that is running as it is.
#[test]
fn calls_that_find_a_reclaim_running_leave_word_and_do_not_run_beside_it() {
    let stops = Arc::new(Stops {
        calls: AtomicUsize::new(0),
        inside_now: AtomicBool::new(false),
        inside: Barrier::new(2),
        go_on: Barrier::new(2),
    });
    let versions = Versions::with_reclaim(version(1), Box::new(stops.clone()));
    thread::scope(|scope| {
        scope.spawn(|| versions.reclaim());
        stops.inside.wait();
        for _ in 0..3 {
            versions.reclaim();
            assert_eq!(stops.calls.load(Ordering::SeqCst), 1);
        }
        thread::scope(|others| {
            for _ in 0..3 {
                others.spawn(|| versions.reclaim());
            }
        });
        assert_eq!(stops.calls.load(Ordering::SeqCst), 1);
        stops.go_on.wait();
    });
    // The thread that ran the first one ran one more, for all of them.
    assert_eq!(stops.calls.load(Ordering::SeqCst), 2);
}

/// PN4 and PN6 on real threads. A writer publishes, readers pin and take
/// guards, and reclaimers ask the three questions. What a reclaimer was
/// told stays true: no reader ever holds a pin below an oldest pin that
/// was answered before the pin was given out, or a guard of an epoch
/// before an oldest guard that was.
#[test]
fn no_answer_is_overtaken_by_a_pin_or_a_guard_on_real_threads() {
    const READERS: usize = 6;
    const COMMITS: u64 = if cfg!(miri) { 40 } else { 40_000 };

    let versions = published(1);
    let answered_pin = AtomicU64::new(0);
    let answered_guard = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    let reads = AtomicU64::new(0);

    thread::scope(|scope| {
        for reader in 0..READERS {
            let (versions, done, reads) = (&versions, &done, &reads);
            let (answered_pin, answered_guard) = (&answered_pin, &answered_guard);
            scope.spawn(move || {
                let mut round = 0u64;
                while !done.load(Ordering::Relaxed) {
                    round += 1;
                    let pin = versions.pin();
                    let guard = versions.guard();
                    let at = guard.latest();
                    for _ in 0..(round + reader as u64) % 5 {
                        assert!(pin.version().get() >= answered_pin.load(Ordering::SeqCst));
                        assert!(guard.epoch().get() >= answered_guard.load(Ordering::SeqCst));
                        assert!(at >= pin.version());
                        thread::yield_now();
                    }
                    assert!(pin.version().get() >= answered_pin.load(Ordering::SeqCst));
                    assert!(guard.epoch().get() >= answered_guard.load(Ordering::SeqCst));
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        for _ in 0..2 {
            let (versions, done) = (&versions, &done);
            let (answered_pin, answered_guard) = (&answered_pin, &answered_guard);
            scope.spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let oldest = versions.oldest_pin();
                    assert!(oldest <= versions.latest());
                    answered_pin.fetch_max(oldest.get(), Ordering::SeqCst);
                    let epoch = versions.next_epoch();
                    let guard = versions.oldest_guard();
                    assert!(guard <= versions.epoch());
                    answered_guard.fetch_max(guard.get().min(epoch.get()), Ordering::SeqCst);
                }
            });
        }
        for raw in 2..=COMMITS {
            versions.publish(version(raw)).expect("the next version");
        }
        // The readers have read, and the reclaimers have been answered
        // with more than the first version and the first epoch.
        while reads.load(Ordering::Relaxed) < COMMITS / 10
            || answered_pin.load(Ordering::SeqCst) <= 1
            || answered_guard.load(Ordering::SeqCst) <= 1
        {
            thread::yield_now();
        }
        done.store(true, Ordering::Relaxed);
    });

    assert_eq!((versions.pins_held(), versions.guards_held()), (0, 0));
    assert_eq!(versions.oldest_pin(), version(COMMITS));
}

/// Asks for another reclaim from inside every one it runs.
struct AlwaysAsksAgain(Arc<AtomicUsize>);

impl Reclaim for AlwaysAsksAgain {
    fn reclaim(&self, pins: &Versions) {
        self.0.fetch_add(1, Ordering::SeqCst);
        pins.reclaim();
    }
}

#[test]
fn a_thread_runs_two_reclaims_at_most_and_the_next_call_runs_what_was_left() {
    let calls = Arc::new(AtomicUsize::new(0));
    let versions = Versions::with_reclaim(version(1), Box::new(AlwaysAsksAgain(calls.clone())));
    versions.reclaim();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // Word was left during the second one. The next call runs it.
    versions.reclaim();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// Panics in its first reclaim.
struct Panics(Arc<AtomicUsize>);

impl Reclaim for Panics {
    fn reclaim(&self, _: &Versions) {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("the first reclaim fails");
        }
    }
}

#[test]
fn a_reclaim_that_panics_does_not_stop_the_next_one() {
    let calls = Arc::new(AtomicUsize::new(0));
    let versions = Versions::with_reclaim(version(1), Box::new(Panics(calls.clone())));
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| versions.reclaim()));
    assert!(failed.is_err());
    versions.reclaim();
    versions.reclaim();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn the_clock_of_retirements_stops_before_the_number_of_an_unclaimed_cell() {
    let versions = published(1);
    versions.set_epoch(u64::MAX - 3);
    assert_eq!(versions.next_epoch(), Epoch::new(u64::MAX - 2));
    assert_eq!(versions.next_epoch(), Epoch::new(u64::MAX - 1));
    let guard = versions.guard();
    assert_eq!(guard.epoch(), Epoch::new(u64::MAX - 1));
    assert_eq!(versions.oldest_guard(), Epoch::new(u64::MAX - 1));
    assert_eq!(versions.guards_held(), 1);

    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| versions.next_epoch()));
    assert!(stopped.is_err());
    assert_eq!(versions.epoch(), Epoch::new(u64::MAX - 1));
    drop(guard);
    assert_eq!(versions.guards_held(), 0);
}

#[test]
fn a_pin_in_a_chunk_that_was_linked_later_is_in_the_answer() {
    let versions = published(3);
    versions.fill_first_chunk_of_pins();
    assert_eq!(versions.oldest_pin(), version(3));
    let pin = versions.pin();
    versions.publish(version(4)).expect("the next version");
    assert_eq!(versions.oldest_pin(), version(3));
    drop(pin);
    assert_eq!(versions.oldest_pin(), version(4));
}
