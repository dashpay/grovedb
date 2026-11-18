//! The three questions, checked against the page id allocator
//! (`transactions.md` T-PN6).
//!
//! The acceptance test of the allocator (`page-alloc.md` section 6) runs it
//! against a model of everything it does not own. Here the same world is
//! built with [`Versions`] in the place of that model's pins and guards:
//! the pins are [`VersionPin`]s, the operations take [`OperationGuard`]s
//! and read the latest version through them, a commit is published with
//! [`Versions::publish`], and a reclaim puts its three questions to the
//! [`Versions`] one step at a time, so that every other actor can move
//! between them. The allocator is the real one. The model keeps what
//! neither is told: which page each id named from which commit to which,
//! and which reader can reach it.
//!
//! [`explore`] visits every state that the actors can reach within the
//! bounds and checks each: no id is free while a reader, the latest
//! version or a commit that is installed and not yet published can reach
//! a page it named. Two states are the same when they differ only in the
//! numbers of their epochs, not the order of those.
//!
//! Neither the allocator nor the versions can be copied, so a state is
//! reached by taking its steps again from the start.
//!
//! With one rule broken at a time the same exploration must find a free id
//! that a reader can reach ([`Fault`]).

use std::collections::{BTreeMap, HashSet};

use lignin_format::PageId;
use lignin_store::alloc::{Epoch, PageAllocator, PageState, Pins};

use super::*;

/// One rule of what a reclaimer is promised, broken on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// The oldest pin is answered one version too high.
    PinOffByOne,
    /// A commit that is installed and not yet published is taken for the
    /// latest version: the versions are told of it at its install.
    InstalledTakenForLatest,
    /// An operation reads the latest version before it takes its guard.
    GuardAfterRead,
    /// A reclaim starts its epoch before it asks for the oldest pin.
    EpochBeforePin,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    ids: u32,
    /// The commits a run can make.
    versions: u64,
    txns: usize,
    pins: usize,
    ops: usize,
    reclaimers: usize,
}

/// What an id named from one commit to another.
#[derive(Clone, Copy, Debug)]
struct Page {
    id: u32,
    born: u64,
    /// The version that freed the page: no handle reaches it from there on.
    gone: Option<u64>,
}

