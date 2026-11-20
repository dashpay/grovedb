//! Backup: a database directory that nobody writes, copied whole into a
//! new directory.
//!
//! - [`Backup`] copies one database directory into another place
//!   ([`Backup::copy`]): from a checkpoint to a backup, and from a backup
//!   back to where a database is to be.
//! - [`Copied`] says what a copy holds, and [`BackupError`] why none was
//!   made.
//!
//! `docs/lignindb/disk-full-and-backup.md` is the procedure an operator
//! follows, and says what the engine does when a disk fills.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_io::StdVfs;
//! use lignindb::{
//!     backup::Backup,
//!     commit::NoStaleHashes,
//!     txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite},
//!     Db, Options,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let (live, backup, restored) = (
//!     dir.path().join("db"),
//!     dir.path().join("backup"),
//!     dir.path().join("restored"),
//! );
//! let subtree = Subtree::new([3; 32]);
//! let db = Db::open(&live, &Options::default())?;
//! db.write(
//!     &Batch {
//!         subtrees: &[SubtreeWrite::Enter {
//!             subtree,
//!             tree_type: b"tree",
//!             host: None,
//!         }],
//!         records: &[RecordWrite::put(Column::Meta, subtree, b"height", b"7")],
//!         ..Batch::default()
//!     },
//!     &mut NoStaleHashes,
//! )?;
//! let version = db.version();
//! // A database that is closed is a directory that nobody writes, and so
//! // is a checkpoint that was not opened.
//! db.close()?;
//!
//! // The backup, and a restore from it: the same call both ways.
//! let copy = Backup::new(Arc::new(StdVfs));
//! assert_eq!(copy.copy(&live, &backup)?.version, version);
//! assert_eq!(copy.copy(&backup, &restored)?.version, version);
//!
//! // What was restored is a database like any other.
//! let db = Db::open(&restored, &Options::default())?;
//! assert_eq!(db.version(), version);
//! let mut value = Vec::new();
//! let snapshot = db.snapshot();
//! let operation = snapshot.operation();
//! let opened = operation.open(subtree)?.expect("the subtree is entered");
//! assert!(opened.get(Column::Meta, b"height", &mut value)?);
//! assert_eq!(value, b"7");
//! # Ok(())
//! # }
//! ```
//!
//! # What a backup is made from
//!
//! **A consistent backup is made from a checkpoint.** A checkpoint
//! ([`crate::checkpoint`]) is one committed version of a database that is
//! open and written, as a directory of its own, made in the time of a few
//! hard links. It shares its files with the live database, so by itself it
//! is no backup: it is on the same disk, and whatever damages a shared
//! file damages both. [`Backup::copy`] makes of it a directory that shares
//! nothing: every byte the checkpoint's manifest vouches for, in files of
//! its own, on any file system.
//!
//! The same call copies **a database that is closed**, which is a
//! directory that nobody writes too: that is how a backup is restored, and
//! how a node that is stopped is backed up without a checkpoint.
//!
//! The source is never the directory of a database that is open. A
//! database in use changes its manifest and its files under whoever reads
//! them, and a copy of that is no state of it. The copy takes the lock of
//! the source for as long as it runs, and refuses a source whose lock a
//! handle holds ([`BackupError::Open`]). So a checkpoint is copied before
//! it is opened, or after it was closed again. A node that opens every
//! checkpoint it makes (P-15 of `platform-requirements.md`) has one made
//! for the backup that it does not open.
//!
//! # The steps of a copy
//!
//! | Step | What is done |
//! | --- | --- |
//! | 1 | A target that is there is refused, and so is one whose path is inside the source, with nothing touched. |
//! | 2 | The lock of the source is taken through its lock file, which must be there, and its manifest is read under the lock. No file is made in the source. |
//! | 3 | Every file the manifest names is measured: a file that is missing, or shorter than the manifest vouches for, ends the copy before anything is made ([`BackupError::Short`]). |
//! | 4 | The target is made, a new directory, and its lock is taken, as for any database directory that is written. Its `LOCK` file is the copy's from here on: a database is opened through the lock file it has (`crate::db`). |
//! | 5 | Each file is copied under its own name and synced: of a file that the manifest names closed or sealed, the bytes up to its valid length; of a file it names open, all it has, since whole records may follow the valid length. The files that the caller has the copy carry ([`Backup::carrying`]) are copied whole. |
//! | 6 | Every name in the target is made durable, and then the target's own name. |
//! | 7 | The log of the copy is read to its end, as the recovery of its first open will read it. The version it ends at is the version of the backup ([`Copied::version`]). |
//! | 8 | The manifest is written, as the first manifest of a database directory is: to a temporary file that is synced, renamed, and made durable with the directory. It is the source's manifest, byte for byte. |
//!
//! **The manifest is written last, and it is what makes the target a
//! database**, as it is for a checkpoint. A copy that was cut short before
//! it, by an error or by the power, leaves nothing that opens.
//!
//! **Nothing of the source is synced, and nothing in it is written.** The
//! log files of a checkpoint are the live database's own files under a
//! second name, and a sync of one that fails through another handle is
//! invisible to the log that writes it (`lignin_store::wal`). A copy only
//! reads them. What it reads below a valid length was durable before the
//! manifest that vouches for it was written.
//!
//! This holds for a target that is outside the source. A target whose
//! path is inside the source's is refused
//! ([`BackupError::TargetInSource`]). The two paths are compared as they
//! are given, whatever the file layers they are of: a target that is
//! inside the source by another name for it, a link or a path from
//! another directory, is not told, and is the caller's to rule out, and a
//! path inside the source's is refused also where the two file layers are
//! two devices.
//!
//! **What lies behind a valid length is not copied.** The live database
//! goes on appending to the files it shares with a checkpoint, behind the
//! lengths the checkpoint's manifest gives them. Those bytes are no part
//! of the checkpoint, and a copy by this call does not have them. A copy
//! of the directory by any other means (`cp -a`, `rsync`, `tar`) has them
//! and is as good a backup: a recovery reads a closed file up to its valid
//! length and no further (`segment-manifest.md` M15). Such a copy is
//! consistent under the same condition, that nobody has the directory open
//! while it is made, and it has to bring the `LOCK` file along.
//!
//! # What the copy is
//!
//! The target opens as the source would have, at the same version, by
//! the store's recovery, as any database directory does. It holds the
//! files the manifest names and no other: not the files of the source
//! that no manifest names, which the next open of the source removes, and
//! not what somebody else keeps in the directory (P-03), unless the caller
//! names it ([`Backup::carrying`]).
//!
//! Two things follow from "as the source would have". A source in which a
//! database was being made and never was, with a manifest and no commit
//! in its log, is copied as that: the copy is of version 0, and its first
//! open makes the database there, as the next open of the source would
//! have. And a file that the copy is told to carry is in the target as it
//! is in the source: one whose name has an open refuse a directory as
//! another engine's (`crate::db`, "The directory") has it refuse
//! both.
//!
//! # What a copy that fails leaves
//!
//! The source is as it was. Of the target the copy takes away what it
//! made: the manifest first, if it got as far as having one written, then
//! the files, the directories, the lock file and the target itself. It
//! removes nothing it did not make. **No file goes while a manifest could
//! come back**: where a manifest was written and its removal cannot be
//! made durable, every file stays, so that a power cut leaves the whole
//! copy or a directory without a manifest, and never a manifest without
//! one of its files.
//!
//! Where the file layer fails the removal too, and after a power cut
//! during a copy, a target can be left without a manifest: empty, with its
//! lock file, or with some of its files. **A directory with no `MANIFEST`
//! is no database and no backup.** Nothing opens it as one
//! (`crate::db::OpenError::NoManifest`, or a new and empty database where
//! the directory holds no file of a database yet and the open may create).
//! It is removed, by whoever asked for the copy, before the copy is made
//! again: the target of a copy is a directory that is not there. The same
//! holds for what a creation of a checkpoint leaves when it is cut short
//! ([`crate::checkpoint`], "Durability, and a creation that fails").
//!
//! A full disk under the target fails the copy with the error of the file
//! layer: [`BackupError::Io`], or [`BackupError::Install`] where it was
//! the manifest that found no room. The source of either says that there
//! is no space ([`crate::space::out_of_space`]). The source directory and
//! the database it is a checkpoint of are as they were.
//!
//! # Choices made here that the plan left open
//!
//! 1. **A backup is a copy of a directory, made by the engine or by any
//!    tool.** The engine's copy adds the lock of the source, the lengths
//!    of the manifest, the durability of each step, the read of the log
//!    and the removal of what a failed copy made.
//! 2. **The source and the target can be in two file layers**
//!    ([`Backup::between`]), so that a test can fill the disk of the one
//!    and not of the other.
//! 3. **The copy is not checked block by block.** The log is read to its
//!    end, which checks every item of it. The segments, the value log and
//!    the table checkpoint are copied as bytes, and what is wrong in them
//!    is found by the open of the copy and by the physical check
//!    ([`crate::verify`]), which a backup that is to be relied on is given
//!    once: on the checkpoint before it is copied, or on a restore.

