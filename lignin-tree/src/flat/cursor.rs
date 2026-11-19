//! A cursor over a flat keyspace, in key order.

use std::{borrow::Cow, cmp::Ordering, collections::BTreeSet};

use lignin_format::{
    flat_page::{FlatLeaf, FlatPage},
    PageId,
};

use super::{
    error::FlatError,
    long_key::{bucket_root, Stored},
    node::{branch, child, leaf, read_value},
    pages::{FlatPages, Revision},
    tree::{FlatTree, Step},
};

/// The first key that is greater than every key that starts with `prefix`,
/// if there is one.
pub(super) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let last = prefix.iter().rposition(|&byte| byte != u8::MAX)?;
    let mut end = prefix[..=last].to_vec();
    end[last] += 1;
    Some(end)
}

/// What a bucket is whose tree, with the root `page`, is a tree that the
/// bucket is in.
fn ring(page: PageId) -> FlatError {
    let what = "a bucket of long keys whose tree is a tree it is in";
    FlatError::Damaged { page, what }
}

/// The position after `index`, or before it with `back`.
fn beside(index: usize, back: bool) -> Option<usize> {
    if back {
        index.checked_sub(1)
    } else {
        Some(index + 1)
    }
}

/// A position in one tree: an entry of a leaf, with the branches above the
/// leaf and the entry's stored key.
#[derive(Debug, Clone)]
struct Level {
    tree: FlatTree,
    steps: Vec<Step>,
    leaf: PageId,
    slot: usize,
    stored: Vec<u8>,
    /// The tree of the entry, if the entry is a bucket of long keys.
    bucket: Option<FlatTree>,
}

impl Level {
    /// A position in `tree` that is nowhere yet.
    fn new(tree: FlatTree) -> Self {
        Self {
            tree,
            steps: Vec::new(),
            leaf: tree.root(),
            slot: 0,
            stored: Vec::new(),
            bucket: None,
        }
    }

    /// Stands on the entry in position `slot` of `leaf`, which is the page
    /// `page` as it was just read.
    fn stand(&mut self, page: PageId, leaf: &FlatLeaf<'_>, slot: usize) -> Result<(), FlatError> {
        let (key, value) = leaf.entry(slot).ok_or(FlatError::Damaged {
            page,
            what: "a leaf without the entry its slots name",
        })?;
        let config = self.tree.config();
        self.bucket = match config.is_bucket(key.len()) {
            true => Some(FlatTree::open(bucket_root(page, value)?, config)),
            false => None,
        };
        (self.leaf, self.slot) = (page, slot);
        self.stored.clear();
        key.append_to(&mut self.stored);
        Ok(())
    }

    /// Whether the entry the position names is still where `leaf` has it.
    fn is_at(&self, leaf: &FlatLeaf<'_>) -> bool {
        let found = leaf.key(self.slot);
        found.is_some_and(|key| key.cmp_bytes(&self.stored) == Ordering::Equal)
    }

