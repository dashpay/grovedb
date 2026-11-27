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

/// What a creation adds to the name of its target for its staging
/// directory, beside the target: a checkpoint at `7` is built in
/// `7.creating/checkpoint.creating` ([`BUILDING_DIR_NAME`]), which is
/// renamed to `7` once it is whole and durable, and `7.creating` is
/// removed.
///
/// **A directory that the directory above lists under such a name, byte
/// for byte, is not to be opened as a database, and it is a creation's
/// only where it lists what a creation puts there and nothing else**: a
/// file under the name of the lock file, which is empty, and a directory
/// under [`BUILDING_DIR_NAME`], each or neither. A creation puts no third
/// entry into its staging directory: everything else it makes, it makes in
/// the directory of that name. So a directory that lists anything else,
/// under whatever name and of whatever kind, was not left so by a
/// creation: nothing in it is removed, locked or made, and the call that
/// was to build in it or to remove it fails
/// ([`CheckpointError::NotStaging`]). A database of any engine that lies
/// under such a name, and whatever else somebody put there, stays as it
/// is.
///
/// The engine's open refuses a path whose last name ends so, in any case
/// of its ASCII letters (`crate::db::OpenError::Creating`); a path that
/// leads to such a directory under another last name is its callers' to
/// keep from the open. A staging directory whose lock is found held is
/// taken for one that is being built in, and the call goes no further.
/// One whose lock is had is taken for what a creation that was cut short
/// left: the next creation at that target removes what was built in it,
/// under its lock, and builds in it, and [`delete`] removes it. One that
/// lists nothing under the name of its lock file, where two looks each
/// find that the name leads to something all the same and the listing
/// after each has no lock file still, is refused
/// ([`CheckpointError::NotALock`]).
///
/// A target that is given a name that ends so, whatever the case of its
/// ASCII letters, is refused ([`CheckpointError::ReservedName`]). **The
/// name under which the file system lists the target has to be outside
/// these names as well, and that is the caller's to see to.** A file
/// system can list a name in another spelling than it was given in, and a
/// creation does not know that spelling before the directory is there. A
/// checkpoint that the directory above lists under a name that ends in
/// `.creating`, byte for byte, is looked at as a staging directory, as
/// above: it lists a manifest and is refused for one.
///
/// What is looked at for this, and what the looks do not tell, is in the
/// [module documentation](super) ("Durability, and a creation that
/// fails").
pub const CREATING_SUFFIX: &str = ".creating";

/// The name, inside a creation's staging directory
/// ([`CREATING_SUFFIX`]), of the directory the checkpoint is built in:
/// the one that is renamed to the target. A creation makes it right after
/// it has the lock of the staging directory, and puts everything of the
/// checkpoint into it.
///
/// **It is what marks a staging directory as one that holds something of
/// a creation.** It needs no sync of its own to be there for whatever a
/// power cut keeps of what was built: a directory whose name is lost is
/// lost with everything in it. The name ends in [`CREATING_SUFFIX`], so
/// the engine's open refuses the directory under this name as it refuses
/// the staging directory.
pub const BUILDING_DIR_NAME: &str = "checkpoint.creating";

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

/// The staging directory of a creation at `target`: beside `target`,
/// under its name with [`CREATING_SUFFIX`] added. `None` for a path with
/// no name of its own.
pub(super) fn staging_of(target: &Path) -> Option<PathBuf> {
    let mut name = target.file_name()?.to_os_string();
    name.push(CREATING_SUFFIX);
    Some(target.with_file_name(name))
}

/// Whether `path` has a name that a creation gives its staging directory.
fn is_staging(path: &Path) -> bool {
    let suffix = CREATING_SUFFIX.as_bytes();
    (path.file_name()).is_some_and(|name| name.as_encoded_bytes().ends_with(suffix))
}

/// Whether the last component of `path` ends in the suffix reserved for
/// checkpoint creation, ignoring ASCII case. A target that is given as
/// such a path is refused, and so is an open of the engine that is given
/// one (`crate::db::OpenError::Creating`).
pub(crate) fn is_reserved(path: &Path) -> bool {
    let suffix = CREATING_SUFFIX.as_bytes();
    (path.file_name()).is_some_and(|name| {
        let name = name.as_encoded_bytes();
        (name.len().checked_sub(suffix.len()))
            .is_some_and(|at| name[at..].eq_ignore_ascii_case(suffix))
    })
}

/// What `path` is in the directory `parent` above it, which is listed and
/// not followed: a directory of its own, a file, or something else, such
/// as a link. `None` if `parent` lists nothing under the name of `path`,
/// byte for byte.
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

