//! The source of a migration: the files of a RocksDB GroveDB, read and
//! never written.
//!
//! # How the source is opened
//!
//! With RocksDB's read-only open (`DB::open_cf_for_read_only`). That open
//! takes no lock, writes no file, and replays a write-ahead log that the
//! node left into memory only. GroveDB's own opens are not used: each of
//! them, the open of a checkpoint among them, is an open for writing, which
//! recovers the log into new files and replaces the manifest.
//!
//! **A source that a process has open for writing is refused.** RocksDB
//! holds a lock on the file `LOCK` of a database that is open for writing.
//! Before the open, the migrator asks for a shared lock on that file,
//! which it is refused while such a process lives ([`Source::open`]), and
//! keeps it until the migration ends, so that no node starts on the source
//! meanwhile. The lock is shared: two migrations can read one source.
//!
//! A directory with no `LOCK` file has nothing to lock. A checkpoint is
//! such a directory until a process opens it for writing, which makes the
//! file. The migrator reads it, and says that nothing keeps a process out
//! of it. Where the file is there once the source is open, a process came
//! to write between the two looks, and the source is refused. A process
//! that comes later changes the directory, which the migration looks at
//! once more when it has compared the target ([`Source::unchanged`]); one
//! that comes after that look is not seen.
//!
//! **A `LOCK` file that is replaced while a writer runs says nothing of
//! that writer**: its lock is on the file that was there, and RocksDB
//! itself would start a second writer on such a directory. So the node
//! is stopped, and nothing else touches the source during the run. The
//! probe holds the name to the file it locked, once it has the lock and
//! again once the database is open, and what such a writer changes is
//! found as it is for a directory with no `LOCK` file.
//!
//! **Whose the lock is.** RocksDB's lock is its process's: the system
//! gives a process one such lock on a file, whichever descriptor asked
//! for it, and takes it away when the process closes any descriptor of
//! the file. On Linux the migrator's lock is of the other kind, which
//! belongs to the one open of the file that asked for it (an open file
//! description lock). A writer's lock keeps it out and it keeps a writer
//! out, also where the two are in one process, and two [`Source`]s of one
//! process each keep a lock of their own. A descriptor whose lock was
//! refused is not closed then, since its close would give up the lock of
//! a writer in the same process: it is kept, and the next probe of that
//! file asks through it. Where that probe gets the lock, the descriptor
//! is the [`Source`]'s and is closed with it; no writer of the process
//! holds a lock on the file by then, since this lock kept it out.
//!
//! On a system other than Linux the migrator's lock is its process's
//! too. There a caller of the library must not have the source open in
//! its own process, for writing or through a second [`Source`]: the probe
//! would turn a writer's lock into a shared one, and the end of either
//! would release the lock of both.
//!
//! **A write-ahead log that was not flushed.** A node that was killed, and
//! one that was stopped, can leave its last commits in the log alone. The
//! read-only open replays them in memory: the migration is of the state the
//! node would recover to at its next start, and the log stays as it is.
//!
//! **A directory with an entry of LigninDB's is refused**, before anything
//! of it is opened or locked: GroveDB's look at the names of its entries
//! (`grovedb::engine::look_at_entries`), which the open that chooses the
//! engine asks too, is to say RocksDB or nothing. A RocksDB open of a
//! LigninDB directory makes a RocksDB database beside the LigninDB one,
//! which would be read in place of the state the directory holds; the
//! look refuses entries of both engines, and the source refuses a
//! directory that it says is LigninDB's.
//!
//! **The source is not to change while it is read.** [`Source::unchanged`]
//! compares the names, the lengths and the modification times of the
//! directory's entries with what they were at the open. RocksDB's info log
//! (`LOG`, `LOG.old.*`) is left out: it is no part of the database, and a
//! process whose open for writing is refused has begun a new one by then.
//!
//! # The layout the reader takes
//!
//! Four column families: the default one for the data keyspace, and `aux`,
//! `roots` and `meta`. In each, a key is the 32 bytes of a subtree's prefix
//! and then the key in that subtree. That is the layout of every GroveDB
//! that has those four families, which the releases since 1.0 have. A
//! database with another set of column families is refused.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::SystemTime,
};

use grovedb::engine::{look_at_entries, EngineKind};
use grovedb_storage::Error as StorageError;
use rocksdb::{ColumnFamily, DBRawIterator, Options, ReadOptions, DB};

use crate::ToolError;

