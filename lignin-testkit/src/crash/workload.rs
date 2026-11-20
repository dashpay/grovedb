//! Random workloads: what a session does to its store, drawn from a seed.
//!
//! A workload is a function of its seed and of what the pages say when
//! each operation is drawn, which is the model's to know. The generator is
//! the pinned one of [`crate::workload::rng`], so a seed gives the same
//! operations on every machine.

use std::{collections::BTreeSet, sync::Arc};

use lignin_format::PageId;

use super::{
    model::{apply, Cell, Change, Hash, Page, Pages, Value},
    store::Reclaim,
};
use crate::workload::rng::Rng;

/// One thing a session does.
///
/// A step of the segment cleaner (STORE-08) is one more operation here and
/// one more call in the campaign's `Lineage::run`: it changes no page, so
/// the model has nothing to learn of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// A write transaction makes these changes and commits. With
    /// `durable`, the log is synced before the commit returns, as the sync
    /// policy "every commit" does.
    Commit {
        /// The changes.
        changes: Vec<Change>,
        /// Whether the commit syncs the log.
        durable: bool,
    },
    /// A write transaction copies these pages and stays open.
    Hold(Vec<PageId>),
    /// The transaction that holds copies makes these changes and commits.
    CommitHeld(Vec<Change>),
    /// The transaction that holds copies rolls back.
    Release,
    /// A reclaiming page step.
    Reclaim {
        /// The page.
        page: PageId,
        /// What the step takes.
        what: Reclaim,
    },
    /// One pass of write-back.
    Pass,
    /// One turn of write-back's loop.
    Tick,
    /// A table checkpoint.
    Checkpoint,
    /// The log is made durable.
    SyncLog,
    /// The log's files are kept going.
    RollLog,
    /// Long chains are consolidated.
    Consolidate,
    /// The value log's cleaner cleans a file.
    Clean,
    /// The value-log files with no live value are dropped.
    DropValueFiles,
    /// The chains of freed pages are trimmed away.
    TrimFreed,
    /// A snapshot pins the latest version.
    Pin,
    /// The snapshot is dropped.
    Unpin,
    /// The process is killed and the store is opened again.
    Kill,
    /// The store is closed and opened again.
    Close,
    /// The next operation of the file layer that changes a file fails,
    /// once.
    FailNext,
    /// The device is full from here on.
    DiskFull,
    /// The device has room again.
    DiskFree,
}

impl Op {
    /// A short name of the kind of operation.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Commit { durable: false, .. } => "commit",
            Self::Commit { durable: true, .. } => "durable commit",
            Self::Hold(_) => "hold",
            Self::CommitHeld(_) => "held commit",
            Self::Release => "release",
            Self::Reclaim { .. } => "reclaim",
            Self::Pass => "pass",
            Self::Tick => "tick",
            Self::Checkpoint => "checkpoint",
            Self::SyncLog => "sync log",
            Self::RollLog => "roll log",
            Self::Consolidate => "consolidate",
            Self::Clean => "clean values",
            Self::DropValueFiles => "drop value files",
            Self::TrimFreed => "trim freed",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
            Self::Kill => "kill",
            Self::Close => "close",
            Self::FailNext => "fail next write",
            Self::DiskFull => "disk full",
            Self::DiskFree => "disk free",
        }
    }
}

/// How large a workload's store gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The most pages in use.
    pub max_pages: usize,
    /// The most cells of a tree page.
    pub max_cells: usize,
    /// The most entries of a flat leaf.
    pub max_entries: usize,
    /// The most changes of one commit.
    pub max_changes: u64,
    /// One value out of line in this many is longer than a page.
    pub long_value_one_in: u64,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            max_pages: 9,
            max_cells: 6,
            max_entries: 6,
            max_changes: 4,
            long_value_one_in: 48,
        }
    }
}