    /// Goes to the first entry whose stored key is `key` or greater, or
    /// with `back` to the last that is `key` or less, and says whether
    /// there is one.
    fn find<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
        back: bool,
    ) -> Result<bool, FlatError> {
        self.steps.clear();
        let tree = self.tree;
        let (page, frame) = tree.walk(pages, key, Some(&mut self.steps))?;
        let leaf = leaf(&frame, page)?;
        let slot = match (leaf.search(key), back) {
            (Ok(slot), _) => Some(slot),
            (Err(slot), false) => Some(slot).filter(|&slot| slot < leaf.len()),
            (Err(slot), true) => slot.checked_sub(1),
        };
        // The entry is read from the page that was searched.
        if let Some(slot) = slot {
            self.stand(page, &leaf, slot)?;
            return Ok(true);
        }
        drop(frame);
        self.next_leaf(pages, back)
    }

    /// Goes to the first entry of the tree, or with `back` to the last,
    /// and says whether there is one.
    fn edge<P: FlatPages + ?Sized>(&mut self, pages: &P, back: bool) -> Result<bool, FlatError> {
        self.steps.clear();
        Ok(self.down(pages, self.tree.root(), None, back)? || self.next_leaf(pages, back)?)
    }

    /// Goes to the entry beside this one, and says whether there is one.
    ///
    /// The entry is not where it was if a step of background work took an
    /// entry out of its leaf. The position then goes on from its key.
    fn step<P: FlatPages + ?Sized>(&mut self, pages: &P, back: bool) -> Result<bool, FlatError> {
        loop {
            let frame = pages.page(self.leaf)?;
            let leaf = leaf(&frame, self.leaf)?;
            if self.is_at(&leaf) {
                let Some(to) = beside(self.slot, back).filter(|&to| to < leaf.len()) else {
                    drop(frame);
                    return self.next_leaf(pages, back);
                };
                self.stand(self.leaf, &leaf, to)?;
                return Ok(true);
            }
            drop(frame);
            let stored = self.stored.clone();
            if !self.find(pages, &stored, back)? {
                return Ok(false);
            }
            if self.stored != stored {
                return Ok(true);
            }
        }
    }

    /// Goes from the leaf below the remembered branches to the nearest
    /// leaf after it that has an entry, or before it with `back`, to that
    /// leaf's first entry, or its last. Says whether there is one.
    fn next_leaf<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        back: bool,
    ) -> Result<bool, FlatError> {
        loop {
            // Up to a branch that has a child on that side.
            let (page, level) = loop {
                let Some(step) = self.steps.last_mut() else {
                    return Ok(false);
                };
                let frame = pages.page(step.page)?;
                let branch = branch(&frame, step.page)?;
                if let Some(to) = beside(step.index, back).filter(|&to| to <= branch.len()) {
                    step.index = to;
                    break (child(&branch, to)?, branch.level());
                }
                self.steps.pop();
            };
            if self.down(pages, page, Some(level), back)? {
                return Ok(true);
            }
        }
    }

    /// Goes down from `page`, which is below a branch of the level
    /// `above`, along the first children, or the last, to the leaf at the
    /// end, and stands on its first entry, or its last. Says whether that
    /// leaf has an entry.
    fn down<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        mut page: PageId,
        mut above: Option<u8>,
        back: bool,
    ) -> Result<bool, FlatError> {
        loop {
            let frame = pages.page(page)?;
            match FlatTree::checked(&frame, page, above)? {
                FlatPage::Leaf(leaf) => {
                    let Some(last) = leaf.len().checked_sub(1) else {
                        return Ok(false);
                    };
                    self.stand(page, &leaf, if back { last } else { 0 })?;
                    return Ok(true);
                }
                FlatPage::Branch(branch) => {
                    let index = if back { branch.len() } else { 0 };
                    self.steps.push(Step { page, index });
                    above = Some(branch.level());
                    page = child(&branch, index)?;
                }
            }
        }
    }
}

/// A cursor over the records of a flat keyspace, or over those whose keys
/// start with one prefix, in key order.
///
/// It is not positioned until the first seek, and stepping past either end
/// leaves it not positioned again, as does a move that fails. It holds no
/// page: every call is given the pages to read, and moves from leaf to
/// leaf through the branches above them, which it remembers.
///
/// **One cursor, one reader or writer.** A cursor is moved with the pages
/// it was last moved with, or with pages that tell themselves apart from
/// those ([`FlatPages::revision`]). Under a writer whose pages have
/// changed since the cursor last moved, [`Self::next`] goes to the first
/// record after the key it stood on and [`Self::prev`] to the last one
/// before it, whether or not that key is still there. So deleting the
/// record a cursor stands on and stepping on is defined, and so is any
/// other write.
///
/// A reader's pages do not change, except that a step of background work
/// can take an entry that a dropped subtree left out of a leaf. The cursor
/// reads what it stands on by its key, so it shows no other record's value
/// under that key, and goes on from the key.
#[derive(Debug, Clone)]
pub struct Cursor {
    tree: FlatTree,
    prefix: Vec<u8>,
    /// The position: one level, and under a long key one more for each
    /// bucket the key is in. Empty when the cursor is not positioned.
    levels: Vec<Level>,
    /// The roots of the trees of `levels`: no tree is gone into twice.
    roots: BTreeSet<PageId>,
    key: Vec<u8>,
    /// The pages the cursor was last moved with.
    revision: Option<Revision>,
}

