//! The engine facade: one open database ([`Db`]).
//!
//! A [`Db`] is a database directory, opened: the store that
//! [recovery](lignin_store::recovery) brought to one committed version, its
//! write-back and its reclaimers running on two threads, and the versions
//! its readers pin. Everything a caller does to a database goes through
//! it:
//!
//! | Call | What it does | Rule |
//! | --- | --- | --- |
//! | [`Db::open`] | Opens the database in a directory, and makes it if the directory has none ([`Options`]). It takes the directory's lock, recovers, and starts the background work. | P-01 to P-04, DU2 to DU4 |
//! | [`Db::close`], or dropping the handle | Ends the session: the background work stops, what can be written back is, a table checkpoint is taken, the log is made durable, and the lock is released. | P-04, P-35 |
//! | [`Db::begin`] | Begins a write transaction ([`WriteTxn`]). | WT1 |
//! | [`Db::commit`], [`Db::rollback`] | Commits a write transaction as the next version, or gives up what it holds. | CM1 to CM9, WT9 |
//! | [`Db::write`] | A write with no transaction: one batch, committed at once. | RD7 |
//! | [`Db::snapshot`] | A snapshot of the latest version, read-only ([`Snapshot`]). | RD1, RD2 |
//! | [`Db::read`] | One operation that pins no version and reads the latest one ([`Operation`]). | RD4 |
//! | [`Db::flush`] | Makes every commit that was published when it was called durable. | DU1a, P-05 |
//! | [`Db::wipe`] | Removes every record of every keyspace, in one commit. | CV3, CF5, P-23 |
//! | [`Db::stats`] | Every count the engine keeps ([`DbStats`]). | |
//! | [`Db::keyspaces`] | The flat keyspace of each column. | |
//!
//! The rules are those of `docs/lignindb/transactions.md`. The modules
//! this one is made of say how each part keeps them: [`crate::txn`] for
//! the write transaction, [`crate::commit`] for the commit and its
//! outcomes, [`crate::snapshot`] for readers, [`crate::scope`],
//! [`crate::savepoint`] and [`crate::batch`] for what is built on a
//! transaction.
//!
//! ```
//! use lignindb::{
//!     commit::{Committed, NoStaleHashes},
//!     txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite},
//!     Db, Options,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let db = Db::open(dir.path(), &Options::default())?;
//!
//! // A write transaction enters a subtree and puts a record into its aux
//! // column. Nobody else sees either before the commit.
//! let subtree = Subtree::new([7; 32]);
//! let txn = db.begin();
//! let scope = txn.scope()?;
//! scope.apply(&Batch {
//!     subtrees: &[SubtreeWrite::Enter {
//!         subtree,
//!         tree_type: b"tree",
//!         host: None,
//!     }],
//!     records: &[RecordWrite::put(Column::Aux, subtree, b"key", b"value")],
//!     ..Batch::default()
//! })?;
//! scope.merge();
//! let before = db.snapshot();
//! let Committed::Published(published) = db.commit(&txn, &mut NoStaleHashes)? else {
//!     unreachable!("the transaction wrote");
//! };
//! assert_eq!(db.version(), published.version);
//!
//! // A snapshot reads the version it was taken at, for as long as it is
//! // held.
//! let mut value = Vec::new();
//! assert!(before.operation().open(subtree)?.is_none());
//! let after = db.snapshot();
//! let operation = after.operation();
//! let opened = operation.open(subtree)?.expect("the subtree is entered");
//! assert!(opened.get(Column::Aux, b"key", &mut value)?);
//! assert_eq!(value, b"value");
//! drop(opened);
//! drop(operation);
//! drop((before, after));
//!
//! // Durable from here: a crash after `flush` returned recovers the
//! // commit.
//! let flushed = db.flush()?;
//! assert_eq!(flushed.version, published.version);
//! db.close()?;
//!
//! // The next session opens the same state, and a wipe empties it.
//! let db = Db::open(dir.path(), &Options::default())?;
//! assert_eq!(db.version(), published.version);
//! assert!(db.snapshot().operation().open(subtree)?.is_some());
//! db.wipe()?;
//! assert!(db.snapshot().operation().open(subtree)?.is_none());
//! # Ok(())
//! # }
//! ```
//!
//! # The directory
//!
//! ```text
//! <db>/
//!   LOCK       locked while a handle has the database open
//!   MANIFEST   the live files, the table checkpoint, the format version,
//!              the page size
//!   wal/       the log
//!   seg/       segments
//!   val/       the value log
//!   table/     page-table checkpoints
//! ```
//!
//! **The engine touches these and nothing else** (P-03). Whatever else a
//! directory holds is not the engine's: an open, a recovery, a wipe and a
//! close read no other entry, and remove and rename none. A node keeps
//! `checkpoints/`, marker files and its own state beside the database, and
//! they are as they were after each of those.
//!
//! **One handle at a time** (P-04). The lock is taken before anything is
//! written and released when the handle is dropped, so the same path can
//! be opened again in the same process once the handle is gone. A second
//! open while one is alive fails with [`OpenError::Locked`].
//!
//! **A directory that is not this engine's is refused, and left byte for
//! byte as it was** (P-02). An open looks before it writes:
//!
//! | The directory | The open |
//! | --- | --- |
//! | is not there, or holds nothing of any engine | makes the database, or fails with [`OpenError::NoDatabase`] if [`Options::create_if_missing`] is off |
//! | has an entry that RocksDB makes (`CURRENT`, `IDENTITY`, `MANIFEST-…`, `OPTIONS-…`, an `.sst` file), or a `MANIFEST` that is no LigninDB file | [`OpenError::OtherEngine`] |
//! | has a manifest of a format version this build does not read | [`OpenError::FormatVersion`] |
//! | has the engine's directories and no manifest | [`OpenError::NoManifest`] |
//! | has pages of another size than [`Options::page_size`] names | [`OpenError::PageSize`] |
//!
//! Each of these is found by reading alone, before the lock file is
//! made ([`OpenError::left_untouched`]).
//!
//! # A new database
//!
//! A new database is its first manifest, recovered like any other
//! (`lignin_store::recovery`), and one commit that the open makes: the
//! catalog, the flat keyspace of each column (data, aux, roots, meta), the
//! subtree directory and the keyspace of the pages that drops unlink, all
//! empty. That commit has version 1, so the first commit of a caller is
//! version 2. A crash before it is in the log leaves a store with no
//! commit, and the next open makes it again.
//!
//! **The catalog says where the keyspaces are.** It is a flat keyspace in
//! the first page a store makes, page 1, with one record for each of the
//! six: the id of its root page, which is the same for as long as the
//! database lives. An open reads it at the version it landed on.
//!
//! # A session that takes no write any more
//!
//! Four outcomes of a commit end the session for writes (D-098): a commit
//! that is published and whose sync failed
//! ([`CommitError::NotDurable`]), a commit that could not be installed
//! ([`CommitError::Halted`]), one of which the log cannot say whether it
//! holds it ([`CommitError::Unsettled`]), and a commit that found the
//! session stopped ([`CommitError::Stopped`]). A [`flush`](Db::flush)
//! whose sync failed is a fifth. After the first of them every
//! [`commit`](Db::commit), [`write`](Db::write) and [`wipe`](Db::wipe) is
//! refused with [`WriteError::Reopen`], with the transaction as it was,
//! and [`Db::reopen`] says why. The log makes nothing durable after a
//! failed sync, and what it holds after the other three is settled only by
//! a recovery, so the caller closes the database and opens it again. Reads
//! go on until then, and what was published stays readable.
//!
//! # Background work
//!
//! Two threads belong to an open database, and both end with its handle.
//!
//! - **Write-back** ([`lignin_store::flush`]): page versions go from the
//!   buffer pool to segments, values that commits carried in their groups
//!   get their place in the value log, table checkpoints are taken, the
//!   log's files are kept going, and the log is synced where the sync
//!   policy is an interval. A committer is slowed while it is behind
//!   (FL5). The values of a tree page are regrouped before the page is
//!   written as a base image (`lignin_tree::value::Regrouping`).
//! - **Reclaiming**: the page id allocator, the buffer pool and the kept
//!   handles on the subtree directory give up what no reader can reach any
//!   more, when a reader that held it back has gone (`page-alloc.md` N8);
//!   and what dropped and cleared subtrees left is taken out of the pages
//!   (`lignin_tree::drop`, CL6). A commit hands it what it dropped
//!   ([`Published::dropped`]), an open what earlier sessions left, and it
//!   reads the records of the lazy range deletes itself. It begins each
//!   once no reader from before the drop is left, takes it out in page
//!   steps whose records go to the log through write-back, and tries
//!   again later what a write transaction's copy of a page holds back.
//!   When a reclaiming is done, the handle on the directory of the latest
//!   version is loaded again, under the commit lock, so that the id of the
//!   Merk leaves the handles.
//!
//! An error of either changes nothing that is committed. It is counted
//! and kept as text ([`DbStats::background`]).
//!
//! **Closing waits for a reclaiming that holds cells.** From the moment
//! the record of a dropped Merk is out of the directory, the cells that
//! are left of it are known to the session and to nothing on disk
//! (`lignin_tree::drop`, "Reclaiming"). A close goes on with such a
//! reclaiming until it is done, or until a page that the latest commit
//! wrote holds it back. What a process that is killed leaves of it is
//! space, and no state.
//!
//! # Wipe
//!
//! [`Db::wipe`] is one commit (CV3): a write transaction that drops every
//! subtree that no other hosts, with a key set that covers every key
//! (CF5). In every column the records of a subtree are those of its entry
//! in the directory (`tree-layout.md` D6), so with the entries gone no
//! record is left to read: no node, and no record of the data, roots, aux
//! and meta columns.
//!
//! **No commit comes between the wipe's look at the database and its
//! publication.** The wipe holds the commit lock from the walk that finds
//! the subtrees to drop until its version is published
//! ([`Committer::commit_alone`](crate::commit::Committer)), so every
//! other commit has a version before the wipe's, and is wiped, or after
//! it. At the wipe's own version no subtree is entered. A snapshot from
//! before the wipe keeps reading its version, a crash leaves the database
//! as it was or empty, and an open write transaction that wrote a record
//! before the wipe fails its commit with a conflict. Entering a subtree
//! writes no record: a transaction that did nothing else commits after
//! the wipe, and its subtree is in use and empty.
//!
//! What the wipe removed is storage until the reclaimer has taken it out,
//! like what any drop leaves. No file is deleted by a wipe, and no entry
//! of the directory that is not the engine's is touched.
//!
//! # Choices made here that the plan left open
//!
//! 1. **The roots of the keyspaces are in a catalog in page 1**, written
//!    by the first commit. The manifest names files and no page, and a
//!    table checkpoint is the store's.
//! 2. **The layout of tree pages and the inline threshold are the
//!    defaults** ([`Layout::proposed`], [`InlinePolicy::DEFAULT`]) and are
//!    not in [`Options`]: a database that was written with one cannot be
//!    read with another, and nothing records which it was written with.
//!    They become options when they are recorded (PERF-08).
//! 3. **A wipe finds the subtrees to drop by a walk of the directory's
//!    entries**, which reads every entry, under the commit lock. A wipe of
//!    a database in which no subtree is entered writes nothing and takes
//!    no version (CV2).
//! 4. **A wipe is made under one hold of the commit lock**, and not as a
//!    transaction that is made again when a commit lands beside it: a
//!    commit that entered a subtree between the walk and the wipe's commit
//!    would meet none of the wipe's writes, and its records would be in
//!    the wipe's own version. Commits wait for as long as the walk and the
//!    drops take.
//! 5. **Every error of `flush` ends the session for writes**, also one
//!    that is not the sync's own: the caller asked for its commits to be
//!    durable and was not given that.
//! 6. **The handles on the directory are behind a read-write lock**
//!    ([`Directories`](crate::snapshot::Directories)). A reader takes it
//!    once for each version it reads at, when it first opens a subtree,
//!    and a snapshot that opens none takes it never.
//!
//! [`Layout::proposed`]: lignin_tree::fragment::Layout::proposed
//! [`InlinePolicy::DEFAULT`]: lignin_tree::value::InlinePolicy::DEFAULT
//! [`Published::dropped`]: crate::commit::Published::dropped
//! [`CommitError::NotDurable`]: crate::commit::CommitError::NotDurable
//! [`CommitError::Halted`]: crate::commit::CommitError::Halted
//! [`CommitError::Unsettled`]: crate::commit::CommitError::Unsettled
//! [`CommitError::Stopped`]: crate::commit::CommitError::Stopped

