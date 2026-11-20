//! Every part of one open database, and the views that are made of them.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError,
    },
    thread::Thread,
};

use lignin_format::{CommitVersion, PageId, PageSize};
use lignin_io::{Vfs, VfsLock};
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
    wal::{LogError, LogStore},
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
    commit::{CommitLock, CommitState, Committer},
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
    /// Why the session takes no write any more: the first reason that was
    /// seen where the session is answered for ([`Store::refusal`]), the
    /// commit path's or the store's own. It is set once, and every answer
    /// of the session is read from it.
    ended: OnceLock<Reopen>,
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

    /// Whether the directory is found to be this database's still, by
    /// both of these: the lock file in it is the file that `guard`, the
    /// guard of this handle, is on, and the manifest in it is the one this
    /// session wrote last. Someone may have removed the directory while
    /// the handle was open, or put another in its place, a copy of this
    /// one even, which another opener may hold the lock of (P-18): the
    /// lock file there is another file than the one this handle locked.
    ///
    /// Nothing is made or locked to find this out. Whatever cannot be
    /// found out counts as no: a caller that is about to write does not,
    /// then.
    ///
    /// One thing is told apart from a no. A session whose replacement of
    /// the manifest did not finish cannot say which manifest it wrote
    /// last, so there is nothing to compare in a directory that has its
    /// lock file still: that is the error. A caller has the manifest
    /// settled then ([`Shared::settle_manifest`]) and asks again.
    pub(super) fn ours(&self, guard: &dyn VfsLock) -> Result<bool, SegmentStoreError> {
        if !guard.is_at(&self.dir.join(LOCK_FILE)).unwrap_or(false) {
            return Ok(false);
        }
        // Under the lock no manifest is installed between the two.
        let ours = lock(&self.manifest);
        let ours = ours.manifest()?;
        Ok(read_manifest(&*self.vfs, &self.dir).is_ok_and(|stored| stored == *ours))
    }

    /// Why the session takes no write any more, if it does not.
    ///
    /// A commit or a flush that ends the session says so in the commit
    /// path's record, under the commit lock. The store ends it as well,
    /// whichever step met what it cannot undo ([`Self::stays_failed`]): a
    /// turn of write-back, a table checkpoint, the creation of a
    /// checkpoint of the database. That is looked for here, by every
    /// commit before it begins and by whoever asks. The first reason that
    /// is seen here, the commit path's or the store's, is kept in one
    /// place that is written once ([`Self::said`]): it stands whatever
    /// fails later, and nobody is given another.
    ///
    /// The look is two reads of what the store holds, and takes no lock
    /// of the commit path. Only what it finds is recorded under the commit
    /// lock, as a commit and a flush record theirs: no commit is published
    /// behind the record of a sync of the log that failed. Here the lock
    /// is tried and not waited for. While a commit is being made, that
    /// commit records a failed sync of its own, and whatever else the
    /// store holds is recorded by [`Self::look_after_a_commit`]. So
    /// this can be called by a thread that holds the commit lock.
    pub(super) fn refusal(&self) -> Option<Reopen> {
        self.ended(|| self.committing.try_lock())
    }

    /// The look of [`Self::refusal`], by a thread whose commit has ended and
    /// that holds no lock: it waits for a commit that is being made. So a
    /// store that failed beside commits ends the session after at most one
    /// more commit of each thread that was writing, however closely the
    /// commits follow each other.
    pub(super) fn look_after_a_commit(&self) -> Option<Reopen> {
        self.ended(|| Some(self.committing.lock()))
    }

    fn ended<'s>(&'s self, take: impl FnOnce() -> Option<CommitLock<'s>>) -> Option<Reopen> {
        if let Some(reason) = self.said(None) {
            return Some(reason);
        }
        self.stays_failed()?;
        let _commit = take()?;
        // Under the commit lock no commit records a failed sync, and none
        // is between its group in the log and its publication.
        if let Some(reason) = self.said(None) {
            return Some(reason);
        }
        let found = match self.stays_failed()? {
            // In the commit path's record, as a flush's failed sync is:
            // a commit that waits for the lock is refused under it.
            Failed::Log(failed) => {
                (self.committing).sync_failed(self.log.latest(), &FlushError::Log(failed));
                debug_assert!(
                    self.committing.stopped().is_some(),
                    "the commits of a database end the session at a failed sync"
                );
                None
            }
            Failed::Segments => Some(Reopen::SegmentsNotDurable),
        };
        #[cfg(test)]
        if let Some(change) = BEFORE_A_REASON_IS_KEPT.with(|hook| hook.borrow_mut().take()) {
            change();
        }
        self.said(found)
    }

    /// The one reason the session gives for having ended, if it has: the
    /// first that is seen here, kept in a place that is written once. The
    /// commit path's own record is looked at before `found`, which is what
    /// the caller found in the store.
    ///
    /// Two records can both be there: the commit path stops a session by
    /// itself, also outside the commit lock where the log cannot say
    /// whether an item is in it, and the store's failed sync of a segment
    /// is found here. Whichever of them gets here first is the reason from
    /// then on, for everyone who asks and for every commit that is
    /// refused.
    fn said(&self, found: Option<Reopen>) -> Option<Reopen> {
        let ended = &self.troubles.ended;
        if let Some(said) = ended.get() {
            return Some(said.clone());
        }
        let first = self.committing.stopped().map(Reopen::from).or(found)?;
        Some(ended.get_or_init(|| first).clone())
    }

    /// What of the store stays failed until it is opened again, if
    /// anything does, whatever room or luck the device has later:
    ///
    /// - a sync of the log failed, and the log makes nothing durable any
    ///   more (`lignin_store::wal`);
    /// - a sync of a segment failed, and no table checkpoint is installed
    ///   any more (`lignin_store::flush`).
    ///
    /// A session in one of these states takes commits that can never
    /// become durable, or keeps a log that only grows, or refuses its
    /// commits for a failure that is long over. So it has ended.
    ///
    /// A replacement of the manifest that did not finish is not among
    /// them: it is settled, and ends no session
    /// ([`Shared::settle_manifest`]).
    ///
    /// These are reads of state: no operation on a file, and no lock that
    /// is held over one.
    fn stays_failed(&self) -> Option<Failed> {
        if let Some(failed) = self.log.sync_failed() {
            return Some(Failed::Log(failed));
        }
        (self.flush.segments_not_durable()).then_some(Failed::Segments)
    }

    /// Whether a replacement of the manifest left it in doubt, as far as
    /// that is known without waiting. The manifest's state is behind the
    /// lock that write-back holds over a sync while it replaces a
    /// manifest, and that whoever settles a manifest holds while it is
    /// opened again. The lock is tried: while somebody holds it this says
    /// no, whatever the manifest's state is, and the holder is the one who
    /// deals with it.
    fn manifest_in_doubt(&self) -> bool {
        let manifest = match self.manifest.try_lock() {
            Ok(manifest) => manifest,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        manifest.manifest().is_err()
    }
}