/// The bytes of a subtree's prefix.
pub(crate) const PREFIX_LEN: usize = 32;

/// A record as a cursor shows it: its stored key and its value.
pub(crate) type Record<'a> = (&'a [u8], &'a [u8]);

/// The prefix of a subtree.
pub(crate) type Prefix = [u8; PREFIX_LEN];

/// The four keyspaces of a subtree, each a column family of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Keyspace {
    /// The nodes of a Merk, or the flat records of a tree that keeps
    /// those.
    Data,
    /// Auxiliary records.
    Aux,
    /// The record of a Merk's root key.
    Roots,
    /// Meta records.
    Meta,
}

impl Keyspace {
    /// Every keyspace.
    pub(crate) const ALL: [Self; 4] = [Self::Data, Self::Aux, Self::Roots, Self::Meta];

    /// Its name in a report.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Aux => "aux",
            Self::Roots => "roots",
            Self::Meta => "meta",
        }
    }

    /// The column family that holds it.
    const fn family(self) -> &'static str {
        match self {
            Self::Data => rocksdb::DEFAULT_COLUMN_FAMILY_NAME,
            Self::Aux => "aux",
            Self::Roots => "roots",
            Self::Meta => "meta",
        }
    }

    const fn at(self) -> usize {
        self as usize
    }
}

fn source_error(dir: &Path, what: impl std::fmt::Display) -> ToolError {
    ToolError::Migrate(format!("the source {}: {what}", dir.display()))
}

/// Refuses `dir` where GroveDB's look at its entries, which opens nothing,
/// says that it holds LigninDB's entries, alone or with RocksDB's, and
/// where it cannot list it. A directory that is not there, or in which no
/// entry says an engine, is left to the open.
fn of_rocksdb_alone(dir: &Path) -> Result<(), ToolError> {
    let words = match look_at_entries(dir) {
        Ok(None | Some((EngineKind::RocksDb, _))) => return Ok(()),
        Ok(Some((EngineKind::Lignin, entry))) => format!(
            "it holds `{}`, an entry that only a LigninDB database makes, and this command reads \
             a RocksDB database alone: nothing was opened or locked. Find out which database \
             there holds the node's state",
            entry.to_string_lossy()
        ),
        Err(StorageError::NotSupported(words) | StorageError::StorageError(words)) => words,
        Err(error) => error.to_string(),
    };
    Err(source_error(dir, words))
}

/// One entry of the source's directory: its name, its length and when it
/// was last modified.
type Listed = (std::ffi::OsString, u64, Option<SystemTime>);

/// Whether `name` is a file of RocksDB's info log, which every open for
/// writing begins anew before it asks for the lock.
fn is_info_log(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "LOG" || name.starts_with("LOG.old.")
}

/// The entries of `dir` but RocksDB's info log, in the order of their
/// names.
fn listing(dir: &Path) -> io::Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if is_info_log(&entry.file_name()) {
            continue;
        }
        let metadata = entry.metadata()?;
        listed.push((entry.file_name(), metadata.len(), metadata.modified().ok()));
    }
    listed.sort();
    Ok(listed)
}

/// The least key that is greater than every key which begins with
/// `prefix`, or `None` where no key is: `prefix` up to its last byte that
/// is not `0xff`, with that byte one more. It is as short as it can be,
/// so that no stored key lies between the keys of the prefix and it, also
/// not one that is shorter than a prefix.
pub(crate) fn after(prefix: &Prefix) -> Option<Vec<u8>> {
    let last = prefix.iter().rposition(|byte| *byte != 0xff)?;
    let mut bound = prefix[..=last].to_vec();
    bound[last] += 1;
    Some(bound)
}

/// The device and the number of a file: what a file is, whatever it is
/// named.
pub(crate) fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// The `LOCK` files whose lock this process was refused, each by what the
/// file is, with the descriptor that asked.
///
/// A descriptor is not closed while it is here. The system takes the
/// lock that RocksDB holds on a file away from its process when the
/// process closes any descriptor of the file, and the holder that refused
/// the lock may be this process. The next probe of the file takes the
/// descriptor from here and asks through it, so that a file has one of
/// them however often it is refused.
static REFUSED_LOCKS: Mutex<Vec<((u64, u64), File)>> = Mutex::new(Vec::new());

