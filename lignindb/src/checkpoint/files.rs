//! The files of a checkpoint's directory: each is a hard link to a file of
//! the live database, or a copy of it where no link can be made, and a
//! creation that fails takes away what it made.

use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{manifest::Manifest, FileId};
use lignin_io::{Create, DirEntry, EntryKind, OpenMode, Vfs, VfsFile, VfsLock};
use lignin_store::segment::{read_manifest, FileNames, ManifestStore, SegmentStoreError};

use super::error::CheckpointError;

/// The file whose lock lets one handle at a time into a database directory
/// (`lignin_io::Vfs::lock`).
pub(super) const LOCK_FILE_NAME: &str = "LOCK";

/// The name the manifest of a directory has while [`delete`] removes the
/// directory. A directory that holds a manifest under this name and none
/// under its own was a database and is none any more: nothing opens it,
/// nothing makes a database in it, and [`delete`] finishes its removal.
pub const DELETING_FILE_NAME: &str = "MANIFEST.deleting";

/// How many bytes of a file are copied at a time.
const COPY_CHUNK_LEN: u64 = 1 << 20;

/// The directory above `path` as the file layer is given it, or `None` if
/// `path` has none.
pub(super) fn parent_of(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    Some(match parent.as_os_str().is_empty() {
        true => Path::new("."),
        false => parent,
    })
}

/// Whether a hard link that failed with `kind` is one that the system
/// does not make, so that a copy stands in for it: the two names are on
/// different file systems, the file has as many names as it can have, the
/// file system has no hard links, which it says as "not supported" or, on
/// Linux, as "not permitted", or the system keeps this file from being
/// linked, which it says in the same words (a file of another owner where
/// links are protected, a file that is marked append-only). The last two
/// cannot be told apart, and a copy is a checkpoint in both. It is made
/// only if the file can be read and the copy created, so a link that is
/// refused for want of a permission on the directories fails the creation
/// with the error of the open or of the create.
const fn copies_instead(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::CrossesDevices
            | ErrorKind::TooManyLinks
            | ErrorKind::Unsupported
            | ErrorKind::PermissionDenied
    )
}

/// A file of the live database that could not be linked and waits to be
/// copied. The handle keeps the file whatever becomes of its name.
struct Waiting {
    from: PathBuf,
    file: Box<dyn VfsFile>,
    to: PathBuf,
    len: u64,
}

/// The directory a checkpoint is being made in, with everything this
/// creation has put into it.
pub(super) struct Target {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    parent: PathBuf,
    /// The directories made in the target, in the order they were made.
    dirs: Vec<PathBuf>,
    /// The files this creation has put into the target, as links or as
    /// copies.
    files: Vec<PathBuf>,
    waiting: Vec<Waiting>,
    /// The lock of the target as a database directory, held while it is
    /// made: whoever opens a directory takes it first.
    lock: Box<dyn VfsLock>,
    /// Whether this creation has begun to write the manifest: only from
    /// then on is a manifest in the target, or its temporary file, this
    /// creation's to remove.
    installing: bool,
    pub(super) linked: usize,
    pub(super) copied: usize,
    pub(super) copied_bytes: u64,
}