mod error;
mod open;
mod reclaim;
mod store;

#[cfg(all(test, not(loom)))]
mod tests;

use std::{
    ffi::OsString,
    fmt,
    path::Path,
    sync::{atomic::Ordering, Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use lignin_format::{tree_page::Hash, CommitVersion, PageSize};
use lignin_io::VfsLock;
use lignin_store::{
    flush::{Stores, WriteBack},
    recovery::RecoveryReport,
    stats::StoreStats,
    table::LogDurable,
};
use lignin_tree::{
    hash::{HashRules, Refusal},
    node::NodeRef,
    value::{RegroupPolicy, Regrouping},
};

use self::store::{lock, Shared};
pub use self::{
    error::{DbError, OpenError, Reopen, WriteError},
    reclaim::{ReclaimStats, Round},
};
use crate::{
    commit::{CommitError, CommitStats, Committed, Committer, NoStaleHashes, Published},
    options::Options,
    snapshot::{LatestLease, Operation, Snapshot},
    txn::{Batch, KeySet, Keyspaces, Subtree, SubtreeWrite, TxnError, WriteTxn},
};

/// What a [`Db::flush`] made durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flushed {
    /// The target of the flush (DU1a): the latest version when the flush
    /// was called. A crash at any later time recovers this version or a
    /// later one.
    pub version: CommitVersion,
    /// How far the log is durable, which can be further.
    pub durable: LogDurable,
}

/// What the background work of a session could not do
/// ([`DbStats::background`]). None of it changes what is committed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Background {
    /// Turns of write-back that failed, in its loop and in the turns that
    /// committers made or waited for.
    pub write_back_errors: u64,
    /// Calls of the reclaimer that failed.
    pub reclaim_errors: u64,
    /// The last of either, as text.
    pub last_error: Option<String>,
}

