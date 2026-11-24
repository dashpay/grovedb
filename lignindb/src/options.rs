//! How a database is opened ([`Options`]).

use std::{fmt, sync::Arc, time::Duration};

use lignin_format::PageSize;
use lignin_io::{StdVfs, Vfs};
/// When a commit makes the log durable: the type of [`Options::sync`],
/// which is the store's.
pub use lignin_store::wal::SyncPolicy;
use lignin_store::{
    cleaner::CleanerConfig,
    flush::FlushConfig,
    recovery::{RecoveryOptions, DEFAULT_POOL_CAPACITY},
    wal::LogOptions,
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
    /// files back: what [`CleanerConfig`] says of each of its fields.
    /// `None` is [`CleanerConfig::new`]. None of it changes what a reader
    /// is given.
    pub cleaner: Option<CleanerConfig>,
    /// How long the reclaiming thread sleeps when nobody wakes it. A
    /// reclaim that a reader held back, and a step of the reclaimer that a
    /// write transaction held back, are tried again after this long at
    /// most. Write-back's loop has its own idle time, in [`Self::flush`].
    pub idle: Duration,
    /// The bytes of the reserve that the session keeps in the database
    /// directory ([`Reserve`](crate::space::Reserve)): room that the
    /// session gives back when the device is full, and takes again once
    /// room is back (`disk-full-and-backup.md`, DF13). The open writes
    /// the reserve where the directory has none of this length. With 0,
    /// the default, the session keeps none and touches no file of the
    /// reserve's two names.
    pub reserve: u64,
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
            reserve: 0,
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
            .field("reserve", &self.reserve)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the ones the documentation states.
    #[test]
    fn the_defaults_are_as_documented() {
        let options = Options::default();
        assert!(options.create_if_missing);
        assert_eq!(options.page_size, None);
        assert_eq!(options.cache_size, DEFAULT_POOL_CAPACITY);
        assert_eq!(options.cache_size, 256 << 20);
        assert_eq!(options.sync, SyncPolicy::Never);
        assert_eq!(options.log_file_len, None);
        assert_eq!(options.flush, None);
        assert_eq!(options.cleaner, None);
        assert_eq!(options.idle, Duration::from_millis(100));
        assert_eq!(options.reserve, 0);
    }

    /// What the store's recovery is told is what the options say: the
    /// pool's size, the sync policy, the thresholds of write-back, and the
    /// length of a log file only where one is given.
    #[test]
    fn the_recovery_is_told_what_the_options_say() {
        let told = Options::default().recovery();
        assert_eq!(told.pool_capacity, DEFAULT_POOL_CAPACITY);
        assert_eq!(told.log.sync, SyncPolicy::Never);
        assert_eq!(
            told.log.target_file_len,
            LogOptions::default().target_file_len
        );
        assert_eq!(told.flush, None);
        let rest = RecoveryOptions::default();
        assert_eq!(told.max_replay_pages, rest.max_replay_pages);

        let flush = FlushConfig::new(PageSize::MIN);
        let given = Options {
            cache_size: 16 << 20,
            sync: SyncPolicy::Interval(Duration::from_secs(1)),
            log_file_len: Some(1 << 20),
            flush: Some(flush),
            ..Options::default()
        };
        let told = given.recovery();
        assert_eq!(told.pool_capacity, 16 << 20);
        assert_eq!(told.log.sync, SyncPolicy::Interval(Duration::from_secs(1)));
        assert_eq!(told.log.target_file_len, 1 << 20);
        assert_eq!(told.flush, Some(flush));
    }

    /// The options print every field but the file system, and a clone is
    /// of the same file system.
    #[test]
    fn the_options_print_what_they_hold() {
        let options = Options {
            create_if_missing: false,
            page_size: Some(PageSize::MIN),
            cleaner: Some(CleanerConfig::new()),
            ..Options::default()
        };
        let text = format!("{options:?}");
        for field in [
            "create_if_missing: false",
            "page_size: Some(",
            "cache_size: 268435456",
            "sync: Never",
            "log_file_len: None",
            "flush: None",
            "cleaner: Some(",
            "idle: 100ms",
            "reserve: 0",
            "..",
        ] {
            assert!(text.contains(field), "{field} in {text}");
        }
        assert!(!text.contains("vfs"));
        assert!(Arc::ptr_eq(&options.vfs, &options.clone().vfs));
    }
}
