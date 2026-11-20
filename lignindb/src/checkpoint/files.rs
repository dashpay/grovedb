//! The files of a checkpoint's directory: each is a hard link to a file of
//! the live database, or a copy of it where no link can be made. The
//! directory is built under another name and renamed to the target's, and
//! a creation that fails takes away what it made.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{manifest::Manifest, FileId};
use lignin_io::{Create, DirEntry, EntryKind, OpenMode, SyncItem, Vfs, VfsFile, VfsLock};
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

/// What a creation adds to the name of its target for the directory it
/// builds the checkpoint in, beside the target: a checkpoint at `7` is
/// built in `7.creating`, which is renamed to `7` once it is whole and
/// durable.
///
/// **A directory of such a name is a creation's, with everything in it,
/// and its callers open none as a database.** Nothing in the engine's open
/// knows the name: it would take an empty one, or one with a lock file
/// alone, for a place to make a database in. One whose lock is held is
/// taken for one that is being built, and is left alone. Any other is
/// taken for what a creation that was cut short left, a part of a
/// checkpoint or a whole one that did not get its name: the next creation
/// at the same target empties it and builds in it, and [`delete`] removes
/// it. No checkpoint is made at a name that ends so
/// ([`CheckpointError::ReservedName`]).
pub const CREATING_SUFFIX: &str = ".creating";

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

/// The directory a checkpoint at `target` is built in: beside `target`,
/// under its name with [`CREATING_SUFFIX`] added. `None` for a path with
/// no name of its own.
pub(super) fn building_of(target: &Path) -> Option<PathBuf> {
    let mut name = target.file_name()?.to_os_string();
    name.push(CREATING_SUFFIX);
    Some(target.with_file_name(name))
}

/// Whether `path` has a name that a creation builds under.
fn is_building(path: &Path) -> bool {
    let suffix = CREATING_SUFFIX.as_bytes();
    (path.file_name()).is_some_and(|name| name.as_encoded_bytes().ends_with(suffix))
}

