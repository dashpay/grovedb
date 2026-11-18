//! The acceptance test of STORE-10: no page id is handed out while a reader
//! that can reach its page is open.
//!
//! The allocator is run against a model of everything it does not own: the
//! commits, the write transactions, the version pins, the operation guards
//! and whoever reclaims a dropped subtree. The model is written from
//! `transactions.md`, not from the allocator, and it keeps what the
//! allocator is never told: which page each id named at each version, and
//! which reader can reach it.
//!
//! [`explore`] visits every state that any interleaving of the actors'
//! steps can reach within the bounds, and checks every state. A step is
//! what the engine does under one lock or with one atomic write, so a
//! commit is two steps (install, publish), an operation three (guard, read
//! the latest version, end) and a reclaim four (the three questions it puts
//! to [`Pins`], then its one step under the allocator's lock). The answers
//! a reclaim got are handed to the allocator when its last step runs, and
//! [`Asked`] checks that the allocator asks in the order the model
//! answered.
//!
//! Two states are the same state when they differ only in the numbers of
//! their epochs, not in the order of those, or in a page that no reader can
//! reach any more.

use std::{cell::Cell, collections::HashSet, fmt};

use super::*;

/// One rule of the allocator's contract, broken on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// The oldest pin is reported one version too high.
    PinOffByOne,
    /// Operation guards are not looked at.
    GuardsIgnored,
    /// A reclaim starts its epoch before it asks for the oldest pin.
    EpochBeforePin,
    /// A commit frees a page with the version before its own.
    FreedEarly,
    /// An operation reads the latest version before it takes its guard.
    GuardAfterRead,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    /// The ids there are.
    ids: u32,
    /// The commits a run can make.
    versions: u64,
    txns: usize,
    /// The ids one transaction can hold.
    private: usize,
    /// The pages one transaction can free or drop.
    marks: usize,
    pins: usize,
    ops: usize,
    reclaimers: usize,
    /// Whether a transaction can drop a page that is freed later.
    drops: bool,
}

/// What an id named from one commit to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Page {
    id: u32,
    born: u64,
    /// The version from which no handle reaches the page.
    gone: Option<u64>,
    /// The version the allocator was told the page was freed at.
    freed_at: Option<u64>,
}

