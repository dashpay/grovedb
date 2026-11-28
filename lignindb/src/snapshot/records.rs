//! The records of a database as a reader finds them: through the subtree
//! directory of the version it reads.

use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

use lignin_format::{CellAddress, CommitVersion};
use lignin_store::values::ValueStore;
use lignin_tree::{
    cursor::CursorError,
    directory::{self, Directory, DirectoryError, Entry},
    flat::{self, Committed, FlatError},
    node::NodeError,
};

use super::{kept::Directories, read::Operation, versions::OperationGuard};
use crate::txn::{unlinked, Column, Keyspaces, StandIns, Subtree};

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
    /// A cursor over flat records was given an operation of another
    /// version than the one it was made at, or of another database. The
    /// cursor is as it was.
    OtherVersion {
        /// The version the cursor was made at.
        made_at: CommitVersion,
        /// The version of the operation that was given.
        given_at: CommitVersion,
    },
    /// The engine keeps no handle on the subtree directory for the version
    /// that is read. It keeps one for every version that is pinned, and
    /// for the latest version of an operation under its guard
    /// ([`Directories`]), so this is a read path whose versions were
    /// published by something that is no commit of the engine.
    NotKept {
        /// The version the reader reads at.
        at: CommitVersion,
    },
    /// A flat record in the data column of the subtree was read as a node
    /// of its Merk, and is no unlinked record (`lignindb::txn`, "Unlinked
    /// records").
    NotUnlinked,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRecords => f.write_str("the read path names no records"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
            Self::Node(error) => write!(f, "node: {error}"),
            Self::Cursor(error) => write!(f, "cursor: {error}"),
            Self::OtherVersion { made_at, given_at } => write!(
                f,
                "a cursor over flat records made at version {} was given an operation at \
                 version {}, or one of another database",
                made_at.get(),
                given_at.get()
            ),
            Self::NotKept { at } => write!(
                f,
                "no handle on the subtree directory is kept for version {}",
                at.get()
            ),
            Self::NotUnlinked => {
                f.write_str("a flat record among the nodes of a Merk is no unlinked record")
            }
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoRecords
            | Self::NotKept { .. }
            | Self::OtherVersion { .. }
            | Self::NotUnlinked => None,
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
/// as the version `at` of one database has it, and whether that version
/// holds unlinked records under it, once a read has asked
/// ([`Opened::node`](Opened::node)).
///
/// It is of that version of that database and of no reader: an operation
/// of any reader of the version resumes from it
/// ([`Operation::resume`]). It holds no version and no page. A copy
/// shares the entry and the answer on unlinked records.
#[derive(Debug, Clone)]
pub struct Kept {
    subtree: Subtree,
    entry: Arc<Entry>,
    database: u64,
    at: CommitVersion,
    unlinked: Arc<Mutex<Option<bool>>>,
}

impl Kept {
    /// The subtree that is kept.
    pub fn subtree(&self) -> Subtree {
        self.subtree
    }
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
            entry: Arc::new(entry),
            unlinked: Arc::default(),
        }))
    }

    /// A walk over every entry of the subtree directory of the version
    /// the operation reads, in the order of the prefixes
    /// ([`EntryWalk`]). It reads the directory's keyspace as it goes, one
    /// page at a time, and holds nothing of it between two steps but its
    /// place.
    ///
    /// It is for the tools that look at a whole database: a reader that
    /// knows its subtree opens it ([`Self::open`]).
    pub fn entries(&self) -> Result<EntryWalk<'_, 'o>, ReadError> {
        let records = self.records.ok_or(ReadError::NoRecords)?;
        let directory = self.directory(&records)?.clone();
        Ok(EntryWalk {
            operation: self,
            records,
            entries: directory.entries(),
            directory,
            last: None,
            failed: false,
        })
    }

    /// How many records `column` holds in the subtrees after `subtree` in
    /// the order of their prefixes, or with `back` in those before it, at
    /// the version the operation reads: counted from the nearest subtree
    /// on and up to `limit`. The nodes of a Merk and the flat records of
    /// the data column are both records of that column
    /// (`transactions.md` ST1).
    ///
    /// It is what tells where a step from past the end of a subtree leads
    /// on an engine that keeps the records of every subtree of a column
    /// in one keyspace (`flat-store.md`, rule 6).
    ///
    /// For each subtree it comes to, the call seeks once in the column's
    /// tree, which finds the nearest subtree the version stores a record
    /// under. For the data column it also walks the entries of the
    /// subtree directory to the nearest Merk with a node, and no further
    /// than the subtree the seek found ([`Directory::rooted_beside`]). It
    /// then opens the nearer of the two and steps over its records until
    /// it has counted `limit`. A subtree whose records are what a drop
    /// left is not opened, and is passed.
    pub fn beside(
        &self,
        column: Column,
        subtree: Subtree,
        back: bool,
        limit: usize,
    ) -> Result<usize, ReadError> {
        self.beside_with(column, subtree, back, limit, None)
    }

    /// As [`Self::beside`], for an engine that keeps, as keys of one unit,
    /// what stands for records of other subtrees (`stand_ins`), as
    /// [`Cursor::beside_with`](crate::txn::Cursor::beside_with) is: where
    /// the unit is of `column`, its subtree is passed as one that holds
    /// no record, and each key of the unit from [`StandIns::low`] on and
    /// below [`StandIns::high`] is counted as one record beside
    /// `subtree`, after the records of the subtrees and up to `limit`
    /// with them, at the version the operation reads.
    pub fn beside_with(
        &self,
        column: Column,
        subtree: Subtree,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, ReadError> {
        let apart = stand_ins
            .filter(|stand_ins| stand_ins.unit.column == column)
            .map(|stand_ins| stand_ins.unit.subtree);
        let records = self.records.ok_or(ReadError::NoRecords)?;
        let pages = self.flat_pages(&records);
        let (mut from, mut found) = (subtree, 0);
        while found < limit {
            let stored = (records.keyspaces).stored_beside(&pages, column, &from, back)?;
            let rooted = match column {
                Column::Data => {
                    let until = stored.as_ref().map(Subtree::as_bytes);
                    let directory = self.directory(&records)?;
                    let rooted = directory.rooted_beside(&pages, from.as_bytes(), until, back)?;
                    rooted.map(Subtree::new)
                }
                Column::Aux | Column::Roots | Column::Meta => None,
            };
            let beside = [stored, rooted].into_iter().flatten();
            let next = match back {
                false => beside.min(),
                true => beside.max(),
            };
            let Some(next) = next else {
                break;
            };
            if Some(next) != apart
                && let Some(opened) = self.open(next)?
            {
                found += opened.count(column, None, limit - found)?;
            }
            from = next;
        }
        if let Some(stand_ins) = stand_ins.filter(|_| found < limit)
            && let Some(opened) = self.open(stand_ins.unit.subtree)?
        {
            let range = Some((stand_ins.low, stand_ins.high));
            found += opened.count(stand_ins.unit.column, range, limit - found)?;
        }
        Ok(found)
    }

    /// Opens a subtree that an earlier operation kept ([`Opened::keep`]):
    /// one of this reader, or of another reader of the same database.
    /// At the version of the database it was kept at, the entry is the one
    /// that version has, and nothing is read: a subtree leaves with a
    /// commit, which makes another version. At another version, or in
    /// another database, it is looked up again.
    ///
    /// In a build with the feature `held_reads`, an entry that is resumed
    /// is held to the lookup: the subtree is looked up all the same, a
    /// lookup that fails ends the call with its error, and an entry that
    /// is not the directory's panics. This crate's own tests hold that a
    /// resume asks for no page, in a build without the feature.
    pub fn resume(&self, kept: &Kept) -> Result<Option<Opened<'_, 'o>>, ReadError> {
        if (kept.database, kept.at) != (self.database, self.at()) {
            return self.open(kept.subtree);
        }
        let records = self.records.ok_or(ReadError::NoRecords)?;
        #[cfg(feature = "held_reads")]
        {
            let directory = self.directory(&records)?;
            let stored = directory.open(&self.flat_pages(&records), kept.subtree.as_bytes())?;
            assert_eq!(
                stored.as_ref(),
                Some(&*kept.entry),
                "the entry of the subtree {:?} that was kept at version {} against the \
                 directory's at that version",
                kept.subtree,
                kept.at.get()
            );
        }
        Ok(Some(Opened {
            operation: self,
            records,
            subtree: kept.subtree,
            entry: Arc::clone(&kept.entry),
            unlinked: Arc::clone(&kept.unlinked),
        }))
    }
}