impl Page {
    fn reachable_at(&self, version: u64) -> bool {
        self.born <= version && self.gone.is_none_or(|gone| version < gone)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Txn {
    /// The id it holds, for the page it creates.
    private: Option<u32>,
    /// The page its commit frees, by id and born version.
    mark: Option<(u32, u64)>,
}

enum Op<'v> {
    Idle,
    Guarded(OperationGuard<'v>),
    /// The latest version was read with no guard held.
    Unguarded(u64),
    Reading(OperationGuard<'v>, u64),
}

/// The answers a reclaim has so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Answers {
    oldest_pin: Option<u64>,
    epoch: Option<u64>,
    oldest_guard: Option<u64>,
}

/// Hands the allocator the answers a reclaim took in its earlier steps.
struct Asked(Answers);

impl Pins for Asked {
    fn oldest_pin(&self) -> CommitVersion {
        version(self.0.oldest_pin.expect("the answer was taken"))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.0.epoch.expect("the answer was taken"))
    }

    fn oldest_guard(&self) -> Epoch {
        Epoch::new(self.0.oldest_guard.expect("the answer was taken"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Allocate(usize),
    RollBack(usize),
    Mark(usize, (u32, u64)),
    Install(usize),
    Publish,
    Pin,
    Unpin(u64),
    Guard(usize),
    Read(usize),
    End(usize),
    Reclaim(usize),
}

#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    states: u64,
    transitions: u64,
    deepest: usize,
    /// Allocations of an id that named a page before.
    reused: u64,
    /// Those of them made while a reader was open that cannot reach the
    /// page.
    reused_beside_a_reader: u64,
    /// Reclaims that left an id of a published commit waiting for a pin.
    held_for_a_pin: u64,
    /// Reclaims that left an id waiting for an operation guard.
    held_for_a_guard: u64,
    released: u64,
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

struct World<'v> {
    bounds: Bounds,
    fault: Fault,
    versions: &'v Versions,
    ids: PageAllocator,
    /// The latest published version, as the model has it.
    latest: u64,
    /// The version of a commit that is installed and not yet published.
    installing: Option<u64>,
    pages: Vec<Page>,
    txns: Vec<Txn>,
    pins: Vec<VersionPin<'v>>,
    ops: Vec<Op<'v>>,
    reclaimers: Vec<Answers>,
    /// The epoch each retired id was stamped with, once a reclaim found
    /// that no pin can reach it (`page-alloc.md` A9).
    stamps: BTreeMap<u32, u64>,
}

impl<'v> World<'v> {
    fn new(versions: &'v Versions, bounds: Bounds, fault: Fault) -> Self {
        Self {
            bounds,
            fault,
            versions,
            ids: PageAllocator::new(),
            latest: 0,
            installing: None,
            pages: Vec::new(),
            txns: vec![Txn::default(); bounds.txns],
            pins: Vec::new(),
            ops: (0..bounds.ops).map(|_| Op::Idle).collect(),
            reclaimers: vec![Answers::default(); bounds.reclaimers],
            stamps: BTreeMap::new(),
        }
    }

    fn is_free(&self, id: u32) -> bool {
        self.ids.state(page(id)) == PageState::Free
    }

    /// The versions that open readers read: every version pin, and every
    /// operation that has read the latest version.
    fn readers(&self) -> Vec<u64> {
        let pins = self.pins.iter().map(|pin| pin.version().get());
        let reading = self.ops.iter().filter_map(|op| match op {
            Op::Reading(_, version) | Op::Unguarded(version) => Some(*version),
            _ => None,
        });
        pins.chain(reading).collect()
    }

    /// Every step that some actor can take now.
    fn steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        let a_free_id = (1..=self.bounds.ids).any(|id| self.is_free(id));
        for (at, txn) in self.txns.iter().enumerate() {
            if txn.private.is_none() && a_free_id {
                steps.push(Step::Allocate(at));
            }
            if txn.private.is_some() || txn.mark.is_some() {
                steps.push(Step::RollBack(at));
                if self.installing.is_none() && self.latest < self.bounds.versions {
                    steps.push(Step::Install(at));
                }
            }
            if txn.mark.is_none() {
                let visible = self
                    .pages
                    .iter()
                    .filter(|page| page.gone.is_none() && page.born <= self.latest);
                steps.extend(visible.map(|page| Step::Mark(at, (page.id, page.born))));
            }
        }
        if self.installing.is_some() {
            steps.push(Step::Publish);
        }
        if self.pins.len() < self.bounds.pins {
            steps.push(Step::Pin);
        }
        let mut pinned: Vec<u64> = self.pins.iter().map(|pin| pin.version().get()).collect();
        pinned.sort();
        pinned.dedup();
        steps.extend(pinned.into_iter().map(Step::Unpin));
        for (at, op) in self.ops.iter().enumerate() {
            match op {
                Op::Idle => steps.push(Step::Guard(at)),
                Op::Guarded(_) | Op::Unguarded(_) => steps.extend([Step::Read(at), Step::End(at)]),
                Op::Reading(..) => steps.push(Step::End(at)),
            }
        }
        steps.extend((0..self.reclaimers.len()).map(Step::Reclaim));
        steps
    }

    fn apply(&mut self, step: Step, stats: &mut Stats) -> Result<(), String> {
        match step {
            Step::Allocate(at) => self.allocate(at, stats)?,
            Step::RollBack(at) => self.roll_back(at),
            Step::Mark(at, key) => self.txns[at].mark = Some(key),
            Step::Install(at) => self.install(at),
            Step::Publish => {
                let commit = self.installing.take().expect("the step is enabled");
                self.latest = commit;
                if self.fault != Fault::InstalledTakenForLatest {
                    self.versions.publish(version(commit)).expect("the next");
                }
            }
            Step::Pin => {
                let pin = self.versions.pin();
                let told_early = self.fault == Fault::InstalledTakenForLatest;
                if pin.version().get() != self.latest && !told_early {
                    return Err(format!(
                        "a pin is on version {} and the latest is {}",
                        pin.version(),
                        self.latest
                    ));
                }
                self.pins.push(pin);
            }
            Step::Unpin(pinned) => {
                let at = self
                    .pins
                    .iter()
                    .position(|pin| pin.version().get() == pinned);
                self.pins.remove(at.expect("the step is enabled"));
            }
            Step::Guard(at) => {
                self.ops[at] = if self.fault == Fault::GuardAfterRead {
                    Op::Unguarded(self.versions.latest().get())
                } else {
                    Op::Guarded(self.versions.guard())
                };
            }
            Step::Read(at) => {
                self.ops[at] = match std::mem::replace(&mut self.ops[at], Op::Idle) {
                    Op::Guarded(guard) => {
                        let read = guard.latest().get();
                        Op::Reading(guard, read)
                    }
                    Op::Unguarded(read) => Op::Reading(self.versions.guard(), read),
                    op => op,
                };
            }
            Step::End(at) => self.ops[at] = Op::Idle,
            Step::Reclaim(at) => self.reclaim(at, stats)?,
        }
        self.check()
    }

    fn roll_back(&mut self, at: usize) {
        if let Some(id) = std::mem::take(&mut self.txns[at]).private {
            self.ids.release(page(id)).expect("it is private");
        }
    }

    fn allocate(&mut self, at: usize, stats: &mut Stats) -> Result<(), String> {
        let got = self.ids.allocate().expect("an id is free").get();
        if got > self.bounds.ids {
            return Err(format!("allocate gave id {got} with a lower id free"));
        }
        if self.pages.iter().any(|page| page.id == got) {
            stats.reused += 1;
            stats.reused_beside_a_reader += u64::from(!self.readers().is_empty());
        }
        self.pages.retain(|page| page.id != got);
        self.txns[at].private = Some(got);
        Ok(())
    }

    /// Step 7 of a commit: its page is installed, and the id of the page it
    /// frees is retired with its version. A transaction that marked a page
    /// which another commit has freed since is rolled back, as in the
    /// allocator's own model: in the engine a commit is built over the
    /// latest version by then.
    fn install(&mut self, at: usize) {
        let mark = self.txns[at].mark;
        let live = |(id, born): (u32, u64)| {
            self.pages
                .iter()
                .any(|page| page.id == id && page.born == born && page.gone.is_none())
        };
        if mark.is_some_and(|mark| !live(mark)) {
            return self.roll_back(at);
        }
        let txn = std::mem::take(&mut self.txns[at]);
        let commit = self.latest + 1;
        if let Some(id) = txn.private {
            self.ids.publish(page(id)).expect("it is private");
            self.pages.push(Page {
                id,
                born: commit,
                gone: None,
            });
        }
        if let Some((id, born)) = txn.mark {
            let freed = self
                .pages
                .iter_mut()
                .find(|page| page.id == id && page.born == born)
                .expect("it is live");
            freed.gone = Some(commit);
            self.ids
                .free(page(id), version(commit))
                .expect("it is live");
        }
        self.installing = Some(commit);
        if self.fault == Fault::InstalledTakenForLatest {
            self.versions.publish(version(commit)).expect("the next");
        }
    }

    /// The next step of a reclaim: one of its three questions, put to the
    /// versions, or its step under the allocator's lock.
    fn reclaim(&mut self, at: usize, stats: &mut Stats) -> Result<(), String> {
        let mut answers = self.reclaimers[at];
        let pin_first = self.fault != Fault::EpochBeforePin;
        if answers.oldest_pin.is_none() && (pin_first || answers.epoch.is_some()) {
            let oldest = self.versions.oldest_pin().get();
            answers.oldest_pin = Some(oldest + u64::from(self.fault == Fault::PinOffByOne));
        } else if answers.epoch.is_none() {
            answers.epoch = Some(self.versions.next_epoch().get());
        } else if answers.oldest_guard.is_none() {
            answers.oldest_guard = Some(self.versions.oldest_guard().get());
        } else {
            let (oldest_pin, epoch, oldest_guard) = (
                answers.oldest_pin.expect("taken"),
                answers.epoch.expect("taken"),
                answers.oldest_guard.expect("taken"),
            );
            let mut released = Vec::new();
            self.ids.reclaim(&Asked(answers), &mut released);
            let mut released: Vec<u32> = released.iter().map(|page| page.get()).collect();
            released.sort();

            let mut retired = Vec::new();
            self.ids.retired(&mut retired);
            let mut expected = Vec::new();
            for (id, freed_at) in retired
                .iter()
                .map(|(page, freed_at)| (page.get(), freed_at.get()))
                .chain(released.iter().map(|id| (*id, 0)))
            {
                if freed_at <= oldest_pin {
                    self.stamps.entry(id).or_insert(epoch);
                }
                if self
                    .stamps
                    .get(&id)
                    .is_some_and(|stamp| *stamp <= oldest_guard)
                {
                    self.stamps.remove(&id);
                    expected.push(id);
                }
            }
            expected.sort();
            if released != expected {
                return Err(format!(
                    "a reclaim released {released:?}, and by its answers {expected:?} are due"
                ));
            }
            let waits_for_a_pin = retired.iter().any(|(page, freed_at)| {
                freed_at.get() <= self.latest && !self.stamps.contains_key(&page.get())
            });
            stats.held_for_a_pin += u64::from(waits_for_a_pin);
            stats.held_for_a_guard += u64::from(!self.stamps.is_empty());
            stats.released += released.len() as u64;
            answers = Answers::default();
        }
        self.reclaimers[at] = answers;
        Ok(())
    }

    /// What must hold in every state: no id is free while anything can
    /// reach a page it named.
    fn check(&self) -> Result<(), String> {
        let readers = self.readers();
        for id in (1..=self.bounds.ids).filter(|id| self.is_free(*id)) {
            let every = [self.latest].into_iter().chain(self.installing);
            for reader in every.chain(readers.iter().copied()) {
                let reached = self
                    .pages
                    .iter()
                    .find(|page| page.id == id && page.reachable_at(reader));
                if let Some(old) = reached {
                    return Err(format!(
                        "id {id} is free while a reader of version {reader} can reach the page \
                         it named from version {} to {:?}",
                        old.born, old.gone
                    ));
                }
            }
        }
        Ok(())
    }

    /// What tells this state from every state with another future.
    fn key(&self) -> Vec<u64> {
        let mut epochs: Vec<u64> = vec![self.versions.epoch().get()];
        for op in &self.ops {
            if let Op::Guarded(guard) | Op::Reading(guard, _) = op {
                epochs.push(guard.epoch().get());
            }
        }
        for answers in &self.reclaimers {
            epochs.extend(answers.epoch);
            epochs.extend(answers.oldest_guard);
        }
        epochs.extend(self.stamps.values());
        epochs.sort();
        epochs.dedup();
        let rank = |epoch: u64| epochs.binary_search(&epoch).expect("collected") as u64;
        let maybe = |value: Option<u64>| value.map_or(0, |value| value + 1);

        let mut key = vec![
            self.latest,
            maybe(self.installing),
            self.versions.latest().get(),
            rank(self.versions.epoch().get()),
        ];
        let mut retired = Vec::new();
        self.ids.retired(&mut retired);
        for id in 1..=self.bounds.ids {
            let freed_at = retired
                .iter()
                .find(|(page, _)| page.get() == id)
                .map(|(_, freed_at)| freed_at.get());
            key.extend([
                self.ids.state(page(id)) as u64,
                maybe(freed_at),
                maybe(self.stamps.get(&id).map(|stamp| rank(*stamp))),
            ]);
        }
        let mut pages: Vec<[u64; 3]> = self
            .pages
            .iter()
            .map(|page| [page.id.into(), page.born, maybe(page.gone)])
            .collect();
        pages.sort();
        key.push(u64::MAX);
        key.extend(pages.into_iter().flatten());
        let mut txns: Vec<[u64; 3]> = self
            .txns
            .iter()
            .map(|txn| {
                let (id, born) = txn.mark.map_or((0, 0), |(id, born)| (id.into(), born + 1));
                [maybe(txn.private.map(u64::from)), id, born]
            })
            .collect();
        txns.sort();
        key.push(u64::MAX);
        key.extend(txns.into_iter().flatten());
        let mut pins: Vec<u64> = self.pins.iter().map(|pin| pin.version().get()).collect();
        pins.sort();
        key.push(u64::MAX);
        key.extend(pins);
        let mut ops: Vec<[u64; 3]> = self
            .ops
            .iter()
            .map(|op| match op {
                Op::Idle => [0, 0, 0],
                Op::Guarded(guard) => [1, rank(guard.epoch().get()), 0],
                Op::Unguarded(read) => [2, 0, *read],
                Op::Reading(guard, read) => [3, rank(guard.epoch().get()), *read],
            })
            .collect();
        ops.sort();
        key.push(u64::MAX);
        key.extend(ops.into_iter().flatten());
        let mut reclaimers: Vec<[u64; 3]> = self
            .reclaimers
            .iter()
            .map(|answers| {
                [
                    maybe(answers.oldest_pin),
                    maybe(answers.epoch.map(rank)),
                    maybe(answers.oldest_guard.map(rank)),
                ]
            })
            .collect();
        reclaimers.sort();
        key.push(u64::MAX);
        key.extend(reclaimers.into_iter().flatten());
        key
    }
}

/// The state that `path` leads to: what tells it apart, and the steps that
/// can be taken from it. Only the last step is checked and counted: the
/// ones before it were when they were the last.
fn visit(
    bounds: Bounds,
    fault: Fault,
    path: &[Step],
    stats: &mut Stats,
) -> Result<(Vec<u64>, Vec<Step>), String> {
    let versions = Versions::new(CommitVersion::EMPTY);
    let mut world = World::new(&versions, bounds, fault);
    if let Some((last, before)) = path.split_last() {
        let mut uncounted = Stats::default();
        for step in before {
            world.apply(*step, &mut uncounted)?;
        }
        world.apply(*last, stats)?;
    }
    Ok((world.key(), world.steps()))
}

/// Visits every state that the actors can reach within `bounds`, depth
/// first, and checks each. Stops at the first state that breaks the rule,
/// with the steps that led there.
fn explore(bounds: Bounds, fault: Fault) -> Result<Stats, (Vec<Step>, String)> {
    let mut stats = Stats::default();
    let mut path = Vec::new();
    let (key, steps) = visit(bounds, fault, &path, &mut stats).expect("the start is sound");
    let mut seen = HashSet::from([key]);
    let mut stack = vec![(steps, 0)];
    while let Some((steps, next)) = stack.last_mut() {
        let Some(step) = steps.get(*next).copied() else {
            stack.pop();
            path.pop();
            continue;
        };
        *next += 1;
        path.push(step);
        stats.transitions += 1;
        let (key, steps) = match visit(bounds, fault, &path, &mut stats) {
            Ok(visited) => visited,
            Err(why) => return Err((path, why)),
        };
        if seen.insert(key) {
            stack.push((steps, 0));
            stats.deepest = stats.deepest.max(stack.len());
        } else {
            path.pop();
        }
    }
    stats.states = seen.len() as u64;
    Ok(stats)
}

fn explored(bounds: Bounds) -> Stats {
    let stats = explore(bounds, Fault::None)
        .unwrap_or_else(|(steps, why)| panic!("{why}, after {steps:?}"));
    println!("{bounds:?}\n{stats:?}");
    assert!(stats.reused > 0, "no id was used again: {stats:?}");
    assert!(stats.reused_beside_a_reader > 0, "{stats:?}");
    assert!(stats.held_for_a_pin > 0, "no pin held an id: {stats:?}");
    assert!(stats.held_for_a_guard > 0, "no guard held an id: {stats:?}");
    stats
}

const SMALL: Bounds = Bounds {
    ids: 2,
    versions: 3,
    txns: 1,
    pins: 1,
    ops: 1,
    reclaimers: 1,
};

#[test]
fn no_id_is_reused_while_a_reader_can_reach_its_page() {
    explored(SMALL);
}

/// The full bounds: `cargo test --release -p lignindb --lib -- --ignored
/// --nocapture every_interleaving`.
#[test]
#[ignore = "minutes in a release build"]
fn every_interleaving_within_the_full_bounds() {
    let all = [
        Bounds { txns: 2, ..SMALL },
        Bounds { pins: 2, ..SMALL },
        Bounds { ops: 2, ..SMALL },
        Bounds {
            versions: 2,
            reclaimers: 2,
            ..SMALL
        },
        Bounds {
            ids: 3,
            versions: 4,
            ..SMALL
        },
    ];
    for bounds in all {
        explored(bounds);
    }
}

#[test]
fn every_broken_rule_is_found() {
    let faults = [
        Fault::PinOffByOne,
        Fault::InstalledTakenForLatest,
        Fault::GuardAfterRead,
        Fault::EpochBeforePin,
    ];
    for fault in faults {
        let (steps, why) = match explore(SMALL, fault) {
            Err(found) => found,
            Ok(stats) => panic!("{fault:?} was not found: {stats:?}"),
        };
        println!("{fault:?}: {why}, after {steps:?}");
        assert!(why.contains("can reach"), "{fault:?}: {why}");
    }
}