impl Page {
    fn reachable_at(&self, version: u64) -> bool {
        self.born <= version && self.gone.is_none_or(|gone| version < gone)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Mark {
    Free,
    Drop,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Txn {
    /// The ids it holds, in the order it took them.
    private: Vec<u32>,
    /// The pages its commit frees or drops, by id and born version.
    marks: Vec<((u32, u64), Mark)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Op {
    #[default]
    Idle,
    Guarded {
        epoch: u64,
    },
    Unguarded {
        version: u64,
    },
    Reading {
        epoch: u64,
        version: u64,
    },
}

/// The answers a reclaim has so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Reclaimer {
    oldest_pin: Option<u64>,
    epoch: Option<u64>,
    oldest_guard: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Allocate(usize),
    GiveBack(usize),
    RollBack(usize),
    Mark(usize, (u32, u64), Mark),
    Install(usize),
    Publish,
    Pin,
    Unpin(u64),
    Guard(usize),
    Read(usize),
    End(usize),
    Reclaim(usize),
    FreeDropped((u32, u64), bool),
}

#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    states: u64,
    transitions: u64,
    deepest: usize,
    allocations: u64,
    /// Allocations of an id that named a page before.
    reused: u64,
    /// Those of them made while a reader was open that cannot reach the
    /// page.
    reused_beside_a_reader: u64,
    exhausted: u64,
    /// Reclaims that left an id of a published commit waiting for a pin.
    held_for_a_pin: u64,
    /// Reclaims that left an id waiting for an operation guard.
    held_for_a_guard: u64,
    released: u64,
    /// Frees of a dropped page with a version below the latest.
    freed_below_the_latest: u64,
}

/// Hands the allocator the answers a reclaim got earlier, and checks that
/// it asks for them in the order they were taken in.
struct Asked {
    answers: Reclaimer,
    asked: Cell<u8>,
}

impl Asked {
    fn of(answers: Reclaimer) -> Self {
        Self {
            answers,
            asked: Cell::new(0),
        }
    }

    fn answer(&self, turn: u8, answer: Option<u64>) -> u64 {
        assert_eq!(self.asked.replace(turn + 1), turn, "asked out of turn");
        answer.expect("the answer was taken")
    }
}

impl Pins for Asked {
    fn oldest_pin(&self) -> CommitVersion {
        version(self.answer(0, self.answers.oldest_pin))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.answer(1, self.answers.epoch))
    }

    fn oldest_guard(&self) -> Epoch {
        Epoch::new(self.answer(2, self.answers.oldest_guard))
    }
}

struct World {
    bounds: Bounds,
    fault: Fault,
    ids: PageAllocator,
    latest: u64,
    /// The version of a commit that is installed and not yet published.
    installing: Option<u64>,
    pages: Vec<Page>,
    txns: Vec<Txn>,
    pins: Vec<u64>,
    ops: Vec<Op>,
    clock: u64,
    reclaimers: Vec<Reclaimer>,
}

impl Clone for World {
    fn clone(&self) -> Self {
        Self {
            bounds: self.bounds,
            fault: self.fault,
            ids: self.ids.fork(),
            latest: self.latest,
            installing: self.installing,
            pages: self.pages.clone(),
            txns: self.txns.clone(),
            pins: self.pins.clone(),
            ops: self.ops.clone(),
            clock: self.clock,
            reclaimers: self.reclaimers.clone(),
        }
    }
}

impl World {
    fn new(bounds: Bounds, fault: Fault) -> Self {
        Self {
            bounds,
            fault,
            ids: PageAllocator::with_limit(page(bounds.ids)),
            latest: 0,
            installing: None,
            pages: Vec::new(),
            txns: vec![Txn::default(); bounds.txns],
            pins: Vec::new(),
            ops: vec![Op::Idle; bounds.ops],
            clock: 0,
            reclaimers: vec![Reclaimer::default(); bounds.reclaimers],
        }
    }

    /// The versions that open readers read: every version pin, and every
    /// operation that has read the latest version.
    fn readers(&self) -> Vec<u64> {
        let reading = self.ops.iter().filter_map(|op| match op {
            Op::Reading { version, .. } | Op::Unguarded { version } => Some(*version),
            _ => None,
        });
        self.pins.iter().copied().chain(reading).collect()
    }

    fn oldest_pin(&self) -> u64 {
        let oldest = self.pins.first().copied().unwrap_or(self.latest);
        oldest + u64::from(self.fault == Fault::PinOffByOne)
    }

    fn oldest_guard(&self) -> u64 {
        if self.fault == Fault::GuardsIgnored {
            return u64::MAX;
        }
        let guards = self.ops.iter().filter_map(|op| match op {
            Op::Guarded { epoch } | Op::Reading { epoch, .. } => Some(*epoch),
            _ => None,
        });
        guards.min().unwrap_or(self.clock)
    }

    fn page_mut(&mut self, (id, born): (u32, u64)) -> Option<&mut Page> {
        self.pages
            .iter_mut()
            .find(|page| page.id == id && page.born == born)
    }