impl Cursor {
    pub(super) fn new(tree: FlatTree, prefix: &[u8]) -> Self {
        Self {
            tree,
            prefix: prefix.to_vec(),
            levels: Vec::new(),
            roots: BTreeSet::new(),
            key: Vec::new(),
            revision: None,
        }
    }

    /// Whether the cursor stands on a record.
    pub fn valid(&self) -> bool {
        !self.levels.is_empty()
    }

    /// The key of the record the cursor stands on, without the cursor's
    /// prefix.
    pub fn key(&self) -> Option<&[u8]> {
        self.levels.last()?;
        self.key.get(self.prefix.len()..)
    }

    /// Appends the value of the record the cursor stands on to `out`, and
    /// says whether there is one: the cursor is positioned, and no write
    /// has removed the record since.
    pub fn value<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        out: &mut Vec<u8>,
    ) -> Result<bool, FlatError> {
        let Some(level) = self.levels.last() else {
            return Ok(false);
        };
        if Some(pages.revision()) == self.revision {
            let frame = pages.page(level.leaf)?;
            let leaf = leaf(&frame, level.leaf)?;
            if level.is_at(&leaf)
                && let Some(value) = leaf.value(level.slot)
            {
                read_value(pages, &frame, level.leaf, level.slot, value, out)?;
                return Ok(true);
            }
        }
        // The record is not where the cursor left it: it is read by its
        // key.
        self.tree.get(pages, &self.key, out)
    }

    /// Goes to the first record whose key is `key` or greater.
    pub fn seek<P: FlatPages + ?Sized>(&mut self, pages: &P, key: &[u8]) -> Result<(), FlatError> {
        self.seek_to(pages, key, false)
    }

    /// Goes to the last record whose key is `key` or less.
    pub fn seek_for_prev<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
    ) -> Result<(), FlatError> {
        self.seek_to(pages, key, true)
    }

    fn seek_to<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
        back: bool,
    ) -> Result<(), FlatError> {
        let key = [&self.prefix, key].concat();
        let moved = self.land(pages, &key, back);
        self.arrive(pages, moved)
    }

    /// Goes to the first record.
    pub fn seek_to_first<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        self.seek(pages, &[])
    }

    /// Goes to the last record.
    pub fn seek_to_last<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        let moved = match prefix_end(&self.prefix) {
            Some(end) => self.past(pages, &end, true),
            None => self.last(pages),
        };
        self.arrive(pages, moved)
    }

    /// Goes to the next record in key order. A cursor that is not
    /// positioned stays so.
    pub fn next<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        self.advance(pages, false)
    }

    /// Goes to the record before this one in key order. A cursor that is
    /// not positioned stays so.
    pub fn prev<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        self.advance(pages, true)
    }

    fn advance<P: FlatPages + ?Sized>(&mut self, pages: &P, back: bool) -> Result<(), FlatError> {
        if self.levels.is_empty() {
            return Ok(());
        }
        let moved = if Some(pages.revision()) == self.revision {
            self.pass(pages, back)
        } else {
            let key = std::mem::take(&mut self.key);
            self.past(pages, &key, back)
        };
        self.arrive(pages, moved)
    }

    /// Ends a move. A move that failed leaves no position, and neither
    /// does a record outside the cursor's prefix.
    fn arrive<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        moved: Result<(), FlatError>,
    ) -> Result<(), FlatError> {
        self.name();
        if moved.is_err() || !self.key.starts_with(&self.prefix) {
            self.leave();
        }
        self.revision = Some(pages.revision());
        moved
    }

    /// Makes the key of the position: the head of each bucket it is in,
    /// and the stored key of the innermost entry.
    fn name(&mut self) {
        let limit = self.tree.config().key_limit();
        self.key.clear();
        if let Some((last, buckets)) = self.levels.split_last() {
            for bucket in buckets {
                self.key
                    .extend_from_slice(bucket.stored.get(..limit).unwrap_or_default());
            }
            self.key.extend_from_slice(&last.stored);
        }
    }

    /// Goes to the first record whose key is `key` or greater, or with
    /// `back` to the last that is `key` or less.
    fn land<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
        back: bool,
    ) -> Result<(), FlatError> {
        self.leave();
        let (mut tree, mut key) = (self.tree, key);
        loop {
            let (stored, rest) = match tree.config().stored(key) {
                Stored::Short(key) => (Cow::Borrowed(key), None),
                Stored::Long { bucket, rest } => (Cow::Owned(bucket), Some(rest)),
            };
            let mut level = Level::new(tree);
            if !level.find(pages, &stored, back)? {
                // This tree has nothing on that side: on from the bucket
                // that holds it.
                return self.settle(pages, back, true);
            }
            // The key's own bucket is gone into, to the rest of the key.
            let own = level.bucket.filter(|_| level.stored == *stored);
            self.go_into(level)?;
            match (own, rest) {
                (Some(own), Some(_)) if self.roots.contains(&own.root()) => {
                    return Err(ring(own.root()));
                }
                (Some(own), Some(rest)) => (tree, key) = (own, rest),
                _ => return self.settle(pages, back, false),
            }
        }
    }

    /// Goes to the first record after `key`, or with `back` to the last
    /// before it.
    fn past<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
        back: bool,
    ) -> Result<(), FlatError> {
        self.land(pages, key, back)?;
        self.name();
        if !self.levels.is_empty() && self.key == key {
            self.pass(pages, back)?;
        }
        Ok(())
    }

    /// Goes to the last record of the keyspace.
    fn last<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        self.leave();
        let mut level = Level::new(self.tree);
        if level.edge(pages, true)? {
            self.go_into(level)?;
        }
        self.settle(pages, true, false)
    }

    /// Goes one record on, or one back: to the entry beside the innermost
    /// one, and out of each bucket that has none on that side.
    fn pass<P: FlatPages + ?Sized>(&mut self, pages: &P, back: bool) -> Result<(), FlatError> {
        self.settle(pages, back, true)
    }

    /// Brings the position to a record. With `step`, the innermost level
    /// first goes to the entry beside the one it stands on, and where it
    /// has none on that side the level is left, for the entry beside its
    /// bucket. Then, while the innermost entry is a bucket, its tree is
    /// gone into: to its first key, or with `back` to its last. A bucket
    /// whose tree is empty is passed by.
    fn settle<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        back: bool,
        mut step: bool,
    ) -> Result<(), FlatError> {
        while let Some(level) = self.levels.last_mut() {
            if step {
                if !level.step(pages, back)? {
                    self.roots.remove(&level.tree.root());
                    self.levels.pop();
                    continue;
                }
                step = false;
            }
            let Some(inner) = level.bucket else {
                return Ok(());
            };
            let mut level = Level::new(inner);
            if level.edge(pages, back)? {
                self.go_into(level)?;
            } else {
                step = true;
            }
        }
        Ok(())
    }

    /// Takes the position into the tree of `level`, below the levels it
    /// has.
    ///
    /// A tree that the position is in already is damage: the trees on the
    /// way to a key are different ones, so the way ends.
    fn go_into(&mut self, level: Level) -> Result<(), FlatError> {
        let page = level.tree.root();
        if !self.roots.insert(page) {
            return Err(ring(page));
        }
        self.levels.push(level);
        Ok(())
    }

    /// Leaves the position: the cursor stands nowhere.
    fn leave(&mut self) {
        self.levels.clear();
        self.roots.clear();
    }
}
