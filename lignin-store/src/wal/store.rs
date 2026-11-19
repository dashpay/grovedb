//! The log files of one database directory: appending to them, turning from
//! one to the next, syncing them, and their lives from creation to deletion.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    time::{Duration, Instant},
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    value_log::PendingValue,
    wal::{
        self, CheckpointMark, LogAddress, LogCursor, LogPlace, LogStart, LoggedValue, Record,
        WalError, CHECKPOINT_MARK_LEN, FIRST_ITEM_OFFSET, MAX_LOG_OFFSET,
    },
    CommitVersion, FileHeader, FileId, FileKind, FormatError, FormatVersion, PageId,
};
use lignin_io::{Create, DirEntry, EntryKind, OpenMode, Vfs};

use super::{
    error::LogError,
    file::LogFile,
    reader::{log_files, LogEnd},
};
use crate::{
    segment::{create_file, list_or_create_dir, FileNames, ManifestStore},
    table::LogDurable,
};

/// The length at which the log turns to its next file, unless
/// [`LogOptions::target_file_len`] sets another.
pub const DEFAULT_TARGET_FILE_LEN: u64 = 64 << 20;

const HEADER_LEN: u64 = FileHeader::ENCODED_LEN as u64;

/// What a file of an earlier session is called while it is written again.
const TEMPORARY_SUFFIX: &str = ".tmp";

/// How often [`LogStore::roll`] turns, creates and replaces the manifest
/// before it gives up: more than it takes when one of them fails.
const ROLL_ROUNDS: usize = 6;

/// Bytes that are copied at a time when a file is written again.
const COPY_CHUNK_LEN: u64 = 1 << 20;

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(TEMPORARY_SUFFIX);
    PathBuf::from(name)
}

/// When a commit makes the log durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// No commit syncs the log. It is made durable by [`LogStore::sync`]:
    /// by `flush()` and by a checkpoint. This is the default (DEC-04).
    Never,
    /// A commit syncs the log when the last sync is at least this long ago.
    /// A timer that calls [`LogStore::sync_if_due`] does the same between
    /// commits.
    Interval(Duration),
    /// Every commit syncs the log through its own group.
    EveryCommit,
}

impl SyncPolicy {
    /// Whether a commit syncs the log, when the last sync was `since_sync`
    /// ago.
    pub fn is_due(&self, since_sync: Duration) -> bool {
        match self {
            Self::Never => false,
            Self::Interval(interval) => since_sync >= *interval,
            Self::EveryCommit => true,
        }
    }
}

/// How a [`LogStore`] writes its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogOptions {
    /// The length at which a log file is full and the log turns to the
    /// next one ([`LogStore::rotate`]). An item is never split over two
    /// files, and the log goes on in a full file until it is turned, so a
    /// file can end up longer. A length that leaves no room for an item
    /// behind the start record is taken as one byte more than that.
    pub target_file_len: u64,
    /// When a commit makes the log durable.
    pub sync: SyncPolicy,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            target_file_len: DEFAULT_TARGET_FILE_LEN,
            sync: SyncPolicy::Never,
        }
    }
}

/// Where an item that was appended lies in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appended {
    /// Where the item begins.
    pub at: LogPlace,
    /// Where the item ends: the end of the log when the append returned.
    pub end: LogPlace,
}

/// The place the next commit group goes to, from [`LogStore::next_group`]:
/// the end of the log when it was asked for.
///
/// A group is written for one place (LG22 of `log-format.md`): its cells
/// hold log addresses inside it. So the commit asks for the place, writes
/// its group with [`Self::version`] and [`Self::address`], and appends it
/// with [`LogStore::append_group`], which takes the group only while the
/// log still ends here.
#[derive(Debug, Clone)]
pub struct GroupPlace {
    session: u64,
    turn: u64,
    place: LogPlace,
    address: LogAddress,
    version: CommitVersion,
    cursor: LogCursor,
}

impl GroupPlace {
    #[cfg(test)]
    pub(super) fn cursor(&self) -> &LogCursor {
        &self.cursor
    }

    /// The file and the offset the group's first byte will have.
    pub fn place(&self) -> LogPlace {
        self.place
    }

    /// The log address the group's first byte will have: what
    /// [`GroupWriter::new`](lignin_format::wal::GroupWriter::new) takes.
    pub fn address(&self) -> LogAddress {
        self.address
    }

    /// The commit version the group must have: the one after the last
    /// group of the log.
    pub fn version(&self) -> CommitVersion {
        self.version
    }
}

/// The places the log is kept from, each for one checkpoint.
#[derive(Debug, Default)]
struct Holds {
    kept: Mutex<BTreeMap<u64, Option<LogPlace>>>,
    taken: AtomicU64,
}

/// Keeps the log from a place on, for a page-table checkpoint: from
/// [`LogStore::hold_from`].
///
/// While it lives, [`LogStore::drop_before`] takes out no log file from the
/// file of the place on, whatever place it is called with. Dropping the
/// hold lets them go again.
///
/// Whoever writes and installs table checkpoints has one hold for each
/// checkpoint that the manifest names or may come to name (K14 of
/// `page-table.md`):
///
/// - for the checkpoint in force, from the session's start, until another
///   one has been installed
/// - for a checkpoint that was written, from the moment its sample is
///   taken, until it is discarded or the manifest is known to name another
/// - after `ManifestInDoubt`, for both, until the manifest has been opened
///   again
///
/// Its place is [`CheckpointInfo::log_kept_from`]: the end of the mark the
/// sample was taken at. With no checkpoint in force the whole log is kept,
/// which is the hold from `None`.
///
/// [`CheckpointInfo::log_kept_from`]: crate::table::CheckpointInfo::log_kept_from
#[derive(Debug)]
pub struct LogHold {
    holds: Arc<Holds>,
    id: u64,
    place: Option<LogPlace>,
}

impl LogHold {
    /// The place the log is kept from. `None` is the place before every
    /// record: the whole log is kept.
    pub fn place(&self) -> Option<LogPlace> {
        self.place
    }
}

impl Drop for LogHold {
    fn drop(&mut self) {
        lock(&self.holds.kept).remove(&self.id);
    }
}

/// An error of the file layer that the log goes on reporting.
#[derive(Debug)]
struct Sticky {
    path: PathBuf,
    kind: ErrorKind,
    message: String,
}

impl Sticky {
    fn of(path: &Path, error: &io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: error.kind(),
            message: error.to_string(),
        }
    }

    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ItemKind {
    Group,
    Change,
    Mark,
}

impl ItemKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Group => "a commit group",
            Self::Change => "the record of a background change",
            Self::Mark => "a checkpoint mark",
        }
    }
}

/// Whether an item that begins at `offset` of a file with the start record
/// `start` can be read. A reader gives an item the addresses of a file
/// that begins with it, so behind the last address at which a file can
/// begin no reader takes an item: the log is full there.
fn has_address(start: &LogStart, offset: u64) -> bool {
    let begins = start.base().checked_add(offset - FIRST_ITEM_OFFSET);
    begins.is_some_and(|base| base <= MAX_LOG_OFFSET)
}