    /// Every step that some actor can take now.
    fn steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        let visible: Vec<&Page> = self
            .pages
            .iter()
            .filter(|page| page.gone.is_none() && page.born <= self.latest)
            .collect();
        for (at, txn) in self.txns.iter().enumerate() {
            if txn.private.len() < self.bounds.private {
                steps.push(Step::Allocate(at));
            }
            if !txn.private.is_empty() {
                steps.push(Step::GiveBack(at));
            }
            if !txn.private.is_empty() || !txn.marks.is_empty() {
                steps.push(Step::RollBack(at));
                if self.installing.is_none() && self.latest < self.bounds.versions {
                    steps.push(Step::Install(at));
                }
            }
            if txn.marks.len() < self.bounds.marks {
                for page in &visible {
                    let key = (page.id, page.born);
                    if txn.marks.iter().any(|(marked, _)| *marked == key) {
                        continue;
                    }
                    steps.push(Step::Mark(at, key, Mark::Free));
                    if self.bounds.drops {
                        steps.push(Step::Mark(at, key, Mark::Drop));
                    }
                }
            }
        }
        if self.installing.is_some() {
            steps.push(Step::Publish);
        }
        if self.pins.len() < self.bounds.pins {
            steps.push(Step::Pin);
        }
        let mut pinned = self.pins.clone();
        pinned.dedup();
        steps.extend(pinned.into_iter().map(Step::Unpin));
        for (at, op) in self.ops.iter().enumerate() {
            match op {
                Op::Idle => steps.push(Step::Guard(at)),
                Op::Guarded { .. } | Op::Unguarded { .. } => {
                    steps.extend([Step::Read(at), Step::End(at)]);
                }
                Op::Reading { .. } => steps.push(Step::End(at)),
            }
        }
        steps.extend((0..self.reclaimers.len()).map(Step::Reclaim));
        for page in &self.pages {
            if page.freed_at.is_none() && page.gone.is_some_and(|gone| gone <= self.latest) {
                let key = (page.id, page.born);
                steps.push(Step::FreeDropped(key, false));
                if page.gone != Some(self.latest) {
                    steps.push(Step::FreeDropped(key, true));
                }
            }
        }
        steps
    }

    fn apply(&mut self, step: Step, stats: &mut Stats) -> Result<(), String> {
        match step {
            Step::Allocate(at) => self.allocate(at, stats)?,
            Step::GiveBack(at) => {
                let id = self.txns[at].private.pop().expect("the step is enabled");
                self.give_back(id)?;
            }
            Step::RollBack(at) => {
                for id in std::mem::take(&mut self.txns[at]).private {
                    self.give_back(id)?;
                }
            }
            Step::Mark(at, key, mark) => {
                self.txns[at].marks.push((key, mark));
                self.txns[at].marks.sort();
            }
            Step::Install(at) => self.install(at)?,
            Step::Publish => {
                self.latest = self.installing.take().expect("the step is enabled");
            }
            Step::Pin => {
                self.pins.push(self.latest);
                self.pins.sort();
            }
            Step::Unpin(pinned) => {
                let at = self.pins.iter().position(|pin| *pin == pinned);
                self.pins.remove(at.expect("the step is enabled"));
            }
            Step::Guard(at) => {
                self.ops[at] = if self.fault == Fault::GuardAfterRead {
                    Op::Unguarded {
                        version: self.latest,
                    }
                } else {
                    Op::Guarded { epoch: self.clock }
                };
            }
            Step::Read(at) => {
                self.ops[at] = match self.ops[at] {
                    Op::Guarded { epoch } => Op::Reading {
                        epoch,
                        version: self.latest,
                    },
                    Op::Unguarded { version } => Op::Reading {
                        epoch: self.clock,
                        version,
                    },
                    op => op,
                };
            }
            Step::End(at) => self.ops[at] = Op::Idle,
            Step::Reclaim(at) => self.reclaim(at, stats)?,
            Step::FreeDropped(key, at_latest) => {
                let latest = self.latest;
                let page = self.page_mut(key).expect("the step is enabled");
                let freed_at = if at_latest {
                    latest
                } else {
                    page.gone.expect("the page was dropped")
                };
                page.freed_at = Some(freed_at);
                let id = page.id;
                stats.freed_below_the_latest += u64::from(freed_at < latest);
                if let Err(error) = self.ids.free(super::page(id), version(freed_at)) {
                    return Err(format!(
                        "the free of dropped page {id} was refused: {error}"
                    ));
                }
            }
        }
        self.check()
    }

