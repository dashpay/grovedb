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
    wal::{LogError, LogHold, LogStore},
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
    /// Where the group of each commit begins in the log, and how many
    /// bytes write-back had seen logged before it, for the commits that no
    /// table checkpoint covers yet.
    groups: BTreeMap<u64, (LogPlace, u64)>,
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

    /// The page was freed at the version `at`: nothing older of it is
    /// written back (`page-table.md` T18), so nothing older is kept. What
    /// a later commit did to a page that was made again under the id
    /// stays.
    pub(super) fn freed(&mut self, page: PageId, at: u64) {
        if let Some(changes) = self.pages.get_mut(&page) {
            changes.retain(|kept| kept.born >= at);
            if changes.is_empty() {
                self.pages.remove(&page);
            }
        }
        self.unfold_where(page, |born| born < at);
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

    /// The version `born` of `page` is to be kept in the pool for its
    /// readers, and its changes go into the block of the next version.
    /// Returns whether that is new.
    pub(super) fn fold(&mut self, page: PageId, born: u64) -> bool {
        let folded = self.folded.entry(page).or_default();
        let new = !folded.contains(&born);
        if new {
            folded.push(born);
            folded.sort_unstable();
        }
        new
    }

    /// The version `born` of `page` is not kept for its readers after all.
    pub(super) fn unfold_one(&mut self, page: PageId, born: u64) {
        self.unfold_where(page, |folded| folded == born);
    }

    fn unfold_where(&mut self, page: PageId, gone: impl Fn(u64) -> bool) {
        if let Some(folded) = self.folded.get_mut(&page) {
            folded.retain(|&born| !gone(born));
            if folded.is_empty() {
                self.folded.remove(&page);
            }
        }
    }

    /// The versions of `page` whose changes no block holds although the
    /// pool does not list them as owed.
    pub(super) fn folded(&self, page: PageId) -> Vec<u64> {
        self.folded.get(&page).cloned().unwrap_or_default()
    }

    /// Nothing waits for a block of what `page` was up to `through`: a
    /// block holds every change up to it, or the page was freed after it.
    pub(super) fn unfold(&mut self, page: PageId, through: u64) {
        self.unfold_where(page, |born| born <= through);
    }

    /// The pages that have a folded version, each with its oldest and its
    /// newest one.
    pub(super) fn folded_pages(&self) -> Vec<(PageId, u64, u64)> {
        self.folded
            .iter()
            .filter_map(|(&page, folded)| Some((page, *folded.first()?, *folded.last()?)))
            .collect()
    }

    /// The group of the commit `version` begins at `at`, with `before`
    /// bytes logged before it.
    pub(super) fn group_at(&mut self, version: u64, at: LogPlace, before: u64) {
        self.groups.insert(version, (at, before));
    }

    /// Where the group of the commit `version` begins, if write-back was
    /// told.
    pub(super) fn group_place(&self, version: u64) -> Option<LogPlace> {
        self.groups.get(&version).map(|&(at, _)| at)
    }

    /// How many bytes were logged before the group of the commit
    /// `version`, if write-back was told of it.
    #[cfg(test)]
    pub(super) fn logged_before(&self, version: u64) -> Option<u64> {
        self.groups.get(&version).map(|&(_, before)| before)
    }

    /// How many bytes were logged before the oldest group that no table
    /// checkpoint covers, if write-back was told of one.
    pub(super) fn logged_before_uncovered(&self) -> Option<u64> {
        self.groups.values().next().map(|&(_, before)| before)
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

/// What a waiting committer looks at: the turns of the background loop.
#[derive(Debug, Default)]
pub(super) struct Turns {
    /// The number of turns that have begun.
    pub(super) begun: u64,
    /// The number of the last turn that has ended, counted as they begin.
    pub(super) ended: u64,
    /// Whether that turn moved anything on.
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
    /// The values that wait for their page step. Its lock makes the
    /// placing steps of write-back one at a time.
    pub(super) placing: Mutex<Vec<Ready>>,
    pub(super) kept: Mutex<Kept>,
    pub(super) turns: Mutex<Turns>,
    pub(super) turned: Condvar,
    pub(super) wake: Mutex<bool>,
    pub(super) woken: Condvar,
    /// The page versions that wait to be written back, as far as
    /// write-back knows.
    pub(super) owed: AtomicUsize,
    /// The bytes logged since the last table checkpoint.
    pub(super) logged: AtomicU64,
    /// The bytes logged since this state was made.
    pub(super) logged_in_all: AtomicU64,
    /// Pages the last pass could not write because of their bytes.
    pub(super) failing: AtomicUsize,
    /// Pages the last pass could not write because a write transaction
    /// holds a private copy ([`Pass::blocked`](super::Pass::blocked)).
    pub(super) blocked: AtomicUsize,
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
            turns: Mutex::default(),
            turned: Condvar::new(),
            wake: Mutex::new(false),
            woken: Condvar::new(),
            owed: AtomicUsize::new(0),
            logged: AtomicU64::new(0),
            logged_in_all: AtomicU64::new(0),
            failing: AtomicUsize::new(0),
            blocked: AtomicUsize::new(0),
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

    /// The pages that the last pass could not write because of their
    /// bytes: a page its format does not allow ([`FlushError::Page`]), or
    /// a pending value with no bytes of its length
    /// ([`FlushError::PendingValue`]). Such a page holds the log until the
    /// store is opened again (D-091).
    ///
    /// [`FlushError::Page`]: super::FlushError::Page
    /// [`FlushError::PendingValue`]: super::FlushError::PendingValue
    pub fn failing(&self) -> usize {
        self.failing.load(Ordering::Relaxed)
    }

    /// The pages that the last pass could not write because a write
    /// transaction holds a private copy of them.
    pub fn blocked(&self) -> usize {
        self.blocked.load(Ordering::Relaxed)
    }

    /// Whether a segment's sync failed in this session, so that no table
    /// checkpoint is installed until the store is opened again.
    pub fn segments_not_durable(&self) -> bool {
        self.segments_failed.load(Ordering::Acquire)
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

    /// Keeps the log for the table checkpoint the manifest names, which
    /// `in_force` is, or the whole log if it names none: what
    /// [`WriteBack::adopt`](super::WriteBack::adopt) does, for whoever has
    /// the log and no view of the stores yet.
    pub(crate) fn adopt(
        &self,
        log: &LogStore,
        in_force: Option<&CheckpointInfo>,
    ) -> Result<(), LogError> {
        let place = in_force.and_then(CheckpointInfo::log_kept_from);
        let hold = log.hold_from(place)?;
        let mut checkpoints = lock(&self.checkpoints);
        checkpoints.last_sample = place;
        checkpoints.in_force = Some(Named {
            covers: in_force.map_or(CommitVersion::EMPTY, CheckpointInfo::covers),
            place,
            _hold: hold,
        });
        Ok(())
    }

    /// Notes an item of the log that a recovery replayed and that the
    /// table checkpoint in force does not cover: `len` bytes of log, which
    /// are the group of the commit `group`, beginning at that place, if
    /// the item is one.
    ///
    /// Write-back then knows how much log is held ([`WriteBack::log_held`])
    /// and where the group of each replayed commit begins, which is what
    /// it drops log files by once a checkpoint covers the commit. It is
    /// told nothing of the pages: a page that a recovery rebuilt is
    /// written as a base image.
    ///
    /// [`WriteBack::log_held`]: super::WriteBack::log_held
    pub(crate) fn replayed(&self, group: Option<(CommitVersion, LogPlace)>, len: u64) {
        let mut kept = lock(&self.kept);
        let before = self.logged_in_all.fetch_add(len, Ordering::Relaxed);
        if let Some((version, at)) = group {
            kept.group_at(version.get(), at, before);
        }
        drop(kept);
        self.logged.fetch_add(len, Ordering::Relaxed);
    }

    /// Notes a page version that a recovery has put into the pool for
    /// write-back to write. `oldest` is the oldest replayed commit whose
    /// change to `page` it holds, if it holds one: the page version then
    /// holds the changes of every replayed commit from that one on, and
    /// no block holds any of them.
    ///
    /// The pool lists that page version under the commit that bore it,
    /// which is the newest of those commits. Without this note write-back
    /// would take the commits before it for written back, and a table
    /// checkpoint could cover them while their changes to the page are in
    /// the log alone. With it they wait, as the changes of a page version
    /// that is kept for its readers do, for the block of the page's next
    /// version that is written.
    pub(crate) fn replayed_page(&self, page: PageId, oldest: Option<CommitVersion>) {
        if let Some(oldest) = oldest {
            lock(&self.kept).fold(page, oldest.get());
        }
        self.owed.fetch_add(1, Ordering::Relaxed);
    }
}
