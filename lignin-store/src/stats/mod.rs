//! Stats: one struct with every count the store keeps (STORE-11).
//!
//! Each part of the store counts what it does where it does it: the
//! [log](crate::wal) counts what it appends ([`LogStore::appended`]), the
//! [buffer pool](crate::cache) its hits and misses in each thread's lane
//! ([`BufferPool::stats`]), and [write-back](crate::flush) the records it
//! was told of, the blocks it wrote and the waits it imposed
//! ([`FlushState::stats`]). What only the layers above see, the hashes
//! the tree computes and the lookups by key it makes, they count in a
//! [`Stats`], which whoever opens the store makes beside the
//! [`FlushState`] and hands to them. [`Stats::read`] gathers all of it
//! into one [`StoreStats`].
//!
//! # The prediction table
//!
//! [`StoreStats`] has a field for every row of the plan's prediction table
//! that counts what the engine did, and the benchmark suite
//! (`lignin_testkit::bench::Counters`) takes each of its engine counts from
//! one of them:
//!
//! | Field | What it counts | Counted by |
//! | --- | --- | --- |
//! | [`log_bytes`](StoreStats::log_bytes) | bytes appended to the log: commit groups, page steps and checkpoint marks, every record's frame included | [`LogStore::appended`] |
//! | [`log_groups`](StoreStats::log_groups) | commit groups appended | [`LogStore::appended`] |
//! | [`node_records`](StoreStats::node_records), [`node_record_bytes`](StoreStats::node_record_bytes) | the page deltas of tree pages the groups carry, and their bytes plus those of the out-of-line values the groups carry | [`WriteBack::committed`] |
//! | [`other_records`](StoreStats::other_records), [`other_record_bytes`](StoreStats::other_record_bytes) | the changes of flat pages the groups carry: the flat keyspace | [`WriteBack::committed`] |
//! | [`flush_bytes`](StoreStats::flush_bytes) | bytes write-back wrote: blocks as stored in segments, values as stored in the value log, the copies of regrouped values among them, table-checkpoint files | [`FlushState::stats`] |
//! | [`cleaner_bytes`](StoreStats::cleaner_bytes) | bytes the cleaners wrote when they moved live blocks and values | [`Stats::count_cleaner_bytes`] |
//! | [`page_reads`](StoreStats::page_reads) | page versions that were read from storage: each a base image and its runs, read in full for a load | [`BufferPool::stats`] (loads) |
//! | [`cache_hits`](StoreStats::cache_hits) | page versions the pool held | [`BufferPool::stats`] (hits) |
//! | [`hashes`](StoreStats::hashes) | hashes the tree computed | [`Stats::count_hashes`] |
//! | [`keyed_lookups`](StoreStats::keyed_lookups) | searches by key that went on below a tree's root | [`Stats::count_keyed_lookups`] |
//! | [`value_reads`](StoreStats::value_reads), [`value_read_bytes`](StoreStats::value_read_bytes) | values read from the value log by their pointers, and their bytes: by readers, write transactions and the invariant checker; not the cleaner's copies | [`ValueStore::reads`] |
//! | [`block_reads`](StoreStats::block_reads), [`block_read_bytes`](StoreStats::block_read_bytes) | blocks read from segments by their locations, and their bytes as stored: the base image and the delta runs of every page that is loaded, and what write-back reads of a page it builds again; each is one read of the file layer | [`SegmentStore::reads`] |
//!
//! Every count is a running total since the store was opened, and only
//! goes up. A benchmark takes two readings and subtracts.
//!
//! # What an operator watches
//!
//! [`StoreStats::watch`] is what can go wrong slowly while the store is in
//! use ([`Watch`]): the log that write-back holds, and what holds it
//! ([`Watch::log_held_by`]); pages that cannot be written; table
//! checkpoints put off; how often and how long committers waited for room;
//! segments whose sync failed. The log that is held is what a recovery
//! reads and what no file can be dropped from, so it is the number to
//! alert on, and its holders say why it grows.
//!
//! # Tracing
//!
//! The store logs nothing and depends on no tracing crate (DEC-07). Its
//! events are counted here, and every failure is returned to the caller:
//! the background loop passes the error of each turn that fails to the
//! closure given to [`WriteBack::run`]. [`StoreStats`] prints as a short
//! report ([`std::fmt::Display`]) for a log line of the caller's.
//!
//! ```
//! use lignin_store::stats::Stats;
//!
//! let stats = Stats::new();
//! stats.count_hashes(3);
//! stats.count_keyed_lookups(1);
//! let write = stats.write_opened();
//! assert_eq!(stats.open_writes().0, 1);
//! drop(write);
//! assert_eq!(stats.open_writes(), (0, None));
//! assert_eq!((stats.hashes(), stats.keyed_lookups()), (3, 1));
//! ```
//!
//! [`LogStore::appended`]: crate::wal::LogStore::appended
//! [`BufferPool::stats`]: crate::cache::BufferPool::stats
//! [`FlushState::stats`]: crate::flush::FlushState::stats
//! [`FlushState`]: crate::flush::FlushState
//! [`WriteBack::committed`]: crate::flush::WriteBack::committed
//! [`WriteBack::run`]: crate::flush::WriteBack::run
//! [`ValueStore::reads`]: crate::values::ValueStore::reads
//! [`SegmentStore::reads`]: crate::segment::SegmentStore::reads

