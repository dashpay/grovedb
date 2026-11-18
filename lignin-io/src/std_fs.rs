//! The [`Vfs`] backend over the operating system's files.
//!
//! It is compiled for Linux and macOS only, the platforms LigninDB supports.
//! It is written against `std` alone, so it holds no `unsafe` code and no
//! code of its own that differs between the two: `std` picks the system
//! call. What runs on each:
//!
//! | Operation | Linux | macOS |
//! | --- | --- | --- |
//! | [`VfsFile::sync_all`] | `fsync` | `fcntl(F_FULLFSYNC)` |
//! | [`VfsFile::sync_data`] | `fdatasync` | `fcntl(F_FULLFSYNC)` |
//! | [`Vfs::sync_dir`] | `fsync` on the directory, opened read-only | `fcntl(F_FULLFSYNC)` on the directory, opened read-only |
//! | [`Vfs::lock`] | `flock(LOCK_EX \| LOCK_NB)` | the same |
//! | [`Vfs::hard_link`] | `linkat` | the same |
//! | [`Vfs::rename`] | `rename` | the same |
//! | [`VfsFile::read_at`], [`VfsFile::write_at`] | `pread64`, `pwrite64` | `pread`, `pwrite` |
//! | [`VfsFile::append`] | `write` on an `O_APPEND` descriptor | the same |
//! | [`VfsFile::set_len`] | `ftruncate` | the same |
//!
//! On macOS `fsync` hands the bytes to the drive without asking it to write
//! them out, and there is no `fdatasync`, so both syncs are the slower
//! `F_FULLFSYNC`.
//!
//! An append is a single `write` unless the system writes only part of the
//! buffer, and it never overwrites: the descriptor is `O_APPEND`, so the
//! system places the bytes at the end of the file whatever this backend
//! believes the length to be.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, ErrorKind, Write},
    os::unix::fs::{FileExt, MetadataExt},
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::vfs::{Create, DirEntry, EntryKind, OpenMode, Vfs, VfsFile, VfsLock};

/// The [`Vfs`] over the operating system's files.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdVfs;

impl StdVfs {
    fn open_file(path: &Path, mode: OpenMode) -> io::Result<StdFile> {
        let mut options = OpenOptions::new();
        options.read(true);
        let create = match mode {
            OpenMode::Read => Create::Never,
            OpenMode::Write(create) => {
                options.write(true);
                create
            }
            OpenMode::Append(create) => {
                options.append(true);
                create
            }
        };
        match create {
            Create::Never => {}
            Create::IfMissing => {
                options.create(true);
            }
            Create::New => {
                options.create_new(true);
            }
        }
        let file = options.open(path)?;
        let access = match mode {
            OpenMode::Read => Access::Read,
            OpenMode::Write(_) => Access::Write,
            OpenMode::Append(_) => Access::Append(Mutex::new(AppendEnd {
                len: file.metadata()?.len(),
                broken: None,
            })),
        };
        Ok(StdFile {
            file,
            access,
            sync_failure: Mutex::new(None),
        })
    }
}

impl Vfs for StdVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        Ok(Box::new(Self::open_file(path, mode)?))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        fs::hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                // Removed since it was listed: it is not in the directory.
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let kind = if file_type.is_file() {
                EntryKind::File
            } else if file_type.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::Other
            };
            entries.push(DirEntry {
                name: entry.file_name(),
                kind,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let dir = File::open(path)?;
        if !dir.metadata()?.is_dir() {
            return Err(io::Error::new(
                ErrorKind::NotADirectory,
                format!("{} is not a directory", path.display()),
            ));
        }
        dir.sync_all()
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    ErrorKind::WouldBlock,
                    format!("{} is locked", path.display()),
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
        let lock = StdLock { file };
        if !lock.is_named(path)? {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                format!(
                    "{} was removed or replaced while it was being locked",
                    path.display()
                ),
            ));
        }
        Ok(Box::new(lock))
    }
}

