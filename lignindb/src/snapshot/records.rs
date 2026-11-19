//! The records of a database as a reader finds them: through the subtree
//! directory of the version it reads.

use std::{
    error::Error,
    fmt,
    sync::{Arc, OnceLock},
};

use lignin_format::{CellAddress, CommitVersion};
use lignin_store::{load::PageLoader, values::ValueStore};
use lignin_tree::{
    directory::{Directory, DirectoryError, Entry},
    flat::{self, Committed, FlatError, FlatTree},
};

use super::{read::Operation, versions::OperationGuard};
use crate::txn::{Column, Keyspaces, Subtree};

/// Where the records of one store are: its value log, the flat keyspace of
/// each column, and the keyspace of its subtree directory. The engine's
/// open pairs it with the store's [`ReadPath`](super::ReadPath).
#[derive(Clone, Copy)]
pub(super) struct Records<'s> {
    pub(super) values: &'s ValueStore,
    pub(super) keyspaces: Keyspaces,
    pub(super) directory: FlatTree,
}

impl<'s> Records<'s> {
    /// Loads the subtree directory of the version `at`, read under
    /// `guard`: one walk of the records of what waits to be reclaimed.
    pub(super) fn load(
        &self,
        pages: PageLoader<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
    ) -> Result<Directory, DirectoryError> {
        let pages = Committed::new(pages, self.values, guard, at);
        Directory::load(&pages, self.directory.root(), self.directory.config())
    }
}

impl fmt::Debug for Records<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Records")
            .field("keyspaces", &self.keyspaces)
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

/// Why a reader did not read a record.
#[derive(Debug)]
pub enum ReadError {
    /// The read path was made of the pages of a store alone, with no word
    /// of where its records are.
    NoRecords,
    /// The subtree directory could not be read.
    Directory(DirectoryError),
    /// A flat keyspace could not be read.
    Flat(FlatError),
    /// The subtree directory of the snapshot's version could not be loaded
    /// when the snapshot was taken. Every open of the snapshot answers
    /// this: a handle that is loaded later is not one of its version.
    Unloaded(Arc<DirectoryError>),
    /// An operation that pins no version opened its first subtree after
    /// the latest version had moved on from the one it reads at. A handle
    /// that is loaded then is not one of its version, so nothing is
    /// opened: the caller begins another operation, which reads the newer
    /// version.
    Moved {
        /// The version the operation reads at.
        at: CommitVersion,
        /// The latest version when it loaded its handle.
        latest: CommitVersion,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRecords => f.write_str("the read path names no records"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
            Self::Unloaded(error) => {
                write!(f, "subtree directory, when the snapshot was taken: {error}")
            }
            Self::Moved { at, latest } => write!(
                f,
                "the operation reads version {} and the latest is {}",
                at.get(),
                latest.get()
            ),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoRecords | Self::Moved { .. } => None,
            Self::Directory(error) => Some(error),
            Self::Flat(error) => Some(error),
            Self::Unloaded(error) => Some(&**error),
        }
    }
}

impl From<DirectoryError> for ReadError {
    fn from(error: DirectoryError) -> Self {
        Self::Directory(error)
    }
}

impl From<FlatError> for ReadError {
    fn from(error: FlatError) -> Self {
        Self::Flat(error)
    }
}

/// What a snapshot has of the subtree directory of its version: the
/// handle that was loaded when the version was chosen, or why there is
/// none.
pub(super) type Loaded = Result<Directory, Arc<DirectoryError>>;

