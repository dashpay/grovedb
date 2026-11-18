//! A cursor over a flat keyspace, in key order.

use lignin_format::{flat_page::FlatPage, PageId};

use super::{
    error::FlatError,
    long_key::{Bucket, Stored},
    node::{branch, child, leaf, read_value},
    pages::FlatPages,
    tree::{FlatTree, Step},
};

/// Where a cursor stands: an entry of a leaf, and inside a bucket of long
/// keys one of its keys.
#[derive(Debug, Clone)]
struct At {
    leaf: PageId,
    slot: usize,
    bucket: Option<(Bucket, usize)>,
}

/// Which key of a bucket a cursor that arrives at it stands on.
#[derive(Debug, Clone, Copy)]
enum Edge {
    First,
    Last,
}

/// The first key that is greater than every key that starts with `prefix`,
/// if there is one.
pub(super) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let last = prefix.iter().rposition(|&byte| byte != u8::MAX)?;
    let mut end = prefix[..=last].to_vec();
    end[last] += 1;
    Some(end)
}

/// A cursor over the records of a flat keyspace, or over those whose keys
/// start with one prefix, in key order.
///
/// It is not positioned until the first seek, and stepping past either end
/// leaves it not positioned again. It holds no page: every call is given
/// the pages to read, and moves from leaf to leaf through the branches
/// above them, which it remembers.
///
/// The cursor reads the pages as they are at each call. When a page of a
/// writer has changed since the cursor last moved
/// ([`FlatPages::revision`]), [`Self::next`] goes to the first record after
/// the key it stood on and [`Self::prev`] to the last one before it,
/// whether or not that key is still there. So deleting the record a cursor
/// stands on and stepping on is defined, and so is any other write.
#[derive(Debug, Clone)]
pub struct Cursor {
    tree: FlatTree,
    prefix: Vec<u8>,
    end: Option<Vec<u8>>,
    steps: Vec<Step>,
    at: Option<At>,
    key: Vec<u8>,
    revision: u64,
}

impl Cursor {
    pub(super) fn new(tree: FlatTree, prefix: &[u8]) -> Self {
        Self {
            tree,
            prefix: prefix.to_vec(),
            end: prefix_end(prefix),
            steps: Vec::new(),
            at: None,
            key: Vec::new(),
            revision: 0,
        }
    }

    /// Whether the cursor stands on a record.
    pub fn valid(&self) -> bool {
        self.at.is_some()
    }

    /// The key of the record the cursor stands on, without the cursor's
    /// prefix.
    pub fn key(&self) -> Option<&[u8]> {
        self.at.as_ref()?;
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
        let Some(at) = &self.at else {
            return Ok(false);
        };
        if pages.revision() != self.revision {
            return self.tree.get(pages, &self.key, out);
        }
        if let Some((bucket, index)) = &at.bucket {
            out.extend_from_slice(bucket.value(*index).unwrap_or_default());
            return Ok(true);
        }
        let frame = pages.page(at.leaf)?;
        let value = leaf(&frame, at.leaf)?
            .value(at.slot)
            .ok_or(Self::moved(at.leaf))?;
        read_value(pages, at.leaf, at.slot, value, out)?;
        Ok(true)
    }

    /// Goes to the first record whose key is `key` or greater.
    pub fn seek<P: FlatPages + ?Sized>(&mut self, pages: &P, key: &[u8]) -> Result<(), FlatError> {
        let key = [&self.prefix, key].concat();
        self.at_or_after(pages, &key)?;
        self.arrive(pages);
        Ok(())
    }

