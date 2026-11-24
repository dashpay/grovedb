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

/// The most pages a turn of the background loop leaves for a later turn
/// by default.
pub const DEFAULT_DEFERRED: usize = 1024;

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
    /// Deferral: the most pages whose current version a turn of the
    /// background loop leaves for a later turn
    /// ([`WriteBack::tick_deferring`](super::WriteBack::tick_deferring)). A turn that finds
    /// more pages waiting writes those whose current version is the
    /// oldest and leaves this many, the ones that changed last, flat
    /// pages before tree pages. With 0 every turn writes every page it
    /// can. A turn leaves no more than half of [`Self::max_unwritten`]
    /// and no more than a quarter of the frames of the pool, whatever
    /// this is, and none where a table checkpoint is due. A pass or a
    /// turn that a caller makes
    /// ([`WriteBack::pass`](super::WriteBack::pass),
    /// [`WriteBack::tick`](super::WriteBack::tick)) and a table
    /// checkpoint leave none, and neither does the end of a session that
    /// comes to its pass ([`WriteBack::finish`](super::WriteBack::finish)).
    pub max_deferred: usize,
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
    /// The longest wait of the background loop after a turn that failed
    /// ([`Self::wait_after_failure`]). Where it is shorter than
    /// [`Self::idle`], or than a millisecond, the longer of those two is
    /// the wait after every failed turn.
    pub max_failed_wait: Duration,
}

impl FlushConfig {
    /// The defaults for a store of `page_size`: at most 8 delta runs and
    /// half a page of delta blocks after a base image, segments of 64 MiB,
    /// at most 1024 pages left by a turn for a later one,
    /// at most 4096 page versions and 32 MiB of pending values waiting, at
    /// most 256 MiB of log that no table checkpoint covers, and a table
    /// checkpoint for every 64 MiB of log. The background loop sleeps
    /// 50 ms when nobody wakes it, and a second at most after a turn that
    /// failed.
    pub const fn new(page_size: PageSize) -> Self {
        Self {
            max_chain_deltas: DEFAULT_CHAIN_DELTAS,
            max_chain_bytes: page_size.get() / 2,
            segment_len: DEFAULT_SEGMENT_LEN,
            max_deferred: DEFAULT_DEFERRED,
            max_unwritten: 4096,
            max_pending_bytes: 32 << 20,
            max_log_held: 256 << 20,
            checkpoint_log_bytes: 64 << 20,
            idle: Duration::from_millis(50),
            max_failed_wait: Duration::from_secs(1),
        }
    }

