//! Writing the directory: entering a Merk, naming its root, and dropping
//! or clearing it.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use lignin_format::CellAddress;

use super::{
    error::DirectoryError,
    record::{
        child_key, dropped_key, encode_child, encode_dropped, encode_header, entry_key, root_key,
        Dropped, Entry, Fields, Host, MerkId, Prefix, Root, HEADER_KEY,
    },
    Directory,
};
use crate::{
    flat::{FlatPages, FlatPagesMut},
    fragment::Carried,
};

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
        let was = self.entry.root();
        let root = match self.now {
            Now::At(address) => was.map(|root| Root { address, ..root }),
            Now::Named(root) => root,
        };
        let fields = self.entry.fields();
        Fields { root, ..fields }.encode()
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
        let left = self.free(pages, prefix)?;
        let place = match host {
            Some(host) => {
                let place = self.below(pages, &host, None)?;
                self.host_is_free(pages, &host, &place)?;
                Some(place)
            }
            None => None,
        };
        let mut fields = Fields {
            id: MerkId::FIRST,
            root: None,
            tree_type,
            host,
            lineage: place.as_ref().map_or(&[], |place| &place.lineage),
        };
        fields.encode()?;

        fields.id = self.take_id(pages)?;
        let entry = fields.encode()?;
        self.displace(pages, prefix, left.as_ref())?;
        self.tree.put(pages, &entry_key(prefix), entry.as_bytes())?;
        if let Some(place) = &place {
            let child = encode_child(prefix, fields.id);
            self.tree.put(pages, &place.child, &child)?;
        }
        Ok(fields.id)
    }

    /// Gives the Merk under `prefix` another tree type.
    pub fn set_tree_type<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        prefix: &Prefix,
        tree_type: &[u8],
    ) -> Result<(), DirectoryError> {
        let entry = self.live(pages, prefix)?;
        let fields = entry.fields();
        let changed = Fields {
            tree_type,
            ..fields
        }
        .encode()?;
        if changed != entry {
            self.tree
                .put(pages, &entry_key(prefix), changed.as_bytes())?;
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
        let mut merks: Vec<Changed<'_>> = Vec::new();
        let mut places: HashMap<Prefix, usize> = HashMap::new();
        let mut became_roots = Vec::new();
        for change in changes {
            let &RootChange::Carried { from, to } = change else {
                continue;
            };
            let found = match self.root_owner(pages, from)? {
                Some(prefix) => self.open(pages, &prefix)?.map(|entry| (prefix, entry)),
                None => None,
            };
            let Some((prefix, entry)) = found else {
                became_roots.push((from, to));
                continue;
            };
            if entry.root().map(|root| root.address) != Some(from) {
                return Err(DirectoryError::Damaged {
                    key: root_key(from).to_vec(),
                    what: "the Merk has its root elsewhere",
                });
            }
            if places.insert(prefix, merks.len()).is_some() {
                return Err(DirectoryError::Twice { prefix });
            }
            let now = Now::At(to);
            merks.push(Changed { prefix, entry, now });
        }
        let mut named = HashSet::new();
        for change in changes {
            let &RootChange::Named { prefix, root } = change else {
                continue;
            };
            if !named.insert(*prefix) {
                return Err(DirectoryError::Twice { prefix: *prefix });
            }
            let now = Now::Named(root);
            match places.get(prefix) {
                Some(&place) => merks[place].now = now,
                None => {
                    let entry = self.live(pages, prefix)?;
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
            if let Some(holder) = self.root_owner(pages, address)?
                && !places.contains_key(&holder)
            {
                return Err(DirectoryError::RootTaken { address });
            }
        }
        if let Some(&(address, _)) = became_roots.iter().find(|(_, to)| !roots.contains(to)) {
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
                self.tree.delete(pages, &root_key(was))?;
            }
        }
        for (merk, entry) in merks.iter().zip(&entries) {
            if let Some(address) = merk.address().filter(|now| Some(*now) != merk.was()) {
                self.tree.put(pages, &root_key(address), &merk.prefix)?;
            }
            if *entry != merk.entry {
                self.tree
                    .put(pages, &entry_key(&merk.prefix), entry.as_bytes())?;
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
        let entry = self.live(pages, prefix)?;
        let dropped = self.dropped_of(pages, prefix, &entry)?;
        if let Some(root) = dropped.root {
            self.tree.delete(pages, &root_key(root))?;
        }
        if let (Some(host), Some(parent)) = (entry.host(), entry.parent_id()) {
            let child = child_key(parent, host.position, host.key);
            self.tree.delete(pages, &child)?;
        }
        self.tree.delete(pages, &entry_key(prefix))?;
        self.leave(pages, dropped)?;
        Ok(dropped)
    }

    /// Clears the Merk under `prefix`: a drop that keeps the host
    /// (`tree-layout.md` D4). The Merk is entered as one with no node and
    /// under another id, and what it held is left under the id it had, as
    /// by [`Self::unlink`].
    ///
    /// Returns `None`, and writes nothing, for a Merk with no node.
    pub fn clear<P: FlatPagesMut + ?Sized>(
        &mut self,
        pages: &mut P,
        prefix: &Prefix,
    ) -> Result<Option<Dropped>, DirectoryError> {
        let entry = self.live(pages, prefix)?;
        let Some(root) = entry.root() else {
            return Ok(None);
        };
        let dropped = Dropped {
            id: entry.id(),
            root: Some(root.address),
        };
        let id = self.take_id(pages)?;
        let fields = entry.fields();
        let cleared = Fields {
            id,
            root: None,
            ..fields
        };
        let cleared = cleared.encode()?;
        self.tree.delete(pages, &root_key(root.address))?;
        if let (Some(host), Some(parent)) = (entry.host(), entry.parent_id()) {
            let child = child_key(parent, host.position, host.key);
            self.tree.put(pages, &child, &encode_child(prefix, id))?;
        }
        self.tree
            .put(pages, &entry_key(prefix), cleared.as_bytes())?;
        self.leave(pages, dropped)?;
        Ok(Some(dropped))
    }

    /// The entry that is in the way of a Merk to be entered under
    /// `prefix`: one that a dropped Merk left. A live one refuses.
    pub(super) fn free<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        prefix: &Prefix,
    ) -> Result<Option<Entry>, DirectoryError> {
        match self.stored(pages, prefix)? {
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
        pages: &P,
        host: &Host<'_>,
        moving: Option<(&Prefix, MerkId)>,
    ) -> Result<Place, DirectoryError> {
        let parent = self.open(pages, host.parent)?;
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
        pages: &P,
        host: &Host<'_>,
        place: &Place,
    ) -> Result<(), DirectoryError> {
        match self.get(pages, &place.child)? {
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
    pub(super) fn displace<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        prefix: &Prefix,
        left: Option<&Entry>,
    ) -> Result<(), DirectoryError> {
        let Some(root) = left.and_then(Entry::root) else {
            return Ok(());
        };
        if self.root_owner(pages, root.address)? == Some(*prefix) {
            self.tree.delete(pages, &root_key(root.address))?;
        }
        Ok(())
    }

    pub(super) fn take_id<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
    ) -> Result<MerkId, DirectoryError> {
        let id = self.next_id(pages)?;
        let next = id.get().checked_add(1).and_then(MerkId::new);
        let next = next.ok_or(DirectoryError::IdsExhausted)?;
        self.tree.put(pages, &HEADER_KEY, &encode_header(next))?;
        Ok(id)
    }

    /// What dropping the Merk `entry` leaves. A Merk with no root has no
    /// node, and so hosts no Merk.
    fn dropped_of<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        prefix: &Prefix,
        entry: &Entry,
    ) -> Result<Dropped, DirectoryError> {
        let root = entry.root().map(|root| root.address);
        if root.is_none() && self.has_children(pages, entry.id())? {
            return Err(DirectoryError::NotEmpty { prefix: *prefix });
        }
        Ok(Dropped {
            id: entry.id(),
            root,
        })
    }

    /// Records what a Merk left. From here on the Merks below it are not
    /// in use.
    fn leave<P: FlatPagesMut + ?Sized>(
        &mut self,
        pages: &mut P,
        dropped: Dropped,
    ) -> Result<(), DirectoryError> {
        let record = encode_dropped(dropped.root);
        self.tree.put(pages, &dropped_key(dropped.id), &record)?;
        if dropped.root.is_some() {
            Arc::make_mut(&mut self.dropped).insert(dropped.id);
        }
        Ok(())
    }
}

/// Where a hosted Merk is entered.
pub(super) struct Place {
    /// The ids of the Merks it is below, as an entry has them.
    pub lineage: Vec<u8>,
    /// The key of its child record.
    pub child: Vec<u8>,
}
