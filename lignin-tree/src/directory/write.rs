//! Writing the directory: entering a Merk, naming its root, and dropping
//! or clearing it, and the records of a batch that are in no page yet
//! ([`Pending`]).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use lignin_format::CellAddress;

use super::{
    error::{DirectoryError, Malformed},
    read::damaged,
    record::{
        child_key, children_key, decode_child, decode_child_key, decode_header, decode_prefix,
        dropped_key, encode_child, encode_dropped, encode_header, entry_key, root_key, Child,
        Dropped, Entry, Fields, Host, MerkId, Prefix, Root, HEADER_KEY,
    },
    Directory,
};
use crate::{
    flat::{Changes, FlatPages, FlatPagesMut},
    fragment::Carried,
};

/// The records that the writes of one batch put into the directory and
/// take out of it, before they are in a page: each key once, with the
/// value it has after all of them or with none, in the order of the keys.
///
/// A write that is given one ([`Directory::add_in`] and the others whose
/// names end so) changes no page. It reads what it needs through the
/// records first and then in the pages
/// ([`Directory::open_in`]), so it sees what the writes before it put
/// there, and it puts its own records beside them. [`Directory::settle`]
/// then writes them all, each leaf of the directory once for the records
/// that go into it.
///
/// **Until they are settled the records are in no page.** A read that is
/// not given them reads pages that have nothing of them:
/// [`Directory::open`] and every other call that takes pages alone. It
/// is not for that a read of the directory before the first of them: the
/// handle has the ids of the Merks that the batch dropped or cleared
/// (below), so it opens no Merk below one of those, and it opens a
/// dropped Merk itself, whose entry the pages still have. So the records
/// are settled before anything else reads the directory, and before the
/// operation that made them ends. Records that are dropped unsettled were
/// never put: no page has anything of them.
///
/// What a handle keeps beside the pages follows the calls, not the
/// settling: the ids of the dropped Merks are in the handle from the call
/// that dropped them ([`Directory::unlink_in`]), as they are for a write
/// that is settled at once. A handle whose records were dropped unsettled
/// is loaded again ([`Directory::load`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pending {
    records: Changes,
}

impl Pending {
    /// No record.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no record is pending.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// How many records are pending: one for each key that a write put or
    /// took out, however often.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// The keys of the pending records, in their order.
    pub fn keys(&self) -> impl Iterator<Item = &[u8]> {
        self.records.keys().map(Vec::as_slice)
    }

    pub(super) fn put(&mut self, key: &[u8], value: &[u8]) {
        self.records.insert(key.to_vec(), Some(value.to_vec()));
    }

    pub(super) fn delete(&mut self, key: &[u8]) {
        self.records.insert(key.to_vec(), None);
    }

    /// What is pending under `key`: its value, `Some(None)` for a record
    /// that goes, and `None` where nothing is pending.
    fn get(&self, key: &[u8]) -> Option<Option<&[u8]>> {
        self.records.get(key).map(Option::as_deref)
    }
}

/// A record of the directory: its key and its value.
type Record = (Vec<u8>, Vec<u8>);

/// What settling the records of a batch did ([`Directory::settle`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settled {
    /// The records that were pending.
    pub records: u64,
    /// The leaves of the directory that the records changed
    /// ([`Written::leaves`](crate::flat::Written::leaves)). A record that
    /// is put as the pages have it changes none where its value is kept
    /// in its page, and its leaf where the value is kept out of line.
    pub leaves: u64,
    /// The images of leaves that were written for them
    /// ([`Written::leaf_writes`](crate::flat::Written::leaf_writes)): one
    /// at most one for each of those leaves, and one more for each
    /// further piece of a leaf that is cut.
    pub leaf_writes: u64,
}

