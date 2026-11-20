//! The records of a database as a reader finds them: through the subtree
//! directory of the version it reads.

use std::{error::Error, fmt, sync::OnceLock};

use lignin_format::{CellAddress, CommitVersion};
use lignin_store::values::ValueStore;
use lignin_tree::{
    cursor::CursorError,
    directory::{Directory, DirectoryError, Entry},
    flat::{self, Committed, FlatError},
    node::NodeError,
};

use super::{kept::Directories, read::Operation, versions::OperationGuard};
use crate::txn::{Column, Keyspaces, Subtree};

/// Where the records of one store are: its value log, the flat keyspace of
/// each column, and the handles on its subtree directory that the engine
/// keeps. The engine's open pairs it with the store's
/// [`ReadPath`](super::ReadPath).
#[derive(Clone, Copy)]
pub(super) struct Records<'s> {
    pub(super) values: &'s ValueStore,
    pub(super) keyspaces: Keyspaces,
    pub(super) directories: &'s Directories,
}

impl fmt::Debug for Records<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Records")
            .field("keyspaces", &self.keyspaces)
            .field("directories", &self.directories)
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
    /// A node of a Merk could not be read.
    Node(NodeError),
    /// A cursor over the nodes of a Merk did not move.
    Cursor(CursorError),
    /// The engine keeps no handle on the subtree directory for the version
    /// that is read. It keeps one for every version that is pinned, and
    /// for the latest version of an operation under its guard
    /// ([`Directories`]), so this is a read path whose versions were
    /// published by something that is no commit of the engine.
    NotKept {
        /// The version the reader reads at.
        at: CommitVersion,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRecords => f.write_str("the read path names no records"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
            Self::Node(error) => write!(f, "node: {error}"),
            Self::Cursor(error) => write!(f, "cursor: {error}"),
            Self::NotKept { at } => write!(
                f,
                "no handle on the subtree directory is kept for version {}",
                at.get()
            ),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoRecords | Self::NotKept { .. } => None,
            Self::Directory(error) => Some(error),
            Self::Flat(error) => Some(error),
            Self::Node(error) => Some(error),
            Self::Cursor(error) => Some(error),
        }
    }
}

impl From<NodeError> for ReadError {
    fn from(error: NodeError) -> Self {
        Self::Node(error)
    }
}

impl From<CursorError> for ReadError {
    fn from(error: CursorError) -> Self {
        Self::Cursor(error)
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

/// The directory handle of one operation: its snapshot's, or its own.
///
/// **A handle is a copy of the one the engine keeps for the version**
/// ([`Directories`]), taken when the reader first opens a subtree. The
/// kept handle was made while its version was the newest, by the commit
/// that made the version or by the open of the database, so it has the id
/// of every Merk that is dropped there and not reclaimed, and it hides
/// what is taken out of the pages afterwards (`lignin_tree::directory`, "A
/// handle is of the pages it was loaded from"). A handle that a reader
/// loaded itself for a version that is the latest no more would not: what
/// a drop left is taken out one page step at a time, on the page versions
/// that are current then, the record of the dropped Merk last.
pub(super) enum Handle<'o> {
    /// The snapshot's, taken at its first open and kept for its life.
    Snapshot(&'o OnceLock<Option<Directory>>),
    /// The handle of an operation that pins no version: the kept one of
    /// the latest version it read, which its guard keeps there.
    Own(OnceLock<Option<Directory>>),
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
            Handle::Snapshot(cell) => *cell,
            Handle::Own(cell) => cell,
        };
        let kept = cell.get_or_init(|| records.directories.at(self.at()));
        kept.as_ref().ok_or(ReadError::NotKept { at: self.at() })
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
    pub(super) operation: &'a Operation<'o>,
    pub(super) records: Records<'o>,
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

impl Opened<'_, '_> {
    /// A cursor over the flat records of `column` of the subtree, in key
    /// order, not positioned, which the reader keeps from one operation to
    /// the next ([`RecordWalk`]).
    pub fn record_walk(&self, column: Column) -> RecordWalk {
        let tree = self.records.keyspaces.of(column);
        RecordWalk {
            cursor: tree.cursor_within(self.subtree.as_bytes()),
        }
    }
}

/// A cursor over the flat records of one column of one subtree, which a
/// reader keeps from one operation to the next ([`Opened::record_walk`]).
///
/// It holds no page between two calls. Each call is given the operation it
/// is made in, which reads the version the cursor was made at: the
/// operations of one snapshot do.
pub struct RecordWalk {
    cursor: flat::Cursor,
}

macro_rules! record_moves {
    ($($(#[$doc:meta])* $name:ident($($key:ident)?);)*) => {$(
        $(#[$doc])*
        pub fn $name(
            &mut self,
            operation: &Operation<'_>,
            $($key: &[u8],)?
        ) -> Result<(), ReadError> {
            let records = operation.records.ok_or(ReadError::NoRecords)?;
            let pages = operation.flat_pages(&records);
            Ok(self.cursor.$name(&pages $(, $key)?)?)
        }
    )*};
}

impl RecordWalk {
    record_moves! {
        /// Goes to the first key at or after `key`.
        seek(key);
        /// Goes to the last key at or before `key`.
        seek_for_prev(key);
        /// Goes to the first key.
        seek_to_first();
        /// Goes to the last key.
        seek_to_last();
        /// Goes to the next key. Past the last one the cursor stands on
        /// none.
        next();
        /// Goes to the key before. Before the first one the cursor stands
        /// on none.
        prev();
    }

    /// The key the cursor stands on, if it stands on one.
    pub fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }

    /// Appends the value under the key the cursor stands on to `out`, and
    /// says whether it stands on one.
    pub fn value(&self, operation: &Operation<'_>, out: &mut Vec<u8>) -> Result<bool, ReadError> {
        let records = operation.records.ok_or(ReadError::NoRecords)?;
        Ok(self.cursor.value(&operation.flat_pages(&records), out)?)
    }
}

impl fmt::Debug for RecordWalk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordWalk")
            .field("key", &self.key())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for RecordCursor<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordCursor")
            .field("key", &self.key())
            .finish_non_exhaustive()
    }
}
