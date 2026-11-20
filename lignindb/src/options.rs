//! How a database is opened ([`Options`]).

use std::{fmt, sync::Arc, time::Duration};

use lignin_format::PageSize;
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    cleaner::CleanerConfig,
    flush::FlushConfig,
    recovery::{RecoveryOptions, DEFAULT_POOL_CAPACITY},
    wal::{LogOptions, SyncPolicy},
};

/// How a database is opened ([`Db::open`](crate::Db::open)).
///
/// Every field has a default, and [`Options::default`] opens a database as
/// `GroveDb::open(path)` needs it opened (P-01): it is created if there is
/// none, with pages of 32 KiB, a cache of 256 MiB, and no fsync by a
/// commit (DEC-04).
///
/// ```
/// use std::time::Duration;
///
/// use lignin_format::PageSize;
/// use lignin_store::wal::SyncPolicy;
/// use lignindb::Options;
///
/// // A checkpoint that is opened to be read: a small cache, and an error
/// // if the directory holds no database.
/// let options = Options {
///     create_if_missing: false,
///     cache_size: 16 << 20,
///     ..Options::default()
/// };
/// assert_eq!(options.page_size, None);
///
/// // A database whose commits are durable within a second.
/// let options = Options {
///     sync: SyncPolicy::Interval(Duration::from_secs(1)),
///     page_size: Some(PageSize::DEFAULT),
///     ..Options::default()
/// };
/// assert!(options.create_if_missing);
/// ```
#[derive(Clone)]
pub struct Options {
    /// Whether a directory that holds no database gets one. With `false`
    /// such a directory is refused
    /// ([`OpenError::NoDatabase`](crate::db::OpenError::NoDatabase)) and
    /// nothing is written to it.
    pub create_if_missing: bool,
    /// The size of the store's pages. `None` takes the size the database
    /// was made with, and [`PageSize::DEFAULT`] for a new one. A size that
    /// is given and is not the one the database was made with is refused
    /// ([`OpenError::PageSize`](crate::db::OpenError::PageSize)): a store
    /// keeps its page size for good (`segment-manifest.md` M17).
    pub page_size: Option<PageSize>,
    /// The memory of the buffer pool, in bytes: what the database caches of
    /// its pages. Each open database has a pool of its own, so a process
    /// that opens several, as a node does with its checkpoints, sizes each
    /// (P-06). The pool also holds what it cannot evict: the private
    /// copies of open write transactions and the page versions that
    /// write-back has not written yet.
    pub cache_size: usize,
    /// When a commit makes the log durable (DU1). With
    /// [`SyncPolicy::Never`], the default, a commit is durable once
    /// [`Db::flush`](crate::Db::flush) or a checkpoint has synced the log,
    /// and a crash before that lands on an earlier commit (DEC-04).
    pub sync: SyncPolicy,
    /// The length at which a log file is full and the log goes on in the
    /// next one. `None` is the store's default.
    pub log_file_len: Option<u64>,
    /// The thresholds of write-back: when a page is written as a base
    /// image, how much may wait to be written, and how much log is held
    /// before committers are slowed. `None` is [`FlushConfig::new`] of the
    /// page size.
    pub flush: Option<FlushConfig>,
    /// The policy of the cleaner, which moves what is live out of
    /// segments and value-log files that are mostly dead and gives the
    /// files back: the share of dead bytes it works towards in the
    /// segments ([`CleanerConfig::target_dead_percent`]) and in the value
    /// log ([`CleanerConfig::value_dead_percent`]), and its throttle, the
    /// bytes one pass may copy ([`CleanerConfig::max_pass_bytes`]) and the
    /// sleep between two passes ([`CleanerConfig::idle`]). `None` is
    /// [`CleanerConfig::new`], with which dead space stays below 20 bytes
    /// in a hundred under a steady load. None of it changes what a reader
    /// is given.
    pub cleaner: Option<CleanerConfig>,
    /// How long the reclaiming thread sleeps when nobody wakes it. A
    /// reclaim that a reader held back, and a step of the reclaimer that a
    /// write transaction held back, are tried again after this long at
    /// most. Write-back's loop has its own idle time, in [`Self::flush`].
    pub idle: Duration,
    /// The file system the database is in. Tests pass a simulated one
    /// (`lignin_io::sim`).
    pub vfs: Arc<dyn Vfs>,
}

impl Options {
    /// The default of [`Self::idle`]: 100 ms.
    pub const DEFAULT_IDLE: Duration = Duration::from_millis(100);

    /// What the store's recovery is told.
    pub(crate) fn recovery(&self) -> RecoveryOptions {
        let mut log = LogOptions {
            sync: self.sync,
            ..LogOptions::default()
        };
        if let Some(len) = self.log_file_len {
            log.target_file_len = len;
        }
        RecoveryOptions {
            pool_capacity: self.cache_size,
            log,
            flush: self.flush,
            ..RecoveryOptions::default()
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            create_if_missing: true,
            page_size: None,
            cache_size: DEFAULT_POOL_CAPACITY,
            sync: SyncPolicy::Never,
            log_file_len: None,
            flush: None,
            cleaner: None,
            idle: Self::DEFAULT_IDLE,
            vfs: Arc::new(StdVfs),
        }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("create_if_missing", &self.create_if_missing)
            .field("page_size", &self.page_size)
            .field("cache_size", &self.cache_size)
            .field("sync", &self.sync)
            .field("log_file_len", &self.log_file_len)
            .field("flush", &self.flush)
            .field("cleaner", &self.cleaner)
            .field("idle", &self.idle)
            .finish_non_exhaustive()
    }
}