mod error;

#[cfg(all(test, not(loom)))]
pub(crate) mod tests;

use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{
    manifest::{FileState, Manifest},
    CommitVersion, FileId, FileKind,
};
use lignin_io::{Create, DirEntry, EntryKind, OpenMode, Vfs, VfsFile, VfsLock};
use lignin_store::{
    segment::{read_manifest, FileNames, ManifestStore, SegmentStoreError},
    wal::LogReader,
};

pub use self::error::BackupError;
use crate::checkpoint::DELETING_FILE_NAME;

/// The file whose lock lets one handle at a time into a database
/// directory.
const LOCK_FILE_NAME: &str = "LOCK";

/// How many bytes of a file are copied at a time.
const COPY_CHUNK_LEN: u64 = 1 << 20;

/// A copy that was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// The committed version the copy holds: the one its log ends at.
    /// The first open of the copy lands on it, as the next open of the
    /// source would have, with one exception: a copy of version 0 is of a
    /// directory in which a database was being made and never was, and
    /// the first open of either makes the database, which is version 1.
    pub version: CommitVersion,
    /// The files of the store that were copied: the log, the segments, the
    /// value log and the table checkpoint.
    pub files: usize,
    /// The bytes of those files.
    pub bytes: u64,
    /// The files that were carried beside them ([`Backup::carrying`]), and
    /// their bytes.
    pub carried: usize,
    /// See [`Self::carried`].
    pub carried_bytes: u64,
}