    fn give_back(&mut self, id: u32) -> Result<(), String> {
        self.ids
            .release(page(id))
            .map_err(|error| format!("a private id was not taken back: {error}"))
    }

    fn allocate(&mut self, at: usize, stats: &mut Stats) -> Result<(), String> {
        let lowest = (1..=self.bounds.ids).find(|id| self.ids.state(page(*id)) == PageState::Free);
        let got = match self.ids.allocate() {
            Ok(got) => got.get(),
            Err(AllocError::Exhausted) if lowest.is_none() => {
                stats.exhausted += 1;
                return Ok(());
            }
            Err(error) => return Err(format!("allocate failed with id {lowest:?} free: {error}")),
        };
        if Some(got) != lowest {
            return Err(format!(
                "allocate gave id {got}, and the lowest free id is {lowest:?}"
            ));
        }
        if self.txns.iter().any(|txn| txn.private.contains(&got)) {
            return Err(format!("id {got} was handed out to two transactions"));
        }
        let readers = self.readers();
        let named: Vec<Page> = self
            .pages
            .iter()
            .filter(|page| page.id == got)
            .copied()
            .collect();
        for old in &named {
            if old.freed_at.is_none() {
                return Err(format!(
                    "id {got} was handed out while it names a committed page"
                ));
            }
            let reached = [self.latest]
                .into_iter()
                .chain(self.installing)
                .chain(readers.iter().copied())
                .find(|reader| old.reachable_at(*reader));
            if let Some(reader) = reached {
                return Err(format!(
                    "id {got} was handed out while a reader of version {reader} can reach the \
                     page it named from version {} to {:?}",
                    old.born, old.gone
                ));
            }
        }
        stats.allocations += 1;
        if !named.is_empty() {
            stats.reused += 1;
            stats.reused_beside_a_reader += u64::from(!readers.is_empty());
        }
        self.pages.retain(|page| page.id != got);
        self.txns[at].private.push(got);
        Ok(())
    }

    /// Step 7 of a commit: its page versions are installed, and the ids of
    /// the pages it frees are retired. A commit that would free a page
    /// another commit has freed fails with a conflict, and is rolled back.
    fn install(&mut self, at: usize) -> Result<(), String> {
        let txn = std::mem::take(&mut self.txns[at]);
        let live = |world: &Self, (id, born): (u32, u64)| {
            world
                .pages
                .iter()
                .any(|page| page.id == id && page.born == born && page.gone.is_none())
        };
        if txn.marks.iter().any(|(key, _)| !live(self, *key)) {
            for id in txn.private {
                self.give_back(id)?;
            }
            return Ok(());
        }
        let commit = self.latest + 1;
        for id in txn.private {
            if let Err(error) = self.ids.publish(page(id)) {
                return Err(format!("a private id was not published: {error}"));
            }
            self.pages.push(Page {
                id,
                born: commit,
                gone: None,
                freed_at: None,
            });
        }
        for (key, mark) in txn.marks {
            let freed_at = match self.fault {
                Fault::FreedEarly => (commit - 1).max(1),
                _ => commit,
            };
            let page = self.page_mut(key).expect("it is live");
            page.gone = Some(commit);
            if mark == Mark::Free {
                page.freed_at = Some(freed_at);
                if let Err(error) = self.ids.free(super::page(key.0), version(freed_at)) {
                    return Err(format!("the free of page {} was refused: {error}", key.0));
                }
            }
        }
        self.pages.sort();
        self.installing = Some(commit);
        Ok(())
    }

