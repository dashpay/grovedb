//! One log file: its handle, where it begins in the log, and where its
//! items end.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        OnceLock,
    },
};

use lignin_format::{wal::LogStart, FileHeader, FileId, FormatVersion};
use lignin_io::{OpenMode, Vfs, VfsFile};

use super::{error::LogError, reader::FileEnd};
use crate::segment::FileNames;

/// The atomics are what a reader of a pending value, a sync and a manifest
/// look at, so that none of them takes the lock an append holds.
#[derive(Debug)]
pub(super) struct LogFile {
    id: FileId,
    path: PathBuf,
    version: FormatVersion,
    handle: Box<dyn VfsFile>,
    start: OnceLock<LogStart>,
    end: AtomicU64,
    finished: AtomicBool,
    uncut: AtomicBool,
    taken_back: AtomicBool,
    dead: AtomicBool,
    adopted: bool,
}

impl LogFile {
    /// A file this session created: its header, and nothing behind it.
    pub(super) fn created(id: FileId, path: PathBuf, handle: Box<dyn VfsFile>) -> Self {
        Self {
            id,
            path,
            version: FormatVersion::CURRENT,
            handle,
            start: OnceLock::new(),
            end: AtomicU64::new(FileHeader::ENCODED_LEN as u64),
            finished: AtomicBool::new(false),
            uncut: AtomicBool::new(false),
            taken_back: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            adopted: false,
        }
    }

    /// A file of an earlier session, as a reader of the log found it. It is
    /// read and synced, and never written.
    pub(super) fn adopted(
        vfs: &dyn Vfs,
        db: &Path,
        read: &FileEnd,
        version: FormatVersion,
    ) -> Result<Self, LogError> {
        let path = FileNames::WAL.path(db, read.file());
        let handle = vfs
            .open(&path, OpenMode::Read)
            .map_err(LogError::io("open", &path))?;
        let len = handle.len().map_err(LogError::io("measure", &path))?;
        if len != read.file_len() {
            return Err(LogError::WrongLogEnd { file: read.file() });
        }
        Ok(Self {
            id: read.file(),
            path,
            version,
            handle,
            start: OnceLock::from(read.start()),
            end: AtomicU64::new(read.valid_len()),
            finished: AtomicBool::new(true),
            uncut: AtomicBool::new(false),
            taken_back: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            adopted: true,
        })
    }

    pub(super) fn id(&self) -> FileId {
        self.id
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn version(&self) -> FormatVersion {
        self.version
    }

    /// Whether an earlier session wrote the file.
    pub(super) fn is_adopted(&self) -> bool {
        self.adopted
    }

    /// The file's start record, once it has one.
    pub(super) fn start(&self) -> Option<LogStart> {
        self.start.get().copied()
    }

    /// Gives the file its start record: from here on it is part of the log.
    pub(super) fn begin(&self, start: LogStart, end: u64) {
        let _ = self.start.set(start);
        self.end.store(end, Ordering::Release);
    }

    /// Where the file's items end: the bytes of it that are part of the log.
    pub(super) fn end(&self) -> u64 {
        self.end.load(Ordering::Acquire)
    }

    pub(super) fn appended(&self, end: u64) {
        self.end.store(end, Ordering::Release);
    }

    /// Whether the file takes no more items.
    pub(super) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub(super) fn finish(&self) {
        self.finished.store(true, Ordering::Release);
    }

    /// Whether an append to the file failed, and no durable manifest names
    /// the file closed where its last whole item ends.
    pub(super) fn is_uncut(&self) -> bool {
        self.uncut.load(Ordering::Acquire)
    }

    /// Whether an append to the file failed and left it with the length it
    /// had, while the file as it is now is not known to be durable: a crash
    /// could still bring back what the append wrote and took back.
    pub(super) fn is_taken_back(&self) -> bool {
        self.taken_back.load(Ordering::Acquire)
    }

    /// An append to the file failed. `taken_back` says that it left the
    /// file with the length it had.
    pub(super) fn failed(&self, taken_back: bool) {
        self.uncut.store(true, Ordering::Release);
        if taken_back {
            self.taken_back.store(true, Ordering::Release);
        }
    }

    /// The file is durable as it is: nothing that was taken back returns.
    pub(super) fn made_durable(&self) {
        self.taken_back.store(false, Ordering::Release);
    }

    /// A durable manifest names the file closed.
    pub(super) fn cut(&self) {
        self.taken_back.store(false, Ordering::Release);
        self.uncut.store(false, Ordering::Release);
    }

    /// Whether the file holds nothing and never will.
    pub(super) fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    pub(super) fn kill(&self) {
        self.dead.store(true, Ordering::Release);
    }

    pub(super) fn append(&self, bytes: &[u8]) -> io::Result<u64> {
        self.handle.append(bytes)
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        self.handle.sync_data()
    }

    pub(super) fn len(&self) -> io::Result<u64> {
        self.handle.len()
    }

    pub(super) fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.handle.read_exact_at(buf, offset)
    }
}
