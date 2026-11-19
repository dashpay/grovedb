//! Recovery: how a store is opened (STORE-07).

mod error;
mod replay;

#[cfg(test)]
mod tests;

use lignin_format::CommitVersion;

pub use self::error::RecoveryError;
use self::replay::Replay;
use crate::{
    alloc::PageAllocator,
    cache::{BufferPool, PoolConfig},
    flush::{FlushConfig, FlushState},
    load::PageLoader,
    segment::{ManifestStore, SegmentStore},
    table::{CheckpointInfo, PageTable, TableCheckpoints},
    values::{FileCount, ValueStore},
    wal::{LogOptions, LogReader, LogStore, DEFAULT_READ_WINDOW_LEN},
};

/// The size of the buffer pool a store is recovered into unless
/// [`RecoveryOptions::pool_capacity`] says another: 256 MiB.
pub const DEFAULT_POOL_CAPACITY: usize = 256 << 20;

/// How a store is recovered, and what the session that follows is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryOptions {
    /// The size of the store's buffer pool in bytes
    /// ([`PoolConfig::capacity`]). Its frames are of the page size the
    /// manifest records.
    pub pool_capacity: usize,
    /// How the session's log is written.
    pub log: LogOptions,
    /// The thresholds of the session's write-back. `None` is
    /// [`FlushConfig::new`] of the store's page size.
    pub flush: Option<FlushConfig>,
    /// The most pages that replay keeps rebuilt in memory before it writes
    /// those that can be written to a segment. `None` is
    /// [`FlushConfig::max_unwritten`], the number of page versions that a
    /// committer may leave waiting for write-back.
    pub max_replay_pages: Option<usize>,
    /// How many bytes of the log are fetched at a time
    /// ([`LogReader::with_window`]).
    pub read_window: usize,
}

impl Default for RecoveryOptions {
    fn default() -> Self {
        Self {
            pool_capacity: DEFAULT_POOL_CAPACITY,
            log: LogOptions::default(),
            flush: None,
            max_replay_pages: None,
            read_window: DEFAULT_READ_WINDOW_LEN,
        }
    }
}

/// What a recovery did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    /// The commit version recovery landed on: the last commit whose group
    /// the log holds whole.
    pub landed: CommitVersion,
    /// The commit version the table checkpoint covers, or 0 if the
    /// manifest names none.
    pub covers: CommitVersion,
    /// The items of the log that were read.
    pub items: u64,
    /// The bytes those items take in the log.
    pub log_bytes: u64,
    /// The commits that were replayed: those after the one the checkpoint
    /// covers.
    pub commits: u64,
    /// The records of background changes that were looked at: those from
    /// the place of the checkpoint's sample on.
    pub changes: u64,
    /// The background changes among them that were made to their page,
    /// which did not have them yet.
    pub steps: u64,
    /// The changes that were dropped because the table checkpoint has no
    /// image of their page (`page-table.md` K11).
    pub changes_dropped: u64,
    /// The pages that replayed commits and page steps freed.
    pub pages_freed: u64,
    /// The pages that were written to a segment while the log was read,
    /// because there were more of them than replay keeps.
    pub pages_spilled: u64,
    /// The pages that were put into the buffer pool for write-back to
    /// write.
    pub pages_rebuilt: u64,
    /// The pending values of those pages, whose bytes were read from the
    /// log and are with their frames.
    pub pending_values: u64,
    /// The releases of value pointers that a count of the value log did
    /// not include. The file of each is kept from now on, whatever its
    /// count says ([`FileCount::miscounted`]).
    pub miscounts: u64,
}

