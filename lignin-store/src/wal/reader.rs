//! Reading the log: the files a manifest names, item by item, up to where
//! the log ends.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    wal::{self, LogItem, LogPlace, LogStart, WalError, FIRST_ITEM_OFFSET},
    CommitVersion, FileHeader, FileId, FileKind, FormatError, FormatVersion,
};
use lignin_io::{OpenMode, Vfs, VfsFile};

use super::error::LogError;
use crate::{segment::FileNames, table::LogDurable};

/// Bytes a reader fetches at a time, unless [`LogReader::with_window`] sets
/// another number. An item that is longer is fetched whole.
pub const DEFAULT_READ_WINDOW_LEN: usize = 1 << 20;

const PREFIX_LEN: usize = FIRST_ITEM_OFFSET as usize;
const HEADER_LEN: usize = FileHeader::ENCODED_LEN;

/// One item of the log, as [`LogReader::next_item`] hands it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRead<'a> {
    /// Where the item begins.
    pub at: LogPlace,
    /// Where the item ends, which is where the next one begins.
    pub end: LogPlace,
    /// The item: the group of a commit, a background change or a checkpoint
    /// mark.
    pub item: LogItem<'a>,
}

/// One log file as a reader left it: how much of it is part of the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileEnd {
    file: FileId,
    start: LogStart,
    valid_len: u64,
    latest: CommitVersion,
    named_len: u64,
    named_latest: CommitVersion,
}

impl FileEnd {
    /// The file.
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The file's start record.
    pub const fn start(&self) -> LogStart {
        self.start
    }

    /// The length of the file that is part of the log: the end of its last
    /// whole item.
    pub const fn valid_len(&self) -> u64 {
        self.valid_len
    }

    /// The version of the last commit group of the log up to the end of
    /// this file.
    pub const fn latest(&self) -> CommitVersion {
        self.latest
    }

    /// The valid length the manifest gives the file, which is how much of
    /// it was durable when the manifest was written.
    pub const fn named_len(&self) -> u64 {
        self.named_len
    }

    /// The version of the last commit group of the log up to
    /// [`Self::named_len`].
    pub const fn named_latest(&self) -> CommitVersion {
        self.named_latest
    }
}

/// Where the log of a database directory ends, and which files it is made
/// of: what a [`LogReader`] has found once it has read everything.
///
/// [`LogStore::open`](super::LogStore::open) takes it, so that the log is
/// read once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogEnd {
    files: Vec<FileEnd>,
    unread: Vec<FileId>,
}

impl LogEnd {
    /// The files the log is made of, in order, each as far as it is valid.
    pub fn files(&self) -> &[FileEnd] {
        &self.files
    }

    /// The log files the manifest names that hold nothing of the log: a
    /// file with no start record, a file that does not continue the log
    /// where the one before it stops being valid, and every file behind
    /// such a one.
    pub fn unread(&self) -> &[FileId] {
        &self.unread
    }

    /// The end of the log: the end of the last whole item of its last file.
    /// `None` if the log has no file.
    pub fn place(&self) -> Option<LogPlace> {
        let last = self.files.last()?;
        LogPlace::new(last.file, last.valid_len).ok()
    }

    /// The version of the last commit group of the log, or of the last one
    /// before the log if it holds none.
    pub fn latest(&self) -> CommitVersion {
        self.files
            .last()
            .map_or(CommitVersion::EMPTY, |last| last.latest)
    }

    /// The start record of the file that continues the log. A log with no
    /// file begins with [`LogStart::FIRST`].
    pub fn next_start(&self) -> Result<LogStart, FormatError> {
        match self.files.last() {
            Some(last) => last.start.next(last.valid_len, last.latest),
            None => Ok(LogStart::FIRST),
        }
    }

    /// How far the manifest vouches for the log: it gives every file up to
    /// that place a valid length that reaches it. What lies behind it was
    /// read back whole, and a loss of power can still take it.
    pub fn durable(&self) -> LogDurable {
        let mut durable = LogDurable {
            version: CommitVersion::EMPTY,
            place: None,
        };
        for file in &self.files {
            if let Ok(place) = LogPlace::new(file.file, file.named_len) {
                durable = LogDurable {
                    version: file.named_latest,
                    place: Some(place),
                };
            }
            if file.named_len < file.valid_len {
                break;
            }
        }
        durable
    }
}