/// Asks for a shared lock on the whole of `file`, and does not wait for
/// it: an open file description lock, which belongs to the open of the
/// file that `file` is and not to the process.
#[cfg(target_os = "linux")]
fn lock_shared(file: &File) -> io::Result<()> {
    use nix::{
        fcntl::{fcntl, FcntlArg},
        libc,
    };
    let whole = libc::flock {
        l_type: libc::F_RDLCK as libc::c_short,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    fcntl(file, FcntlArg::F_OFD_SETLK(&whole))?;
    Ok(())
}

/// Asks for a shared lock on the whole of `file`, and does not wait for
/// it: the process's lock, where the system has no other.
#[cfg(not(target_os = "linux"))]
fn lock_shared(file: &File) -> io::Result<()> {
    rustix::fs::fcntl_lock(file, rustix::fs::FlockOperation::NonBlockingLockShared)?;
    Ok(())
}

/// Whether `path` names the file that `file` is an open of: a name can
/// be given to another file between two looks at it.
fn still_named(path: &Path, file: &File) -> bool {
    match (std::fs::metadata(path), file.metadata()) {
        (Ok(named), Ok(open)) => file_identity(&named) == file_identity(&open),
        _ => false,
    }
}

/// Fails where the `LOCK` file of `dir` is not what the probe found: a
/// file where it found none, or another file than the one it locked. A
/// process came to write the directory since.
fn no_writer_came(dir: &Path, lock: &Option<File>) -> Result<(), ToolError> {
    let path = dir.join("LOCK");
    let as_found = match lock {
        None => !path.exists(),
        Some(lock) => still_named(&path, lock),
    };
    match as_found {
        true => Ok(()),
        false => Err(source_error(
            dir,
            "a process opened it for writing while this command began to read it. A migration \
             is made offline: stop the node, or migrate a checkpoint that nothing opens",
        )),
    }
}

/// What identifies the state of a source: the identity RocksDB gave the
/// database when it was made, and the number of the last write it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    /// RocksDB's identity of the database.
    pub(crate) identity: Vec<u8>,
    /// The sequence number of the last write.
    pub(crate) sequence: u64,
}

/// A RocksDB GroveDB that is open for reading.
pub(crate) struct Source {
    db: DB,
    dir: PathBuf,
    opened_as: Vec<Listed>,
    /// The shared lock on the directory's `LOCK` file, where it has one.
    /// It is released when the file is closed, which is when the source
    /// is.
    lock: Option<File>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source").field("dir", &self.dir).finish()
    }
}

impl Source {
    /// Opens the database in `dir` for reading.
    ///
    /// It is refused where `dir` holds an entry of LigninDB's, which is
    /// found before anything is opened or locked, where it holds no RocksDB
    /// database, where a process has the database open for writing, and
    /// where its column families are not the four of a GroveDB.
    pub(crate) fn open(dir: &Path) -> Result<Self, ToolError> {
        of_rocksdb_alone(dir)?;
        if !dir.join("CURRENT").is_file() {
            return Err(source_error(
                dir,
                "it holds no RocksDB database (no file CURRENT)",
            ));
        }
        let lock = Self::probe(dir)?;
        let opened_as = listing(dir).map_err(|error| source_error(dir, error))?;

        let options = Options::default();
        let families = DB::list_cf(&options, dir).map_err(|error| source_error(dir, error))?;
        let mut expected: Vec<&str> = Keyspace::ALL
            .iter()
            .map(|keyspace| keyspace.family())
            .collect();
        expected.sort_unstable();
        let mut found: Vec<&str> = families.iter().map(String::as_str).collect();
        found.sort_unstable();
        if found != expected {
            return Err(source_error(
                dir,
                format!(
                    "its column families are {found:?}, and a GroveDB that this command reads \
                     has {expected:?}"
                ),
            ));
        }
        let mut options = Options::default();
        // A node keeps every table file open; a tool of one pass does not
        // need to, and a process is given 1,024 descriptors on many
        // systems.
        options.set_max_open_files(256);
        let db = DB::open_cf_for_read_only(&options, dir, expected, false)
            .map_err(|error| source_error(dir, error))?;
        no_writer_came(dir, &lock)?;
        Ok(Self {
            db,
            dir: dir.to_path_buf(),
            opened_as,
            lock,
        })
    }