/// What copies database directories: from one file layer into another, or
/// within one.
///
/// See the [module documentation](self) for what a copy is made from, for
/// its steps and for what it leaves when it fails.
#[derive(Clone)]
pub struct Backup<'a> {
    from: Arc<dyn Vfs>,
    to: Arc<dyn Vfs>,
    carried: &'a [&'a str],
}

impl std::fmt::Debug for Backup<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backup")
            .field("carried", &self.carried)
            .finish_non_exhaustive()
    }
}

/// One file of the source, with the bytes of it that are copied.
struct Planned {
    from: PathBuf,
    /// The directory of the file's kind in the target, if it is a file of
    /// the store, and its path there.
    dir: Option<PathBuf>,
    to: PathBuf,
    len: u64,
}

/// The directory a copy is being made in, with everything the copy has put
/// into it.
struct Target<'v> {
    vfs: &'v Arc<dyn Vfs>,
    dir: PathBuf,
    parent: PathBuf,
    dirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
    /// The lock of the target as a database directory, held while it is
    /// made.
    lock: Box<dyn VfsLock>,
    /// Whether the copy has begun to write the manifest: only from then on
    /// is a manifest in the target, or its temporary file, the copy's to
    /// remove.
    installing: bool,
}

/// The directory above `path` as the file layer is given it.
fn parent_of(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    Some(match parent.as_os_str().is_empty() {
        true => Path::new("."),
        false => parent,
    })
}

impl<'a> Backup<'a> {
    /// What copies database directories within the file layer `vfs`.
    pub fn new(vfs: Arc<dyn Vfs>) -> Self {
        Self::between(vfs.clone(), vfs)
    }

    /// What copies a database directory of the file layer `from` into the
    /// file layer `to`.
    pub fn between(from: Arc<dyn Vfs>, to: Arc<dyn Vfs>) -> Self {
        Self {
            from,
            to,
            carried: &[],
        }
    }

