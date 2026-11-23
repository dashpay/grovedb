//! What the cleaner keeps from one pass to the next: the files it appends
//! to, the segments that wait to be deleted, and its counts.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex, MutexGuard, PoisonError,
    },
    time::{Duration, Instant},
};

use lignin_format::{CommitVersion, FileId};

use super::{
    config::{CleanerConfig, CleanerStats, Counters},
    space::Space,
};
use crate::{
    alloc::Epoch,
    flush::trim_bound::TrimBound,
    segment::{SegmentWriter, SyncRecord},
    values::{ChangeMark, Retired, ValueWriter},
};

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A segment that the page table points into no more.
#[derive(Debug, Clone, Copy)]
pub(super) struct RetiredSegment {
    /// The epoch that was started when the segment was found so: every
    /// operation that can have read a location in it holds a guard of an
    /// earlier epoch (`transactions.md` PN6).
    pub(super) epoch: Epoch,
    /// The bytes of its blocks.
    pub(super) block_bytes: u64,
}

/// What told the last pass that it had nothing to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Seen {
    pub(super) links: u64,
    pub(super) pages: u64,
    pub(super) blocks_written: u64,
    pub(super) oldest_pin: CommitVersion,
    pub(super) segments: usize,
    /// The segments that have a writer: one that loses its writer can be
    /// cleaned from then on, with nothing else changed.
    pub(super) writers: usize,
}

/// What one pass uses and the next pass uses again. Its lock makes the
/// passes of the cleaner one at a time.
#[derive(Debug, Default)]
pub(super) struct Work {
    /// The segment that takes the moved blocks.
    pub(super) segment: Option<SegmentWriter>,
    /// The writers of segments whose sync failed. They are kept so that
    /// the blocks behind what was synced stay readable in this session.
    pub(super) failed: Vec<SegmentWriter>,
    /// The value-log file that takes the copies of moved values.
    pub(super) values: Option<ValueWriter>,
    /// The segments that wait to be deleted.
    pub(super) retired: BTreeMap<FileId, RetiredSegment>,
    /// What the value log may drop, each with the epoch that was started
    /// when it was asked (`ValueStore::retire`).
    pub(super) value_tokens: VecDeque<(Retired, Epoch)>,
    /// The mark of the value log's changes that the cleaner last made the
    /// log durable through.
    pub(super) values_synced: Option<ChangeMark>,
    /// What the value log was last asked to retire, with the mark the log
    /// was durable through then, and how many passes have not asked again
    /// since because the answer would have been the same.
    pub(super) values_asked: Option<(Retired, ChangeMark)>,
    pub(super) asks_spared: u32,
    /// How many passes over the value log have begun.
    pub(super) value_passes: u64,
    /// The files whose last cleaning moved nothing because write
    /// transactions held copies of the pages of their values, each with
    /// the pass from which it is tried again.
    pub(super) busy_files: BTreeMap<FileId, u64>,
    /// The version at which chains are trimmed.
    pub(super) trim_bound: TrimBound,
    /// Where the blocks of each sealed segment end.
    pub(super) blocks_end: BTreeMap<FileId, u64>,
    /// Segments of which a live block could not be read.
    pub(super) unreadable: BTreeSet<FileId>,
    /// What the store was like when a pass last found nothing to do.
    pub(super) idle_at: Option<Seen>,
}

/// The state of the cleaner for one open store.
///
/// It is made once, when the store is opened, and lives beside the stores
/// and the state of write-back. [`Cleaner`](super::Cleaner) is a view of
/// all of them. It holds no reference, so whoever owns the stores can own
/// it too.
#[derive(Debug)]
pub struct CleanerState {
    pub(super) config: CleanerConfig,
    pub(super) counters: Counters,
    pub(super) work: Mutex<Work>,
    /// The segment store's record of each segment that the cleaner has
    /// moved blocks into and that is not sealed, or whose sync failed.
    pub(super) written: Mutex<Vec<SyncRecord>>,
    /// What the last read of the page table found.
    pub(super) space: Mutex<Space>,
    pub(super) wake: Mutex<bool>,
    pub(super) woken: Condvar,
    /// Whether room was made on the device since the loop last ended a
    /// sleep ([`Self::room_made`]). It is set and cleared under the lock
    /// on `wake` only.
    pub(super) room_made: AtomicBool,
    pub(super) stopping: AtomicBool,
    /// What a test does between the copy of a block and the switch of its
    /// link.
    #[cfg(test)]
    pub(super) at_switch: Mutex<Option<AtSwitch>>,
}

