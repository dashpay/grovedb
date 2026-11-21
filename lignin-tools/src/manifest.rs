//! `lignin manifest DIR`: what the manifest names.
//!
//! It reads `MANIFEST` and measures each file the manifest names. It opens
//! nothing as a database, takes no lock and writes nothing, so it reads a
//! checkpoint before it is opened, and a database whose node is running
//! (the manifest it reads is the last one that was installed).

use std::{fs, io::Write, path::Path};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    FileKind,
};
use lignin_io::StdVfs;
use lignin_store::segment::{read_manifest, FileNames};

use crate::{open::refuse_parts, Outcome, ToolError};

/// The names of the files of `kind`.
pub(crate) fn names(kind: FileKind) -> FileNames {
    match kind {
        FileKind::Wal => FileNames::WAL,
        FileKind::Segment => FileNames::SEGMENT,
        FileKind::ValueLog => FileNames::VALUE_LOG,
        _ => FileNames::PAGE_TABLE,
    }
}

/// How the manifest says a file ends.
pub(crate) const fn state(state: FileState) -> &'static str {
    match state {
        FileState::Open => "open",
        FileState::Closed => "closed",
        FileState::Sealed => "sealed",
    }
}

/// Reads the manifest of `dir`, refusing a directory that is no database.
pub(crate) fn read(dir: &Path) -> Result<Manifest, ToolError> {
    refuse_parts(dir)?;
    Ok(read_manifest(&StdVfs, dir)?)
}

/// The path of `file` in `dir`, and the length the file has on disk, or
/// `None` where it is not there.
pub(crate) fn measured(dir: &Path, file: &LiveFile) -> (std::path::PathBuf, Option<u64>) {
    let path = names(file.kind()).path(dir, file.id());
    let len = fs::metadata(&path).ok().map(|meta| meta.len());
    (path, len)
}

/// Writes what the manifest of `dir` names to `out`.
pub fn run(dir: &Path, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let manifest = read(dir)?;
    writeln!(out, "format version {:?}", manifest.format_version())?;
    writeln!(out, "page size {} bytes", manifest.page_size().get())?;
    let last = manifest.last_file_ids();
    let id = |id: Option<lignin_format::FileId>| id.map_or("none".to_owned(), |id| id.to_string());
    writeln!(
        out,
        "last file ids: log {}, segment {}, value log {}, page table {}",
        id(last.wal),
        id(last.segment),
        id(last.value_log),
        id(last.page_table)
    )?;
    let mut problems = 0;
    match manifest.table_checkpoint() {
        Some(table) => {
            let path = FileNames::PAGE_TABLE.path(dir, table.file());
            let len = fs::metadata(&path).ok().map(|meta| meta.len());
            problems += usize::from(len.is_none_or(|len| len < table.valid_len()));
            writeln!(
                out,
                "table checkpoint: {}, {} bytes valid{}, covers version {}",
                relative(dir, &path),
                table.valid_len(),
                on_disk(table.valid_len(), len),
                table.covers()
            )?;
        }
        None => writeln!(out, "table checkpoint: none")?,
    }
    let files = manifest.files();
    writeln!(out, "live files: {}", files.len())?;
    for file in files {
        let (path, len) = measured(dir, file);
        problems += usize::from(len.is_none_or(|len| len < file.valid_len()));
        writeln!(
            out,
            "  {} {} {} bytes valid{}",
            relative(dir, &path),
            state(file.state()),
            file.valid_len(),
            on_disk(file.valid_len(), len)
        )?;
    }
    if problems > 0 {
        writeln!(
            out,
            "{problems} files are missing or shorter than the manifest vouches for: the database \
             does not open"
        )?;
    }
    Ok(Outcome::Done)
}

/// `path` from `dir` on, as the manifest names it.
pub(crate) fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir).unwrap_or(path).display().to_string()
}

fn on_disk(valid: u64, len: Option<u64>) -> String {
    match len {
        None => ", MISSING".to_owned(),
        Some(len) if len < valid => format!(", SHORT: {len} bytes on disk"),
        Some(len) if len > valid => format!(", {len} on disk"),
        Some(_) => String::new(),
    }
}
