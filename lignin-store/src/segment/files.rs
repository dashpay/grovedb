//! The names of a database directory's numbered files, and how one is
//! created.

use std::{
    ffi::OsStr,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use lignin_format::{FileHeader, FileId, FileKind, FormatVersion};
use lignin_io::{Create, DirEntry, OpenMode, Vfs, VfsFile};

use super::error::SegmentStoreError;

/// Where the files of one numbered kind live and what they are called.
///
/// A file is `<directory>/<id>.<extension>` inside the database directory.
/// The id is in decimal, padded with zeros to eight digits; a larger id is
/// written in full. No other spelling names the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileNames {
    kind: FileKind,
    directory: &'static str,
    extension: &'static str,
}

impl FileNames {
    /// Log files, `wal/<n>.log`.
    pub const WAL: Self = Self::new(FileKind::Wal, "wal", "log");
    /// Segments, `seg/<n>.seg`.
    pub const SEGMENT: Self = Self::new(FileKind::Segment, "seg", "seg");
    /// Value-log files, `val/<n>.val`.
    pub const VALUE_LOG: Self = Self::new(FileKind::ValueLog, "val", "val");
    /// Page-table checkpoints, `table/<n>.pt`.
    pub const PAGE_TABLE: Self = Self::new(FileKind::PageTable, "table", "pt");

    const MIN_DIGITS: usize = 8;

    const fn new(kind: FileKind, directory: &'static str, extension: &'static str) -> Self {
        Self {
            kind,
            directory,
            extension,
        }
    }

    /// The kind of the files.
    pub const fn kind(&self) -> FileKind {
        self.kind
    }

    /// The directory that holds the files, inside the database directory
    /// `db`.
    pub fn dir(&self, db: &Path) -> PathBuf {
        db.join(self.directory)
    }

    /// The name of file `id` inside its directory.
    pub fn file_name(&self, id: FileId) -> String {
        format!(
            "{:0digits$}.{}",
            id.get(),
            self.extension,
            digits = Self::MIN_DIGITS
        )
    }

    /// The path of file `id` of the database directory `db`.
    pub fn path(&self, db: &Path, id: FileId) -> PathBuf {
        self.dir(db).join(self.file_name(id))
    }

    /// The id of the file called `name`, or `None` if `name` is not exactly
    /// what [`Self::file_name`] gives for some id.
    pub fn parse(&self, name: &OsStr) -> Option<FileId> {
        let digits = name
            .to_str()?
            .strip_suffix(self.extension)?
            .strip_suffix('.')?;
        let padded = digits.len() == Self::MIN_DIGITS;
        let in_full = digits.len() > Self::MIN_DIGITS && !digits.starts_with('0');
        if !(padded || in_full) || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
            return None;
        }
        FileId::new(digits.parse().ok()?)
    }
}

/// Lists the directory that holds the files of the kind `names` in the
/// database directory `db`, and makes the directory's own name durable. The
/// directory is created if it is missing.
///
/// The database directory is synced whether or not this call created the
/// directory. An earlier call may have created it and failed before the
/// sync, and a manifest must not name a file in a directory that a crash can
/// take away with everything in it.
pub fn list_or_create_dir(
    vfs: &dyn Vfs,
    db: &Path,
    names: &FileNames,
) -> Result<Vec<DirEntry>, SegmentStoreError> {
    let dir = names.dir(db);
    let entries = match vfs.read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            vfs.create_dir(&dir)
                .map_err(SegmentStoreError::io("create the directory", &dir))?;
            Vec::new()
        }
        Err(error) => return Err(SegmentStoreError::io("list", &dir)(error)),
    };
    vfs.sync_dir(db)
        .map_err(SegmentStoreError::io("sync the directory", db))?;
    Ok(entries)
}

/// Creates file `id` of the kind `names` in the database directory `db`,
/// with its file header, and returns it open for appending.
///
/// When this returns, the header and the file's name are durable, so a
/// manifest may name the file with a valid length of
/// [`FileHeader::ENCODED_LEN`]. The directory of the kind must exist and
/// have a durable name ([`list_or_create_dir`]). If any step fails the file
/// is removed again.
///
/// A manifest written while this runs must not record `id` as used: see
/// [the module documentation](super#files-of-the-other-kinds).
pub fn create_file(
    vfs: &dyn Vfs,
    db: &Path,
    names: &FileNames,
    id: FileId,
) -> Result<Box<dyn VfsFile>, SegmentStoreError> {
    let path = names.path(db, id);
    let file = vfs
        .open(&path, OpenMode::Append(Create::New))
        .map_err(SegmentStoreError::io("create", &path))?;
    match write_header(vfs, &names.dir(db), &path, &*file, names.kind(), id) {
        Ok(()) => Ok(file),
        Err(error) => {
            drop(file);
            let _ = vfs.remove_file(&path);
            Err(error)
        }
    }
}

fn write_header(
    vfs: &dyn Vfs,
    dir: &Path,
    path: &Path,
    file: &dyn VfsFile,
    kind: FileKind,
    id: FileId,
) -> Result<(), SegmentStoreError> {
    let header = FileHeader::new(kind, FormatVersion::CURRENT, id.get())
        .map_err(SegmentStoreError::format(path, 0))?;
    file.append(&header.encode())
        .map_err(SegmentStoreError::io("append to", path))?;
    file.sync_data()
        .map_err(SegmentStoreError::io("sync", path))?;
    vfs.sync_dir(dir)
        .map_err(SegmentStoreError::io("sync the directory", dir))
}

pub(super) fn remove_if_present(vfs: &dyn Vfs, path: &Path) -> Result<(), SegmentStoreError> {
    match vfs.remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(SegmentStoreError::io("remove", path)(error))
        }
        _ => Ok(()),
    }
}
