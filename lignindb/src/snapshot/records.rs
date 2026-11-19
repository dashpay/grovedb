//! The records of a database as a reader finds them: through the subtree
//! directory of the version it reads.

use std::{error::Error, fmt, sync::OnceLock};

use lignin_format::{CellAddress, CommitVersion};
use lignin_store::values::ValueStore;
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
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRecords => f.write_str("the read path names no records"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoRecords => None,
            Self::Directory(error) => Some(error),
            Self::Flat(error) => Some(error),
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

/// The directory handle of one operation: its snapshot's, which is loaded
/// once for the snapshot's version, or its own.
pub(super) enum Handle<'o> {
    Snapshot(&'o OnceLock<Directory>),
    Own(OnceLock<Directory>),
}

impl Handle<'_> {
    fn cell(&self) -> &OnceLock<Directory> {
        match self {
            Self::Snapshot(cell) => cell,
            Self::Own(cell) => cell,
        }
    }
}

/// A subtree that a reader keeps from one operation to the next: its entry
/// as the version `at` has it.
#[derive(Debug, Clone)]
pub struct Kept {
    subtree: Subtree,
    entry: Entry,
    at: CommitVersion,
}

impl<'o> Operation<'o> {
    /// The committed flat pages of the operation's version.
    fn flat_pages(&self, records: &Records<'o>) -> Committed<'o, '_, OperationGuard<'o>> {
        Committed::new(self.pages, records.values, self.guard(), self.at())
    }

    /// The subtree directory of the operation's version. A handle is of
    /// the pages it was loaded from (`lignin_tree::directory`): a snapshot
    /// loads its own once, and an operation that pins no version loads one
    /// for the version it reads.
    fn directory(&self, records: &Records<'o>) -> Result<&Directory, ReadError> {
        let cell = self.directory.cell();
        if let Some(directory) = cell.get() {
            return Ok(directory);
        }
        let (root, config) = (records.directory.root(), records.directory.config());
        let loaded = Directory::load(&self.flat_pages(records), root, config)?;
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

    /// Opens a subtree that an earlier operation of the same reader kept
    /// ([`Opened::keep`]). At the version it was kept at, the entry is
    /// good while the directory has it as one in use, which is asked in
    /// memory and reads no page. At another version it is looked up again.
    pub fn resume(&self, kept: &Kept) -> Result<Option<Opened<'_, 'o>>, ReadError> {
        if kept.at != self.at() {
            return self.open(kept.subtree);
        }
        let records = self.records.ok_or(ReadError::NoRecords)?;
        let live = self.directory(&records)?.is_live(&kept.entry);
        Ok(live.then(|| Opened {
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

    /// The subtree, to open again in a later operation of the same reader
    /// without a lookup ([`Operation::resume`]).
    pub fn keep(self) -> Kept {
        Kept {
            subtree: self.subtree,
            entry: self.entry,
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
