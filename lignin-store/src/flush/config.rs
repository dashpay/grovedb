//! How write-back is set, and the counts of what it has done.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use lignin_format::{FileId, PageSize};

/// The length a segment is sealed at by default: 64 MiB.
pub const DEFAULT_SEGMENT_LEN: u64 = 64 << 20;

/// The most delta runs a page's chain has after its base image by default.
pub const DEFAULT_CHAIN_DELTAS: usize = 8;

/// The longest run of deltas that write-back puts into one block of a store
/// of `page_size`: one page size. A longer run would cost a reader more
/// than the base image it stands for, so the page is written as a base
/// image instead.
pub const fn max_run_len(page_size: PageSize) -> usize {
    page_size.get()
}

/// The thresholds of write-back.
///
/// They decide how a page is written and when the writer of a commit is
/// made to wait. None of them changes what a reader is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushConfig {
    /// The chain-length threshold: the most delta runs a chain has after
    /// its base image. A change that would make one more is written as a
    /// base image. With 0 every block is a base image.
    pub max_chain_deltas: usize,
    /// The byte threshold: the most bytes the delta blocks after a base
    /// image take in their segments, frames included. A run that would
    /// pass it is written as a base image.
    pub max_chain_bytes: usize,
    /// The length at which a segment is sealed and the next one begun.
    pub segment_len: u64,
    /// Backpressure: the most page versions that may wait to be written
    /// back before a committer is made to wait
    /// ([`WriteBack::wait_for_room`](super::WriteBack::wait_for_room)).
    pub max_unwritten: usize,
    /// Backpressure: the most bytes of pending values that frames may hold
    /// before a committer is made to wait.
    pub max_pending_bytes: usize,
    /// Backpressure: the most bytes of log that write-back may hold before
    /// a committer is made to wait: the log from the oldest commit that no
    /// table checkpoint covers
    /// ([`WriteBack::log_held`](super::WriteBack::log_held)). It is what a
    /// recovery reads.
    pub max_log_held: u64,
    /// A table checkpoint is due when this many bytes were logged since
    /// the last one, and whenever more log is held than
    /// [`Self::max_log_held`].
    pub checkpoint_log_bytes: u64,
    /// How long the background loop sleeps when nobody wakes it.
    pub idle: Duration,
}

impl FlushConfig {
    /// The defaults for a store of `page_size`: at most 8 delta runs and
    /// half a page of delta blocks after a base image, segments of 64 MiB,
    /// at most 4096 page versions and 32 MiB of pending values waiting, at
    /// most 256 MiB of log that no table checkpoint covers, and a table
    /// checkpoint for every 64 MiB of log.
    pub const fn new(page_size: PageSize) -> Self {
        Self {
            max_chain_deltas: DEFAULT_CHAIN_DELTAS,
            max_chain_bytes: page_size.get() / 2,
            segment_len: DEFAULT_SEGMENT_LEN,
            max_unwritten: 4096,
            max_pending_bytes: 32 << 20,
            max_log_held: 256 << 20,
            checkpoint_log_bytes: 64 << 20,
            idle: Duration::from_millis(50),
        }
    }
}

/// Blocks of one segment that no chain points to any more: what a trim of
/// the page table removed after write-back had put a newer base image in
/// front of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeadSpace {
    /// How many blocks.
    pub blocks: u64,
    /// The bytes they take in the segment, frames included.
    pub bytes: u64,
}