    /// The next step of a reclaim: one of its three questions, in the order
    /// the allocator puts them, or its step under the allocator's lock.
    fn reclaim(&mut self, at: usize, stats: &mut Stats) -> Result<(), String> {
        let mut answers = self.reclaimers[at];
        let pin_first = self.fault != Fault::EpochBeforePin;
        if answers.oldest_pin.is_none() && (pin_first || answers.epoch.is_some()) {
            answers.oldest_pin = Some(self.oldest_pin());
        } else if answers.epoch.is_none() {
            self.clock += 1;
            answers.epoch = Some(self.clock);
        } else if answers.oldest_guard.is_none() {
            answers.oldest_guard = Some(self.oldest_guard());
        } else {
            let asked = Asked::of(answers);
            let mut released = Vec::new();
            let before = self.ids.census().retired;
            self.ids.reclaim(&asked, &mut released);
            if asked.asked.get() != 3 {
                return Err("a reclaim did not ask all three questions".into());
            }
            if before - self.ids.census().retired != released.len() as u64 {
                return Err(format!(
                    "a reclaim reported {released:?} and freed other ids"
                ));
            }
            let (waiting, draining) = self.ids.stages();
            let published = waiting
                .iter()
                .any(|(freed_at, _)| freed_at.get() <= self.latest);
            stats.held_for_a_pin += u64::from(published);
            stats.held_for_a_guard += u64::from(!draining.is_empty());
            stats.released += released.len() as u64;
            answers = Reclaimer::default();
        }
        self.reclaimers[at] = answers;
        Ok(())
    }

    /// What must hold in every state.
    fn check(&self) -> Result<(), String> {
        let readers = self.readers();
        let mut retired = Vec::new();
        for id in 1..=self.bounds.ids {
            let state = self.ids.state(page(id));
            let holders = self
                .txns
                .iter()
                .filter(|txn| txn.private.contains(&id))
                .count();
            let named: Vec<&Page> = self.pages.iter().filter(|page| page.id == id).collect();
            let committed = named.iter().filter(|page| page.freed_at.is_none()).count();
            if holders + committed > 1 {
                return Err(format!(
                    "id {id} names {committed} pages and has {holders} holders"
                ));
            }
            let fits = match state {
                PageState::Private => holders == 1,
                PageState::Live => committed == 1,
                PageState::Retired | PageState::Free => holders + committed == 0,
            };
            if !fits {
                return Err(format!(
                    "id {id} is {state} with {holders} holders and {committed} committed pages"
                ));
            }
            if state == PageState::Free {
                let every = [self.latest].into_iter().chain(self.installing);
                for reader in every.chain(readers.iter().copied()) {
                    if let Some(old) = named.iter().find(|page| page.reachable_at(reader)) {
                        return Err(format!(
                            "id {id} is free while a reader of version {reader} can reach the \
                             page it named from version {} to {:?}",
                            old.born, old.gone
                        ));
                    }
                }
            }
            if state == PageState::Retired {
                let newest = named.iter().filter_map(|page| page.freed_at).max();
                let Some(freed_at) = newest else {
                    return Err(format!("id {id} is retired and no page of it was freed"));
                };
                retired.push((page(id), version(freed_at)));
            }
        }

        let census = self.ids.census();
        let private: usize = self.txns.iter().map(|txn| txn.private.len()).sum();
        let live = self
            .pages
            .iter()
            .filter(|page| page.freed_at.is_none())
            .count();
        let counted = (census.private, census.live, census.retired);
        if counted != (private as u64, live as u64, retired.len() as u64) {
            return Err(format!(
                "the census is {census:?} with {private} private, {live} live and {} retired ids",
                retired.len()
            ));
        }
        if super::retired(&self.ids) != retired {
            return Err(format!("the retired ids are not {retired:?}"));
        }

        let settled = self.ids.fork();
        let quiet = Asked::of(Reclaimer {
            oldest_pin: Some(self.installing.unwrap_or(self.latest)),
            epoch: Some(u64::MAX),
            oldest_guard: Some(u64::MAX),
        });
        settled.reclaim(&quiet, &mut Vec::new());
        if settled.census().retired != 0 {
            return Err("an id stays retired when every reader is gone".into());
        }
        Ok(())
    }

