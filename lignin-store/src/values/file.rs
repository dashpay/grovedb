//! One value-log file: its bytes, and where it is in its life.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex, MutexGuard, PoisonError,
    },
};

use lignin_format::{
    manifest::{FileState, LiveFile},
    value_log::{Value, ValueLogError, ValuePointer, FIRST_RECORD_OFFSET},
    FileHeader, FileId, FileKind, FormatError, FormatVersion, PageId,
};
use lignin_io::{OpenMode, Vfs, VfsFile};

use super::error::ValueStoreError;
use crate::segment::FileNames;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Created,
    Open,
    Closed,
}

#[derive(Debug)]
struct Life {
    phase: Phase,
    durable_len: u64,
    failed: bool,
    has_writer: bool,
}

/// The atomics are what a reader, an appender and the store's own lock
/// holders look at, so that none of them waits behind `life`.
#[derive(Debug)]
pub(super) struct ValueFile {
    id: FileId,
    path: PathBuf,
    version: FormatVersion,
    file: Box<dyn VfsFile>,
    end: AtomicU64,
    appendable: AtomicBool,
    closed: AtomicBool,
    holds: AtomicUsize,
    covered: AtomicU64,
    syncing: Mutex<()>,
    life: Mutex<Life>,
}

impl ValueFile {
    pub(super) fn adopt(
        vfs: &dyn Vfs,
        db: &Path,
        named: &LiveFile,
        manifest_version: FormatVersion,
    ) -> Result<Self, ValueStoreError> {
        let (id, valid_len) = (named.id(), named.valid_len());
        let path = FileNames::VALUE_LOG.path(db, id);
        let file = vfs
            .open(&path, OpenMode::Read)
            .map_err(ValueStoreError::io("open", &path))?;
        let len = file.len().map_err(ValueStoreError::io("measure", &path))?;
        if len < valid_len {
            return Err(ValueStoreError::FileTooShort {
                path,
                valid_len,
                len,
            });
        }
        let mut header = [0; FileHeader::ENCODED_LEN];
        file.read_exact_at(&mut header, 0)
            .map_err(ValueStoreError::io("read", &path))?;
        let header = FileHeader::decode(&header).map_err(ValueStoreError::format(&path, 0))?;
        if header.kind() != FileKind::ValueLog {
            let found = header.kind();
            return Err(ValueStoreError::format(&path, 0)(FormatError::ValueLog(
                ValueLogError::WrongFileKind { found },
            )));
        }
        if header.sequence() != id.get() {
            return Err(ValueStoreError::WrongFileId {
                path,
                found: header.sequence(),
            });
        }
        let version = header.version();
        if version > manifest_version {
            return Err(ValueStoreError::FileNewerThanManifest {
                path,
                file: version,
                manifest: manifest_version,
            });
        }
        let adopted = Self::new(id, path, version, file, Phase::Closed, valid_len, false);
        adopted.cover(valid_len);
        Ok(adopted)
    }

    pub(super) fn created(id: FileId, path: PathBuf, file: Box<dyn VfsFile>) -> Self {
        Self::new(
            id,
            path,
            FormatVersion::CURRENT,
            file,
            Phase::Created,
            FIRST_RECORD_OFFSET,
            true,
        )
    }

    fn new(
        id: FileId,
        path: PathBuf,
        version: FormatVersion,
        file: Box<dyn VfsFile>,
        phase: Phase,
        len: u64,
        has_writer: bool,
    ) -> Self {
        Self {
            id,
            path,
            version,
            file,
            end: AtomicU64::new(len),
            appendable: AtomicBool::new(false),
            closed: AtomicBool::new(phase == Phase::Closed),
            holds: AtomicUsize::new(0),
            covered: AtomicU64::new(0),
            syncing: Mutex::new(()),
            life: Mutex::new(Life {
                phase,
                durable_len: len,
                failed: false,
                has_writer,
            }),
        }
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

    pub(super) fn end(&self) -> u64 {
        self.end.load(Ordering::Acquire)
    }

    fn life(&self) -> MutexGuard<'_, Life> {
        self.life.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn format_error(
        &self,
        offset: u64,
    ) -> impl FnOnce(FormatError) -> ValueStoreError + use<'_> {
        ValueStoreError::format(&self.path, offset)
    }