/// What a session is in when its next operation is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Situation<'a> {
    /// The pages a write transaction holds copies of, if one is open.
    pub held: Option<&'a BTreeSet<PageId>>,
    /// Whether a snapshot holds a pin.
    pub pinned: bool,
    /// Whether the run injects faults, and so may fill the device.
    pub faulty: bool,
    /// Whether the device is full.
    pub full: bool,
}

/// The operations of one run.
#[derive(Clone, Debug)]
pub struct Workload {
    rng: Rng,
    shape: Shape,
}

impl Workload {
    /// The workload of `seed`, for the lineage `lineage` of its run.
    pub fn new(seed: u64, lineage: u64, shape: Shape) -> Self {
        const DOMAIN: u64 = 0x5645_522d_3033;
        Self {
            rng: Rng::derive(seed, DOMAIN, lineage),
            shape,
        }
    }

    fn pick<T: Copy>(&mut self, weighted: &[(u64, T)]) -> T {
        let total: u64 = weighted.iter().map(|(weight, _)| weight).sum();
        let mut at = self.rng.below(total);
        for (weight, item) in weighted {
            if at < *weight {
                return *item;
            }
            at -= weight;
        }
        unreachable!("a draw below the sum of the weights")
    }

    fn hash(&mut self) -> Hash {
        self.rng.id()
    }

    fn value(&mut self) -> Value {
        if self.rng.chance(11, 20) {
            let len = self.rng.below(41) as u32;
            return Value::Inline(self.rng.bytes(len));
        }
        let len = match self.rng.chance(1, self.shape.long_value_one_in) {
            true => self.rng.between(2_000, 9_000),
            false => self.rng.between(64, 900),
        };
        Value::Out(Arc::from(self.rng.bytes(len as u32)))
    }

    fn cell(&mut self) -> Cell {
        let (key, feature) = (self.rng.between(1, 12), self.rng.below(7));
        Cell {
            key: self.rng.bytes(key as u32),
            feature: self.rng.bytes(feature as u32),
            value_hash: self.hash(),
            kv_hash: self.hash(),
            value: self.value(),
        }
    }

    /// A key that `taken` does not say is in use.
    fn key(&mut self, taken: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        loop {
            let len = self.rng.between(1, 10) as u32;
            let key = self.rng.bytes(len);
            if !taken(&key) {
                return key;
            }
        }
    }

    fn new_tree(&mut self) -> Change {
        let cells = self.rng.between(1, self.shape.max_cells.min(4) as u64) as u16;
        Change::NewTree {
            cells: (1..=cells).map(|slot| (slot, self.cell())).collect(),
        }
    }

    fn new_flat(&mut self) -> Change {
        let count = self.rng.between(1, self.shape.max_entries.min(4) as u64);
        let mut entries: Vec<(Vec<u8>, Value)> = Vec::new();
        for _ in 0..count {
            let key = self.key(|key| entries.iter().any(|(taken, _)| taken == key));
            entries.push((key, self.value()));
        }
        Change::NewFlat { entries }
    }

