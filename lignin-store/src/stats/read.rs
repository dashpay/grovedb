//! One reading of every count: [`StoreStats`].

use std::{fmt, time::Duration};

use super::Stats;
use crate::{cache::PoolStats, flush::FlushStats, flush::WriteBack, wal::LogAppends};

/// Every count of one open store at one moment ([`Stats::read`]). See the
/// [module documentation](super) for where each comes from.
///
/// The counts are read one after the other while other threads work, so
/// they are of slightly different moments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// Bytes appended to the log, frames included: commit groups, page
    /// steps and checkpoint marks.
    pub log_bytes: u64,
    /// Commit groups appended to the log.
    pub log_groups: u64,
    /// Page deltas of tree pages that the commit groups carry: the cells
    /// and hashes the commits changed.
    pub node_records: u64,
    /// Their bytes, and the bytes of the out-of-line values the groups
    /// carry.
    pub node_record_bytes: u64,
    /// Changes of flat pages that the commit groups carry.
    pub other_records: u64,
    /// Their bytes.
    pub other_record_bytes: u64,
    /// Bytes write-back wrote: blocks and values as stored, the copies
    /// of regrouped values among them, and table-checkpoint files.
    pub flush_bytes: u64,
    /// Bytes the cleaners wrote.
    pub cleaner_bytes: u64,
    /// Page versions read from storage because the pool did not hold them.
    pub page_reads: u64,
    /// Page versions the pool held when they were asked for.
    pub cache_hits: u64,
    /// Hashes the tree computed.
    pub hashes: u64,
    /// Searches by key from a tree's root that went on below the root. A
    /// node that is reached by its address, and the root that a search
    /// begins at, are none.
    pub keyed_lookups: u64,
    /// Values read from the value log by their pointers: by readers,
    /// write transactions and the invariant checker, and not by the
    /// cleaner.
    pub value_reads: u64,
    /// Their bytes.
    pub value_read_bytes: u64,
    /// What an operator watches.
    pub watch: Watch,
    /// The log's counts, by kind of item.
    pub log: LogAppends,
    /// Write-back's counts.
    pub flush: FlushStats,
    /// The pool's counts.
    pub pool: PoolStats,
}

/// What can go wrong slowly while a store is in use. The counts that end
/// in `_now` or that say how many things are, rather than how many things
/// happened, are of the moment of the reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Watch {
    /// Bytes of log that write-back holds: from the group of the oldest
    /// commit that no table checkpoint covers. A recovery reads them, and
    /// no log file is dropped from them.
    pub log_held: u64,
    /// Page versions that wait to be written back.
    pub unwritten: usize,
    /// Bytes of pending values that frames hold.
    pub pending_value_bytes: usize,
    /// Write transactions that are open.
    pub open_writes: usize,
    /// How long the oldest of them has been open.
    pub oldest_write: Option<Duration>,
    /// Pages the last pass could not write because a write transaction
    /// holds a private copy.
    pub blocked_pages: usize,
    /// Pages the last pass could not write because of an error about the
    /// page itself. They hold the log until the store is opened again.
    pub failing_pages: usize,
    /// Attempts at a table checkpoint that were put off, in all.
    pub checkpoints_put_off: u64,
    /// Attempts at a table checkpoint that ended with an error, in all.
    pub checkpoints_failed: u64,
    /// Waits for room: commits that found write-back over its bounds.
    pub room_waits: u64,
    /// Those of them that ended with write-back still over its bounds.
    pub room_stalls: u64,
    /// The time those waits took, in all.
    pub room_waited: Duration,
    /// Segments whose sync failed, in all.
    pub segment_syncs_failed: u64,
    /// Whether a segment's sync failed in this session, so that no table
    /// checkpoint is installed until the store is opened again.
    pub segments_not_durable: bool,
    /// Turns of write-back that have begun
    /// ([`FlushState::turns_begun`](crate::flush::FlushState::turns_begun)).
    pub turns_begun: u64,
    /// The number of the turn of write-back that ended last
    /// ([`FlushState::turns`](crate::flush::FlushState::turns)). Whoever
    /// waits for write-back to have seen a commit reads
    /// [`Self::turns_begun`] once the commit is published and waits for
    /// this to pass it.
    pub turns_ended: u64,
}

/// Why the log is held ([`Watch::log_held_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    /// Page versions wait to be written back.
    Unwritten {
        /// How many.
        pages: usize,
    },
    /// Frames hold pending values that wait for their place.
    PendingValues {
        /// Their bytes.
        bytes: usize,
    },
    /// Write transactions are open.
    OpenWrites {
        /// How many.
        count: usize,
        /// How long the oldest has been open.
        oldest: Duration,
    },
    /// Write transactions hold private copies of pages that wait.
    BlockedPages {
        /// How many pages.
        pages: usize,
    },
    /// Pages cannot be written because of an error about them.
    FailingPages {
        /// How many.
        pages: usize,
    },
    /// A segment's sync failed, and no checkpoint is installed until the
    /// store is opened again.
    SegmentsNotDurable,
    /// Nothing stands in the way: the log waits for the next table
    /// checkpoint.
    NextCheckpoint,
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unwritten { pages } => write!(f, "{pages} page versions to write back"),
            Self::PendingValues { bytes } => write!(f, "{bytes} bytes of pending values"),
            Self::OpenWrites { count, oldest } => write!(
                f,
                "{count} open write transactions, the oldest open for {oldest:?}"
            ),
            Self::BlockedPages { pages } => {
                write!(f, "{pages} pages held back by write transactions")
            }
            Self::FailingPages { pages } => write!(f, "{pages} pages that cannot be written"),
            Self::SegmentsNotDurable => {
                f.write_str("a segment sync failed: no checkpoint until reopened")
            }
            Self::NextCheckpoint => f.write_str("the next table checkpoint"),
        }
    }
}