    pub(super) fn read_exact(&self, buf: &mut [u8], offset: u64) -> Result<(), ValueStoreError> {
        self.file
            .read_exact_at(buf, offset)
            .map_err(ValueStoreError::io("read", &self.path))
    }

    fn ends_within(&self, pointer: ValuePointer, len: u64) -> bool {
        pointer
            .offset()
            .checked_add(pointer.extent_len(self.version))
            .is_some_and(|value_end| value_end <= len)
    }

    pub(super) fn contains(&self, pointer: ValuePointer) -> bool {
        self.ends_within(pointer, self.end())
    }

    pub(super) fn cover(&self, valid_len: u64) {
        self.covered.fetch_max(valid_len, Ordering::AcqRel);
    }

    pub(super) fn covers(&self, pointer: ValuePointer) -> bool {
        self.ends_within(pointer, self.covered.load(Ordering::Acquire))
    }

    pub(super) fn read<'a>(
        &self,
        pointer: ValuePointer,
        owner: PageId,
        buf: &'a mut Vec<u8>,
    ) -> Result<Value<'a>, ValueStoreError> {
        if !self.contains(pointer) {
            return Err(ValueStoreError::OutOfBounds {
                pointer,
                end: self.end(),
            });
        }
        let extent = pointer.extent_len(self.version);
        let len = usize::try_from(extent).map_err(|_| ValueStoreError::ValueTooLong { extent })?;
        buf.clear();
        buf.resize(len, 0);
        self.read_exact(buf, pointer.offset())?;
        pointer
            .read(buf, owner, self.version)
            .map_err(self.format_error(pointer.offset()))
    }

    pub(super) fn live_file(&self) -> Result<LiveFile, ValueStoreError> {
        let life = self.life();
        let state = match life.phase {
            Phase::Created | Phase::Open => FileState::Open,
            Phase::Closed => FileState::Closed,
        };
        LiveFile::new(FileKind::ValueLog, self.id, state, life.durable_len)
            .map_err(self.format_error(0))
    }

    pub(super) fn name(&self) {
        let mut life = self.life();
        if life.phase == Phase::Created {
            life.phase = Phase::Open;
            self.appendable.store(!life.failed, Ordering::Release);
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(super) fn hold(&self) {
        self.holds.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn release_hold(&self) {
        self.holds.fetch_sub(1, Ordering::AcqRel);
    }

    /// Whether the file is closed and no hold is on it, now and from now on.
    ///
    /// Whether it is closed is asked first. A writer takes a hold before it
    /// appends and closes the file after its last value, so a file that is
    /// closed and then found without a hold has none coming.
    pub(super) fn is_closed_and_free(&self) -> bool {
        let closed = self.is_closed();
        #[cfg(test)]
        between_checks::run();
        closed && self.holds.load(Ordering::Acquire) == 0
    }

    pub(super) fn is_held(&self) -> bool {
        self.holds.load(Ordering::Acquire) != 0
    }

    pub(super) fn check_appendable(&self) -> Result<(), ValueStoreError> {
        if self.appendable.load(Ordering::Acquire) {
            return Ok(());
        }
        let file = self.id;
        let life = self.life();
        match life.phase {
            _ if life.failed => Err(ValueStoreError::FileFailed { file }),
            Phase::Open if life.has_writer => Ok(()),
            Phase::Created => Err(ValueStoreError::NotNamed { file }),
            Phase::Open | Phase::Closed => Err(ValueStoreError::NotOpen { file }),
        }
    }

    pub(super) fn appended(&self, end: u64) {
        self.end.store(end, Ordering::Release);
    }

    fn settle(&self, life: &mut Life) {
        if life.has_writer || life.phase == Phase::Closed {
            return;
        }
        if life.failed {
            self.end.store(life.durable_len, Ordering::Release);
        }
        if life.durable_len == self.end() {
            life.phase = Phase::Closed;
            self.appendable.store(false, Ordering::Release);
            self.closed.store(true, Ordering::Release);
        }
    }

    /// The lock that puts the appends to the file and its syncs in one
    /// order. One sync runs at a time under it, and an append is admitted
    /// and handed to the file layer under it
    /// ([`ValueWriter::append`](super::ValueWriter::append)). So an
    /// append that the file admitted is in the file before a sync that
    /// fails begins, and none is admitted after that sync: a file of
    /// which a sync failed is not appended to again.
    pub(super) fn in_order(&self) -> MutexGuard<'_, ()> {
        #[cfg(test)]
        if matches!(
            self.syncing.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ) {
            hooks::run(hooks::Point::Waits);
        }
        self.syncing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Syncs the file through the handle the store reads it by. One sync
    /// runs at a time, and `life` is not held while it does, so that
    /// ending a writer waits for no sync.
    pub(super) fn sync(&self) -> Result<u64, ValueStoreError> {
        self.sync_through(&*self.file)
    }

    /// Syncs the file through `handle`, which is a handle on this file:
    /// what [`Self::sync`] does, for a writer that syncs through the
    /// handle it appended by.
    ///
    /// A file that is closed, or of which a sync has failed, is not
    /// synced again, through whichever handle: after a sync that failed,
    /// a later one that succeeded would say nothing of the bytes the
    /// failed one was for.
    pub(super) fn sync_through(&self, handle: &dyn VfsFile) -> Result<u64, ValueStoreError> {
        let _one_at_a_time = self.in_order();
        let end = {
            let mut life = self.life();
            if life.phase == Phase::Closed {
                return Ok(life.durable_len);
            }
            if life.failed {
                return Err(ValueStoreError::FileFailed { file: self.id });
            }
            let end = self.end();
            if end == life.durable_len {
                self.settle(&mut life);
                return Ok(life.durable_len);
            }
            end
        };
        let synced = handle.sync_data();
        let mut life = self.life();
        let result = match synced {
            Ok(()) => {
                life.durable_len = end;
                Ok(end)
            }
            Err(error) => {
                life.failed = true;
                self.appendable.store(false, Ordering::Release);
                Err(ValueStoreError::io("sync", &self.path)(error))
            }
        };
        self.settle(&mut life);
        result
    }

    pub(super) fn release_writer(&self) {
        let mut life = self.life();
        life.has_writer = false;
        self.appendable.store(false, Ordering::Release);
        self.settle(&mut life);
    }
}

/// What a test has happen on the thread that comes to a point of an
/// append or of a sync.
#[cfg(test)]
pub(super) mod hooks {
    use std::cell::RefCell;

    /// A point that a thread comes to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(in crate::values) enum Point {
        /// An append was admitted, and is not handed to the file layer
        /// yet.
        Admitted,
        /// Another thread has the lock that orders the appends and the
        /// syncs of the file, and this one is about to wait for it.
        Waits,
    }

    type Then = Box<dyn FnOnce()>;

    thread_local! {
        static NEXT: RefCell<Vec<(Point, Then)>> = const { RefCell::new(Vec::new()) };
    }

    pub(in crate::values) fn set(point: Point, then: impl FnOnce() + 'static) {
        NEXT.with(|next| next.borrow_mut().push((point, Box::new(then))));
    }

    pub(in crate::values) fn run(point: Point) {
        let then = NEXT.with(|next| {
            let mut next = next.borrow_mut();
            let at = next.iter().position(|(set, _)| *set == point)?;
            Some(next.remove(at).1)
        });
        if let Some(then) = then {
            then();
        }
    }
}

/// What a test has happen between the two questions of
/// [`ValueFile::is_closed_and_free`], on the thread that asks them.
#[cfg(test)]
pub(super) mod between_checks {
    use std::cell::RefCell;

    thread_local! {
        static NEXT: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(in crate::values) fn set(then: impl FnOnce() + 'static) {
        NEXT.with(|next| *next.borrow_mut() = Some(Box::new(then)));
    }

    pub(super) fn run() {
        if let Some(then) = NEXT.with(|next| next.borrow_mut().take()) {
            then();
        }
    }
}