/// A store that was recovered: every part of it, open, as of one committed
/// version.
#[derive(Debug)]
pub struct Recovered {
    /// The manifest, which names every file of the earlier sessions closed
    /// and the first log file of this one.
    pub manifest: ManifestStore,
    /// The buffer pool. It holds the pages that replay rebuilt, each as
    /// the current version of its page, for write-back to write.
    pub pool: BufferPool,
    /// The page table: the chains of the checkpoint, the free links of the
    /// pages that replayed commits freed, and the links of the pages that
    /// were written while the log was read.
    pub table: PageTable,
    /// The segments.
    pub segments: SegmentStore,
    /// The value log, with the counts of live values of the version
    /// recovery landed on.
    pub values: ValueStore,
    /// The log, turned to the first file of this session.
    pub log: LogStore,
    /// The table checkpoints.
    pub checkpoints: TableCheckpoints,
    /// The page ids, as that version has them.
    pub allocator: PageAllocator,
    /// The state of the session's write-back. It keeps the log for the
    /// checkpoint in force and knows what of the replayed commits no block
    /// holds.
    pub flush: FlushState,
    /// The table checkpoint the manifest names, if it names one.
    pub checkpoint: Option<CheckpointInfo>,
    /// What the recovery did.
    pub report: RecoveryReport,
}

impl Default for RecoveryReport {
    fn default() -> Self {
        Self {
            landed: CommitVersion::EMPTY,
            covers: CommitVersion::EMPTY,
            items: 0,
            log_bytes: 0,
            commits: 0,
            changes: 0,
            steps: 0,
            changes_dropped: 0,
            pages_freed: 0,
            pages_spilled: 0,
            pages_rebuilt: 0,
            pending_values: 0,
            miscounts: 0,
        }
    }
}

impl Recovered {
    /// The commit version the store was recovered to: the first commit of
    /// the session takes the version after it.
    pub fn landed(&self) -> CommitVersion {
        self.report.landed
    }
}

/// Recovers the store whose manifest `manifest` is.
pub fn recover(
    mut manifest: ManifestStore,
    options: &RecoveryOptions,
) -> Result<Recovered, RecoveryError> {
    let named = manifest.manifest()?.clone();
    let vfs = manifest.vfs().clone();
    let dir = manifest.dir().to_path_buf();
    let size = named.page_size();

    let (table, checkpoint) = PageTable::load(&*vfs, &dir, &named)?;
    let segments = SegmentStore::open(&manifest)?;
    let sample: Vec<FileCount> = checkpoint
        .iter()
        .flat_map(|info| &info.sample().counts)
        .map(|count| FileCount {
            file: count.file,
            values: count.values,
            bytes: count.bytes,
            miscounted: count.miscounted,
        })
        .collect();
    let counts = ValueStore::recover(&manifest, &sample)?;
    let checkpoints = TableCheckpoints::open(&manifest)?;
    let pool = BufferPool::new(PoolConfig {
        page_size: size,
        capacity: options.pool_capacity,
    })?;
    let flush = FlushState::new(options.flush.unwrap_or(FlushConfig::new(size)));
    let covers = checkpoint
        .as_ref()
        .map_or(CommitVersion::EMPTY, CheckpointInfo::covers);
    let place = checkpoint.as_ref().and_then(|info| info.sample().place);
    let max_pages = options
        .max_replay_pages
        .unwrap_or(flush.config().max_unwritten);

    let (values, allocator, log, report) = {
        let loader = PageLoader::new(&pool, &table, &segments, size)
            .map_err(|source| RecoveryError::Loader { source })?;
        let mut replay = Replay::new(
            &pool, &table, &segments, &flush, loader, size, counts, covers, place, max_pages,
        );
        let mut reader = LogReader::new(vfs.clone(), &dir, &named).with_window(options.read_window);
        while let Some(replayed) = reader.next_item(|read| replay.item(read, &mut manifest))? {
            replayed?;
        }
        let end = reader.into_end()?;
        let landed = replay.ended(&end)?;

        let log = LogStore::open(&manifest, &end, options.log)?;
        flush.adopt(&log, checkpoint.as_ref())?;
        replay.install_rebuilt(&log)?;
        replay.close()?;
        let (counts, ids, report) = replay.into_parts();
        let values = counts.finish()?;
        let allocator = ids.finish(&table, landed)?;
        (values, allocator, log, report)
    };

    segments.commit(&mut manifest)?;
    values.commit(&mut manifest)?;
    log.roll(&mut manifest)?;
    values.mark_durable(values.applied())?;

    Ok(Recovered {
        manifest,
        pool,
        table,
        segments,
        values,
        log,
        checkpoints,
        allocator,
        flush,
        checkpoint,
        report,
    })
}
