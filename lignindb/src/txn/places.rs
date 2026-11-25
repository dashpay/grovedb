//! Where a transaction last saw the cells of the nodes it was asked for
//! by their keys.
//!
//! A node that is asked for by its key, with no handle or with one that
//! has ended, is found from the root of its Merk, one cell for each level
//! ([`find`](super::pages::find)). GroveDB opens the Merks of a path anew
//! for every batch of a block, and reads the element that hosts each by
//! its key: a node that the batch before wrote. So the transaction keeps,
//! for each key it searched for, the handle it last had of the node,
//! through its own writes of that node, and follows that handle where a
//! read or a write names the key and no handle that is good
//! (`tree-layout.md` P5, `transactions.md` PN8).
//!
//! # What is kept
//!
//! - **A node that a search found**: the handle of the cell it found.
//! - **A key under which a search found no cell.**
//! - **A node that a write of the transaction put under a key the table
//!   has**, where the write left it: the handle that the write reports
//!   ([`Applied::nodes`]). The table has a key from the first search for
//!   it under the table's lease until the table is emptied, whatever was
//!   forgotten of the key since.
//!
//! A node that a write put under a key the table does not have is not
//! kept ([`Places::renew`]): a write keeps nothing for a key that nothing
//! searched for. Such a node is searched for the first time it is asked
//! for by its key, as a node the transaction never wrote is, and is kept
//! from then on. A node that is reached from its parent needs no place
//! here: the cell of the parent says where it is.
//!
//! # What a place is good for
//!
//! A place is a hint, as a handle that a caller names is
//! ([`NodePut::at`]): it is followed only while its lease is the one the
//! pages are read under, no cell has left its page since, and the cell
//! there has the key ([`follow`](super::pages::follow)). A place that is
//! not followed says nothing, and the node is found by its key.
//!
//! A cell does not say which Merk it is in, so a place is kept under the
//! subtree whose read or write gave it, and is asked for under that
//! subtree alone.
//!
//! # What a key with no cell is good for
//!
//! That no cell is under a key is no hint: it is taken as it was found.
//! So it is kept only while it holds, by whoever keeps the table:
//!
//! - a cell comes to be under a key of a Merk by a write that puts the
//!   node, which keeps its place in the entry that says the key has no
//!   cell ([`Places::renew`]), or by a load of the whole Merk, which
//!   forgets what is kept of the subtree ([`Places::forget_subtree`]);
//! - a write that gives a Merk another prefix, and one that drops or
//!   clears a Merk, is made under a new lease
//!   (`lignin_tree::node::Lease`), and so is everything that reads other
//!   pages than those the table was filled from.
//!
//! A table is of one lease: every handle in it was issued under that
//! lease, and what it says of a key was found under it. It answers
//! nothing under another lease, the first search that is kept under
//! another empties it, and a write under another keeps nothing in it. So
//! nothing has to empty a table where a lease ends.
//!
//! [`Applied::nodes`]: super::Applied::nodes
//! [`NodePut::at`]: super::NodePut::at

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hasher, RandomState},
};

use lignin_tree::node::{Lease, NodeHandle};

use super::key::Subtree;

/// The most keys a table takes before it is emptied. A key that is new
/// to a table that took this many empties the table first: a transaction
/// that comes to more keys than this under one lease searches for those
/// of them that it comes back to. A key that was forgotten counts until
/// the table is emptied.
pub(crate) const KEPT_PLACES: usize = 1 << 16;

/// What a table has of one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// The handle the node under the key last had.
    At(NodeHandle),
    /// No cell is under the key.
    Absent,
}

/// One key of a table.
#[derive(Debug)]
struct Entry {
    subtree: Subtree,
    /// Where the key is in [`Places::keys`], and how long it is.
    key: (u32, u32),
    /// What is kept of the key: nothing once it is forgotten.
    place: Option<Place>,
    /// The entry that was put before this one under the same hash, or no
    /// entry of the table.
    before: u32,
}

/// What one lease has shown of the keys of each subtree.
///
/// The keys are in one buffer and the entries in one list, so a table
/// that is emptied or dropped gives its memory back in three pieces,
/// however many keys it took.
#[derive(Debug, Default)]
pub(crate) struct Places<S = RandomState> {
    lease: Option<Lease>,
    /// The entry that was put last under each hash of a subtree and a key.
    newest: HashMap<u64, u32>,
    entries: Vec<Entry>,
    keys: Vec<u8>,
    /// How many entries have something of their key.
    len: usize,
    hashes: S,
}

impl<S: BuildHasher> Places<S> {
    /// What the table has of `key` in `subtree`, if it is of `lease`.
    pub(crate) fn get(&self, lease: Lease, subtree: &Subtree, key: &[u8]) -> Option<Place> {
        if self.len == 0 || self.lease != Some(lease) {
            return None;
        }
        let entry = self.entry(self.hash(subtree, key), subtree, key)?;
        self.entries[entry].place
    }

