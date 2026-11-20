//! The commit of a write transaction, step by step (`transactions.md` CM3).

use std::{
    collections::{BTreeSet, VecDeque},
    fmt,
};

use lignin_format::{
    delta::DeltaRun, tree_page::Hash, value_log::ValuePointer, wal::GroupWriter, CommitVersion,
    FormatVersion, PageId,
};
use lignin_store::{
    cache::PrivateRef,
    flush::{FlushError, Room, WriteBack},
    table::{Link, LinkError, PageTable},
    values::{CountStep, ValueChange},
    wal::{GroupPlace, LogError, LogStore, SyncPolicy},
};
use lignin_tree::{
    hash::{HashRules, Refusal},
    node::NodeRef,
    value::{TreeValues, ValueSource},
};

use super::{
    error::{CommitError, Failure, Stopped},
    outcome::{Committed, Published},
    state::{CommitLock, CommitState, CommitStats, Counted},
};
use crate::{
    snapshot::OperationGuard,
    txn::{
        Batch, FlatFrames, KeySet, Parts, Sealed, TreeCopy, TreePages, TxnError, WritePath,
        WriteTxn,
    },
};

/// How many times a commit writes its group again, for another place,
/// before it gives up. The lock on the counts of live values keeps every
/// other append out, so a store that keeps its rules never needs one.
const MAX_PLACES: usize = 3;

/// The hash rules of a caller whose writers make every hash themselves, as
/// Merk does on existing GroveVersions: nothing is ever stale, so no rule
/// is asked, and a rule that is asked all the same refuses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStaleHashes;

impl NoStaleHashes {
    fn refuse<T>() -> Result<T, Refusal> {
        Err("a hash is stale where every hash is made by its writer".into())
    }
}

impl HashRules for NoStaleHashes {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        _: &[u8],
        _: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        Self::refuse()
    }

    fn kv_hash(&mut self, _: &[u8], _: &Hash) -> Result<Hash, Refusal> {
        Self::refuse()
    }

    fn node_hash(&mut self, _: NodeRef<'_>) -> Result<Hash, Refusal> {
        Self::refuse()
    }
}

/// The commit path of one database: its write path, its write-back, its
/// page table, its log, and what its commits share ([`CommitState`]).
///
/// It is a view, a few references that every thread copies. It is made
/// where the database is opened, of the structures of one store (TXN-07).
#[derive(Clone, Copy)]
pub struct Committer<'s> {
    path: WritePath<'s>,
    write_back: WriteBack<'s>,
    table: &'s PageTable,
    log: &'s LogStore,
    state: &'s CommitState,
}

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Committer<'static>>();
};

/// What one writing of a group found out for the steps after it.
struct Logged {
    /// The flat pages whose copies are as the pages were: they have no run
    /// and are discarded.
    unchanged_flat: BTreeSet<PageId>,
    /// The bytes of the value records of the group.
    value_bytes: u64,
    /// The value pointers the group lists as released, in ascending order.
    released: Vec<ValuePointer>,
}

/// What the install of a commit did to the pages.
#[derive(Default)]
struct Installed {
    created: Vec<PageId>,
    changed: usize,
    freed: Vec<PageId>,
    freed_unlinked: usize,
    tree_copies: usize,
    unchanged_tree: usize,
    unchanged_flat: usize,
}

/// One step of the install of a commit.
enum Step<'t> {
    /// A private copy becomes a page version, or is discarded because it
    /// is as its page was.
    Put {
        copy: PrivateRef<'t>,
        /// Whether the transaction made the page.
        made: bool,
        /// Whether it is a tree page.
        tree: bool,
        unchanged: bool,
    },
    /// A committed page is freed, and the copy that was kept of it goes.
    Free(PrivateRef<'t>),
    /// The id of a page that the transaction made and emptied again goes
    /// back to the allocator.
    Unused(PageId),
}

/// A page of a group, as the group says it.
enum PageEntry<'a, 's> {
    Tree(&'a TreeCopy<'s>),
    Flat,
    Freed,
}

