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
//! A directory with no `LOCK` file was never opened for writing since it
//! was made, which a checkpoint is until something opens it: nothing holds
//! it, and nothing is there to lock. Where such a file is there once the
//! source is open, a process came to write between the two looks, and the
//! source is refused; one that comes later is found by the check that
//! nothing changed.
//!
//! **The lock is the process's, not the handle's.** The system gives a
//! process one lock on a file, whichever descriptor asked for it, and
//! takes it away when any descriptor of the file is closed. So a process
//! that has the source open for writing itself, and one that reads it
//! through a second [`Source`], must not get to the `LOCK` file here: the
//! probe would turn the writer's lock into a shared one and its end would
//! release it. On Linux the system lists the locks a process holds
//! (`/proc/locks`), and a source this process holds a lock on is refused
//! before its `LOCK` file is opened. On a system with no such list, a
//! caller of the library that has the source open in its own process is
//! not told apart, and must not call this.
//!
//! **A write-ahead log that was not flushed.** A node that was killed, and
//! one that was stopped, can leave its last commits in the log alone. The
//! read-only open replays them in memory: the migration is of the state the
//! node would recover to at its next start, and the log stays as it is.
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

/// Whether this process holds a lock on the file `lock`, by the system's
/// list of the locks of files, or `None` where the system has no such
/// list or it cannot be read.
fn held_by_this_process(lock: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let inode = std::fs::metadata(lock).ok()?.ino();
        let listed = std::fs::read_to_string("/proc/locks").ok()?;
        let own = std::process::id().to_string();
        // A line: its number, the class of the lock, whether it is
        // advisory, its kind, the process, the file as
        // `major:minor:inode`, and the range.
        let holds = listed.lines().any(|line| {
            let mut words = line.split_whitespace().filter(|word| *word != "->");
            let class = words.nth(1);
            let process = words.nth(2);
            let file = words.next().and_then(|file| file.rsplit(':').next());
            class == Some("POSIX")
                && process == Some(own.as_str())
                && file.and_then(|inode| inode.parse::<u64>().ok()) == Some(inode)
        });
        Some(holds)
    }
    #[cfg(not(unix))]
    {
        let _ = lock;
        None
    }
}

/// Fails where `dir` has a `LOCK` file though the open found none to
/// lock: a process came to write the directory since.
fn no_writer_came(dir: &Path, lock: &Option<File>) -> Result<(), ToolError> {
    match lock.is_none() && dir.join("LOCK").exists() {
        false => Ok(()),
        true => Err(source_error(
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
        // Before the file is opened: the end of a descriptor of it gives
        // up the lock this process has on it.
        if held_by_this_process(&dir.join("LOCK")) == Some(true) {
            return Err(source_error(
                dir,
                "this process has it open. A migration is made of a database that nothing has \
                 open: close it, or migrate a checkpoint of it",
            ));
        }
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

    /// A process that has the source open for writing itself is refused
    /// before the lock file is touched: its lock is still its own, and
    /// keeps every other process out. So is a second reader in the
    /// process, whose end would give up the lock of the first.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_source_that_this_process_has_open_is_refused_with_its_lock_kept() {
        let dir = tempfile::tempdir().expect("a directory");
        let dir = dir.path().join("rocksdb");
        let message = |dir: &Path| match Source::open(dir) {
            Err(ToolError::Migrate(message)) => message,
            Ok(_) => panic!("{} opened", dir.display()),
            Err(other) => panic!("{other:?}"),
        };
        let writer = open_for_writing(&dir).expect("a database");
        assert!(message(&dir).contains("this process has it open"));
        let said = another_process_opens(&dir);
        assert!(said.contains(REFUSED), "the writer's lock holds: {said}");
        drop(writer);

        let first = Source::open(&dir).expect("the source opens");
        assert!(message(&dir).contains("this process has it open"));
        let said = another_process_opens(&dir);
        assert!(said.contains(REFUSED), "the reader's lock holds: {said}");
        drop(first);
        drop(Source::open(&dir).expect("the source opens again"));
    }

    /// A lock file that is there once a source with none was opened says
    /// that a process came to write it.
    #[test]
    fn a_lock_file_that_appears_refuses_the_source() {
        let dir = tempfile::tempdir().expect("a directory");
        no_writer_came(dir.path(), &None).expect("no lock file is there");
        std::fs::write(dir.path().join("LOCK"), b"").expect("a lock file");
        let refused = no_writer_came(dir.path(), &None).expect_err("a writer came");
        assert!(refused.to_string().contains("opened it for writing while"));
        let lock = File::open(dir.path().join("LOCK")).expect("the lock file");
        no_writer_came(dir.path(), &Some(lock)).expect("the lock that was taken is the file");
    }
}