/// One entry of the subtree directory, as a walk over it gives it
/// ([`EntryWalk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The subtree the entry is under.
    pub subtree: Subtree,
    /// The entry: the tree type, the root of the Merk with the root's key,
    /// and the element that hosts it.
    pub entry: Entry,
    /// Whether the subtree is in use at the version: `false` for the entry
    /// of a subtree below one that was dropped or cleared, which stays in
    /// the directory until the reclaimer takes it. A subtree that is not
    /// in use is not opened ([`Operation::open`]).
    pub live: bool,
}

/// A walk over every entry of the subtree directory of one operation's
/// version, in the order of the prefixes ([`Operation::entries`]).
///
/// **Its end is told from a failure.** [`Self::next`] returns `Ok(None)`
/// only after the last entry, and a page of the directory that cannot be
/// read is an error, whichever entry it holds: a caller that stops at the
/// first error has not seen the end of the directory, and says so. A call
/// after an error reads the directory again from its first page and gives
/// the entry after the last one the walk gave, so a walk that goes on
/// after errors gives each entry once, in order.
pub struct EntryWalk<'a, 'o> {
    operation: &'a Operation<'o>,
    records: Records<'o>,
    directory: Directory,
    entries: directory::Entries,
    /// The prefix of the last entry given.
    last: Option<[u8; Subtree::LEN]>,
    /// Whether the last call failed: the next one starts again.
    failed: bool,
}