/// What one write did to the root of a Merk.
///
/// The roots that one write changed are stated together
/// ([`Directory::set_roots`]): a cell that one root left can be the cell
/// another root is in afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootChange<'a> {
    /// The Merk under `prefix` has this root now, or none: another node
    /// became its root, it got its first node, or it lost its last.
    Named {
        /// The Merk's prefix.
        prefix: &'a Prefix,
        /// Its root after the write.
        root: Option<Root<'a>>,
    },
    /// The root cell that was at `from` is at `to`. It is the root of the
    /// same Merk, which is found by the address: the write need not have
    /// opened that Merk (`tree-layout.md` S3, P5).
    Carried {
        /// The address of the root's cell before the write.
        from: CellAddress,
        /// Its address after the write.
        to: CellAddress,
    },
}

impl RootChange<'_> {
    /// The change that a carried cell is for the directory: one for the
    /// root of a hosted Merk ([`Carried::hosted_by`]), and none for any
    /// other cell.
    ///
    /// The root of a Merk with no host is not told apart from the other
    /// cells by a report. Whoever holds its root pointer names it
    /// ([`Self::Named`]).
    pub fn carried(carried: &Carried) -> Option<Self> {
        carried.hosted_by.map(|_| Self::Carried {
            from: carried.from,
            to: carried.to.address(),
        })
    }
}

/// A Merk that keeps its nodes and is stated anew
/// ([`Directory::restate`]): with the tree type it has from here on, and
/// with its place among the Merks of its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restated<'a> {
    /// The Merk's prefix.
    pub prefix: &'a Prefix,
    /// The tree type it has from here on: bytes that are kept and given
    /// back, as those it was entered with.
    pub tree_type: &'a [u8],
    /// The position it has among the Merks of its host from here on, or
    /// `None` for the one it has. A Merk with no host has no position.
    pub position: Option<u8>,
}

/// The root a Merk has after a change.
#[derive(Debug, Clone, Copy)]
enum Now<'a> {
    /// The root it had, in another cell.
    At(CellAddress),
    /// The root that was named.
    Named(Option<Root<'a>>),
}

/// A Merk whose root one change names, as it is entered.
struct Changed<'a> {
    prefix: Prefix,
    entry: Entry,
    now: Now<'a>,
}

impl Changed<'_> {
    fn address(&self) -> Option<CellAddress> {
        match self.now {
            Now::At(address) => Some(address),
            Now::Named(root) => root.map(|root| root.address),
        }
    }

    fn was(&self) -> Option<CellAddress> {
        self.entry.root().map(|root| root.address)
    }

    /// The entry after the change.
    fn encode(&self) -> Result<Entry, DirectoryError> {
        let root = match self.now {
            Now::At(address) => self.entry.root().map(|root| Root { address, ..root }),
            Now::Named(root) => root,
        };
        let fields = Fields {
            root,
            ..self.entry.fields()
        };
        fields.encode()
    }
}

