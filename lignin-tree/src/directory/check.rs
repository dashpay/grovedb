//! Checking that the records of the directory agree with each other.

use std::collections::{BTreeSet, HashSet};

use lignin_format::CellAddress;

use super::{
    error::DirectoryError,
    read::{damaged, Children, Records},
    record::{dropped_key, entry_key, Child, MerkId, Prefix, RecordKind},
    Directory,
};
use crate::flat::FlatPages;

/// What a directory holds, as [`Directory::check`] counted it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Census {
    /// The Merks in use.
    pub live: u64,
    /// The entries of Merks below a dropped or cleared one, which the
    /// reclaimer has not taken yet.
    pub left_over: u64,
    /// The dropped and cleared Merks that have their record: what they
    /// left is not reclaimed, or not all of it.
    pub dropped: u64,
}

/// A Merk whose children the check is walking.
struct Level {
    children: Children,
    id: MerkId,
    /// The prefix the Merk is entered under, or was.
    prefix: Prefix,
    /// Whether the Merk is in use.
    live: bool,
    /// Whether it has a node, as far as an entry says: only such a Merk
    /// hosts another.
    rooted: bool,
}

/// What the check has found so far.
struct Found {
    /// The next id to give out.
    next: MerkId,
    /// The ids of the entries and of the dropped Merks.
    ids: HashSet<MerkId>,
    /// The ids that a child record names.
    hosted: HashSet<MerkId>,
    census: Census,
}

impl Directory {
    /// Checks the records against each other, and counts them.
    ///
    /// - Every Merk in use is reached from a Merk with no host through
    ///   child records, each of which has its entry. The entry names the
    ///   host the record is under and the ids of the Merks above it, and
    ///   the Merk that hosts has a root.
    /// - Every other entry is reached in the same way from the record of a
    ///   dropped Merk, and no lookup returns it.
    /// - Every root of a Merk in use has its record, every root record has
    ///   its entry, no two Merks have their root in one cell, and no id is
    ///   held twice.
    /// - The keyspace holds no other record.
    /// - The handle has the dropped Merks that the pages have.
    ///
    /// What is not checked here needs the tree pages and the elements: that
    /// a root's address is the root cell's, and that an entry agrees with
    /// the element that hosts its Merk. That is the invariant checker's
    /// (VER-07), which finds every record through [`Self::entries`],
    /// [`Self::children`] and [`Self::drops`].
    pub fn check<P: FlatPages + ?Sized>(&self, pages: &P) -> Result<Census, DirectoryError> {
        let next = self.next_id(pages)?;
        let mut ids = HashSet::new();
        let mut drops = Vec::new();
        let mut dead = BTreeSet::new();
        let mut left_cells: HashSet<CellAddress> = HashSet::new();
        let mut walk = self.drops();
        while let Some(dropped) = walk.next(pages)? {
            if dropped.id >= next {
                let what = "an id that was not given out";
                return Err(damaged(&dropped_key(dropped.id), what));
            }
            ids.insert(dropped.id);
            if let Some(root) = dropped.root {
                dead.insert(dropped.id);
                left_cells.insert(root);
            }
            drops.push(dropped);
        }
        if dead != *self.dropped {
            let key = [RecordKind::Dropped.code()];
            return Err(damaged(&key, "the handle has other dropped Merks"));
        }

        let mut tops = Vec::new();
        let (mut entries, mut roots) = (0u64, 0u64);
        let mut walk = self.entries();
        while let Some((prefix, entry)) = walk.next(pages)? {
            let key = entry_key(&prefix);
            entries += 1;
            if entry.id() >= next {
                return Err(damaged(&key, "an id that was not given out"));
            }
            if !ids.insert(entry.id()) {
                return Err(damaged(&key, "an id that another Merk has"));
            }
            if let Some(root) = entry.root() {
                if left_cells.contains(&root.address) {
                    return Err(damaged(&key, "a root in the cell of a dropped Merk"));
                }
                // The reclaimer takes the record of a left-over root
                // before the entry.
                match self.root_owner(pages, root.address)? {
                    Some(owner) if owner == prefix => roots += 1,
                    Some(_) => return Err(damaged(&key, "a root in the cell of another Merk")),
                    None if self.is_live(&entry) => {
                        return Err(damaged(&key, "a root that has no record"));
                    }
                    None => {}
                }
            }
            if entry.host().is_none() {
                tops.push((prefix, entry.id(), entry.root().is_some()));
            }
        }
        let children = self.count(pages, &[RecordKind::Child.code()])?;
        if self.count(pages, &[RecordKind::Root.code()])? != roots {
            let key = [RecordKind::Root.code()];
            return Err(damaged(&key, "a root record of no entry"));
        }
        let known = 1 + entries + children + roots + drops.len() as u64;
        if self.count(pages, &[])? != known {
            return Err(damaged(&[], "a record of no kind"));
        }

        let census = Census {
            live: tops.len() as u64,
            left_over: 0,
            dropped: drops.len() as u64,
        };
        let mut found = Found {
            next,
            ids,
            hosted: HashSet::new(),
            census,
        };
        for (prefix, id, rooted) in tops {
            let top = Level {
                children: self.children(id),
                id,
                prefix,
                live: true,
                rooted,
            };
            self.walk(pages, top, &mut found)?;
        }
        for dropped in drops {
            let top = Level {
                children: self.children(dropped.id),
                id: dropped.id,
                prefix: Prefix::default(),
                live: false,
                rooted: dropped.root.is_some(),
            };
            self.walk(pages, top, &mut found)?;
        }
        if found.hosted.len() as u64 != children {
            let key = [RecordKind::Child.code()];
            return Err(damaged(&key, "a child record that no Merk leads to"));
        }
        if found.census.live + found.census.left_over != entries {
            let key = [RecordKind::Entry.code()];
            return Err(damaged(&key, "an entry that no Merk leads to"));
        }
        Ok(found.census)
    }

