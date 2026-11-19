//! The creation of a checkpoint, step by step (`transactions.md` CK1 to
//! CK10).

use std::{
    fmt,
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError},
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest, TableCheckpoint},
    wal::LogPlace,
    CommitVersion, FileKind,
};
use lignin_store::{
    flush::{Checkpointed, FlushError, WriteBack},
    segment::{FileNames, ManifestStore},
};

use super::{error::CheckpointError, files::Target};
use crate::{
    commit::{CommitState, Committer},
    snapshot::Versions,
};

/// How much log a checkpoint leaves for its first open to replay before
/// write-back is made to catch up first, unless
/// [`CheckpointOptions::catch_up_above`] says another amount: 4 MiB.
pub const DEFAULT_CATCH_UP_ABOVE: u64 = 4 << 20;

/// How a checkpoint is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointOptions {
    /// Write-back catches up before the checkpoint is sampled when it
    /// holds more log than this many bytes: the log from the oldest commit
    /// that no table checkpoint covers
    /// ([`WriteBack::log_held`]), which is what the first open of the
    /// checkpoint would replay. `None` never waits for write-back.
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
    /// sampling step. `None` for a log with no record.
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
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// The sampling step has ended, and both of its locks are given back.
    /// The log is not synced yet and no log file is in the target.
    Sampled,
    /// Every file is in the target, or open to be copied. The pin and the
    /// hold on the log are given back, and the manifest is not written.
    Secured,
}

#[cfg(test)]
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
    #[cfg(test)]
    pause: Option<Pause<'s>>,
}

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Checkpointer<'static>>();
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The numbered files of each kind that a manifest names as live.
const fn names_of(kind: FileKind) -> Option<FileNames> {
    match kind {
        FileKind::Wal => Some(FileNames::WAL),
        FileKind::Segment => Some(FileNames::SEGMENT),
        FileKind::ValueLog => Some(FileNames::VALUE_LOG),
        FileKind::Manifest | FileKind::PageTable => None,
    }
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
            #[cfg(test)]
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
    /// Each is one plain name inside the database directory, and not the
    /// manifest's or the lock's ([`CheckpointError::CarriedName`]). The
    /// engine vouches that such a file is durable and is never written
    /// again: a hard link shares every later change, and a copy is of the
    /// bytes the file has when the checkpoint is made.
    pub fn carrying(mut self, names: &'s [&'s str]) -> Self {
        self.carried = names;
        self
    }

    #[cfg(test)]
    pub(super) fn pausing(mut self, pause: Pause<'s>) -> Self {
        self.pause = Some(pause);
        self
    }

    #[cfg(test)]
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
    /// **When this returns `Ok`, the checkpoint is durable, and so is its
    /// version in the live database** (CK4). `target` is then a database
    /// directory like any other, and is opened as one.
    ///
    /// Commits are kept out for the sampling step and for nothing else of
    /// this call (CK5). Open write transactions are not waited for, and
    /// nothing of them is in the checkpoint (CK8). Any number of
    /// checkpoints may be made at once (CK9).
    ///
    /// If it fails, the live database is as it was (CK10), and what was
    /// made of `target` is taken away again as far as the file layer lets
    /// it. Where that fails too, `target` is left without a manifest, or
    /// with one that this call does not vouch for, and removing it is the
    /// caller's.
    pub fn create(&self, target: &Path) -> Result<Created, CheckpointError> {
        let stores = self.write_back.stores();
        let (vfs, source) = {
            let manifest = lock(stores.manifest);
            (manifest.vfs().clone(), manifest.dir().to_path_buf())
        };
        let mut made = Target::make(vfs, target)?;
        let created = self.fill(&mut made, &source);
        if created.is_err() {
            made.remove();
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

    /// Everything of a creation after the target was made.
    fn fill(&self, target: &mut Target, source: &Path) -> Result<Created, CheckpointError> {
        let stores = self.write_back.stores();
        let log = stores.log;
        // The name of a database directory is durable before its first
        // manifest is written (`lignin_store::segment`).
        target.sync_parent()?;
        let catch_up = self.catch_up();

        // The sampling step (CK2): the commit lock, then the lock on the
        // manifest, which is the file-set lock (levels 1 and 3 of LT7). No
        // commit is published, no manifest is replaced and no file of the
        // store is deleted while both are held.
        let commits = self.state.lock();
        if let Some(stopped) = self.state.stopped() {
            return Err(CheckpointError::Stopped(stopped));
        }
        let manifest = lock(stores.manifest);
        let named = manifest
            .manifest()
            .map_err(CheckpointError::Manifest)?
            .clone();
        // From here until the files are in the target, the version is
        // pinned and the whole log is kept (CK3).
        let pin = self.versions.pin();
        let version = pin.version();
        let (end, logged) = (log.end(), log.latest());
        if logged != version {
            return Err(CheckpointError::LogAhead {
                log: logged,
                latest: version,
            });
        }
        let hold = log.hold_from(None)?;
        drop(commits);

        // Still under the file-set lock: the table checkpoint, the
        // segments and the value-log files that the sampled manifest
        // names, each of which is on disk for as long as a durable
        // manifest names it. Once it has a name in the target, or is
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
            let Some(names) = names_of(kind).filter(|_| kind != FileKind::Wal) else {
                continue;
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
        #[cfg(test)]
        self.reached(Step::Sampled);

        // The log makes itself durable: nobody else syncs a file of it
        // (`lignin_store::wal`). The sync covers the sampled end, and with
        // it the version of the checkpoint (CK4).
        let durable = self.write_back.sync_log().map_err(CheckpointError::Sync)?;
        let mut last_log_id = named.last_file_ids().wal;
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
                    std::cmp::Ordering::Greater => continue,
                    std::cmp::Ordering::Less if file.state() != FileState::Closed => {
                        return Err(CheckpointError::LogFile {
                            file: id,
                            reason: "lies before the end of the checkpoint's log and is not closed",
                        });
                    }
                    std::cmp::Ordering::Less => file.valid_len(),
                    std::cmp::Ordering::Equal if file.valid_len() < end.offset() => {
                        return Err(CheckpointError::LogFile {
                            file: id,
                            reason: "is not durable through the end of the checkpoint's log",
                        });
                    }
                    std::cmp::Ordering::Equal => {
                        ended = true;
                        end.offset()
                    }
                };
                target.take(source, &FileNames::WAL, id, len)?;
                files.push(LiveFile::new(FileKind::Wal, id, FileState::Closed, len)?);
                last_log_id = last_log_id.max(Some(id));
            }
            if !ended {
                return Err(CheckpointError::LogFile {
                    file: end.file(),
                    reason:
                        "holds the end of the checkpoint's log and is not among the log's files",
                });
            }
        }
        drop((pin, hold));
        #[cfg(test)]
        self.reached(Step::Secured);

        if target.has_waiting() {
            target.copy_waiting()?;
        }
        // Every name is durable before the manifest that names it is.
        target.sync_names()?;
        let mut last_file_ids = named.last_file_ids();
        last_file_ids.wal = last_log_id;
        let first = Manifest::new(named.page_size(), last_file_ids, table, files)?;
        ManifestStore::create(target.vfs().clone(), target.dir(), first)
            .map_err(CheckpointError::Install)?;
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
