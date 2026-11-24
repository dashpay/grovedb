//! Where a transaction last saw the cells of the nodes it found by their
//! keys and of the nodes it wrote.
//!
//! A node that is asked for by its key, with no handle or with one that
//! has ended, is found from the root of its Merk, one cell for each level
//! ([`find`](super::pages::find)). GroveDB opens the Merks of a path anew
//! for every batch of a block, and reads the element that hosts each by
//! its key: a node that the batch before wrote. So the transaction keeps,
//! for each node it found or wrote, the handle it last had of it, and
//! follows that handle where a read or a write names the key and no
//! handle that is good (`tree-layout.md` P5, `transactions.md` PN8).
//!
//! # What is kept
//!
//! - **A node that a search found**: the handle of the cell it found.
//! - **A key under which a search found no cell.**
//! - **A node that a write of the transaction put**, where the write left
//!   it: the handle that the write reports ([`Applied::nodes`]).
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
//!   node, which puts its place here ([`Places::put`]), or by a load of
//!   the whole Merk, which forgets what is kept of the subtree
//!   ([`Places::forget_subtree`]);
//! - a write that gives a Merk another prefix, and one that drops or
//!   clears a Merk, is made under a new lease
//!   (`lignin_tree::node::Lease`), and so is everything that reads other
//!   pages than those the table was filled from.
//!
//! A table is of one lease: every handle in it was issued under that
//! lease, and what it says of a key was found under it. It answers
//! nothing under another lease, and the first thing that is put under
//! another empties it. So nothing has to empty a table where a lease
//! ends.
//!
//! [`Applied::nodes`]: super::Applied::nodes
//! [`NodePut::at`]: super::NodePut::at

use std::collections::HashMap;

use lignin_tree::node::{Lease, NodeHandle};

use super::key::Subtree;

/// The most keys a table keeps. One that is put into a table that has
/// this many empties the table first: a transaction that comes to more
/// keys than this under one lease searches for those of them that it
/// comes back to.
pub(crate) const KEPT_PLACES: usize = 1 << 16;

/// What a table has of one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// The handle the node under the key last had.
    At(NodeHandle),
    /// No cell is under the key.
    Absent,
}

/// What one lease has shown of the keys of each subtree.
#[derive(Debug, Default)]
pub(crate) struct Places {
    lease: Option<Lease>,
    by_subtree: HashMap<Subtree, HashMap<Box<[u8]>, Place>>,
    len: usize,
}

impl Places {
    /// What the table has of `key` in `subtree`, if it is of `lease`.
    pub(crate) fn get(&self, lease: Lease, subtree: &Subtree, key: &[u8]) -> Option<Place> {
        if self.len == 0 || self.lease != Some(lease) {
            return None;
        }
        self.by_subtree.get(subtree)?.get(key).copied()
    }

    /// Keeps `handle` as the place of the node under `key` of `subtree`.
    pub(crate) fn put(&mut self, subtree: Subtree, key: &[u8], handle: NodeHandle) {
        self.set(handle.lease(), subtree, key, Place::At(handle));
    }

    /// Keeps that no cell is under `key` of `subtree`, as the pages that
    /// are read under `lease` have it.
    pub(crate) fn put_absent(&mut self, lease: Lease, subtree: Subtree, key: &[u8]) {
        self.set(lease, subtree, key, Place::Absent);
    }

    fn set(&mut self, lease: Lease, subtree: Subtree, key: &[u8], place: Place) {
        if self.lease != Some(lease) || self.len >= KEPT_PLACES {
            self.clear();
            self.lease = Some(lease);
        }
        let keys = self.by_subtree.entry(subtree).or_default();
        match keys.get_mut(key) {
            Some(kept) => *kept = place,
            None => {
                keys.insert(key.into(), place);
                self.len += 1;
            }
        }
    }

    /// Forgets what is kept of `key` in `subtree`.
    pub(crate) fn forget(&mut self, subtree: &Subtree, key: &[u8]) {
        if let Some(keys) = self.by_subtree.get_mut(subtree)
            && keys.remove(key).is_some()
        {
            self.len -= 1;
        }
    }

    /// Forgets what is kept of every key of `subtree`.
    pub(crate) fn forget_subtree(&mut self, subtree: &Subtree) {
        if let Some(keys) = self.by_subtree.remove(subtree) {
            self.len -= keys.len();
        }
    }

    /// Forgets everything.
    pub(crate) fn clear(&mut self) {
        self.by_subtree.clear();
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
        let mut places = Places::default();
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
        let mut places = Places::default();
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
        let mut places = Places::default();
        for key in 0..KEPT_PLACES as u32 {
            places.put_absent(lease, subtree(1), &key.to_be_bytes());
        }
        assert_eq!(places.len(), KEPT_PLACES);
        // A key the table has is put in its place, and empties nothing
        // but a table that is full.
        places.put_absent(lease, subtree(1), b"one more");
        assert_eq!(places.len(), 1);
        assert_eq!(places.get(lease, &subtree(1), &0u32.to_be_bytes()), None);
        assert_eq!(
            places.get(lease, &subtree(1), b"one more"),
            Some(Place::Absent)
        );
    }
}