impl Target {
    /// Refuses a target that is there, as a directory or as anything else,
    /// and one with no directory above it, before anything is done for it.
    /// [`Self::make`] is what decides.
    pub(super) fn refuse_existing(vfs: &dyn Vfs, dir: &Path) -> Result<(), CheckpointError> {
        if parent_of(dir).is_none() {
            return Err(CheckpointError::NoParent {
                target: dir.to_path_buf(),
            });
        }
        match vfs.read_dir(dir) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotADirectory => {}
            Err(_) => return Ok(()),
        }
        Err(CheckpointError::TargetExists {
            target: dir.to_path_buf(),
        })
    }

    /// Makes the directory `dir`, which must not be there, in a directory
    /// that is, and takes its lock: a manifest is written under the lock
    /// of its directory (`lignin_store::segment`), and nobody that takes
    /// the lock opens a target that is being made.
    ///
    /// **The target is this creation's only if nobody got into it first.**
    /// Between the making of the directory and the lock, somebody else can
    /// take the lock, and put a database of their own there. So where the
    /// lock is not had, nothing in the directory is touched: it goes if it
    /// is empty, and stays otherwise. And where the lock is had and the
    /// directory holds anything but the lock file, it is somebody else's
    /// by now ([`CheckpointError::TargetExists`]), and stays as it is.
    pub(super) fn make(vfs: Arc<dyn Vfs>, dir: &Path) -> Result<Self, CheckpointError> {
        let Some(parent) = parent_of(dir) else {
            return Err(CheckpointError::NoParent {
                target: dir.to_path_buf(),
            });
        };
        vfs.create_dir(dir).map_err(|source| match source.kind() {
            ErrorKind::AlreadyExists => CheckpointError::TargetExists {
                target: dir.to_path_buf(),
            },
            _ => CheckpointError::io("create the directory", dir)(source),
        })?;
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = match vfs.lock(&lock_path) {
            Ok(lock) => lock,
            Err(source) => {
                let _ = vfs.remove_dir(dir);
                let _ = vfs.sync_dir(parent);
                return Err(CheckpointError::io("lock", &lock_path)(source));
            }
        };
        let entries = vfs
            .read_dir(dir)
            .map_err(CheckpointError::io("list", dir))?;
        if entries.iter().any(|entry| entry.name != LOCK_FILE_NAME) {
            return Err(CheckpointError::TargetExists {
                target: dir.to_path_buf(),
            });
        }
        Ok(Self {
            vfs,
            dir: dir.to_path_buf(),
            parent: parent.to_path_buf(),
            dirs: Vec::new(),
            files: Vec::new(),
            waiting: Vec::new(),
            lock,
            installing: false,
            linked: 0,
            copied: 0,
            copied_bytes: 0,
        })
    }

    /// Makes the name of the target durable, with everything that is
    /// durable in it.
    pub(super) fn sync_parent(&self) -> Result<(), CheckpointError> {
        self.vfs
            .sync_dir(&self.parent)
            .map_err(CheckpointError::io("sync the directory", &self.parent))
    }

    /// Gives file `id` of the kind `names` in the database directory
    /// `source` its name in the target, of which `len` bytes are the
    /// checkpoint's.
    pub(super) fn take(
        &mut self,
        source: &Path,
        names: &FileNames,
        id: FileId,
        len: u64,
    ) -> Result<(), CheckpointError> {
        let dir = names.dir(&self.dir);
        if !self.dirs.contains(&dir) {
            self.vfs
                .create_dir(&dir)
                .map_err(CheckpointError::io("create the directory", &dir))?;
            self.dirs.push(dir);
        }
        self.secure(
            &names.path(source, id),
            names.path(&self.dir, id),
            Some(len),
        )
    }

    /// Gives the file `name` of the database directory `source`, whole,
    /// the same name in the target.
    ///
    /// The name is that of a file in `source`, and of nothing else: a link
    /// to a file would be linked as the link it is and copied as the file
    /// it leads to, and in the target it would lead nowhere.
    pub(super) fn carry(&mut self, source: &Path, name: &str) -> Result<(), CheckpointError> {
        let plain = Path::new(name).file_name() == Some(OsStr::new(name));
        let reserved = [
            ManifestStore::FILE_NAME,
            ManifestStore::TEMP_FILE_NAME,
            DELETING_FILE_NAME,
            LOCK_FILE_NAME,
        ];
        let refused = || CheckpointError::CarriedName {
            name: name.to_string(),
        };
        if !plain || reserved.contains(&name) {
            return Err(refused());
        }
        let entries = self
            .vfs
            .read_dir(source)
            .map_err(CheckpointError::io("list", source))?;
        let is_file = |entry: &DirEntry| entry.name == name && entry.kind == EntryKind::File;
        if !entries.iter().any(is_file) {
            return Err(refused());
        }
        self.secure(&source.join(name), self.dir.join(name), None)
    }

    /// Links `from` as `to`. Where the file system makes no such link, the
    /// file is opened instead, and its first `len` bytes, or all it has
    /// now, are copied by [`Self::copy_waiting`].
    fn secure(
        &mut self,
        from: &Path,
        to: PathBuf,
        len: Option<u64>,
    ) -> Result<(), CheckpointError> {
        match self.vfs.hard_link(from, &to) {
            Ok(()) => {
                self.files.push(to);
                self.linked += 1;
                Ok(())
            }
            Err(error) if copies_instead(error.kind()) => {
                let file = self
                    .vfs
                    .open(from, OpenMode::Read)
                    .map_err(CheckpointError::io("open", from))?;
                let len = match len {
                    Some(len) => len,
                    None => file.len().map_err(CheckpointError::io("measure", from))?,
                };
                self.waiting.push(Waiting {
                    from: from.to_path_buf(),
                    file,
                    to,
                    len,
                });
                Ok(())
            }
            Err(error) => Err(CheckpointError::io("link", from)(error)),
        }
    }

    /// Copies every file that could not be linked, and makes each copy
    /// durable.
    pub(super) fn copy_waiting(&mut self) -> Result<(), CheckpointError> {
        let mut chunk = Vec::new();
        for waiting in std::mem::take(&mut self.waiting) {
            let Waiting {
                from,
                file,
                to,
                len,
            } = waiting;
            let copy = self
                .vfs
                .open(&to, OpenMode::Append(Create::New))
                .map_err(CheckpointError::io("create", &to))?;
            // Only now is the name this creation's: one that was there is
            // somebody else's file.
            self.files.push(to.clone());
            let mut at = 0;
            while at < len {
                let part = (len - at).min(COPY_CHUNK_LEN) as usize;
                chunk.resize(part, 0);
                file.read_exact_at(&mut chunk, at)
                    .map_err(CheckpointError::io("read", &from))?;
                copy.append(&chunk)
                    .map_err(CheckpointError::io("append to", &to))?;
                at += part as u64;
            }
            copy.sync_all().map_err(CheckpointError::io("sync", &to))?;
            self.copied += 1;
            self.copied_bytes += len;
        }
        Ok(())
    }

    /// Makes every name in the target durable: the files in each directory
    /// of it, and then the directories and the files in the target itself.
    pub(super) fn sync_names(&self) -> Result<(), CheckpointError> {
        for dir in self.dirs.iter().chain([&self.dir]) {
            self.vfs
                .sync_dir(dir)
                .map_err(CheckpointError::io("sync the directory", dir))?;
        }
        Ok(())
    }

    /// Writes `first` as the manifest of the target, which makes it a
    /// checkpoint. Every file `first` names is in the target, durably.
    pub(super) fn install(&mut self, first: Manifest) -> Result<(), CheckpointError> {
        match ManifestStore::create(self.vfs.clone(), &self.dir, first) {
            Ok(_) => Ok(()),
            // A manifest that is there is not this creation's: somebody
            // wrote one into the target without its lock.
            Err(theirs @ SegmentStoreError::ManifestExists { .. }) => {
                Err(CheckpointError::Install(theirs))
            }
            Err(error) => {
                self.installing = true;
                Err(CheckpointError::Install(error))
            }
        }
    }

    /// Takes away what this creation made, as far as the file layer lets
    /// it: the manifest first, if this creation got as far as writing one,
    /// so that what is left is no checkpoint, then the files it linked or
    /// created, the directories, the lock file and the target. The lock
    /// file is this creation's: the target held nothing else when its lock
    /// was taken. Nothing else is touched: a file that somebody else put
    /// into the target since stays, and the target with it. What cannot be
    /// removed stays for the caller to remove.
    ///
    /// **No file goes while the manifest could come back.** Where a
    /// manifest was written and its removal cannot be made durable,
    /// everything else stays: every file it names is durable by then, so
    /// what a power cut leaves is the whole checkpoint or a directory
    /// without a manifest, and never a manifest without one of its files.
    pub(super) fn remove(self) {
        let vfs = &*self.vfs;
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

/// Removes what `path` names: a file, or a directory with everything in
/// it. A link to somewhere else is removed and not followed.
fn remove_entry(vfs: &dyn Vfs, path: &Path, kind: EntryKind) -> Result<(), CheckpointError> {
    if kind != EntryKind::Dir {
        return vfs
            .remove_file(path)
            .map_err(CheckpointError::io("remove", path));
    }
    let entries = vfs
        .read_dir(path)
        .map_err(CheckpointError::io("list", path))?;
    for entry in entries {
        remove_entry(vfs, &path.join(&entry.name), entry.kind)?;
    }
    vfs.remove_dir(path)
        .map_err(CheckpointError::io("remove the directory", path))
}

/// Deletes the checkpoint in the directory `dir` of `vfs`: the directory
/// goes, with everything in it, whoever put it there.
///
/// **Deleting a checkpoint is removing its directory, and nothing else.**
/// A checkpoint holds nothing in the database it was made from
/// (`transactions.md` CK7), so nothing there is told. The files the two
/// share are hard links: the live database keeps every one of them under
/// its own name, and a file that the live database has deleted since gives
/// its space back when the checkpoint's name for it goes. So a caller may
/// as well remove the directory itself, as Platform does (P-18 of
/// `platform-requirements.md`), also while a handle has the checkpoint
/// open: see the [module documentation](super) for what that handle does
/// then.
///
/// This call is the careful way. It removes nothing unless
///
/// - `dir` is a directory of its own in the directory above it, and not a
///   link to one ([`CheckpointError::NotADirectory`]): a link is not
///   followed into what it leads to;
/// - the directory has a manifest of this engine, under the manifest's
///   name or under [`DELETING_FILE_NAME`], in a file of its own and not
///   behind a link ([`CheckpointError::NotADatabase`]): a name alone is no
///   proof, and a file that is named as either and holds anything else
///   makes the directory nobody's that this call knows;
/// - no handle has it open ([`CheckpointError::Open`]): the call takes the
///   directory's lock for the time of the removal, and looks again under
///   it at what the directory is.
///
/// A checkpoint is a database like any other once it is made, so the call
/// cannot tell one from a database that was created, and deletes that
/// too. It does not keep a path from being given another meaning while it
/// runs: whoever replaces `dir` by a link during the call has it followed.
///
/// **The directory stops being a database before anything in it goes.**
/// The manifest is renamed to [`DELETING_FILE_NAME`], nothing is left
/// under the manifest's own name, and that is made durable. Only then is
/// the first file removed. So if the removal is cut short, by an error or
/// by the power, the directory either opens as the whole checkpoint it
/// was, with every file that anybody put beside it, or it has no manifest
/// and opens as nothing. When the call returns `Ok`, the removal is
/// durable.
///
/// **What a deletion leaves that was cut short is finished by the next
/// call**: a directory with the renamed manifest, whatever else it holds.
/// Two more things are removed whatever they were, since nothing is lost
/// with them. An empty directory is removed without its lock, by a removal
/// that fails if it is no longer empty. And of a directory that holds an
/// empty lock file and nothing else, the lock file is removed under its
/// lock and then the directory in the same way: a deletion leaves one
/// where somebody took the lock of the directory while its last names
/// went, and a creation where it could not take the lock of its target.
pub fn delete(vfs: &dyn Vfs, dir: &Path) -> Result<(), CheckpointError> {
    let not_a_directory = || CheckpointError::NotADirectory {
        dir: dir.to_path_buf(),
    };
    let (Some(parent), Some(name)) = (parent_of(dir), dir.file_name()) else {
        return Err(not_a_directory());
    };
    let beside = vfs
        .read_dir(parent)
        .map_err(CheckpointError::io("list", parent))?;
    let own = |entry: &DirEntry| entry.name == name && entry.kind == EntryKind::Dir;
    if !beside.iter().any(own) {
        return Err(not_a_directory());
    }
    let entries = vfs
        .read_dir(dir)
        .map_err(CheckpointError::io("list", dir))?;
    if !entries.is_empty() {
        // Nothing is changed, the lock file included, in a directory that
        // is neither a database nor what a deletion left.
        Left::of(vfs, dir, &entries)?;
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = vfs.lock(&lock_path).map_err(|source| match source.kind() {
            ErrorKind::WouldBlock => CheckpointError::Open {
                dir: dir.to_path_buf(),
            },
            _ => CheckpointError::io("lock", &lock_path)(source),
        })?;
        empty(vfs, dir)?;
        drop(lock);
    }
    vfs.remove_dir(dir)
        .map_err(CheckpointError::io("remove the directory", dir))?;
    vfs.sync_dir(parent)
        .map_err(CheckpointError::io("sync the directory", parent))
}

/// What a directory that [`delete`] may empty is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Left {
    /// A database: it has a manifest of this engine.
    Database,
    /// A database whose deletion has begun: it has a manifest of this
    /// engine under the name of one that is being deleted. Whatever it has
    /// under the manifest's own name goes before anything else does: the
    /// same file under its second name, or what somebody made of the
    /// directory since, against the rule for one that is being deleted.
    BeingDeleted,
    /// An empty lock file and nothing else.
    LockAlone,
}

