//! Reading the directory: the lookups, and the walks over its records.

use lignin_format::CellAddress;

use super::{
    error::{DirectoryError, Malformed},
    record::{
        children_key, decode_child, decode_child_key, decode_dropped, decode_header, decode_id,
        decode_prefix, entry_key, root_key, Child, Dropped, Entry, MerkId, Prefix, RecordKind,
        HEADER_KEY,
    },
    Directory,
};
use crate::flat::{Cursor, FlatPages, FlatTree};

/// A record that is not what its key says.
pub(super) fn damaged(key: &[u8], what: Malformed) -> DirectoryError {
    DirectoryError::Damaged {
        key: key.to_vec(),
        what: what.0,
    }
}

impl Directory {
    /// The live Merk under `prefix`: where its root is, its tree type, its
    /// root key and the element that hosts it.
    ///
    /// This is the open by prefix, and the open by path once the path is
    /// hashed. It is one lookup in the directory's keyspace and reads no
    /// other page. A Merk that was dropped has no entry, and the entry that
    /// a Merk below a dropped one still has is not returned
    /// (`transactions.md` CV9).
    pub fn open<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Entry>, DirectoryError> {
        let entry = self.stored(pages, prefix)?;
        Ok(entry.filter(|entry| self.is_live(entry)))
    }

    /// The entry that is stored under `prefix`, whether the Merk is live
    /// or was left under a dropped one ([`Self::is_live`]). It is for the
    /// invariant checker, the tools and the reclaimer: nothing is opened
    /// from it.
    pub fn stored<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Entry>, DirectoryError> {
        let key = entry_key(prefix);
        let Some(raw) = self.get(pages, &key)? else {
            return Ok(None);
        };
        let entry = Entry::decode(raw).map_err(|what| damaged(&key, what))?;
        Ok(Some(entry))
    }

    /// Whether `entry` is of a Merk that is in use: none of the Merks it is
    /// below was dropped or cleared since it was entered.
    pub fn is_live(&self, entry: &Entry) -> bool {
        self.dropped.is_empty() || !entry.lineage().any(|id| self.dropped.contains(&id))
    }

    /// The prefix of the Merk whose root is the cell at `address`.
    ///
    /// Until the reclaimer has taken its records, a Merk below a dropped
    /// one is found by its root as well.
    pub fn root_owner<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        address: CellAddress,
    ) -> Result<Option<Prefix>, DirectoryError> {
        let key = root_key(address);
        let Some(value) = self.get(pages, &key)? else {
            return Ok(None);
        };
        let prefix = decode_prefix(&value).map_err(|what| damaged(&key, what))?;
        Ok(Some(prefix))
    }

    /// The Merks that the nodes of the Merk `parent` host, in the order of
    /// their position and then of the host's key.
    ///
    /// Under the id of a dropped or cleared Merk these are what it left:
    /// the Merks that were below it when it was dropped.
    pub fn children(&self, parent: MerkId) -> Children {
        Children(Records::new(&self.tree, &children_key(parent)))
    }

    /// Every entry, live or left over, in the order of the prefixes.
    pub fn entries(&self) -> Entries {
        Entries(Records::new(&self.tree, &[RecordKind::Entry.code()]))
    }

    /// What each dropped or cleared Merk left that is not reclaimed yet,
    /// in the order of the ids as they are stored.
    pub fn drops(&self) -> Drops {
        Drops(Records::new(&self.tree, &[RecordKind::Dropped.code()]))
    }

    /// The id that the next Merk to be entered gets.
    pub fn next_id<P: FlatPages + ?Sized>(&self, pages: &P) -> Result<MerkId, DirectoryError> {
        let header = self.get(pages, &HEADER_KEY)?;
        let header = header.ok_or(Malformed("the directory has no header"));
        header
            .and_then(|header| decode_header(&header))
            .map_err(|what| damaged(&HEADER_KEY, what))
    }

    pub(super) fn get<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, DirectoryError> {
        let mut value = Vec::new();
        Ok(self.tree.get(pages, key, &mut value)?.then_some(value))
    }

    /// The live Merk under `prefix`, for a change to it.
    pub(super) fn live<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Entry, DirectoryError> {
        let entry = self.open(pages, prefix)?;
        entry.ok_or(DirectoryError::NotFound { prefix: *prefix })
    }

    /// Whether a child record is under the id `parent`.
    pub(super) fn has_children<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        parent: MerkId,
    ) -> Result<bool, DirectoryError> {
        let mut records = Records::new(&self.tree, &children_key(parent));
        records.step(pages)
    }
}