#[cfg(test)]
thread_local! {
    /// What a test has run, once, when a look on this thread has found
    /// what stays failed in the store and is about to keep its reason:
    /// the moment in which the commit path can stop the session by itself.
    pub(super) static BEFORE_A_REASON_IS_KEPT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// What stays failed in a store until it is opened again
/// ([`Store::stays_failed`]).
enum Failed {
    /// A sync of the log, with what the log says of it.
    Log(LogError),
    /// A sync of a segment.
    Segments,
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

    /// Has a manifest that a replacement left in doubt opened again, as
    /// write-back does by itself where it next needs it
    /// (`WriteBack::settle_manifest`). Until then nothing that reads the
    /// manifest itself can: no checkpoint of the database is made, and a
    /// close cannot tell that the directory is the database's. With
    /// nothing in doubt this is one look at the manifest, which waits for
    /// nothing.
    ///
    /// A manifest in doubt ends no session (D-157): it is not known which
    /// of two manifests a crash would leave, and opening it again finds
    /// out. Where it cannot be opened, the manifest stays in doubt, the
    /// error is the caller's, and the next call tries again.
    ///
    /// One more error is the caller's and is not made again by the next
    /// call: the manifest was opened, and a store that was told of it
    /// answered an error (it could not delete a file that the manifest
    /// names no longer). Every store was told all the same, and the
    /// manifest is in doubt no more (`WriteBack::settle_manifest`).
    ///
    /// The settling waits for the lock on the manifest and is made under
    /// it, with a read and a sync. So this is for a thread that holds no
    /// lock: a commit before it begins, as the cut of the log that a
    /// commit's thread makes there, and a close. A look at the session
    /// ([`Store::refusal`]) settles nothing, and can be made under any
    /// lock.
    pub(super) fn settle_manifest(&self) -> Result<(), FlushError> {
        match self.store.manifest_in_doubt() {
            true => self.write_back().settle_manifest(),
            false => Ok(()),
        }
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

/// What a test runs when a reclaimer asks for the latest version.
#[cfg(test)]
pub(super) type AtTheLatest = Box<dyn FnMut()>;

#[cfg(test)]
thread_local! {
    /// What a test has run each time a reclaimer on this thread asks for
    /// the latest version, before it is given it: a commit that lands at
    /// one of the reclaimer's reads.
    pub(super) static AT_THE_LATEST: std::cell::RefCell<Option<AtTheLatest>> =
        const { std::cell::RefCell::new(None) };
}

impl StepLog for Steps<'_> {
    fn latest(&self) -> CommitVersion {
        #[cfg(test)]
        if let Some(mut change) = AT_THE_LATEST.with(|hook| hook.borrow_mut().take()) {
            // The hook is out of its place while it runs: what it does
            // can ask for the latest version itself.
            change();
            AT_THE_LATEST.with(|hook| *hook.borrow_mut() = Some(change));
        }
        self.versions.latest()
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        self.write_back.append_change(record)?;
        Ok(())
    }
}
