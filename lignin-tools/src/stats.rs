//! `lignin stats DIR`: every count of the engine after an open.
//!
//! The database is opened and [`Db::stats`](lignindb::Db::stats) is
//! printed whole: the counts of the store (the value-log reads among
//! them), what the recovery did and how long the open took, the pool's
//! memory, the page ids, the reclaimer, the cleaner, the readers and what
//! the background work could not do. Then the files on disk, by kind.
//!
//! **The pool's memory is what it asked for, not what the process holds.**
//! A pool of a megabyte or more is a mapping that the system backs as
//! frames are first used: after an open the resident size of the process
//! is far below the pool's size, and grows towards it as pages are read.
//! The report gives the size asked for and the part of it that is such a
//! mapping side by side.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use crate::{args::Common, open::open, Outcome, ToolError};

/// The files in each of the engine's directories under a database's
/// directory: its name, and how many files of how many bytes, or `None`
/// where the directory or an entry of it could not be read.
pub(crate) type OnDisk = Vec<(&'static str, Option<(u64, u64)>)>;

/// The count and the bytes of the files in each of the engine's
/// directories under `dir`.
pub(crate) fn files_on_disk(dir: &Path) -> OnDisk {
    let kind = |name: &str| -> io::Result<(u64, u64)> {
        let (mut count, mut bytes) = (0, 0);
        for entry in fs::read_dir(dir.join(name))? {
            let meta = entry?.metadata()?;
            if meta.is_file() {
                count += 1;
                bytes += meta.len();
            }
        }
        Ok((count, bytes))
    };
    (["wal", "seg", "val", "table"].into_iter())
        .map(|name| (name, kind(name).ok()))
        .collect()
}

/// Writes `files` to `out` as one line.
pub(crate) fn write_files(files: &OnDisk, out: &mut dyn Write) -> Result<(), ToolError> {
    let parts: Vec<String> = (files.iter())
        .map(|(name, files)| match files {
            Some((count, bytes)) => format!("{name}/ {count} files {bytes} bytes"),
            None => format!("{name}/ not read"),
        })
        .collect();
    writeln!(out, "on disk: {}", parts.join(", "))?;
    Ok(())
}

/// Writes every count of the database in `common.dir` to `out`.
pub fn run(common: &Common, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let db = open(common)?;
    write!(out, "{}", db.stats())?;
    write_files(&files_on_disk(&common.dir), out)?;
    db.close()?;
    Ok(Outcome::Done)
}