/// Every count of an open database ([`Db::stats`]).
#[derive(Debug, Clone)]
pub struct DbStats {
    /// The latest published version.
    pub version: CommitVersion,
    /// The counts of the store (`lignin_store::stats`): the rows of the
    /// plan's prediction table that count what the engine did, and what an
    /// operator watches.
    pub store: StoreStats,
    /// What the commits of the session did.
    pub commits: CommitStats,
    /// What the recovery that opened the session did.
    pub recovery: RecoveryReport,
    /// What the reclaiming of dropped subtrees has done, and what waits.
    pub reclaimed: ReclaimStats,
    /// Page ids that reclaims gave back to the allocator.
    pub ids_released: u64,
    /// Version pins and operation guards that are held.
    pub pins: usize,
    /// See [`Self::pins`].
    pub guards: usize,
    /// Handles on the subtree directory that are kept, one for each
    /// version a reader can still read at.
    pub directories: usize,
    /// What the background work could not do.
    pub background: Background,
    /// Why the session takes no write any more, if it does not.
    pub reopen: Option<Reopen>,
}

impl fmt::Display for DbStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version {}", self.version)?;
        write!(f, "{}", self.store)?;
        writeln!(
            f,
            "commits: {} published, {} conflicts, {} failed, {} rollbacks",
            self.commits.published,
            self.commits.conflicts,
            self.commits.failed,
            self.commits.rollbacks
        )?;
        writeln!(
            f,
            "reclaimed: {} dropped subtrees ({} waiting), {} cells, {} pages freed",
            self.reclaimed.merks,
            self.reclaimed.waiting,
            self.reclaimed.cells,
            self.reclaimed.pages_freed
        )?;
        writeln!(
            f,
            "readers: {} pins, {} guards, {} directory handles kept",
            self.pins, self.guards, self.directories
        )?;
        if let Some(error) = &self.background.last_error {
            writeln!(f, "background: {error}")?;
        }
        if let Some(reopen) = &self.reopen {
            writeln!(f, "{reopen}")?;
        }
        Ok(())
    }
}