/// Reads `bytes` as recovery will: as records that follow what `before` has
/// read. They must be exactly one item, of the kind `kind`. Returns the
/// cursor behind the item.
pub(super) fn check_item(
    before: &LogCursor,
    bytes: &[u8],
    kind: ItemKind,
) -> Result<LogCursor, LogError> {
    let mut cursor = before.clone();
    let mut at = 0;
    let last = loop {
        let step = cursor
            .step(bytes.get(at..).unwrap_or_default())
            .map_err(|source| LogError::Refused { offset: at, source })?;
        at += step.len;
        if step.ends_item {
            break step.record;
        }
    };
    let found = match last {
        Record::Part(_) => Some(ItemKind::Group),
        Record::Change(_) => Some(ItemKind::Change),
        Record::Mark(_) => Some(ItemKind::Mark),
        Record::Start(_) | Record::Value(_) | Record::Carried(_) => None,
    };
    if found != Some(kind) {
        return Err(LogError::WrongItem {
            expected: kind.name(),
        });
    }
    if at != bytes.len() {
        return Err(LogError::NotOneItem {
            len: bytes.len(),
            item_len: at,
        });
    }
    Ok(cursor)
}

/// What became of an append that the file layer failed.
#[derive(Debug)]
enum Settled {
    /// All of its bytes are in the file, as they were given.
    InLog,
    /// The file ends where it did. The append wrote nothing, or what it
    /// wrote was taken back, and a crash can bring that back until the
    /// file as it is now is durable.
    Absent,
    /// A part of it, or bytes that are no item, stay behind the end of the
    /// log. No crash completes them.
    Torn,
    /// It could not be found out.
    Unknown(io::Error),
}

/// Finds out whether the bytes of a failed append at `offset` of `file` are
/// in the file, by recovery's own test: they are there in full, and they
/// are what was written. Whatever else stays behind `offset` is read
/// back, and `verifies` says whether a recovery would take an item from
/// it. Nothing is synced.
fn settle(
    file: &LogFile,
    offset: u64,
    bytes: &[u8],
    verifies: impl FnOnce(&[u8]) -> bool,
) -> Settled {
    let len = match file.len() {
        Ok(len) => len,
        Err(error) => return Settled::Unknown(error),
    };
    let whole = offset + bytes.len() as u64;
    if len == offset {
        return Settled::Absent;
    }
    if len < offset || len > whole {
        return Settled::Unknown(io::Error::other(format!(
            "the file is {len} bytes long after an append of {} bytes at offset {offset} failed",
            bytes.len()
        )));
    }
    let mut found = vec![0; (len - offset) as usize];
    if let Err(error) = file.read_exact_at(&mut found, offset) {
        return Settled::Unknown(error);
    }
    if found == bytes {
        Settled::InLog
    } else if verifies(&found) {
        Settled::Unknown(io::Error::other(format!(
            "after an append at offset {offset} failed, the file holds bytes there that are not \
             what was appended and that a recovery would take"
        )))
    } else {
        Settled::Torn
    }
}

#[derive(Debug)]
struct Entry {
    file: Arc<LogFile>,
    /// How much of the file is durable: all a manifest names of it.
    durable_len: u64,
}

impl Entry {
    /// Whether the file takes no more items and all of it is durable: what
    /// a manifest names as closed.
    fn is_closed(&self) -> bool {
        self.file.is_finished() && self.durable_len == self.file.end()
    }
}

/// The files the log has, and how far each is durable. Whoever reads a
/// pending value takes this for one lookup. It is never held together with
/// the tail.
#[derive(Debug)]
struct Files {
    live: BTreeMap<FileId, Entry>,
    /// Files taken out of the log, until they are deleted.
    dropped: BTreeMap<FileId, PathBuf>,
    /// The highest id of a file the log has had: what a manifest records.
    last_id: Option<FileId>,
    durable: LogDurable,
    sync_failed: Option<Sticky>,
    last_sync: Instant,
}

impl Files {
    fn named(&self) -> Result<Vec<LiveFile>, LogError> {
        self.live
            .values()
            .filter(|entry| !entry.file.is_dead())
            .map(|entry| {
                let state = if entry.is_closed() {
                    FileState::Closed
                } else {
                    FileState::Open
                };
                LiveFile::new(FileKind::Wal, entry.file.id(), state, entry.durable_len)
                    .map_err(LogError::format(entry.file.path(), 0))
            })
            .collect()
    }
}

/// The end of the log. This is the append lock: it is held for one append,
/// and nothing is acquired under it.
#[derive(Debug)]
struct Tail {
    unsettled: Option<Sticky>,
    /// The file the log ends in, with the cursor behind its last item. It
    /// takes items until it is finished.
    active: Option<(Arc<LogFile>, LogCursor)>,
    /// The start record of the session's first file.
    first_start: LogStart,
    /// Where the log of the earlier sessions ends.
    adopted_end: Option<LogPlace>,
    /// The version of the last commit group of the log.
    latest: CommitVersion,
    /// The file the log turns to next, and whether a durable manifest
    /// names it.
    spare: Option<(Arc<LogFile>, bool)>,
    /// How often the log has turned.
    turn: u64,
}

impl Tail {
    fn check_settled(&self) -> Result<(), LogError> {
        match &self.unsettled {
            Some(unsettled) => Err(LogError::Unsettled {
                path: unsettled.path.clone(),
                source: unsettled.error(),
            }),
            None => Ok(()),
        }
    }

    /// The file that takes items, and the cursor behind its last item.
    fn active(&self) -> Result<(&Arc<LogFile>, &LogCursor), LogError> {
        self.check_settled()?;
        match &self.active {
            Some((file, cursor)) if !file.is_finished() => Ok((file, cursor)),
            _ => Err(LogError::NoActiveFile),
        }
    }

    /// The place after the last record of the log.
    fn end(&self) -> Option<LogPlace> {
        match &self.active {
            Some((file, cursor)) => LogPlace::new(file.id(), cursor.offset()).ok(),
            None => self.adopted_end,
        }
    }

    /// The start record of the file the log turns to.
    fn next_start(&self) -> Result<LogStart, FormatError> {
        match &self.active {
            Some((file, cursor)) => file
                .start()
                .ok_or(WalError::MissingStart)?
                .next(cursor.offset(), self.latest),
            None => Ok(self.first_start),
        }
    }
}

static SESSIONS: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn remove(vfs: &dyn Vfs, path: &Path) -> io::Result<()> {
    match vfs.remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_unnamed(
    vfs: &dyn Vfs,
    dir: &Path,
    entries: &[DirEntry],
    named: &BTreeSet<FileId>,
    last_id: Option<FileId>,
) -> Result<(), LogError> {
    let wal_dir = FileNames::WAL.dir(dir);
    let mut unnamed = Vec::new();
    for entry in entries {
        if entry.kind != EntryKind::File {
            continue;
        }
        // What an interrupted rewrite of a log file left behind.
        let rewritten = entry
            .name
            .to_str()
            .and_then(|name| name.strip_suffix(TEMPORARY_SUFFIX))
            .and_then(|name| FileNames::WAL.parse(name.as_ref()));
        if rewritten.is_some() {
            unnamed.push(wal_dir.join(&entry.name));
            continue;
        }
        let Some(id) = FileNames::WAL.parse(&entry.name) else {
            continue;
        };
        if named.contains(&id) {
            continue;
        }
        let path = wal_dir.join(&entry.name);
        if Some(id) > last_id {
            let len = vfs
                .open(&path, OpenMode::Read)
                .and_then(|file| file.len())
                .map_err(LogError::io("measure", &path))?;
            if len > HEADER_LEN {
                return Err(LogError::UnnamedFileHoldsItems { path, len });
            }
        }
        unnamed.push(path);
    }
    for path in unnamed {
        remove(vfs, &path).map_err(LogError::io("remove", &path))?;
    }
    Ok(())
}

/// The write-ahead log of one database directory.
///
/// It appends items to the file the log ends in, turns to the next file,
/// makes the log durable, reads a pending value by its address, and deletes
/// a file once the caller no longer needs it. See [the module
/// documentation](super) for the life of a file, for what an append is, for
/// what a failed append and a failed sync leave, and for what the caller
/// does.
///
/// Every method takes `&self` and may be called from any thread. An append
/// holds the log's append lock for one call of the file layer. It waits for
/// no sync, no creation of a file and no manifest.
#[derive(Debug)]
pub struct LogStore {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    options: LogOptions,
    /// Tells this log's places from another's.
    session: u64,
    /// The highest id a creation has taken. It is held for the length of a
    /// creation, so that files enter the log in the order of their ids.
    creating: Mutex<Option<FileId>>,
    /// Held by the one sync that runs.
    syncing: Mutex<()>,
    files: RwLock<Files>,
    tail: Mutex<Tail>,
    holds: Arc<Holds>,
    /// Whether the log would turn to another file now.
    due: AtomicBool,
    appended: Appends,
}

/// What a [`LogStore`] has appended since it was opened, by kind of item
/// ([`LogStore::appended`]). An item counts once it is known to be in the
/// log; one whose append failed does not count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogAppends {
    /// Commit groups.
    pub groups: u64,
    /// Their bytes, every record's frame included.
    pub group_bytes: u64,
    /// Records of background changes: page steps.
    pub changes: u64,
    /// Their bytes, frames included.
    pub change_bytes: u64,
    /// Table-checkpoint marks.
    pub marks: u64,
    /// Their bytes, frames included.
    pub mark_bytes: u64,
}

