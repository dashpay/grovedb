//! How the cleaner is set, and the counts of what it has done.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::flush::DEFAULT_SEGMENT_LEN;

/// The share of dead bytes, in a hundred, above which the cleaner moves
/// blocks by default.
pub const DEFAULT_TARGET_DEAD_PERCENT: u8 = 10;

/// The most bytes of blocks one pass moves by default: 8 MiB.
pub const DEFAULT_PASS_BYTES: u64 = 8 << 20;

/// The settings of the cleaner.
///
/// They decide when blocks and values are moved and how fast. None of them
/// changes what a reader is given, or when a file may be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanerConfig {
    /// The share of dead bytes among the bytes of all blocks, in a hundred,
    /// that the cleaner works towards: a pass moves live blocks out of
    /// segments while the share is above it. The segments that only wait
    /// to be deleted are left out of the share, since no moving helps them
    /// on.
    pub target_dead_percent: u8,
    /// A table checkpoint is asked for when the segments that the page
    /// table points into no more, and that only the table checkpoint in
    /// force still keeps, hold this share of the bytes of all blocks, in a
    /// hundred.
    pub checkpoint_dead_percent: u8,
    /// The segment that write-back is filling is sealed when its dead
    /// blocks are this share of the bytes of all blocks, in a hundred. A
    /// segment is not cleaned while it is written.
    pub seal_dead_percent: u8,
    /// The segment that write-back is filling is not sealed for the
    /// cleaner before its blocks take this many bytes: a store that is
    /// small against it is not held to the target.
    pub min_segment_len: u64,
    /// The throttle: the most bytes of blocks, as they are stored, that
    /// one pass moves. With [`Self::idle`] it bounds what the cleaner
    /// writes in a second.
    pub max_pass_bytes: u64,
    /// The length at which the segment that takes the moved blocks is
    /// sealed and the next one begun.
    pub segment_len: u64,
    /// The share of dead bytes among the bytes of all values in the value
    /// log, in a hundred, above which a pass cleans a value-log file. One
    /// file is cleaned in a pass.
    pub value_dead_percent: u8,
    /// How long the background loop sleeps between two passes when nobody
    /// wakes it.
    pub idle: Duration,
}

impl CleanerConfig {
    /// The defaults: work towards 10 dead bytes in a hundred, in segments
    /// and in the value log; ask for a table checkpoint, and seal the
    /// segment that is being filled, at 5 in a hundred, and seal no
    /// segment before it holds 1 MiB; move at most 8 MiB in a pass, into
    /// segments of 64 MiB; sleep 100 ms between passes.
    pub const fn new() -> Self {
        Self {
            target_dead_percent: DEFAULT_TARGET_DEAD_PERCENT,
            checkpoint_dead_percent: DEFAULT_TARGET_DEAD_PERCENT / 2,
            seal_dead_percent: DEFAULT_TARGET_DEAD_PERCENT / 2,
            min_segment_len: 1 << 20,
            max_pass_bytes: DEFAULT_PASS_BYTES,
            segment_len: DEFAULT_SEGMENT_LEN,
            value_dead_percent: DEFAULT_TARGET_DEAD_PERCENT,
            idle: Duration::from_millis(100),
        }
    }
}