/// The two threads of an open database.
struct Threads {
    write_back: JoinHandle<()>,
    reclaim: JoinHandle<()>,
}

/// An open database. See the [module documentation](self).
///
/// It is `Send` and `Sync`: every thread of a process shares one handle,
/// by reference or in an `Arc`. Transactions, snapshots and operations
/// borrow it, so the database stays open while one of them lives.
pub struct Db {
    shared: Arc<Shared>,
    threads: Mutex<Option<Threads>>,
    created: bool,
    /// The lock of the directory. It is the last field, so that it is
    /// released when everything else of the handle is gone.
    _lock: Box<dyn VfsLock>,
}

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Db>();
};

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db")
            .field("dir", &self.shared.store.dir)
            .field("version", &self.version())
            .field("page_size", &self.page_size())
            .finish_non_exhaustive()
    }
}

/// The hash rules of a commit, with a count of the calls: the hashes the
/// tree layer computes are those a commit evaluates (CM3 step 5).
struct Counted<'r, R> {
    rules: &'r mut R,
    calls: u64,
}

impl<R: HashRules> HashRules for Counted<'_, R> {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.value_hash(node, value, hosted)
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.kv_hash(key, value_hash)
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.calls += 1;
        self.rules.node_hash(node)
    }
}

impl Db {
    /// Opens the database in the directory `dir`, and makes one there if
    /// the directory has none and [`Options::create_if_missing`] is on.
    /// The directory is created with every directory above it that is
    /// missing.
    ///
    /// The directory's lock is held from here until the handle is dropped.
    /// The store is recovered to the last commit whose group its log holds
    /// whole (DU2), which is every commit of a session that was closed,
    /// and the background work is started.
    ///
    /// A directory that this build does not open is refused before
    /// anything is written to it ([`OpenError::left_untouched`]), and
    /// entries that are not the engine's are never read, removed or
    /// renamed.
    pub fn open(dir: impl AsRef<Path>, options: &Options) -> Result<Self, OpenError> {
        let dir = dir.as_ref();
        let opened = open::open_dir(dir, options)?;
        let (shared, directory) = open::open_store(dir, options, opened.manifest)?;
        // What earlier sessions dropped and did not reclaim.
        shared
            .adopt_drops(&directory)
            .map_err(OpenError::catalog("what earlier drops left"))?;
        let shared = Arc::new(shared);
        let writer = shared.clone();
        let write_back = spawn("lignindb-write-back", move || write_back(&writer))?;
        // A thread that is never to sleep would spin.
        let idle = options.idle.max(Duration::from_millis(1));
        let reclaimer = shared.clone();
        let reclaim = spawn("lignindb-reclaim", move || reclaimer.reclaim_loop(idle));
        let reclaim = match reclaim {
            Ok(reclaim) => reclaim,
            Err(error) => {
                shared.write_back().stop();
                let _ = write_back.join();
                return Err(error);
            }
        };
        Ok(Self {
            shared,
            threads: Mutex::new(Some(Threads {
                write_back,
                reclaim,
            })),
            created: opened.created,
            _lock: opened.lock,
        })
    }

