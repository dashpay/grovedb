//! How the cleaner is set, and the counts of what it has done.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::flush::DEFAULT_SEGMENT_LEN;

/// The share of dead bytes, in a hundred, above which the cleaner moves
/// blocks by default.
pub const DEFAULT_TARGET_DEAD_PERCENT: u8 = 14;

/// The most bytes of blocks one pass moves by default: 8 MiB.
pub const DEFAULT_PASS_BYTES: u64 = 8 << 20;

/// The share of dead bytes of the value log, in a hundred, above which the
/// cleaner cleans value-log files by default.
pub const DEFAULT_VALUE_DEAD_PERCENT: u8 = 18;

/// The share of a value-log file's own bytes, in a hundred, that must be
/// dead for the file to be cleaned by default, while the value log is
/// under its ceiling.
pub const DEFAULT_VALUE_MIN_FILE_DEAD_PERCENT: u8 = 40;

/// The share of dead bytes of the value log, in a hundred, over which the
/// cleaner cleans the file most worth it whatever its own share, by
/// default.
pub const DEFAULT_VALUE_CEILING_DEAD_PERCENT: u8 = 20;

/// Five classes of the share of a file's bytes that are dead when a
/// cleaner takes it: from 0 to 20 in a hundred, from 20 to 40, and so on,
/// the last from 80 to all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeadClass(usize);

impl DeadClass {
    /// The class of a file of `len` bytes of which `dead` are dead.
    pub(super) fn of(dead: u64, len: u64) -> Self {
        let percent = u128::from(dead) * 100 / u128::from(len.max(1));
        Self((percent / 20).min(4) as usize)
    }

    /// Its place in the arrays of [`CleanerStats`].
    pub(super) fn index(self) -> usize {
        self.0
    }
}

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
    /// on. The lower it is, the more bytes the cleaner writes for each
    /// byte it gives back.
    pub target_dead_percent: u8,
    /// The most that waits for a table checkpoint: the share of the bytes
    /// of all blocks, in a hundred, that the segments may hold which the
    /// page table points into no more and which only the table checkpoint
    /// in force still keeps. A checkpoint is asked for when they hold
    /// that much, and when the cleaner would move blocks and has no room
    /// for the copies under it: a copy takes room until its segment is
    /// deleted. While no checkpoint can be taken, only a segment that
    /// still fits under it is moved.
    pub checkpoint_dead_percent: u8,
    /// A segment that is being filled is sealed when its dead blocks are
    /// this share of the bytes of all blocks, in a hundred, and a
    /// value-log file that is being filled is closed when its dead values
    /// are this share of the bytes of all values. A file is not cleaned
    /// while it is written.
    pub seal_dead_percent: u8,
    /// A file that is being filled is also closed when what it holds is
    /// this share of the bytes of all blocks, or of all values, in a
    /// hundred. Until a file is deleted the store holds what was moved
    /// out of it twice, so no file is to be a large part of the store.
    pub max_segment_percent: u8,
    /// A file that is being filled is not closed for the cleaner before
    /// what it holds takes this many bytes: a store that is small against
    /// it is not held to the target.
    pub min_segment_len: u64,
    /// The throttle: the most bytes of blocks, as they are stored, that
    /// one pass over the segments copies, and one block more: a pass
    /// stops moving once it has written that many. With [`Self::idle`] it
    /// bounds the copies of blocks that the cleaner writes in a second. A
    /// pass over the value log cleans its first file whole, and after it
    /// the files that are nearly empty while their live bytes stay within
    /// this.
    pub max_pass_bytes: u64,
    /// The length at which the segment that takes the moved blocks is
    /// sealed and the next one begun.
    pub segment_len: u64,
    /// The share of dead bytes among the bytes of all values in the value
    /// log, in a hundred, above which a pass cleans value-log files: the
    /// one with the largest share of dead bytes, if that share is at least
    /// [`Self::value_min_file_dead_percent`] (or one byte in a hundred over
    /// [`Self::value_ceiling_dead_percent`]), and after it the files that
    /// are nearly empty, within [`Self::max_pass_bytes`]. With 0 here, a
    /// file is cleaned for any dead byte.
    pub value_dead_percent: u8,
    /// The share of a value-log file's own bytes, in a hundred, that must
    /// be dead for a pass to clean it while the value log is above its
    /// target and not over its ceiling; one byte in a hundred at least,
    /// whatever is set here, unless the target is 0. Each live value of a
    /// file costs a copy and a page step, so a file that is mostly live is
    /// not worth the space it gives back.
    pub value_min_file_dead_percent: u8,
    /// The share of dead bytes of the value log, in a hundred, over which a
    /// pass cleans the file with the largest share of dead bytes whatever
    /// that share is, from one byte in a hundred. It is not a bound a pass
    /// keeps: a pass cleans one such file, and while it is over, each pass
    /// takes the next. Where the deaths are spread over every file, no
    /// file may reach the floor, and the value log comes to rest at this
    /// share once the cleaner has caught up, with what the commits free
    /// faster than the passes clean on top. Below
    /// [`Self::value_dead_percent`] it is over whenever the target is.
    pub value_ceiling_dead_percent: u8,
    /// How long the background loop sleeps between two passes when nobody
    /// wakes it.
    pub idle: Duration,
}