/// What `path` is in the directory `parent` above it, which is listed and
/// not followed: a directory of its own, a file, or something else, such
/// as a link. `None` if nothing there has its name.
fn kind_of(
    vfs: &dyn Vfs,
    parent: &Path,
    path: &Path,
) -> Result<Option<EntryKind>, CheckpointError> {
    let beside = vfs
        .read_dir(parent)
        .map_err(CheckpointError::io("list", parent))?;
    let name = path.file_name();
    let named = beside.iter().find(|entry| Some(&*entry.name) == name);
    Ok(named.map(|entry| entry.kind))
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

/// The directory a checkpoint is being built in, with everything this
/// creation has put into it, and the name it is to get.
pub(super) struct Target {
    vfs: Arc<dyn Vfs>,
    /// The name the checkpoint is to have.
    target: PathBuf,
    /// The directory it is built in ([`CREATING_SUFFIX`]): every path
    /// below is in it.
    dir: PathBuf,
    parent: PathBuf,
    /// The directories made in it, in the order they were made.
    dirs: Vec<PathBuf>,
    /// The files this creation has put into it: links, copies and the
    /// manifest.
    files: Vec<PathBuf>,
    waiting: Vec<Waiting>,
    /// The files this creation wrote, each open through the handle that
    /// wrote it until it is made durable through that handle
    /// ([`Self::settle`]).
    written: Vec<Box<dyn VfsFile>>,
    /// The lock of the directory as a database directory, held from its
    /// making until the creation ends: whoever opens a directory takes it
    /// first, and it goes with the directory when that is renamed.
    lock: Box<dyn VfsLock>,
    /// Whether the directory has been given the name of the target.
    placed: bool,
    pub(super) linked: usize,
    pub(super) copied: usize,
    pub(super) copied_bytes: u64,
}

impl Target {
    /// Refuses, before anything is done for it, a target that is there, as
    /// a directory or as anything else, one with no directory above it or
    /// no name of its own, and one whose name is a creation's to build
    /// under. [`Self::place`] is what decides.
    pub(super) fn refuse_existing(vfs: &dyn Vfs, target: &Path) -> Result<(), CheckpointError> {
        if parent_of(target).is_none() || target.file_name().is_none() {
            return Err(CheckpointError::NoParent {
                target: target.to_path_buf(),
            });
        }
        if is_building(target) {
            return Err(CheckpointError::ReservedName {
                target: target.to_path_buf(),
            });
        }
        Self::refuse_taken(vfs, target)
    }

    /// Refuses a target whose name is taken, by a directory or by
    /// anything else, and one of which the file layer does not say
    /// whether it is.
    fn refuse_taken(vfs: &dyn Vfs, target: &Path) -> Result<(), CheckpointError> {
        match vfs.read_dir(target) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(CheckpointError::TargetExists {
                target: target.to_path_buf(),
            }),
            Err(error) if error.kind() == ErrorKind::NotADirectory => {
                Err(CheckpointError::TargetExists {
                    target: target.to_path_buf(),
                })
            }
            Err(error) => Err(CheckpointError::io("look at", target)(error)),
        }
    }

    /// Makes the directory the checkpoint at `target` is built in, beside
    /// `target` in a directory that is there, and takes its lock: nobody
    /// that takes the lock opens a directory that is being built, and no
    /// second creation builds in it.
    ///
    /// **A directory that is there under that name is a creation's**
    /// ([`CREATING_SUFFIX`]). Where its lock is held, nothing of it is
    /// touched ([`CheckpointError::Creating`]). Where the lock is had, it
    /// is taken for what an earlier creation left ([`take_over`]):
    /// everything in it goes but the lock file, and this creation builds
    /// in it. Only a directory of its own is taken so: a link of that name
    /// is refused ([`CheckpointError::NotADirectory`]), at the look before
    /// the lock and at the one under it. A link that is put there between
    /// two calls of this function is followed by the next: the file layer
    /// names a directory by its path, and the lock file is made where the
    /// path leads.
    ///
    /// **The directory is looked at under its lock, whoever made it.** One
    /// that this call has just made is empty until its lock file is in it,
    /// and a rename replaces an empty directory: another creation of the
    /// same target, which fails at that moment, can give its own directory
    /// the name back onto this one. So a directory that holds anything but
    /// its lock file once the lock is had is taken over like one that was
    /// there. From the lock on, the directory has an entry, and no rename
    /// replaces it.
    pub(super) fn make(vfs: Arc<dyn Vfs>, target: &Path) -> Result<Self, CheckpointError> {
        let (Some(parent), Some(dir)) = (parent_of(target), building_of(target)) else {
            return Err(CheckpointError::NoParent {
                target: target.to_path_buf(),
            });
        };
        let made = match vfs.create_dir(&dir) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if kind_of(&*vfs, parent, &dir)? != Some(EntryKind::Dir) {
                    return Err(CheckpointError::NotADirectory { dir });
                }
                false
            }
            Err(error) => return Err(CheckpointError::io("create the directory", &dir)(error)),
        };
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = match vfs.lock(&lock_path) {
            Ok(lock) => lock,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                return Err(CheckpointError::Creating { dir });
            }
            Err(error) => {
                if made {
                    let _ = vfs.remove_dir(&dir);
                }
                return Err(CheckpointError::io("lock", &lock_path)(error));
            }
        };
        if kind_of(&*vfs, parent, &dir)? != Some(EntryKind::Dir) {
            return Err(CheckpointError::NotADirectory { dir });
        }
        let entries = vfs
            .read_dir(&dir)
            .map_err(CheckpointError::io("list", &dir))?;
        if !made || entries.iter().any(|entry| entry.name != LOCK_FILE_NAME) {
            take_over(&*vfs, parent, &dir)?;
        }
        Ok(Self {
            vfs,
            target: target.to_path_buf(),
            dir,
            parent: parent.to_path_buf(),
            dirs: Vec::new(),
            files: Vec::new(),
            waiting: Vec::new(),
            written: Vec::new(),
            lock,
            placed: false,
            linked: 0,
            copied: 0,
            copied_bytes: 0,
        })
    }

    /// Gives file `id` of the kind `names` in the database directory
    /// `source` its name in the checkpoint, of which `len` bytes are the
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
    /// the same name in the checkpoint.
    ///
    /// The name is that of a file in `source`, and of nothing else: a link
    /// to a file would be linked as the link it is and copied as the file
    /// it leads to, and in the checkpoint it would lead nowhere.
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

    /// Copies every file that could not be linked. Each copy stays open:
    /// [`Self::settle`] makes it durable through the handle that wrote it.
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
            self.written.push(copy);
            self.copied += 1;
            self.copied_bytes += len;
        }
        Ok(())
    }

    /// Writes `first` as the manifest of the checkpoint, under the
    /// manifest's name and with the bytes the store reads there
    /// (`lignin_store::segment::read_manifest`). It is not durable before
    /// [`Self::settle`], and the directory has not the target's name
    /// before [`Self::place`].
    pub(super) fn write_manifest(&mut self, first: &Manifest) -> Result<(), CheckpointError> {
        let bytes = first.encode()?;
        let path = self.dir.join(ManifestStore::FILE_NAME);
        let manifest = self
            .vfs
            .open(&path, OpenMode::Write(Create::New))
            .map_err(CheckpointError::io("create", &path))?;
        self.files.push(path.clone());
        manifest
            .write_at(&bytes, 0)
            .map_err(CheckpointError::io("write", &path))?;
        self.written.push(manifest);
        Ok(())
    }

    /// Makes durable, as one set of syncs (`lignin_io::Vfs::sync_together`),
    /// everything in the directory and the directory's own name: each file
    /// this creation wrote, through the handle that wrote it, every name
    /// in the directories it made and in the directory itself, and the
    /// name it is built under in the directory above.
    ///
    /// The set has no order and is not whole before it returns, and needs
    /// neither: until [`Self::place`] the directory has a name under which
    /// nothing opens it.
    pub(super) fn settle(&self) -> Result<(), CheckpointError> {
        let files = self.written.iter().map(|file| SyncItem::All(&**file));
        let dirs = (self.dirs.iter())
            .chain([&self.dir, &self.parent])
            .map(|dir| SyncItem::Dir(dir));
        let items: Vec<SyncItem<'_>> = files.chain(dirs).collect();
        self.vfs
            .sync_together(&items)
            .map_err(CheckpointError::io("make durable what is in", &self.dir))
    }

    /// Gives the directory the name of the target, which makes it the
    /// checkpoint, and makes that durable. Everything in it is durable by
    /// now, and so is the name it had ([`Self::settle`]): a power cut
    /// leaves it whole under one of its two names.
    ///
    /// The target's name is looked at once more: one that is taken by now
    /// is refused, and nothing is put there. So is a rename onto a file or
    /// onto a directory with entries, which the file layer refuses. An
    /// empty directory that is made there between this look and the rename
    /// is replaced. And the directory this creation built in is looked
    /// for, with what it holds ([`Self::found_whole`]): if it is not found
    /// so, nothing is renamed ([`CheckpointError::Replaced`]). The looks
    /// come right before the rename, which names its directory by its
    /// path: what is put at that path, or changed in the directory,
    /// between the looks and the rename is renamed as it is.
    pub(super) fn place(&mut self) -> Result<(), CheckpointError> {
        Self::refuse_taken(&*self.vfs, &self.target)?;
        self.found_whole()?;
        (self.vfs.rename(&self.dir, &self.target)).map_err(|error| match error.kind() {
            ErrorKind::DirectoryNotEmpty | ErrorKind::NotADirectory => {
                CheckpointError::TargetExists {
                    target: self.target.clone(),
                }
            }
            _ => CheckpointError::io("rename", &self.dir)(error),
        })?;
        self.placed = true;
        self.vfs
            .sync_dir(&self.parent)
            .map_err(CheckpointError::io("sync the directory", &self.parent))
    }

    /// Looks for the directory this creation built in, as it built it:
    /// at its path a directory of its own and not a link, with the lock
    /// file this creation holds, and holding the directories and the files
    /// the creation put into it and nothing else, each a directory of its
    /// own or a file. [`CheckpointError::Replaced`] if it is not found so.
    ///
    /// The file layer names everything by its path and keeps nobody from
    /// changing the directory meanwhile: this is what a look by names can
    /// tell. It does not tell a file from another file that was put under
    /// its name.
    fn found_whole(&self) -> Result<(), CheckpointError> {
        let vfs = &*self.vfs;
        let replaced = || CheckpointError::Replaced {
            dir: self.dir.clone(),
        };
        if !self.is_here(&self.dir)? {
            return Err(replaced());
        }
        let name_of = |path: &Path| path.file_name().map(OsStr::to_os_string);
        let mut held: BTreeMap<&Path, Vec<(OsString, EntryKind)>> = BTreeMap::new();
        let root = held.entry(&self.dir).or_default();
        root.push((LOCK_FILE_NAME.into(), EntryKind::File));
        root.extend((self.dirs.iter()).filter_map(|dir| Some((name_of(dir)?, EntryKind::Dir))));
        for dir in &self.dirs {
            held.entry(dir).or_default();
        }
        for file in &self.files {
            let (Some(dir), Some(name)) = (file.parent(), name_of(file)) else {
                return Err(replaced());
            };
            held.entry(dir).or_default().push((name, EntryKind::File));
        }
        // The directory itself comes before the directories in it.
        for (dir, mut names) in held {
            let listed: Vec<(OsString, EntryKind)> = (vfs.read_dir(dir))
                .map_err(CheckpointError::io("list", dir))?
                .into_iter()
                .map(|entry| (entry.name, entry.kind))
                .collect();
            names.sort_by(|one, other| one.0.cmp(&other.0));
            if listed != names {
                return Err(replaced());
            }
        }
        Ok(())
    }

    /// Whether the directory this creation built in is at `path`: a
    /// directory of its own there, with the lock file this creation holds.
    fn is_here(&self, path: &Path) -> Result<bool, CheckpointError> {
        let lock_path = path.join(LOCK_FILE_NAME);
        Ok(
            kind_of(&*self.vfs, &self.parent, path)? == Some(EntryKind::Dir)
                && (self.lock.is_at(&lock_path))
                    .map_err(CheckpointError::io("look at", &lock_path))?,
        )
    }

    /// Takes away what this creation made, as far as the file layer lets
    /// it: the files it linked or wrote, the directories, the lock file
    /// and the directory it built in. Nothing else is touched: a file that
    /// somebody else put there since this creation began stays, and the
    /// directory with it, for the next creation or a deletion to remove.
    /// What cannot be removed stays in the same way.
    ///
    /// **Nothing is removed from a directory that has, or can come back
    /// with, the name of the target.** If the directory was given that
    /// name, it is given its first name back, and only once that is
    /// durable is anything in it removed. Where the name cannot be taken
    /// back, or that cannot be made durable, everything stays: the
    /// checkpoint is whole under whichever of its two names a power cut
    /// leaves it, and the lock is given back.
    ///
    /// **And nothing is renamed or removed at a path where the directory
    /// this creation built in is not found**: a directory of its own
    /// there, with the lock file this creation holds. A file in a
    /// directory of it goes only if that is a directory of its own still,
    /// and not a link that was put in its place. These looks are made
    /// once, before the first removal: what is changed after them is not
    /// seen.
    pub(super) fn remove(self) {
        let vfs = &*self.vfs;
        let at = match self.placed {
            true => &self.target,
            false => &self.dir,
        };
        if !matches!(self.is_here(at), Ok(true)) {
            return;
        }
        if self.placed {
            let back = vfs.rename(&self.target, &self.dir);
            if back.is_err() || vfs.sync_dir(&self.parent).is_err() {
                return;
            }
        }
        let Ok(entries) = vfs.read_dir(&self.dir) else {
            return;
        };
        let own = |dir: &Path| {
            let is_dir = |entry: &DirEntry| {
                Some(&*entry.name) == dir.file_name() && entry.kind == EntryKind::Dir
            };
            dir == self.dir || entries.iter().any(is_dir)
        };
        drop(self.written);
        for file in self.files.iter().rev() {
            if file.parent().is_some_and(own) {
                let _ = vfs.remove_file(file);
            }
        }
        for dir in self.dirs.iter().rev() {
            if own(dir) {
                let _ = vfs.remove_dir(dir);
            }
        }
        let _ = vfs.remove_file(&self.dir.join(LOCK_FILE_NAME));
        drop(self.lock);
        let _ = vfs.remove_dir(&self.dir);
        let _ = vfs.sync_dir(&self.parent);
    }
}