    /// Keeps `handle` as the place of the node that a search found under
    /// `key` of `subtree`.
    pub(crate) fn put(&mut self, subtree: Subtree, key: &[u8], handle: NodeHandle) {
        self.set(handle.lease(), subtree, key, Place::At(handle));
    }

    /// Keeps `handle` as the place of the node that a write put under
    /// `key` of `subtree`, where the table has the key and is of the
    /// lease of `handle`. A key that is new to the table is not taken.
    pub(crate) fn renew(&mut self, subtree: &Subtree, key: &[u8], handle: NodeHandle) {
        if self.entries.is_empty() || self.lease != Some(handle.lease()) {
            return;
        }
        if let Some(entry) = self.entry(self.hash(subtree, key), subtree, key) {
            let place = &mut self.entries[entry].place;
            if place.replace(Place::At(handle)).is_none() {
                self.len += 1;
            }
        }
    }

    /// Keeps that no cell is under `key` of `subtree`, as the pages that
    /// are read under `lease` have it.
    pub(crate) fn put_absent(&mut self, lease: Lease, subtree: Subtree, key: &[u8]) {
        self.set(lease, subtree, key, Place::Absent);
    }

    fn hash(&self, subtree: &Subtree, key: &[u8]) -> u64 {
        let mut hasher = self.hashes.build_hasher();
        hasher.write(subtree.as_bytes());
        hasher.write(key);
        hasher.finish()
    }

    /// The entry of `key` in `subtree`, whose hash is `hash`.
    fn entry(&self, hash: u64, subtree: &Subtree, key: &[u8]) -> Option<usize> {
        let mut at = *self.newest.get(&hash)? as usize;
        loop {
            let entry = self.entries.get(at)?;
            let (start, len) = (entry.key.0 as usize, entry.key.1 as usize);
            if entry.subtree == *subtree && self.keys[start..start + len] == *key {
                return Some(at);
            }
            at = entry.before as usize;
        }
    }

    fn set(&mut self, lease: Lease, subtree: Subtree, key: &[u8], place: Place) {
        if self.lease != Some(lease) {
            self.clear();
            self.lease = Some(lease);
        }
        let hash = self.hash(&subtree, key);
        if let Some(entry) = self.entry(hash, &subtree, key) {
            if self.entries[entry].place.replace(place).is_none() {
                self.len += 1;
            }
            return;
        }
        if self.entries.len() >= KEPT_PLACES {
            self.clear();
            self.lease = Some(lease);
        }
        let entry = self.entries.len() as u32;
        let before = self.newest.insert(hash, entry).unwrap_or(u32::MAX);
        self.entries.push(Entry {
            subtree,
            key: (self.keys.len() as u32, key.len() as u32),
            place: Some(place),
            before,
        });
        self.keys.extend_from_slice(key);
        self.len += 1;
    }

    /// Forgets what is kept of `key` in `subtree`.
    pub(crate) fn forget(&mut self, subtree: &Subtree, key: &[u8]) {
        if self.len == 0 {
            return;
        }
        if let Some(entry) = self.entry(self.hash(subtree, key), subtree, key)
            && self.entries[entry].place.take().is_some()
        {
            self.len -= 1;
        }
    }

    /// Forgets what is kept of every key of `subtree`.
    pub(crate) fn forget_subtree(&mut self, subtree: &Subtree) {
        if self.len == 0 {
            return;
        }
        let of_subtree = (self.entries.iter_mut()).filter(|entry| entry.subtree == *subtree);
        let forgotten = of_subtree.filter_map(|entry| entry.place.take()).count();
        self.len -= forgotten;
    }

    /// Forgets everything.
    pub(crate) fn clear(&mut self) {
        self.newest.clear();
        self.entries.clear();
        self.keys.clear();
        self.len = 0;
        self.lease = None;
    }