/// The directory handle of one operation: its snapshot's, or its own.
///
/// **A handle is loaded while the version it is for is the latest one.**
/// What waits to be reclaimed is taken out of the pages one step at a
/// time, the record of a dropped Merk after the entries of the Merks that
/// were below it, and each step is on the page versions that are current
/// then. A reader of an older version may still have such an entry in a
/// page version of its own while the record of the drop is gone from a
/// page it shares with the latest version, so a handle that is loaded for
/// its version then would take the entry for one in use. A handle that
/// was loaded while the version was the latest has the id of the drop, or
/// reads pages from which the entries are gone, and stays right: what is
/// taken out later, it hides (`lignin_tree::directory`, "A handle is of
/// the pages it was loaded from").
pub(super) enum Handle<'o> {
    /// The snapshot's, loaded when it was taken. `None` where the read
    /// path names no records.
    Snapshot(Option<&'o Loaded>),
    /// The handle of an operation that pins no version, loaded when it
    /// first opens a subtree, and kept only if the latest version is still
    /// the one it reads at ([`ReadError::Moved`]).
    Own(OnceLock<Directory>),
}

/// A subtree that a reader keeps from one operation to the next: its entry
/// as the version `at` of one database has it.
#[derive(Debug, Clone)]
pub struct Kept {
    subtree: Subtree,
    entry: Entry,
    database: u64,
    at: CommitVersion,
}

impl<'o> Operation<'o> {
    /// The committed flat pages of the operation's version.
    fn flat_pages(&self, records: &Records<'o>) -> Committed<'o, '_, OperationGuard<'o>> {
        Committed::new(self.pages, records.values, self.guard(), self.at())
    }

    /// The subtree directory of the operation's version ([`Handle`]).
    fn directory(&self, records: &Records<'o>) -> Result<&Directory, ReadError> {
        let cell = match &self.directory {
            Handle::Snapshot(None) => return Err(ReadError::NoRecords),
            Handle::Snapshot(Some(Ok(directory))) => return Ok(directory),
            Handle::Snapshot(Some(Err(error))) => {
                return Err(ReadError::Unloaded(Arc::clone(error)));
            }
            Handle::Own(cell) => cell,
        };
        if let Some(directory) = cell.get() {
            return Ok(directory);
        }
        let loaded = records.load(self.pages, self.guard(), self.at())?;
        let (at, latest) = (self.at(), self.guard().latest());
        if latest != at {
            return Err(ReadError::Moved { at, latest });
        }
        Ok(cell.get_or_init(|| loaded))
    }

    /// Opens `subtree`: its entry in the subtree directory of the version
    /// the operation reads, by one lookup, or `None` where that version
    /// has no subtree in use under the prefix.
    ///
    /// **No record of a subtree is read without its entry**
    /// (`tree-layout.md` D6, `transactions.md` CV9): in every column, the
    /// records under a prefix are those of the entry that the directory
    /// has under it. So with `None` there is no record to read, in any
    /// column, and nothing is read. A subtree that was dropped, and one
    /// that was below a dropped one, is not opened from the drop's version
    /// on, whether or not what it left is reclaimed; a snapshot from
    /// before the drop opens both.
    pub fn open(&self, subtree: Subtree) -> Result<Option<Opened<'_, 'o>>, ReadError> {
        let records = self.records.ok_or(ReadError::NoRecords)?;
        let directory = self.directory(&records)?;
        let entry = directory.open(&self.flat_pages(&records), subtree.as_bytes())?;
        Ok(entry.map(|entry| Opened {
            operation: self,
            records,
            subtree,
            entry,
        }))
    }

    /// Opens a subtree that an earlier operation kept ([`Opened::keep`]).
    /// At the version of the database it was kept at, the entry is the one
    /// that version has, and nothing is read: a subtree leaves with a
    /// commit, which makes another version. At another version, or in
    /// another database, it is looked up again.
    pub fn resume(&self, kept: &Kept) -> Result<Option<Opened<'_, 'o>>, ReadError> {
        if (kept.database, kept.at) != (self.database, self.at()) {
            return self.open(kept.subtree);
        }
        let records = self.records.ok_or(ReadError::NoRecords)?;
        Ok(Some(Opened {
            operation: self,
            records,
            subtree: kept.subtree,
            entry: kept.entry.clone(),
        }))
    }
}

/// A subtree as one operation of a reader opened it ([`Operation::open`]).
/// The records of the subtree are read through this, and through nothing
/// else.
pub struct Opened<'a, 'o> {
    operation: &'a Operation<'o>,
    records: Records<'o>,
    subtree: Subtree,
    entry: Entry,
}

impl<'a, 'o> Opened<'a, 'o> {
    /// The subtree.
    pub fn subtree(&self) -> Subtree {
        self.subtree
    }