/// The file a reader is in.
#[derive(Debug)]
struct Current {
    named: LiveFile,
    path: PathBuf,
    file: Box<dyn VfsFile>,
    header: [u8; HEADER_LEN],
    version: FormatVersion,
    start: LogStart,
    /// The bytes of the file that are read: all of an open file, and a
    /// closed one up to its valid length.
    limit: u64,
    /// Where the next item begins.
    offset: u64,
    latest: CommitVersion,
    named_latest: CommitVersion,
    /// The file offset of the first byte the buffer holds behind its first
    /// [`PREFIX_LEN`] bytes.
    window_at: u64,
    scratch: Vec<u8>,
}

impl Current {
    fn held_end(&self, buf: &[u8]) -> u64 {
        self.window_at + (buf.len() - PREFIX_LEN) as u64
    }

    fn place(&self, offset: u64) -> Result<LogPlace, LogError> {
        LogPlace::new(self.named.id(), offset).map_err(LogError::format(&self.path, offset))
    }

    /// Makes the buffer hold the file from the next item on, for `len`
    /// bytes or up to the limit.
    fn load(&mut self, buf: &mut Vec<u8>, len: usize) -> Result<(), LogError> {
        let consumed = (self.offset - self.window_at) as usize;
        buf.drain(PREFIX_LEN..PREFIX_LEN + consumed);
        self.window_at = self.offset;
        let have = buf.len() - PREFIX_LEN;
        let left = self.limit - self.offset;
        let want = usize::try_from(left).map_or(len, |left| left.min(len));
        if want > have {
            buf.resize(PREFIX_LEN + want, 0);
            let read = self
                .file
                .read_exact_at(&mut buf[PREFIX_LEN + have..], self.offset + have as u64);
            if let Err(error) = read {
                buf.truncate(PREFIX_LEN + have);
                return Err(LogError::io("read", &self.path)(error));
            }
        }
        Ok(())
    }

    /// Writes, in front of the next item, the first bytes of a log file
    /// that begins there: the file's own header, and a start record that
    /// gives the item the address and the version before it that it has in
    /// the file. A scan of the bytes from there on is then the scan of the
    /// file from this item on.
    fn write_prefix(&mut self, prefix: &mut [u8]) -> Result<(), LogError> {
        match self.version {
            FormatVersion::V1 => {}
        }
        let begins = self.offset - FIRST_ITEM_OFFSET;
        let start = self
            .start
            .base()
            .checked_add(begins)
            .ok_or(FormatError::Wal(WalError::AddressOutOfRange))
            .and_then(|base| LogStart::new(base, self.latest))
            .map_err(LogError::format(&self.path, self.offset))?;
        self.scratch.clear();
        wal::write_file_start(self.named.id(), &start, &mut self.scratch)
            .map_err(LogError::format(&self.path, self.offset))?;
        prefix[..HEADER_LEN].copy_from_slice(&self.header);
        prefix[HEADER_LEN..].copy_from_slice(&self.scratch[HEADER_LEN..]);
        Ok(())
    }