    /// The same, and every copy also carries the files `names` of the
    /// source, each whole and under its own name: what somebody keeps in a
    /// database directory beside the store's files and wants in a backup
    /// of it, as a node keeps its own state beside a checkpoint (P-17 of
    /// `platform-requirements.md`).
    ///
    /// Each is the plain name of a file in the source: not of a directory
    /// or of a link to a file, and not the manifest's or the lock's
    /// ([`BackupError::CarriedName`]). The caller vouches that nobody
    /// writes such a file while it is copied.
    ///
    /// A name is not looked at beyond that. A file that makes an open
    /// refuse the source as another engine's directory (`CURRENT`, for
    /// one: `crate::db`, "The directory") is carried like any other,
    /// and the copy is refused by an open as the source is.
    pub fn carrying(mut self, names: &'a [&'a str]) -> Self {
        self.carried = names;
        self
    }

    /// Copies the database in the directory `source`, which no handle has
    /// open, into the new directory `target`: every file its manifest
    /// names, as far as the manifest says the file is the database's, and
    /// the manifest last.
    ///
    /// The directory above `target` is there, and `target` is not: this
    /// makes it, and refuses one that exists with nothing touched
    /// ([`BackupError::TargetExists`]). A `target` whose path is inside
    /// the path of `source` is refused as well
    /// ([`BackupError::TargetInSource`]).
    ///
    /// **When this returns `Ok`, the copy is durable.** `target` is then a
    /// database directory with a lock file of its own, which opens as
    /// `source` would have, at [`Copied::version`] (the
    /// [module documentation](self), "What the copy is"). It shares no
    /// file with `source`.
    ///
    /// The lock of `source` is held for the whole call, so no handle opens
    /// it meanwhile, and nothing is written to it: a checkpoint that was
    /// copied can be opened afterwards, or deleted. A source that is open
    /// is refused ([`BackupError::Open`]).
    ///
    /// If it fails, `source` is as it was, and what was made of `target`
    /// is taken away again as far as the file layer lets it. Where that
    /// fails too, `target` is left for the caller to remove: without a
    /// manifest, or, where a manifest was written and its removal could
    /// not be made durable, with every file that manifest names.
    pub fn copy(&self, source: &Path, target: &Path) -> Result<Copied, BackupError> {
        let Some(parent) = parent_of(target) else {
            return Err(BackupError::NoParent {
                target: target.to_path_buf(),
            });
        };
        self.refuse_existing(target)?;
        if target.starts_with(source) {
            return Err(BackupError::TargetInSource {
                dir: source.to_path_buf(),
                target: target.to_path_buf(),
            });
        }
        let from = &*self.from;

        // The lock of the source, through the lock file it has: none is
        // made in a directory that is only read.
        let lock_path = source.join(LOCK_FILE_NAME);
        let _source_lock = from
            .lock_existing(&lock_path)
            .map_err(|error| match error.kind() {
                ErrorKind::NotFound => BackupError::NoLockFile {
                    dir: source.to_path_buf(),
                },
                ErrorKind::WouldBlock => BackupError::Open {
                    dir: source.to_path_buf(),
                },
                _ => BackupError::io("lock", &lock_path)(error),
            })?;
        let manifest = read_manifest(from, source).map_err(|error| BackupError::NotADatabase {
            dir: source.to_path_buf(),
            source: error,
        })?;
        let (mut planned, files, bytes) = self.plan(source, target, &manifest)?;
        let (carried, carried_bytes) = self.plan_carried(source, target, &mut planned)?;

        let mut made = Target::make(&self.to, target, parent)?;
        let copied = made.fill(from, planned, &manifest);
        match copied {
            Ok(version) => Ok(Copied {
                version,
                files,
                bytes,
                carried,
                carried_bytes,
            }),
            Err(error) => {
                made.remove();
                Err(error)
            }
        }
    }

    /// Refuses a target that is there, as a directory or as anything else,
    /// before anything is done for it. The making of the directory is what
    /// decides.
    fn refuse_existing(&self, target: &Path) -> Result<(), BackupError> {
        match self.to.read_dir(target) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotADirectory => {}
            Err(_) => return Ok(()),
        }
        Err(BackupError::TargetExists {
            target: target.to_path_buf(),
        })
    }

    /// Measures every file that `manifest` names in `source` and says how
    /// many bytes of each are copied, and where to. Nothing is made.
    fn plan(
        &self,
        source: &Path,
        target: &Path,
        manifest: &Manifest,
    ) -> Result<(Vec<Planned>, usize, u64), BackupError> {
        let table = manifest.table_checkpoint().map(|table| {
            (
                FileNames::PAGE_TABLE,
                table.file(),
                FileState::Closed,
                table.valid_len(),
            )
        });
        let named = manifest.files().iter().map(|file| {
            let names = match file.kind() {
                FileKind::Wal => FileNames::WAL,
                FileKind::Segment => FileNames::SEGMENT,
                FileKind::ValueLog => FileNames::VALUE_LOG,
                kind @ (FileKind::Manifest | FileKind::PageTable) => {
                    return Err(BackupError::Kind {
                        kind,
                        file: file.id(),
                    });
                }
            };
            Ok((names, file.id(), file.state(), file.valid_len()))
        });
        let mut planned = Vec::new();
        let mut bytes = 0;
        for file in table.into_iter().map(Ok).chain(named) {
            let (names, id, state, valid_len): (FileNames, FileId, FileState, u64) = file?;
            let path = names.path(source, id);
            let short = |len| BackupError::Short {
                path: path.clone(),
                valid_len,
                len,
            };
            let open = match self.from.open(&path, OpenMode::Read) {
                Ok(open) => open,
                Err(error) if error.kind() == ErrorKind::NotFound => return Err(short(None)),
                Err(error) => return Err(BackupError::io("open", &path)(error)),
            };
            let has = open.len().map_err(BackupError::io("measure", &path))?;
            if has < valid_len {
                return Err(short(Some(has)));
            }
            // Whole records may follow the valid length of a file that the
            // manifest names open, and nothing behind that length is the
            // database's in one it names closed or sealed (M15).
            let len = match state {
                FileState::Open => has,
                FileState::Closed | FileState::Sealed => valid_len,
            };
            bytes += len;
            planned.push(Planned {
                from: path,
                dir: Some(names.dir(target)),
                to: names.path(target, id),
                len,
            });
        }
        let files = planned.len();
        Ok((planned, files, bytes))
    }

    /// Measures the files the copy is to carry beside the store's.
    fn plan_carried(
        &self,
        source: &Path,
        target: &Path,
        planned: &mut Vec<Planned>,
    ) -> Result<(usize, u64), BackupError> {
        if self.carried.is_empty() {
            return Ok((0, 0));
        }
        let entries = self
            .from
            .read_dir(source)
            .map_err(BackupError::io("list", source))?;
        let reserved = [
            ManifestStore::FILE_NAME,
            ManifestStore::TEMP_FILE_NAME,
            DELETING_FILE_NAME,
            LOCK_FILE_NAME,
        ];
        let mut bytes = 0;
        for name in self.carried {
            let plain = Path::new(name).file_name() == Some(OsStr::new(name));
            let is_file = |entry: &DirEntry| entry.name == *name && entry.kind == EntryKind::File;
            let twice = self.carried.iter().filter(|other| *other == name).count() > 1;
            if !plain || twice || reserved.contains(name) || !entries.iter().any(is_file) {
                return Err(BackupError::CarriedName {
                    name: name.to_string(),
                });
            }
            let path = source.join(name);
            let open = self
                .from
                .open(&path, OpenMode::Read)
                .map_err(BackupError::io("open", &path))?;
            let len = open.len().map_err(BackupError::io("measure", &path))?;
            bytes += len;
            planned.push(Planned {
                from: path,
                dir: None,
                to: target.join(name),
                len,
            });
        }
        Ok((self.carried.len(), bytes))
    }
}