    /// One change to the page `page`, which says `said`. `fresh` says that
    /// the commit has not changed the page yet, and `spare` that other
    /// pages are in use.
    fn change(&mut self, page: PageId, said: &Page, fresh: bool, spare: bool) -> Change {
        #[derive(Clone, Copy)]
        enum Kind {
            Insert,
            Remove,
            Hash,
            Feature,
            SetValue,
            PutNew,
            PutOver,
            Delete,
            Rewrite,
            Free,
        }
        let free = u64::from(fresh && spare);
        match said {
            Page::Tree(cells) => {
                let slots: Vec<u16> = cells.keys().copied().collect();
                let room = u64::from(slots.len() < self.shape.max_cells);
                let some = u64::from(!slots.is_empty());
                let kind = self.pick(&[
                    (3 * room + u64::from(slots.is_empty()), Kind::Insert),
                    (2 * u64::from(slots.len() >= 2), Kind::Remove),
                    (4 * some, Kind::Hash),
                    (some, Kind::Feature),
                    (3 * some, Kind::SetValue),
                    (free, Kind::Free),
                ]);
                let slot = match slots.is_empty() {
                    true => 0,
                    false => slots[self.rng.index(slots.len())],
                };
                match kind {
                    Kind::Insert => Change::Insert {
                        page,
                        slot: (1..)
                            .find(|slot| !cells.contains_key(slot))
                            .expect("a slot"),
                        cell: self.cell(),
                    },
                    Kind::Remove => Change::Remove { page, slot },
                    Kind::Hash => Change::Hash {
                        page,
                        slot,
                        kv: self.rng.chance(1, 2),
                        hash: self.hash(),
                    },
                    Kind::Feature => {
                        let len = self.rng.below(7) as u32;
                        Change::Feature {
                            page,
                            slot,
                            feature: self.rng.bytes(len),
                        }
                    }
                    Kind::SetValue => Change::SetValue {
                        page,
                        slot,
                        value: self.value(),
                    },
                    _ => Change::Free { page },
                }
            }
            Page::Flat(entries) => {
                let keys: Vec<&Vec<u8>> = entries.keys().collect();
                let room = u64::from(keys.len() < self.shape.max_entries);
                let some = u64::from(!keys.is_empty());
                let kind = self.pick(&[
                    (3 * room + u64::from(keys.is_empty()), Kind::PutNew),
                    (3 * some, Kind::PutOver),
                    (2 * u64::from(keys.len() >= 2), Kind::Delete),
                    (u64::from(fresh), Kind::Rewrite),
                    (free, Kind::Free),
                ]);
                let at = self.rng.index(keys.len().max(1));
                match kind {
                    Kind::PutNew => Change::Put {
                        page,
                        key: self.key(|key| entries.contains_key(key)),
                        value: self.value(),
                    },
                    Kind::PutOver => Change::Put {
                        page,
                        key: keys[at].clone(),
                        value: self.value(),
                    },
                    Kind::Delete => Change::Delete {
                        page,
                        key: keys[at].clone(),
                    },
                    Kind::Rewrite => {
                        let mut written: Vec<(Vec<u8>, Option<Value>)> = Vec::new();
                        for (key, value) in entries {
                            if self.rng.chance(1, 2) {
                                continue;
                            }
                            // A value out of line stays where it lies.
                            let kept = match value {
                                Value::Out(_) => None,
                                Value::Inline(_) => Some(value.clone()),
                            };
                            written.push((key.clone(), kept));
                        }
                        let room = (self.shape.max_entries - written.len()) as u64;
                        let more = self.rng.below(3) + u64::from(written.is_empty());
                        for _ in 0..more.min(room) {
                            let taken = |key: &[u8]| {
                                entries.contains_key(key)
                                    || written.iter().any(|(taken, _)| taken == key)
                            };
                            let key = self.key(taken);
                            written.push((key, Some(self.value())));
                        }
                        Change::Rewrite {
                            page,
                            entries: written,
                        }
                    }
                    _ => Change::Free { page },
                }
            }
        }
    }

    /// The changes of one commit to `pages`, of which it may change those
    /// that `mine` says.
    fn changes(&mut self, pages: &Pages, mine: impl Fn(PageId) -> bool) -> Vec<Change> {
        let count = self.rng.between(1, self.shape.max_changes);
        let mut scratch = pages.clone();
        // Pages that the commit has written whole or freed take no other
        // change of it, and neither does a page it makes.
        let mut sealed: BTreeSet<PageId> = BTreeSet::new();
        let mut touched: BTreeSet<PageId> = BTreeSet::new();
        let mut changes = Vec::new();
        let mut made = 0;
        for _ in 0..count {
            let open: Vec<PageId> = scratch
                .keys()
                .copied()
                .filter(|page| mine(*page) && !sealed.contains(page))
                .collect();
            let room = scratch.len() + made < self.shape.max_pages;
            if open.is_empty() || (room && self.rng.chance(1, 5)) {
                if room || changes.is_empty() {
                    changes.push(match self.rng.chance(2, 3) {
                        true => self.new_tree(),
                        false => self.new_flat(),
                    });
                    made += 1;
                }
                continue;
            }
            let page = open[self.rng.index(open.len())];
            let fresh = !touched.contains(&page);
            let spare = scratch.len() >= 2;
            let change = self.change(page, &scratch[&page].clone(), fresh, spare);
            scratch = apply(&scratch, std::slice::from_ref(&change), &[])
                .expect("a change that was drawn for the page fits it");
            touched.insert(page);
            if matches!(change, Change::Rewrite { .. } | Change::Free { .. }) {
                sealed.insert(page);
            }
            changes.push(change);
        }
        changes
    }

