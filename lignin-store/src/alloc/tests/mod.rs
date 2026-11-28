mod allocator;
mod model_check;
mod recovery;
mod states;
mod threads;

use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use lignin_format::{CommitVersion, PageId};

use super::*;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn pages(raw: impl IntoIterator<Item = u32>) -> Vec<PageId> {
    raw.into_iter().map(page).collect()
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

#[derive(Default)]
struct Held {
    latest: u64,
    pins: BTreeMap<u64, usize>,
    clock: u64,
    guards: BTreeMap<u64, usize>,
}

fn hold(held: &mut BTreeMap<u64, usize>, at: u64) {
    *held.entry(at).or_default() += 1;
}

fn drop_hold(held: &mut BTreeMap<u64, usize>, at: u64) {
    let count = held.get_mut(&at).expect("it is held");
    *count -= 1;
    if *count == 0 {
        held.remove(&at);
    }
}

/// A keeper of version pins and operation guards that does what [`Pins`]
/// asks, written from `transactions.md` PN1 to PN6: each of its steps is
/// one step under a lock.
#[derive(Default)]
struct Registry {
    held: Mutex<Held>,
}

impl Registry {
    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn latest(&self) -> u64 {
        self.held().latest
    }

    /// Publishes the commit of `latest`.
    fn publish(&self, latest: u64) {
        self.held().latest = latest;
    }

    /// Pins the latest version and returns it.
    fn pin(&self) -> u64 {
        let mut held = self.held();
        let latest = held.latest;
        hold(&mut held.pins, latest);
        latest
    }

    fn unpin(&self, pinned: u64) {
        drop_hold(&mut self.held().pins, pinned);
    }

    /// Takes an operation guard and returns its epoch.
    fn guard(&self) -> u64 {
        let mut held = self.held();
        let epoch = held.clock;
        hold(&mut held.guards, epoch);
        epoch
    }

    fn unguard(&self, epoch: u64) {
        drop_hold(&mut self.held().guards, epoch);
    }
}

impl Pins for Registry {
    fn oldest_pin(&self) -> CommitVersion {
        let held = self.held();
        version(held.pins.keys().next().copied().unwrap_or(held.latest))
    }

    fn next_epoch(&self) -> Epoch {
        let mut held = self.held();
        held.clock += 1;
        Epoch::new(held.clock)
    }

    fn oldest_guard(&self) -> Epoch {
        let held = self.held();
        Epoch::new(held.guards.keys().next().copied().unwrap_or(held.clock))
    }
}

/// Runs one reclaim and returns the ids it freed, in ascending order.
fn reclaim(ids: &PageAllocator, pins: &dyn Pins) -> Vec<PageId> {
    let mut released = Vec::new();
    ids.reclaim(pins, &mut released);
    released.sort();
    released
}

fn retired(ids: &PageAllocator) -> Vec<(PageId, CommitVersion)> {
    let mut retired = Vec::new();
    ids.retired(&mut retired);
    retired.sort();
    retired
}

/// Every id that is not free, with its state.
fn in_use(ids: &PageAllocator) -> Vec<(PageId, PageState)> {
    let mut found = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        next = ids.scan(from, &mut found);
    }
    found
}