    /// How many records have a key that begins with `within`.
    fn count<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        within: &[u8],
    ) -> Result<u64, DirectoryError> {
        let mut records = Records::new(&self.tree, within);
        let mut count = 0;
        while records.step(pages)? {
            count += 1;
        }
        Ok(count)
    }

    /// Walks the Merks below `top`, which are in use if `top` is, and left
    /// over if it is not.
    fn walk<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        top: Level,
        found: &mut Found,
    ) -> Result<(), DirectoryError> {
        // The ids of the Merks above the children of the level that is
        // walked, as an entry has them, for the Merks in use.
        let mut lineage = top.id.encode().to_vec();
        let mut levels = vec![top];
        while let Some(level) = levels.last_mut() {
            let Some(child) = level.children.next(pages)? else {
                levels.pop();
                lineage.truncate(lineage.len().saturating_sub(MerkId::ENCODED_LEN));
                continue;
            };
            let key = entry_key(&child.prefix);
            if child.id >= found.next {
                return Err(damaged(&key, "an id that was not given out"));
            }
            // A Merk has one host, so no walk comes to it twice.
            if !found.hosted.insert(child.id) {
                return Err(damaged(&key, "a Merk that two hosts have"));
            }
            if !level.rooted {
                return Err(damaged(&key, "a Merk with no node hosts a Merk"));
            }
            let entry = self.stored(pages, &child.prefix)?;
            let entry = entry.filter(|entry| entry.id() == child.id);
            let (id, prefix, live) = (level.id, level.prefix, level.live);
            let mut rooted = true;
            if let Some(entry) = &entry {
                rooted = entry.root().is_some();
                let host = entry.host();
                let hosted = host.is_some_and(|host| {
                    (host.key, host.position) == (child.key.as_slice(), child.position)
                });
                if !hosted || entry.parent_id() != Some(id) {
                    return Err(damaged(&key, "an entry under the record of another host"));
                }
                if live {
                    if host.map(|host| *host.parent) != Some(prefix) {
                        return Err(damaged(&key, "an entry that names another parent"));
                    }
                    if entry.lineage_bytes() != lineage || !self.is_live(entry) {
                        return Err(damaged(&key, "an entry with other Merks above it"));
                    }
                    found.census.live += 1;
                } else {
                    if self.is_live(entry) {
                        return Err(damaged(&key, "a left-over entry that a lookup returns"));
                    }
                    found.census.left_over += 1;
                }
            } else if live {
                return Err(damaged(&key, "a Merk in use hosts a Merk with no entry"));
            } else if found.ids.contains(&child.id) {
                // Its entry was replaced or taken: no other record has
                // its id.
                return Err(damaged(
                    &key,
                    "a left-over record of a Merk that is entered",
                ));
            }
            let Child { id, prefix, .. } = child;
            lineage.extend_from_slice(&id.encode());
            levels.push(Level {
                children: self.children(id),
                id,
                prefix,
                live,
                rooted,
            });
        }
        Ok(())
    }
}