    /// Asks for a shared lock on the `LOCK` file of `dir`, which a process
    /// that has the database open for writing holds exclusively, and keeps
    /// it. `None` where the directory has no such file.
    fn probe(dir: &Path) -> Result<Option<File>, ToolError> {
        let path = dir.join("LOCK");
        let failed = |error: io::Error| source_error(dir, format!("its LOCK file: {error}"));
        // One probe at a time in the process: a file whose lock was
        // refused has one descriptor, and no second is opened beside it.
        let mut refused = REFUSED_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
        // RocksDB's lock file is a file. Anything else of that name is not
        // opened: the open of a pipe waits for its other end, with every
        // other probe of the process behind it.
        let no_file = || source_error(dir, "its LOCK is no file: it is no RocksDB database");
        let identity = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => file_identity(&metadata),
            Ok(_) => return Err(no_file()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(failed(error)),
        };
        let kept = refused.iter().position(|(kept, _)| *kept == identity);
        let file = match kept {
            Some(kept) => refused.swap_remove(kept).1,
            None => {
                // The name can be given to something else since the look
                // at what it names: an open that does not wait.
                use std::os::unix::fs::OpenOptionsExt;
                let no_wait = rustix::fs::OFlags::NONBLOCK.bits() as i32;
                let mut options = std::fs::OpenOptions::new();
                match options.read(true).custom_flags(no_wait).open(&path) {
                    Ok(file) if file.metadata().is_ok_and(|opened| opened.is_file()) => file,
                    Ok(_) => return Err(no_file()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(failed(error)),
                }
            }
        };
        let error = match lock_shared(&file) {
            // The lock is on the file that was opened, or that was kept
            // from an earlier probe. Where the name was given to another
            // file since the look at what it names, that file is the one
            // a writer locks.
            Ok(()) if still_named(&path, &file) => return Ok(Some(file)),
            Ok(()) => io::Error::from(io::ErrorKind::WouldBlock),
            Err(error) => error,
        };
        // The descriptor is kept, whatever refused the lock: its close
        // would give up a lock that this process holds on the file.
        let identity = (file.metadata()).map_or(identity, |metadata| file_identity(&metadata));
        refused.push((identity, file));
        let held = matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied
        );
        Err(match held {
            true => source_error(
                dir,
                "a process has it open. A migration is made offline: stop the node, or migrate \
                 a checkpoint of it",
            ),
            false => failed(error),
        })
    }

    /// The directory.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the open took a lock that keeps a node from starting on the
    /// source: `false` for a directory with no `LOCK` file.
    pub(crate) fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// What identifies the state that is read.
    pub(crate) fn fingerprint(&self) -> Result<Fingerprint, ToolError> {
        let identity = self.db.get_db_identity();
        Ok(Fingerprint {
            identity: identity.map_err(|error| source_error(&self.dir, error))?,
            sequence: self.db.latest_sequence_number(),
        })
    }

    /// Fails if an entry of the directory is not what it was at the open:
    /// something wrote the source while it was read.
    pub(crate) fn unchanged(&self) -> Result<(), ToolError> {
        let now = listing(&self.dir).map_err(|error| source_error(&self.dir, error))?;
        if now == self.opened_as {
            return Ok(());
        }
        let differs = |of: &[Listed], other: &[Listed]| -> Vec<String> {
            let changed = of.iter().filter(|entry| !other.contains(entry));
            changed
                .map(|(name, ..)| name.to_string_lossy().into_owned())
                .collect()
        };
        let mut names = differs(&now, &self.opened_as);
        names.extend(differs(&self.opened_as, &now));
        names.sort();
        names.dedup();
        Err(source_error(
            &self.dir,
            format!(
                "it changed while it was read ({}): something wrote it. Nothing of this run is \
                 to be used",
                names.join(", ")
            ),
        ))
    }

    fn family(&self, keyspace: Keyspace) -> &ColumnFamily {
        let family = self.db.cf_handle(keyspace.family());
        family.expect("the four column families were opened")
    }

    fn failed(&self, keyspace: Keyspace, error: impl std::fmt::Display) -> ToolError {
        source_error(
            &self.dir,
            format!("a read of its {} keyspace: {error}", keyspace.name()),
        )
    }

