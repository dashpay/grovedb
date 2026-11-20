//! `MANIFEST`: reading it, and replacing it so that a crash leaves the old
//! one or the new one.

use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    FileKind, PageSize,
};
use lignin_io::{Create, OpenMode, Vfs, VfsFile};

use super::{
    error::{ManifestChange, SegmentStoreError},
    files::remove_if_present,
};

/// Reads the manifest of the database directory `dir` and changes nothing.
///
/// This is how a reader that must not write looks at a database. The
/// manifest it returns is the one in force, which a crash may still undo if
/// its replacement has not finished. Use [`ManifestStore::open`] to act on a
/// manifest.
pub fn read_manifest(vfs: &dyn Vfs, dir: &Path) -> Result<Manifest, SegmentStoreError> {
    let path = dir.join(ManifestStore::FILE_NAME);
    let file = vfs
        .open(&path, OpenMode::Read)
        .map_err(|source| match source.kind() {
            ErrorKind::NotFound => SegmentStoreError::NoManifest {
                dir: dir.to_path_buf(),
            },
            _ => SegmentStoreError::io("open", &path)(source),
        })?;
    let len = file
        .len()
        .map_err(SegmentStoreError::io("measure", &path))?;
    let mut bytes = vec![0; len.min(Manifest::MAX_ENCODED_LEN as u64 + 1) as usize];
    file.read_exact_at(&mut bytes, 0)
        .map_err(SegmentStoreError::io("read", &path))?;
    Manifest::decode(&bytes).map_err(SegmentStoreError::format(&path, 0))
}

/// Whether a session has made a directory in its database directory whose
/// name no sync of the database directory has made durable yet.
///
/// [`list_or_create_dir`](super::list_or_create_dir) keeps it: the stores
/// of a database directory each have a directory in it, and each makes its
/// own if it is missing. The [`ManifestStore`] of the directory has the
/// one that all of them share ([`ManifestStore::dir_sync`]), and it starts
/// with nothing owed, because a manifest store syncs the database
/// directory when it is opened or created.
#[derive(Debug, Clone)]
pub struct DirSync {
    owed: Arc<Mutex<bool>>,
}

impl DirSync {
    /// The record of a database directory that was just synced.
    pub(super) fn new() -> Self {
        Self {
            owed: Arc::default(),
        }
    }

    /// Whether the sync is owed, for whoever lists or makes a directory:
    /// one of them at a time, so that the sync one of them makes covers
    /// every directory made before it.
    pub(super) fn owed(&self) -> MutexGuard<'_, bool> {
        self.owed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The manifest of one database directory, and the only way to replace it.
///
/// The manifest it holds is durable: a crash leaves that manifest or a
/// later one. See [the module documentation](super#replacing-the-manifest)
/// for the steps and for what a crash at each of them leaves.
///
/// The caller holds the directory's lock ([`Vfs::lock`]) for as long as this
/// lives.
#[derive(Debug)]
pub struct ManifestStore {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    manifest: Manifest,
    in_doubt: bool,
    dir_sync: DirSync,
}

impl ManifestStore {
    /// The manifest's file name inside the database directory.
    pub const FILE_NAME: &'static str = "MANIFEST";
    /// The name the next manifest is written under before it is renamed.
    pub const TEMP_FILE_NAME: &'static str = "MANIFEST.tmp";