    /// Goes to the last record whose key is `key` or less.
    pub fn seek_for_prev<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
    ) -> Result<(), FlatError> {
        let key = [&self.prefix, key].concat();
        self.at_or_before(pages, &key)?;
        self.arrive(pages);
        Ok(())
    }

    /// Goes to the first record.
    pub fn seek_to_first<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        self.seek(pages, &[])
    }

    /// Goes to the last record.
    pub fn seek_to_last<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        match self.end.clone() {
            Some(end) => self.before(pages, &end)?,
            None => {
                self.steps.clear();
                let (page, len) = self.down(pages, self.tree.root(), None, true)?;
                let found = match len.checked_sub(1) {
                    Some(slot) => Some((page, slot)),
                    None => self.next_leaf(pages, true)?,
                };
                self.settle(pages, found, Edge::Last)?;
            }
        }
        self.arrive(pages);
        Ok(())
    }

    /// Goes to the next record in key order. A cursor that is not
    /// positioned stays so.
    pub fn next<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        if self.at.is_none() {
            return Ok(());
        }
        if pages.revision() == self.revision {
            self.step(pages, false)?;
        } else {
            let key = std::mem::take(&mut self.key);
            self.at_or_after(pages, &key)?;
            if self.at.is_some() && self.key == key {
                self.step(pages, false)?;
            }
        }
        self.arrive(pages);
        Ok(())
    }

    /// Goes to the record before this one in key order. A cursor that is
    /// not positioned stays so.
    pub fn prev<P: FlatPages + ?Sized>(&mut self, pages: &P) -> Result<(), FlatError> {
        if self.at.is_none() {
            return Ok(());
        }
        if pages.revision() == self.revision {
            self.step(pages, true)?;
        } else {
            let key = std::mem::take(&mut self.key);
            self.before(pages, &key)?;
        }
        self.arrive(pages);
        Ok(())
    }

    fn moved(page: PageId) -> FlatError {
        FlatError::Damaged {
            page,
            what: "a leaf without the entry a cursor stands on",
        }
    }

    /// Ends a move: a record outside the cursor's prefix is no position.
    fn arrive<P: FlatPages + ?Sized>(&mut self, pages: &P) {
        if !self.key.starts_with(&self.prefix) {
            self.at = None;
        }
        self.revision = pages.revision();
    }

    /// Goes to the last record whose key is less than `key`.
    fn before<P: FlatPages + ?Sized>(&mut self, pages: &P, key: &[u8]) -> Result<(), FlatError> {
        self.at_or_before(pages, key)?;
        if self.at.is_some() && self.key == key {
            self.step(pages, true)?;
        }
        Ok(())
    }

    fn at_or_after<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
    ) -> Result<(), FlatError> {
        let (stored, rest) = match self.tree.config().stored(key) {
            Stored::Short(key) => (key.to_vec(), None),
            Stored::Long { bucket, rest } => (bucket, Some(rest)),
        };
        self.steps.clear();
        let (page, frame) = self.tree.walk(pages, &stored, Some(&mut self.steps))?;
        let leaf = leaf(&frame, page)?;
        let slot = leaf.search(&stored).unwrap_or_else(|slot| slot);
        let in_leaf = slot < leaf.len();
        drop(frame);
        let found = if in_leaf {
            Some((page, slot))
        } else {
            self.next_leaf(pages, false)?
        };
        self.settle(pages, found, Edge::First)?;
        if let Some(rest) = rest
            && let Some((bucket, index)) = self.bucket_of(&stored)
        {
            let at = bucket.search(rest).unwrap_or_else(|at| at);
            let past = at == bucket.len();
            *index = at.min(bucket.len() - 1);
            self.name();
            if past {
                self.step(pages, false)?;
            }
        }
        Ok(())
    }

    fn at_or_before<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        key: &[u8],
    ) -> Result<(), FlatError> {
        let (stored, rest) = match self.tree.config().stored(key) {
            Stored::Short(key) => (key.to_vec(), None),
            Stored::Long { bucket, rest } => (bucket, Some(rest)),
        };
        self.steps.clear();
        let (page, frame) = self.tree.walk(pages, &stored, Some(&mut self.steps))?;
        let slot = match leaf(&frame, page)?.search(&stored) {
            Ok(slot) => Some(slot),
            Err(slot) => slot.checked_sub(1),
        };
        drop(frame);
        let found = match slot {
            Some(slot) => Some((page, slot)),
            None => self.next_leaf(pages, true)?,
        };
        self.settle(pages, found, Edge::Last)?;
        if let Some(rest) = rest
            && let Some((bucket, index)) = self.bucket_of(&stored)
        {
            let (at, past) = match bucket.search(rest) {
                Ok(at) => (at, false),
                Err(at) => (at.saturating_sub(1), at == 0),
            };
            *index = at;
            self.name();
            if past {
                self.step(pages, true)?;
            }
        }
        Ok(())
    }

    /// The bucket the cursor stands in, if its stored key is `stored`.
    fn bucket_of(&mut self, stored: &[u8]) -> Option<(&Bucket, &mut usize)> {
        let head = stored.get(..self.tree.config().key_limit())?;
        let (bucket, index) = self.at.as_mut()?.bucket.as_mut()?;
        self.key.starts_with(head).then_some((&*bucket, index))
    }

    /// Makes the key of a position inside a bucket: the bucket's head and
    /// the rest the cursor stands on.
    fn name(&mut self) {
        if let Some((bucket, index)) = self.at.as_ref().and_then(|at| at.bucket.as_ref()) {
            self.key.truncate(self.tree.config().key_limit());
            self.key
                .extend_from_slice(bucket.rest(*index).unwrap_or_default());
        }
    }

    /// Stands on the entry `found`, and inside a bucket on the key at
    /// `edge`. With no entry the cursor is not positioned.
    fn settle<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        found: Option<(PageId, usize)>,
        edge: Edge,
    ) -> Result<(), FlatError> {
        self.at = None;
        self.key.clear();
        let Some((page, slot)) = found else {
            return Ok(());
        };
        let frame = pages.page(page)?;
        let (key, value) = leaf(&frame, page)?.entry(slot).ok_or(Self::moved(page))?;
        key.append_to(&mut self.key);
        let bucket = if self.tree.config().is_bucket(&self.key) {
            let mut bytes = Vec::new();
            read_value(pages, page, slot, value, &mut bytes)?;
            let bucket = self.tree.read_bucket(&bytes)?;
            let index = match edge {
                Edge::First => 0,
                Edge::Last => bucket.len() - 1,
            };
            Some((bucket, index))
        } else {
            None
        };
        self.at = Some(At {
            leaf: page,
            slot,
            bucket,
        });
        self.name();
        Ok(())
    }

    /// Goes one record on, or one back.
    fn step<P: FlatPages + ?Sized>(&mut self, pages: &P, back: bool) -> Result<(), FlatError> {
        let Some(at) = &mut self.at else {
            return Ok(());
        };
        if let Some((bucket, index)) = &mut at.bucket {
            let to = if back {
                index.checked_sub(1)
            } else {
                Some(*index + 1)
            };
            if let Some(to) = to.filter(|&to| to < bucket.len()) {
                *index = to;
                self.name();
                return Ok(());
            }
        }
        let (page, slot) = (at.leaf, at.slot);
        let len = leaf(&pages.page(page)?, page)?.len();
        let to = if back {
            slot.checked_sub(1)
        } else {
            Some(slot + 1)
        };
        let found = match to.filter(|&to| to < len) {
            Some(to) => Some((page, to)),
            None => self.next_leaf(pages, back)?,
        };
        let edge = if back { Edge::Last } else { Edge::First };
        self.settle(pages, found, edge)
    }

    /// Goes from the leaf below the remembered branches to the nearest leaf
    /// after it that has an entry, or before it with `back`, and returns
    /// that leaf's first entry, or its last.
    fn next_leaf<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        back: bool,
    ) -> Result<Option<(PageId, usize)>, FlatError> {
        loop {
            // Up to a branch that has a child on that side.
            let (page, level) = loop {
                let Some(step) = self.steps.last_mut() else {
                    return Ok(None);
                };
                let frame = pages.page(step.page)?;
                let branch = branch(&frame, step.page)?;
                let to = if back {
                    step.index.checked_sub(1)
                } else {
                    Some(step.index + 1)
                };
                if let Some(to) = to.filter(|&to| to <= branch.len()) {
                    step.index = to;
                    break (child(&branch, to)?, branch.level());
                }
                self.steps.pop();
            };
            let (leaf, len) = self.down(pages, page, Some(level), back)?;
            if let Some(last) = len.checked_sub(1) {
                return Ok(Some((leaf, if back { last } else { 0 })));
            }
        }
    }

    /// Goes down from `page`, which is below a branch of the level
    /// `above`, along the first children, or the last, and returns the leaf
    /// at the end with the number of its entries.
    fn down<P: FlatPages + ?Sized>(
        &mut self,
        pages: &P,
        mut page: PageId,
        mut above: Option<u8>,
        last: bool,
    ) -> Result<(PageId, usize), FlatError> {
        loop {
            let frame = pages.page(page)?;
            match FlatTree::checked(&frame, page, above)? {
                FlatPage::Leaf(leaf) => return Ok((page, leaf.len())),
                FlatPage::Branch(branch) => {
                    let index = if last { branch.len() } else { 0 };
                    self.steps.push(Step { page, index });
                    above = Some(branch.level());
                    page = child(&branch, index)?;
                }
            }
        }
    }
}