/// Makes the directory `building`, which has a name that a creation
/// builds under and whose lock the caller holds, one that nothing is in
/// but its lock file.
///
/// **Its name is made durable first.** A creation that failed can have
/// given the directory the name of its target and taken it back without
/// either rename being durable ([`Target::remove`]), and a power cut can
/// keep what is removed from a directory and lose a rename of it: the
/// directory would be back at the target with a part of what it held. So
/// the directory `parent` above it is synced, which settles under which
/// name the directory is, before anything in it goes. If that fails,
/// nothing is removed.
fn take_over(vfs: &dyn Vfs, parent: &Path, building: &Path) -> Result<(), CheckpointError> {
    vfs.sync_dir(parent)
        .map_err(CheckpointError::io("sync the directory", parent))?;
    remove_all_in(vfs, building, &[LOCK_FILE_NAME])
}

/// Removes the directory `building`, which has a name that a creation
/// builds under ([`CREATING_SUFFIX`]), with everything in it, and says
/// whether there was such a directory. Of one whose lock is held nothing
/// is touched ([`CheckpointError::Creating`]). What has the name and is no
/// directory of its own is left alone, at the look before the lock and at
/// the one under it. A link that is put there between two calls of this
/// function is followed by the next, as [`delete`] says of the path it is
/// given: the lock file is then made where the link leads, and what is
/// there is emptied if the link comes after the second look.
fn remove_leftover(vfs: &dyn Vfs, parent: &Path, building: &Path) -> Result<bool, CheckpointError> {
    if kind_of(vfs, parent, building)? != Some(EntryKind::Dir) {
        return Ok(false);
    }
    let lock_path = building.join(LOCK_FILE_NAME);
    let lock = vfs.lock(&lock_path).map_err(|error| match error.kind() {
        ErrorKind::WouldBlock => CheckpointError::Creating {
            dir: building.to_path_buf(),
        },
        _ => CheckpointError::io("lock", &lock_path)(error),
    })?;
    // The look is made again under the lock: a link that was put there
    // since the first is not followed any further.
    if kind_of(vfs, parent, building)? != Some(EntryKind::Dir) {
        return Ok(false);
    }
    take_over(vfs, parent, building)?;
    vfs.remove_file(&lock_path)
        .map_err(CheckpointError::io("remove", &lock_path))?;
    drop(lock);
    vfs.remove_dir(building)
        .map_err(CheckpointError::io("remove the directory", building))?;
    vfs.sync_dir(parent)
        .map_err(CheckpointError::io("sync the directory", parent))?;
    Ok(true)
}