/// Reads `entries`, a listing of the directory `staging`, by the rule for
/// a staging directory ([`CREATING_SUFFIX`]), and says whether it lists
/// the directory a checkpoint is built in ([`BUILDING_DIR_NAME`]).
///
/// A creation puts two entries into its staging directory and never a
/// third: the lock file, which `lignin_io::Vfs::lock` makes empty and
/// which nobody writes, and the directory it builds in. So the listing
/// may hold a file of no length under the name of the lock file and a
/// directory under [`BUILDING_DIR_NAME`], each under that name byte for
/// byte, and nothing else. The first entry that is something else is the
/// error ([`CheckpointError::NotStaging`]): one of another name, one of
/// these names that is of another kind, such as a link, and a lock file
/// that holds a byte. The rule names what a creation of this engine makes,
/// and knows nothing of what else a directory can be.
///
/// It reads a listing, and opens the lock file to measure it: it makes,
/// locks and removes nothing.
fn staging_holds(
    vfs: &dyn Vfs,
    staging: &Path,
    entries: &[DirEntry],
) -> Result<bool, CheckpointError> {
    let not_staging = |entry: &DirEntry| CheckpointError::NotStaging {
        dir: staging.to_path_buf(),
        found: entry.name.clone(),
    };
    let mut building = false;
    for entry in entries {
        if entry.name == BUILDING_DIR_NAME && entry.kind == EntryKind::Dir {
            building = true;
        } else if entry.name == LOCK_FILE_NAME && entry.kind == EntryKind::File {
            let lock_path = staging.join(LOCK_FILE_NAME);
            let len = vfs
                .open(&lock_path, OpenMode::Read)
                .and_then(|lock| lock.len())
                .map_err(CheckpointError::io("measure", &lock_path))?;
            if len != 0 {
                return Err(not_staging(entry));
            }
        } else {
            return Err(not_staging(entry));
        }
    }
    Ok(building)
}

/// Takes the lock of the directory `building`, a staging directory which
/// was there before this call, which the directory above lists as a
/// directory and whose listing the rule for one admits
/// ([`staging_holds`]), in a lock file of its own.
/// [`CheckpointError::Creating`] where the lock is held.
///
/// Where `building` lists its lock file, under that name byte for byte,
/// the entry has to be a file ([`CheckpointError::NotALock`] otherwise),
/// and the lock is taken in the file that is there: none is made.
///
/// Where it lists none, the call looks whether the name leads to
/// something all the same, as it can where a file system takes several
/// spellings for one name: by a rename of the path of the lock file to
/// that same path. That rename follows no link at the name, and it
/// leaves what is there as it is where nothing on its path is changed
/// while it runs; what it can do otherwise is in the
/// [module documentation](super) ("A change after a look"). Where it
/// answers `NotFound`, the lock is taken by `lignin_io::Vfs::lock`, which
/// makes the lock file where there is none. Where it answers no error,
/// the directory is listed again, and if it still lists no lock file,
/// the same is done once more. A directory for which it answers no error
/// both times, and which lists no lock file after either, is refused
/// ([`CheckpointError::NotALock`]), with no lock file made. Another error
/// of it is returned.
fn lock_what_is_there(vfs: &dyn Vfs, building: &Path) -> Result<Box<dyn VfsLock>, CheckpointError> {
    let lock_path = building.join(LOCK_FILE_NAME);
    let refused = |error: std::io::Error| match error.kind() {
        ErrorKind::WouldBlock => CheckpointError::Creating {
            dir: building.to_path_buf(),
        },
        _ => CheckpointError::io("lock", &lock_path)(error),
    };
    let listed = || kind_of(vfs, building, &lock_path);
    let mut kind = listed()?;
    for _ in 0..2 {
        if kind.is_some() {
            break;
        }
        match vfs.rename(&lock_path, &lock_path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return vfs.lock(&lock_path).map_err(refused);
            }
            Ok(()) => kind = listed()?,
            Err(error) => return Err(CheckpointError::io("look for", &lock_path)(error)),
        }
    }
    if kind != Some(EntryKind::File) {
        return Err(CheckpointError::NotALock { path: lock_path });
    }
    vfs.lock_existing(&lock_path).map_err(refused)
}