mod read;
mod striped;

#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, PoisonError,
    },
    time::{Duration, Instant},
};

pub use read::{Holder, StoreStats, Watch};
use striped::Striped;

/// The counts the layers above the store keep in it, and the write
/// transactions that are open. One for each open store, made beside its
/// [`FlushState`](crate::flush::FlushState).
///
/// The hashes and lookups are counted by each thread in a counter of its
/// own, so threads that hash at once write no cache line in common.
#[derive(Debug, Default)]
pub struct Stats {
    hashes: Striped,
    keyed_lookups: Striped,
    cleaner_bytes: AtomicU64,
    writes: Mutex<OpenWrites>,
}

#[derive(Debug, Default)]
struct OpenWrites {
    next: u64,
    began: BTreeMap<u64, Instant>,
}

/// A write transaction that is open, from [`Stats::write_opened`]. It
/// counts as open until it is dropped.
#[derive(Debug)]
#[must_use = "the transaction counts as open only while this lives"]
pub struct OpenWrite<'s> {
    stats: &'s Stats,
    id: u64,
}

impl Drop for OpenWrite<'_> {
    fn drop(&mut self) {
        self.stats.writes().began.remove(&self.id);
    }
}

impl Stats {
    /// Counts of nothing, with no write transaction open.
    pub fn new() -> Self {
        Self::default()
    }

    fn writes(&self) -> std::sync::MutexGuard<'_, OpenWrites> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The tree computed `hashes` hashes: of values, of keys and values,
    /// and of nodes.
    pub fn count_hashes(&self, hashes: u64) {
        self.hashes.add(hashes);
    }

    /// The tree searched for `lookups` keys from a tree's root, and each
    /// search went on below the root: the key chose a link there.
    pub fn count_keyed_lookups(&self, lookups: u64) {
        self.keyed_lookups.add(lookups);
    }

    /// A cleaner wrote `bytes` bytes, as stored, to move live blocks or
    /// values out of files it empties.
    pub fn count_cleaner_bytes(&self, bytes: u64) {
        self.cleaner_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// The hashes counted so far.
    pub fn hashes(&self) -> u64 {
        self.hashes.sum()
    }

    /// The lookups by key counted so far.
    pub fn keyed_lookups(&self) -> u64 {
        self.keyed_lookups.sum()
    }

    /// The bytes the cleaners wrote so far.
    pub fn cleaner_bytes(&self) -> u64 {
        self.cleaner_bytes.load(Ordering::Relaxed)
    }

    /// A write transaction began. It holds the log for as long as it is
    /// open (D-091), so the transaction layer calls this when it begins
    /// one and keeps what it returns until the transaction has committed
    /// or rolled back.
    pub fn write_opened(&self) -> OpenWrite<'_> {
        let mut writes = self.writes();
        let id = writes.next;
        writes.next += 1;
        writes.began.insert(id, Instant::now());
        OpenWrite { stats: self, id }
    }

    /// How many write transactions are open, and how long the oldest of
    /// them has been.
    pub fn open_writes(&self) -> (usize, Option<Duration>) {
        let writes = self.writes();
        let oldest = writes.began.values().next().map(Instant::elapsed);
        (writes.began.len(), oldest)
    }
}
