//! Every part of one open database, and the views that are made of them.

use std::{
    io::ErrorKind,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    },
    thread::Thread,
};

use lignin_format::{CommitVersion, PageId, PageSize};
use lignin_io::Vfs;
use lignin_store::{
    alloc::PageAllocator,
    cache::BufferPool,
    flush::{FlushError, FlushState, Stores, WriteBack},
    load::PageLoader,
    recovery::RecoveryReport,
    segment::{read_manifest, ManifestStore, SegmentStore, SegmentStoreError},
    stats::Stats,
    table::{PageTable, TableCheckpoints},
    values::{OwnersError, ValueStore},
    wal::LogStore,
};
use lignin_tree::{
    directory::{Directory, DirectoryError, Dropped},
    drop::FlatDrops,
    flat::{Committed, FlatConfig, FlatPages, FlatTree, StepLog},
    fragment::Layout,
    value::InlinePolicy,
};

use super::{
    error::Reopen,
    open::LOCK_FILE,
    reclaim::{ReclaimStats, Reclaiming},
};
use crate::{
    commit::{CommitState, Committer},
    snapshot::{Directories, OperationGuard, ReadPath, Reclaim, Versions},
    txn::{Commits, Keyspaces, Store as PathStore, WritePath},
};

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the keyspaces of a database are: the root page of each, which is
/// the same for as long as the database lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Catalog {
    /// The flat keyspace of each column.
    pub(super) keyspaces: Keyspaces,
    /// The keyspace of the subtree directory.
    pub(super) directory: FlatTree,
    /// The keyspace of the pages that drops unlinked.
    pub(super) flat_drops: FlatDrops,
}

impl Catalog {
    /// Loads the handle on the subtree directory as `pages` have it.
    pub(super) fn load_directory<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
    ) -> Result<Directory, DirectoryError> {
        Directory::load(pages, self.directory.root(), self.directory.config())
    }
}

/// What wakes the thread that reclaims, and tells it to end.
#[derive(Debug, Default)]
pub(super) struct Signal {
    thread: OnceLock<Thread>,
    stop: AtomicBool,
    /// Whether the thread ends without the work it does for the next
    /// session: the directory is the database's no more.
    abandon: AtomicBool,
}

impl Signal {
    /// Names the thread that is woken.
    pub(super) fn attach(&self, thread: Thread) {
        let _ = self.thread.set(thread);
    }

    /// Wakes the thread. It takes no lock and waits for nothing, so a
    /// reader may call it (`Reclaim::wake`).
    pub(super) fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake();
    }

    pub(super) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Tells the thread to end with nothing more written.
    pub(super) fn abandon(&self) {
        self.abandon.store(true, Ordering::Release);
    }

    pub(super) fn abandoned(&self) -> bool {
        self.abandon.load(Ordering::Acquire)
    }
}

/// What the background work of a session could not do. Nothing of it
/// changes what is committed.
#[derive(Debug, Default)]
pub(super) struct Troubles {
    /// Turns of write-back that failed, in its loop or in a committer's
    /// own turn.
    pub(super) write_back: AtomicU64,
    /// Calls of the reclaimer that failed.
    pub(super) reclaim: AtomicU64,
    /// The last of either, as text.
    pub(super) last: Mutex<Option<String>>,
}

impl Troubles {
    pub(super) fn write_back_failed(&self, error: &FlushError) {
        self.write_back.fetch_add(1, Ordering::Relaxed);
        *lock(&self.last) = Some(format!("write-back: {error}"));
    }

    pub(super) fn reclaim_failed(&self, error: &dyn std::fmt::Display) {
        self.reclaim.fetch_add(1, Ordering::Relaxed);
        *lock(&self.last) = Some(format!("reclaiming: {error}"));
    }
}

/// The parts of an open database that live as long as its handle and that
/// its reclaimers work on.
pub(super) struct Store {
    pub(super) vfs: Arc<dyn Vfs>,
    pub(super) dir: PathBuf,
    pub(super) page_size: PageSize,
    pub(super) pool: BufferPool,
    pub(super) table: PageTable,
    pub(super) segments: SegmentStore,
    pub(super) values: ValueStore,
    pub(super) log: LogStore,
    pub(super) checkpoints: TableCheckpoints,
    pub(super) ids: PageAllocator,
    pub(super) manifest: Mutex<ManifestStore>,
    /// What write-back keeps of the session, as recovery made it.
    pub(super) flush: FlushState,
    /// The counts of the store.
    pub(super) stats: Stats,
    /// The key sets of the session's commits.
    pub(super) commits: Commits,
    /// What the commits of the session share.
    pub(super) committing: CommitState,
    /// The handles on the subtree directory, by version.
    pub(super) directories: Directories,
    /// Where the keyspaces are: read when the database is opened, or made
    /// by its first commit.
    pub(super) catalog: OnceLock<Catalog>,
    /// What the recovery that opened the session did.
    pub(super) recovery: RecoveryReport,
    /// What each commit of the session dropped or cleared, for the
    /// reclaimer to take.
    pub(super) dropped: Mutex<Vec<(CommitVersion, Dropped)>>,
    /// The reclaiming of what drops left: one round at a time.
    pub(super) reclaiming: Mutex<Reclaiming>,
    /// What the reclaiming has done, as of its last finished piece of
    /// work: whoever reads the counts waits for no round.
    pub(super) reclaimed: Mutex<ReclaimStats>,
    /// Wakes the thread that reclaims.
    pub(super) signal: Signal,
    pub(super) troubles: Arc<Troubles>,
    /// Page ids that reclaims gave back to the allocator.
    pub(super) ids_released: AtomicU64,
}

