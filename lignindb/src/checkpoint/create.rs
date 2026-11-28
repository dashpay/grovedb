//! The creation of a checkpoint, step by step (`transactions.md` CK1 to
//! CK10).

use std::{
    cmp::Ordering,
    fmt,
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError, TryLockError},
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest, TableCheckpoint},
    wal::LogPlace,
    CommitVersion, FileKind,
};
use lignin_store::{
    flush::{Checkpointed, FlushError, WriteBack},
    segment::{FileNames, ManifestStore},
    values::ValueHold,
    wal::LogHold,
};

use super::{error::CheckpointError, files::Target};
use crate::{
    commit::{CommitLock, CommitState, Committer},
    snapshot::{VersionPin, Versions},
};

/// How much log a checkpoint leaves for its first open to replay before
/// write-back is made to catch up first, unless
/// [`CheckpointOptions::catch_up_above`] says another amount: 1 MiB.
///
/// It is about where the two cost the same. On a store of the reference
/// database's size (`tests/reference.rs`) the first open of a checkpoint
/// took some 25 ms longer for each megabyte of log it replayed, which is
/// ten blocks of twenty documents, and a catch-up took a creation 15 to
/// 30 ms. So a creation that finds more than a megabyte pays for the
/// catch-up, and the open of its checkpoint replays only what was
/// committed after write-back had caught up.
pub const DEFAULT_CATCH_UP_ABOVE: u64 = 1 << 20;

/// How a checkpoint is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointOptions {
    /// Write-back catches up before the checkpoint is sampled when it
    /// holds more log than this many bytes: the log from the oldest commit
    /// that no table checkpoint covers
    /// ([`WriteBack::log_held`]), which is what the first open of the
    /// checkpoint would replay. `None` leaves the catch-up out. The
    /// settling of a manifest in doubt is asked of write-back all the
    /// same.
    pub catch_up_above: Option<u64>,
}

impl Default for CheckpointOptions {
    fn default() -> Self {
        Self {
            catch_up_above: Some(DEFAULT_CATCH_UP_ABOVE),
        }
    }
}

/// A checkpoint that was made.
#[derive(Debug)]
pub struct Created {
    /// The committed version the checkpoint holds, and the version its
    /// first open lands on (CK1).
    pub version: CommitVersion,
    /// Where the checkpoint's log ends: the end of the live log at the
    /// sampling step, or, where the live log was durable through the
    /// version already, the place it was durable to. `None` for a log with
    /// no record.
    pub log_end: Option<LogPlace>,
    /// The table checkpoint its manifest names, which is the one the live
    /// manifest named at the sampling step.
    pub table: Option<TableCheckpoint>,
    /// The files it shares with the live database as hard links.
    pub linked: usize,
    /// The files that were copied because no link could be made.
    pub copied: usize,
    /// The bytes of those copies.
    pub copied_bytes: u64,
    /// What became of write-back's catch-up before the sampling step, if
    /// one was due ([`CheckpointOptions::catch_up_above`]). An error here
    /// changed nothing of the checkpoint but how much log its first open
    /// replays.
    pub catch_up: Option<Result<Checkpointed, FlushError>>,
}

/// A point of a creation at which the tests let other work in.
#[cfg(all(test, not(loom)))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// The directory the checkpoint is built in is made, and nothing is
    /// sampled.
    Made,
    /// The sample found the manifest locked. The commit lock is given
    /// back, and the creation is about to wait for the manifest.
    Waiting,
    /// The sample is taken and the commit lock is given back. The lock on
    /// the manifest is held, and nothing is in the directory yet.
    Linking,
    /// The lock on the manifest is given back too. The log is not synced
    /// yet and no log file is in the directory.
    Sampled,
    /// Every file is in the directory, or open to be copied. The pin and
    /// the holds are given back, and the manifest is not written.
    Secured,
    /// Everything in the directory is durable, and so is the name it is
    /// built under. The rename has not been made.
    Settled,
}

#[cfg(all(test, not(loom)))]
type Pause<'s> = &'s (dyn Fn(Step) + Sync);