    /// The record under `key` of `prefix` in `keyspace`.
    pub(crate) fn get(
        &self,
        keyspace: Keyspace,
        prefix: &Prefix,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, ToolError> {
        let stored = [prefix.as_slice(), key].concat();
        let found = self.db.get_cf(self.family(keyspace), stored);
        found.map_err(|error| self.failed(keyspace, error))
    }

    /// A cursor over `keyspace`, for a walk in the order of the keys: of
    /// the keys below `upper`, or of the whole keyspace.
    fn cursor(&self, keyspace: Keyspace, upper: Option<Vec<u8>>) -> DBRawIterator<'_> {
        let mut options = ReadOptions::default();
        // A walk reads each block once: it is not kept for a second read.
        options.fill_cache(false);
        match upper {
            // The records of one subtree: RocksDB reads ahead by itself
            // once a cursor has gone on for a few blocks, which a subtree
            // of two nodes never does.
            Some(upper) => options.set_iterate_upper_bound(upper),
            None => options.set_readahead_size(2 << 20),
        }
        self.db.raw_iterator_cf_opt(self.family(keyspace), options)
    }

    /// Calls `each` with every record of `prefix` in `keyspace`, in the
    /// order of the keys: the key in the subtree, and the value. A walk
    /// that RocksDB ends with an error fails, and is not taken for the end
    /// of the records.
    pub(crate) fn subtree(
        &self,
        keyspace: Keyspace,
        prefix: &Prefix,
        mut each: impl FnMut(&[u8], &[u8]) -> Result<(), ToolError>,
    ) -> Result<(), ToolError> {
        let mut cursor = self.cursor(keyspace, after(prefix));
        cursor.seek(prefix);
        while let (Some(key), Some(value)) = (cursor.key(), cursor.value()) {
            let Some(key) = key.strip_prefix(prefix.as_slice()) else {
                break;
            };
            each(key, value)?;
            cursor.next();
        }
        (cursor.status()).map_err(|error| self.failed(keyspace, error))
    }

    /// Whether `prefix` has a record in `keyspace`.
    pub(crate) fn holds(&self, keyspace: Keyspace, prefix: &Prefix) -> Result<bool, ToolError> {
        let mut cursor = self.cursor(keyspace, after(prefix));
        cursor.seek(prefix);
        let holds = cursor.key().is_some_and(|key| key.starts_with(prefix));
        (cursor.status()).map_err(|error| self.failed(keyspace, error))?;
        Ok(holds)
    }

    /// A walk over every record of `keyspace`, whatever its prefix.
    pub(crate) fn records(&self, keyspace: Keyspace) -> Records<'_> {
        let mut cursor = self.cursor(keyspace, None);
        cursor.seek_to_first();
        Records {
            source: self,
            keyspace,
            cursor,
        }
    }
}

/// A walk over the records of one keyspace of a source, in the order of
/// their stored keys: the prefix, and then the key in the subtree.
pub(crate) struct Records<'s> {
    source: &'s Source,
    keyspace: Keyspace,
    cursor: DBRawIterator<'s>,
}

impl Records<'_> {
    /// The stored key and the value of the record the walk stands on, or
    /// `None` past the last one. A walk that RocksDB ended with an error
    /// fails here.
    pub(crate) fn current(&self) -> Result<Option<Record<'_>>, ToolError> {
        match (self.cursor.key(), self.cursor.value()) {
            (Some(key), Some(value)) => Ok(Some((key, value))),
            _ => {
                let status = self.cursor.status();
                status.map_err(|error| self.source.failed(self.keyspace, error))?;
                Ok(None)
            }
        }
    }

    /// Goes to the next record.
    pub(crate) fn next(&mut self) {
        self.cursor.next();
    }

    /// Goes to the first record whose stored key is at or after `key`.
    pub(crate) fn seek(&mut self, key: &[u8]) {
        self.cursor.seek(key);
    }
}

/// The count of the records of one keyspace, and the bytes of their keys
/// and values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Count {
    /// How many records.
    pub(crate) records: u64,
    /// The bytes of their keys in the subtree and of their values.
    pub(crate) bytes: u64,
}

impl Count {
    /// Counts one record.
    pub(crate) fn add(&mut self, key: &[u8], value: &[u8]) {
        self.records += 1;
        self.bytes += (key.len() + value.len()) as u64;
    }

    /// Adds what `other` counted.
    pub(crate) fn merge(&mut self, other: Count) {
        self.records += other.records;
        self.bytes += other.bytes;
    }
}

/// A count for each keyspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts([Count; 4]);

impl Counts {
    /// The count of `keyspace`.
    pub(crate) fn of(&self, keyspace: Keyspace) -> Count {
        self.0[keyspace.at()]
    }

    /// The count of `keyspace`, to add to.
    pub(crate) fn of_mut(&mut self, keyspace: Keyspace) -> &mut Count {
        &mut self.0[keyspace.at()]
    }