#[derive(Debug)]
struct StdFile {
    file: File,
    access: Access,
    sync_failure: Mutex<Option<StickyError>>,
}

#[derive(Debug)]
enum Access {
    Read,
    Write,
    Append(Mutex<AppendEnd>),
}

#[derive(Debug)]
struct AppendEnd {
    len: u64,
    broken: Option<StickyError>,
}

#[derive(Debug)]
struct StickyError {
    kind: ErrorKind,
    message: String,
}

impl StickyError {
    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}

type WriteFn<'a> = &'a mut dyn FnMut(&File, &[u8]) -> io::Result<usize>;

impl StdFile {
    fn sync_using(&self, sync: fn(&File) -> io::Result<()>) -> io::Result<()> {
        let mut failure = lock(&self.sync_failure);
        if let Some(failure) = &*failure {
            return Err(failure.error());
        }
        sync(&self.file).inspect_err(|error| {
            *failure = Some(StickyError {
                kind: error.kind(),
                message: format!(
                    "an earlier sync of this file failed ({error}), so this handle can no longer \
                     make it durable"
                ),
            });
        })
    }

    fn append_using(&self, buf: &[u8], write: WriteFn<'_>) -> io::Result<u64> {
        let Access::Append(end) = &self.access else {
            return Err(wrong_mode(
                "append needs a file opened with OpenMode::Append",
            ));
        };
        let mut end = lock(end);
        if let Some(broken) = &end.broken {
            return Err(broken.error());
        }
        let start = end.len;
        let mut written = 0;
        while written < buf.len() {
            let error = match write(&self.file, &buf[written..]) {
                Ok(0) => ErrorKind::WriteZero.into(),
                Ok(count) => {
                    written += count;
                    continue;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => error,
            };
            if let Err(undo) = self.undo_append(start, written) {
                end.broken = Some(StickyError {
                    kind: ErrorKind::Other,
                    message: format!(
                        "an earlier append to this file failed ({error}) and could not be undone \
                         ({undo}), so this handle no longer knows where the file ends"
                    ),
                });
            }
            return Err(error);
        }
        end.len = start + buf.len() as u64;
        Ok(start)
    }

    fn undo_append(&self, start: u64, written: usize) -> io::Result<()> {
        let torn_end = start + written as u64;
        let len = self.file.metadata()?.len();
        if len != torn_end {
            return Err(io::Error::other(format!(
                "the file is {len} bytes long where {torn_end} were expected, so something else \
                 changed it"
            )));
        }
        if written > 0 {
            self.file.set_len(start)?;
        }
        Ok(())
    }
}

impl VfsFile for StdFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let mut filled = 0;
        while filled < buf.len() {
            match self
                .file
                .read_at(&mut buf[filled..], offset + filled as u64)
            {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(filled)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        match self.access {
            Access::Write => self.file.write_all_at(buf, offset),
            _ => Err(wrong_mode(
                "write_at needs a file opened with OpenMode::Write",
            )),
        }
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.append_using(buf, &mut |mut file, bytes| file.write(bytes))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync_using(File::sync_data)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync_using(File::sync_all)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        match &self.access {
            Access::Read => Err(wrong_mode(
                "set_len needs a file opened with OpenMode::Write or OpenMode::Append",
            )),
            Access::Write => self.file.set_len(len),
            Access::Append(end) => {
                let mut end = lock(end);
                self.file.set_len(len)?;
                end.len = len;
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct StdLock {
    file: File,
}

impl StdLock {
    fn is_named(&self, path: &Path) -> io::Result<bool> {
        let held = self.file.metadata()?;
        match fs::metadata(path) {
            Ok(named) => Ok(named.dev() == held.dev() && named.ino() == held.ino()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl VfsLock for StdLock {}

impl Drop for StdLock {
    fn drop(&mut self) {
        // Closing the file releases the lock only once no copy of the
        // descriptor is left, and a child process forked on another thread
        // holds one until it calls exec. Unlocking releases it now.
        let _ = self.file.unlock();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wrong_mode(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
