//! The files of a checkpoint's directory: each is a hard link to a file of
//! the live database, or a copy of it where no link can be made, and a
//! creation that fails takes away what it made.

use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::FileId;
use lignin_io::{Create, DirEntry, EntryKind, OpenMode, Vfs, VfsFile, VfsLock};
use lignin_store::segment::{read_manifest, FileNames, ManifestStore};

use super::error::CheckpointError;

/// The file whose lock lets one handle at a time into a database directory
/// (`lignin_io::Vfs::lock`).
pub(super) const LOCK_FILE_NAME: &str = "LOCK";

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

/// Whether a hard link that failed with `kind` is one that the file system
/// does not make, so that a copy stands in for it: the two names are on
/// different file systems, the file has as many names as it can have, or
/// the file system has no hard links, which it says as "not supported" or,
/// on Linux, as "not permitted". Where a link is refused for want of a
/// permission, the copy is refused too, when the file is opened or its
/// copy is created.
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
    /// The names given to files in the target, linked or to be copied.
    files: Vec<PathBuf>,
    waiting: Vec<Waiting>,
    /// The lock of the target as a database directory, held while it is
    /// made: whoever opens a directory takes it first.
    lock: Option<Box<dyn VfsLock>>,
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
        let mut made = Self {
            vfs,
            dir: dir.to_path_buf(),
            parent: parent.to_path_buf(),
            dirs: Vec::new(),
            files: Vec::new(),
            waiting: Vec::new(),
            lock: None,
            linked: 0,
            copied: 0,
            copied_bytes: 0,
        };
        let lock_path = dir.join(LOCK_FILE_NAME);
        match made.vfs.lock(&lock_path) {
            Ok(lock) => made.lock = Some(lock),
            Err(source) => {
                made.remove();
                return Err(CheckpointError::io("lock", &lock_path)(source));
            }
        }
        Ok(made)
    }

    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
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
    pub(super) fn carry(&mut self, source: &Path, name: &str) -> Result<(), CheckpointError> {
        let plain = Path::new(name).file_name() == Some(OsStr::new(name));
        let reserved = [
            ManifestStore::FILE_NAME,
            ManifestStore::TEMP_FILE_NAME,
            LOCK_FILE_NAME,
        ];
        if !plain || reserved.contains(&name) {
            return Err(CheckpointError::CarriedName {
                name: name.to_string(),
            });
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
            self.files.push(to.clone());
            let copy = self
                .vfs
                .open(&to, OpenMode::Append(Create::New))
                .map_err(CheckpointError::io("create", &to))?;
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

    /// Takes away what this creation made, as far as the file layer lets
    /// it: the manifest first, so that what is left is no checkpoint, then
    /// the files, the directories, the lock file and the target. Nothing
    /// else is touched, and what cannot be removed stays for the caller to
    /// remove.
    pub(super) fn remove(self) {
        let vfs = &*self.vfs;
        for name in [ManifestStore::FILE_NAME, ManifestStore::TEMP_FILE_NAME] {
            let _ = vfs.remove_file(&self.dir.join(name));
        }
        let _ = vfs.sync_dir(&self.dir);
        let waiting = self.waiting.iter().map(|waiting| &waiting.to);
        for file in self.files.iter().chain(waiting).rev() {
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
/// This call is the careful way. It removes nothing unless the directory
/// has a manifest of this engine, or is what a deletion that was cut short
/// left of one ([`CheckpointError::NotADatabase`]), and no handle has it
/// open ([`CheckpointError::Open`]): it takes the directory's lock for the
/// time of the removal. A checkpoint is a database like any
/// other once it is made, so the call cannot tell one from a database that
/// was created, and deletes that too.
///
/// The manifest goes after everything but the lock file, and only once
/// the removal of everything else is durable. If the removal is cut
/// short, by an error or by the power, what is left still has the
/// manifest and no longer opens, or is the lock file alone, or is empty,
/// and the call can be made again: it finishes a directory that holds
/// nothing else. When it returns `Ok`, the removal is durable.
pub fn delete(vfs: &dyn Vfs, dir: &Path) -> Result<(), CheckpointError> {
    if let Err(source) = read_manifest(vfs, dir) {
        // What a deletion leaves that was cut short behind the manifest.
        let lock_alone =
            |entries: Vec<DirEntry>| (entries.iter()).all(|entry| entry.name == LOCK_FILE_NAME);
        if !vfs.read_dir(dir).is_ok_and(lock_alone) {
            return Err(CheckpointError::NotADatabase {
                dir: dir.to_path_buf(),
                source,
            });
        }
    }
    let lock_path = dir.join(LOCK_FILE_NAME);
    let lock = vfs.lock(&lock_path).map_err(|source| match source.kind() {
        ErrorKind::WouldBlock => CheckpointError::Open {
            dir: dir.to_path_buf(),
        },
        _ => CheckpointError::io("lock", &lock_path)(source),
    })?;
    let last = [ManifestStore::FILE_NAME, LOCK_FILE_NAME];
    let entries = vfs
        .read_dir(dir)
        .map_err(CheckpointError::io("list", dir))?;
    for entry in entries {
        if !last.iter().any(|name| entry.name == *name) {
            remove_entry(vfs, &dir.join(&entry.name), entry.kind)?;
        }
    }
    // A power cut keeps the removal of a name with or without the removals
    // before it. The manifest goes only once those are durable, so that
    // none of them comes back beside a directory that has no manifest.
    vfs.sync_dir(dir)
        .map_err(CheckpointError::io("sync the directory", dir))?;
    for name in last {
        let path = dir.join(name);
        match vfs.remove_file(&path) {
            // A deletion that is made again has no manifest to remove.
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            removed => removed.map_err(CheckpointError::io("remove", &path))?,
        }
    }
    drop(lock);
    vfs.remove_dir(dir)
        .map_err(CheckpointError::io("remove the directory", dir))?;
    match parent_of(dir) {
        Some(parent) => vfs
            .sync_dir(parent)
            .map_err(CheckpointError::io("sync the directory", parent)),
        None => Ok(()),
    }
}