    /// Adds what `other` counted.
    pub(crate) fn merge(&mut self, other: &Counts) {
        for keyspace in Keyspace::ALL {
            self.of_mut(keyspace).merge(other.of(keyspace));
        }
    }

    /// The records of every keyspace.
    pub(crate) fn records(&self) -> u64 {
        self.0.iter().map(|count| count.records).sum()
    }

    /// The bytes of every keyspace.
    pub(crate) fn bytes(&self) -> u64 {
        self.0.iter().map(|count| count.bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use rocksdb::{Options, DB};

    use super::*;

    /// The name of the variable that tells
    /// [`a_process_that_opens_a_database`] which directory to open, and
    /// what it prints.
    const OPENS: &str = "LIGNIN_MIGRATE_TEST_OPENS";
    const OPENED: &str = "the database was opened for writing";
    const REFUSED: &str = "the open for writing was refused";

    fn families() -> Vec<&'static str> {
        Keyspace::ALL
            .iter()
            .map(|keyspace| keyspace.family())
            .collect()
    }

    fn open_for_writing(dir: &Path) -> Result<DB, rocksdb::Error> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        DB::open_cf(&options, dir, families())
    }

    /// No test of its own: the process that the tests below start, which
    /// opens a database for writing as a node does and says how that went.
    #[test]
    fn a_process_that_opens_a_database() {
        let Some(dir) = std::env::var_os(OPENS) else {
            return;
        };
        match open_for_writing(Path::new(&dir)) {
            Ok(_) => println!("{OPENED}"),
            Err(error) => println!("{REFUSED}: {error}"),
        }
    }

    /// What another process is told when it opens `dir` for writing.
    fn another_process_opens(dir: &Path) -> String {
        let mut child = Command::new(std::env::current_exe().expect("this binary"));
        child.args([
            "--exact",
            "migrate::source::tests::a_process_that_opens_a_database",
        ]);
        child.arg("--nocapture").env(OPENS, dir);
        let output = child.output().expect("a process");
        let said = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(said.contains(OPENED) || said.contains(REFUSED), "{said}");
        said
    }

    /// While a source is open here, no other process opens it for writing:
    /// a node cannot be started on a database that is being migrated. It
    /// can once the source is closed.
    #[test]
    fn no_process_opens_a_source_for_writing_while_it_is_read() {
        let dir = tempfile::tempdir().expect("a directory");
        let dir = dir.path().join("rocksdb");
        {
            let db = open_for_writing(&dir).expect("a database");
            let roots = db.cf_handle("roots").expect("the family");
            db.put_cf(roots, [[0; 32].as_slice(), b"r"].concat(), b"k")
                .expect("a put");
            db.put([[0; 32].as_slice(), b"k"].concat(), b"a record")
                .expect("a put");
        }
        let source = Source::open(&dir).expect("the source opens");
        assert!(source.is_locked());
        // Every read the migrator makes is made with the lock held.
        let fingerprint = source.fingerprint().expect("a fingerprint");
        assert!(fingerprint.sequence >= 2, "{fingerprint:?}");
        assert_eq!(
            source.get(Keyspace::Roots, &[0; 32], b"r").unwrap(),
            Some(b"k".to_vec())
        );
        assert!(source.holds(Keyspace::Data, &[0; 32]).unwrap());
        assert!(!source.holds(Keyspace::Aux, &[0; 32]).unwrap());
        {
            let mut records = source.records(Keyspace::Data);
            assert!(records.current().unwrap().is_some());
            records.next();
            assert!(records.current().unwrap().is_none());
        }
        source.unchanged().expect("nothing wrote the source");

        let logs = |dir: &Path| {
            let entries = std::fs::read_dir(dir).expect("the directory");
            let names = entries.map(|entry| entry.expect("an entry").file_name());
            names.filter(|name| is_info_log(name)).count()
        };
        let before = logs(&dir);
        let said = another_process_opens(&dir);
        assert!(said.contains(REFUSED), "{said}");
        // The refused open began a new info log, and wrote nothing else.
        assert_eq!(logs(&dir), before + 1);
        source
            .unchanged()
            .expect("the refused open wrote no file of the database");
        drop(source);
        let said = another_process_opens(&dir);
        assert!(said.contains(OPENED), "{said}");
    }

