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

#[derive(Debug)]
pub(super) struct ValueFile {
    id: FileId,
    path: PathBuf,
    version: FormatVersion,
    file: Box<dyn VfsFile>,
    end: AtomicU64,
    appendable: AtomicBool,
    holds: AtomicUsize,
    life: Mutex<Life>,
}

impl ValueFile {
    pub(super) fn adopt(
        vfs: &dyn Vfs,
        db: &Path,
        named: &LiveFile,
        database: FormatVersion,
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
        if version > database {
            return Err(ValueStoreError::FileNewerThanManifest {
                path,
                file: version,
                manifest: database,
            });
        }
        Ok(Self::new(
            id,
            path,
            version,
            file,
            Phase::Closed,
            valid_len,
            false,
        ))
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
            holds: AtomicUsize::new(0),
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

    pub(super) fn contains(&self, pointer: ValuePointer) -> bool {
        pointer
            .offset()
            .checked_add(pointer.extent_len(self.version))
            .is_some_and(|value_end| value_end <= self.end())
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
        self.life().phase == Phase::Closed
    }

    pub(super) fn hold(&self) {
        self.holds.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn release_hold(&self) {
        self.holds.fetch_sub(1, Ordering::AcqRel);
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
            Phase::Open => Ok(()),
            Phase::Created => Err(ValueStoreError::NotNamed { file }),
            Phase::Closed => Err(ValueStoreError::NotOpen { file }),
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
        }
    }

    pub(super) fn sync(&self) -> Result<u64, ValueStoreError> {
        let mut life = self.life();
        if life.phase == Phase::Closed {
            return Ok(life.durable_len);
        }
        if life.failed {
            return Err(ValueStoreError::FileFailed { file: self.id });
        }
        let end = self.end();
        if end > life.durable_len {
            if let Err(error) = self.file.sync_data() {
                life.failed = true;
                self.appendable.store(false, Ordering::Release);
                self.settle(&mut life);
                return Err(ValueStoreError::io("sync", &self.path)(error));
            }
            life.durable_len = end;
        }
        self.settle(&mut life);
        Ok(life.durable_len)
    }

    pub(super) fn release_writer(&self) {
        let mut life = self.life();
        life.has_writer = false;
        self.appendable.store(false, Ordering::Release);
        self.settle(&mut life);
    }
}