impl Watch {
    /// What holds the log, worst first: the things that keep a table
    /// checkpoint from covering it. Empty when no log is held.
    pub fn log_held_by(&self) -> Vec<Holder> {
        if self.log_held == 0 {
            return Vec::new();
        }
        let mut holders = Vec::new();
        if self.segments_not_durable {
            holders.push(Holder::SegmentsNotDurable);
        }
        if self.failing_pages > 0 {
            holders.push(Holder::FailingPages {
                pages: self.failing_pages,
            });
        }
        if self.blocked_pages > 0 {
            holders.push(Holder::BlockedPages {
                pages: self.blocked_pages,
            });
        }
        if let (count @ 1.., Some(oldest)) = (self.open_writes, self.oldest_write) {
            holders.push(Holder::OpenWrites { count, oldest });
        }
        if self.pending_value_bytes > 0 {
            holders.push(Holder::PendingValues {
                bytes: self.pending_value_bytes,
            });
        }
        if self.unwritten > 0 {
            holders.push(Holder::Unwritten {
                pages: self.unwritten,
            });
        }
        if holders.is_empty() {
            holders.push(Holder::NextCheckpoint);
        }
        holders
    }
}

impl Stats {
    /// Reads every count of the store that `write_back` works on, with the
    /// counts kept here.
    pub fn read(&self, write_back: &WriteBack<'_>) -> StoreStats {
        let stores = write_back.stores();
        let state = write_back.state();
        let log = stores.log.appended();
        let flush = state.stats();
        let pool = stores.pool.stats();
        let (open_writes, oldest_write) = self.open_writes();
        let (value_reads, value_read_bytes) = stores.values.reads();
        let watch = Watch {
            log_held: write_back.log_held(),
            unwritten: state.unwritten(),
            pending_value_bytes: stores.pool.attached_bytes(),
            open_writes,
            oldest_write,
            blocked_pages: state.blocked(),
            failing_pages: state.failing(),
            checkpoints_put_off: flush.checkpoints_put_off,
            checkpoints_failed: flush.checkpoints_failed,
            room_waits: flush.room_waits,
            room_stalls: flush.room_stalls,
            room_waited: flush.room_waited,
            segment_syncs_failed: flush.segment_syncs_failed,
            segments_not_durable: state.segments_not_durable(),
            turns_begun: state.turns_begun(),
            turns_ended: state.turns(),
        };
        StoreStats {
            log_bytes: log.bytes(),
            log_groups: log.groups,
            node_records: flush.node_records,
            node_record_bytes: flush.node_record_bytes,
            other_records: flush.other_records,
            other_record_bytes: flush.other_record_bytes,
            flush_bytes: flush.stored_bytes
                + flush.value_stored_bytes
                + flush.regroup_stored_bytes
                + flush.checkpoint_bytes,
            cleaner_bytes: self.cleaner_bytes(),
            page_reads: pool.misses,
            cache_hits: pool.hits,
            hashes: self.hashes(),
            keyed_lookups: self.keyed_lookups(),
            value_reads,
            value_read_bytes,
            watch,
            log,
            flush,
            pool,
        }
    }
}

impl fmt::Display for StoreStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let watch = &self.watch;
        writeln!(
            f,
            "log: {} bytes in {} groups; {} node records ({} bytes), {} other records ({} bytes)",
            self.log_bytes,
            self.log_groups,
            self.node_records,
            self.node_record_bytes,
            self.other_records,
            self.other_record_bytes,
        )?;
        writeln!(
            f,
            "written back: {} bytes; cleaned: {} bytes; pages read: {}, cache hits: {}; \
             hashes: {}; keyed lookups: {}",
            self.flush_bytes,
            self.cleaner_bytes,
            self.page_reads,
            self.cache_hits,
            self.hashes,
            self.keyed_lookups,
        )?;
        writeln!(
            f,
            "values read from the value log: {} ({} bytes)",
            self.value_reads, self.value_read_bytes,
        )?;
        write!(f, "log held: {} bytes", watch.log_held)?;
        for (at, holder) in watch.log_held_by().iter().enumerate() {
            let sep = if at == 0 { " by " } else { ", " };
            write!(f, "{sep}{holder}")?;
        }
        writeln!(f)?;
        write!(
            f,
            "checkpoints put off: {}, failed: {}; waits for room: {} ({} stalled, {:?} in \
             all); segment syncs failed: {}",
            watch.checkpoints_put_off,
            watch.checkpoints_failed,
            watch.room_waits,
            watch.room_stalls,
            watch.room_waited,
            watch.segment_syncs_failed,
        )
    }
}