    /// Ends the session and releases the directory's lock: the background
    /// work stops, what can be written back is, a table checkpoint is
    /// taken, the session's files are closed and the log is made durable,
    /// so that the next open has little to replay.
    ///
    /// Dropping the handle does the same and reports nothing. Either way
    /// every commit that was published is in the log, and the next open
    /// recovers what was durable or more (DU2). An error here says that
    /// something of the above was not done: the database is closed all the
    /// same, and the next open replays what the checkpoint does not cover.
    pub fn close(self) -> Result<(), DbError> {
        self.shut()
    }

    fn shut(&self) -> Result<(), DbError> {
        let Some(threads) = lock(&self.threads).take() else {
            return Ok(());
        };
        let shared = &*self.shared;
        // The reclaimer first: what it has to end, it ends through
        // write-back's log.
        shared.store.signal.stop();
        let reclaimed = threads.reclaim.join();
        shared.write_back().stop();
        let written = threads.write_back.join();
        let finished = shared.write_back().finish();
        reclaimed.map_err(|_| DbError::Panicked("reclaim"))?;
        written.map_err(|_| DbError::Panicked("write-back"))?;
        finished?;
        Ok(())
    }

    /// The directory of the database.
    pub fn dir(&self) -> &Path {
        &self.shared.store.dir
    }