impl Left {
    /// What the directory `dir` with `entries` is, or
    /// [`CheckpointError::NotADatabase`] for one that is none of these.
    /// Nothing is read through a link: a manifest is a file that the
    /// directory itself holds.
    fn of(vfs: &dyn Vfs, dir: &Path, entries: &[DirEntry]) -> Result<Self, CheckpointError> {
        let is_file = |name: &str| {
            (entries.iter()).any(|entry| entry.name == name && entry.kind == EntryKind::File)
        };
        let not_a_database = |source| CheckpointError::NotADatabase {
            dir: dir.to_path_buf(),
            source,
        };
        let no_manifest = || {
            not_a_database(SegmentStoreError::NoManifest {
                dir: dir.to_path_buf(),
            })
        };
        // A renamed manifest is a manifest still, and is known by what it
        // holds. A file that is only named like one says nothing.
        let renamed = dir.join(DELETING_FILE_NAME);
        if is_file(DELETING_FILE_NAME) && holds_manifest(vfs, &renamed)? {
            return Ok(Self::BeingDeleted);
        }
        if is_file(ManifestStore::FILE_NAME) {
            return match read_manifest(vfs, dir) {
                Ok(_) => Ok(Self::Database),
                Err(source) => Err(not_a_database(source)),
            };
        }
        if entries.len() != 1 || !is_file(LOCK_FILE_NAME) {
            return Err(no_manifest());
        }
        let lock_path = dir.join(LOCK_FILE_NAME);
        let len = vfs
            .open(&lock_path, OpenMode::Read)
            .and_then(|lock| lock.len())
            .map_err(CheckpointError::io("measure", &lock_path))?;
        match len {
            0 => Ok(Self::LockAlone),
            _ => Err(no_manifest()),
        }
    }
}

