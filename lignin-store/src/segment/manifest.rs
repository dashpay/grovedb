//! `MANIFEST`: reading it, and replacing it so that a crash leaves the old
//! one or the new one.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest, MAX_LIVE_FILES},
    FileHeader, FileKind, RecordHeader,
};
use lignin_io::{Create, OpenMode, Vfs};

use super::{
    error::{ManifestChange, SegmentStoreError},
    files::remove_if_present,
};

const BODY_FIXED_LEN: usize = 36;
const BODY_ENTRY_LEN: usize = 16;

pub(super) const MAX_FILE_LEN: usize = FileHeader::ENCODED_LEN
    + RecordHeader::ENCODED_LEN
    + BODY_FIXED_LEN
    + BODY_ENTRY_LEN * MAX_LIVE_FILES;

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
    let mut bytes = vec![0; len.min(MAX_FILE_LEN as u64 + 1) as usize];
    file.read_exact_at(&mut bytes, 0)
        .map_err(SegmentStoreError::io("read", &path))?;
    Manifest::decode(&bytes).map_err(SegmentStoreError::format(&path, 0))
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
    /// from a manifest that names the files linked into its directory.
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
        install(&*vfs, dir, &manifest).map_err(|(_, error)| error)?;
        Ok(Self {
            vfs,
            dir: dir.to_path_buf(),
            manifest,
            in_doubt: false,
        })
    }

    /// Opens the manifest of the directory `dir` in order to act on it.
    ///
    /// It makes the manifest it finds durable first, since an earlier session
    /// may have ended between the rename of a replacement and the sync of the
    /// directory. Then it removes the temporary file an interrupted
    /// replacement can leave.
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
        let next = next(self.manifest()?)?;
        Ok(self.replace(next)?)
    }

    fn replace(&mut self, next: Manifest) -> Result<(), SegmentStoreError> {
        let current = self.manifest()?;
        if *current == next {
            return Ok(());
        }
        check_change(current, &next).map_err(SegmentStoreError::ManifestChange)?;
        match install(&*self.vfs, &self.dir, &next) {
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
) -> Result<(), (Reached, SegmentStoreError)> {
    let path = dir.join(ManifestStore::FILE_NAME);
    let temporary = dir.join(ManifestStore::TEMP_FILE_NAME);
    write_temporary(vfs, &temporary, manifest).map_err(|error| (Reached::TemporaryFile, error))?;
    vfs.rename(&temporary, &path)
        .map_err(SegmentStoreError::io("rename", &temporary))
        .and_then(|()| {
            vfs.sync_dir(dir)
                .map_err(SegmentStoreError::io("sync the directory", dir))
        })
        .map_err(|error| (Reached::Rename, error))
}

fn write_temporary(
    vfs: &dyn Vfs,
    temporary: &Path,
    manifest: &Manifest,
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
            file.sync_all()
                .map_err(SegmentStoreError::io("sync", temporary))
        });
    if written.is_err() {
        let _ = vfs.remove_file(temporary);
    }
    written
}

fn check_change(current: &Manifest, next: &Manifest) -> Result<(), ManifestChange> {
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
