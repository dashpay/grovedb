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

/// A record that is not what its key says, or that does not agree with
/// another.
pub(super) fn damaged(key: &[u8], what: &'static str) -> DirectoryError {
    let key = key.to_vec();
    DirectoryError::Damaged { key, what }
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
        let entry = Entry::decode(raw).map_err(|what| damaged(&key, what.0))?;
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
        let prefix = decode_prefix(&value).map_err(|what| damaged(&key, what.0))?;
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

    /// The nearest Merk with a node after `prefix` in the order of the
    /// prefixes, or with `back` the nearest before it: a Merk in use whose
    /// entry names a root. `None` if there is none that way, or none
    /// before the walk comes to `until` or passes it.
    ///
    /// It is one seek among the entries and a step for each entry it
    /// passes: that of a Merk with no node, of a tree that keeps its data
    /// elsewhere, and one that a drop left. With `until`, which is a
    /// prefix on the side the walk goes to, it passes no entry beyond it.
    pub fn rooted_beside<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        prefix: &Prefix,
        until: Option<&Prefix>,
        back: bool,
    ) -> Result<Option<Prefix>, DirectoryError> {
        let within = [RecordKind::Entry.code()];
        let mut cursor = self.tree.cursor_within(&within);
        match back {
            false => cursor.seek(pages, prefix)?,
            true => cursor.seek_for_prev(pages, prefix)?,
        }
        let mut value = Vec::new();
        loop {
            let Some(key) = cursor.key() else {
                return Ok(None);
            };
            let damaged = |what: Malformed| damaged(&[&within[..], key].concat(), what.0);
            let beside = decode_prefix(key).map_err(damaged)?;
            let beyond = until.is_some_and(|until| match back {
                false => beside >= *until,
                true => beside <= *until,
            });
            if beyond {
                return Ok(None);
            }
            value.clear();
            // A record that a page step took out under the walk is passed.
            if beside != *prefix && cursor.value(pages, &mut value)? {
                let entry = Entry::decode(std::mem::take(&mut value)).map_err(damaged)?;
                if self.is_live(&entry) && entry.root().is_some() {
                    return Ok(Some(beside));
                }
            }
            match back {
                false => cursor.next(pages)?,
                true => cursor.prev(pages)?,
            }
        }
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
            .map_err(|what| damaged(&HEADER_KEY, what.0))
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
        Records::new(&self.tree, &children_key(parent)).step(pages)
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
            if self.started {
                self.cursor.next(pages)?;
            } else {
                self.cursor.seek_to_first(pages)?;
                self.started = true;
            }
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

    /// Goes to the next record and reads it, from its key without the
    /// bytes the walk is within and from its value.
    fn next<T, P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        decode: impl FnOnce(&[u8], &[u8]) -> Result<T, Malformed>,
    ) -> Result<Option<T>, DirectoryError> {
        if !self.step(pages)? {
            return Ok(None);
        }
        let key = self.cursor.key().unwrap_or_default();
        let read = decode(key, &self.value);
        read.map(Some)
            .map_err(|what| damaged(&[&self.within, key].concat(), what.0))
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
        self.0.next(pages, |key, value| {
            let (position, key) = decode_child_key(key)?;
            let (prefix, id) = decode_child(value)?;
            let key = key.to_vec();
            Ok(Child {
                position,
                key,
                prefix,
                id,
            })
        })
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
        self.0.next(pages, |key, value| {
            Ok((decode_prefix(key)?, Entry::decode(value.to_vec())?))
        })
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
        self.0.next(pages, |key, value| {
            let (id, root) = (decode_id(key)?, decode_dropped(value)?);
            Ok(Dropped { id, root })
        })
    }
}