/// Whether the file `path` holds a manifest of this engine.
fn holds_manifest(vfs: &dyn Vfs, path: &Path) -> Result<bool, CheckpointError> {
    let file = vfs
        .open(path, OpenMode::Read)
        .map_err(CheckpointError::io("open", path))?;
    let len = file.len().map_err(CheckpointError::io("measure", path))?;
    let mut bytes = vec![0; len.min(Manifest::MAX_ENCODED_LEN as u64 + 1) as usize];
    file.read_exact_at(&mut bytes, 0)
        .map_err(CheckpointError::io("read", path))?;
    Ok(Manifest::decode(&bytes).is_ok())
}

/// Removes everything from the directory `dir`, whose lock the caller
/// holds, the lock file included.
fn empty(vfs: &dyn Vfs, dir: &Path) -> Result<(), CheckpointError> {
    let list = || vfs.read_dir(dir).map_err(CheckpointError::io("list", dir));
    let sync = || {
        vfs.sync_dir(dir)
            .map_err(CheckpointError::io("sync the directory", dir))
    };
    let remove = |name: &str| {
        let path = dir.join(name);
        match vfs.remove_file(&path) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                Err(CheckpointError::io("remove", &path)(error))
            }
            _ => Ok(()),
        }
    };
    // What the directory is, asked again now that nobody else is in it.
    match Left::of(vfs, dir, &list()?)? {
        // Nothing but the lock file is this call's to remove here. The
        // directory goes afterwards if nothing has come into it.
        Left::LockAlone => return remove(LOCK_FILE_NAME),
        Left::Database => {
            let manifest = dir.join(ManifestStore::FILE_NAME);
            vfs.rename(&manifest, &dir.join(DELETING_FILE_NAME))
                .map_err(CheckpointError::io("rename", &manifest))?;
        }
        Left::BeingDeleted => {}
    }
    // Nothing keeps the manifest's own name: a rename changes nothing
    // where both names are of one file, and a directory that is being
    // deleted may have been given a manifest since. From the sync on the
    // directory is no database, whatever a power cut keeps of the removals
    // below.
    remove(ManifestStore::FILE_NAME)?;
    sync()?;
    let kept = [DELETING_FILE_NAME, LOCK_FILE_NAME];
    for entry in list()? {
        if !kept.iter().any(|name| entry.name == *name) {
            remove_entry(vfs, &dir.join(&entry.name), entry.kind)?;
        }
    }
    // The renamed manifest goes last, and only once every other removal is
    // durable: what a power cut leaves is known by it for what it is, or
    // is empty. The lock file goes before it for that reason. Whoever
    // takes the lock of the directory from here makes a new lock file and
    // finds no database. Nothing but the renamed manifest is removed any
    // more, so their lock file stays, and the directory with it, for the
    // next call to remove.
    remove(LOCK_FILE_NAME)?;
    sync()?;
    remove(DELETING_FILE_NAME)
}