/// What a test is told of a block that was copied: its page and where it
/// was.
#[cfg(test)]
pub(super) struct AtSwitch(
    pub(super) Box<dyn FnMut(lignin_format::PageId, crate::segment::BlockLocation) + Send>,
);

#[cfg(test)]
impl std::fmt::Debug for AtSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AtSwitch")
    }
}

impl CleanerState {
    /// The state of the cleaner for a store that is being opened.
    pub fn new(config: CleanerConfig) -> Self {
        Self {
            config,
            counters: Counters::default(),
            work: Mutex::default(),
            written: Mutex::default(),
            space: Mutex::default(),
            wake: Mutex::new(false),
            woken: Condvar::new(),
            room_made: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            at_switch: Mutex::default(),
        }
    }

    /// The settings this state was made with.
    pub fn config(&self) -> CleanerConfig {
        self.config
    }

    /// What the cleaner has done so far.
    pub fn stats(&self) -> CleanerStats {
        let failed = lock(&self.written)
            .iter()
            .filter(|segment| segment.failed())
            .count();
        CleanerStats {
            segments_not_durable: failed as u64,
            ..self.counters.snapshot()
        }
    }

    /// Notes that the cleaner moves blocks into the segment of `record`.
    pub(super) fn writes(&self, record: SyncRecord) {
        lock(&self.written).push(record);
    }

    /// Gives up the record of the segment `file`, which was sealed or
    /// was synced as a whole just now with its writer still held: no
    /// sync of it has failed, and none can fail any more.
    pub(super) fn wrote(&self, file: FileId) {
        lock(&self.written).retain(|segment| segment.file_id() != file);
    }

    /// What the segments held when a pass last read the page table
    /// through. It is empty until a pass has.
    pub fn space(&self) -> Space {
        lock(&self.space).clone()
    }

    pub(super) fn wake(&self) {
        *lock(&self.wake) = true;
        self.woken.notify_all();
    }

    /// Notes that room was made on the device, and wakes the loop
    /// ([`Cleaner::room_made`](super::Cleaner::room_made)): the sleep
    /// that the loop is in, or the one that follows the turn it is in,
    /// ends at once, whichever kind it is ([`Self::sleep`]). The mark is
    /// set under the lock that a sleep holds whenever it does not wait,
    /// and the sleep that finds it clears it under the same lock: one
    /// call ends one sleep.
    pub(super) fn room_made(&self) {
        let _woken = lock(&self.wake);
        self.room_made.store(true, Ordering::Release);
        self.woken.notify_all();
    }

    /// The sleep of the background loop between two turns: until it is
    /// woken or its idle time has passed after a turn in which no pass
    /// failed (`failed_wait` is `None`), and for `failed_wait` after one in
    /// which a pass did, whoever wakes it. The end of the loop ends both,
    /// and so does a call that room was made ([`Self::room_made`]), which
    /// ends the one sleep that is running when it comes or that begins
    /// next.
    pub(super) fn sleep(&self, failed_wait: Option<Duration>) {
        let stopping = || self.stopping.load(Ordering::Acquire);
        let room_made = || self.room_made.load(Ordering::Acquire);
        let mut woken = lock(&self.wake);
        match failed_wait {
            None => {
                if !*woken && !stopping() && !room_made() {
                    let (next, _) = self
                        .woken
                        .wait_timeout(woken, self.config.idle)
                        .unwrap_or_else(PoisonError::into_inner);
                    woken = next;
                }
            }
            Some(wait) => {
                let began = Instant::now();
                while !stopping() && !room_made() {
                    let Some(left) = wait.checked_sub(began.elapsed()) else {
                        break;
                    };
                    let (next, _) = self
                        .woken
                        .wait_timeout(woken, left)
                        .unwrap_or_else(PoisonError::into_inner);
                    woken = next;
                }
            }
        }
        *woken = false;
        self.room_made.store(false, Ordering::Release);
    }
}