impl CleanerConfig {
    /// The defaults: work towards 14 dead bytes in a hundred in segments;
    /// in the value log, clean files at least 40 in a hundred dead once 18
    /// in a hundred of its bytes are, and the file most worth it whatever
    /// its share while over 20; let at most 4 in a hundred wait for a table
    /// checkpoint; close a file that is being filled when it holds 1 in a
    /// hundred of the store, or when 2 in a hundred of the store are dead
    /// in it, and none before it holds 64 KiB; copy 8 MiB of blocks in a
    /// pass, into segments of 64 MiB; sleep 100 ms between passes.
    ///
    /// With these, the dead space of a store stays below 20 in a hundred
    /// under a steady load of commits, as read after a pass of the
    /// cleaner: in the segments, 14 that the cleaner leaves, and at most 4
    /// more in what was emptied and waits to be deleted. The value log
    /// comes to rest at 20, the ceiling, where its deaths are spread
    /// evenly over its files, and between 18 and 20 where files reach the
    /// floor before the log reaches the ceiling. That is for a store with no
    /// reader on an old version, no operation that runs for long, and
    /// blocks that are large against [`Self::min_segment_len`]. What a
    /// burst of commits kills faster than [`Self::max_pass_bytes`] and
    /// [`Self::idle`] let the cleaner move shows until the cleaner has
    /// caught up.
    pub const fn new() -> Self {
        Self {
            target_dead_percent: DEFAULT_TARGET_DEAD_PERCENT,
            checkpoint_dead_percent: 4,
            seal_dead_percent: 2,
            max_segment_percent: 1,
            min_segment_len: 64 << 10,
            max_pass_bytes: DEFAULT_PASS_BYTES,
            segment_len: DEFAULT_SEGMENT_LEN,
            value_dead_percent: DEFAULT_VALUE_DEAD_PERCENT,
            value_min_file_dead_percent: DEFAULT_VALUE_MIN_FILE_DEAD_PERCENT,
            value_ceiling_dead_percent: DEFAULT_VALUE_CEILING_DEAD_PERCENT,
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
    /// Reads of the whole page table: one for each round of a pass, and
    /// none for a pass that found the store as the pass before left it.
    pub scans: u64,
    /// Links to blocks that were trimmed out of chains at the cleaner's
    /// asking: no reader at the oldest pinned version or later needed
    /// them.
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
    /// Segments whose blocks a pass had listed to move and that were out
    /// of the store when the pass came to them, with no link left to the
    /// blocks it asked for: a second cleaner of the same store had
    /// dropped them. With one cleaner to a store it stays 0.
    pub segments_gone: u64,
    /// Table checkpoints the cleaner asked for and that were installed.
    pub checkpoints: u64,
    /// Table checkpoints it asked for that were put off or failed.
    pub checkpoints_missed: u64,
    /// Segments that were being filled and were sealed because too much
    /// of them was dead.
    pub segments_sealed: u64,
    /// Value-log files that were being filled and were closed for the
    /// same reason.
    pub value_files_closed: u64,
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
    /// The value-log files that were cleaned, by the share of their bytes
    /// that was dead when a pass took them: from 0 to 20 in a hundred, from
    /// 20 to 40, from 40 to 60, from 60 to 80, and from 80.
    pub value_files_by_dead: [u64; 5],
    /// The bytes appended for the copies of each class of
    /// [`Self::value_files_by_dead`].
    pub value_bytes_by_dead: [u64; 5],
    /// Passes over the value log in which the dead space was above its
    /// target and every file that could be cleaned was set aside, because
    /// its last cleaning moved nothing while write transactions held the
    /// pages of its values.
    pub value_passes_all_busy: u64,
    /// The segments that blocks were moved out of, by the share of their
    /// bytes that was dead when the pass chose them, in the classes of
    /// [`Self::value_files_by_dead`]. A segment that two passes move blocks
    /// out of is counted by each.
    pub segments_by_dead: [u64; 5],
    /// The bytes of the copies of blocks out of each class of
    /// [`Self::segments_by_dead`], those whose link was gone with them.
    pub segment_bytes_by_dead: [u64; 5],
    /// Value-log files that were taken out of the store.
    pub value_files_dropped: u64,
    /// Value-log files for the cleaner's own copies that it gave up
    /// after a cleaning into the file failed, whether a copy had been
    /// written to it by then or not. Such a file is closed, where it
    /// ends or where it was last durable.
    pub value_files_given_up: u64,
    /// Passes that ended with an error.
    pub errors: u64,
    /// Segments that the cleaner moved blocks into and of which a sync
    /// has failed, whoever made that sync: the cleaner's own, after an
    /// append or a sealing that failed, or the sync that a table
    /// checkpoint makes of every segment that is being written. Each is
    /// counted once, by the segment store's own record of it
    /// ([`SyncRecord`](crate::segment::SyncRecord)), so the count is right
    /// before the cleaner has met the failure itself.
    pub segments_not_durable: u64,
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
    pub(super) segments_gone: AtomicU64,
    pub(super) checkpoints: AtomicU64,
    pub(super) checkpoints_missed: AtomicU64,
    pub(super) segments_sealed: AtomicU64,
    pub(super) value_files_closed: AtomicU64,
    pub(super) value_passes: AtomicU64,
    pub(super) value_files_cleaned: AtomicU64,
    pub(super) values_moved: AtomicU64,
    pub(super) values_left: AtomicU64,
    pub(super) value_bytes_copied: AtomicU64,
    pub(super) value_files_by_dead: [AtomicU64; 5],
    pub(super) value_bytes_by_dead: [AtomicU64; 5],
    pub(super) value_passes_all_busy: AtomicU64,
    pub(super) segments_by_dead: [AtomicU64; 5],
    pub(super) segment_bytes_by_dead: [AtomicU64; 5],
    pub(super) value_files_dropped: AtomicU64,
    pub(super) value_files_given_up: AtomicU64,
    pub(super) errors: AtomicU64,
}

pub(super) fn count(counter: &AtomicU64, by: u64) {
    counter.fetch_add(by, Ordering::Relaxed);
}

impl Counters {
    pub(super) fn snapshot(&self) -> CleanerStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let classes = |counters: &[AtomicU64; 5]| counters.each_ref().map(read);
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
            segments_gone: read(&self.segments_gone),
            checkpoints: read(&self.checkpoints),
            checkpoints_missed: read(&self.checkpoints_missed),
            segments_sealed: read(&self.segments_sealed),
            value_files_closed: read(&self.value_files_closed),
            value_passes: read(&self.value_passes),
            value_files_cleaned: read(&self.value_files_cleaned),
            values_moved: read(&self.values_moved),
            values_left: read(&self.values_left),
            value_bytes_copied: read(&self.value_bytes_copied),
            value_files_by_dead: classes(&self.value_files_by_dead),
            value_bytes_by_dead: classes(&self.value_bytes_by_dead),
            value_passes_all_busy: read(&self.value_passes_all_busy),
            segments_by_dead: classes(&self.segments_by_dead),
            segment_bytes_by_dead: classes(&self.segment_bytes_by_dead),
            value_files_dropped: read(&self.value_files_dropped),
            value_files_given_up: read(&self.value_files_given_up),
            errors: read(&self.errors),
            segments_not_durable: 0,
        }
    }
}