impl Directory {
    /// Enters a Merk under `prefix`, with no node yet, and returns the id
    /// it has from here on.
    ///
    /// `host` is the element that hosts the Merk: the Merk it is in must be
    /// entered, and is read for the ids of the Merks above. A Merk with no
    /// host is the root of the grove, or one that stands alone.
    ///
    /// A prefix under which a Merk below a dropped one is still entered is
    /// free: the new Merk takes the place of that entry, and holds nothing
    /// of it (`transactions.md` CV9).
    pub fn add<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        prefix: &Prefix,
        tree_type: &[u8],
        host: Option<Host<'_>>,
    ) -> Result<MerkId, DirectoryError> {
        let mut pending = Pending::new();
        let id = self.add_in(&mut pending, pages, prefix, tree_type, host)?;
        self.settle(pages, pending)?;
        Ok(id)
    }

    /// [`Self::add`] among the records of a batch ([`Pending`]): the
    /// Merk's records are put beside them, and no page is changed.
    pub fn add_in<P: FlatPages + ?Sized>(
        &self,
        pending: &mut Pending,
        pages: &P,
        prefix: &Prefix,
        tree_type: &[u8],
        host: Option<Host<'_>>,
    ) -> Result<MerkId, DirectoryError> {
        let left = self.free(pending, pages, prefix)?;
        let place = match host {
            Some(host) => {
                let place = self.below(pending, pages, &host, None)?;
                self.host_is_free(pending, pages, &host, &place)?;
                Some(place)
            }
            None => None,
        };
        let id = self.next_id_in(pending, pages)?;
        let fields = Fields {
            id,
            root: None,
            tree_type,
            host,
            lineage: place.as_ref().map_or(&[], |place| &place.lineage),
        };
        let entry = fields.encode()?;

        Self::give_out(pending, id)?;
        self.displace(pending, pages, prefix, left.as_ref())?;
        let child = place.as_ref().map(|place| &place.child[..]);
        Self::enter(pending, prefix, &entry, child);
        Ok(id)
    }

    /// Gives the Merk under `prefix` another tree type.
    pub fn set_tree_type<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        prefix: &Prefix,
        tree_type: &[u8],
    ) -> Result<(), DirectoryError> {
        let entry = self.live(pages, prefix)?;
        let fields = Fields {
            tree_type,
            ..entry.fields()
        };
        let changed = fields.encode()?;
        if changed != entry {
            self.tree
                .put(pages, &entry_key(prefix), changed.as_bytes())?;
        }
        Ok(())
    }

    /// States anew, in one statement, Merks that keep their nodes: each
    /// with the tree type it has from here on, and with its place among
    /// the Merks of its host where that is another ([`Restated`]). It is
    /// what a conversion of a tree in place asks: the tree is neither
    /// dropped nor entered again, its element is of another type, and an
    /// element that hosts several Merks can name them in another order.
    ///
    /// Every Merk keeps its id, its root, its host and the Merks below it.
    /// The Merks are stated together because two of them can trade places:
    /// a place that one of them takes is free, or is left by another of
    /// the same statement.
    ///
    /// The statement is refused, and nothing is written, if a Merk is
    /// named twice ([`DirectoryError::Twice`]) or is not in use
    /// ([`DirectoryError::NotFound`]), if a place is given to a Merk with
    /// no host ([`DirectoryError::NoParent`], with the Merk's own prefix:
    /// no Merk holds a host of it), and if a place is taken by a Merk
    /// that does not leave it, or by two of the statement
    /// ([`DirectoryError::HostTaken`]).
    pub fn restate<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        changes: &[Restated<'_>],
    ) -> Result<(), DirectoryError> {
        let mut pending = Pending::new();
        self.restate_in(&mut pending, pages, changes)?;
        self.settle(pages, pending)?;
        Ok(())
    }

    /// [`Self::restate`] among the records of a batch ([`Pending`]): the
    /// Merks are read as those records leave them, their records are put
    /// beside them, and no page is changed.
    pub fn restate_in<P: FlatPages + ?Sized>(
        &self,
        pending: &mut Pending,
        pages: &P,
        changes: &[Restated<'_>],
    ) -> Result<(), DirectoryError> {
        struct Stated {
            prefix: Prefix,
            was: Entry,
            now: Entry,
            /// The keys of the child record it leaves and of the one it
            /// takes, where it moves.
            moved: Option<(Vec<u8>, Vec<u8>)>,
        }
        let mut named = HashSet::new();
        let mut stated = Vec::with_capacity(changes.len());
        for change in changes {
            let prefix = *change.prefix;
            if !named.insert(prefix) {
                return Err(DirectoryError::Twice { prefix });
            }
            let was = self.live_in(pending, pages, &prefix)?;
            let host = match (was.host(), change.position) {
                (None, Some(_)) => return Err(DirectoryError::NoParent { parent: prefix }),
                (Some(host), Some(position)) => Some(Host { position, ..host }),
                (host, None) => host,
            };
            let fields = Fields {
                tree_type: change.tree_type,
                host,
                ..was.fields()
            };
            let now = fields.encode()?;
            let moved = match (was.child_key(), now.child_key()) {
                (Some(from), Some(to)) if from != to => Some((from, to)),
                _ => None,
            };
            stated.push(Stated {
                prefix,
                was,
                now,
                moved,
            });
        }
        let moves = || {
            stated
                .iter()
                .filter_map(|merk| Some((merk, merk.moved.as_ref()?)))
        };
        let left: HashSet<&[u8]> = moves().map(|(_, (from, _))| &from[..]).collect();
        let mut taken = HashSet::new();
        for (merk, (_, to)) in moves() {
            let free = left.contains(&to[..]) || self.get_in(pending, pages, to)?.is_none();
            if !taken.insert(&to[..]) || !free {
                let host = merk.now.host();
                let what = "a hosted Merk whose entry names no host";
                let host = host.ok_or_else(|| damaged(&entry_key(&merk.prefix), what))?;
                return Err(DirectoryError::HostTaken {
                    parent: *host.parent,
                    position: host.position,
                });
            }
        }

        // Every place that is left is given up before one is taken: a Merk
        // may be where another was.
        for (_, (from, _)) in moves() {
            pending.delete(from);
        }
        for merk in &stated {
            if let Some((_, to)) = &merk.moved {
                let record = encode_child(&merk.prefix, merk.now.id());
                pending.put(to, &record);
            }
            if merk.now != merk.was {
                let key = entry_key(&merk.prefix);
                pending.put(&key, merk.now.as_bytes());
            }
        }
        Ok(())
    }

    /// States the roots that one write changed: in the same operation as
    /// the write, and so in the same commit (`tree-layout.md` S3,
    /// `transactions.md` SC2).
    ///
    /// - [`RootChange::Named`] for every Merk whose root is another node
    ///   than before, or that has its first node or none any more, with the
    ///   address the write's report gives.
    /// - [`RootChange::Carried`] for every carried cell that is the root of
    ///   a hosted Merk ([`RootChange::carried`]).
    ///
    /// A Merk may be stated in both ways, and then has the root it is
    /// named with. A cell that is said to be carried from an address at
    /// which no Merk has its root must be the root that a Merk of the same
    /// change is named with: it became a root in this write.
    ///
    /// The change is refused, and nothing is written, if two Merks would
    /// have their root in one cell.
    pub fn set_roots<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        changes: &[RootChange<'_>],
    ) -> Result<(), DirectoryError> {
        let mut pending = Pending::new();
        self.set_roots_in(&mut pending, pages, changes)?;
        self.settle(pages, pending)?;
        Ok(())
    }

    /// [`Self::set_roots`] among the records of a batch ([`Pending`]): the
    /// roots are resolved against the entries as those records leave
    /// them, the records of the change are put beside them, and no page
    /// is changed.
    pub fn set_roots_in<P: FlatPages + ?Sized>(
        &self,
        pending: &mut Pending,
        pages: &P,
        changes: &[RootChange<'_>],
    ) -> Result<(), DirectoryError> {
        let mut merks: Vec<Changed<'_>> = Vec::new();
        let mut places: HashMap<Prefix, usize> = HashMap::new();
        let mut became_roots = Vec::new();
        for change in changes {
            let &RootChange::Carried { from, to } = change else {
                continue;
            };
            let found = match self.root_owner_in(pending, pages, from)? {
                Some(prefix) => {
                    let entry = self.open_in(pending, pages, &prefix)?;
                    entry.map(|entry| (prefix, entry))
                }
                None => None,
            };
            let Some((prefix, entry)) = found else {
                became_roots.push((from, to));
                continue;
            };
            if entry.root().map(|root| root.address) != Some(from) {
                let what = "the Merk has its root elsewhere";
                return Err(damaged(&root_key(from), what));
            }
            if places.insert(prefix, merks.len()).is_some() {
                return Err(DirectoryError::Twice { prefix });
            }
            let now = Now::At(to);
            merks.push(Changed { prefix, entry, now });
        }
        let mut named = HashSet::new();
        let mut named_roots = HashSet::new();
        for change in changes {
            let &RootChange::Named { prefix, root } = change else {
                continue;
            };
            if !named.insert(*prefix) {
                return Err(DirectoryError::Twice { prefix: *prefix });
            }
            named_roots.extend(root.map(|root| root.address));
            let now = Now::Named(root);
            match places.get(prefix) {
                Some(&place) => merks[place].now = now,
                None => {
                    let entry = self.live_in(pending, pages, prefix)?;
                    places.insert(*prefix, merks.len());
                    merks.push(Changed {
                        prefix: *prefix,
                        entry,
                        now,
                    });
                }
            }
        }

        let mut roots = HashSet::new();
        for merk in &merks {
            let Some(address) = merk.address() else {
                continue;
            };
            if !roots.insert(address) {
                return Err(DirectoryError::RootTaken { address });
            }
            if merk.was() == Some(address) {
                continue;
            }
            // Another Merk may have its root there only if this change
            // takes it away.
            if let Some(holder) = self.root_owner_in(pending, pages, address)?
                && !places.contains_key(&holder)
            {
                return Err(DirectoryError::RootTaken { address });
            }
        }
        let unknown = became_roots
            .iter()
            .find(|(_, to)| !named_roots.contains(to));
        if let Some(&(address, _)) = unknown {
            return Err(DirectoryError::UnknownRoot { address });
        }
        let entries: Vec<Entry> = merks
            .iter()
            .map(Changed::encode)
            .collect::<Result<_, _>>()?;

        // Every address that a root left is given up before one is taken:
        // a root may be where another was.
        for merk in &merks {
            if let Some(was) = merk.was().filter(|was| Some(*was) != merk.address()) {
                pending.delete(&root_key(was));
            }
        }
        for (merk, entry) in merks.iter().zip(&entries) {
            if let Some(address) = merk.address().filter(|now| Some(*now) != merk.was()) {
                pending.put(&root_key(address), &merk.prefix);
            }
            if *entry != merk.entry {
                pending.put(&entry_key(&merk.prefix), entry.as_bytes());
            }
        }
        Ok(())
    }

    /// Drops the Merk under `prefix`: its entry goes, and one record of
    /// what it left takes its place, for the reclaimer
    /// (`tree-layout.md` D1).
    ///
    /// Nothing of the Merks below it is read or written. From this call
    /// on no lookup of this handle returns one of them, and from the
    /// commit of the write on no lookup at all
    /// ([`Directory::load`], `transactions.md` CV9).
    pub fn unlink<P: FlatPagesMut + ?Sized>(
        &mut self,
        pages: &mut P,
        prefix: &Prefix,
    ) -> Result<Dropped, DirectoryError> {
        let mut pending = Pending::new();
        let dropped = self.unlink_in(&mut pending, pages, prefix)?;
        self.settle(pages, pending)?;
        Ok(dropped)
    }

    /// [`Self::unlink`] among the records of a batch ([`Pending`]): the
    /// Merk is read as those records leave it, its records are put beside
    /// them, and no page is changed. The handle has the id of a dropped
    /// Merk that had a root from this call on, as after [`Self::unlink`].
    pub fn unlink_in<P: FlatPages + ?Sized>(
        &mut self,
        pending: &mut Pending,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Dropped, DirectoryError> {
        let entry = self.live_in(pending, pages, prefix)?;
        let root = entry.root().map(|root| root.address);
        // A Merk with no root has no node, and so hosts no Merk.
        if root.is_none() && self.has_children_in(pending, pages, entry.id())? {
            return Err(DirectoryError::NotEmpty { prefix: *prefix });
        }
        let id = entry.id();
        let dropped = Dropped { id, root };
        if let Some(root) = root {
            pending.delete(&root_key(root));
        }
        if let Some(child) = entry.child_key() {
            pending.delete(&child);
        }
        pending.delete(&entry_key(prefix));
        self.leave(pending, dropped);
        Ok(dropped)
    }

    /// Clears the Merk under `prefix`: a drop that keeps the host
    /// (`tree-layout.md` D4). The Merk is entered as one with no node and
    /// under another id, and what it held is left under the id it had, as
    /// by [`Self::unlink`].
    ///
    /// Returns `None`, and writes nothing, for a Merk with no node: it has
    /// nothing to leave, and keeps its id.
    pub fn clear<P: FlatPagesMut + ?Sized>(
        &mut self,
        pages: &mut P,
        prefix: &Prefix,
    ) -> Result<Option<Dropped>, DirectoryError> {
        let mut pending = Pending::new();
        let dropped = self.clear_in(&mut pending, pages, prefix)?;
        self.settle(pages, pending)?;
        Ok(dropped)
    }

    /// [`Self::clear`] among the records of a batch ([`Pending`]): the
    /// Merk is read as those records leave it, its records are put beside
    /// them, and no page is changed. The handle has the id that a Merk
    /// with a root had from this call on, as after [`Self::clear`].
    pub fn clear_in<P: FlatPages + ?Sized>(
        &mut self,
        pending: &mut Pending,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Dropped>, DirectoryError> {
        let entry = self.live_in(pending, pages, prefix)?;
        let Some(root) = entry.root() else {
            return Ok(None);
        };
        let dropped = Dropped {
            id: entry.id(),
            root: Some(root.address),
        };
        let id = self.next_id_in(pending, pages)?;
        let fields = Fields {
            id,
            root: None,
            ..entry.fields()
        };
        let cleared = fields.encode()?;

        Self::give_out(pending, id)?;
        pending.delete(&root_key(root.address));
        Self::enter(pending, prefix, &cleared, entry.child_key().as_deref());
        self.leave(pending, dropped);
        Ok(Some(dropped))
    }

    /// Writes the records of a batch into the pages, each leaf of the
    /// directory once for the records that go into it
    /// ([`FlatTree::write`](crate::flat::FlatTree::write)), and says how
    /// many leaves that was.
    ///
    /// The records are those that the writes of one operation put into
    /// `pending`, read from this directory as `pages` have it. After an
    /// error the writer's pages are given up, as after any write of the
    /// directory that failed in a page.
    pub fn settle<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        pending: Pending,
    ) -> Result<Settled, DirectoryError> {
        if pending.is_empty() {
            return Ok(Settled::default());
        }
        let written = self.tree.write(pages, &pending.records)?;
        #[cfg(any(test, feature = "held_reads"))]
        self.held_to_the_pages(pages, &pending);
        Ok(Settled {
            records: pending.records.len() as u64,
            leaves: written.leaves,
            leaf_writes: written.leaf_writes,
        })
    }

    /// Holds the pages to the records that were settled into them: a
    /// lookup of each key finds the value that was pending under it, or
    /// none under a key whose record went, or the call panics. That is so
    /// in a test build of this crate, and in a build with the feature
    /// `held_reads`. A lookup that fails is not held.
    #[cfg(any(test, feature = "held_reads"))]
    fn held_to_the_pages<P: FlatPages + ?Sized>(&self, pages: &P, pending: &Pending) {
        for (key, record) in &pending.records {
            if let Ok(found) = self.get(pages, key) {
                assert_eq!(
                    found.as_deref(),
                    record.as_deref(),
                    "the record of the directory under the key {key:?}: in the pages, and as \
                     it was settled"
                );
            }
        }
    }

    /// The record under `key` as the records of a batch leave it: what is
    /// pending under the key, and where nothing is, what the pages have.
    fn get_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, DirectoryError> {
        match pending.get(key) {
            Some(record) => Ok(record.map(<[u8]>::to_vec)),
            None => self.get(pages, key),
        }
    }

    /// [`Self::stored`] as the records of a batch leave the directory
    /// ([`Pending`]).
    pub fn stored_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Entry>, DirectoryError> {
        let key = entry_key(prefix);
        let Some(raw) = self.get_in(pending, pages, &key)? else {
            return Ok(None);
        };
        let entry = Entry::decode(raw).map_err(|what| damaged(&key, what.0))?;
        Ok(Some(entry))
    }

    /// [`Self::open`] as the records of a batch leave the directory
    /// ([`Pending`]): the live Merk under `prefix` after the writes that
    /// put them.
    pub fn open_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Entry>, DirectoryError> {
        let entry = self.stored_in(pending, pages, prefix)?;
        Ok(entry.filter(|entry| self.is_live(entry)))
    }

    /// Whether a Merk is hosted in the Merk `parent` as the records of a
    /// batch leave the directory ([`Pending`]): a child record is under
    /// the id, pending or in the pages and not taken out by a pending
    /// record.
    ///
    /// It reads what the walk without pending records reads of the
    /// directory as the records leave it: the first child record in the
    /// order of the keys, and no other. A record of the pages that a
    /// pending record takes out is passed by its key, whatever it holds,
    /// and the value of the first one that stays is read, and not
    /// decoded. A pending record is the first where no record of the
    /// pages that stays is before it, and the pages are searched up to
    /// its key: no leaf is read whose key range begins at or after it.
    pub fn has_children_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        parent: MerkId,
    ) -> Result<bool, DirectoryError> {
        Ok(self.child_record_in(pending, pages, parent)?.is_some())
    }

    /// The first of the Merks that the nodes of the Merk `parent` host, as
    /// the records of a batch leave the directory ([`Pending`]): what
    /// [`Self::children`] gives first once they are settled. It reads
    /// what [`Self::has_children_in`] reads, and decodes that record.
    pub fn first_child_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        parent: MerkId,
    ) -> Result<Option<Child>, DirectoryError> {
        let Some((key, value)) = self.child_record_in(pending, pages, parent)? else {
            return Ok(None);
        };
        let rest = key.get(children_key(parent).len()..).unwrap_or_default();
        let read = decode_child_key(rest).and_then(|(position, host)| {
            let (prefix, id) = decode_child(&value)?;
            Ok(Child {
                position,
                key: host.to_vec(),
                prefix,
                id,
            })
        });
        read.map(Some).map_err(|what| damaged(&key, what.0))
    }

    /// The key and the value of the first child record under the id
    /// `parent`, as the records of a batch leave the directory.
    ///
    /// The pages are searched up to the first pending record under the
    /// id, and no leaf is read whose key range begins at or after its
    /// key ([`FlatTree::first_under`](crate::flat::FlatTree)): the search
    /// reads the pages that a search reads once the records are settled.
    /// A bucket of long keys that the search comes to under the id is
    /// damage, and is reported: no key of a directory is that long, and
    /// a record of a Merk below could not be told from it by passing it.
    fn child_record_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        parent: MerkId,
    ) -> Result<Option<Record>, DirectoryError> {
        let within = children_key(parent);
        let under = pending.records.range(within.to_vec()..);
        let mut put = under
            .take_while(|(key, _)| key.starts_with(&within))
            .filter_map(|(key, record)| Some((key, record.as_ref()?)));
        let put = put.next();
        let end = put.map(|(key, _)| key.as_slice());
        // A record of the pages that a pending record takes out is
        // passed by its key. None of them is put again before `end`.
        let taken_out = |key: &[u8]| pending.get(key).is_some();
        let mut value = Vec::new();
        let found = (self.tree).first_under(pages, &within, end, taken_out, &mut value)?;
        Ok(match found {
            Some(key) => Some((key, value)),
            None => put.map(|(key, value)| (key.clone(), value.clone())),
        })
    }

    /// [`Self::live`] as the records of a batch leave the directory.
    fn live_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Entry, DirectoryError> {
        let entry = self.open_in(pending, pages, prefix)?;
        entry.ok_or(DirectoryError::NotFound { prefix: *prefix })
    }

    /// [`Self::root_owner`] as the records of a batch leave the
    /// directory.
    fn root_owner_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        address: CellAddress,
    ) -> Result<Option<Prefix>, DirectoryError> {
        let key = root_key(address);
        let Some(value) = self.get_in(pending, pages, &key)? else {
            return Ok(None);
        };
        let prefix = decode_prefix(&value).map_err(|what| damaged(&key, what.0))?;
        Ok(Some(prefix))
    }

    /// [`Self::next_id`] as the records of a batch leave the directory:
    /// after the ids that the writes which put them gave out.
    fn next_id_in<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
    ) -> Result<MerkId, DirectoryError> {
        let header = self.get_in(pending, pages, &HEADER_KEY)?;
        let header = header.ok_or(Malformed("the directory has no header"));
        header
            .and_then(|header| decode_header(&header))
            .map_err(|what| damaged(&HEADER_KEY, what.0))
    }

    /// The entry that is in the way of a Merk to be entered under
    /// `prefix`: one that a dropped Merk left. A live one refuses.
    pub(super) fn free<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Entry>, DirectoryError> {
        match self.stored_in(pending, pages, prefix)? {
            Some(entry) if self.is_live(&entry) => Err(DirectoryError::Exists { prefix: *prefix }),
            left => Ok(left),
        }
    }

    /// Where a Merk hosted by `host` is entered: the ids of the Merks it
    /// is below, and the key of its child record.
    ///
    /// `moving` is the Merk that is put there, if it is entered already:
    /// it does not go below itself.
    pub(super) fn below<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        host: &Host<'_>,
        moving: Option<(&Prefix, MerkId)>,
    ) -> Result<Place, DirectoryError> {
        let parent = self.open_in(pending, pages, host.parent)?;
        let parent = parent.ok_or(DirectoryError::NoParent {
            parent: *host.parent,
        })?;
        if let Some((prefix, id)) = moving
            && (parent.id() == id || parent.lineage().any(|above| above == id))
        {
            return Err(DirectoryError::Inside { prefix: *prefix });
        }
        let mut lineage = parent.lineage_bytes().to_vec();
        lineage.extend_from_slice(&parent.id().encode());
        let child = child_key(parent.id(), host.position, host.key);
        Ok(Place { lineage, child })
    }

    /// Refuses if `host` has a Merk at its position already.
    pub(super) fn host_is_free<P: FlatPages + ?Sized>(
        &self,
        pending: &Pending,
        pages: &P,
        host: &Host<'_>,
        place: &Place,
    ) -> Result<(), DirectoryError> {
        match self.get_in(pending, pages, &place.child)? {
            Some(_) => Err(DirectoryError::HostTaken {
                parent: *host.parent,
                position: host.position,
            }),
            None => Ok(()),
        }
    }

    /// Takes away what the left-over entry `left` under `prefix` has
    /// beside itself, before another entry takes its place: the record of
    /// its root. The record of its host stays for the reclaimer, which
    /// finds what was below the Merk by it.
    pub(super) fn displace<P: FlatPages + ?Sized>(
        &self,
        pending: &mut Pending,
        pages: &P,
        prefix: &Prefix,
        left: Option<&Entry>,
    ) -> Result<(), DirectoryError> {
        let Some(root) = left.and_then(Entry::root) else {
            return Ok(());
        };
        if self.root_owner_in(pending, pages, root.address)? == Some(*prefix) {
            pending.delete(&root_key(root.address));
        }
        Ok(())
    }

    /// Puts every record of the Merk under `prefix` as `entry` has it:
    /// the entry, the record of its root, and under `child` the record of
    /// its host.
    pub(super) fn enter(
        pending: &mut Pending,
        prefix: &Prefix,
        entry: &Entry,
        child: Option<&[u8]>,
    ) {
        pending.put(&entry_key(prefix), entry.as_bytes());
        if let Some(child) = child {
            pending.put(child, &encode_child(prefix, entry.id()));
        }
        if let Some(root) = entry.root() {
            pending.put(&root_key(root.address), prefix);
        }
    }

    /// Puts into the header that `id`, the next id to give out, is given
    /// out.
    fn give_out(pending: &mut Pending, id: MerkId) -> Result<(), DirectoryError> {
        let next = id.get().checked_add(1).and_then(MerkId::new);
        let next = next.ok_or(DirectoryError::IdsExhausted)?;
        pending.put(&HEADER_KEY, &encode_header(next));
        Ok(())
    }

    /// Records what a Merk left. From here on the Merks below it are not
    /// in use.
    fn leave(&mut self, pending: &mut Pending, dropped: Dropped) {
        pending.put(&dropped_key(dropped.id), &encode_dropped(dropped.root));
        if dropped.root.is_some() {
            Arc::make_mut(&mut self.dropped).insert(dropped.id);
        }
    }
}

/// Where a hosted Merk is entered.
pub(super) struct Place {
    /// The ids of the Merks it is below, as an entry has them.
    pub lineage: Vec<u8>,
    /// The key of its child record.
    pub child: Vec<u8>,
}