    /// What tells this state from every state with another future.
    fn key(&self) -> Box<[u8]> {
        let readers = self.readers();
        let small = |value: u64| u8::try_from(value).expect("a small number");
        let maybe = |value: Option<u64>| value.map_or(0, |value| small(value) + 1);
        let (waiting, draining) = self.ids.stages();

        let mut epochs: Vec<u64> = vec![self.clock];
        for op in &self.ops {
            if let Op::Guarded { epoch } | Op::Reading { epoch, .. } = op {
                epochs.push(*epoch);
            }
        }
        for reclaimer in &self.reclaimers {
            epochs.extend(reclaimer.epoch);
            epochs.extend(reclaimer.oldest_guard);
        }
        epochs.extend(draining.iter().map(|(epoch, ..)| epoch.get()));
        epochs.sort();
        epochs.dedup();
        let rank =
            |epoch: u64| small(epochs.binary_search(&epoch).expect("it was collected") as u64);

        let mut key = vec![small(self.latest), maybe(self.installing), rank(self.clock)];
        for id in 1..=self.bounds.ids {
            key.push(match self.ids.state(page(id)) {
                PageState::Free => 0,
                PageState::Private => 1,
                PageState::Live => 2,
                PageState::Retired => 3,
            });
        }
        for (freed_at, pages) in waiting {
            key.extend([0xF0, small(freed_at.get())]);
            let mut pages: Vec<u8> = pages.iter().map(|page| small(page.get().into())).collect();
            pages.sort();
            key.extend(pages);
        }
        for (epoch, _, pages) in draining {
            key.extend([0xF1, rank(epoch.get())]);
            let mut pages: Vec<u8> = pages.iter().map(|page| small(page.get().into())).collect();
            pages.sort();
            key.extend(pages);
        }
        for page in &self.pages {
            let matters = page.freed_at.is_none()
                || page.gone.is_none_or(|gone| gone > self.latest)
                || readers.iter().any(|reader| page.reachable_at(*reader));
            if matters {
                key.extend([
                    0xF2,
                    small(page.id.into()),
                    small(page.born),
                    maybe(page.gone),
                    u8::from(page.freed_at.is_some()),
                ]);
            }
        }
        let mut txns: Vec<Vec<u8>> = self
            .txns
            .iter()
            .map(|txn| {
                let mut part: Vec<u8> = txn.private.iter().map(|id| small((*id).into())).collect();
                for ((id, born), mark) in &txn.marks {
                    part.extend([0xF3, small((*id).into()), small(*born), *mark as u8]);
                }
                part
            })
            .collect();
        txns.sort();
        for part in txns {
            key.push(0xF4);
            key.extend(part);
        }
        key.push(0xF5);
        key.extend(self.pins.iter().map(|pin| small(*pin)));
        let mut ops: Vec<[u8; 3]> = self
            .ops
            .iter()
            .map(|op| match op {
                Op::Idle => [0, 0, 0],
                Op::Guarded { epoch } => [1, rank(*epoch), 0],
                Op::Unguarded { version } => [2, 0, small(*version)],
                Op::Reading { epoch, version } => [3, rank(*epoch), small(*version)],
            })
            .collect();
        ops.sort();
        key.push(0xF6);
        key.extend(ops.into_iter().flatten());
        let mut reclaimers: Vec<[u8; 3]> = self
            .reclaimers
            .iter()
            .map(|reclaimer| {
                [
                    maybe(reclaimer.oldest_pin),
                    reclaimer.epoch.map_or(0, |epoch| rank(epoch) + 1),
                    reclaimer.oldest_guard.map_or(0, |epoch| rank(epoch) + 1),
                ]
            })
            .collect();
        reclaimers.sort();
        key.push(0xF7);
        key.extend(reclaimers.into_iter().flatten());
        key.into_boxed_slice()
    }
}

/// A run that breaks a rule: the steps, and what was found after the last.
struct Counterexample {
    steps: Vec<Step>,
    why: String,
}

impl fmt::Display for Counterexample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}, after {} steps:", self.why, self.steps.len())?;
        self.steps
            .iter()
            .try_for_each(|step| writeln!(f, "  {step:?}"))
    }
}