    /// How long the background loop waits after a turn that failed, before
    /// its next turn. `in_vain` is how many turns in a row, that one
    /// included, failed and moved nothing on: 0 after a failed turn that
    /// moved something on.
    ///
    /// For 0 and for 1 it is [`Self::idle`], or a millisecond where the
    /// idle time is shorter. It is twice as long for each turn more, up
    /// to [`Self::max_failed_wait`], and never shorter than for 1,
    /// whatever the bound is set to.
    pub fn wait_after_failure(&self, in_vain: u32) -> Duration {
        let first = self.idle.max(Duration::from_millis(1));
        let bound = self.max_failed_wait.max(first);
        let mut wait = first;
        for _ in 1..in_vain {
            if wait >= bound {
                break;
            }
            wait = wait.saturating_mul(2);
        }
        wait.min(bound)
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
    /// The base images among `base_blocks` that are of flat pages, whose
    /// every block is an image.
    pub flat_blocks: u64,
    /// The bytes those take in their segments, frames included.
    pub flat_stored_bytes: u64,
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
    /// Values that were appended and then given up. Their cell was gone
    /// before it could be given the pointer, or the value log did not
    /// come to know a manifest that covers them: a sync of their file
    /// was refused, or the manifest was not replaced, or its replacement
    /// was left in doubt. A value of the second kind that a cell still
    /// needs is appended again by a later pass. What a file keeps of a
    /// value that was given up is dead space.
    pub values_given_up: u64,
    /// Value-log files that write-back gave up while it was filling
    /// them: an append to the file was refused, a sync of it failed,
    /// also the one that was to close it, or a regrouping that wrote to
    /// it did. Such a file is closed, where it ends or where it was last
    /// durable, and the next value goes to another file.
    pub value_files_given_up: u64,
    /// Values that were appended again and repointed when the values of
    /// their page were regrouped.
    pub values_regrouped: u64,
    /// The bytes that regroupings appended to the value log, frames
    /// included: the copies of the values counted in `values_regrouped`,
    /// and those of a regrouping that failed or whose page could not take
    /// the new pointers, which are dead space.
    pub regroup_stored_bytes: u64,
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
    /// Attempts at a table checkpoint that ended with an error: a page that
    /// cannot be written, a file operation that failed, or segments that
    /// are not durable.
    pub checkpoints_failed: u64,
    /// Log files taken out of the log.
    pub log_files_dropped: u64,
    /// Calls of the background loop that ended with an error.
    pub errors: u64,
    /// Waits of the background loop after a failed turn in which a turn
    /// was asked for, which the wait put off to its end
    /// ([`WriteBack::run`](super::WriteBack::run)).
    pub turns_put_off: u64,
    /// Commit groups write-back was told of ([`WriteBack::committed`]).
    ///
    /// [`WriteBack::committed`]: super::WriteBack::committed
    pub groups: u64,
    /// Page deltas those groups carry for tree pages: the cells and hashes
    /// the commits changed.
    pub node_records: u64,
    /// Their bytes, without the page id of each run, plus the bytes of the
    /// out-of-line values the groups carry.
    pub node_record_bytes: u64,
    /// Changes those groups carry for flat pages: the flat keyspace.
    pub other_records: u64,
    /// Their bytes, without the page id of each run.
    pub other_record_bytes: u64,
    /// The bytes the values counted in `value_bytes` take in the value
    /// log, frames included.
    pub value_stored_bytes: u64,
    /// The bytes of the table-checkpoint files written, installed or not.
    pub checkpoint_bytes: u64,
    /// Calls of [`WriteBack::wait_for_room`] that found write-back over its
    /// bounds and waited.
    ///
    /// [`WriteBack::wait_for_room`]: super::WriteBack::wait_for_room
    pub room_waits: u64,
    /// Those of them that ended with [`Room::Stalled`].
    ///
    /// [`Room::Stalled`]: super::Room::Stalled
    pub room_stalls: u64,
    /// The time those calls waited, in all.
    pub room_waited: Duration,
    /// Segments whose sync failed. Not zero means that no table checkpoint
    /// is installed until the store is opened again.
    pub segment_syncs_failed: u64,
}

#[derive(Debug, Default)]
pub(super) struct Counters {
    pub(super) base_blocks: AtomicU64,
    pub(super) base_bytes: AtomicU64,
    pub(super) delta_blocks: AtomicU64,
    pub(super) delta_bytes: AtomicU64,
    pub(super) stored_bytes: AtomicU64,
    pub(super) consolidations: AtomicU64,
    pub(super) flat_blocks: AtomicU64,
    pub(super) flat_stored_bytes: AtomicU64,
    pub(super) runs_refused: AtomicU64,
    pub(super) values_appended: AtomicU64,
    pub(super) value_bytes: AtomicU64,
    pub(super) values_placed: AtomicU64,
    pub(super) values_given_up: AtomicU64,
    pub(super) value_files_given_up: AtomicU64,
    pub(super) values_regrouped: AtomicU64,
    pub(super) regroup_stored_bytes: AtomicU64,
    pub(super) steps_put_off: AtomicU64,
    pub(super) held_for_readers: AtomicU64,
    pub(super) links_refused: AtomicU64,
    pub(super) checkpoints: AtomicU64,
    pub(super) checkpoints_put_off: AtomicU64,
    pub(super) checkpoints_failed: AtomicU64,
    pub(super) log_files_dropped: AtomicU64,
    pub(super) errors: AtomicU64,
    pub(super) turns_put_off: AtomicU64,
    pub(super) groups: AtomicU64,
    pub(super) node_records: AtomicU64,
    pub(super) node_record_bytes: AtomicU64,
    pub(super) other_records: AtomicU64,
    pub(super) other_record_bytes: AtomicU64,
    pub(super) value_stored_bytes: AtomicU64,
    pub(super) checkpoint_bytes: AtomicU64,
    pub(super) room_waits: AtomicU64,
    pub(super) room_stalls: AtomicU64,
    pub(super) room_waited_nanos: AtomicU64,
    pub(super) segment_syncs_failed: AtomicU64,
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
            flat_blocks: read(&self.flat_blocks),
            flat_stored_bytes: read(&self.flat_stored_bytes),
            runs_refused: read(&self.runs_refused),
            values_appended: read(&self.values_appended),
            value_bytes: read(&self.value_bytes),
            values_placed: read(&self.values_placed),
            values_given_up: read(&self.values_given_up),
            value_files_given_up: read(&self.value_files_given_up),
            values_regrouped: read(&self.values_regrouped),
            regroup_stored_bytes: read(&self.regroup_stored_bytes),
            steps_put_off: read(&self.steps_put_off),
            held_for_readers: read(&self.held_for_readers),
            links_refused: read(&self.links_refused),
            checkpoints: read(&self.checkpoints),
            checkpoints_put_off: read(&self.checkpoints_put_off),
            checkpoints_failed: read(&self.checkpoints_failed),
            log_files_dropped: read(&self.log_files_dropped),
            errors: read(&self.errors),
            turns_put_off: read(&self.turns_put_off),
            groups: read(&self.groups),
            node_records: read(&self.node_records),
            node_record_bytes: read(&self.node_record_bytes),
            other_records: read(&self.other_records),
            other_record_bytes: read(&self.other_record_bytes),
            value_stored_bytes: read(&self.value_stored_bytes),
            checkpoint_bytes: read(&self.checkpoint_bytes),
            room_waits: read(&self.room_waits),
            room_stalls: read(&self.room_stalls),
            room_waited: Duration::from_nanos(read(&self.room_waited_nanos)),
            segment_syncs_failed: read(&self.segment_syncs_failed),
        }
    }
}

/// The dead blocks of each segment, by segment.
pub(super) type Dead = BTreeMap<FileId, DeadSpace>;