    /// Writes `manifest` as the first manifest of the directory `dir`, which
    /// must exist and must not have one.
    ///
    /// A new database starts from the empty manifest. A checkpoint starts
    /// from a manifest that names the files linked into its directory. The
    /// page size of this manifest is the store's from then on: no
    /// replacement changes it. A checkpoint takes the page size of the
    /// database it was made from.
    pub fn create(
        vfs: Arc<dyn Vfs>,
        dir: &Path,
        manifest: Manifest,
    ) -> Result<Self, SegmentStoreError> {
        let path = dir.join(Self::FILE_NAME);
        match vfs.open(&path, OpenMode::Read) {
            Ok(_) => {
                return Err(SegmentStoreError::ManifestExists {
                    dir: dir.to_path_buf(),
                });
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(SegmentStoreError::io("open", &path)(error)),
        }
        install(&*vfs, dir, &manifest, |temporary| temporary.sync_all())
            .map_err(|(_, error)| error)?;
        Ok(Self {
            vfs,
            dir: dir.to_path_buf(),
            manifest,
            in_doubt: false,
            dir_sync: DirSync::new(),
        })
    }

    /// Opens the manifest of the directory `dir` in order to act on it.
    ///
    /// It makes the manifest it finds durable first, since an earlier session
    /// may have ended between the rename of a replacement and the sync of the
    /// directory. That sync of the directory also makes durable the name of
    /// every directory that is in it, which is why [`Self::dir_sync`] starts
    /// with nothing owed. Then it removes the temporary file an
    /// interrupted replacement can leave.
    pub fn open(vfs: Arc<dyn Vfs>, dir: &Path) -> Result<Self, SegmentStoreError> {
        let manifest = read_manifest(&*vfs, dir)?;
        vfs.sync_dir(dir)
            .map_err(SegmentStoreError::io("sync the directory", dir))?;
        remove_if_present(&*vfs, &dir.join(Self::TEMP_FILE_NAME))?;
        Ok(Self {
            vfs,
            dir: dir.to_path_buf(),
            manifest,
            in_doubt: false,
            dir_sync: DirSync::new(),
        })
    }

    /// The file layer the manifest is read and written through.
    pub fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    /// The database directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether this session has made a directory in the database directory
    /// whose name is not durable yet. It has made none when the manifest
    /// store is opened or created, which syncs the database directory.
    pub fn dir_sync(&self) -> &DirSync {
        &self.dir_sync
    }

    /// The durable manifest: the one a crash now would leave.
    ///
    /// Fails with [`SegmentStoreError::ManifestInDoubt`] after a replacement
    /// that failed once its rename had begun.
    pub fn manifest(&self) -> Result<&Manifest, SegmentStoreError> {
        if self.in_doubt {
            Err(SegmentStoreError::ManifestInDoubt {
                dir: self.dir.clone(),
            })
        } else {
            Ok(&self.manifest)
        }
    }

    /// Checks that the store's pages are `page_size` bytes, the size the
    /// caller is about to read and write pages with.
    ///
    /// A page image does not hold the page size, so whoever opens a store
    /// with a page size calls this before it reads a page:
    /// [`SegmentStoreError::PageSizeMismatch`] if the manifest records
    /// another size. A caller with no size of its own takes the store's
    /// from [`Manifest::page_size`].
    pub fn check_page_size(&self, page_size: PageSize) -> Result<(), SegmentStoreError> {
        let stored = self.manifest()?.page_size();
        if stored == page_size {
            Ok(())
        } else {
            Err(SegmentStoreError::PageSizeMismatch {
                dir: self.dir.clone(),
                stored,
                requested: page_size,
            })
        }
    }

    /// Replaces the manifest with the one `next` makes from the current
    /// manifest. When this returns `Ok`, the new manifest is durable.
    ///
    /// The new manifest is always made from the current one, here, so that a
    /// replacement cannot be built on a manifest that has since been
    /// replaced and drop a file some other part of the store has added. It
    /// is checked against the current one ([`ManifestChange`]), and a
    /// manifest equal to the current one is not written again.
    ///
    /// If `next` or a step before the rename fails, the current manifest
    /// stays in force and the replacement can be tried again. If the rename
    /// or the sync of the directory fails, it is not known which of the two a
    /// crash would leave: this and every later call fail with
    /// [`SegmentStoreError::ManifestInDoubt`] until the manifest is opened
    /// again.
    pub fn update<E>(
        &mut self,
        next: impl FnOnce(&Manifest) -> Result<Manifest, E>,
    ) -> Result<(), E>
    where
        E: From<SegmentStoreError>,
    {
        self.update_with(next, |temporary| temporary.sync_all())
    }

    /// As [`Self::update`], with the sync of the new manifest's temporary
    /// file left to `sync`, which is given that file and makes it durable
    /// as [`VfsFile::sync_all`] does. It is called once, before the
    /// rename, and not at all for a manifest equal to the current one.
    ///
    /// This is for a replacement that names a file for the first time.
    /// Such a file and its name have to be durable before a manifest that
    /// names the file is in force, which it can be from the rename on.
    /// Neither has to be durable before the temporary file is. So `sync`
    /// can make all of them durable in one set ([`Vfs::sync_together`],
    /// with [`SyncItem::All`] of the temporary file among its items), and
    /// where a sync ends in a flush of a whole device the set makes that
    /// flush once.
    ///
    /// If `sync` fails, the replacement fails as it does when the sync of
    /// the temporary file fails: the current manifest stays in force.
    ///
    /// [`SyncItem::All`]: lignin_io::SyncItem::All
    pub(crate) fn update_with<E>(
        &mut self,
        next: impl FnOnce(&Manifest) -> Result<Manifest, E>,
        sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>,
    ) -> Result<(), E>
    where
        E: From<SegmentStoreError>,
    {
        let next = next(self.manifest()?)?;
        Ok(self.replace(next, sync)?)
    }

    fn replace(
        &mut self,
        next: Manifest,
        sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>,
    ) -> Result<(), SegmentStoreError> {
        let current = self.manifest()?;
        if *current == next {
            return Ok(());
        }
        check_change(current, &next).map_err(SegmentStoreError::ManifestChange)?;
        match install(&*self.vfs, &self.dir, &next, sync) {
            Ok(()) => {
                self.manifest = next;
                Ok(())
            }
            Err((reached, error)) => {
                self.in_doubt = reached == Reached::Rename;
                Err(error)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reached {
    TemporaryFile,
    Rename,
}

fn install(
    vfs: &dyn Vfs,
    dir: &Path,
    manifest: &Manifest,
    sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>,
) -> Result<(), (Reached, SegmentStoreError)> {
    let path = dir.join(ManifestStore::FILE_NAME);
    let temporary = dir.join(ManifestStore::TEMP_FILE_NAME);
    write_temporary(vfs, &temporary, manifest, sync)
        .map_err(|error| (Reached::TemporaryFile, error))?;
    vfs.rename(&temporary, &path)
        .map_err(SegmentStoreError::io("rename", &temporary))
        .and_then(|()| {
            vfs.sync_dir(dir)
                .map_err(SegmentStoreError::io("sync the directory", dir))
        })
        .map_err(|error| (Reached::Rename, error))
}

/// Writes `manifest` to the temporary file and has `sync` make it durable
/// there.
fn write_temporary(
    vfs: &dyn Vfs,
    temporary: &Path,
    manifest: &Manifest,
    sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>,
) -> Result<(), SegmentStoreError> {
    let bytes = manifest
        .encode()
        .map_err(SegmentStoreError::format(temporary, 0))?;
    remove_if_present(vfs, temporary)?;
    let written = vfs
        .open(temporary, OpenMode::Write(Create::New))
        .map_err(SegmentStoreError::io("create", temporary))
        .and_then(|file| {
            file.write_at(&bytes, 0)
                .map_err(SegmentStoreError::io("write", temporary))?;
            sync(&*file).map_err(SegmentStoreError::io("sync", temporary))
        });
    if written.is_err() {
        let _ = vfs.remove_file(temporary);
    }
    written
}

fn check_change(current: &Manifest, next: &Manifest) -> Result<(), ManifestChange> {
    let (from, to) = (current.page_size(), next.page_size());
    if from != to {
        return Err(ManifestChange::PageSizeChanged { from, to });
    }

    let (last, next_last) = (current.last_file_ids(), next.last_file_ids());
    for kind in FileKind::ALL {
        if next_last.of(kind) < last.of(kind) {
            return Err(ManifestChange::LastFileIdLowered { kind });
        }
    }

    let order = |file: &LiveFile| (file.kind().code(), file.id());
    for file in next.files() {
        let (kind, id) = (file.kind(), file.id());
        match current.files().binary_search_by_key(&order(file), order) {
            Ok(at) => {
                let named = &current.files()[at];
                if file.valid_len() < named.valid_len() {
                    return Err(ManifestChange::FileShortened { kind, id });
                }
                if named.state() != FileState::Open && file != named {
                    return Err(ManifestChange::FinishedFileChanged { kind, id });
                }
            }
            Err(_) => {
                if Some(id) <= last.of(kind) {
                    return Err(ManifestChange::FileIdReused { kind, id });
                }
            }
        }
    }

    if let Some(table) = next.table_checkpoint() {
        let id = table.file();
        match current.table_checkpoint() {
            Some(named) if named.file() == id => {
                if named != table {
                    return Err(ManifestChange::TableCheckpointChanged { id });
                }
            }
            _ => {
                if Some(id) <= last.page_table {
                    return Err(ManifestChange::FileIdReused {
                        kind: FileKind::PageTable,
                        id,
                    });
                }
            }
        }
    }
    Ok(())
}