    /// How many keys the table has something of.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn subtree(name: u8) -> Subtree {
        Subtree::new([name; 32])
    }

    #[test]
    fn a_table_answers_under_its_lease_alone() {
        let (lease, other) = (Lease::take(), Lease::take());
        let mut places = Places::<RandomState>::default();
        assert_eq!(places.get(lease, &subtree(1), b"a"), None);

        places.put_absent(lease, subtree(1), b"a");
        places.put_absent(lease, subtree(1), b"b");
        places.put_absent(lease, subtree(2), b"a");
        assert_eq!(places.len(), 3);
        assert_eq!(places.get(lease, &subtree(1), b"a"), Some(Place::Absent));
        assert_eq!(places.get(lease, &subtree(2), b"b"), None);
        assert_eq!(places.get(lease, &subtree(3), b"a"), None);
        assert_eq!(places.get(other, &subtree(1), b"a"), None);

        // What is put under another lease empties the table first.
        places.put_absent(other, subtree(2), b"c");
        assert_eq!(places.len(), 1);
        assert_eq!(places.get(lease, &subtree(1), b"a"), None);
        assert_eq!(places.get(other, &subtree(1), b"a"), None);
        assert_eq!(places.get(other, &subtree(2), b"c"), Some(Place::Absent));
    }

    #[test]
    fn a_key_is_forgotten_alone_or_with_its_subtree() {
        let lease = Lease::take();
        let mut places = Places::<RandomState>::default();
        for key in [b"a", b"b", b"c"] {
            places.put_absent(lease, subtree(1), key);
        }
        places.put_absent(lease, subtree(2), b"a");
        places.put_absent(lease, subtree(1), b"a");
        assert_eq!(places.len(), 4, "a key is kept once");

        places.forget(&subtree(1), b"b");
        places.forget(&subtree(1), b"b");
        places.forget(&subtree(9), b"b");
        assert_eq!(places.len(), 3);
        assert_eq!(places.get(lease, &subtree(1), b"b"), None);
        assert_eq!(places.get(lease, &subtree(1), b"c"), Some(Place::Absent));

        places.forget_subtree(&subtree(1));
        places.forget_subtree(&subtree(1));
        assert_eq!(places.len(), 1);
        assert_eq!(places.get(lease, &subtree(1), b"c"), None);
        assert_eq!(places.get(lease, &subtree(2), b"a"), Some(Place::Absent));

        places.clear();
        assert_eq!(places.len(), 0);
        assert_eq!(places.get(lease, &subtree(2), b"a"), None);
    }

    #[test]
    fn a_full_table_is_emptied_by_the_next_key() {
        let lease = Lease::take();
        let mut places = Places::<RandomState>::default();
        for key in 0..KEPT_PLACES as u32 {
            places.put_absent(lease, subtree(1), &key.to_be_bytes());
            // A key the table has is put in its place.
            places.put_absent(lease, subtree(1), &key.to_be_bytes());
        }
        assert_eq!(places.len(), KEPT_PLACES);
        // A key that is forgotten counts for the table's size until the
        // table is emptied, and a full table takes it again.
        places.forget(&subtree(1), &7u32.to_be_bytes());
        assert_eq!(places.len(), KEPT_PLACES - 1);
        places.put_absent(lease, subtree(1), &7u32.to_be_bytes());
        assert_eq!(places.len(), KEPT_PLACES);
        places.forget(&subtree(1), &7u32.to_be_bytes());
        places.put_absent(lease, subtree(1), b"one more");
        assert_eq!(places.len(), 1);
        assert_eq!(places.get(lease, &subtree(1), &0u32.to_be_bytes()), None);
        assert_eq!(
            places.get(lease, &subtree(1), b"one more"),
            Some(Place::Absent)
        );
    }

    /// Every key under one hash.
    #[derive(Default)]
    struct OneHash;

    impl BuildHasher for OneHash {
        type Hasher = Self;

        fn build_hasher(&self) -> Self {
            Self
        }
    }

    impl Hasher for OneHash {
        fn finish(&self) -> u64 {
            7
        }

        fn write(&mut self, _: &[u8]) {}
    }

    /// Keys that share a hash are told apart by their subtree and their
    /// bytes: a key that is a part of another, the same key in another
    /// subtree, and the empty key.
    #[test]
    fn keys_under_one_hash_are_kept_apart() {
        let lease = Lease::take();
        let mut places = Places::<OneHash>::default();
        let keys: [(u8, &[u8]); 6] = [
            (1, b"ab"),
            (1, b"a"),
            (1, b"abc"),
            (2, b"ab"),
            (1, b""),
            (2, b""),
        ];
        for (name, key) in keys {
            assert_eq!(places.get(lease, &subtree(name), key), None, "{key:?}");
            places.put_absent(lease, subtree(name), key);
        }
        assert_eq!(places.len(), keys.len());
        for (at, (name, key)) in keys.into_iter().enumerate() {
            let kept = places.get(lease, &subtree(name), key);
            assert_eq!(kept, Some(Place::Absent), "{key:?}");
            places.forget(&subtree(name), key);
            assert_eq!(places.get(lease, &subtree(name), key), None, "{key:?}");
            assert_eq!(places.len(), keys.len() - at - 1);
        }
        assert_eq!(places.get(lease, &subtree(1), b"b"), None);
        places.forget(&subtree(1), b"b");

        // A key that was forgotten is kept again in its entry.
        places.put_absent(lease, subtree(1), b"a");
        places.put_absent(lease, subtree(2), b"ab");
        assert_eq!(places.len(), 2);
        assert_eq!(places.get(lease, &subtree(1), b"a"), Some(Place::Absent));
        assert_eq!(places.get(lease, &subtree(1), b"ab"), None);
        places.forget_subtree(&subtree(2));
        assert_eq!(places.len(), 1);
        assert_eq!(places.get(lease, &subtree(2), b"ab"), None);
    }
}