impl LogAppends {
    /// Every byte appended: groups, changes and marks.
    pub fn bytes(&self) -> u64 {
        self.group_bytes + self.change_bytes + self.mark_bytes
    }
}

#[derive(Debug, Default)]
struct Appends {
    groups: AtomicU64,
    group_bytes: AtomicU64,
    changes: AtomicU64,
    change_bytes: AtomicU64,
    marks: AtomicU64,
    mark_bytes: AtomicU64,
}

impl Appends {
    fn count(&self, kind: ItemKind, len: usize) {
        let (items, bytes) = match kind {
            ItemKind::Group => (&self.groups, &self.group_bytes),
            ItemKind::Change => (&self.changes, &self.change_bytes),
            ItemKind::Mark => (&self.marks, &self.mark_bytes),
        };
        items.fetch_add(1, Ordering::Relaxed);
        bytes.fetch_add(len as u64, Ordering::Relaxed);
    }

    fn read(&self) -> LogAppends {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        LogAppends {
            groups: read(&self.groups),
            group_bytes: read(&self.group_bytes),
            changes: read(&self.changes),
            change_bytes: read(&self.change_bytes),
            marks: read(&self.marks),
            mark_bytes: read(&self.mark_bytes),
        }
    }
}

impl LogStore {
    /// Opens the log that `manifest` names, in its directory, for writing.
    /// `end` is where a [`LogReader`](super::LogReader) found the log to
    /// end, with nothing written to `wal/` since and the log files named
    /// as the reader's manifest named them: an end of another directory,
    /// or of files that have another length now, is refused
    /// ([`LogError::WrongDirectory`], [`LogError::WrongLogEnd`]). What the
    /// manifest says of the other kinds of files may have changed
    /// meanwhile, as it does when a recovery writes a segment while it
    /// reads the log.
    ///
    /// Every file of the log belongs to an earlier session. It is read and
    /// synced, and never appended to: it ends where the reader found its
    /// last whole item. The log goes on in a file this session creates
    /// ([`Self::roll`]), whose start record says where the last one
    /// stopped being valid.
    ///
    /// The files the manifest names that hold nothing of the log
    /// ([`LogEnd::unread`]) are taken out: the next manifest does not name
    /// them, and they are deleted then. Every file in `wal/` with a log
    /// file's name that the manifest does not name is removed at once: a
    /// crash can leave one that was created and never named, or one that
    /// was dropped and not yet deleted. The manifest is durable when a
    /// [`ManifestStore`] of it exists, so no crash brings back one that
    /// names such a file. If such a file has an id above the last id in
    /// the manifest and holds more than a file header, the open fails with
    /// [`LogError::UnnamedFileHoldsItems`] and removes nothing. What an
    /// interrupted rewrite of a log file left is removed too. Other entries
    /// of `wal/` are left alone.
    ///
    /// `wal/` is created if it is missing, and its name is made durable
    /// either way.
    pub fn open(
        manifest: &ManifestStore,
        end: &LogEnd,
        options: LogOptions,
    ) -> Result<Self, LogError> {
        let current = manifest.manifest()?;
        let vfs = manifest.vfs().clone();
        let dir = manifest.dir().to_path_buf();
        if end.dir() != dir {
            return Err(LogError::WrongDirectory {
                expected: dir,
                found: end.dir().to_path_buf(),
            });
        }
        let options = LogOptions {
            target_file_len: options.target_file_len.max(FIRST_ITEM_OFFSET + 1),
            ..options
        };

        let mut read: BTreeMap<FileId, _> = end.files().iter().map(|f| (f.file(), f)).collect();
        let mut unread: BTreeSet<FileId> = end.unread().iter().copied().collect();
        let mut live = BTreeMap::new();
        let mut dropped = BTreeMap::new();
        let mut named = BTreeSet::new();
        for file in log_files(current) {
            let id = file.id();
            named.insert(id);
            if let Some(found) = read.remove(&id) {
                let agrees = found.named_len() == file.valid_len()
                    && (file.state() == FileState::Open || found.valid_len() == file.valid_len());
                if !agrees {
                    return Err(LogError::WrongLogEnd { file: id });
                }
                let adopted = LogFile::adopted(&*vfs, &dir, found, current.format_version())?;
                let entry = Entry {
                    file: Arc::new(adopted),
                    durable_len: file.valid_len(),
                };
                live.insert(id, entry);
            } else if unread.remove(&id) {
                dropped.insert(id, FileNames::WAL.path(&dir, id));
            } else {
                return Err(LogError::WrongLogEnd { file: id });
            }
        }
        if let Some(&file) = read.keys().chain(&unread).next() {
            return Err(LogError::WrongLogEnd { file });
        }
        let first_start = end
            .next_start()
            .map_err(LogError::format(&FileNames::WAL.dir(&dir), 0))?;

        let last_id = current.last_file_ids().wal;
        let entries = list_or_create_dir(&*vfs, &dir, &FileNames::WAL)?;
        remove_unnamed(&*vfs, &dir, &entries, &named, last_id)?;

        Ok(Self {
            vfs,
            dir,
            options,
            session: SESSIONS.fetch_add(1, Ordering::Relaxed),
            creating: Mutex::new(last_id),
            syncing: Mutex::new(()),
            files: RwLock::new(Files {
                live,
                dropped,
                last_id,
                durable: end.durable(),
                sync_failed: None,
                last_sync: Instant::now(),
            }),
            tail: Mutex::new(Tail {
                unsettled: None,
                active: None,
                first_start,
                adopted_end: end.place(),
                latest: end.latest(),
                spare: None,
                turn: 0,
            }),
            holds: Arc::default(),
            due: AtomicBool::new(true),
            appended: Appends::default(),
        })
    }