/// Removes what `path` names: a file, or a directory with everything in
/// it. A link to somewhere else is removed and not followed.
fn remove_entry(vfs: &dyn Vfs, path: &Path, kind: EntryKind) -> Result<(), CheckpointError> {
    if kind != EntryKind::Dir {
        return vfs
            .remove_file(path)
            .map_err(CheckpointError::io("remove", path));
    }
    remove_all_in(vfs, path, &[])?;
    vfs.remove_dir(path)
        .map_err(CheckpointError::io("remove the directory", path))
}

/// Removes everything that the directory `dir` holds but the entries
/// named in `kept`.
fn remove_all_in(vfs: &dyn Vfs, dir: &Path, kept: &[&str]) -> Result<(), CheckpointError> {
    let entries = vfs
        .read_dir(dir)
        .map_err(CheckpointError::io("list", dir))?;
    for entry in entries {
        if !kept.iter().any(|name| entry.name == *name) {
            remove_entry(vfs, &dir.join(&entry.name), entry.kind)?;
        }
    }
    Ok(())
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
/// This call is the careful way. What has the name `dir` goes only if
/// all of this holds, unless the name is one that creations build under
/// ([`CREATING_SUFFIX`]), which the end of this text is about:
///
/// - `dir` is a directory of its own in the directory above it, and not a
///   link to one ([`CheckpointError::NotADirectory`]): a link is not
///   followed into what it leads to;
/// - the directory has a manifest of this engine, under the manifest's
///   name or under [`DELETING_FILE_NAME`], in a file of its own and not
///   behind a link ([`CheckpointError::NotADatabase`]). A name alone is no
///   proof: a directory with a file that is named as either and holds
///   something else is known for a database only by a manifest under the
///   other name, and then what the first name holds goes with the rest;
/// - no handle has it open ([`CheckpointError::Open`]): the call takes the
///   directory's lock for the time of the removal, and looks again under
///   it at what the directory is.
///
/// A checkpoint is a database like any other once it is made, so the call
/// cannot tell one from a database that was created, and deletes that
/// too. It does not keep a path from being given another meaning while it
/// runs: whoever replaces `dir`, or the directory beside it that a
/// creation built in, by a link during the call has it followed.
///
/// **The directory stops being a database before anything in it goes.**
/// The manifest is renamed to [`DELETING_FILE_NAME`], nothing is left
/// under the manifest's own name, and that is made durable. Only then is
/// the first file removed. So if the removal is cut short, by an error or
/// by the power, the directory either opens as the whole checkpoint it
/// was, with every file that anybody put beside it, or it has no manifest
/// and opens as nothing. It has the name `dir` then, or, if a creation
/// had not made that name durable, the name it was built under, with
/// everything in it. When the call returns `Ok`, the removal is durable.
///
/// **What a deletion leaves that was cut short is finished by the next
/// call**: a directory with the renamed manifest, whatever else it holds.
/// Two more things are removed whatever they were, since nothing is lost
/// with them. An empty directory is removed without its lock, by a removal
/// that fails if it is no longer empty. And of a directory that holds an
/// empty lock file and nothing else, the lock file is removed under its
/// lock and then the directory in the same way: a deletion leaves one
/// where somebody took the lock of the directory while its last names
/// went.
///
/// **What a creation of a checkpoint at `dir` left beside it goes too**:
/// the directory it built in ([`CREATING_SUFFIX`]), with everything in it
/// and whatever it holds, under that directory's lock.
///
/// - Where `dir` was deleted, it goes afterwards. An error of its removal
///   is then returned though `dir` is gone. One whose lock is held is left
///   alone with no error: if a creation is building in it, a checkpoint
///   can get the name of `dir` again.
/// - Where nothing has the name of `dir`, the call returns `Ok` if there
///   was such a directory, and [`CheckpointError::Creating`] for one whose
///   lock is held.
/// - Where `dir` is refused, it is not looked for, and stays.
/// - Given such a directory as `dir`, the call removes it in the same
///   way, or fails with [`CheckpointError::NotADirectory`] if there is
///   none.
pub fn delete(vfs: &dyn Vfs, dir: &Path) -> Result<(), CheckpointError> {
    let not_a_directory = || CheckpointError::NotADirectory {
        dir: dir.to_path_buf(),
    };
    let Some(parent) = parent_of(dir) else {
        return Err(not_a_directory());
    };
    let found = |left: bool| match left {
        true => Ok(()),
        false => Err(not_a_directory()),
    };
    if is_building(dir) {
        return found(remove_leftover(vfs, parent, dir)?);
    }
    let left = || match building_of(dir) {
        Some(building) => remove_leftover(vfs, parent, &building),
        None => Ok(false),
    };
    match kind_of(vfs, parent, dir)? {
        Some(EntryKind::Dir) => {
            delete_database(vfs, dir, parent)?;
            match left() {
                Ok(_) | Err(CheckpointError::Creating { .. }) => Ok(()),
                Err(error) => Err(error),
            }
        }
        Some(_) => Err(not_a_directory()),
        None => found(left()?),
    }
}

/// Deletes the database in `dir`, a directory of its own in the directory
/// `parent`, as [`delete`] says.
fn delete_database(vfs: &dyn Vfs, dir: &Path, parent: &Path) -> Result<(), CheckpointError> {
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
    remove_all_in(vfs, dir, &[DELETING_FILE_NAME, LOCK_FILE_NAME])?;
    // The renamed manifest goes last, and only once every other removal is
    // durable: what a power cut leaves is known by it for what it is, or
    // is empty. The lock file goes before it for that reason. An open of
    // the engine is refused from here by the mark, before it locks.
    // Whoever else takes the lock of the directory makes a new lock file
    // and finds no database. Nothing but the renamed manifest is removed any
    // more, so their lock file stays, and the directory with it, for the
    // next call to remove.
    remove(LOCK_FILE_NAME)?;
    sync()?;
    remove(DELETING_FILE_NAME)
}