struct Frame {
    world: World,
    steps: Vec<Step>,
    next: usize,
}

/// Visits every state that the actors can reach within `bounds`, depth
/// first, and checks each. Stops at the first state that breaks a rule.
fn explore(bounds: Bounds, fault: Fault) -> Result<Stats, Counterexample> {
    let mut stats = Stats::default();
    let root = World::new(bounds, fault);
    let mut seen: HashSet<Box<[u8]>> = HashSet::from([root.key()]);
    let steps = root.steps();
    let mut stack = vec![Frame {
        world: root,
        steps,
        next: 0,
    }];
    while let Some(frame) = stack.last_mut() {
        let Some(step) = frame.steps.get(frame.next).copied() else {
            stack.pop();
            continue;
        };
        frame.next += 1;
        let mut world = frame.world.clone();
        stats.transitions += 1;
        if let Err(why) = world.apply(step, &mut stats) {
            let steps = stack
                .iter()
                .map(|frame| frame.steps[frame.next - 1])
                .collect();
            return Err(Counterexample { steps, why });
        }
        if seen.insert(world.key()) {
            let steps = world.steps();
            stack.push(Frame {
                world,
                steps,
                next: 0,
            });
            stats.deepest = stats.deepest.max(stack.len());
        }
    }
    stats.states = seen.len() as u64;
    Ok(stats)
}

/// The allocator did every kind of thing the test is about.
fn assert_not_vacuous(bounds: Bounds, stats: &Stats) {
    assert!(
        !bounds.drops || stats.freed_below_the_latest > 0,
        "no dropped page was freed with an earlier version: {stats:?}"
    );
    assert!(stats.reused > 0, "no id was used again: {stats:?}");
    assert!(stats.reused_beside_a_reader > 0, "{stats:?}");
    assert!(
        stats.held_for_a_pin > 0,
        "no pin held an id back: {stats:?}"
    );
    assert!(
        stats.held_for_a_guard > 0,
        "no guard held an id back: {stats:?}"
    );
    assert!(stats.exhausted > 0, "the ids never ran out: {stats:?}");
}

const SMALL: Bounds = Bounds {
    ids: 2,
    versions: 3,
    txns: 2,
    private: 1,
    marks: 1,
    pins: 1,
    ops: 1,
    reclaimers: 1,
    drops: false,
};

#[test]
fn no_id_is_reused_while_a_reader_can_reach_its_page() {
    let stats = explore(SMALL, Fault::None).unwrap_or_else(|found| panic!("{found}"));
    println!("{SMALL:?}\n{stats:?}");
    assert_not_vacuous(SMALL, &stats);
}

#[test]
fn no_id_is_reused_while_a_reader_can_reach_a_dropped_page() {
    let bounds = Bounds {
        txns: 1,
        drops: true,
        ..SMALL
    };
    let stats = explore(bounds, Fault::None).unwrap_or_else(|found| panic!("{found}"));
    println!("{bounds:?}\n{stats:?}");
    assert_not_vacuous(bounds, &stats);
}

#[test]
fn no_id_is_reused_too_early_by_two_reclaims_at_once() {
    let bounds = Bounds {
        versions: 2,
        txns: 1,
        reclaimers: 2,
        ..SMALL
    };
    let stats = explore(bounds, Fault::None).unwrap_or_else(|found| panic!("{found}"));
    println!("{bounds:?}\n{stats:?}");
    assert_not_vacuous(bounds, &stats);
}

/// The full bounds. `cargo test --release -p lignin-store -- --ignored
/// --nocapture every_interleaving`.
#[test]
#[ignore = "minutes in a release build"]
fn every_interleaving_within_the_full_bounds() {
    let all = [
        Bounds {
            ids: 3,
            versions: 4,
            pins: 2,
            ..SMALL
        },
        Bounds {
            ids: 3,
            txns: 1,
            private: 2,
            marks: 2,
            pins: 2,
            ..SMALL
        },
        Bounds {
            versions: 4,
            pins: 2,
            ..SMALL
        },
        Bounds {
            pins: 2,
            ops: 2,
            ..SMALL
        },
        Bounds {
            pins: 2,
            drops: true,
            ..SMALL
        },
        Bounds {
            txns: 1,
            reclaimers: 2,
            drops: true,
            ..SMALL
        },
    ];
    for bounds in all {
        let stats = explore(bounds, Fault::None).unwrap_or_else(|found| panic!("{found}"));
        println!("{bounds:?}\n{stats:?}");
        assert_not_vacuous(bounds, &stats);
    }
}