/// A walk over the records whose keys begin with the same bytes.
///
/// It holds no page between two steps, and goes on from its key under a
/// writer that changes the records, the one it stands on among them, as a
/// [`Cursor`] does.
#[derive(Debug, Clone)]
pub(super) struct Records {
    cursor: Cursor,
    within: Vec<u8>,
    started: bool,
    value: Vec<u8>,
}

impl Records {
    pub(super) fn new(tree: &FlatTree, within: &[u8]) -> Self {
        Self {
            cursor: tree.cursor_within(within),
            within: within.to_vec(),
            started: false,
            value: Vec::new(),
        }
    }

    /// Goes to the next record, and says whether there is one.
    pub(super) fn step<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
    ) -> Result<bool, DirectoryError> {
        loop {
            match self.started {
                true => self.cursor.next(pages)?,
                false => self.cursor.seek_to_first(pages)?,
            }
            self.started = true;
            if !self.cursor.valid() {
                return Ok(false);
            }
            self.value.clear();
            // A record that a page step took out under the walk is passed.
            if self.cursor.value(pages, &mut self.value)? {
                return Ok(true);
            }
        }
    }

    /// The key of the record, without the bytes the walk is within.
    pub(super) fn key(&self) -> &[u8] {
        self.cursor.key().unwrap_or_default()
    }

    pub(super) fn value(&self) -> &[u8] {
        &self.value
    }

    pub(super) fn damaged(&self, what: Malformed) -> DirectoryError {
        damaged(&[&self.within, self.key()].concat(), what)
    }
}

/// The Merks that the nodes of one Merk host ([`Directory::children`]).
#[derive(Debug, Clone)]
pub struct Children(Records);

impl Children {
    /// The next of them, or `None` after the last.
    pub fn next<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
    ) -> Result<Option<Child>, DirectoryError> {
        if !self.0.step(pages)? {
            return Ok(None);
        }
        let (position, key) =
            decode_child_key(self.0.key()).map_err(|what| self.0.damaged(what))?;
        let (prefix, id) = decode_child(self.0.value()).map_err(|what| self.0.damaged(what))?;
        Ok(Some(Child {
            position,
            key: key.to_vec(),
            prefix,
            id,
        }))
    }
}

/// Every entry of the directory ([`Directory::entries`]).
#[derive(Debug, Clone)]
pub struct Entries(Records);

impl Entries {
    /// The next entry with its prefix, or `None` after the last.
    pub fn next<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
    ) -> Result<Option<(Prefix, Entry)>, DirectoryError> {
        if !self.0.step(pages)? {
            return Ok(None);
        }
        let prefix = decode_prefix(self.0.key()).map_err(|what| self.0.damaged(what))?;
        let entry = Entry::decode(self.0.value().to_vec()).map_err(|what| self.0.damaged(what))?;
        Ok(Some((prefix, entry)))
    }
}

/// What the dropped Merks left ([`Directory::drops`]).
#[derive(Debug, Clone)]
pub struct Drops(Records);

impl Drops {
    /// The next of them, or `None` after the last.
    pub fn next<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
    ) -> Result<Option<Dropped>, DirectoryError> {
        if !self.0.step(pages)? {
            return Ok(None);
        }
        let id = decode_id(self.0.key()).map_err(|what| self.0.damaged(what))?;
        let root = decode_dropped(self.0.value()).map_err(|what| self.0.damaged(what))?;
        Ok(Some(Dropped { id, root }))
    }
}