/// What makes checkpoints of one open database: a view of its commit
/// path, as a [`Committer`] is.
///
/// See the [module documentation](super) for what a checkpoint is, for the
/// steps of a creation and for what the live database is held to
/// meanwhile.
#[derive(Clone, Copy)]
pub struct Checkpointer<'s> {
    versions: &'s Versions,
    state: &'s CommitState,
    write_back: WriteBack<'s>,
    options: CheckpointOptions,
    carried: &'s [&'s str],
    #[cfg(all(test, not(loom)))]
    pause: Option<Pause<'s>>,
}

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Checkpointer<'static>>();
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a creation reads in its sampling step, and what it holds in the
/// live database from then until its files are in the checkpoint's
/// directory (CK3).
struct Sample<'s> {
    /// The latest version.
    version: CommitVersion,
    /// Where the checkpoint's log ends: behind the group of the version,
    /// at the end of a record.
    end: Option<LogPlace>,
    /// Whether the log is durable to there already.
    durable: bool,
    /// The durable manifest.
    named: Manifest,
    /// The pin on the version, the hold on the whole log, and the hold on
    /// the value-log files, which the value log asks a checkpoint to take
    /// (`lignin_store::values`).
    held: (VersionPin<'s>, LogHold, ValueHold),
}

/// What the sampling step reads and takes under the commit lock.
struct Read<'s> {
    version: CommitVersion,
    end: Option<LogPlace>,
    durable: bool,
    named: Manifest,
    pin: VersionPin<'s>,
    log_hold: LogHold,
}

impl<'s> Checkpointer<'s> {
    /// What makes checkpoints of the database that `committer` commits to,
    /// with the default options.
    pub fn new(committer: &Committer<'s>) -> Self {
        Self {
            versions: committer.path().versions(),
            state: committer.state(),
            write_back: *committer.write_back(),
            options: CheckpointOptions::default(),
            carried: &[],
            #[cfg(all(test, not(loom)))]
            pause: None,
        }
    }

    /// The same, with `options`.
    pub fn with_options(mut self, options: CheckpointOptions) -> Self {
        self.options = options;
        self
    }