    fn files(&self) -> RwLockReadGuard<'_, Files> {
        self.files.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn files_mut(&self) -> RwLockWriteGuard<'_, Files> {
        self.files.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn tail(&self) -> MutexGuard<'_, Tail> {
        lock(&self.tail)
    }

    fn check_same_dir(&self, manifest: &ManifestStore) -> Result<(), LogError> {
        if manifest.dir() == self.dir {
            Ok(())
        } else {
            Err(LogError::WrongDirectory {
                expected: self.dir.clone(),
                found: manifest.dir().to_path_buf(),
            })
        }
    }

    /// Whether an append that was taken back still waits for a sync.
    pub(super) fn waits_for_sync(&self) -> bool {
        matches!(&self.tail().active, Some((file, _)) if file.is_taken_back())
    }

    /// The database directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How the log writes its files.
    pub fn options(&self) -> LogOptions {
        self.options
    }

    /// Where the log ends now: the place after its last record, or `None`
    /// while it has none. Before the session has turned to its first file
    /// it is where the log of the earlier sessions ends.
    ///
    /// The place never goes back in a session, and the next session begins
    /// at it unless a crash took items. Every item whose append has
    /// returned is before it, so the record of every change that has
    /// reached a block is:
    /// a page step appends its record before its change is written back.
    /// This is what [`TableCheckpoints::write`] asks for, before it copies
    /// its first chain and after its last (K14 of `page-table.md`).
    ///
    /// [`TableCheckpoints::write`]: crate::table::TableCheckpoints::write
    pub fn end(&self) -> Option<LogPlace> {
        self.tail().end()
    }

    /// The version of the last commit group of the log.
    pub fn latest(&self) -> CommitVersion {
        self.tail().latest
    }

    /// Whether [`Self::rotate`] would turn the log to another file now: the
    /// file the log ends in is full, an append to it failed, or the
    /// session has no file yet.
    pub fn is_due(&self) -> bool {
        self.due.load(Ordering::Acquire)
    }

    /// Creates the file the log turns to next.
    ///
    /// The file is created under an id above every id used so far, with its
    /// header, and both are durable when this returns. It takes no item
    /// until a manifest names it: replace the manifest ([`Self::commit`])
    /// and then turn the log ([`Self::rotate`]). [`Self::roll`] does the
    /// three.
    ///
    /// The log has one such file at a time
    /// ([`LogError::SpareExists`]). The file and its id enter the log in
    /// one step, when the file is whole, so a manifest written before that
    /// neither names the file nor records its id.
    pub fn create(&self) -> Result<FileId, LogError> {
        let mut taken = lock(&self.creating);
        {
            let tail = self.tail();
            tail.check_settled()?;
            if let Some((spare, _)) = &tail.spare {
                return Err(LogError::SpareExists { file: spare.id() });
            }
        }
        let next = taken.map_or(1, |last| last.get().wrapping_add(1));
        let id = FileId::new(next).ok_or(LogError::FileIdsExhausted)?;
        *taken = Some(id);

        let handle = create_file(&*self.vfs, &self.dir, &FileNames::WAL, id)?;
        let path = FileNames::WAL.path(&self.dir, id);
        let file = Arc::new(LogFile::created(id, path, handle));
        self.tail().spare = Some((file.clone(), false));
        let mut files = self.files_mut();
        let entry = Entry {
            file,
            durable_len: HEADER_LEN,
        };
        files.live.insert(id, entry);
        files.last_id = Some(id);
        Ok(id)
    }

    /// Turns the log to its next file, if it is due: the file the log ends
    /// in is full, an append to it failed, or the session has no file yet.
    /// Returns whether it turned.
    ///
    /// The file the log ends in takes no more items. The next file gets
    /// its start record, which says where that one ends and which commit
    /// it comes after (LG9), in one append, and takes the items from then
    /// on. Nothing is synced.
    ///
    /// The log turns only to a file that a durable manifest names
    /// ([`LogError::NoSpare`], [`LogError::SpareNotNamed`]): a log file
    /// that no manifest names is not read after a crash.
    ///
    /// A turn moves the end of the log, like any append that is not a
    /// commit's. Make it where no commit is between asking for its place
    /// and appending its group, or that commit writes its group again
    /// ([`LogError::PlaceTaken`]).
    ///
    /// After an append that failed, the log turns only once a durable
    /// manifest names the file closed where its last whole item ends
    /// ([`LogError::CutNotDurable`]): [`Self::roll`] does that.
    ///
    /// If the file layer fails the append of the start record, the log has
    /// not turned ([`LogError::NotAppended`]), and the file it would have
    /// turned to is given up: create another one.
    pub fn rotate(&self) -> Result<bool, LogError> {
        self.turn(true)
    }

    /// Turns the log to its next file if a turn is due. A file that still
    /// takes items is turned away from, when it is full, only if
    /// `from_a_file_that_takes_items` says so: that turn moves an end of
    /// the log at which a commit may have asked for the place of its group.
    fn turn(&self, from_a_file_that_takes_items: bool) -> Result<bool, LogError> {
        let mut tail = self.tail();
        tail.check_settled()?;
        let due = match &tail.active {
            Some((file, cursor)) if !file.is_finished() => {
                from_a_file_that_takes_items && cursor.offset() >= self.options.target_file_len
            }
            Some((file, _)) if file.is_uncut() => {
                return Err(LogError::CutNotDurable {
                    path: file.path().to_path_buf(),
                });
            }
            _ => true,
        };
        if !due {
            return Ok(false);
        }
        let (spare, named) = tail.spare.clone().ok_or(LogError::NoSpare)?;
        if !named {
            return Err(LogError::SpareNotNamed { file: spare.id() });
        }

        let refused = LogError::format(spare.path(), HEADER_LEN);
        let (start, record, cursor) = tail
            .next_start()
            .and_then(|start| {
                let mut bytes = Vec::with_capacity(FIRST_ITEM_OFFSET as usize);
                wal::write_file_start(spare.id(), &start, &mut bytes)?;
                let header = FileHeader::decode(&bytes)?;
                let cursor = LogCursor::new(&header, &start)?;
                Ok((start, bytes.split_off(HEADER_LEN as usize), cursor))
            })
            .map_err(refused)?;

        let takes_items = match spare.append(&record) {
            Ok(HEADER_LEN) => true,
            Ok(found) => {
                let error = io::Error::other(format!(
                    "the start record landed at offset {found} of a new log file"
                ));
                return Err(self.unsettle(&mut tail, &spare, error));
            }
            Err(error) => {
                let verifies = |found: &[u8]| {
                    let mut file =
                        FileHeader::new(FileKind::Wal, spare.version(), spare.id().get())
                            .map(|header| header.encode().to_vec())
                            .unwrap_or_default();
                    file.extend_from_slice(found);
                    wal::read_file_start(&file).is_ok()
                };
                match settle(&spare, HEADER_LEN, &record, verifies) {
                    Settled::InLog => false,
                    Settled::Absent | Settled::Torn => {
                        // Whatever the file holds, its handle may append
                        // no more.
                        spare.kill();
                        tail.spare = None;
                        return Err(LogError::NotAppended {
                            path: spare.path().to_path_buf(),
                            source: error,
                        });
                    }
                    Settled::Unknown(why) => return Err(self.unsettle(&mut tail, &spare, why)),
                }
            }
        };

        if let Some((before, _)) = &tail.active {
            before.finish();
        }
        spare.begin(start, FIRST_ITEM_OFFSET);
        if !takes_items {
            spare.finish();
        }
        tail.active = Some((spare, cursor));
        tail.spare = None;
        tail.turn += 1;
        let full = FIRST_ITEM_OFFSET >= self.options.target_file_len;
        self.due.store(!takes_items || full, Ordering::Release);
        Ok(true)
    }

    /// Makes the log durable, gives it a file that takes items if it has
    /// none, and sees to it that it then has a file to turn to which a
    /// durable manifest names. Returns whether it turned.
    ///
    /// It is [`Self::sync`], then a turn, [`Self::create`] and
    /// [`Self::commit`] as far as each is needed. A session calls it once
    /// before its first append: the first manifest it writes then names
    /// every file of the earlier sessions closed where the log was found
    /// to end in it, and names no file that holds nothing of the log.
    /// After that it is called by whoever keeps the log's files, from time
    /// to time and when [`Self::is_due`], and by nobody that is inside a
    /// commit or a page step.
    ///
    /// **It turns the log only while the log has no file that takes
    /// items**: before the session's first turn, and after an append that
    /// failed. No commit can have asked for the place of its group at such
    /// an end of the log and still append there
    /// ([`LogError::NoActiveFile`]), so this turn takes no commit's place,
    /// whoever calls it and under whatever lock. A file that is full and
    /// still takes items is left to [`Self::rotate`], which is made where
    /// no commit is between asking for its place and appending its group:
    /// by the committer whose file is full, or under the lock that keeps
    /// commits out of the log ([the locks](super#locks)). So when this
    /// returns `Ok`, the log takes items, a named file was ready, as far as
    /// that can be said while committers turn the log, and
    /// [`Self::is_due`] may still say that the file it ends in is full.
    ///
    /// Because it leaves a file ready, the log can be turned with
    /// [`Self::rotate`] alone the next time, which syncs nothing and
    /// replaces no manifest.
    ///
    /// After an append that failed it replaces the manifest before it
    /// turns, with the file named closed where its last whole item ends:
    /// the cut of CM9, which is made before the next commit and not by
    /// one.
    ///
    /// If an append or a turn fails while it runs, it goes round again,
    /// and after a few rounds it ends with what stands in the way, such as
    /// [`LogError::CutNotDurable`] or [`LogError::NoSpare`]. Committers
    /// that turn the log to each file as fast as it is made are no such
    /// failure: it then returns `Ok` with no file ready, and the next call
    /// goes on.
    ///
    /// If it fails, it can be called again.
    pub fn roll(&self, manifest: &mut ManifestStore) -> Result<bool, LogError> {
        self.check_same_dir(manifest)?;
        self.sync()?;
        let mut turned = false;
        for _ in 0..ROLL_ROUNDS {
            turned |= self.turn_if_ready()?;
            let (spare, uncut, takes_items) = {
                let tail = self.tail();
                let uncut = matches!(&tail.active, Some((file, _)) if file.is_uncut());
                let spare = tail.spare.as_ref().map(|(_, named)| *named);
                (spare, uncut, tail.active().is_ok())
            };
            match spare {
                Some(true) if takes_items => return Ok(turned),
                // The log has a file to turn to and none that takes items:
                // the next round turns to it, or says why it cannot.
                Some(true) if !uncut => continue,
                Some(_) => {}
                None => {
                    self.create()?;
                }
            }
            if uncut {
                // The file takes no more items, so this makes it durable
                // to its end, and the manifest names it closed.
                self.sync()?;
            }
            self.commit(manifest)?;
        }
        if self.tail().active().is_ok() {
            // The log takes items, and every file that was made ready was
            // turned to before this could see it: committers fill the
            // files as fast as they are made. Nothing is wrong, and the
            // next call goes on.
            return Ok(turned);
        }
        // Something failed in every round. A last turn says what still
        // stands in the way, if it is not made.
        self.turn(false)?;
        if self.tail().active().is_ok() {
            return Ok(true);
        }
        Err(LogError::NoSpare)
    }

    /// Turns the log if it has no file that takes items and has a file to
    /// turn to.
    fn turn_if_ready(&self) -> Result<bool, LogError> {
        match self.turn(false) {
            Err(
                LogError::NoSpare | LogError::SpareNotNamed { .. } | LogError::CutNotDurable { .. },
            ) => Ok(false),
            turned => turned,
        }
    }

    /// The place the next commit group goes to: the end of the log now,
    /// with the address and the version the group is written with.
    ///
    /// The log sets nothing aside. A group is appended at the place it was
    /// written for or not at all, so whoever asks keeps every other append
    /// out until its group is in: the commit lock keeps out other commits,
    /// and the lock on the counts of live values keeps out the page steps
    /// and the checkpoint mark, which are made under it (LT7 of
    /// `transactions.md`). A place that is not used needs no giving back.
    pub fn next_group(&self) -> Result<GroupPlace, LogError> {
        let tail = self.tail();
        let (file, cursor) = tail.active()?;
        let offset = cursor.offset();
        let place =
            LogPlace::new(file.id(), offset).map_err(LogError::format(file.path(), offset))?;
        let address = file
            .start()
            .ok_or(FormatError::Wal(WalError::MissingStart))
            .and_then(|start| match has_address(&start, offset) {
                true => start.address(offset),
                false => Err(WalError::AddressOutOfRange.into()),
            })
            .map_err(LogError::format(file.path(), offset))?;
        let version = tail.latest.next().ok_or(LogError::Refused {
            offset: 0,
            source: WalError::VersionGap {
                after: tail.latest,
                found: tail.latest,
            }
            .into(),
        })?;
        Ok(GroupPlace {
            session: self.session,
            turn: tail.turn,
            place,
            address,
            version,
            cursor: cursor.clone(),
        })
    }

    /// Appends the group of a commit, which was written for the place
    /// `at`, in one append to the log file. Nothing is synced.
    ///
    /// `group` is the bytes of one whole group as
    /// [`GroupWriter`](lignin_format::wal::GroupWriter) writes it, with the
    /// version and the address of `at`. It is read as recovery will read
    /// it before it is appended, and bytes that recovery would not take
    /// for the next group of the log are refused and not appended
    /// ([`LogError::Refused`], [`LogError::WrongItem`],
    /// [`LogError::NotOneItem`]).
    ///
    /// If the log no longer ends at `at`, because something else was
    /// appended or the log turned to another file, nothing is appended
    /// ([`LogError::PlaceTaken`]).
    ///
    /// When this returns `Ok`, the group is in the log: recovery takes the
    /// commit whenever these bytes survive, and everything before them in
    /// the log. When it returns [`LogError::NotAppended`], the group is not
    /// in the log and no crash brings it back: the commit has failed. When
    /// it returns [`LogError::TakenBack`], the group is not in the log, and
    /// a crash can still bring it back until [`Self::settle`] has returned
    /// `Ok`: then the commit has failed. When it returns
    /// [`LogError::Unsettled`], neither is known.
    pub fn append_group(&self, at: &GroupPlace, group: &[u8]) -> Result<Appended, LogError> {
        if at.session != self.session {
            return Err(LogError::WrongStore);
        }
        let after = check_item(&at.cursor, group, ItemKind::Group)?;
        let mut tail = self.tail();
        let still_there = tail.turn == at.turn
            && matches!(tail.active(), Ok((_, cursor)) if *cursor == at.cursor);
        if !still_there {
            tail.check_settled()?;
            return Err(LogError::PlaceTaken {
                expected: at.place,
                found: tail.active().ok().and(tail.end()),
            });
        }
        self.append_checked(&mut tail, group, after, ItemKind::Group)
    }

    /// Appends the record of a background change: of one page step that
    /// places or moves a value, or that removes what a dropped subtree
    /// left. One append to the log file, and nothing is synced.
    ///
    /// `record` is the bytes of one record as
    /// [`Change::write`](lignin_format::wal::Change::write) writes it. A
    /// page step calls this under its own lock, and installs its change
    /// only when this has returned `Ok`: the record is then in the log
    /// where the step happened.
    ///
    /// It is refused, and the outcome of a failed append is told, as for
    /// [`Self::append_group`].
    pub fn append_change(&self, record: &[u8]) -> Result<Appended, LogError> {
        self.append_single(record, ItemKind::Change)
    }

    /// Appends the mark of a page-table checkpoint that will cover the
    /// commit version `covers`. One append to the log file, and nothing is
    /// synced.
    ///
    /// The place of the sample of the counts that the checkpoint carries
    /// is the end of its mark (LG29): [`Appended::end`]. So the mark is
    /// appended while the lock on the counts is held, with the sample.
    ///
    /// It is refused, and the outcome of a failed append is told, as for
    /// [`Self::append_group`].
    pub fn append_mark(&self, covers: CommitVersion) -> Result<Appended, LogError> {
        let mut record = Vec::with_capacity(CHECKPOINT_MARK_LEN);
        CheckpointMark::new(covers)
            .write(&mut record)
            .map_err(|source| LogError::Refused { offset: 0, source })?;
        self.append_single(&record, ItemKind::Mark)
    }

    fn append_single(&self, record: &[u8], kind: ItemKind) -> Result<Appended, LogError> {
        let mut tail = self.tail();
        let (_, cursor) = tail.active()?;
        let after = check_item(cursor, record, kind)?;
        self.append_checked(&mut tail, record, after, kind)
    }

    /// Appends `bytes`, which `after` has read as one item behind the end
    /// of the log.
    fn append_checked(
        &self,
        tail: &mut Tail,
        bytes: &[u8],
        after: LogCursor,
        kind: ItemKind,
    ) -> Result<Appended, LogError> {
        let (file, before) = tail.active()?;
        let (file, before) = (file.clone(), before.clone());
        let offset = before.offset();
        let refused = |at: u64| LogError::format(file.path(), at);
        let appended = Appended {
            at: LogPlace::new(file.id(), offset).map_err(refused(offset))?,
            end: LogPlace::new(file.id(), after.offset()).map_err(refused(after.offset()))?,
        };
        if !file
            .start()
            .is_some_and(|start| has_address(&start, offset))
        {
            return Err(refused(offset)(WalError::AddressOutOfRange.into()));
        }

        let error = match file.append(bytes) {
            Ok(found) if found == offset => {
                self.took(tail, &file, after);
                self.appended.count(kind, bytes.len());
                return Ok(appended);
            }
            Ok(found) => {
                let error = io::Error::other(format!(
                    "an append landed at offset {found} where the log ends at {offset}"
                ));
                return Err(self.unsettle(tail, &file, error));
            }
            Err(error) => error,
        };
        let verifies = |found: &[u8]| {
            let mut cursor = before.clone();
            let mut at = 0;
            while let Ok(step) = cursor.step(&found[at..]) {
                if step.ends_item {
                    return true;
                }
                at += step.len;
            }
            false
        };
        match settle(&file, offset, bytes, verifies) {
            Settled::InLog => {
                self.took(tail, &file, after);
                self.appended.count(kind, bytes.len());
                self.finish(&file);
                Ok(appended)
            }
            Settled::Absent => {
                file.failed(true);
                self.finish(&file);
                Err(LogError::TakenBack {
                    path: file.path().to_path_buf(),
                    source: error,
                })
            }
            Settled::Torn => {
                file.failed(false);
                self.finish(&file);
                Err(LogError::NotAppended {
                    path: file.path().to_path_buf(),
                    source: error,
                })
            }
            Settled::Unknown(why) => Err(self.unsettle(tail, &file, why)),
        }
    }

    /// What this log has appended since it was opened.
    pub fn appended(&self) -> LogAppends {
        self.appended.read()
    }

    fn took(&self, tail: &mut Tail, file: &Arc<LogFile>, after: LogCursor) {
        file.appended(after.offset());
        tail.latest = after.latest();
        self.due.store(
            after.offset() >= self.options.target_file_len,
            Ordering::Release,
        );
        tail.active = Some((file.clone(), after));
    }

    /// The file takes no more items: the log goes on in the next one.
    fn finish(&self, file: &LogFile) {
        file.finish();
        self.due.store(true, Ordering::Release);
    }

    fn unsettle(&self, tail: &mut Tail, file: &LogFile, error: io::Error) -> LogError {
        tail.unsettled = Some(Sticky::of(file.path(), &error));
        LogError::Unsettled {
            path: file.path().to_path_buf(),
            source: error,
        }
    }

    /// Makes durable every item that was appended before this was called,
    /// and returns how far the log is durable. It also makes durable the
    /// end of a file that a failed append left with the length it had, so
    /// that no crash brings that item back.
    ///
    /// It may run while items are appended, and no append waits for it.
    /// One sync runs at a time. The files are synced in the order of the
    /// log, each file of an earlier session and each file the log has
    /// turned away from before the one the log ends in: an item is durable
    /// only when every item before it is, because a log ends at its first
    /// item that is not whole.
    ///
    /// If the file layer fails a sync, the items of that file that were
    /// not durable yet are unknown, and no later sync makes them good
    /// (`lignin_io::vfs`). They cannot be written again either: a group is
    /// written for one place. So this call and every later one fail with
    /// [`LogError::SyncFailed`], and nothing behind what was durable then
    /// becomes durable in this session. The log still takes items. What a
    /// crash keeps of them is settled by the next open.
    pub fn sync(&self) -> Result<LogDurable, LogError> {
        self.sync_until(None)
    }

    /// Sees to it that no crash brings back an item whose append returned
    /// [`LogError::TakenBack`]. Call it after that error, outside every
    /// page step's lock, before anything relies on the item being gone: a
    /// commit calls it before it reports that it failed.
    ///
    /// When it returns `Ok`, the item is out of the log for good: a sync
    /// has made the file durable as the append left it, or a manifest
    /// names the file closed. With nothing waiting it does nothing.
    ///
    /// When the sync fails, nothing keeps a crash from bringing the item
    /// back. The append is unsettled, this returns
    /// [`LogError::Unsettled`], and the log takes nothing more in this
    /// session. It returns another error only when it could not try, as
    /// when a file of an earlier session cannot be written again: it can
    /// then be called again.
    pub fn settle(&self) -> Result<(), LogError> {
        if !self.waits_for_sync() {
            return Ok(());
        }
        let Err(error) = self.sync() else {
            return Ok(());
        };
        self.tail().check_settled()?;
        // A manifest that names the file closed settles it as well.
        if self.waits_for_sync() {
            Err(error)
        } else {
            Ok(())
        }
    }

    /// Makes the log durable through `place`: every item that ends at or
    /// before it. Returns how far the log is durable then, which may be
    /// further.
    ///
    /// If the log is durable through `place` already, nothing is synced.
    /// So of several threads that wait for their own items, one syncs for
    /// all that were appended before it began. It fails as [`Self::sync`]
    /// does, and for a place behind the end of the log, where nothing was
    /// appended ([`LogError::BeyondEnd`]).
    pub fn sync_through(&self, place: LogPlace) -> Result<LogDurable, LogError> {
        self.sync_until(Some(place))
    }

    fn sync_until(&self, through: Option<LogPlace>) -> Result<LogDurable, LogError> {
        let _one_at_a_time = lock(&self.syncing);
        let failed = {
            let files = self.files();
            let failed = files.sync_failed.as_ref();
            failed.map(|failed| (failed.path.clone(), failed.error()))
        };
        if let Some((path, source)) = failed {
            return Err(self.sync_failed(&path, source));
        }
        let (end, latest) = {
            let tail = self.tail();
            (tail.end(), tail.latest)
        };
        if let Some(place) = through.filter(|&place| Some(place) > end) {
            return Err(LogError::BeyondEnd { place, end });
        }
        let targets: Vec<(Arc<LogFile>, u64)> = {
            let files = self.files();
            let targets: Vec<_> = files
                .live
                .values()
                .filter(|entry| entry.file.start().is_some())
                .filter_map(|entry| {
                    let file_end = match end {
                        Some(end) if entry.file.id() > end.file() => return None,
                        Some(end) if entry.file.id() == end.file() => end.offset(),
                        _ => entry.file.end(),
                    };
                    (entry.durable_len < file_end || entry.file.is_taken_back())
                        .then(|| (entry.file.clone(), file_end))
                })
                .collect();
            let covered = |place| files.durable.place.is_some_and(|durable| durable >= place);
            let cut_waits = targets.iter().any(|(file, _)| file.is_taken_back());
            if through.is_some_and(covered) && !cut_waits {
                return Ok(files.durable);
            }
            targets
        };
        for (file, file_end) in targets {
            let taken_back = file.is_taken_back();
            if file.is_adopted() {
                self.write_again(&file, file_end)?;
            } else if let Err(source) = file.sync() {
                self.files_mut().sync_failed = Some(Sticky::of(file.path(), &source));
                return Err(self.sync_failed(file.path(), source));
            }
            if taken_back {
                file.made_durable();
            }
            if let Some(entry) = self.files_mut().live.get_mut(&file.id()) {
                entry.durable_len = entry.durable_len.max(file_end);
            }
        }

        let mut files = self.files_mut();
        if end >= files.durable.place {
            files.durable = LogDurable {
                version: latest,
                place: end,
            };
        }
        files.last_sync = Instant::now();
        Ok(files.durable)
    }

    /// The error of a sync that failed, now or earlier in the session. An
    /// append that was taken back and still waits for a sync is unsettled
    /// from here on: nothing can keep a crash from bringing it back.
    fn sync_failed(&self, path: &Path, source: io::Error) -> LogError {
        let mut tail = self.tail();
        let waits = matches!(&tail.active, Some((file, _)) if file.is_taken_back());
        if waits && tail.unsettled.is_none() {
            tail.unsettled = Some(Sticky::of(path, &source));
        }
        LogError::SyncFailed {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Makes durable a file of an earlier session whose end the manifest
    /// does not vouch for, by writing the file again: its bytes up to `end`
    /// go into a new file, which is synced and then takes the file's name.
    ///
    /// A sync of the file itself would not do. If a sync of it failed in the
    /// session that wrote it, and no crash came since, a sync that succeeds
    /// now covers nothing of what was appended before that failure
    /// (`lignin_io::vfs`), and this session cannot know. Bytes that are
    /// written again are covered. The file is not written in place and not
    /// appended to (B11): the name gets a new file with the same bytes, and
    /// whoever shares the old one as a hard link keeps it.
    ///
    /// The log goes on reading the file through the handle it has.
    fn write_again(&self, file: &LogFile, end: u64) -> Result<(), LogError> {
        let temporary = temporary_path(file.path());
        remove(&*self.vfs, &temporary).map_err(LogError::io("remove", &temporary))?;
        self.copy(file, end, &temporary).inspect_err(|_| {
            let _ = self.vfs.remove_file(&temporary);
        })?;
        self.vfs
            .rename(&temporary, file.path())
            .map_err(LogError::io("rename", &temporary))?;
        let wal_dir = FileNames::WAL.dir(&self.dir);
        self.vfs
            .sync_dir(&wal_dir)
            .map_err(LogError::io("sync the directory", &wal_dir))
    }

    fn copy(&self, file: &LogFile, end: u64, to: &Path) -> Result<(), LogError> {
        let copy = self
            .vfs
            .open(to, OpenMode::Append(Create::New))
            .map_err(LogError::io("create", to))?;
        let mut chunk = vec![0; end.min(COPY_CHUNK_LEN) as usize];
        let mut at = 0;
        while at < end {
            let len = (end - at).min(COPY_CHUNK_LEN) as usize;
            file.read_exact_at(&mut chunk[..len], at)
                .map_err(LogError::io("read", file.path()))?;
            copy.append(&chunk[..len])
                .map_err(LogError::io("append to", to))?;
            at += len as u64;
        }
        copy.sync_all().map_err(LogError::io("sync", to))
    }

    /// Makes the log durable for the commit whose group is `appended`, if
    /// the [`SyncPolicy`] asks for it: this is the fsync of a commit (step
    /// 6 of CM3 in `transactions.md`). Returns how far the log is durable
    /// if it synced, and `None` if the policy asked for no sync.
    ///
    /// The append and this wait are two calls, so that a caller which must
    /// not wait for a sync, such as a page step, makes only the first.
    ///
    /// If it fails, the group is in the log and is not known to be
    /// durable: the commit has happened and ends as `NotDurable`.
    pub fn sync_for_commit(&self, appended: &Appended) -> Result<Option<LogDurable>, LogError> {
        match self.options.sync {
            SyncPolicy::EveryCommit => self.sync_through(appended.end).map(Some),
            _ => self.sync_if_due(),
        }
    }

    /// Syncs the log if the [`SyncPolicy`] has a sync due now: with
    /// [`SyncPolicy::Interval`], when the last sync is at least the
    /// interval ago. Returns how far the log is durable if it synced.
    ///
    /// A commit does this for itself ([`Self::sync_for_commit`]). A timer
    /// calls it so that the last commits before a pause do not wait for
    /// the next commit to become durable.
    pub fn sync_if_due(&self) -> Result<Option<LogDurable>, LogError> {
        let since_sync = self.files().last_sync.elapsed();
        if self.options.sync.is_due(since_sync) {
            self.sync().map(Some)
        } else {
            Ok(None)
        }
    }

    /// How far the log is durable: through which commit, and up to which
    /// place. It is what a successful sync last returned, and before the
    /// first one, how far the manifest the log was opened from vouches for
    /// it.
    pub fn durable(&self) -> LogDurable {
        self.files().durable
    }

    /// Reads the pending value `pending`, for the cell of the page `owner`
    /// that holds it, into `buf`: the value's bytes, or the name of the
    /// record in the value log that holds them.
    ///
    /// The 8 bytes of a pending value are the log address of the value's
    /// first record (LG21). The file is the one whose addresses hold it,
    /// found among the handful of files the log has, and the read is one
    /// positional read. What stands at the address is verified: it must be
    /// of the pending value's length and must name `owner`.
    ///
    /// A value in a file that was dropped ([`Self::drop_before`]) is not
    /// read ([`LogError::UnknownAddress`]).
    pub fn read_pending<'a>(
        &self,
        pending: &PendingValue,
        owner: PageId,
        buf: &'a mut Vec<u8>,
    ) -> Result<LoggedValue<'a>, LogError> {
        let address =
            LogAddress::of(pending).map_err(|source| LogError::Refused { offset: 0, source })?;
        let (file, offset) = {
            let files = self.files();
            let holds = |entry: &Entry| {
                let offset = entry.file.start()?.offset_of(address)?;
                (offset < entry.file.end()).then(|| (entry.file.clone(), offset))
            };
            files.live.values().find_map(holds)
        }
        .ok_or(LogError::UnknownAddress { address })?;
        let extent = wal::logged_read_len(pending.value_len(), file.version());
        let extent = extent.min(file.end() - offset);
        let len = usize::try_from(extent).map_err(|_| LogError::ValueTooLong { extent })?;
        buf.clear();
        buf.resize(len, 0);
        file.read_exact_at(buf, offset)
            .map_err(LogError::io("read", file.path()))?;
        match file.version() {
            FormatVersion::V1 => {}
        }
        wal::read_pending_value(pending, buf, owner, file.version())
            .map_err(LogError::format(file.path(), offset))
    }

    /// Keeps the log from `place` on, for as long as the hold lives: for a
    /// page-table checkpoint whose sample was taken there. `None` keeps the
    /// whole log, as it is kept while no checkpoint is in force.
    ///
    /// [`Self::drop_before`] then takes out no file from the file of the
    /// place on: also not when the place is that file's end and every
    /// commit in the file is covered. A recovery from the checkpoint reads
    /// that file from its start to find the mark, and takes the background
    /// changes and the count changes from the place on (LG29). See
    /// [`LogHold`] for who takes a hold, and when.
    ///
    /// The file of the place must be in the log
    /// ([`LogError::UnknownFile`]): a hold cannot bring back what was
    /// dropped.
    pub fn hold_from(&self, place: Option<LogPlace>) -> Result<LogHold, LogError> {
        let files = self.files();
        if let Some(place) = place {
            let held = files.live.get(&place.file());
            if !held.is_some_and(|entry| entry.file.start().is_some()) {
                return Err(LogError::UnknownFile { file: place.file() });
            }
        }
        let id = self.holds.taken.fetch_add(1, Ordering::Relaxed);
        lock(&self.holds.kept).insert(id, place);
        Ok(LogHold {
            holds: self.holds.clone(),
            id,
            place,
        })
    }

    /// Takes out of the log every file that lies wholly before the file of
    /// `keep_from`, and returns their ids. Such a file is no longer read,
    /// and [`Self::describe`] no longer lists it. Its file is deleted by
    /// [`Self::manifest_replaced`], once the durable manifest does not
    /// name it.
    ///
    /// `keep_from` is the earliest place of the log that anything still
    /// needs (FL6 of `transactions.md`): the group after the commit the
    /// table checkpoint in force covers, the place of that checkpoint's
    /// sample, whose file is kept also when the mark is the last item in
    /// it (LG29), and every pending value. The caller vouches for it. The
    /// file of `keep_from` and every later one stay.
    ///
    /// So does every file from the file of a [`LogHold`] on, whatever
    /// `keep_from` is: with a hold on the whole log, nothing is taken out.
    ///
    /// The log must be durable up to `keep_from` ([`LogError::NotDurable`]):
    /// sync it first. What is kept then begins with a file whose start
    /// record no crash takes away, so the log that is left never reads as
    /// a log with no file, which would begin again at version 0. Every
    /// file before a durable place takes no more items and is durable to
    /// its end, and the files a manifest names are always a run of the log
    /// with nothing missing in it.
    pub fn drop_before(&self, keep_from: LogPlace) -> Result<Vec<FileId>, LogError> {
        let mut files = self.files_mut();
        let keep = keep_from.file();
        let kept = files.live.get(&keep);
        if !kept.is_some_and(|entry| entry.file.start().is_some()) {
            return Err(LogError::UnknownFile { file: keep });
        }
        if Some(keep_from) > files.durable.place {
            return Err(LogError::NotDurable {
                place: keep_from,
                durable: files.durable.place,
            });
        }
        // Read under the lock on the files, so that a hold that was taken
        // on a file which is still in the log keeps it.
        let held = lock(&self.holds.kept).values().copied().min();
        let keep = match held {
            Some(None) => return Ok(Vec::new()),
            Some(Some(held)) => keep.min(held.file()),
            None => keep,
        };
        let Files { live, dropped, .. } = &mut *files;
        let mut taken = Vec::new();
        while let Some(first) = live.first_entry() {
            let entry = first.get();
            if *first.key() >= keep || !(entry.is_closed() || entry.file.is_dead()) {
                break;
            }
            let (id, entry) = first.remove_entry();
            if !entry.file.is_dead() {
                taken.push(id);
            }
            dropped.insert(id, entry.file.path().to_path_buf());
        }
        Ok(taken)
    }

    /// The files of the log, as a manifest written now would name them:
    /// each with its state and the length of it that is durable.
    pub fn files_named(&self) -> Result<Vec<LiveFile>, LogError> {
        self.files().named()
    }

    /// The manifest `base` with its log files replaced by the ones the log
    /// has, and its last log file id raised to the last one used here.
    ///
    /// A file is named up to what was synced of it. It is named closed
    /// once it takes no more items and all of it is durable, and open
    /// until then: the file the log ends in, the file it turns to next,
    /// and a file whose end is not synced yet. A file of the manifest that
    /// holds nothing of the log is not named. Everything else in `base` is
    /// kept.
    pub fn describe(&self, base: &Manifest) -> Result<Manifest, LogError> {
        let mut named: Vec<LiveFile> = base
            .files()
            .iter()
            .filter(|file| file.kind() != FileKind::Wal)
            .copied()
            .collect();
        let last_id = {
            let files = self.files();
            named.extend(files.named()?);
            files.last_id
        };
        let mut last_file_ids = base.last_file_ids();
        last_file_ids.wal = last_file_ids.wal.max(last_id);
        Manifest::new(
            base.page_size(),
            last_file_ids,
            base.table_checkpoint(),
            named,
        )
        .map_err(LogError::format(
            &self.dir.join(ManifestStore::FILE_NAME),
            0,
        ))
    }

    /// Tells the log that the manifest was replaced. Call it after every
    /// [`ManifestStore::update`] that returned `Ok`.
    ///
    /// A new file that the durable manifest names can be turned to from
    /// now on. If the manifest no longer names the file the log would have
    /// turned to, that file is given up, and the log needs another. The
    /// file of a dropped log file that the durable manifest no longer
    /// names is deleted.
    ///
    /// A file that cannot be deleted does not stop the rest: the call ends
    /// with [`LogError::NotDeleted`], and the next call tries the deletion
    /// again.
    pub fn manifest_replaced(&self, manifest: &ManifestStore) -> Result<(), LogError> {
        self.check_same_dir(manifest)?;
        let current = manifest.manifest()?;
        let named: BTreeSet<FileId> = log_files(current).map(LiveFile::id).collect();
        {
            let files = self.files();
            let closed = log_files(current).filter(|file| file.state() == FileState::Closed);
            for entry in closed.filter_map(|file| files.live.get(&file.id())) {
                entry.file.cut();
            }
        }
        {
            // A file that a manifest has stopped naming is given up: no
            // manifest may name it again.
            let mut tail = self.tail();
            tail.spare = tail.spare.take().and_then(|(spare, was_named)| {
                let is_named = named.contains(&spare.id());
                if was_named && !is_named {
                    spare.kill();
                    return None;
                }
                Some((spare, is_named))
            });
        }

        let unnamed: BTreeMap<FileId, PathBuf> = {
            let mut files = self.files_mut();
            let Files { live, dropped, .. } = &mut *files;
            live.retain(|&id, entry| {
                let dead = entry.file.is_dead();
                if dead {
                    dropped.insert(id, entry.file.path().to_path_buf());
                }
                !dead
            });
            let (kept, gone) = std::mem::take(dropped)
                .into_iter()
                .partition(|(id, _)| named.contains(id));
            *dropped = kept;
            gone
        };
        let mut result = Ok(());
        for (id, path) in unnamed {
            if let Err(source) = remove(&*self.vfs, &path) {
                self.files_mut().dropped.insert(id, path.clone());
                result = result.and(Err(LogError::NotDeleted { path, source }));
            }
        }
        result
    }

    /// Replaces the manifest with one that names the log's files as they
    /// are now: [`ManifestStore::update`] with [`Self::describe`], then
    /// [`Self::manifest_replaced`].
    ///
    /// After [`LogError::NotDeleted`] the new manifest is in force. After
    /// any other error [`ManifestStore::manifest`] tells where things
    /// stand: it gives the old manifest, and this call can be made again,
    /// or it fails with `ManifestInDoubt`, and the directory has the old
    /// manifest or the new one.
    pub fn commit(&self, manifest: &mut ManifestStore) -> Result<(), LogError> {
        self.check_same_dir(manifest)?;
        manifest.update(|current| self.describe(current))?;
        self.manifest_replaced(manifest)
    }
}
