//! Which engine's database a directory holds, by the names of its entries:
//! the look that opens nothing.
//!
//! It is compiled with either engine, so that a build of one of them, such
//! as the migrator's, asks it as the engine chosen at the open does.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::ErrorKind,
    path::Path,
};

use grovedb_storage::Error;

/// Which of the two engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// The RocksDB engine, `RocksDbEngine`.
    RocksDb,
    /// The LigninDB engine, `LigninEngine`.
    Lignin,
}

impl EngineKind {
    /// The other engine.
    pub fn other(self) -> Self {
        match self {
            EngineKind::RocksDb => EngineKind::Lignin,
            EngineKind::Lignin => EngineKind::RocksDb,
        }
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EngineKind::RocksDb => "RocksDB",
            EngineKind::Lignin => "LigninDB",
        })
    }
}

/// The name LigninDB gives its manifest while a removal of its database is
/// under way (`lignindb::checkpoint::DELETING_FILE_NAME`), written out for a
/// build without LigninDB.
const DELETING_FILE_NAME: &str = "MANIFEST.deleting";

/// The engine whose database an entry of this name belongs to, if the
/// name is one that only that engine gives an entry of its database
/// directory.
pub(crate) fn engine_of_entry(name: &OsStr) -> Option<EngineKind> {
    let name = name.to_str()?;
    let rocksdb = matches!(name, "CURRENT" | "IDENTITY")
        || name.starts_with("MANIFEST-")
        || name.starts_with("OPTIONS-")
        || name.ends_with(".sst");
    let lignin =
        name == "MANIFEST" || name == DELETING_FILE_NAME || LIGNIN_DIRECTORIES.contains(&name);
    match (rocksdb, lignin) {
        (true, _) => Some(EngineKind::RocksDb),
        (false, true) => Some(EngineKind::Lignin),
        (false, false) => None,
    }
}

/// The directories LigninDB keeps its files in, inside its database
/// directory: the log, the segments, the value log and the page table.
pub(crate) const LIGNIN_DIRECTORIES: [&str; 4] = ["wal", "seg", "val", "table"];

/// What the entries of a directory say it holds: an engine, with the first
/// entry that says so.
pub(crate) fn held_among(
    dir: &Path,
    entries: impl IntoIterator<Item = OsString>,
) -> Result<Option<(EngineKind, OsString)>, Error> {
    let mut found: Option<(EngineKind, OsString)> = None;
    for entry in entries {
        let Some(kind) = engine_of_entry(&entry) else {
            continue;
        };
        let Some((first, of_first)) = &found else {
            found = Some((kind, entry));
            continue;
        };
        if kind != *first {
            return Err(Error::NotSupported(format!(
                "the directory {} holds entries of both engines (`{}` is {first}'s and `{}` is \
                 {kind}'s): it is opened as neither",
                dir.display(),
                of_first.to_string_lossy(),
                entry.to_string_lossy(),
            )));
        }
    }
    Ok(found)
}

pub(crate) fn unlisted(dir: &Path, error: std::io::Error) -> Error {
    Error::StorageError(format!(
        "the directory {} could not be listed to see which engine's database it holds: {error}",
        dir.display()
    ))
}

/// Which engine the entries of the directory `dir` say, by their names, with
/// the first entry that says it: the directory is listed on the file system
/// of the process, and no entry of it is opened, read or changed. Its names
/// are taken in their order as bytes.
///
/// - RocksDB: an entry named `CURRENT` or `IDENTITY`, or one whose name
///   starts with `MANIFEST-` or `OPTIONS-` or ends in `.sst`.
/// - LigninDB: an entry named `MANIFEST` or `MANIFEST.deleting`, or `wal`,
///   `seg`, `val` or `table`, the names of its directories.
/// - `None`: there is no directory, or it has no such entry. A lock file
///   says nothing: both engines keep one named `LOCK`.
///
/// A listing with names of both engines returns [`Error::NotSupported`],
/// whose words name the directory and one entry of each. A listing that
/// fails otherwise than for a directory that is not there returns
/// [`Error::StorageError`].
pub fn look(dir: &Path) -> Result<Option<(EngineKind, OsString)>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unlisted(dir, error)),
    };
    let mut names = entries
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| unlisted(dir, error))?;
    names.sort();
    held_among(dir, names)
}