    /// Reads the next item of the file and hands it to `visit`. `None` when
    /// the part of the file that belongs to the log has ended.
    fn item<T, F>(
        &mut self,
        buf: &mut Vec<u8>,
        window: usize,
        visit: &mut Option<F>,
    ) -> Result<Option<T>, LogError>
    where
        F: FnOnce(LogRead<'_>) -> T,
    {
        let mut needed = 1;
        loop {
            let have = self.held_end(buf) - self.offset;
            if have < needed as u64 && self.held_end(buf) < self.limit {
                self.load(buf, needed.max(window))?;
            }
            let at = (self.offset - self.window_at) as usize;
            self.write_prefix(&mut buf[at..at + PREFIX_LEN])?;
            let reaches_limit = self.held_end(buf) >= self.limit;
            let have = buf.len() - at - PREFIX_LEN;

            let mut scan =
                wal::scan(&buf[at..]).map_err(LogError::format(&self.path, self.offset))?;
            if let Some(item) = scan.next() {
                let end = self.offset + (scan.offset() - FIRST_ITEM_OFFSET);
                let latest = scan.tail().latest();
                let read = LogRead {
                    at: self.place(self.offset)?,
                    end: self.place(end)?,
                    item,
                };
                let Some(visit) = visit.take() else {
                    return Ok(None);
                };
                let visited = visit(read);
                self.offset = end;
                self.latest = latest;
                if end <= self.named.valid_len() {
                    self.named_latest = latest;
                }
                return Ok(Some(visited));
            }
            let Some(failure) = scan.tail().failure() else {
                return Ok(None);
            };
            let record_at = (failure.offset - FIRST_ITEM_OFFSET) as usize;
            let cut_short = match failure.error {
                FormatError::Truncated { needed, .. } => Some(record_at.saturating_add(needed)),
                FormatError::Wal(WalError::UnfinishedGroup) => Some(have.saturating_add(1)),
                _ => None,
            };
            match cut_short {
                Some(whole) if !reaches_limit => needed = whole.max(have.saturating_mul(2)),
                _ if self.offset < self.named.valid_len() => {
                    return Err(LogError::Damaged {
                        path: self.path.clone(),
                        offset: self.offset,
                        valid_len: self.named.valid_len(),
                        source: failure.error,
                    });
                }
                _ => return Ok(None),
            }
        }
    }

    fn ended(&self) -> FileEnd {
        FileEnd {
            file: self.named.id(),
            start: self.start,
            valid_len: self.offset,
            latest: self.latest,
            named_len: self.named.valid_len(),
            named_latest: self.named_latest,
        }
    }
}

pub(super) fn log_files(manifest: &Manifest) -> impl Iterator<Item = &LiveFile> {
    manifest
        .files()
        .iter()
        .filter(|file| file.kind() == FileKind::Wal)
}

/// Reads the log of a database directory: the log files a manifest names,
/// in the order of their ids, item by item, each file as far as it is
/// valid. This is the recovery iterator. It changes nothing in the
/// directory.
///
/// It gives whole items only. A commit group whose last part is missing,
/// or any of whose records fails, is not given, and neither is anything
/// behind it: the log ends before the group's first record. See [the module
/// documentation](super#reading-the-log) for where a file ends, when the
/// next file continues the log, and what is damage and not an end.
///
/// The reader holds one file's window of bytes at a time, never a whole
/// file, and at least the longest item it reads.
#[derive(Debug)]
pub struct LogReader {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    database: FormatVersion,
    named: Vec<LiveFile>,
    next: usize,
    current: Option<Current>,
    end: LogEnd,
    done: bool,
    buf: Vec<u8>,
    window: usize,
}

impl LogReader {
    /// A reader at the first item of the log that `manifest` names, in the
    /// database directory `dir`.
    pub fn new(vfs: Arc<dyn Vfs>, dir: &Path, manifest: &Manifest) -> Self {
        let named = log_files(manifest).copied().collect();
        Self {
            vfs,
            dir: dir.to_path_buf(),
            database: manifest.format_version(),
            named,
            next: 0,
            current: None,
            end: LogEnd::default(),
            done: false,
            buf: vec![0; PREFIX_LEN],
            window: DEFAULT_READ_WINDOW_LEN,
        }
    }

    /// Sets how many bytes the reader fetches at a time. An item that is
    /// longer is still fetched whole.
    pub fn with_window(mut self, len: usize) -> Self {
        self.window = len.max(1);
        self
    }

