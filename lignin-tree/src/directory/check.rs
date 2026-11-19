//! Checking that the records of the directory agree with each other.

use std::collections::{BTreeSet, HashSet};

use super::{
    error::DirectoryError,
    read::{Children, Records},
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

fn wrong(key: &[u8], what: &'static str) -> DirectoryError {
    DirectoryError::Damaged {
        key: key.to_vec(),
        what,
    }
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
    /// - Every root has its record, and no id is held twice.
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
        let mut tops = Vec::new();
        let (mut entries, mut rooted) = (0u64, 0u64);
        let mut walk = self.entries();
        while let Some((prefix, entry)) = walk.next(pages)? {
            let key = entry_key(&prefix);
            entries += 1;
            if entry.id() >= next {
                return Err(wrong(&key, "an id that was not given out"));
            }
            if !ids.insert(entry.id()) {
                return Err(wrong(&key, "an id that another entry has"));
            }
            if let Some(root) = entry.root() {
                rooted += 1;
                if self.root_owner(pages, root.address)? != Some(prefix) {
                    return Err(wrong(&key, "a root that has no record"));
                }
            }
            if entry.host().is_none() {
                tops.push((prefix, entry.id(), entry.root().is_some()));
            }
        }
        let children = self.count(pages, RecordKind::Child)?;
        if self.count(pages, RecordKind::Root)? != rooted {
            let key = [RecordKind::Root.code()];
            return Err(wrong(&key, "a root record of no entry"));
        }

        let mut drops = Vec::new();
        let mut dead = BTreeSet::new();
        let mut walk = self.drops();
        while let Some(dropped) = walk.next(pages)? {
            let key = dropped_key(dropped.id);
            if dropped.id >= next {
                return Err(wrong(&key, "an id that was not given out"));
            }
            if !ids.insert(dropped.id) {
                return Err(wrong(&key, "a dropped Merk that has an entry"));
            }
            if dropped.root.is_some() {
                dead.insert(dropped.id);
            }
            drops.push(dropped);
        }
        if dead != *self.dropped {
            let key = [RecordKind::Dropped.code()];
            return Err(wrong(&key, "the handle has other dropped Merks"));
        }

        let mut census = Census {
            live: tops.len() as u64,
            left_over: 0,
            dropped: drops.len() as u64,
        };
        let mut reached = 0;
        for (prefix, id, rooted) in tops {
            let top = Level {
                children: self.children(id),
                id,
                prefix,
                live: true,
                rooted,
            };
            self.walk(pages, top, children, &mut reached, &mut census)?;
        }
        for dropped in drops {
            let top = Level {
                children: self.children(dropped.id),
                id: dropped.id,
                prefix: Prefix::default(),
                live: false,
                rooted: dropped.root.is_some(),
            };
            self.walk(pages, top, children, &mut reached, &mut census)?;
        }
        if reached != children {
            let key = [RecordKind::Child.code()];
            return Err(wrong(&key, "a child record that no Merk leads to"));
        }
        if census.live + census.left_over != entries {
            let key = [RecordKind::Entry.code()];
            return Err(wrong(&key, "an entry that no Merk leads to"));
        }
        Ok(census)
    }

    fn count<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        kind: RecordKind,
    ) -> Result<u64, DirectoryError> {
        let mut records = Records::new(&self.tree, &[kind.code()]);
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
        children: u64,
        reached: &mut u64,
        census: &mut Census,
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
            *reached += 1;
            if *reached > children {
                return Err(wrong(&key, "child records that lead in a circle"));
            }
            if !level.rooted {
                return Err(wrong(&key, "a Merk with no node hosts a Merk"));
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
                    return Err(wrong(&key, "an entry under the record of another host"));
                }
                if live {
                    if host.map(|host| *host.parent) != Some(prefix) {
                        return Err(wrong(&key, "an entry that names another parent"));
                    }
                    if entry.lineage_bytes() != lineage || !self.is_live(entry) {
                        return Err(wrong(&key, "an entry with other Merks above it"));
                    }
                    census.live += 1;
                } else {
                    if self.is_live(entry) {
                        return Err(wrong(&key, "a left-over entry that a lookup returns"));
                    }
                    census.left_over += 1;
                }
            } else if live {
                return Err(wrong(&key, "a Merk in use hosts a Merk with no entry"));
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