impl<'v> Target<'v> {
    /// Makes the directory `dir`, which must not be there, and takes its
    /// lock. Where the lock is not had, or the directory holds anything
    /// but its lock file by then, somebody else got into it first, and
    /// nothing in it is touched. Where the directory cannot be listed,
    /// that is not known: the lock file goes, and the directory if it is
    /// empty then.
    fn make(vfs: &'v Arc<dyn Vfs>, dir: &Path, parent: &Path) -> Result<Self, BackupError> {
        vfs.create_dir(dir).map_err(|source| match source.kind() {
            ErrorKind::AlreadyExists => BackupError::TargetExists {
                target: dir.to_path_buf(),
            },
            _ => BackupError::io("create the directory", dir)(source),
        })?;
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = match vfs.lock(&lock_path) {
            Ok(lock) => lock,
            Err(source) => {
                let _ = vfs.remove_dir(dir);
                let _ = vfs.sync_dir(parent);
                return Err(BackupError::io("lock", &lock_path)(source));
            }
        };
        let made = Self {
            vfs,
            dir: dir.to_path_buf(),
            parent: parent.to_path_buf(),
            dirs: Vec::new(),
            files: Vec::new(),
            lock,
            installing: false,
        };
        match vfs.read_dir(dir) {
            Ok(entries) if entries.iter().any(|entry| entry.name != LOCK_FILE_NAME) => {
                Err(BackupError::TargetExists {
                    target: dir.to_path_buf(),
                })
            }
            Ok(_) => Ok(made),
            Err(source) => {
                made.remove();
                Err(BackupError::io("list", dir)(source))
            }
        }
    }