    /// What is no source is refused in words: a directory with no
    /// database, and a database whose column families are not GroveDB's.
    #[test]
    fn what_is_no_grovedb_is_refused() {
        let dir = tempfile::tempdir().expect("a directory");
        let message = |dir: &Path| match Source::open(dir) {
            Err(ToolError::Migrate(message)) => message,
            Ok(_) => panic!("{} opened", dir.display()),
            Err(other) => panic!("{other:?}"),
        };
        assert!(message(dir.path()).contains("holds no RocksDB database"));
        let other = dir.path().join("other");
        {
            let mut options = Options::default();
            options.create_if_missing(true);
            DB::open(&options, &other).expect("a database");
        }
        assert!(message(&other).contains("its column families are"));
        // A directory with no LOCK file, as one that was never opened for
        // writing: it is read, and nothing keeps a process from it.
        let unlocked = dir.path().join("unlocked");
        drop(open_for_writing(&unlocked).expect("a database"));
        std::fs::remove_file(unlocked.join("LOCK")).expect("the lock file is removed");
        let source = Source::open(&unlocked).expect("the source opens");
        assert!(!source.is_locked());
        // Something writes the directory while it is read.
        std::fs::write(unlocked.join("a file"), b"written").expect("a file");
        let changed = source.unchanged().expect_err("the directory changed");
        assert!(changed
            .to_string()
            .contains("it changed while it was read (a file)"));
    }

    #[test]
    fn the_bound_after_the_keys_of_a_prefix() {
        let bound = after(&[0; 32]).unwrap();
        assert_eq!((bound.len(), bound[31]), (32, 1));
        let mut carried = [0xff; 32];
        carried[0] = 7;
        assert_eq!(after(&carried), Some(vec![8]));
        assert_eq!(after(&[0xff; 32]), None);
        // No key lies between the keys of a prefix and the bound: one that
        // is shorter than a prefix and follows them is at the bound or
        // after it.
        let mut prefix = [0; 32];
        prefix[31] = 0xff;
        let bound = after(&prefix).unwrap();
        let mut short = vec![0; 31];
        short[30] = 1;
        assert_eq!(bound, short);
        assert!(prefix.as_slice() < short.as_slice());
    }

    /// A key of the data keyspace that is shorter than a prefix is met by
    /// a walk that goes from prefix to prefix, wherever it lies.
    #[test]
    fn a_walk_from_prefix_to_prefix_meets_a_key_that_is_shorter_than_one() {
        let dir = tempfile::tempdir().expect("a directory");
        let dir = dir.path().join("rocksdb");
        let mut prefix = [0; 32];
        prefix[31] = 0xff;
        let mut short = vec![0; 31];
        short[30] = 1;
        {
            let db = open_for_writing(&dir).expect("a database");
            db.put([prefix.as_slice(), b"k"].concat(), b"a record")
                .expect("a put");
            db.put(&short, b"a record under a short key")
                .expect("a put");
        }
        let source = Source::open(&dir).expect("the source opens");
        let mut records = source.records(Keyspace::Data);
        let (first, _) = records.current().unwrap().expect("the first record");
        assert!(first.starts_with(&prefix));
        records.seek(&after(&prefix).unwrap());
        let (met, _) = records
            .current()
            .unwrap()
            .expect("the record after the prefix");
        assert_eq!(met, short.as_slice());
    }

    /// A writer and the readers of one process each keep a lock of their
    /// own. A process that has the source open for writing itself is
    /// refused, as often as it asks, and its lock still keeps every other
    /// process out. Two readers in the process do not share a lock: the
    /// end of one leaves the other's. And while a reader lives, a writer
    /// in its own process is kept out as any other is.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_writer_and_the_readers_of_one_process_keep_their_own_locks() {
        let dir = tempfile::tempdir().expect("a directory");
        let dir = dir.path().join("rocksdb");
        let message = |dir: &Path| match Source::open(dir) {
            Err(ToolError::Migrate(message)) => message,
            Ok(_) => panic!("{} opened", dir.display()),
            Err(other) => panic!("{other:?}"),
        };
        let writer = open_for_writing(&dir).expect("a database");
        for _ in 0..3 {
            assert!(message(&dir).contains("a process has it open"));
            let said = another_process_opens(&dir);
            assert!(said.contains(REFUSED), "the writer's lock holds: {said}");
        }
        // The three refusals left one descriptor of that file.
        let kept = |dir: &Path| {
            let identity = file_identity(&std::fs::metadata(dir.join("LOCK")).unwrap());
            let refused = REFUSED_LOCKS.lock().unwrap();
            refused.iter().filter(|(kept, _)| *kept == identity).count()
        };
        assert_eq!(kept(&dir), 1);
        drop(writer);