/// What write-back has done since its state was made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlushStats {
    /// Base images written to segments.
    pub base_blocks: u64,
    /// The bytes of their payloads, before compression.
    pub base_bytes: u64,
    /// Runs of deltas written to segments.
    pub delta_blocks: u64,
    /// The bytes of their payloads, before compression.
    pub delta_bytes: u64,
    /// The bytes all those blocks take in their segments, frames included.
    pub stored_bytes: u64,
    /// Base images written because a chain had reached the length or the
    /// byte threshold.
    pub consolidations: u64,
    /// Runs that were not written because they did not give the page in
    /// its frame when they were applied to the page as its chain gives it.
    /// The page was written as a base image. Not zero means that a commit
    /// or a page step did not tell write-back what it did.
    pub runs_refused: u64,
    /// Values appended to the value log for a pending value.
    pub values_appended: u64,
    /// The bytes of those values.
    pub value_bytes: u64,
    /// Pending values that were given their pointer in a page step.
    pub values_placed: u64,
    /// Values that were appended and then given up, because their cell was
    /// gone before it could be given the pointer. They are dead space.
    pub values_given_up: u64,
    /// Values that were appended again and repointed when the values of
    /// their page were regrouped.
    pub values_regrouped: u64,
    /// Page steps that were put off because a write transaction held a
    /// private copy of the page.
    pub steps_put_off: u64,
    /// Page versions that were kept in the pool for their readers instead
    /// of being written: superseded with a pending value, or superseded
    /// before any reader could ask for them.
    pub held_for_readers: u64,
    /// Blocks that were written and whose link the page table refused,
    /// because a commit had freed the page meanwhile. They are dead space.
    pub links_refused: u64,
    /// Table checkpoints installed.
    pub checkpoints: u64,
    /// Attempts at a table checkpoint that were given up because a page
    /// that had to be in it could not be written back yet.
    pub checkpoints_put_off: u64,
    /// Log files taken out of the log.
    pub log_files_dropped: u64,
    /// Calls of the background loop that ended with an error.
    pub errors: u64,
}

#[derive(Debug, Default)]
pub(super) struct Counters {
    pub(super) base_blocks: AtomicU64,
    pub(super) base_bytes: AtomicU64,
    pub(super) delta_blocks: AtomicU64,
    pub(super) delta_bytes: AtomicU64,
    pub(super) stored_bytes: AtomicU64,
    pub(super) consolidations: AtomicU64,
    pub(super) runs_refused: AtomicU64,
    pub(super) values_appended: AtomicU64,
    pub(super) value_bytes: AtomicU64,
    pub(super) values_placed: AtomicU64,
    pub(super) values_given_up: AtomicU64,
    pub(super) values_regrouped: AtomicU64,
    pub(super) steps_put_off: AtomicU64,
    pub(super) held_for_readers: AtomicU64,
    pub(super) links_refused: AtomicU64,
    pub(super) checkpoints: AtomicU64,
    pub(super) checkpoints_put_off: AtomicU64,
    pub(super) log_files_dropped: AtomicU64,
    pub(super) errors: AtomicU64,
}

pub(super) fn count(counter: &AtomicU64, by: u64) {
    counter.fetch_add(by, Ordering::Relaxed);
}

impl Counters {
    pub(super) fn snapshot(&self) -> FlushStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        FlushStats {
            base_blocks: read(&self.base_blocks),
            base_bytes: read(&self.base_bytes),
            delta_blocks: read(&self.delta_blocks),
            delta_bytes: read(&self.delta_bytes),
            stored_bytes: read(&self.stored_bytes),
            consolidations: read(&self.consolidations),
            runs_refused: read(&self.runs_refused),
            values_appended: read(&self.values_appended),
            value_bytes: read(&self.value_bytes),
            values_placed: read(&self.values_placed),
            values_given_up: read(&self.values_given_up),
            values_regrouped: read(&self.values_regrouped),
            steps_put_off: read(&self.steps_put_off),
            held_for_readers: read(&self.held_for_readers),
            links_refused: read(&self.links_refused),
            checkpoints: read(&self.checkpoints),
            checkpoints_put_off: read(&self.checkpoints_put_off),
            log_files_dropped: read(&self.log_files_dropped),
            errors: read(&self.errors),
        }
    }
}

/// The dead blocks of each segment, by segment.
pub(super) type Dead = BTreeMap<FileId, DeadSpace>;