    /// Whether this open made the database: the directory held none.
    pub fn created(&self) -> bool {
        self.created
    }

    /// The size of the store's pages.
    pub fn page_size(&self) -> PageSize {
        self.shared.store.page_size
    }

    /// The latest published version. The first commit of a caller is
    /// version 2: version 1 made the keyspaces.
    pub fn version(&self) -> CommitVersion {
        self.shared.versions.latest()
    }

    /// The flat keyspace of each column of records: data, aux, roots and
    /// meta. A record of a subtree is in its column's keyspace under the
    /// subtree's 32 bytes and its key, and is read and written through a
    /// transaction or a snapshot by its [`Unit`](crate::txn::Unit).
    pub fn keyspaces(&self) -> Keyspaces {
        self.shared.store.catalog().keyspaces
    }

    /// Begins a write transaction (WT1). Any number may be open, and one
    /// commits at a time. It holds nothing until it writes.
    ///
    /// It is committed with [`Self::commit`] and rolled back with
    /// [`Self::rollback`], through this handle. A transaction that is
    /// dropped gives up what it holds.
    pub fn begin(&self) -> WriteTxn<'_> {
        self.shared.path().begin()
    }

    /// Commits `txn` as the next version of the database
    /// (`transactions.md` section 8, [`crate::commit`]): its stale hashes
    /// are evaluated with `rules`, one group is appended to the log, and
    /// the version is published. A caller whose writers make every hash,
    /// as on existing GroveVersions, passes [`NoStaleHashes`].
    ///
    /// How a commit ends, and what each outcome leaves of the transaction
    /// and of the database, is the table of [`CommitError`]. A transaction
    /// that wrote nothing commits nothing ([`Committed::Empty`]).
    ///
    /// A session that takes no write any more refuses with
    /// [`WriteError::Reopen`] and leaves the transaction as it was.
    pub fn commit<R: HashRules>(
        &self,
        txn: &WriteTxn<'_>,
        rules: &mut R,
    ) -> Result<Committed, WriteError> {
        self.counted(rules, |committer, rules| committer.commit(txn, rules))
    }

    /// Rolls `txn` back (WT9): its writes, its private copies, its page
    /// ids and its savepoints go, and it can be written again. It is
    /// refused while a scope of the transaction is open.
    pub fn rollback(&self, txn: &WriteTxn<'_>) -> Result<(), TxnError> {
        self.shared.committer().rollback(txn)
    }

    /// A write with no transaction (RD7): `batch` is applied to a write
    /// transaction of its own, which is committed at once. It waits for no
    /// open write transaction, and its commit is one like any other to
    /// them (CF8).
    pub fn write<R: HashRules>(
        &self,
        batch: &Batch<'_>,
        rules: &mut R,
    ) -> Result<Committed, WriteError> {
        self.counted(rules, |committer, rules| committer.write(batch, rules))
    }

    /// A snapshot of the latest version (RD1): every read through it is of
    /// that version, until it is dropped. It takes no lock and reads no
    /// page here. A read with no transaction is one of these, held for one
    /// operation of the caller (RD2).
    pub fn snapshot(&self) -> Snapshot<'_> {
        self.shared.reads().snapshot()
    }

    /// One operation that pins no version (RD4): it takes its guard and
    /// reads the latest version. `lease` is the caller's, kept from one
    /// operation to the next ([`LatestLease`]).
    pub fn read(&self, lease: &mut LatestLease) -> Operation<'_> {
        self.shared.reads().operation(lease)
    }

    /// Makes every commit that was published when this was called durable
    /// (DU1a, P-05): the log is synced through the group of the latest
    /// version, which is the flush's target. A crash at any time after it
    /// returned recovers that version or a later one. A commit that is
    /// published while it runs may or may not be covered.
    ///
    /// It takes no lock that a reader, a writer or a committer needs.
    ///
    /// If it fails nothing is promised, and the session takes no write any
    /// more ([`Reopen::NotDurable`]): after a sync that failed the log
    /// makes nothing durable in this session.
    pub fn flush(&self) -> Result<Flushed, DbError> {
        let shared = &*self.shared;
        // The target is read first: every commit up to it has its group
        // in the log (CM3 steps 6 and 8).
        let version = shared.versions.latest();
        let _guard = shared.versions.guard();
        match shared.write_back().sync_log() {
            Ok(durable) => Ok(Flushed { version, durable }),
            Err(error) => {
                shared.store.refuse(Reopen::NotDurable {
                    version,
                    reason: error.to_string(),
                });
                Err(DbError::Flush(error))
            }
        }
    }

    /// Removes every record of every keyspace: the nodes of every Merk,
    /// and the records of the data, roots, aux and meta columns (P-23).
    /// It is one commit (CV3), so no reader sees a database that is wiped
    /// in part, and a crash leaves it as it was or empty.
    ///
    /// The commit lock is held from the walk that finds what there is to
    /// drop until the wipe is published, so no commit comes between the
    /// two: every commit is before the wipe, and wiped, or after it. At the
    /// wipe's own version no subtree is entered.
    ///
    /// Its key set is every key (CF5): an open write transaction that
    /// wrote a record before the wipe was published fails its commit with
    /// a conflict. A snapshot from before keeps reading its version.
    ///
    /// A database in which no subtree is entered has no record, and its
    /// wipe writes nothing and takes no version ([`Committed::Empty`]).
    pub fn wipe(&self) -> Result<Committed, WriteError> {
        self.refused()?;
        let mut everything = KeySet::new();
        everything.everything();
        let committer = self.shared.committer();
        let committed = committer.commit_alone(&mut NoStaleHashes, Some(&everything), |txn| {
            let outermost = self.outermost()?;
            if outermost.is_empty() {
                return Ok(());
            }
            let drops: Vec<SubtreeWrite<'_>> = (outermost.iter())
                .map(|subtree| SubtreeWrite::Drop { subtree: *subtree })
                .collect();
            let scope = txn.scope()?;
            scope.apply(&Batch {
                subtrees: &drops,
                ..Batch::default()
            })?;
            scope.merge();
            Ok(())
        });
        self.settle(committed)
    }

    /// The subtrees in use that no other subtree hosts, as the latest
    /// version has them: dropping them drops every subtree. The caller
    /// holds the commit lock, so the latest version stays the latest.
    fn outermost(&self) -> Result<Vec<Subtree>, TxnError> {
        let shared = &*self.shared;
        let guard = shared.versions.guard();
        let at = guard.latest();
        let directory = shared.path().directory_at(at)?;
        let pages = shared.store.committed(&guard, at);
        let mut outermost = Vec::new();
        let mut entries = directory.entries();
        while let Some((prefix, entry)) = entries.next(&pages)? {
            if entry.host().is_none() && directory.is_live(&entry) {
                outermost.push(Subtree::new(prefix));
            }
        }
        Ok(outermost)
    }

    /// Every count the engine keeps.
    pub fn stats(&self) -> DbStats {
        let shared = &*self.shared;
        let store = &*shared.store;
        let troubles = &store.troubles;
        DbStats {
            version: shared.versions.latest(),
            store: store.stats.read(&shared.write_back()),
            commits: store.committing.stats(),
            recovery: store.recovery.clone(),
            reclaimed: *lock(&store.reclaimed),
            ids_released: store.ids_released.load(Ordering::Relaxed),
            pins: shared.versions.pins_held(),
            guards: shared.versions.guards_held(),
            directories: store.directories.len(),
            background: Background {
                write_back_errors: troubles.write_back.load(Ordering::Relaxed),
                reclaim_errors: troubles.reclaim.load(Ordering::Relaxed),
                last_error: lock(&troubles.last).clone(),
            },
            reopen: store.refusal(),
        }
    }

    /// Why the session takes no write any more, if it does not: the
    /// database has to be closed and opened again.
    pub fn reopen(&self) -> Option<Reopen> {
        self.shared.store.refusal()
    }

    /// One round of the reclaiming that the background thread makes, on
    /// the calling thread: the reclaimers of the store take their step,
    /// and what dropped subtrees left is taken out as far as it can be
    /// now. A tool that has to see the space back, and a test, call it;
    /// nothing else has to.
    pub fn reclaim(&self) -> Round {
        self.shared.reclaim_round(false)
    }

    /// The entries of the database directory that are not the engine's,
    /// by name, in order: what an open, a recovery, a wipe and a close
    /// leave alone (P-03).
    pub fn foreign_entries(&self) -> std::io::Result<Vec<OsString>> {
        open::foreign_entries(&*self.shared.store.vfs, &self.shared.store.dir)
    }

    /// A commit through the facade: refused in a session that takes no
    /// write, made with the calls of `rules` counted as hashes of the
    /// store, and settled.
    fn counted<R: HashRules>(
        &self,
        rules: &mut R,
        commit: impl FnOnce(&Committer<'_>, &mut Counted<'_, R>) -> Result<Committed, CommitError>,
    ) -> Result<Committed, WriteError> {
        self.refused()?;
        let mut rules = Counted { rules, calls: 0 };
        let committed = commit(&self.shared.committer(), &mut rules);
        self.shared.store.stats.count_hashes(rules.calls);
        self.settle(committed)
    }

    fn refused(&self) -> Result<(), WriteError> {
        match self.shared.store.refusal() {
            Some(reopen) => Err(WriteError::Reopen(reopen)),
            None => Ok(()),
        }
    }

    /// What the facade does with the end of a commit: the reclaimer is
    /// handed what the commit dropped, write-back is woken, and an outcome
    /// that ends the session for writes is recorded.
    fn settle(&self, committed: Result<Committed, CommitError>) -> Result<Committed, WriteError> {
        let shared = &*self.shared;
        let published = |published: &Published| {
            shared.dropped(published.version, &published.dropped);
            shared.write_back().wake();
        };
        match &committed {
            Ok(Committed::Published(commit)) => published(commit),
            Err(CommitError::NotDurable {
                published: commit,
                source,
            }) => {
                published(commit);
                shared.store.refuse(Reopen::NotDurable {
                    version: commit.version,
                    reason: source.to_string(),
                });
            }
            _ => {}
        }
        // `Halted`, `Unsettled` and a failed commit that found a group it
        // never published have stopped the commit path.
        let _ = shared.store.refusal();
        committed.map_err(WriteError::Commit)
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        // What a close reports is dropped here: a handle whose directory
        // was removed under it closes without a word (P-18).
        let _ = self.shut();
    }
}

/// Starts a thread of a database's background work.
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, OpenError> {
    let builder = thread::Builder::new().name(name.to_owned());
    builder.spawn(work).map_err(OpenError::Thread)
}

/// The background loop of write-back, for as long as the session lives
/// (`lignin_store::flush`). Its view of the store has the tree layer's
/// word on the values of a page: their scan order, and the regrouping
/// before a base image. The page steps of a regrouping append their
/// records through a view that has no such word, since the two refer to
/// each other.
fn write_back(shared: &Shared) {
    let store = &*shared.store;
    let plain = shared.write_back();
    let regrouping = Regrouping::new(
        store.loader(),
        &store.pool,
        &store.values,
        store.page_size,
        shared.steps(),
        RegroupPolicy::default(),
    );
    let stores = Stores {
        page_values: Some(&regrouping),
        ..plain.stores()
    };
    let write_back = WriteBack::new(stores, &store.flush, store.page_size).unwrap_or(plain);
    write_back.run(|error| store.troubles.write_back_failed(&error));
}