#[test]
fn every_broken_rule_is_found() {
    let faults = [
        (Fault::PinOffByOne, "can reach"),
        (Fault::GuardsIgnored, "can reach"),
        (Fault::EpochBeforePin, "can reach"),
        (Fault::FreedEarly, "can reach"),
        (Fault::GuardAfterRead, "can reach"),
    ];
    for (fault, expected) in faults {
        let found = match explore(SMALL, fault) {
            Err(found) => found,
            Ok(stats) => panic!("{fault:?} was not found: {stats:?}"),
        };
        println!("{fault:?}: {found}");
        assert!(found.why.contains(expected), "{fault:?}: {found}");
    }
}

#[test]
fn an_allocator_that_asks_out_of_turn_is_caught() {
    let asked = Asked::of(Reclaimer {
        oldest_pin: Some(1),
        epoch: Some(1),
        oldest_guard: Some(1),
    });
    asked.oldest_pin();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| asked.oldest_guard()));
    assert!(caught.is_err());
}

#[test]
fn a_counterexample_prints_its_steps() {
    let found = Counterexample {
        steps: vec![Step::Pin, Step::Publish],
        why: "broken".into(),
    };
    assert_eq!(
        found.to_string(),
        "broken, after 2 steps:\n  Pin\n  Publish\n"
    );
}

/// Long runs with more of everything than the exhaustive runs can have, on
/// one allocator from start to end.
#[test]
fn drawn_runs_keep_every_rule() {
    let bounds = Bounds {
        ids: 6,
        versions: u64::MAX,
        txns: 3,
        private: 3,
        marks: 2,
        pins: 3,
        ops: 2,
        reclaimers: 2,
        drops: true,
    };
    let mut stats = Stats::default();
    for seed in 1..=8 {
        let mut rng = Rng::new(seed);
        let mut world = World::new(bounds, Fault::None);
        let mut taken = Vec::new();
        for _ in 0..25_000 {
            let steps = world.steps();
            let step = steps[rng.below(steps.len() as u64) as usize];
            taken.push(step);
            stats.transitions += 1;
            if let Err(why) = world.apply(step, &mut stats) {
                let steps = taken.split_off(taken.len().saturating_sub(40));
                panic!("seed {seed}: {}", Counterexample { steps, why });
            }
        }
    }
    println!("{bounds:?}\n{stats:?}");
    assert_not_vacuous(bounds, &stats);
    assert!(stats.reused > 1000, "{stats:?}");
}

#[test]
fn drawn_runs_find_every_broken_rule() {
    let bounds = Bounds {
        ids: 4,
        versions: u64::MAX,
        txns: 2,
        private: 2,
        marks: 2,
        pins: 2,
        ops: 2,
        reclaimers: 1,
        drops: true,
    };
    let faults = [
        Fault::PinOffByOne,
        Fault::GuardsIgnored,
        Fault::EpochBeforePin,
        Fault::FreedEarly,
        Fault::GuardAfterRead,
    ];
    for fault in faults {
        let found = (1..=20).any(|seed| {
            let mut rng = Rng::new(seed);
            let mut world = World::new(bounds, fault);
            let mut stats = Stats::default();
            (0..20_000).any(|_| {
                let steps = world.steps();
                let step = steps[rng.below(steps.len() as u64) as usize];
                world.apply(step, &mut stats).is_err()
            })
        });
        assert!(found, "{fault:?} was not found");
    }
}