    /// Reads the next item of the log and hands it to `visit`, whose result
    /// it returns. `None` when the log has ended: `visit` is not called
    /// then, and [`Self::end`] says where the log ends.
    ///
    /// The item borrows the reader's buffer, which is why it is handed to a
    /// function and not returned.
    ///
    /// An error is not an end. It says that the directory does not hold the
    /// log its manifest names: a file is missing or too short, is not the
    /// log file of its name, or fails below its valid length
    /// ([`LogError::Damaged`]). After an error the reader gives nothing
    /// more.
    pub fn next_item<T>(
        &mut self,
        visit: impl FnOnce(LogRead<'_>) -> T,
    ) -> Result<Option<T>, LogError> {
        let mut visit = Some(visit);
        while !self.done {
            let read = match self.current.as_mut() {
                Some(current) => current.item(&mut self.buf, self.window, &mut visit),
                None => {
                    self.open_next().inspect_err(|_| self.done = true)?;
                    continue;
                }
            };
            match read {
                Ok(Some(visited)) => return Ok(Some(visited)),
                Ok(None) => {
                    let ended = self.current.take().map(|current| current.ended());
                    self.end.files.extend(ended);
                }
                Err(error) => {
                    self.done = true;
                    self.current = None;
                    return Err(error);
                }
            }
        }
        Ok(None)
    }

    /// Where the log ends, once [`Self::next_item`] has returned `None`.
    pub fn end(&self) -> Option<&LogEnd> {
        self.done.then_some(&self.end)
    }

    /// Reads the rest of the log, looking at no item, and returns where it
    /// ends.
    pub fn into_end(mut self) -> Result<LogEnd, LogError> {
        while self.next_item(|_| ())?.is_some() {}
        Ok(self.end)
    }

    /// Opens the next file the manifest names, if it continues the log.
    /// Otherwise the log has ended.
    fn open_next(&mut self) -> Result<(), LogError> {
        let Some(named) = self.named.get(self.next).copied() else {
            self.done = true;
            return Ok(());
        };
        let path = FileNames::WAL.path(&self.dir, named.id());
        let file = self
            .vfs
            .open(&path, OpenMode::Read)
            .map_err(LogError::io("open", &path))?;
        let len = file.len().map_err(LogError::io("measure", &path))?;
        let valid_len = named.valid_len();
        if len < valid_len {
            return Err(LogError::FileTooShort {
                path,
                valid_len,
                len,
            });
        }
        let limit = match named.state() {
            FileState::Open => len,
            FileState::Closed | FileState::Sealed => valid_len,
        };

        let mut first = [0; PREFIX_LEN];
        let first = &mut first[..limit.min(FIRST_ITEM_OFFSET) as usize];
        file.read_exact_at(first, 0)
            .map_err(LogError::io("read", &path))?;
        let damaged = |offset: u64, source: FormatError| LogError::Damaged {
            path: path.clone(),
            offset,
            valid_len,
            source,
        };
        let header = FileHeader::decode(first).map_err(|error| damaged(0, error))?;
        if header.kind() != FileKind::Wal {
            let found = header.kind();
            return Err(damaged(0, WalError::NotALog { found }.into()));
        }
        if header.file_id() != Some(named.id()) {
            let found = header.sequence();
            return Err(LogError::WrongFileId { path, found });
        }
        if header.version() > self.database {
            return Err(LogError::FileNewerThanManifest {
                path,
                file: header.version(),
                manifest: self.database,
            });
        }

        // The manifest vouches for the bytes below the valid length. A
        // start record behind it is part of the tail: it may be torn, or
        // written for an end of the file before that a crash took away.
        let vouched = valid_len >= FIRST_ITEM_OFFSET;
        let start = match wal::read_file_start(first) {
            Ok((_, start)) => Some(start),
            Err(error) if vouched => return Err(damaged(HEADER_LEN as u64, error)),
            Err(_) => None,
        };
        let continues = match (start, self.end.files.last()) {
            (None, _) => None,
            (Some(start), None) => Some(start),
            (Some(start), Some(before)) => {
                match before
                    .start
                    .check_next(before.valid_len, before.latest, &start)
                {
                    Ok(()) => Some(start),
                    Err(error) if vouched => return Err(damaged(HEADER_LEN as u64, error)),
                    Err(_) => None,
                }
            }
        };
        let Some(start) = continues else {
            let unread = self.named[self.next..].iter().map(LiveFile::id);
            self.end.unread.extend(unread);
            self.next = self.named.len();
            self.done = true;
            return Ok(());
        };

        self.next += 1;
        self.buf.truncate(PREFIX_LEN);
        let mut header_bytes = [0; HEADER_LEN];
        header_bytes.copy_from_slice(&first[..HEADER_LEN]);
        self.current = Some(Current {
            named,
            path,
            file,
            header: header_bytes,
            version: header.version(),
            start,
            limit,
            offset: FIRST_ITEM_OFFSET,
            latest: start.after(),
            named_latest: start.after(),
            window_at: FIRST_ITEM_OFFSET,
            scratch: Vec::with_capacity(PREFIX_LEN),
        });
        Ok(())
    }
}