        let first = Source::open(&dir).expect("the source opens");
        assert_eq!(kept(&dir), 0, "the descriptor that was kept has the lock");
        let second = Source::open(&dir).expect("a second reader of the process");
        drop(first);
        let said = another_process_opens(&dir);
        assert!(
            said.contains(REFUSED),
            "the second reader's lock holds: {said}"
        );
        let refused = match open_for_writing(&dir) {
            Err(refused) => refused,
            Ok(_) => panic!("a writer of this process opened {}", dir.display()),
        };
        assert!(refused.to_string().contains("LOCK"), "{refused}");
        let said = another_process_opens(&dir);
        assert!(
            said.contains(REFUSED),
            "and holds after that writer: {said}"
        );
        second.unchanged().expect("no file of the database changed");
        drop(second);
        let said = another_process_opens(&dir);
        assert!(said.contains(OPENED), "{said}");
    }

    /// A pipe with the name of the lock file is not opened, which would
    /// wait for its other end: the source is refused.
    #[cfg(unix)]
    #[test]
    fn a_lock_that_is_no_file_refuses_the_source() {
        let dir = tempfile::tempdir().expect("a directory");
        let dir = dir.path().join("rocksdb");
        drop(open_for_writing(&dir).expect("a database"));
        std::fs::remove_file(dir.join("LOCK")).expect("the lock file is removed");
        let owner = nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR;
        nix::unistd::mkfifo(&dir.join("LOCK"), owner).expect("a pipe");
        let refused = match Source::open(&dir) {
            Err(refused) => refused.to_string(),
            Ok(_) => panic!("{} opened", dir.display()),
        };
        assert!(refused.contains("its LOCK is no file"), "{refused}");
    }

    /// A lock file that is there once a source with none was opened says
    /// that a process came to write it, and so does a lock file that is
    /// another file than the one that was locked.
    #[test]
    fn a_lock_file_that_appears_or_is_another_refuses_the_source() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("LOCK");
        no_writer_came(dir.path(), &None).expect("no lock file is there");
        std::fs::write(&path, b"").expect("a lock file");
        let refused = no_writer_came(dir.path(), &None).expect_err("a writer came");
        assert!(refused.to_string().contains("opened it for writing while"));
        let lock = Some(File::open(&path).expect("the lock file"));
        no_writer_came(dir.path(), &lock).expect("the lock that was taken is the file");

        // The name is given to another file: the one that was locked is
        // not the one a writer locks.
        std::fs::rename(&path, dir.path().join("the file that was locked")).expect("a rename");
        std::fs::write(&path, b"").expect("another lock file");
        assert!(!still_named(&path, lock.as_ref().unwrap()));
        let refused = no_writer_came(dir.path(), &lock).expect_err("a writer came");
        assert!(refused.to_string().contains("opened it for writing while"));
        std::fs::remove_file(&path).expect("the file is removed");
        assert!(!still_named(&path, lock.as_ref().unwrap()));
    }

    /// A probe that finds the name of the lock file on another file than
    /// the one it kept from an earlier refusal locks the file that is
    /// named, and a source whose lock file is replaced while it is opened
    /// is refused.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_lock_file_that_is_replaced_is_not_taken_for_the_one_that_was_kept() {
        let dir = tempfile::tempdir().expect("a directory");
        let dir = dir.path().join("rocksdb");
        let path = dir.join("LOCK");
        let writer = open_for_writing(&dir).expect("a database");
        assert!(Source::open(&dir).is_err(), "the writer has it open");
        drop(writer);
        // The descriptor of the refusal is kept. The name goes to another
        // file, as where a database is made again in the directory.
        let kept = file_identity(&std::fs::metadata(&path).unwrap());
        std::fs::rename(&path, dir.join("LOCK.before")).expect("a rename");
        std::fs::write(&path, b"").expect("another lock file");
        let lock = Source::probe(&dir).expect("the probe").expect("a lock");
        assert!(still_named(&path, &lock));
        assert_ne!(file_identity(&lock.metadata().unwrap()), kept);
        drop(lock);
        let said = another_process_opens(&dir);
        assert!(said.contains(OPENED), "{said}");
    }
}