    /// The next operation of a session whose pages say `pages`.
    pub fn next(&mut self, pages: &Pages, situation: Situation<'_>) -> Op {
        #[derive(Clone, Copy)]
        enum Kind {
            Commit,
            Durable,
            Hold,
            CommitHeld,
            Release,
            Reclaim,
            Pass,
            Tick,
            Checkpoint,
            SyncLog,
            RollLog,
            Consolidate,
            Clean,
            DropValueFiles,
            TrimFreed,
            Pin,
            Unpin,
            Kill,
            Close,
            FailNext,
            DiskFull,
            DiskFree,
        }
        let Situation {
            held,
            pinned,
            faulty,
            full,
        } = situation;
        let free: Vec<PageId> = pages
            .keys()
            .copied()
            .filter(|page| held.is_none_or(|held| !held.contains(page)))
            .collect();
        let some = u64::from(!free.is_empty());
        let holding = u64::from(held.is_some());
        let kind = self.pick(&[
            (34, Kind::Commit),
            (4, Kind::Durable),
            (3 * some * (1 - holding), Kind::Hold),
            (7 * holding, Kind::CommitHeld),
            (3 * holding, Kind::Release),
            (5 * some, Kind::Reclaim),
            (7, Kind::Pass),
            (6, Kind::Tick),
            (6, Kind::Checkpoint),
            (5, Kind::SyncLog),
            (2, Kind::RollLog),
            (1, Kind::Consolidate),
            (4, Kind::Clean),
            (2, Kind::DropValueFiles),
            (3, Kind::TrimFreed),
            (2 * u64::from(!pinned), Kind::Pin),
            (4 * u64::from(pinned), Kind::Unpin),
            (2, Kind::Kill),
            (1, Kind::Close),
            (3 * u64::from(faulty), Kind::FailNext),
            (u64::from(faulty && !full), Kind::DiskFull),
            (8 * u64::from(full), Kind::DiskFree),
        ]);
        let not_held = |page: PageId| held.is_none_or(|held| !held.contains(&page));
        match kind {
            Kind::Commit | Kind::Durable => Op::Commit {
                changes: self.changes(pages, not_held),
                durable: matches!(kind, Kind::Durable),
            },
            Kind::Hold => {
                let mut held = vec![free[self.rng.index(free.len())]];
                if self.rng.chance(1, 3) {
                    held.push(free[self.rng.index(free.len())]);
                    held.dedup();
                }
                Op::Hold(held)
            }
            Kind::CommitHeld => {
                let held = held.expect("a transaction holds copies");
                Op::CommitHeld(self.changes(pages, |page| held.contains(&page)))
            }
            Kind::Release => Op::Release,
            Kind::Reclaim => {
                let page = free[self.rng.index(free.len())];
                let what = match &*pages[&page] {
                    Page::Tree(cells) if cells.len() >= 2 && self.rng.chance(2, 3) => {
                        let mut slots: Vec<u16> = cells.keys().copied().collect();
                        self.rng.shuffle(&mut slots);
                        slots.truncate(self.rng.between(1, slots.len() as u64 - 1) as usize);
                        slots.sort_unstable();
                        Reclaim::Cells(slots)
                    }
                    _ => Reclaim::Page,
                };
                Op::Reclaim { page, what }
            }
            Kind::Pass => Op::Pass,
            Kind::Tick => Op::Tick,
            Kind::Checkpoint => Op::Checkpoint,
            Kind::SyncLog => Op::SyncLog,
            Kind::RollLog => Op::RollLog,
            Kind::Consolidate => Op::Consolidate,
            Kind::Clean => Op::Clean,
            Kind::DropValueFiles => Op::DropValueFiles,
            Kind::TrimFreed => Op::TrimFreed,
            Kind::Pin => Op::Pin,
            Kind::Unpin => Op::Unpin,
            Kind::Kill => Op::Kill,
            Kind::Close => Op::Close,
            Kind::FailNext => Op::FailNext,
            Kind::DiskFull => Op::DiskFull,
            Kind::DiskFree => Op::DiskFree,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Runs a workload on the model alone: every commit is made, with ids
    /// from 1 up, and every reclaiming step.
    fn walk(seed: u64, ops: usize, faulty: bool) -> (Vec<Op>, Pages) {
        let mut workload = Workload::new(seed, 0, Shape::default());
        let mut pages = Pages::new();
        let mut held: Option<BTreeSet<PageId>> = None;
        let (mut pinned, mut full) = (false, false);
        let mut next_id = 1;
        let mut all = Vec::new();
        for _ in 0..ops {
            let situation = Situation {
                held: held.as_ref(),
                pinned,
                faulty,
                full,
            };
            let op = workload.next(&pages, situation);
            match &op {
                Op::Commit { changes, .. } | Op::CommitHeld(changes) => {
                    let made: Vec<PageId> = changes
                        .iter()
                        .filter(|change| change.makes_page())
                        .map(|_| {
                            next_id += 1;
                            PageId::new(next_id).expect("a page id")
                        })
                        .collect();
                    if let Op::CommitHeld(_) = op {
                        let held = held.take().expect("a transaction holds copies");
                        let touched = changes.iter().filter_map(Change::page);
                        assert!(touched.clone().all(|page| held.contains(&page)));
                    } else if let Some(held) = &held {
                        let mut touched = changes.iter().filter_map(Change::page);
                        assert!(touched.all(|page| !held.contains(&page)));
                    }
                    assert!(!changes.is_empty());
                    pages = apply(&pages, changes, &made).expect("the changes fit");
                }
                Op::Hold(held_pages) => {
                    assert!(held.is_none() && !held_pages.is_empty());
                    assert!(held_pages.iter().all(|page| pages.contains_key(page)));
                    held = Some(held_pages.iter().copied().collect());
                }
                Op::Release => assert!(held.take().is_some()),
                Op::Reclaim { page, what } => {
                    assert!(held.as_ref().is_none_or(|held| !held.contains(page)));
                    let slots = match what {
                        Reclaim::Cells(slots) => slots.clone(),
                        Reclaim::Page => Vec::new(),
                    };
                    pages = super::super::model::reclaim(&pages, *page, &slots).expect("fits");
                    match (what, pages.get(page).map(|page| &**page)) {
                        (Reclaim::Cells(_), Some(Page::Tree(cells))) => assert!(!cells.is_empty()),
                        (Reclaim::Page, None) => {}
                        (what, left) => panic!("{what:?} left {left:?}"),
                    }
                }
                Op::Pin => pinned = !std::mem::replace(&mut pinned, true),
                Op::Unpin => pinned = !std::mem::replace(&mut pinned, false),
                Op::DiskFull => full = !std::mem::replace(&mut full, true),
                Op::DiskFree => full = !std::mem::replace(&mut full, false),
                Op::Kill | Op::Close => held = None,
                _ => {}
            }
            let shape = Shape::default();
            // A commit of several changes can pass the bound by what it
            // makes, never by more.
            assert!(pages.len() <= shape.max_pages + shape.max_changes as usize);
            for page in pages.values() {
                match &**page {
                    Page::Tree(cells) => assert!(cells.len() <= shape.max_cells),
                    Page::Flat(entries) => assert!(entries.len() <= shape.max_entries),
                }
            }
            all.push(op);
        }
        (all, pages)
    }

    #[test]
    fn the_same_seed_gives_the_same_operations() {
        let (first, pages) = walk(7, 400, true);
        let (second, again) = walk(7, 400, true);
        assert_eq!(first, second);
        assert_eq!(pages, again);
        assert_ne!(first, walk(8, 400, true).0);
        // Each lineage of a run has operations of its own.
        let mut one = Workload::new(1, 0, Shape::default());
        let mut other = Workload::new(1, 1, Shape::default());
        let quiet = Situation::default();
        let first = |workload: &mut Workload| workload.next(&Pages::new(), quiet);
        assert_ne!(first(&mut one), first(&mut other));
    }

    #[test]
    fn a_workload_has_every_operation_and_every_change() {
        let mut names: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut kinds: BTreeMap<&'static str, u64> = BTreeMap::new();
        let (mut long, mut kept) = (0, 0);
        for seed in 0..12 {
            for op in walk(seed, 600, true).0 {
                *names.entry(op.name()).or_default() += 1;
                let changes = match &op {
                    Op::Commit { changes, .. } | Op::CommitHeld(changes) => changes.as_slice(),
                    Op::Reclaim {
                        what: Reclaim::Cells(_),
                        ..
                    } => {
                        *kinds.entry("reclaim cells").or_default() += 1;
                        &[]
                    }
                    Op::Reclaim { .. } => {
                        *kinds.entry("reclaim page").or_default() += 1;
                        &[]
                    }
                    _ => &[],
                };
                for change in changes {
                    let kind = match change {
                        Change::NewTree { .. } => "new tree",
                        Change::NewFlat { .. } => "new flat",
                        Change::Insert { .. } => "insert",
                        Change::Remove { .. } => "remove",
                        Change::Hash { kv: true, .. } => "kv hash",
                        Change::Hash { kv: false, .. } => "value hash",
                        Change::Feature { .. } => "feature",
                        Change::SetValue { .. } => "set value",
                        Change::Rewrite { entries, .. } => {
                            kept += entries.iter().filter(|(_, value)| value.is_none()).count();
                            "rewrite"
                        }
                        Change::Put { .. } => "put",
                        Change::Delete { .. } => "delete",
                        Change::Free { .. } => "free",
                    };
                    *kinds.entry(kind).or_default() += 1;
                    if let Change::SetValue { value, .. } | Change::Put { value, .. } = change {
                        long += usize::from(value.bytes().len() >= 2_000);
                    }
                }
            }
        }
        let expected = [
            "commit",
            "durable commit",
            "hold",
            "held commit",
            "release",
            "reclaim",
            "pass",
            "tick",
            "checkpoint",
            "sync log",
            "roll log",
            "consolidate",
            "clean values",
            "drop value files",
            "trim freed",
            "pin",
            "unpin",
            "kill",
            "close",
            "fail next write",
            "disk full",
            "disk free",
        ];
        for name in expected {
            assert!(
                names.get(name).is_some_and(|count| *count >= 5),
                "{name}: {names:?}"
            );
        }
        assert_eq!(names.len(), expected.len());
        let changes = [
            "new tree",
            "new flat",
            "insert",
            "remove",
            "kv hash",
            "value hash",
            "feature",
            "set value",
            "rewrite",
            "put",
            "delete",
            "free",
            "reclaim cells",
            "reclaim page",
        ];
        for kind in changes {
            assert!(
                kinds.get(kind).is_some_and(|count| *count >= 20),
                "{kind}: {kinds:?}"
            );
        }
        assert_eq!(kinds.len(), changes.len());
        assert!(long >= 5 && kept >= 20, "{long} long values, {kept} kept");

        // A run that injects no fault never fills the device.
        let quiet = walk(3, 600, false).0;
        let fault = |op: &Op| matches!(op, Op::DiskFull | Op::DiskFree | Op::FailNext);
        assert!(!quiet.iter().any(fault));
    }
}