impl EntryWalk<'_, '_> {
    /// The next entry, or `None` after the last.
    #[expect(
        clippy::should_implement_trait,
        reason = "no iterator: an item that is an error can be passed over as no item, and a \
                  walk's end is told from a failure only by `Ok(None)`"
    )]
    pub fn next(&mut self) -> Result<Option<Listed>, ReadError> {
        let pages = self.operation.flat_pages(&self.records);
        // After a failure the place of the walk is not known: it starts
        // again, and passes over what it gave.
        let passing = std::mem::take(&mut self.failed);
        if passing {
            self.entries = self.directory.entries();
        }
        let found = loop {
            match self.entries.next(&pages) {
                Err(error) => {
                    self.failed = true;
                    return Err(error.into());
                }
                Ok(Some((prefix, _)))
                    if passing && self.last.is_some_and(|last| prefix <= last) => {}
                Ok(found) => break found,
            }
        };
        let Some((prefix, entry)) = found else {
            return Ok(None);
        };
        self.last = Some(prefix);
        Ok(Some(Listed {
            subtree: Subtree::new(prefix),
            live: self.directory.is_live(&entry),
            entry,
        }))
    }
}

impl fmt::Debug for EntryWalk<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EntryWalk")
            .field("at", &self.operation.at())
            .finish_non_exhaustive()
    }
}

/// A subtree as one operation of a reader opened it ([`Operation::open`]).
/// The records of the subtree are read through this, and through nothing
/// else.
pub struct Opened<'a, 'o> {
    pub(super) operation: &'a Operation<'o>,
    pub(super) records: Records<'o>,
    subtree: Subtree,
    entry: Arc<Entry>,
    /// Whether the version holds a record in the data column under the
    /// subtree, once it was looked up: shared with what the reader keeps
    /// of the subtree ([`Kept`]).
    unlinked: Arc<Mutex<Option<bool>>>,
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
            unlinked: self.unlinked,
        }
    }

    /// Whether the version holds a record in the data column under the
    /// subtree: for a subtree with a Merk, whether it holds unlinked
    /// records (`lignindb::txn`, "Unlinked records"). The lookup reads no
    /// record, and its answer is kept: by this, by what the reader keeps
    /// of the subtree ([`Self::keep`]), and by every open that resumes
    /// from there at the same version. Those that share the answer do
    /// not look it up at the same time: one looks while the others wait.
    /// A lookup that fails keeps no answer.
    pub(super) fn has_unlinked(&self) -> Result<bool, ReadError> {
        let mut known = self.unlinked.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(known) = *known {
            return Ok(known);
        }
        let pages = self.operation.flat_pages(&self.records);
        let data = self.records.keyspaces.of(Column::Data);
        let any = unlinked::any(&data, &pages, &self.subtree)?;
        *known = Some(any);
        Ok(any)
    }
}