    /// The same, and every checkpoint also carries the files `names` of
    /// the database directory, each whole and under its own name: what
    /// the engine keeps beside the store's files and needs to open a
    /// database, if it keeps any such thing.
    ///
    /// Each is the plain name of a file in the database directory: not of
    /// a directory or of a link to a file, and not the manifest's or the
    /// lock's ([`CheckpointError::CarriedName`]). The engine vouches that
    /// such a file is durable and is never written again: a hard link
    /// shares every later change, and a copy is of the bytes the file has
    /// when the checkpoint is made.
    pub fn carrying(mut self, names: &'s [&'s str]) -> Self {
        self.carried = names;
        self
    }

    #[cfg(all(test, not(loom)))]
    pub(super) fn pausing(mut self, pause: Pause<'s>) -> Self {
        self.pause = Some(pause);
        self
    }

    #[cfg(all(test, not(loom)))]
    fn reached(&self, step: Step) {
        if let Some(pause) = self.pause {
            pause(step);
        }
    }

    /// The options checkpoints are made with.
    pub fn options(&self) -> CheckpointOptions {
        self.options
    }

    /// Makes a checkpoint of the database in the new directory `target`:
    /// the state of the version that is the latest at the sampling step,
    /// with every commit up to it and nothing of any later one (CK1).
    ///
    /// The directory above `target` is there, and `target` is not: this
    /// makes it, and refuses one that exists with nothing touched
    /// ([`CheckpointError::TargetExists`]). It can lie inside the database
    /// directory, and on another file system.
    ///
    /// **This call puts nothing at `target` but the whole checkpoint.** The
    /// checkpoint is built beside it, in the staging directory of the
    /// creation: a directory of the target's name with
    /// [`CREATING_SUFFIX`](super::CREATING_SUFFIX) added, in which the
    /// directory it is built in
    /// ([`BUILDING_DIR_NAME`](super::BUILDING_DIR_NAME)) is renamed to
    /// `target` when everything in it is durable. That name is reserved
    /// for creation. **A directory already there under it is built in
    /// only where it lists what a creation puts there and nothing else**:
    /// a database, or anything else that lies there, fails the call and
    /// is as it was ([`CheckpointError::NotStaging`]). Before building in
    /// what an earlier creation left, the call takes its lock, reads the
    /// directory again under that lock, syncs the directory above and the
    /// directory itself, reads it once more, and removes what was built
    /// in it. Where its lock is found held, as by another creation that
    /// is building in it, the call fails
    /// ([`CheckpointError::Creating`]).
    ///
    /// **When this returns `Ok`, the checkpoint is durable, and so is its
    /// version in the live database** (CK4). `target` is then a database
    /// directory like any other, and is opened as one.
    ///
    /// This call holds the commit lock for the sampling step, which reads
    /// a few things, and for nothing else (CK5). A commit that syncs the
    /// log itself waits, as the log has it, for a sync of the log that is
    /// under way. This call makes one only where the log is not durable
    /// through the version already, and write-back makes one where it
    /// catches up first. And a commit that found the manifest in doubt
    /// waits for the manifest to be settled, which this call does where
    /// it comes first. The [module documentation](super) says when a
    /// commit can meet each. Open write transactions are not
    /// waited for, and nothing of them is in the checkpoint (CK8). Any
    /// number of checkpoints may be made at once (CK9).
    ///
    /// If it fails, the live database is as write-back's catch-up and the
    /// settling of its manifest left it, but that its log may be durable
    /// further (CK10). Where the sync of its log failed
    /// ([`CheckpointError::Sync`]), the log makes nothing more durable
    /// until the database is opened again, as after any sync of it that
    /// fails (`lignin_store::wal`). What was made is taken away again as
    /// far as the file layer lets it, and what it does not let go stays
    /// in the staging directory, for the next creation at `target` or a
    /// [`delete`](super::delete) to remove. Nothing is at `target` then,
    /// with one exception: where the directory had that name already, the
    /// name could not be made durable, and it could not be taken back
    /// either, `target` is the whole checkpoint, and is not known to be
    /// durable. Where it was taken back and that could not be made durable,
    /// the whole checkpoint is in the staging directory, and a power cut
    /// can leave it at `target`.
    pub fn create(&self, target: &Path) -> Result<Created, CheckpointError> {
        if let Some(stopped) = self.state.stopped() {
            return Err(CheckpointError::Stopped(stopped));
        }
        let stores = self.write_back.stores();
        let (vfs, source) = {
            let manifest = lock(stores.manifest);
            (manifest.vfs().clone(), manifest.dir().to_path_buf())
        };
        // The catch-up comes before the directory is made, so that
        // nothing of a checkpoint is on disk for as long as it takes. A
        // target that is there is refused before it, and again before the
        // directory gets its name.
        Target::refuse_existing(&*vfs, target)?;
        let catch_up = self.catch_up();
        (self.write_back.settle_manifest()).map_err(CheckpointError::Settle)?;
        let mut made = Target::make(vfs, target)?;
        #[cfg(all(test, not(loom)))]
        self.reached(Step::Made);
        let created = self.fill(&mut made, &source, catch_up);
        match created {
            Ok(_) => made.leave(),
            Err(_) => made.remove(),
        }
        created
    }

    /// Write-back's catch-up, if it holds more log than the checkpoint is
    /// to leave for its first open (CK6): a pass and a table checkpoint,
    /// as its own loop takes them. It is put off, and not waited for,
    /// while an open write transaction holds a page back.
    fn catch_up(&self) -> Option<Result<Checkpointed, FlushError>> {
        let above = self.options.catch_up_above?;
        (self.write_back.log_held() > above).then(|| self.write_back.checkpoint())
    }

    /// The sampling step (CK2): the latest version, the end of the log and
    /// the durable manifest, read under the commit lock and the lock on
    /// the manifest, which is the file-set lock (levels 1 and 3 of LT7).
    /// No commit is published, no manifest is replaced and no file of the
    /// store is deleted while both are held.
    ///
    /// It returns with the commit lock given back and the lock on the
    /// manifest still held. The value-log files are held once the commit
    /// lock is given back: none of them is deleted but under the lock on
    /// the manifest, and the hold looks at every file the value log has.
    ///
    /// **The sample never waits for the manifest while it holds the
    /// commit lock.** Write-back holds the lock on the manifest over a
    /// sync when it replaces a manifest, and another creation holds it
    /// while it links. So the manifest is only tried under the commit
    /// lock: where it is taken, the commit lock is given back, the
    /// manifest is waited for, and both are taken anew. Behind the sample
    /// a commit so waits for the reads below (CK5, CM8). The tries are
    /// not counted: each one that fails has waited for a holder of the
    /// manifest that has finished, and a bound would have to wait for the
    /// manifest under the commit lock again.
    fn sample(&self) -> Result<(Sample<'s>, MutexGuard<'s, ManifestStore>), CheckpointError> {
        let stores = self.write_back.stores();
        let (commits, manifest) = loop {
            let commits = self.state.lock();
            match stores.manifest.try_lock() {
                Ok(manifest) => break (commits, manifest),
                Err(TryLockError::Poisoned(poisoned)) => break (commits, poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            drop(commits);
            #[cfg(all(test, not(loom)))]
            self.reached(Step::Waiting);
            drop(lock(stores.manifest));
        };
        let Read {
            version,
            end,
            durable,
            named,
            pin,
            log_hold,
        } = self.read(&commits, &manifest)?;
        drop(commits);
        let value_hold = stores.values.hold_files();
        let sample = Sample {
            version,
            end,
            durable,
            named,
            held: (pin, log_hold, value_hold),
        };
        Ok((sample, manifest))
    }

    /// What the sample reads, under the two locks it is given: nothing of
    /// it can be read without them.
    fn read(
        &self,
        _commits: &CommitLock<'_>,
        manifest: &ManifestStore,
    ) -> Result<Read<'s>, CheckpointError> {
        let log = self.write_back.stores().log;
        if let Some(stopped) = self.state.stopped() {
            return Err(CheckpointError::Stopped(stopped));
        }
        let named = manifest
            .manifest()
            .map_err(CheckpointError::Manifest)?
            .clone();
        let pin = self.versions.pin();
        let version = pin.version();
        let (end, logged) = (log.end(), log.latest());
        if logged != version {
            return Err(CheckpointError::LogAhead {
                log: logged,
                latest: version,
            });
        }
        // Where the log is durable through the version already, the
        // checkpoint's log ends where the durable log ends, and nothing is
        // synced for it: that place is behind the group of the version, at
        // the end of a record, and what the log holds behind it are records
        // of page steps, which a checkpoint can do without (PV6). What the
        // log says of itself is what a sync of this session found: a
        // session syncs its log before its first commit, when it turns to
        // a file of its own.
        let synced = log.durable();
        let (end, durable) = match synced.place {
            Some(place) if synced.version == version => (Some(place), true),
            _ => (end, false),
        };
        let log_hold = log.hold_from(None)?;
        Ok(Read {
            version,
            end,
            durable,
            named,
            pin,
            log_hold,
        })
    }

    /// Everything of a creation after the directory it builds in was
    /// made.
    fn fill(
        &self,
        target: &mut Target,
        source: &Path,
        catch_up: Option<Result<Checkpointed, FlushError>>,
    ) -> Result<Created, CheckpointError> {
        let log = self.write_back.stores().log;
        let (sample, manifest) = self.sample()?;
        let Sample {
            version,
            end,
            durable,
            named,
            held,
        } = sample;
        #[cfg(all(test, not(loom)))]
        self.reached(Step::Linking);

        // Still under the file-set lock: the table checkpoint, the
        // segments and the value-log files that the sampled manifest
        // names, each of which is on disk for as long as a durable
        // manifest names it. Once it has a name in the checkpoint, or is
        // open to be copied, it stays whatever the live database does
        // with its own name.
        let table = named.table_checkpoint();
        if let Some(table) = table {
            target.take(
                source,
                &FileNames::PAGE_TABLE,
                table.file(),
                table.valid_len(),
            )?;
        }
        let mut files = Vec::with_capacity(named.files().len());
        for file in named.files() {
            let (kind, id, len) = (file.kind(), file.id(), file.valid_len());
            let names = match kind {
                FileKind::Segment => FileNames::SEGMENT,
                FileKind::ValueLog => FileNames::VALUE_LOG,
                // The log's files are taken from the log, once it is
                // durable.
                FileKind::Wal | FileKind::Manifest | FileKind::PageTable => continue,
            };
            // What a later session of the live database appends behind
            // this length, a segment's footer included, is not the
            // checkpoint's (M15 of `segment-manifest.md`).
            let state = match file.state() {
                FileState::Sealed => FileState::Sealed,
                FileState::Open | FileState::Closed => FileState::Closed,
            };
            target.take(source, &names, id, len)?;
            files.push(LiveFile::new(kind, id, state, len)?);
        }
        for name in self.carried {
            target.carry(source, name)?;
        }
        drop(manifest);
        #[cfg(all(test, not(loom)))]
        self.reached(Step::Sampled);

        // The log makes itself durable, unless it is: nobody else syncs a
        // file of it (`lignin_store::wal`). The sync covers the sampled
        // end, and with it the version of the checkpoint (CK4).
        let durable = match durable {
            true => log.durable(),
            false => self.write_back.sync_log().map_err(CheckpointError::Sync)?,
        };
        if let Some(end) = end {
            if durable.place < Some(end) {
                return Err(CheckpointError::NotDurable {
                    end,
                    durable: durable.place,
                });
            }
            // The log's files up to the one the sampled end is in: each
            // closed where its last item ends, and the last where the
            // sample ended. The hold kept every one of them in the log.
            let mut ended = false;
            for file in log.files_named()? {
                let id = file.id();
                let len = match id.cmp(&end.file()) {
                    Ordering::Greater => continue,
                    Ordering::Less if file.state() != FileState::Closed => {
                        return Err(CheckpointError::LogFile {
                            file: id,
                            reason: "lies before the end of the checkpoint's log and is not closed",
                        });
                    }
                    Ordering::Less => file.valid_len(),
                    Ordering::Equal if file.valid_len() < end.offset() => {
                        return Err(CheckpointError::LogFile {
                            file: id,
                            reason: "is not durable through the end of the checkpoint's log",
                        });
                    }
                    Ordering::Equal => {
                        ended = true;
                        end.offset()
                    }
                };
                target.take(source, &FileNames::WAL, id, len)?;
                files.push(LiveFile::new(FileKind::Wal, id, FileState::Closed, len)?);
            }
            if !ended {
                return Err(CheckpointError::LogFile {
                    file: end.file(),
                    reason: "holds the end of the checkpoint's log and is not among the log's \
                             files",
                });
            }
        }
        drop(held);
        #[cfg(all(test, not(loom)))]
        self.reached(Step::Secured);

        target.copy_waiting()?;
        // The last ids are the sampled manifest's: the log turns to no file
        // that a durable manifest has not named, and a manifest is refused
        // that names a file above them.
        let first = Manifest::new(named.page_size(), named.last_file_ids(), table, files)?;
        target.write_manifest(&first)?;
        // Everything in the directory is made durable, with the name it
        // is built under, and only then does it get the name of the
        // target: a power cut leaves the target whole or not at all.
        target.settle()?;
        #[cfg(all(test, not(loom)))]
        self.reached(Step::Settled);
        target.place()?;
        Ok(Created {
            version,
            log_end: end,
            table,
            linked: target.linked,
            copied: target.copied,
            copied_bytes: target.copied_bytes,
            catch_up,
        })
    }
}

impl fmt::Debug for Checkpointer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Checkpointer")
            .field("options", &self.options)
            .field("carried", &self.carried)
            .finish_non_exhaustive()
    }
}