    /// Everything of a copy after the target was made: the files, their
    /// names, the read of the log, and the manifest.
    fn fill(
        &mut self,
        source: &dyn Vfs,
        planned: Vec<Planned>,
        manifest: &Manifest,
    ) -> Result<CommitVersion, BackupError> {
        let vfs = self.vfs.clone();
        let mut chunk = Vec::new();
        for file in planned {
            let Planned { from, dir, to, len } = file;
            // One file of the source is open at a time: a database has as
            // many files as it has segments.
            let file: Box<dyn VfsFile> = source
                .open(&from, OpenMode::Read)
                .map_err(BackupError::io("open", &from))?;
            if let Some(dir) = dir
                && !self.dirs.contains(&dir)
            {
                vfs.create_dir(&dir)
                    .map_err(BackupError::io("create the directory", &dir))?;
                self.dirs.push(dir);
            }
            let copy = vfs
                .open(&to, OpenMode::Append(Create::New))
                .map_err(BackupError::io("create", &to))?;
            // Only now is the name the copy's: one that was there is
            // somebody else's file.
            self.files.push(to.clone());
            let mut at = 0;
            while at < len {
                let part = (len - at).min(COPY_CHUNK_LEN) as usize;
                chunk.resize(part, 0);
                file.read_exact_at(&mut chunk, at)
                    .map_err(BackupError::io("read", &from))?;
                copy.append(&chunk)
                    .map_err(BackupError::io("append to", &to))?;
                at += part as u64;
            }
            copy.sync_all().map_err(BackupError::io("sync", &to))?;
        }
        // Every name in the target is durable before the target's own name
        // is, and that before the manifest is written: from here on, a
        // target that a power cut leaves has every one of its files.
        for dir in self.dirs.iter().chain([&self.dir]) {
            vfs.sync_dir(dir)
                .map_err(BackupError::io("sync the directory", dir))?;
        }
        vfs.sync_dir(&self.parent)
            .map_err(BackupError::io("sync the directory", &self.parent))?;
        // The log of the copy, read as the recovery of its first open
        // reads it: the version it ends at is the one the copy opens at.
        let end = LogReader::new(self.vfs.clone(), &self.dir, manifest)
            .into_end()
            .map_err(BackupError::Log)?;
        match ManifestStore::create(self.vfs.clone(), &self.dir, manifest.clone()) {
            Ok(_) => Ok(end.latest()),
            // A manifest that is there is not the copy's: somebody wrote
            // one into the target without its lock.
            Err(theirs @ SegmentStoreError::ManifestExists { .. }) => {
                Err(BackupError::Install(theirs))
            }
            Err(error) => {
                self.installing = true;
                Err(BackupError::Install(error))
            }
        }
    }

    /// Takes away what the copy made, as far as the file layer lets it:
    /// the manifest first, if the copy got as far as having one written,
    /// so that what is left is no database, then the files, the
    /// directories, the lock file and the target. Nothing else is touched.
    /// No file goes while the manifest could come back.
    fn remove(self) {
        let vfs = self.vfs.clone();
        if self.installing {
            match vfs.remove_file(&self.dir.join(ManifestStore::FILE_NAME)) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Ok(()) if vfs.sync_dir(&self.dir).is_ok() => {}
                _ => return,
            }
            let _ = vfs.remove_file(&self.dir.join(ManifestStore::TEMP_FILE_NAME));
        }
        for file in self.files.iter().rev() {
            let _ = vfs.remove_file(file);
        }
        for dir in self.dirs.iter().rev() {
            let _ = vfs.remove_dir(dir);
        }
        let _ = vfs.remove_file(&self.dir.join(LOCK_FILE_NAME));
        drop(self.lock);
        let _ = vfs.remove_dir(&self.dir);
        let _ = vfs.sync_dir(&self.parent);
    }
}