    /// The subtree's entry: its tree type, the root of its Merk with the
    /// root's key, and the element that hosts it.
    pub fn entry(&self) -> &Entry {
        &self.entry
    }

    /// The cell of the root of the subtree's Merk, or `None` for a Merk
    /// with no node. The nodes are read from there
    /// ([`Operation::nodes`]).
    pub fn root(&self) -> Option<CellAddress> {
        self.entry.root().map(|root| root.address)
    }

    /// Appends the value of the flat record under `key` in `column` of the
    /// subtree to `out`, and says whether there is one.
    pub fn get(&self, column: Column, key: &[u8], out: &mut Vec<u8>) -> Result<bool, ReadError> {
        let stored = [&self.subtree.as_bytes()[..], key].concat();
        let pages = self.operation.flat_pages(&self.records);
        Ok(self
            .records
            .keyspaces
            .of(column)
            .get(&pages, &stored, out)?)
    }

    /// A cursor over the flat records of `column` of the subtree, in key
    /// order, not positioned. It is one of this operation.
    pub fn cursor(&self, column: Column) -> RecordCursor<'a, 'o> {
        let tree = self.records.keyspaces.of(column);
        RecordCursor {
            pages: self.operation.flat_pages(&self.records),
            cursor: tree.cursor_within(self.subtree.as_bytes()),
        }
    }

    /// The subtree, to open again in a later operation without a lookup
    /// ([`Operation::resume`]).
    pub fn keep(self) -> Kept {
        Kept {
            subtree: self.subtree,
            entry: self.entry,
            database: self.operation.database,
            at: self.operation.at(),
        }
    }
}

impl fmt::Debug for Opened<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opened")
            .field("subtree", &self.subtree)
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
}

/// A cursor of a reader over the flat records of one column of one
/// subtree, as the operation's version has them.
pub struct RecordCursor<'a, 'o> {
    pages: Committed<'o, 'a, OperationGuard<'o>>,
    cursor: flat::Cursor,
}

impl RecordCursor<'_, '_> {
    /// Goes to the first key at or after `key`.
    pub fn seek(&mut self, key: &[u8]) -> Result<(), ReadError> {
        Ok(self.cursor.seek(&self.pages, key)?)
    }

    /// Goes to the last key at or before `key`.
    pub fn seek_for_prev(&mut self, key: &[u8]) -> Result<(), ReadError> {
        Ok(self.cursor.seek_for_prev(&self.pages, key)?)
    }

    /// Goes to the first key.
    pub fn seek_to_first(&mut self) -> Result<(), ReadError> {
        Ok(self.cursor.seek_to_first(&self.pages)?)
    }

    /// Goes to the last key.
    pub fn seek_to_last(&mut self) -> Result<(), ReadError> {
        Ok(self.cursor.seek_to_last(&self.pages)?)
    }

    /// Goes to the next key. Past the last one the cursor stands on none.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<(), ReadError> {
        Ok(self.cursor.next(&self.pages)?)
    }

    /// Goes to the key before. Before the first one the cursor stands on
    /// none.
    pub fn prev(&mut self) -> Result<(), ReadError> {
        Ok(self.cursor.prev(&self.pages)?)
    }

    /// The key the cursor stands on, if it stands on one.
    pub fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }

    /// Appends the value under the key the cursor stands on to `out`, and
    /// says whether it stands on one.
    pub fn value(&self, out: &mut Vec<u8>) -> Result<bool, ReadError> {
        Ok(self.cursor.value(&self.pages, out)?)
    }
}

impl fmt::Debug for RecordCursor<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordCursor")
            .field("key", &self.key())
            .finish_non_exhaustive()
    }
}