impl<'s> Committer<'s> {
    /// The commit path over the write path `path` and the write-back
    /// `write_back`, with `table` and `log` the page table and the log
    /// that write-back works on. The caller vouches that all of them are
    /// of one store, and `state` is what that store's commits share.
    pub(crate) fn new(
        path: WritePath<'s>,
        write_back: WriteBack<'s>,
        table: &'s PageTable,
        log: &'s LogStore,
        state: &'s CommitState,
    ) -> Self {
        Self {
            path,
            write_back,
            table,
            log,
            state,
        }
    }

    /// The write-back of the store.
    pub fn write_back(&self) -> &WriteBack<'s> {
        &self.write_back
    }

    /// The write path that transactions are begun from.
    pub fn path(&self) -> &WritePath<'s> {
        &self.path
    }

    /// What the commits of the database share.
    pub fn state(&self) -> &'s CommitState {
        self.state
    }

    /// The counts of the session's commits.
    pub fn stats(&self) -> CommitStats {
        self.state.stats()
    }

    /// Commits `txn` (`transactions.md` section 8): evaluates its stale
    /// hashes with `rules`, appends one group to the log, installs its
    /// pages and publishes the next version. A transaction that has
    /// written nothing commits nothing and takes no version (CV2).
    ///
    /// **The commit ends the transaction** once it has taken it, however
    /// it ends from there (CM6): the transaction is empty afterwards, its
    /// savepoints are gone, and it can be written again. A commit that is
    /// refused before that leaves the transaction as it was
    /// ([`CommitError::leaves_transaction_open`]): a scope is open, the
    /// write set cannot be laid out over the latest version or a page of
    /// it could not be read, or the session takes no commit.
    ///
    /// It waits for the commit lock and for the lock on the counts of live
    /// values, and for nothing else of the store (CM8): not for a reader,
    /// and not for another open write transaction. The commit writes to no
    /// file but the log, and makes one append to it (CM7). The cut that an
    /// earlier failed append left to make comes before it, and the thread's
    /// turn for write-back after it: neither is under the commit lock (see
    /// the module documentation).
    ///
    /// A caller whose writers make every hash, as on existing
    /// GroveVersions, passes [`NoStaleHashes`].
    pub fn commit<R: HashRules>(
        &self,
        txn: &WriteTxn<'_>,
        rules: &mut R,
    ) -> Result<Committed, CommitError> {
        self.commit_covering(txn, rules, None)
    }

    /// As [`Self::commit`], and the key set of the commit holds `also`
    /// beside the keys of the transaction's writes (CF1, CF5): the range a
    /// load put records into, or every key for a wipe. So the commit meets
    /// every write that an open transaction made to a key of `also`
    /// before it.
    ///
    /// A transaction that has written nothing commits nothing, with or
    /// without `also`.
    pub fn commit_covering<R: HashRules>(
        &self,
        txn: &WriteTxn<'_>,
        rules: &mut R,
        also: Option<&KeySet>,
    ) -> Result<Committed, CommitError> {
        // A transaction is committed where it was begun. One of another
        // database is left as it is.
        let path = *txn.path();
        if !std::ptr::eq(path.versions, self.path.versions)
            || !std::ptr::eq(path.pool, self.path.pool)
        {
            return Err(CommitError::Refused(TxnError::Damaged(
                "the transaction was begun from the write path of another database",
            )));
        }
        // CM1, and step 1 of CM3: neither takes the commit lock.
        match txn.seal_empty() {
            Ok(false) => {}
            Ok(true) => return Ok(self.empty()),
            Err(TxnError::NotFinalized) => return Err(CommitError::NotFinalized),
            Err(_) => return Err(CommitError::ScopesOpen),
        }
        if let Some(stopped) = self.state.stopped() {
            return Err(CommitError::Stopped(stopped));
        }
        self.look_at_log(true)?;
        let mut untold = None;
        let committed = self.commit_locked(txn, &path, rules, also, |_| Ok(()), &mut untold);
        // No lock is held here: whoever is told may call the store.
        if let Some(error) = untold {
            self.state.write_back_failed(&error);
        }
        let open = matches!(&committed, Err(error) if error.leaves_transaction_open());
        if !open {
            // The transaction is ended, and its copies are gone: this is
            // where the thread waits for write-back, if it has to (FL5).
            self.after();
        }
        committed
    }

    /// Steps 2 to 8 of CM3. The commit lock is taken here, and given back
    /// on every way out. `untold` is given what write-back answered when
    /// it could not take the group in, for the caller to tell once the
    /// locks are given back.
    ///
    /// `write` is run on the transaction once the lock is held and the
    /// log was looked at, before the transaction is taken: a commit that
    /// nothing is committed beside writes there
    /// ([`Self::commit_alone`]), and every other commit writes nothing.
    fn commit_locked<'t, R: HashRules>(
        &self,
        txn: &WriteTxn<'t>,
        path: &WritePath<'t>,
        rules: &mut R,
        also: Option<&KeySet>,
        write: impl FnOnce(&WriteTxn<'t>) -> Result<(), TxnError>,
        untold: &mut Option<FlushError>,
    ) -> Result<Committed, CommitError> {
        #[cfg(test)]
        self.state.before_lock();
        let lock = self.state.lock();
        if let Some(stopped) = self.state.stopped() {
            return Err(CommitError::Stopped(stopped));
        }
        // An append of write-back can have failed since the log was looked
        // at. The transaction is still whole here, so the commit is
        // refused. An append that fails from here until the commit has
        // the lock on the counts ends the commit instead.
        self.look_at_log(false)?;
        write(txn).map_err(CommitError::Refused)?;
        let parts = match txn.seal() {
            Ok(Sealed::Ready(parts)) => *parts,
            Ok(Sealed::Empty) => return Ok(self.empty()),
            Ok(Sealed::Conflict) => {
                self.state.count(Counted::Conflict);
                return Err(CommitError::Conflict);
            }
            Err(TxnError::ScopeOpen) => return Err(CommitError::ScopesOpen),
            Err(TxnError::NotFinalized) => return Err(CommitError::NotFinalized),
            // The write set cannot be laid out over the latest version, or
            // a page of it could not be read. The transaction is as it
            // was, its savepoints included.
            Err(error) => return Err(CommitError::Refused(error)),
        };
        self.make(lock, path, parts, rules, also, untold)
    }

    /// Looks at the log before a commit takes its transaction, so that a
    /// log that takes no group refuses the commit with the transaction
    /// whole.
    ///
    /// - **The log cannot say whether an item is in it** that write-back
    ///   appended: a page step's record or a checkpoint's mark that was
    ///   taken back and could not be settled. The log takes nothing more,
    ///   and the session stops as it does for a commit's own group
    ///   (CM9 (d)).
    /// - **An append failed, and its bytes are not cut off.** The log
    ///   takes nothing until the file is named closed where its last whole
    ///   item ends and the log goes on in its next file (CM9 item 4). That
    ///   is made before the next commit and not by one, by whoever keeps
    ///   the log's files going, which is write-back. Where that has not
    ///   come by since, the thread that is about to commit has it done
    ///   here, with `cut`, while it holds no lock. While the cut cannot be
    ///   made, for example because the disk is still full, no commit is
    ///   taken (CM9 item 5).
    fn look_at_log(&self, cut: bool) -> Result<(), CommitError> {
        let unsettled = |error: &LogError| CommitError::Stopped(self.log_unsettled(error));
        match self.log.next_group() {
            Err(error @ LogError::Unsettled { .. }) => Err(unsettled(&error)),
            Err(LogError::NoActiveFile) if cut => match self.write_back.roll_log() {
                Ok(_) => Ok(()),
                Err(FlushError::Log(error @ LogError::Unsettled { .. })) => Err(unsettled(&error)),
                Err(error) => Err(CommitError::NotReady(error)),
            },
            Err(error @ LogError::NoActiveFile) => Err(CommitError::NotReady(error.into())),
            // Whatever else the log has to say, it says to the commit.
            _ => Ok(()),
        }
    }

    /// Stops the session because the log cannot say whether an item is in
    /// it, and gives the reason the session is stopped for.
    fn log_unsettled(&self, error: &LogError) -> Stopped {
        let latest = self.path.versions.latest();
        self.state.stop(Stopped::Unsettled {
            version: latest.next().unwrap_or(latest),
            reason: error.to_string(),
        })
    }

    /// What a committer does for write-back once its transaction has
    /// ended, by a commit or by a rollback, and its thread holds no lock
    /// and no private copy (`lignin_store::flush`, "What the caller
    /// does"): the values that waited for its copies are placed, and it
    /// waits while write-back is over its bounds, which is what bounds the
    /// log when commits come back to back.
    ///
    /// An error of write-back here changes nothing of the commit: it is
    /// counted and told to whoever the [`CommitState`] reports to.
    fn after(&self) {
        if let Err(error) = self.write_back.place_ready() {
            self.state.write_back_failed(&error);
        }
        match self.write_back.wait_for_room() {
            Ok(Room::Free) => {}
            Ok(Room::Stalled) => self.state.count(Counted::Stall),
            Err(error) => self.state.write_back_failed(&error),
        }
    }

    /// A write with no transaction (`transactions.md` RD7): `batch` is
    /// applied to a write transaction of its own, which is committed at
    /// once. It waits for no open write transaction, and none sees it
    /// before it is published. Its commit follows every rule of a commit,
    /// the conflict rule among them (CF8).
    ///
    /// Whatever the error, nothing of the batch is left to commit later:
    /// the transaction is the write's own and goes with it. A batch that
    /// the transaction refuses is [`CommitError::Refused`].
    pub fn write<R: HashRules>(
        &self,
        batch: &Batch<'_>,
        rules: &mut R,
    ) -> Result<Committed, CommitError> {
        let txn = self.path.begin();
        let applied = txn.scope().and_then(|scope| {
            // A scope that is dropped is rolled back.
            scope.apply(batch)?;
            scope.merge();
            Ok(())
        });
        let committed = match applied {
            Ok(()) => self.commit(&txn, rules),
            Err(error) => Err(CommitError::Refused(error)),
        };
        if matches!(&committed, Err(error) if error.leaves_transaction_open()) {
            // The transaction goes with what it copied, as by a rollback.
            drop(txn);
            self.after();
        }
        committed
    }

    /// A commit that no other commit comes between: `write` is given a
    /// write transaction of its own while the commit lock is held, and
    /// what it writes is committed under that same hold of the lock. So
    /// what `write` reads of the latest version is what the commit is made
    /// over, and the version it publishes is the one after it. The key set
    /// of the commit holds `also` beside the keys of the writes
    /// ([`Self::commit_covering`]).
    ///
    /// It is for the one write that has to see everything it replaces: a
    /// wipe, which drops every subtree the latest version has and must
    /// leave none that a commit beside it entered. `write` runs under the
    /// commit lock, so it calls the transaction it is given and reads
    /// committed pages, and commits nothing itself.
    ///
    /// The transaction is the commit's own, so whatever the error nothing
    /// of it is left: a write that `write` refuses is
    /// [`CommitError::Refused`], and nothing was written.
    pub(crate) fn commit_alone<R: HashRules>(
        &self,
        rules: &mut R,
        also: Option<&KeySet>,
        write: impl FnOnce(&WriteTxn<'s>) -> Result<(), TxnError>,
    ) -> Result<Committed, CommitError> {
        if let Some(stopped) = self.state.stopped() {
            return Err(CommitError::Stopped(stopped));
        }
        self.look_at_log(true)?;
        let txn = self.path.begin();
        let mut untold = None;
        let committed = self.commit_locked(&txn, &self.path, rules, also, write, &mut untold);
        if let Some(error) = untold {
            self.state.write_back_failed(&error);
        }
        // The transaction goes with what it copied, whatever the outcome.
        drop(txn);
        self.after();
        committed
    }

    /// Rolls `txn` back (`transactions.md` WT9): its write set, its
    /// private copies and its version pin go, and the page ids it took
    /// return to the allocator. Nothing is written to a file and no
    /// committed page is read, so it costs what the transaction holds and
    /// nothing that depends on the size of the database.
    ///
    /// It is refused while a scope is open, and the transaction is then as
    /// it was.
    ///
    /// Afterwards the thread does for write-back what it does after a
    /// commit: what waited for the transaction's copies is placed, and it
    /// waits while write-back is over its bounds.
    pub fn rollback(&self, txn: &WriteTxn<'_>) -> Result<(), TxnError> {
        txn.rollback()?;
        self.state.count(Counted::Rollback);
        self.after();
        Ok(())
    }

    fn empty(&self) -> Committed {
        self.state.count(Counted::Empty);
        Committed::Empty
    }

    fn failed(&self, failure: Failure) -> CommitError {
        self.state.count(Counted::Failed);
        CommitError::Failed(failure)
    }

    /// What a commit is whose group is in the log and that could not be
    /// installed or published: the session stops.
    fn halted(&self, version: CommitVersion, source: Failure) -> CommitError {
        self.state.stop(Stopped::Halted {
            version,
            reason: source.to_string(),
        });
        CommitError::Halted { version, source }
    }

    /// Steps 5 to 8 of CM3, for the frames `parts` over the latest
    /// version. Whatever of `parts` is dropped on the way out gives its
    /// private copies and its page ids up, as a rollback does.
    ///
    /// `path` is the write path the transaction was begun from, which is
    /// this commit path's own: its lifetime is the one the frames have.
    fn make<'t, R: HashRules>(
        &self,
        mut lock: CommitLock<'_>,
        path: &WritePath<'t>,
        parts: Parts<'t>,
        rules: &mut R,
        also: Option<&KeySet>,
        untold: &mut Option<FlushError>,
    ) -> Result<Committed, CommitError> {
        let Parts {
            base,
            pin,
            mut tree,
            mut values,
            mut hashes,
            flat,
            directory,
            dropped,
            mut keys,
        } = parts;
        // The commit lock is held, so the latest version is the one the
        // frames were made over, and stays so until this publishes.
        let latest = path.versions.latest();
        let Some(version) = base.next().filter(|_| base == latest) else {
            let moved = TxnError::Damaged("a version was published under the commit lock");
            return Err(self.failed(moved.into()));
        };

        // Step 5: no stored hash differs from its recomputation.
        let source = ValueSource::new(path.values, path.loader, base);
        let evaluated = hashes.len();
        if evaluated > 0 {
            hashes
                .evaluate_all(&mut tree, &mut values.reader(&source), rules)
                .map_err(|error| self.failed(error.into()))?;
        }

        // Step 6. The lock on the counts is taken before the log is asked
        // for the place of the group, and held until the commit is counted
        // (LT7): nothing else is appended meanwhile (`log-format.md` LG22).
        let guard = path.versions.guard();
        let mut flat = flat.for_commit(&guard);
        let counts = (path.values.step()).map_err(|error| self.failed(error.into()))?;
        let mut places = 0;
        let (appended, logged) = loop {
            let at = match self.log.next_group() {
                Ok(at) => at,
                Err(error) => return Err(self.not_appended(counts, version, error)),
            };
            if at.version() != version {
                // The log holds a group that this session did not publish,
                // or is not this database's. No commit can be made over
                // it, so the session stops: this commit has failed, and
                // the next ones are refused with their transactions whole.
                let gap = Failure::VersionGap {
                    log: at.version(),
                    latest,
                };
                self.state.stop(Stopped::Halted {
                    version,
                    reason: gap.to_string(),
                });
                return Err(self.failed(gap));
            }
            let group = &mut lock.scratch.group;
            let logged = Self::write_group(group, &at, &mut tree, &mut values, &source, &mut flat)
                .map_err(|failure| self.failed(failure))?;
            match self.log.append_group(&at, &lock.scratch.group) {
                Ok(appended) => break (appended, logged),
                Err(LogError::PlaceTaken { .. }) if places < MAX_PLACES => {
                    places += 1;
                    self.state.count(Counted::Rewritten);
                }
                Err(LogError::PlaceTaken { .. }) => return Err(self.failed(Failure::PlaceLost)),
                Err(error) => return Err(self.not_appended(counts, version, error)),
            }
        };
        let group_len = lock.scratch.group.len() as u64;
        // Write-back is told before the first page is installed, and the
        // commit goes on whatever it answers: what it was not told costs a
        // base image where a run would do, and nothing else.
        let told = (self.write_back).committed(version, appended.at, &lock.scratch.group);
        *untold = told.err();

        // Step 7, in memory only. The group is in the log, so the commit
        // has happened (CM9): nothing here is let to fail.
        let value_pages = values.pages().count();
        let installed = match self.install(path, version, tree, flat, &logged.unchanged_flat) {
            Ok(installed) => installed,
            Err(source) => {
                drop(counts);
                return Err(self.halted(version, source));
            }
        };
        drop(guard);
        let mut change = ValueChange::new();
        for &pointer in &logged.released {
            change.release(pointer);
        }
        // The count is the last thing done under the lock on the counts,
        // which this gives back.
        let miscounted = counts.apply(&change, version).err();

        // The fsync of the commit, if the sync policy asks for one (CM7):
        // before anything is published, and under no lock but the commit
        // lock.
        let synced = match self.log.options().sync {
            SyncPolicy::Never => Ok(None),
            SyncPolicy::EveryCommit => self.write_back.sync_log().map(Some),
            SyncPolicy::Interval(_) => self.write_back.sync_log_if_due(),
        };

        // Step 8. The key set is recorded before the version is published,
        // so that whoever reads the version finds it (CF3).
        if let Some(also) = also {
            keys.merge(also);
        }
        // The pin is on the latest version until the next line: giving it
        // up holds nothing back and tells nobody.
        drop(pin);
        path.commits.record(version, keys);
        // The handle on the subtree directory of the new version is kept
        // before the version is published: whoever reads the version
        // finds its handle, and loads none.
        path.directories().set(version, directory.clone());
        #[cfg(test)]
        self.state.before_publication(version);
        if let Err(error) = path.versions.publish(version) {
            return Err(self.halted(version, error.into()));
        }
        if let Err(error) = &synced {
            // Under the commit lock, so that no commit that waits for it
            // is published behind a sync that failed.
            self.state.sync_failed(version, error);
        }
        drop(lock);

        self.state.count(Counted::Published { group_len });
        path.versions.reclaim();
        let Installed {
            created,
            changed,
            freed,
            freed_unlinked,
            tree_copies,
            unchanged_tree,
            unchanged_flat,
        } = installed;
        let mut published = Box::new(Published {
            version,
            at: appended.at,
            end: appended.end,
            group_len,
            durable: None,
            created,
            changed,
            freed,
            freed_unlinked,
            tree_copies,
            unchanged_tree,
            unchanged_flat,
            value_pages,
            value_bytes: logged.value_bytes,
            released: logged.released.len(),
            evaluated,
            dropped,
            directory,
            miscounted,
        });
        match synced {
            Ok(durable) => {
                published.durable = durable;
                Ok(Committed::Published(published))
            }
            Err(source) => {
                self.state.count(Counted::NotDurable);
                Err(CommitError::NotDurable { published, source })
            }
        }
    }

    /// Writes the group of a commit into `out`, for the place `at` that
    /// the log ends at now: the values, the pages in ascending order, and
    /// the value pointers that cells stopped holding (`log-format.md`
    /// LG12, LG14). Every cell that holds a value of the commit names the
    /// value's place in this group afterwards, and the bytes are with the
    /// frame of its page under that name.
    ///
    /// It can be called again, for another place: every call writes and
    /// names the values anew (LG22).
    fn write_group<'t, 'g>(
        out: &mut Vec<u8>,
        at: &GroupPlace,
        tree: &mut TreePages<'t>,
        values: &mut TreeValues,
        source: &ValueSource<'_>,
        flat: &mut FlatFrames<'t, 'g, OperationGuard<'t>>,
    ) -> Result<Logged, Failure> {
        out.clear();
        let mut group = GroupWriter::new(at.version(), at.address(), out)?;
        values.log_values(tree, source, &mut group)?;
        flat.log_values(&mut group)?;
        let value_bytes = group.encoded_len() as u64;

        let written = tree.written().filter(|(_, copy)| !copy.is_unchanged());
        let mut pages: Vec<(PageId, PageEntry<'_, 't>)> = written
            .map(|(page, copy)| (page, PageEntry::Tree(copy)))
            .chain(tree.freed().map(|page| (page, PageEntry::Freed)))
            .chain(flat.written().map(|page| (page, PageEntry::Flat)))
            .chain(flat.freed().map(|page| (page, PageEntry::Freed)))
            .collect();
        pages.sort_unstable_by_key(|(page, _)| *page);
        let mut unchanged_flat = BTreeSet::new();
        for (page, entry) in pages {
            match entry {
                PageEntry::Tree(copy) => {
                    group.tree_run(&DeltaRun::decode(&copy.run, FormatVersion::CURRENT)?)?;
                }
                PageEntry::Flat => {
                    if !flat.log_page(page, &mut group)? {
                        unchanged_flat.insert(page);
                    }
                }
                PageEntry::Freed => group.freed(page)?,
            }
        }

        let mut released = values.released_pointers().to_vec();
        released.extend_from_slice(flat.released());
        released.sort_unstable_by_key(|pointer| (pointer.file(), pointer.offset()));
        for pointer in &released {
            group.released(pointer)?;
        }
        group.finish();

        // The bytes of the tree cells' values go with their frames now,
        // while the commit can still fail: the install is then left with
        // nothing that can.
        for (_, copy) in tree.written().filter(|(_, copy)| !copy.is_unchanged()) {
            let mut latch = copy.frame.write().map_err(TxnError::from)?;
            values.attach(&mut latch)?;
        }
        Ok(Logged {
            unchanged_flat,
            value_bytes,
            released,
        })
    }

    /// What a commit is whose append the log did not take (CM9).
    fn not_appended(
        &self,
        counts: CountStep<'_>,
        version: CommitVersion,
        error: LogError,
    ) -> CommitError {
        // The lock on the counts is given back before the log is asked to
        // settle, which syncs.
        drop(counts);
        let unsettled = |source: LogError| {
            self.state.stop(Stopped::Unsettled {
                version,
                reason: source.to_string(),
            });
            CommitError::Unsettled { version, source }
        };
        match error {
            // The file has the length it had. Until that is durable, a
            // crash can bring the group back, so the commit has failed
            // only once the log has settled it.
            LogError::TakenBack { .. } => match self.log.settle() {
                Ok(()) => self.failed(error.into()),
                Err(source) => unsettled(source),
            },
            // The log cannot say whether an item before the group is in
            // it, and took the group no more.
            LogError::Unsettled { .. } => unsettled(error),
            // The group is not in the log, and no crash brings it back: a
            // part of it stays and is no item, or nothing of it reached
            // the file.
            _ => self.failed(error.into()),
        }
    }

    /// Step 7 of CM3: the private copies become the page versions that
    /// `version` bore, the ids of the new pages are published, and the
    /// freed pages are freed. Nothing of it is visible before the version
    /// is published.
    ///
    /// Where a part of the store refuses a step, which no rule lets
    /// happen, what the install has not come to is given up as by a
    /// rollback: no copy stays private and no id stays with the
    /// transaction. The page versions that were installed stay in the
    /// pool, where no reader asks for them.
    fn install<'t, 'g>(
        &self,
        path: &WritePath<'t>,
        version: CommitVersion,
        tree: TreePages<'t>,
        flat: FlatFrames<'t, 'g, OperationGuard<'t>>,
        unchanged_flat: &BTreeSet<PageId>,
    ) -> Result<Installed, Failure> {
        let (tree, flat) = (tree.into_parts(), flat.into_parts());
        let tree_written = tree.written.into_iter().map(|(_, copy)| Step::Put {
            unchanged: copy.is_unchanged(),
            made: copy.created,
            tree: true,
            copy: copy.frame,
        });
        let flat_written = flat.written.into_iter().map(|(copy, made)| Step::Put {
            unchanged: unchanged_flat.contains(&copy.page()),
            made,
            tree: false,
            copy,
        });
        let freed = (tree.freed.into_iter().map(|(_, copy)| copy)).chain(flat.freed);
        let unused = tree.unused.into_iter().chain(flat.unused);
        let mut steps: VecDeque<Step<'t>> = tree_written
            .chain(flat_written)
            .chain(freed.map(Step::Free))
            .chain(unused.map(Step::Unused))
            .collect();

        let mut done = Installed::default();
        while let Some(step) = steps.front() {
            if let Err(failure) = self.install_step(path, version, step, &mut done) {
                for step in &steps {
                    Self::give_up(path, step);
                }
                return Err(failure);
            }
            steps.pop_front();
        }
        done.created.sort_unstable();
        done.freed.sort_unstable();
        Ok(done)
    }

    fn install_step(
        &self,
        path: &WritePath<'_>,
        version: CommitVersion,
        step: &Step<'_>,
        done: &mut Installed,
    ) -> Result<(), Failure> {
        let (pool, ids) = (path.pool, path.ids);
        match step {
            Step::Put {
                copy,
                made,
                tree,
                unchanged,
            } => {
                let page = copy.page();
                done.tree_copies += usize::from(*tree);
                if *unchanged {
                    match tree {
                        true => done.unchanged_tree += 1,
                        false => done.unchanged_flat += 1,
                    }
                    pool.discard(copy)?;
                    return Ok(());
                }
                drop(pool.install(copy, version)?);
                if *made {
                    ids.publish(page)?;
                    done.created.push(page);
                } else {
                    done.changed += 1;
                }
            }
            Step::Free(copy) => {
                let page = copy.page();
                // The copy kept every page step off the page until here,
                // under the lock on the counts.
                pool.discard(copy)?;
                // See "A page that is freed again before it was written
                // back" in the module documentation.
                let free = || match self.table.push(page, Link::free(version)) {
                    Ok(()) => Ok(false),
                    Err(LinkError::AlreadyFree) => Ok(true),
                    Err(error) => Err(error),
                };
                let unlinked = free()?;
                pool.free(page, version)?;
                // A write-back pass can have linked the page behind the
                // first push, and none does once the pool is told: the
                // free link then goes above that link.
                if unlinked && free()? {
                    done.freed_unlinked += 1;
                }
                ids.free(page, version)?;
                done.freed.push(page);
            }
            Step::Unused(page) => ids.release(*page)?,
        }
        Ok(())
    }

    /// Gives up what a step of an install that failed would have used. A
    /// copy that the pool still has as private goes, and with it the id of
    /// the page it would have made. What the step that failed had done
    /// before it was refused stays done: the pool and the allocator refuse
    /// a copy that is private no more and an id that is not the
    /// transaction's, and that is all this asks of them.
    fn give_up(path: &WritePath<'_>, step: &Step<'_>) {
        let (pool, ids) = (path.pool, path.ids);
        match step {
            Step::Put { copy, made, .. } => {
                if pool.discard(copy).is_ok() && *made {
                    let _ = ids.release(copy.page());
                }
            }
            Step::Free(copy) => {
                let _ = pool.discard(copy);
            }
            Step::Unused(page) => {
                let _ = ids.release(*page);
            }
        }
    }
}

impl fmt::Debug for Committer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Committer")
            .field("path", &self.path)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}