impl Opened<'_, '_> {
    /// How many records `column` holds under the subtree, up to `limit`:
    /// for the data column the nodes of its Merk and the flat records
    /// among them, as a walk over the Merk shows them. With `range`,
    /// those whose key is at or after its first key and below its second.
    fn count(
        &self,
        column: Column,
        range: Option<(&[u8], &[u8])>,
        limit: usize,
    ) -> Result<usize, ReadError> {
        let within = |key: Option<&[u8]>| match (key, range) {
            (Some(key), Some((_, high))) => key < high,
            (Some(_), None) => true,
            (None, _) => false,
        };
        let mut found = 0;
        if column == Column::Data {
            let mut nodes = self.walk()?;
            match range {
                Some((low, _)) => nodes.seek(self.operation, low)?,
                None => nodes.seek_to_first(self.operation)?,
            }
            while within(nodes.key()) && found < limit {
                found += 1;
                if found < limit {
                    nodes.next(self.operation)?;
                }
            }
        } else {
            let mut flat = self.cursor(column);
            match range {
                Some((low, _)) => flat.seek(low)?,
                None => flat.seek_to_first()?,
            }
            while within(flat.key()) && found < limit {
                found += 1;
                if found < limit {
                    flat.next()?;
                }
            }
        }
        Ok(found)
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
            database: self.operation.database,
            at: self.operation.at(),
            #[cfg(all(test, not(loom)))]
            failing: false,
        }
    }
}

/// A cursor over the flat records of one column of one subtree, which a
/// reader keeps from one operation to the next ([`Opened::record_walk`]).
///
/// It holds no page between two calls. Each call is given the operation it
/// is made in, which has to read the version of the database that the
/// cursor was made at: the operations of one snapshot do. A call with
/// another is refused ([`ReadError::OtherVersion`]), and the cursor stays
/// as it was.
pub struct RecordWalk {
    cursor: flat::Cursor,
    database: u64,
    at: CommitVersion,
    /// Whether the next move fails before the cursor moves. It is for
    /// the tests, which have no page that fails to load.
    #[cfg(all(test, not(loom)))]
    pub(super) failing: bool,
}

macro_rules! record_moves {
    ($($(#[$doc:meta])* $name:ident($($key:ident)?);)*) => {$(
        $(#[$doc])*
        pub fn $name(
            &mut self,
            operation: &Operation<'_>,
            $($key: &[u8],)?
        ) -> Result<(), ReadError> {
            let records = self.records_of(operation)?;
            #[cfg(all(test, not(loom)))]
            if std::mem::take(&mut self.failing) {
                return Err(ReadError::NoRecords);
            }
            let pages = operation.flat_pages(&records);
            Ok(self.cursor.$name(&pages $(, $key)?)?)
        }
    )*};
}

impl RecordWalk {
    /// The records of `operation`, if it reads what the cursor was made
    /// on.
    pub(super) fn records_of<'o>(
        &self,
        operation: &Operation<'o>,
    ) -> Result<Records<'o>, ReadError> {
        if (operation.database, operation.at()) != (self.database, self.at) {
            return Err(ReadError::OtherVersion {
                made_at: self.at,
                given_at: operation.at(),
            });
        }
        operation.records.ok_or(ReadError::NoRecords)
    }

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
        let records = self.records_of(operation)?;
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
