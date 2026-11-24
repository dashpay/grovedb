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
//! **A source that a process has open is refused.** RocksDB holds a lock on
//! the file `LOCK` of a database that is open for writing. Before the open,
//! the migrator asks for a shared lock on that file, which it is refused
//! while such a process lives ([`Source::open`]), and keeps it until the
//! migration ends, so that no node starts on the source meanwhile. A
//! directory with no `LOCK` file was never opened for writing since it was
//! made, which a checkpoint is until something opens it: nothing holds it,
//! and nothing is there to lock.
//!
//! **A write-ahead log that was not flushed.** A node that was killed, and
//! one that was stopped, can leave its last commits in the log alone. The
//! read-only open replays them in memory: the migration is of the state the
//! node would recover to at its next start, and the log stays as it is.
//!
//! **The source is not to change while it is read.** [`Source::unchanged`]
//! compares the names, the lengths and the modification times of the
//! directory's entries with what they were at the open.
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
    time::SystemTime,
};

use rocksdb::{ColumnFamily, DBRawIterator, Options, ReadOptions, DB};
use rustix::fs::{fcntl_lock, FlockOperation};

use crate::ToolError;

/// The bytes of a subtree's prefix.
pub(crate) const PREFIX_LEN: usize = 32;

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

/// One entry of the source's directory: its name, its length and when it
/// was last modified.
type Listed = (std::ffi::OsString, u64, Option<SystemTime>);

/// The entries of `dir`, in the order of their names.
fn listing(dir: &Path) -> io::Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        listed.push((entry.file_name(), metadata.len(), metadata.modified().ok()));
    }
    listed.sort();
    Ok(listed)
}

/// The prefix after `prefix` in the order of the keys, or `None` for the
/// last one.
pub(crate) fn after(prefix: &Prefix) -> Option<Prefix> {
    let mut next = *prefix;
    for byte in next.iter_mut().rev() {
        *byte = byte.wrapping_add(1);
        if *byte != 0 {
            return Some(next);
        }
    }
    None
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
    /// It is released when the file is closed.
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
    /// It is refused where `dir` holds no RocksDB database, where a
    /// process has the database open for writing, and where its column
    /// families are not the four of a GroveDB.
    pub(crate) fn open(dir: &Path) -> Result<Self, ToolError> {
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
        let file = match File::open(dir.join("LOCK")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(source_error(dir, format!("its LOCK file: {error}"))),
        };
        match fcntl_lock(&file, FlockOperation::NonBlockingLockShared) {
            Ok(()) => Ok(Some(file)),
            Err(error)
                if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::ACCESS =>
            {
                Err(source_error(
                    dir,
                    "a process has it open. A migration is made offline: stop the node, or \
                     migrate a checkpoint of it",
                ))
            }
            Err(error) => Err(source_error(dir, format!("its LOCK file: {error}"))),
        }
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

    /// A cursor over `keyspace`, for a walk in the order of the keys.
    fn cursor(&self, keyspace: Keyspace, upper: Option<Prefix>) -> DBRawIterator<'_> {
        let mut options = ReadOptions::default();
        // A walk reads each block once: it is not kept for a second read.
        options.fill_cache(false);
        options.set_readahead_size(2 << 20);
        if let Some(upper) = upper {
            options.set_iterate_upper_bound(upper.to_vec());
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
    pub(crate) fn current(&self) -> Result<Option<(&[u8], &[u8])>, ToolError> {
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
