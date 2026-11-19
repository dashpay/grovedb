//! What write-back keeps from one call to the next: the changes it was
//! told of and has not written, the values that wait for their page step,
//! the files it appends to, and the checkpoint in force.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Condvar, Mutex, MutexGuard, PoisonError,
    },
};

use lignin_format::{value_log::ValuePointer, CommitVersion, PageId};

use super::{
    config::{Counters, Dead, DeadSpace, FlushConfig, FlushStats},
    page::{ChainBuffers, PendingCell},
};
use crate::{
    segment::SegmentWriter,
    table::{CheckpointInfo, Link, LogPlace},
    values::{ValueHold, ValueWriter},
    wal::LogHold,
};

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The records of what one commit did to one tree page, with the records
/// of the page steps that were made on that page version afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Changes {
    pub(super) born: u64,
    /// Records of `page-delta.md`, one after another, without the page id.
    pub(super) records: Vec<u8>,
}

/// What write-back was told and has not put into a block yet. This is the
/// leaf of write-back's locks: it is taken under a page step's lock, and
/// nothing is taken under it.
#[derive(Debug, Default)]
pub(super) struct Kept {
    /// The changes of each tree page, oldest commit first.
    pages: HashMap<PageId, VecDeque<Changes>>,
    /// For each page, the versions of it that are kept in the pool for
    /// their readers and whose changes no block holds: they are in the
    /// block of the next version of the page that is written.
    folded: HashMap<PageId, Vec<u64>>,
    /// Where the group of each commit begins in the log, for the commits
    /// that no table checkpoint covers yet.
    groups: BTreeMap<u64, LogPlace>,
    dead: Dead,
}

impl Kept {
    /// A commit changed a tree page.
    pub(super) fn committed(&mut self, page: PageId, born: u64, records: &[u8]) {
        let changes = self.pages.entry(page).or_default();
        if changes.back().is_some_and(|last| last.born >= born) {
            changes.clear();
        }
        changes.push_back(Changes {
            born,
            records: records.to_vec(),
        });
    }

    /// A page step changed the page version that the commit `born` bore.
    pub(super) fn stepped(&mut self, page: PageId, born: u64, records: &[u8]) {
        let changes = self.pages.entry(page).or_default();
        match changes.back_mut() {
            Some(last) if last.born == born => last.records.extend_from_slice(records),
            Some(last) if last.born > born => {}
            _ => changes.push_back(Changes {
                born,
                records: records.to_vec(),
            }),
        }
    }

    /// The page was made anew or freed: nothing that was kept of it applies
    /// to what has its id now.
    pub(super) fn forget(&mut self, page: PageId) {
        self.pages.remove(&page);
        self.folded.remove(&page);
    }