/// The look at the directory at `path` under its lock: whether the
/// directory whose lock file `lock` is held on is there, as a directory of
/// its own in the directory `parent` above. If it is, what it holds is
/// returned. `parent` has to list `path` as a directory, the directory at
/// `path` has to list its lock file, under that name byte for byte, as a
/// file, and that file has to be the one `lock` is on. What is listed
/// under the name of the lock file as something else is not looked into.
///
/// Where `parent` lists nothing under the name of `path`, byte for byte,
/// the directory is not found, unless `made` says that the caller made it
/// itself: a file system can list a name it was given in another
/// spelling. The directory is then known by its lock file alone. That is
/// looked for in every entry of `parent` that is no file: it has to be
/// found in a directory, and in nothing else. So a link through which it
/// is found refuses the directory, whether `path` is that link or the
/// link is beside it. An entry of which the file layer fails to say
/// whether the lock file is in it counts as not having it if it is a
/// directory, and as having it if not.
fn held_at(
    vfs: &dyn Vfs,
    lock: &dyn VfsLock,
    parent: &Path,
    path: &Path,
    made: bool,
) -> Result<Option<Vec<DirEntry>>, CheckpointError> {
    let lock_in = |dir: &Path| lock.is_at(&dir.join(LOCK_FILE_NAME));
    let beside = vfs
        .read_dir(parent)
        .map_err(CheckpointError::io("list", parent))?;
    let name = path.file_name();
    let named = beside.iter().find(|entry| Some(&*entry.name) == name);
    let listed = match named {
        Some(named) => named.kind == EntryKind::Dir,
        None => made,
    };
    if !listed {
        return Ok(None);
    }
    let entries = match vfs.read_dir(path) {
        Ok(entries) => entries,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(error) => return Err(CheckpointError::io("list", path)(error)),
    };
    let is_lock = |entry: &DirEntry| entry.name == LOCK_FILE_NAME && entry.kind == EntryKind::File;
    if !entries.iter().any(is_lock)
        || !lock_in(path).map_err(CheckpointError::io("look for the lock file in", path))?
    {
        return Ok(None);
    }
    if named.is_none() {
        let mut found = false;
        for entry in beside.iter().filter(|entry| entry.kind != EntryKind::File) {
            match (entry.kind, lock_in(&parent.join(&entry.name))) {
                (_, Ok(false)) | (EntryKind::Dir, Err(_)) => {}
                (EntryKind::Dir, Ok(true)) => found = true,
                _ => return Ok(None),
            }
        }
        if !found {
            return Ok(None);
        }
    }
    Ok(Some(entries))
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

/// The staging directory of a creation ([`CREATING_SUFFIX`]), with its
/// lock held: nobody that takes the lock builds in it, empties it or
/// removes it meanwhile.
struct Staging {
    /// The directory above it, which is the one above the target.
    parent: PathBuf,
    path: PathBuf,
    lock: Box<dyn VfsLock>,
    /// Whether this creation made the directory, and did not find it
    /// there: only one it made is known by its lock file where the
    /// directory above lists nothing under its name ([`held_at`]).
    made: bool,
}

impl Staging {
    /// Whether the staging directory is at its path: a directory of its
    /// own there, with the lock file that is held ([`held_at`]). What it
    /// lists, if it is.
    fn here(&self, vfs: &dyn Vfs) -> Result<Option<Vec<DirEntry>>, CheckpointError> {
        held_at(vfs, &*self.lock, &self.parent, &self.path, self.made)
    }

    /// Removes the staging directory, where it is found at its path and
    /// lists nothing but its lock file: the lock file, and then the
    /// directory by a removal that fails if something has come into it.
    /// One that lists anything else stays as it is, with its lock file,
    /// and the lock is given back. Nothing is synced.
    fn leave(self, vfs: &dyn Vfs) {
        let Ok(Some(entries)) = self.here(vfs) else {
            return;
        };
        if entries.iter().any(|entry| entry.name != LOCK_FILE_NAME) {
            return;
        }
        let _ = vfs.remove_file(&self.path.join(LOCK_FILE_NAME));
        drop(self.lock);
        let _ = vfs.remove_dir(&self.path);
    }
}

/// The directory a checkpoint is being built in, with everything this
/// creation has put into it, and the name it is to get.
pub(super) struct Target {
    vfs: Arc<dyn Vfs>,
    /// The name the checkpoint is to have.
    target: PathBuf,
    /// The staging directory of the creation, beside the target.
    staging: Staging,
    /// The directory the checkpoint is built in, in the staging directory
    /// ([`BUILDING_DIR_NAME`]): every path below is in it.
    dir: PathBuf,
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
    /// The lock of the directory the checkpoint is built in, as a
    /// database directory, held from its making until the creation ends:
    /// whoever opens a directory takes it first, and it goes with the
    /// directory when that is renamed.
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
        if is_reserved(target) {
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

    /// Makes the staging directory of a creation at `target`, beside
    /// `target` in a directory that is there, takes its lock, and makes in
    /// it the directory the checkpoint is built in, with a lock of its
    /// own: nobody that takes the first lock builds in the staging
    /// directory or removes it meanwhile, and nobody that takes the second
    /// opens what is being built.
    ///
    /// **A directory that is there under the staging name is built in
    /// only if it is what a creation left** ([`CREATING_SUFFIX`]). The
    /// directory above has to list it, byte for byte, as a directory:
    /// anything else that the name leads to is refused
    /// ([`CheckpointError::NotADirectory`]). It is listed before anything
    /// is done to it, and where the listing holds anything but what a
    /// creation puts there, the call goes no further
    /// ([`staging_holds`], [`CheckpointError::NotStaging`]): no lock is
    /// taken in it, no lock file is made in it, and nothing of it is
    /// removed. Otherwise its lock is taken in a lock file of its own
    /// ([`lock_what_is_there`]). Where the lock is found held, the call
    /// goes no further ([`CheckpointError::Creating`]).
    ///
    /// **What is removed is decided under the lock, whoever made the
    /// directory.** What the path names then has to be a directory of its
    /// own with the lock file that was just locked ([`held_at`]), or
    /// nothing is emptied and nothing built
    /// ([`CheckpointError::Replaced`]); and what that directory lists
    /// then is read by the rule once more, so that one which was put
    /// there, or filled, between the first look and the lock is left as
    /// it is ([`CheckpointError::NotStaging`]). Where it lists the
    /// directory an earlier creation built in, that one is removed with
    /// what it holds ([`take_over`]), and nothing else is. A directory
    /// that this call has just made is read in the same way: it is empty
    /// until its lock file is in it, and somebody else can have been in
    /// it first.
    ///
    /// What these looks do not tell is in the [module
    /// documentation](super) ("Durability, and a creation that fails").
    pub(super) fn make(vfs: Arc<dyn Vfs>, target: &Path) -> Result<Self, CheckpointError> {
        let (Some(parent), Some(staging)) = (parent_of(target), staging_of(target)) else {
            return Err(CheckpointError::NoParent {
                target: target.to_path_buf(),
            });
        };
        let list = |dir: &Path| vfs.read_dir(dir).map_err(CheckpointError::io("list", dir));
        let made = match vfs.create_dir(&staging) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if kind_of(&*vfs, parent, &staging)? != Some(EntryKind::Dir) {
                    return Err(CheckpointError::NotADirectory { dir: staging });
                }
                staging_holds(&*vfs, &staging, &list(&staging)?)?;
                false
            }
            Err(error) => {
                return Err(CheckpointError::io("create the directory", &staging)(error));
            }
        };
        let lock_path = staging.join(LOCK_FILE_NAME);
        let lock = match made {
            false => lock_what_is_there(&*vfs, &staging)?,
            true => match vfs.lock(&lock_path) {
                Ok(lock) => lock,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return Err(CheckpointError::Creating { dir: staging });
                }
                Err(error) => {
                    let _ = vfs.remove_dir(&staging);
                    return Err(CheckpointError::io("lock", &lock_path)(error));
                }
            },
        };
        let staging = Staging {
            parent: parent.to_path_buf(),
            path: staging,
            lock,
            made,
        };
        let Some(entries) = staging.here(&*vfs)? else {
            return Err(CheckpointError::Replaced { dir: staging.path });
        };
        if staging_holds(&*vfs, &staging.path, &entries)? {
            take_over(&*vfs, &staging)?;
        }
        let dir = staging.path.join(BUILDING_DIR_NAME);
        let lock = match Self::begin(&*vfs, &staging, &dir) {
            Ok(lock) => lock,
            Err(error) => {
                // An empty directory that this call made goes again, and
                // the staging directory with it where that leaves it with
                // its lock file alone.
                let _ = vfs.remove_dir(&dir);
                staging.leave(&*vfs);
                return Err(error);
            }
        };
        Ok(Self {
            vfs,
            target: target.to_path_buf(),
            staging,
            dir,
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

    /// Makes `dir`, the directory the checkpoint is built in, in the
    /// staging directory, takes its lock, which makes its lock file, and
    /// looks for it under that lock: the staging directory has to list it
    /// under its name, byte for byte, as a directory of its own with that
    /// lock file ([`CheckpointError::Replaced`]).
    fn begin(
        vfs: &dyn Vfs,
        staging: &Staging,
        dir: &Path,
    ) -> Result<Box<dyn VfsLock>, CheckpointError> {
        vfs.create_dir(dir)
            .map_err(CheckpointError::io("create the directory", dir))?;
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = vfs
            .lock(&lock_path)
            .map_err(CheckpointError::io("lock", &lock_path))?;
        match held_at(vfs, &*lock, &staging.path, dir, false)? {
            Some(_) => Ok(lock),
            None => Err(CheckpointError::Replaced {
                dir: dir.to_path_buf(),
            }),
        }
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
    /// in the directories it made and in the directory itself, the name
    /// it is built under in the staging directory, and the name of that
    /// one in the directory above.
    ///
    /// The set has no order and is not whole before it returns, and needs
    /// neither: until [`Self::place`] the directory has a name under which
    /// nothing opens it.
    pub(super) fn settle(&self) -> Result<(), CheckpointError> {
        let files = self.written.iter().map(|file| SyncItem::All(&**file));
        let dirs = (self.dirs.iter())
            .chain([&self.dir, &self.staging.path, &self.staging.parent])
            .map(|dir| SyncItem::Dir(dir));
        let items: Vec<SyncItem<'_>> = files.chain(dirs).collect();
        self.vfs
            .sync_together(&items)
            .map_err(CheckpointError::io("make durable what is in", &self.dir))
    }

    /// Makes durable the name the directory has now, under the target's
    /// or in the staging directory: a rename between two directories is
    /// durable once both are synced (`lignin_io::Vfs::rename`), which
    /// this asks for as one set.
    fn settle_name(&self) -> Result<(), CheckpointError> {
        let dirs = [&self.staging.parent, &self.staging.path].map(|dir| SyncItem::Dir(dir));
        self.vfs.sync_together(&dirs).map_err(CheckpointError::io(
            "make durable the name of",
            &self.target,
        ))
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
        self.settle_name()
    }

    /// Looks for the directory this creation built in, as it built it:
    /// the staging directory at its path, a directory of its own with the
    /// lock file this creation holds on it and nothing beside that but
    /// the directory that was built in; that one at its path in it, a
    /// directory of its own and not a link, with the lock file this
    /// creation holds ([`held_at`]), and holding the directories and the
    /// files the creation put into it and nothing else, each a directory
    /// of its own or a file. [`CheckpointError::Replaced`] if it is not
    /// found so, of the staging directory where that is not found, or
    /// lists an entry of another name, and of the directory that was
    /// built in otherwise.
    ///
    /// The file layer names everything by its path and keeps nobody from
    /// changing the directory meanwhile: this is what a look by names can
    /// tell. It does not tell a file from another file that was put under
    /// its name. A name that a directory does not list as the creation
    /// gave it is looked for as [`Self::respelled`] says.
    fn found_whole(&self) -> Result<(), CheckpointError> {
        let vfs = &*self.vfs;
        let not_found = |dir: &Path| CheckpointError::Replaced {
            dir: dir.to_path_buf(),
        };
        let replaced = || not_found(&self.dir);
        let Some(staged) = self.staging.here(vfs)? else {
            return Err(not_found(&self.staging.path));
        };
        // Its lock file is listed as a file, or it was not found. What it
        // lists under the other name is looked at next.
        let is_ours =
            |entry: &DirEntry| entry.name == LOCK_FILE_NAME || entry.name == BUILDING_DIR_NAME;
        if !staged.iter().all(is_ours) {
            return Err(not_found(&self.staging.path));
        }
        if self.is_here(&self.dir)?.is_none() {
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
            if listed != names && !self.respelled(dir, &listed, &names) {
                return Err(replaced());
            }
        }
        Ok(())
    }

    /// Whether the directory `dir`, which lists `listed`, holds `names`
    /// and nothing else though the two lists differ: it lists as many
    /// files and as many directories as were put into it and nothing of
    /// another kind, and each of `names` that it does not list, byte for
    /// byte and as the kind it was put there as, is one it opens as a
    /// file, or lists as a directory, all the same. A name that is not
    /// listed can open on a file system that takes different spellings
    /// for one name. Where names are looked up byte for byte, and the
    /// directory does not change meanwhile, a name that is not listed
    /// opens nothing.
    fn respelled(
        &self,
        dir: &Path,
        listed: &[(OsString, EntryKind)],
        names: &[(OsString, EntryKind)],
    ) -> bool {
        let count =
            |of: &[(OsString, EntryKind)], kind| of.iter().filter(|entry| entry.1 == kind).count();
        let as_many = |kind| count(listed, kind) == count(names, kind);
        let found = |(name, kind): &(OsString, EntryKind)| {
            let path = dir.join(name);
            match kind {
                EntryKind::File => self.vfs.open(&path, OpenMode::Read).is_ok(),
                _ => self.vfs.read_dir(&path).is_ok(),
            }
        };
        listed.len() == names.len()
            && as_many(EntryKind::File)
            && as_many(EntryKind::Dir)
            && (names.iter())
                .filter(|name| !listed.contains(name))
                .all(found)
    }

    /// Whether the directory this creation built in is at `path`, which
    /// is its path in the staging directory or the target: a directory of
    /// its own there, with the lock file this creation holds
    /// ([`held_at`]). At the target it is known by that lock file where
    /// the directory above lists the name in another spelling, since this
    /// creation made the directory itself.
    fn is_here(&self, path: &Path) -> Result<Option<Vec<DirEntry>>, CheckpointError> {
        let (above, made) = match path == self.target {
            true => (&self.staging.parent, true),
            false => (&self.staging.path, false),
        };
        held_at(&*self.vfs, &*self.lock, above, path, made)
    }

    /// Ends a creation that made its checkpoint: the lock of the
    /// checkpoint is given back, and the staging directory, which holds
    /// its lock file and nothing else by now, is removed
    /// ([`Staging::leave`]).
    ///
    /// **That removal is not made durable, and its failure fails
    /// nothing**: the checkpoint is whole and durable at the target
    /// before it. A power cut can leave the staging directory beside the
    /// checkpoint, empty or with its lock file: it holds no file of the
    /// checkpoint and no space, and a [`delete`] of the target removes
    /// it.
    pub(super) fn leave(self) {
        drop(self.written);
        drop(self.lock);
        self.staging.leave(&*self.vfs);
    }

    /// Takes away what this creation made, as far as the file layer lets
    /// it and as far as it is found. A file it linked or wrote, a
    /// directory it made and the lock file go where the directory they
    /// are in lists them, under the name the creation gave, as a file or
    /// as a directory of its own. A file in a directory of it goes only if
    /// that directory is listed so itself, and is not a link that was put
    /// in its place. The directory it built in goes if that leaves it
    /// empty, and then the staging directory, if it lists nothing but its
    /// lock file ([`Staging::leave`]). Whatever is listed otherwise stays,
    /// and the directories with it, for the next creation or a deletion
    /// to deal with: what has a name the creation did not use, and what
    /// is listed, under a name it did use, as something else than the
    /// creation put there. A file that was put under the name of one of
    /// the creation's is not told from that one, and goes. What cannot be
    /// removed stays in the same way.
    ///
    /// **Nothing is removed from a directory that has, or can come back
    /// with, the name of the target.** If the directory was given that
    /// name, it is given its first name back, in the staging directory,
    /// and only once that is durable is anything in it removed. Where the
    /// name cannot be taken back, or that cannot be made durable,
    /// everything of the checkpoint stays: it is whole under whichever of
    /// its two names a power cut leaves it, and the locks are given back.
    /// Where it stays at the target, the staging directory, which holds
    /// its lock file alone, is removed.
    ///
    /// **And nothing is renamed or removed at a path where the directory
    /// this creation built in is not found**: a directory of its own
    /// there, with the lock file this creation holds ([`held_at`]), and
    /// the staging directory at its own path in the same way. Those looks
    /// come first, at the paths the directories have then. After the
    /// rename back and its syncs, where they are due, the directory is
    /// listed, and so is each directory that the creation made in it and
    /// that it lists as one: if the directory cannot be listed, nothing
    /// is removed. Each removal goes by those listings: what is changed
    /// after them is not seen.
    pub(super) fn remove(self) {
        let vfs = &*self.vfs;
        let at = match self.placed {
            true => &self.target,
            false => &self.dir,
        };
        if !matches!(self.staging.here(vfs), Ok(Some(_)))
            || !matches!(self.is_here(at), Ok(Some(_)))
        {
            return;
        }
        if self.placed {
            if vfs.rename(&self.target, &self.dir).is_err() {
                // The checkpoint stays at the target, and the staging
                // directory holds its lock file and nothing else.
                drop(self.lock);
                self.staging.leave(vfs);
                return;
            }
            if self.settle_name().is_err() {
                return;
            }
        }
        let Ok(root) = vfs.read_dir(&self.dir) else {
            return;
        };
        let named = |entries: &[DirEntry], path: &Path, kind: EntryKind| {
            let is_it =
                |entry: &DirEntry| Some(&*entry.name) == path.file_name() && entry.kind == kind;
            entries.iter().any(is_it)
        };
        let mut listings = Vec::new();
        for dir in &self.dirs {
            if named(&root, dir, EntryKind::Dir)
                && let Ok(entries) = vfs.read_dir(dir)
            {
                listings.push((dir.as_path(), entries));
            }
        }
        listings.push((self.dir.as_path(), root));
        let listed = |path: &Path, kind: EntryKind| {
            (listings.iter())
                .any(|(dir, entries)| path.parent() == Some(*dir) && named(entries, path, kind))
        };
        drop(self.written);
        for file in self.files.iter().rev() {
            if listed(file, EntryKind::File) {
                let _ = vfs.remove_file(file);
            }
        }
        for dir in self.dirs.iter().rev() {
            if listed(dir, EntryKind::Dir) {
                let _ = vfs.remove_dir(dir);
            }
        }
        let lock_path = self.dir.join(LOCK_FILE_NAME);
        if listed(&lock_path, EntryKind::File) {
            let _ = vfs.remove_file(&lock_path);
        }
        drop(self.lock);
        let _ = vfs.remove_dir(&self.dir);
        let parent = self.staging.parent.clone();
        self.staging.leave(vfs);
        let _ = vfs.sync_dir(&parent);
    }
}

/// Removes from `staging`, a staging directory whose lock the caller
/// holds and which lists the directory an earlier creation built in
/// ([`BUILDING_DIR_NAME`]), that directory with everything in it, and
/// nothing else: the staging directory is left with its lock file alone.
///
/// **The name of what is removed is made durable first.** A creation that
/// failed can have given the directory it built in the name of its target
/// and taken it back without either rename being durable
/// ([`Target::remove`]), and a power cut can keep what is removed from a
/// directory and lose a rename of it: the directory would be back at the
/// target with a part of what it held. So the directory above and the
/// staging directory are synced, which settles under which of the two
/// names the directory is, before anything in it goes. If that fails,
/// nothing is removed.
///
/// **And the staging directory is looked at once more, right before the
/// first removal**: it has to be at its path, with the lock file that is
/// held ([`CheckpointError::Replaced`]), and to list what a creation puts
/// there and nothing else ([`CheckpointError::NotStaging`]). The syncs
/// take as long as the device takes, and what was put there meanwhile is
/// not removed.
fn take_over(vfs: &dyn Vfs, staging: &Staging) -> Result<(), CheckpointError> {
    for dir in [&staging.parent, &staging.path] {
        vfs.sync_dir(dir)
            .map_err(CheckpointError::io("sync the directory", dir))?;
    }
    let Some(entries) = staging.here(vfs)? else {
        return Err(CheckpointError::Replaced {
            dir: staging.path.clone(),
        });
    };
    if staging_holds(vfs, &staging.path, &entries)? {
        let built = staging.path.join(BUILDING_DIR_NAME);
        remove_entry(vfs, &built, EntryKind::Dir)?;
    }
    Ok(())
}

/// Removes the directory `staging`, which has a name that a creation
/// gives its staging directory ([`CREATING_SUFFIX`]), where it is what a
/// creation left, and says whether there was such a directory: `false`
/// where the directory `parent` above does not list it, under that name
/// byte for byte, as a directory.
///
/// It is listed before anything is done to it, and one that lists
/// anything but what a creation puts there is refused
/// ([`staging_holds`], [`CheckpointError::NotStaging`]): nothing of it is
/// removed, no lock is taken in it and no lock file is made in it.
/// Otherwise its lock is taken in a lock file of its own
/// ([`lock_what_is_there`]), and where the lock is found held the call
/// goes no further ([`CheckpointError::Creating`]). Under the lock it is
/// looked at again ([`held_at`]), and nothing more is done unless the
/// directory whose lock was taken is found at the path
/// ([`CheckpointError::Replaced`]) and lists, then too, nothing but what
/// a creation puts there ([`CheckpointError::NotStaging`]). What was
/// built in it goes first ([`take_over`]), then its lock file, and then
/// the directory, by a removal that fails if something has come into it.
/// What these looks do not tell is in the [module documentation](super)
/// ("Durability, and a creation that fails").
fn remove_leftover(vfs: &dyn Vfs, parent: &Path, staging: &Path) -> Result<bool, CheckpointError> {
    if kind_of(vfs, parent, staging)? != Some(EntryKind::Dir) {
        return Ok(false);
    }
    let entries = vfs
        .read_dir(staging)
        .map_err(CheckpointError::io("list", staging))?;
    staging_holds(vfs, staging, &entries)?;
    let staging = Staging {
        parent: parent.to_path_buf(),
        path: staging.to_path_buf(),
        lock: lock_what_is_there(vfs, staging)?,
        made: false,
    };
    let Some(entries) = staging.here(vfs)? else {
        return Err(CheckpointError::Replaced { dir: staging.path });
    };
    if staging_holds(vfs, &staging.path, &entries)? {
        take_over(vfs, &staging)?;
    }
    let lock_path = staging.path.join(LOCK_FILE_NAME);
    vfs.remove_file(&lock_path)
        .map_err(CheckpointError::io("remove", &lock_path))?;
    drop(staging.lock);
    vfs.remove_dir(&staging.path)
        .map_err(CheckpointError::io("remove the directory", &staging.path))?;
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
/// all of this holds, unless the name is one that creations give their
/// staging directories ([`CREATING_SUFFIX`]), which the end of this text
/// is about:
///
/// - the directory above `dir` lists it, under the name given, as a
///   directory of its own, and not as a link to one
///   ([`CheckpointError::NotADirectory`]): a link is not followed into
///   what it leads to. Where it lists nothing under that name, an empty
///   directory that is there under it, and no link, goes all the same
///   (below);
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
/// runs: each of its calls to the file layer acts where its path leads
/// when that call is made, as the [module documentation](super) says of
/// everything that comes after a look ("Durability, and a creation that
/// fails").
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
/// **What a creation of a checkpoint at `dir` left beside it goes too,
/// and nothing else that lies there does**: the staging directory of
/// that creation ([`CREATING_SUFFIX`]), with what was built in it. The
/// directory above has to list it under its name, byte for byte, as a
/// directory, and it has to list what a creation puts there and nothing
/// else, before its lock is taken and again under it, as a creation
/// reads one it is to build in ([`CheckpointError::NotStaging`],
/// [`CheckpointError::NotALock`], [`CheckpointError::Replaced`]).
///
/// - Where `dir` was deleted, it goes afterwards. **A directory under
///   that name that no creation left is passed over**, with no error and
///   with nothing in it touched: a database, or anything else that
///   somebody put there, is as it was, whether it lay there before the
///   call or came while `dir` was removed. So is one whose lock is found
///   held: if a creation is building in it, a checkpoint can get the
///   name of `dir` again. Any other error of its removal is returned
///   though `dir` is gone: one that the file layer gave, and that of a
///   look under the lock which did not find the directory that was
///   locked.
/// - Where the directory above lists nothing under the name of `dir`, the
///   call finds out whether the name leads to something all the same, as
///   it can where a file system takes several spellings for one name: by
///   a removal of an empty directory, which follows no link at that
///   name. If nothing is there, the call returns `Ok` if a creation's
///   staging directory was beside it and was removed,
///   [`CheckpointError::NotADirectory`] if there was none, or none that
///   a creation left, [`CheckpointError::Creating`] for one whose lock is
///   found held, and the error of the look that refused one otherwise. An
///   empty directory that is there is removed by that, as any empty
///   directory is, and the call goes on as after a deletion. A file, a
///   link or a directory with entries there refuses the call
///   ([`CheckpointError::NotADirectory`]), and another error of that
///   removal is returned.
/// - Where `dir` is refused, it is not looked for, and stays.
/// - Given a directory of such a name as `dir`, the call removes it in
///   the same way where a creation left it, fails with
///   [`CheckpointError::NotStaging`] where it lists anything that no
///   creation puts there, with nothing removed, and fails with
///   [`CheckpointError::NotADirectory`] if there is none.
pub fn delete(vfs: &dyn Vfs, dir: &Path) -> Result<(), CheckpointError> {
    let not_a_directory = || CheckpointError::NotADirectory {
        dir: dir.to_path_buf(),
    };
    let (Some(parent), Some(name)) = (parent_of(dir), dir.file_name()) else {
        return Err(not_a_directory());
    };
    let found = |left: bool| match left {
        true => Ok(()),
        false => Err(not_a_directory()),
    };
    if is_staging(dir) {
        return found(remove_leftover(vfs, parent, dir)?);
    }
    // What lies beside `dir` and is no creation's is passed over as if
    // nothing lay there.
    let left = || match staging_of(dir).map(|staging| remove_leftover(vfs, parent, &staging)) {
        Some(Err(CheckpointError::NotStaging { .. })) | None => Ok(false),
        Some(removed) => removed,
    };
    let after_a_deletion = || match left() {
        Ok(_) | Err(CheckpointError::Creating { .. }) => Ok(()),
        Err(error) => Err(error),
    };
    match kind_of(vfs, parent, dir)? {
        Some(EntryKind::Dir) => {
            delete_database(vfs, dir, parent, true)?;
            after_a_deletion()
        }
        Some(_) => Err(not_a_directory()),
        // The removal of an empty directory follows no link at the name:
        // it says whether the name leads to something that is not listed
        // under it.
        None => match vfs.remove_dir(&parent.join(name)) {
            Err(error) if error.kind() == ErrorKind::NotFound => found(left()?),
            Ok(()) => {
                vfs.sync_dir(parent)
                    .map_err(CheckpointError::io("sync the directory", parent))?;
                after_a_deletion()
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotADirectory
                        | ErrorKind::DirectoryNotEmpty
                        | ErrorKind::AlreadyExists
                ) =>
            {
                Err(not_a_directory())
            }
            Err(error) => Err(CheckpointError::io("look for", dir)(error)),
        },
    }
}

/// What [`finish`] found at a path, and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// What was under the name it was given.
    pub at: AtPath,
    /// What was beside that name, under the name of a creation's staging
    /// directory ([`CREATING_SUFFIX`]). [`Beside::Nothing`] where the
    /// call was given a name of that kind itself.
    pub beside: Beside,
}

/// What [`finish`] found under the name it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtPath {
    /// Nothing that the directory above lists under that name.
    Nothing,
    /// A database, whole: it has a manifest of this engine under the
    /// manifest's name, and none under [`DELETING_FILE_NAME`]. Nothing of
    /// it was touched, and its lock was not taken.
    Database,
    /// What a deletion or a creation that was cut short left, which is
    /// removed now, durably: a directory with a manifest under
    /// [`DELETING_FILE_NAME`], an empty directory, a directory with an
    /// empty lock file alone, or, where the name is a staging
    /// directory's, one that a creation left.
    Removed,
}

/// What [`finish`] found beside the name it was given, under that name
/// with [`CREATING_SUFFIX`] added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beside {
    /// No directory.
    Nothing,
    /// The staging directory that a creation left, which is removed now,
    /// durably, with what was built in it.
    Removed,
    /// A staging directory whose lock is held, as by a creation that is
    /// building in it. It is as it was.
    BeingBuiltIn,
    /// A directory that no creation left: it lists something that no
    /// creation puts into its staging directory
    /// ([`CheckpointError::NotStaging`]). It is as it was.
    NotStaging,
}

/// Finishes what a deletion or a creation of a checkpoint in the
/// directory `dir` of `vfs` left when it was cut short, **and begins
/// nothing**: where `dir` is a database that no deletion has begun on, it
/// is as it was when the call returns, with no lock of it taken, so the
/// call can be made for a checkpoint that a handle has open.
///
/// It is [`delete`] without its first step. [`delete`] makes a directory
/// stop being a database, by renaming its manifest to
/// [`DELETING_FILE_NAME`], and then removes it; this call removes only
/// what that step, or a creation, has left behind already:
///
/// - **A deletion that was cut short**: a directory that holds a manifest
///   of this engine under [`DELETING_FILE_NAME`], whatever else it holds,
///   a manifest under the manifest's own name among it, as [`delete`]
///   has it. It is finished under the directory's lock, where the
///   directory is read again: one that is a database then, and no
///   deletion's, is left as it is. Where a handle has the lock,
///   [`CheckpointError::Open`].
/// - **What a deletion can leave beside that**: an empty directory, which
///   is removed by a removal that fails if something has come into it,
///   and a directory with an empty lock file alone, under its lock.
/// - **What a creation left**: the staging directory beside `dir`
///   ([`CREATING_SUFFIX`]), with what was built in it, by the rule and
///   under the looks of [`delete`]. One whose lock is held, and one that
///   no creation left, is as it was, and [`Finished::beside`] says which.
///   Given a name of that kind as `dir`, the call removes that directory
///   where a creation left it, and fails with
///   [`CheckpointError::NotStaging`] where it lists what no creation puts
///   there.
///
/// A directory that holds files and no manifest of this engine is
/// refused, with nothing touched ([`CheckpointError::NotADatabase`]), and
/// so is a name that the directory above lists as something else than a
/// directory ([`CheckpointError::NotADirectory`]). A name that it does
/// not list is [`AtPath::Nothing`]: unlike [`delete`], this call does not
/// look for a directory under another spelling of the name.
///
/// When it returns `Ok`, what it removed is removed durably. It keeps no
/// path from being given another meaning while it runs, as the [module
/// documentation](super) says of everything that comes after a look
/// ("Durability, and a creation that fails").
pub fn finish(vfs: &dyn Vfs, dir: &Path) -> Result<Finished, CheckpointError> {
    let (Some(parent), Some(_)) = (parent_of(dir), dir.file_name()) else {
        return Err(CheckpointError::NotADirectory {
            dir: dir.to_path_buf(),
        });
    };
    if is_staging(dir) {
        let at = match remove_leftover(vfs, parent, dir)? {
            true => AtPath::Removed,
            false => AtPath::Nothing,
        };
        return Ok(Finished {
            at,
            beside: Beside::Nothing,
        });
    }
    let at = match kind_of(vfs, parent, dir)? {
        Some(EntryKind::Dir) => match delete_database(vfs, dir, parent, false)? {
            true => AtPath::Removed,
            false => AtPath::Database,
        },
        Some(_) => {
            return Err(CheckpointError::NotADirectory {
                dir: dir.to_path_buf(),
            });
        }
        None => AtPath::Nothing,
    };
    let beside = match staging_of(dir).map(|staging| remove_leftover(vfs, parent, &staging)) {
        Some(Ok(true)) => Beside::Removed,
        Some(Ok(false)) | None => Beside::Nothing,
        Some(Err(CheckpointError::Creating { .. })) => Beside::BeingBuiltIn,
        Some(Err(CheckpointError::NotStaging { .. })) => Beside::NotStaging,
        Some(Err(error)) => return Err(error),
    };
    Ok(Finished { at, beside })
}

/// Deletes the database in `dir`, a directory of its own in the directory
/// `parent`, as [`delete`] says, and says whether the directory is gone.
/// Where `begin` is not set, a database that no deletion has begun on is
/// left as it is, with no lock taken, as [`finish`] says, and the answer
/// is `false`.
fn delete_database(
    vfs: &dyn Vfs,
    dir: &Path,
    parent: &Path,
    begin: bool,
) -> Result<bool, CheckpointError> {
    let entries = vfs
        .read_dir(dir)
        .map_err(CheckpointError::io("list", dir))?;
    if !entries.is_empty() {
        // Nothing is changed, the lock file included, in a directory that
        // is neither a database nor what a deletion left.
        if Left::of(vfs, dir, &entries)? == Left::Database && !begin {
            return Ok(false);
        }
        let lock_path = dir.join(LOCK_FILE_NAME);
        let lock = vfs.lock(&lock_path).map_err(|source| match source.kind() {
            ErrorKind::WouldBlock => CheckpointError::Open {
                dir: dir.to_path_buf(),
            },
            _ => CheckpointError::io("lock", &lock_path)(source),
        })?;
        let emptied = empty(vfs, dir, begin)?;
        drop(lock);
        if !emptied {
            return Ok(false);
        }
    }
    vfs.remove_dir(dir)
        .map_err(CheckpointError::io("remove the directory", dir))?;
    vfs.sync_dir(parent)
        .map_err(CheckpointError::io("sync the directory", parent))?;
    Ok(true)
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
/// holds, the lock file included, and says whether it did. Where `begin`
/// is not set and the directory is a database that no deletion has begun
/// on, nothing is removed, and the answer is `false`.
fn empty(vfs: &dyn Vfs, dir: &Path, begin: bool) -> Result<bool, CheckpointError> {
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
        Left::LockAlone => return remove(LOCK_FILE_NAME).map(|()| true),
        Left::Database if !begin => return Ok(false),
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
    remove(DELETING_FILE_NAME)?;
    Ok(true)
}