impl Store {
    pub(super) fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, self.page_size)
            .expect("the pool was made of the store's page size")
    }

    pub(super) fn config(&self) -> FlatConfig {
        FlatConfig::new(self.page_size)
    }

    pub(super) fn catalog(&self) -> &Catalog {
        self.catalog
            .get()
            .expect("the keyspaces are named before the database is handed out")
    }

    /// The committed flat pages at `at`, for the operation that holds
    /// `guard`.
    pub(super) fn committed<'r>(
        &'r self,
        guard: &'r OperationGuard<'r>,
        at: CommitVersion,
    ) -> Committed<'r, 'r, OperationGuard<'r>> {
        Committed::new(self.loader(), &self.values, guard, at)
    }

    /// Whether the directory holds this database no more: it is gone, the
    /// lock file is not in it, or the manifest in it is not the one this
    /// session wrote last. Someone removed the directory while the handle
    /// was open, or put another in its place (P-18). An error of the file
    /// layer that says none of these leaves the answer at no.
    pub(super) fn gone(&self) -> bool {
        match self.vfs.read_dir(&self.dir) {
            Err(error) => return error.kind() == ErrorKind::NotFound,
            Ok(entries) if !entries.iter().any(|entry| entry.name == LOCK_FILE) => return true,
            Ok(_) => {}
        }
        // Under the lock no manifest is installed between the two.
        let ours = lock(&self.manifest);
        match (read_manifest(&*self.vfs, &self.dir), ours.manifest()) {
            (Ok(stored), Ok(ours)) => stored != *ours,
            (Err(SegmentStoreError::NoManifest { .. }), _) => true,
            _ => false,
        }
    }

    /// Why the session takes no write any more, if it does not. There is
    /// one record of it, the commit path's, which every commit reads
    /// again under the commit lock: the first reason stands.
    pub(super) fn refusal(&self) -> Option<Reopen> {
        self.committing.stopped().map(Reopen::from)
    }
}

/// What the reclaimers of a store do when the oldest pin may have moved
/// up, and how a reader says that one is due (`transactions.md` PN6, ID3).
struct Hook(Arc<Store>);

impl Reclaim for Hook {
    fn reclaim(&self, pins: &Versions) {
        let store = &*self.0;
        let mut released: Vec<PageId> = Vec::new();
        store.ids.reclaim(pins, &mut released);
        (store.ids_released).fetch_add(released.len() as u64, Ordering::Relaxed);
        store.pool.reclaim(pins);
        store.directories.reclaim(pins);
    }

    fn wake(&self) {
        self.0.signal.wake();
    }
}

/// An open database: its store, and the versions its readers pin.
pub(super) struct Shared {
    pub(super) store: Arc<Store>,
    pub(super) versions: Versions,
}

impl Shared {
    pub(super) fn new(store: Store) -> Self {
        let landed = store.recovery.landed;
        let store = Arc::new(store);
        let versions = Versions::with_reclaim(landed, Box::new(Hook(store.clone())));
        Self { store, versions }
    }

    /// Write-back's view of the store, with no word of the tree pages'
    /// values: every view but the background loop's.
    pub(super) fn write_back(&self) -> WriteBack<'_> {
        let store = &*self.store;
        let stores = Stores {
            pool: &store.pool,
            table: &store.table,
            segments: &store.segments,
            values: &store.values,
            log: &store.log,
            checkpoints: &store.checkpoints,
            manifest: &store.manifest,
            versions: &self.versions,
            page_values: None,
        };
        WriteBack::new(stores, &store.flush, store.page_size)
            .expect("the pool was made of the store's page size")
    }

    /// The write path: what a write transaction is begun from.
    pub(super) fn path(&self) -> WritePath<'_> {
        let store = &*self.store;
        let catalog = store.catalog();
        let parts = PathStore {
            versions: &self.versions,
            loader: store.loader(),
            pool: &store.pool,
            ids: &store.ids,
            values: &store.values,
            commits: &store.commits,
            stats: &store.stats,
            directory: catalog.directory,
            directories: &store.directories,
            flat_drops: catalog.flat_drops,
        };
        WritePath::new(
            parts,
            catalog.keyspaces,
            Layout::proposed(store.page_size),
            InlinePolicy::DEFAULT,
        )
    }

    /// The read path: what a snapshot is taken from.
    pub(super) fn reads(&self) -> ReadPath<'_> {
        let store = &*self.store;
        ReadPath::new(&self.versions, store.loader()).with_records(
            &store.values,
            store.catalog().keyspaces,
            &store.directories,
        )
    }

    /// The commit path.
    pub(super) fn committer(&self) -> Committer<'_> {
        let store = &*self.store;
        Committer::new(
            self.path(),
            self.write_back(),
            &store.table,
            &store.log,
            &store.committing,
        )
    }

    /// What takes the record of a page step that background work makes.
    pub(super) fn steps(&self) -> Steps<'_> {
        Steps {
            write_back: self.write_back(),
            versions: &self.versions,
        }
    }
}

/// The log of the page steps of background work: of the reclaimer of what
/// drops left, and of the regrouping of a page's values. A record goes to
/// the log through write-back, which so knows of the step
/// (`lignin_store::flush`, "What the caller does").
#[derive(Clone, Copy)]
pub(super) struct Steps<'s> {
    write_back: WriteBack<'s>,
    versions: &'s Versions,
}

impl StepLog for Steps<'_> {
    fn latest(&self) -> CommitVersion {
        self.versions.latest()
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        self.write_back.append_change(record)?;
        Ok(())
    }
}