impl Default for CleanerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// What the cleaner has done since its state was made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanerStats {
    /// Passes over the segments.
    pub passes: u64,
    /// Those of them that read the page table through.
    pub scans: u64,
    /// Links that were trimmed out of chains at the cleaner's asking: no
    /// reader at the oldest pinned version or later needed them.
    pub links_trimmed: u64,
    /// The bytes their blocks take in their segments.
    pub bytes_trimmed: u64,
    /// Blocks that were copied to another segment and whose link was
    /// pointed at the copy.
    pub blocks_moved: u64,
    /// The bytes those copies take in their segment, frames included.
    pub bytes_moved: u64,
    /// Blocks that were copied and whose link was gone when it was to be
    /// pointed at the copy: a trim had taken it meanwhile. The copies are
    /// dead space.
    pub blocks_lost: u64,
    /// The bytes those copies take.
    pub bytes_lost: u64,
    /// Segments that were retired: the page table pointed into them no
    /// more.
    pub segments_retired: u64,
    /// Segments that were taken out of the store.
    pub segments_dropped: u64,
    /// The bytes of the blocks of those segments.
    pub bytes_dropped: u64,
    /// Segments that could not be read when their blocks were to be moved.
    /// They are left as they are until the store is opened again.
    pub segments_unreadable: u64,
    /// Table checkpoints the cleaner asked for and that were installed.
    pub checkpoints: u64,
    /// Table checkpoints it asked for that were put off or failed.
    pub checkpoints_missed: u64,
    /// Times the segment that write-back was filling was sealed at the
    /// cleaner's asking.
    pub segments_sealed: u64,
    /// Passes over the value log.
    pub value_passes: u64,
    /// Value-log files that a pass cleaned.
    pub value_files_cleaned: u64,
    /// Values that were moved: their owners hold the pointers to the
    /// copies.
    pub values_moved: u64,
    /// Values that were copied and not moved: their owner was gone, or the
    /// page could not take the rewrite then.
    pub values_left: u64,
    /// The bytes that were appended to the value log for copies.
    pub value_bytes_copied: u64,
    /// Value-log files that were taken out of the store.
    pub value_files_dropped: u64,
    /// Passes that ended with an error.
    pub errors: u64,
}

#[derive(Debug, Default)]
pub(super) struct Counters {
    pub(super) passes: AtomicU64,
    pub(super) scans: AtomicU64,
    pub(super) links_trimmed: AtomicU64,
    pub(super) bytes_trimmed: AtomicU64,
    pub(super) blocks_moved: AtomicU64,
    pub(super) bytes_moved: AtomicU64,
    pub(super) blocks_lost: AtomicU64,
    pub(super) bytes_lost: AtomicU64,
    pub(super) segments_retired: AtomicU64,
    pub(super) segments_dropped: AtomicU64,
    pub(super) bytes_dropped: AtomicU64,
    pub(super) segments_unreadable: AtomicU64,
    pub(super) checkpoints: AtomicU64,
    pub(super) checkpoints_missed: AtomicU64,
    pub(super) segments_sealed: AtomicU64,
    pub(super) value_passes: AtomicU64,
    pub(super) value_files_cleaned: AtomicU64,
    pub(super) values_moved: AtomicU64,
    pub(super) values_left: AtomicU64,
    pub(super) value_bytes_copied: AtomicU64,
    pub(super) value_files_dropped: AtomicU64,
    pub(super) errors: AtomicU64,
}

pub(super) fn count(counter: &AtomicU64, by: u64) {
    counter.fetch_add(by, Ordering::Relaxed);
}

impl Counters {
    pub(super) fn snapshot(&self) -> CleanerStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        CleanerStats {
            passes: read(&self.passes),
            scans: read(&self.scans),
            links_trimmed: read(&self.links_trimmed),
            bytes_trimmed: read(&self.bytes_trimmed),
            blocks_moved: read(&self.blocks_moved),
            bytes_moved: read(&self.bytes_moved),
            blocks_lost: read(&self.blocks_lost),
            bytes_lost: read(&self.bytes_lost),
            segments_retired: read(&self.segments_retired),
            segments_dropped: read(&self.segments_dropped),
            bytes_dropped: read(&self.bytes_dropped),
            segments_unreadable: read(&self.segments_unreadable),
            checkpoints: read(&self.checkpoints),
            checkpoints_missed: read(&self.checkpoints_missed),
            segments_sealed: read(&self.segments_sealed),
            value_passes: read(&self.value_passes),
            value_files_cleaned: read(&self.value_files_cleaned),
            values_moved: read(&self.values_moved),
            values_left: read(&self.values_left),
            value_bytes_copied: read(&self.value_bytes_copied),
            value_files_dropped: read(&self.value_files_dropped),
            errors: read(&self.errors),
        }
    }
}