    /// The changes kept of `page`, oldest first.
    pub(super) fn changes(&self, page: PageId) -> Vec<Changes> {
        self.pages
            .get(&page)
            .map(|changes| changes.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// A block holds the changes `written` of `page`, as they were when
    /// [`Self::changes`] gave them, and every change of the page before
    /// them. What a page step has added since stays.
    pub(super) fn wrote(&mut self, page: PageId, written: &[Changes], through: u64) {
        let Some(changes) = self.pages.get_mut(&page) else {
            return;
        };
        changes.retain_mut(|kept| {
            if kept.born > through {
                return true;
            }
            let Some(done) = written.iter().find(|done| done.born == kept.born) else {
                return false;
            };
            let unchanged = kept.records.starts_with(&done.records);
            if unchanged {
                kept.records.drain(..done.records.len());
            }
            unchanged && !kept.records.is_empty()
        });
        if changes.is_empty() {
            self.pages.remove(&page);
        }
    }

    /// The version `born` of `page` is kept in the pool for its readers,
    /// and its changes go into the block of the next version.
    pub(super) fn fold(&mut self, page: PageId, born: u64) {
        let folded = self.folded.entry(page).or_default();
        if !folded.contains(&born) {
            folded.push(born);
            folded.sort_unstable();
        }
    }

    /// The versions of `page` whose changes no block holds although the
    /// pool does not list them as owed.
    pub(super) fn folded(&self, page: PageId) -> Vec<u64> {
        self.folded.get(&page).cloned().unwrap_or_default()
    }

    /// A block holds every change of `page` up to `through`.
    pub(super) fn unfold(&mut self, page: PageId, through: u64) {
        if let Some(folded) = self.folded.get_mut(&page) {
            folded.retain(|&born| born > through);
            if folded.is_empty() {
                self.folded.remove(&page);
            }
        }
    }

    /// The pages that have a folded version, each with its oldest one.
    pub(super) fn oldest_folded(&self) -> Vec<(PageId, u64)> {
        self.folded
            .iter()
            .filter_map(|(&page, folded)| Some((page, *folded.first()?)))
            .collect()
    }

    pub(super) fn group_at(&mut self, version: u64, at: LogPlace) {
        self.groups.insert(version, at);
    }

    /// Where the group of the commit `version` begins, if write-back was
    /// told.
    pub(super) fn group_place(&self, version: u64) -> Option<LogPlace> {
        self.groups.get(&version).copied()
    }

    /// A table checkpoint that covers `covers` is in force.
    pub(super) fn covered(&mut self, covers: u64) {
        self.groups = self.groups.split_off(&covers.saturating_add(1));
    }

    pub(super) fn died(&mut self, link: &Link) {
        if let Some(location) = link.location() {
            let dead = self.dead.entry(location.file).or_default();
            dead.blocks += 1;
            dead.bytes += u64::from(location.len);
        }
    }

    pub(super) fn dead(&self) -> Dead {
        self.dead.clone()
    }
}

/// A pending value that is in the value log, under a manifest that covers
/// it, and waits for the page step that gives its cell the pointer.
#[derive(Debug)]
pub(super) struct Ready {
    pub(super) page: PageId,
    pub(super) cell: PendingCell,
    pub(super) pointer: ValuePointer,
    /// Keeps the value's file until the step has counted the value, or
    /// the value is given up (`transactions.md` FL1).
    pub(super) _hold: ValueHold,
}

/// The values that wait for their page step. Its lock makes the placing
/// steps of write-back one at a time.
#[derive(Debug, Default)]
pub(super) struct Placing {
    pub(super) ready: Vec<Ready>,
}

/// What one pass uses and the next pass uses again. Its lock makes passes
/// one at a time.
#[derive(Debug, Default)]
pub(super) struct Files {
    pub(super) segment: Option<SegmentWriter>,
    /// The writers of segments whose sync failed. They are kept so that
    /// the blocks behind what was synced stay readable in this session.
    pub(super) failed: Vec<SegmentWriter>,
    pub(super) values: Option<ValueWriter>,
    /// Pages that got a base image, with its version: their older links
    /// are trimmed once no reader can ask for a version before it.
    pub(super) untrimmed: BTreeMap<PageId, CommitVersion>,
    pub(super) chain: ChainBuffers,
    pub(super) links: Vec<Link>,
    pub(super) image: Vec<u8>,
    pub(super) other: Vec<u8>,
    pub(super) run: Vec<u8>,
    pub(super) scratch: Vec<u8>,
    pub(super) value: Vec<u8>,
}

/// A table checkpoint with what keeps the log for it.
#[derive(Debug)]
pub(super) struct Named {
    pub(super) covers: CommitVersion,
    pub(super) place: Option<LogPlace>,
    /// Keeps the log from the place of the checkpoint's sample
    /// (`page-table.md` K14).
    pub(super) _hold: LogHold,
}

/// The table checkpoints of the session. Its lock makes checkpoints one
/// at a time.
#[derive(Debug, Default)]
pub(super) struct Checkpoints {
    /// The checkpoint the manifest names, or the whole log while it names
    /// none.
    pub(super) in_force: Option<Named>,
    /// A checkpoint whose installation left the manifest in doubt.
    pub(super) in_doubt: Option<(CheckpointInfo, Named)>,
    /// The place of the last sample that was taken (`page-table.md` K15).
    pub(super) last_sample: Option<LogPlace>,
}

/// What a waiting committer looks at.
#[derive(Debug, Default)]
pub(super) struct Room {
    /// The number of passes that have ended.
    pub(super) passes: u64,
    /// Whether the last pass wrote or placed anything.
    pub(super) progress: bool,
}

/// The state of write-back for one open store.
///
/// It is made once, when the store is opened, and lives beside the stores
/// it is for. [`WriteBack`](super::WriteBack) is a view of both. It holds
/// no reference, so whoever owns the stores can own it too.
#[derive(Debug)]
pub struct FlushState {
    pub(super) config: FlushConfig,
    pub(super) counters: Counters,
    pub(super) checkpoints: Mutex<Checkpoints>,
    pub(super) files: Mutex<Files>,
    pub(super) placing: Mutex<Placing>,
    pub(super) kept: Mutex<Kept>,
    pub(super) room: Mutex<Room>,
    pub(super) room_changed: Condvar,
    pub(super) wake: Mutex<bool>,
    pub(super) woken: Condvar,
    /// The page versions that wait to be written back, as far as
    /// write-back knows.
    pub(super) owed: AtomicUsize,
    /// The bytes logged since the last table checkpoint.
    pub(super) logged: AtomicU64,
    /// An append to the log failed, and the log takes nothing until it has
    /// been rolled.
    pub(super) roll_wanted: AtomicBool,
    pub(super) segments_failed: AtomicBool,
    pub(super) running: AtomicBool,
    pub(super) stopping: AtomicBool,
}

impl FlushState {
    /// The state of write-back for a store that is being opened, with
    /// nothing kept.
    pub fn new(config: FlushConfig) -> Self {
        Self {
            config,
            counters: Counters::default(),
            checkpoints: Mutex::default(),
            files: Mutex::default(),
            placing: Mutex::default(),
            kept: Mutex::default(),
            room: Mutex::default(),
            room_changed: Condvar::new(),
            wake: Mutex::new(false),
            woken: Condvar::new(),
            owed: AtomicUsize::new(0),
            logged: AtomicU64::new(0),
            roll_wanted: AtomicBool::new(false),
            segments_failed: AtomicBool::new(false),
            running: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        }
    }

    /// The thresholds this state was made with.
    pub fn config(&self) -> FlushConfig {
        self.config
    }

    /// What write-back has done so far.
    pub fn stats(&self) -> FlushStats {
        self.counters.snapshot()
    }

    /// The blocks of each segment that a trim took out of every chain after
    /// write-back had put a newer base image in front of them. Whoever
    /// cleans segments counts them as dead space.
    pub fn dead_space(&self) -> BTreeMap<lignin_format::FileId, DeadSpace> {
        lock(&self.kept).dead()
    }

    /// The page versions that wait to be written back, as far as
    /// write-back knows: what it was told of since its last pass, and what
    /// that pass left.
    pub fn unwritten(&self) -> usize {
        self.owed.load(Ordering::Relaxed)
    }

    /// The table checkpoint that write-back knows to be in force: the
    /// version it covers and the place of its sample. `None` until
    /// write-back was told of the session's start
    /// ([`WriteBack::adopt`](super::WriteBack::adopt)).
    pub fn in_force(&self) -> Option<(CommitVersion, Option<LogPlace>)> {
        let checkpoints = lock(&self.checkpoints);
        let named = checkpoints.in_force.as_ref()?;
        Some((named.covers, named.place))
    }

    pub(super) fn wake(&self) {
        *lock(&self.wake) = true;
        self.woken.notify_all();
    }
}
