//! The B+tree: get, put, delete and range delete, and how its pages are
//! split, merged and removed.

use std::cmp::Ordering;

use lignin_format::{
    flat_page::{shortest_separator, FlatBranch, FlatPage, FlatPageKind, FlatValue},
    PageId,
};

use super::{
    cursor::{prefix_end, Cursor},
    error::FlatError,
    long_key::{Bucket, Stored},
    node::{
        branch, branch_entries, child, leaf, leaf_entries, place, read_value, view, write_branch,
        write_leaf, BranchEntry, KeyRef, Layout, LeafEntry, ValueRef,
    },
    pages::{value_len, FlatConfig, FlatPages, FlatPagesMut},
};

/// One step of a descent: a branch, and the position of the child that was
/// taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Step {
    pub(super) page: PageId,
    pub(super) index: usize,
}

/// What became of an attempt to merge a page with its neighbour.
enum Merge {
    Done,
    /// The page is the only child of its parent.
    NoNeighbour,
    /// The two together are longer than a merged page may be.
    TooLong,
}

/// What a range delete finds to do in a page.
enum Plan {
    Nothing,
    /// The entries in these positions leave the leaf.
    Entries(usize, usize),
    /// The range reaches the children in these positions, each with the
    /// bounds of its key range.
    Children(usize, Vec<Reached>),
}

type Reached = (PageId, Option<Vec<u8>>, Option<Vec<u8>>);

/// A flat keyspace: one B+tree of flat pages, named by its root page.
///
/// It is a root page id and the store's sizes, and holds no page. Every
/// call is given the pages to work on: [`FlatPages`] to read, and
/// [`FlatPagesMut`] to write. See the [module documentation](super).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatTree {
    root: PageId,
    config: FlatConfig,
}

impl FlatTree {
    /// Makes an empty keyspace: one page, an empty leaf.
    pub fn create<P: FlatPagesMut + ?Sized>(
        pages: &mut P,
        config: FlatConfig,
    ) -> Result<Self, FlatError> {
        let root = pages.allocate()?;
        write_leaf(pages, config.capacity(), root, &[])?;
        Ok(Self { root, config })
    }

    /// The keyspace whose root is the page `root`, in a store of the sizes
    /// `config`.
    pub const fn open(root: PageId, config: FlatConfig) -> Self {
        Self { root, config }
    }

    /// The root page. It is the same page for as long as the keyspace
    /// exists, however the tree grows and shrinks.
    pub const fn root(&self) -> PageId {
        self.root
    }

    /// The sizes the keyspace is laid out with.
    pub const fn config(&self) -> FlatConfig {
        self.config
    }

    /// A cursor over the whole keyspace, not positioned.
    pub fn cursor(&self) -> Cursor {
        Cursor::new(*self, &[])
    }

    /// A cursor over the keys that start with `prefix`, which it shows
    /// without the prefix. It is not positioned.
    pub fn cursor_within(&self, prefix: &[u8]) -> Cursor {
        Cursor::new(*self, prefix)
    }

    /// Appends the value under `key` to `out`, and says whether there is
    /// one.
    pub fn get<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, FlatError> {
        match self.config.stored(key) {
            Stored::Short(key) => self.get_stored(pages, key, out),
            Stored::Long { bucket, rest } => Ok(self
                .bucket(pages, &bucket)?
                .and_then(|bucket| bucket.get(rest).map(|value| out.extend_from_slice(value)))
                .is_some()),
        }
    }

    /// Stores `value` under `key`, in place of any value that is there.
    pub fn put<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), FlatError> {
        value_len(value)?;
        match self.config.stored(key) {
            Stored::Short(key) => self.put_stored(pages, key, value),
            Stored::Long { bucket, rest } => {
                let mut records = self.bucket(pages, &bucket)?.unwrap_or_default();
                records.put(rest, value);
                self.put_stored(pages, &bucket, &records.encode()?)
            }
        }
    }

    /// Removes the record under `key`, and says whether there was one.
    pub fn delete<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
    ) -> Result<bool, FlatError> {
        match self.config.stored(key) {
            Stored::Short(key) => self.delete_stored(pages, key),
            Stored::Long { bucket, rest } => {
                let Some(mut records) = self.bucket(pages, &bucket)? else {
                    return Ok(false);
                };
                if !records.remove(rest) {
                    return Ok(false);
                }
                self.put_bucket(pages, &bucket, &records)?;
                Ok(true)
            }
        }
    }

    /// Removes every record whose key is at or after `start` and before
    /// `end`, or to the end of the keyspace with no `end`.
    ///
    /// Every leaf that holds such a record is read, so that the values its
    /// entries held out of line are dropped.
    pub fn delete_range<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        start: &[u8],
        end: Option<&[u8]>,
    ) -> Result<(), FlatError> {
        if end.is_some_and(|end| end <= start) {
            return Ok(());
        }
        // A bucket that an end of the range lies in loses the keys on the
        // range's side. Between the two ends whole entries leave.
        let (start, end) = (
            self.config.stored(start),
            end.map(|end| self.config.stored(end)),
        );
        let to = match end {
            None => None,
            Some(Stored::Short(key)) => Some(key.to_vec()),
            Some(Stored::Long { bucket, rest: to }) => match &start {
                Stored::Long {
                    bucket: first,
                    rest,
                } if *first == bucket => {
                    return self.trim_bucket(pages, &bucket, Some(rest), Some(to));
                }
                _ => {
                    self.trim_bucket(pages, &bucket, None, Some(to))?;
                    Some(bucket)
                }
            },
        };
        let from = match start {
            Stored::Short(key) => key.to_vec(),
            Stored::Long { mut bucket, rest } => {
                self.trim_bucket(pages, &bucket, Some(rest), None)?;
                // No stored key lies between a bucket and this.
                bucket.push(0);
                bucket
            }
        };
        self.delete_range_stored(pages, &from, to.as_deref())
    }

    /// Removes every record whose key starts with `prefix`: the range
    /// delete of one keyspace among several that share a tree.
    pub fn delete_prefix<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        prefix: &[u8],
    ) -> Result<(), FlatError> {
        self.delete_range(pages, prefix, prefix_end(prefix).as_deref())
    }
}

/// Reading.
impl FlatTree {
    /// A page that was read as `page`, checked to be that page and, below a
    /// branch of the level `above`, to be one level down.
    pub(super) fn checked<'a>(
        bytes: &'a [u8],
        page: PageId,
        above: Option<u8>,
    ) -> Result<FlatPage<'a>, FlatError> {
        let read = view(bytes)?;
        if read.id() != page {
            return Err(FlatError::Damaged {
                page,
                what: "the image of another page",
            });
        }
        if above.is_some_and(|above| above.checked_sub(1) != Some(read.level())) {
            return Err(FlatError::Damaged {
                page,
                what: "a page that is not one level below the branch that names it",
            });
        }
        Ok(read)
    }

    /// Goes down from the root to the leaf whose key range holds `key`, and
    /// returns that leaf. The branches passed are added to `steps`.
    pub(super) fn walk<'p, P: FlatPages + ?Sized>(
        &self,
        pages: &'p P,
        key: &[u8],
        mut steps: Option<&mut Vec<Step>>,
    ) -> Result<(PageId, P::Page<'p>), FlatError> {
        let (mut page, mut above) = (self.root, None);
        loop {
            let frame = pages.page(page)?;
            let FlatPage::Branch(branch) = Self::checked(&frame, page, above)? else {
                return Ok((page, frame));
            };
            let index = branch.child_index(key);
            if let Some(steps) = steps.as_deref_mut() {
                steps.push(Step { page, index });
            }
            above = Some(branch.level());
            page = child(&branch, index)?;
        }
    }

    /// The branches from the root down to the leaf whose key range holds
    /// `key`, and that leaf.
    pub(super) fn descend<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        key: &[u8],
    ) -> Result<(Vec<Step>, PageId), FlatError> {
        let mut steps = Vec::new();
        let (page, _) = self.walk(pages, key, Some(&mut steps))?;
        Ok((steps, page))
    }

    fn get_stored<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, FlatError> {
        let (page, frame) = self.walk(pages, key, None)?;
        let leaf = leaf(&frame, page)?;
        let Some((slot, value)) = leaf
            .search(key)
            .ok()
            .and_then(|slot| Some((slot, leaf.value(slot)?)))
        else {
            return Ok(false);
        };
        read_value(pages, page, slot, value, out)?;
        Ok(true)
    }

    /// The bucket stored under `key`, if there is one.
    fn bucket<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        key: &[u8],
    ) -> Result<Option<Bucket>, FlatError> {
        let mut bytes = Vec::new();
        if !self.get_stored(pages, key, &mut bytes)? {
            return Ok(None);
        }
        self.read_bucket(&bytes).map(Some)
    }

    /// The bucket that the value `bytes` of an entry is.
    pub(super) fn read_bucket(&self, bytes: &[u8]) -> Result<Bucket, FlatError> {
        Bucket::decode(bytes).ok_or(FlatError::Damaged {
            page: self.root,
            what: "a bucket of long keys that does not read",
        })
    }
}

/// Writing one key.
impl FlatTree {
    fn put_bucket<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
        bucket: &Bucket,
    ) -> Result<(), FlatError> {
        if bucket.is_empty() {
            self.delete_stored(pages, key).map(|_| ())
        } else {
            self.put_stored(pages, key, &bucket.encode()?)
        }
    }

    /// Takes the rests from `from` up to `to` out of the bucket stored
    /// under `key`.
    fn trim_bucket<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
        from: Option<&[u8]>,
        to: Option<&[u8]>,
    ) -> Result<(), FlatError> {
        let Some(mut bucket) = self.bucket(pages, key)? else {
            return Ok(());
        };
        if bucket.remove_range(from, to) {
            self.put_bucket(pages, key, &bucket)?;
        }
        Ok(())
    }

    fn put_stored<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), FlatError> {
        value_len(value)?;
        let (steps, page) = self.descend(pages, key)?;
        let old = pages.take(page)?;
        let leaf = leaf(&old, page)?;
        let mut entries = leaf_entries(&leaf);
        let value = if value.len() > self.config.max_inline() {
            ValueRef::New(value)
        } else {
            ValueRef::Inline(value)
        };
        let added = match leaf.search(key) {
            Ok(slot) => {
                if let ValueRef::Held { place: old, .. } = entries[slot].value {
                    pages.drop_value(page, place(old)?)?;
                }
                entries[slot].value = value;
                None
            }
            Err(slot) => {
                let key = KeyRef::Whole(key);
                entries.insert(slot, LeafEntry { key, value });
                Some(slot)
            }
        };
        self.store_leaf(pages, &steps, page, &entries, added)
    }

    /// Writes the leaf `page`, which is below `steps`, with `entries`. If
    /// they are too long for one page, the leaf is split.
    ///
    /// `added` is the position of an entry that is new, if one is: where
    /// keys arrive in order, the split leaves the pages before them full.
    fn store_leaf<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        steps: &[Step],
        page: PageId,
        entries: &[LeafEntry<'_>],
        added: Option<usize>,
    ) -> Result<(), FlatError> {
        let capacity = self.config.capacity();
        let layout = Layout::of(entries);
        if layout.len() <= capacity {
            return write_leaf(pages, capacity, page, entries);
        }
        let cuts = layout.cuts(capacity, 0, added);
        let separators: Vec<Vec<u8>> = cuts
            .iter()
            .map(|&cut| {
                let (left, right) = (entries[cut - 1].key.to_vec(), entries[cut].key.to_vec());
                shortest_separator(&left, &right).to_vec()
            })
            .collect();
        let ids = self.pieces(pages, steps, page, cuts.len())?;
        // The page that entries are carried out of is written last.
        for (piece, &id) in ids.iter().enumerate().rev() {
            let start = piece.checked_sub(1).map_or(0, |before| cuts[before]);
            let end = cuts.get(piece).copied().unwrap_or(entries.len());
            write_leaf(pages, capacity, id, &entries[start..end])?;
        }
        self.grow(pages, steps, 1, &ids, &separators)
    }

    /// Writes the branch `page` of `level`, which is below `steps`, as
    /// [`Self::store_leaf`] writes a leaf. A branch that is split gives the
    /// separator at each cut to the page above it.
    fn store_branch<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        steps: &[Step],
        page: PageId,
        level: u8,
        first: PageId,
        entries: &[BranchEntry<'_>],
    ) -> Result<(), FlatError> {
        let capacity = self.config.capacity();
        let layout = Layout::of(entries);
        if layout.len() <= capacity {
            return write_branch(pages, capacity, page, level, first, entries);
        }
        let cuts = layout.cuts(capacity, 1, None);
        let separators: Vec<Vec<u8>> = cuts.iter().map(|&cut| entries[cut].key.to_vec()).collect();
        let ids = self.pieces(pages, steps, page, cuts.len())?;
        for (piece, &id) in ids.iter().enumerate() {
            let before = piece.checked_sub(1).map(|before| cuts[before]);
            let start = before.map_or(0, |cut| cut + 1);
            let end = cuts.get(piece).copied().unwrap_or(entries.len());
            let first = before.map_or(first, |cut| entries[cut].child);
            write_branch(pages, capacity, id, level, first, &entries[start..end])?;
        }
        let above = level.checked_add(1).ok_or(FlatError::Damaged {
            page,
            what: "a tree of more than 255 levels",
        })?;
        self.grow(pages, steps, above, &ids, &separators)
    }

    /// The pages for the pieces of the page `page`, which is cut `cuts`
    /// times. The first piece stays in `page`, unless `page` is the root:
    /// the root keeps its id, and becomes the branch above every piece.
    fn pieces<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        steps: &[Step],
        page: PageId,
        cuts: usize,
    ) -> Result<Vec<PageId>, FlatError> {
        let mut ids = Vec::with_capacity(cuts + 1);
        if !steps.is_empty() {
            ids.push(page);
        }
        while ids.len() <= cuts {
            ids.push(pages.allocate()?);
        }
        Ok(ids)
    }

    /// Gives the pieces `ids` of the page below `steps` to the branch above
    /// it: each separator, with the piece to its right. With no branch
    /// above, the page was the root, and the root becomes a branch of
    /// `level` over the pieces.
    fn grow<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        steps: &[Step],
        level: u8,
        ids: &[PageId],
        separators: &[Vec<u8>],
    ) -> Result<(), FlatError> {
        let added = separators
            .iter()
            .zip(&ids[1..])
            .map(|(key, &child)| BranchEntry {
                key: KeyRef::Whole(key),
                child,
            });
        let Some((&Step { page, index }, above)) = steps.split_last() else {
            let added: Vec<_> = added.collect();
            return self.store_branch(pages, &[], self.root, level, ids[0], &added);
        };
        let old = pages.take(page)?;
        let branch = branch(&old, page)?;
        let mut entries = branch_entries(&branch)?;
        let at = index.min(entries.len());
        entries.splice(at..at, added);
        let (level, first) = (branch.level(), child(&branch, 0)?);
        self.store_branch(pages, above, page, level, first, &entries)
    }

    fn delete_stored<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
    ) -> Result<bool, FlatError> {
        let mut steps = Vec::new();
        let (page, frame) = self.walk(pages, key, Some(&mut steps))?;
        // A key that is not there costs no copy of the page.
        let found = leaf(&frame, page)?.search(key).is_ok();
        drop(frame);
        if !found {
            return Ok(false);
        }
        let old = pages.take(page)?;
        let leaf = leaf(&old, page)?;
        let Ok(slot) = leaf.search(key) else {
            return Ok(false);
        };
        let mut entries = leaf_entries(&leaf);
        if let ValueRef::Held { place: old, .. } = entries.remove(slot).value {
            pages.drop_value(page, place(old)?)?;
        }
        write_leaf(pages, self.config.capacity(), page, &entries)?;
        self.rebalance(pages, steps, page)?;
        Ok(true)
    }
}

/// The separators of two neighbouring branches as one branch has them:
/// those of `left`, the separator `between` that the branch above had
/// between the two, with the first child of `right`, and those of `right`.
fn joined<'a>(
    left: &FlatBranch<'a>,
    between: &'a [u8],
    right: &FlatBranch<'a>,
) -> Result<Vec<BranchEntry<'a>>, FlatError> {
    let mut entries = branch_entries(left)?;
    entries.push(BranchEntry {
        key: KeyRef::Whole(between),
        child: child(right, 0)?,
    });
    entries.extend(branch_entries(right)?);
    Ok(entries)
}

/// Pages that shrink: merging, removing, and the root.
impl FlatTree {
    /// Looks at `page`, which is below `steps` and has lost entries, and at
    /// the branches above it for as far as the change goes up. An empty
    /// leaf is removed. A page under a quarter of its capacity is merged
    /// with a neighbour below the same branch, if the two fit one page of
    /// three quarters.
    fn rebalance<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        mut steps: Vec<Step>,
        mut page: PageId,
    ) -> Result<(), FlatError> {
        while let Some(&Step {
            page: parent,
            index,
        }) = steps.last()
        {
            let (len, empty_leaf) = {
                let frame = pages.page(page)?;
                let read = view(&frame)?;
                let is_leaf = read.kind() == FlatPageKind::Leaf;
                (read.as_bytes().len(), is_leaf && read.is_empty())
            };
            if empty_leaf {
                pages.take(page)?;
                pages.free(page)?;
                steps.truncate(self.remove_child(pages, &steps)?);
            } else if len >= self.config.underfull() {
                return Ok(());
            } else {
                match self.merge(pages, parent, index)? {
                    Merge::TooLong => return Ok(()),
                    Merge::Done | Merge::NoNeighbour => {}
                }
            }
            // The branch above has changed, or has one child: it is looked
            // at next.
            match steps.pop() {
                Some(step) => page = step.page,
                None => return Ok(()),
            }
        }
        self.collapse_root(pages)
    }

    /// Merges the child in position `index` of the branch `parent` with a
    /// neighbour: the one to its right, or to its left for the last child.
    /// The left page of the two takes the entries of both, and the right
    /// page is freed.
    fn merge<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        parent: PageId,
        index: usize,
    ) -> Result<Merge, FlatError> {
        let capacity = self.config.capacity();
        let (slot, left, right, separator) = {
            let frame = pages.page(parent)?;
            let branch = branch(&frame, parent)?;
            let Some(slot) = branch.len().checked_sub(1).map(|last| index.min(last)) else {
                return Ok(Merge::NoNeighbour);
            };
            let separator = branch.separator(slot).map(|key| key.to_vec());
            let (left, right) = (child(&branch, slot)?, child(&branch, slot + 1)?);
            (slot, left, right, separator.unwrap_or_default())
        };
        let mismatch = FlatError::Damaged {
            page: parent,
            what: "a branch over a leaf and a branch",
        };

        // The two are looked at before they are taken: a page that stays
        // as it is costs no copy.
        let fits = {
            let (first, second) = (pages.page(left)?, pages.page(right)?);
            let merged = match (view(&first)?, view(&second)?) {
                (FlatPage::Leaf(first), FlatPage::Leaf(second)) => {
                    Layout::of(&[leaf_entries(&first), leaf_entries(&second)].concat()).len()
                }
                (FlatPage::Branch(first), FlatPage::Branch(second)) => {
                    Layout::of(&joined(&first, &separator, &second)?).len()
                }
                _ => return Err(mismatch),
            };
            merged <= self.config.merge_limit()
        };
        if !fits {
            return Ok(Merge::TooLong);
        }

        let (first, second) = (pages.take(left)?, pages.take(right)?);
        match (view(&first)?, view(&second)?) {
            (FlatPage::Leaf(first), FlatPage::Leaf(second)) => {
                let entries = [leaf_entries(&first), leaf_entries(&second)].concat();
                write_leaf(pages, capacity, left, &entries)?;
            }
            (FlatPage::Branch(first), FlatPage::Branch(second)) => {
                let entries = joined(&first, &separator, &second)?;
                let (level, head) = (first.level(), child(&first, 0)?);
                write_branch(pages, capacity, left, level, head, &entries)?;
            }
            _ => return Err(mismatch),
        }
        pages.free(right)?;

        let old = pages.take(parent)?;
        let branch = branch(&old, parent)?;
        let mut entries = branch_entries(&branch)?;
        entries.remove(slot.min(entries.len() - 1));
        let (level, first) = (branch.level(), child(&branch, 0)?);
        write_branch(pages, capacity, parent, level, first, &entries)?;
        Ok(Merge::Done)
    }

    /// Takes the child at the end of `steps`, which is gone, out of its
    /// branch. A branch that had no other child is freed and taken out of
    /// its own branch in turn.
    ///
    /// Returns how many of `steps` are left. The last of them is the branch
    /// that was written. None left says that the root had nothing else
    /// below it, and is an empty leaf now.
    fn remove_child<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        steps: &[Step],
    ) -> Result<usize, FlatError> {
        let capacity = self.config.capacity();
        for (above, &Step { page, index }) in steps.iter().enumerate().rev() {
            let old = pages.take(page)?;
            let branch = branch(&old, page)?;
            if branch.is_empty() {
                if page != self.root {
                    pages.free(page)?;
                }
                continue;
            }
            let mut entries = branch_entries(&branch)?;
            let mut first = child(&branch, 0)?;
            // A child leaves with the separator to its left. The first
            // child has none, and the child after it becomes the first.
            match index.checked_sub(1) {
                Some(slot) => drop(entries.remove(slot.min(entries.len() - 1))),
                None => first = entries.remove(0).child,
            }
            let level = branch.level();
            write_branch(pages, capacity, page, level, first, &entries)?;
            return Ok(above + 1);
        }
        pages.take(self.root)?;
        write_leaf(pages, capacity, self.root, &[])?;
        Ok(0)
    }

    /// While the root is a branch with one child, the root takes what that
    /// child holds, with the child's prefix, and the child is freed.
    fn collapse_root<P: FlatPagesMut + ?Sized>(&self, pages: &mut P) -> Result<(), FlatError> {
        let capacity = self.config.capacity();
        loop {
            let only = {
                let frame = pages.page(self.root)?;
                match view(&frame)? {
                    FlatPage::Branch(branch) if branch.is_empty() => child(&branch, 0)?,
                    _ => return Ok(()),
                }
            };
            pages.take(self.root)?;
            let old = pages.take(only)?;
            match view(&old)? {
                FlatPage::Leaf(leaf) => {
                    let entries = leaf_entries(&leaf);
                    write_leaf(pages, capacity, self.root, &entries)?;
                }
                FlatPage::Branch(branch) => {
                    let entries = branch_entries(&branch)?;
                    let (level, first) = (branch.level(), child(&branch, 0)?);
                    write_branch(pages, capacity, self.root, level, first, &entries)?;
                }
            }
            pages.free(only)?;
        }
    }
}

/// Removing a range.
impl FlatTree {
    fn delete_range_stored<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        start: &[u8],
        end: Option<&[u8]>,
    ) -> Result<(), FlatError> {
        if end.is_some_and(|end| end <= start) {
            return Ok(());
        }
        self.prune(pages, self.root, (None, None), start, end)?;
        self.collapse_root(pages)?;
        // The leaf at each end of the range lost entries without leaving.
        for edge in [Some(start), end].into_iter().flatten() {
            let (steps, page) = self.descend(pages, edge)?;
            self.rebalance(pages, steps, page)?;
        }
        Ok(())
    }

    /// Removes the keys from `start` up to `end` from the page `page` and
    /// the pages below it, whose keys lie between `bounds`. Returns whether
    /// the page is gone: its whole key range was in the range, and it was
    /// freed with everything below it.
    fn prune<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        page: PageId,
        bounds: (Option<&[u8]>, Option<&[u8]>),
        start: &[u8],
        end: Option<&[u8]>,
    ) -> Result<bool, FlatError> {
        let capacity = self.config.capacity();
        let (lower, upper) = bounds;
        let covered = lower.map_or(start.is_empty(), |lower| lower >= start)
            && end.is_none_or(|end| upper.is_some_and(|upper| upper <= end));
        if covered && page != self.root {
            self.free_all(pages, page)?;
            return Ok(true);
        }

        let plan = {
            let frame = pages.page(page)?;
            match view(&frame)? {
                FlatPage::Leaf(leaf) => {
                    let at = |key| leaf.search(key).unwrap_or_else(|at| at);
                    let (from, to) = (at(start), end.map_or(leaf.len(), at));
                    if from < to {
                        Plan::Entries(from, to)
                    } else {
                        Plan::Nothing
                    }
                }
                FlatPage::Branch(branch) => {
                    let first = branch.child_index(start);
                    // The child whose range begins at `end` is not reached.
                    let last = end.map_or(branch.len(), |end| {
                        let index = branch.child_index(end);
                        let begins = index
                            .checked_sub(1)
                            .and_then(|slot| branch.separator(slot))
                            .is_some_and(|separator| separator.cmp_bytes(end) == Ordering::Equal);
                        index - usize::from(begins)
                    });
                    let separator = |slot: usize| branch.separator(slot).map(|key| key.to_vec());
                    let reached = (first..=last)
                        .map(|index| {
                            let lower = index.checked_sub(1).and_then(separator);
                            Ok((child(&branch, index)?, lower, separator(index)))
                        })
                        .collect::<Result<Vec<Reached>, FlatError>>()?;
                    Plan::Children(first, reached)
                }
            }
        };
        match plan {
            Plan::Nothing => {}
            Plan::Entries(from, to) => {
                let old = pages.take(page)?;
                let leaf = leaf(&old, page)?;
                let mut entries = leaf_entries(&leaf);
                for entry in entries.drain(from.min(leaf.len())..to.min(leaf.len())) {
                    if let ValueRef::Held { place: old, .. } = entry.value {
                        pages.drop_value(page, place(old)?)?;
                    }
                }
                write_leaf(pages, capacity, page, &entries)?;
            }
            Plan::Children(first, reached) => {
                let mut gone = Vec::with_capacity(reached.len());
                for (child, below, above) in &reached {
                    let bounds = (below.as_deref().or(lower), above.as_deref().or(upper));
                    gone.push(self.prune(pages, *child, bounds, start, end)?);
                }
                if !gone.contains(&true) {
                    return Ok(false);
                }
                let old = pages.take(page)?;
                let branch = branch(&old, page)?;
                // A child that stays keeps the separator to its left,
                // unless it is the first that stays.
                let mut kept = (0..=branch.len()).filter(|index| {
                    index.checked_sub(first).and_then(|at| gone.get(at)) != Some(&true)
                });
                let Some(head) = kept.next() else {
                    if page != self.root {
                        pages.free(page)?;
                        return Ok(true);
                    }
                    write_leaf(pages, capacity, page, &[])?;
                    return Ok(false);
                };
                let all = branch_entries(&branch)?;
                let entries: Vec<_> = kept
                    .filter_map(|index| all.get(index - 1).copied())
                    .collect();
                let (level, head) = (branch.level(), child(&branch, head)?);
                write_branch(pages, capacity, page, level, head, &entries)?;
            }
        }
        Ok(false)
    }

    /// Frees `page` and every page below it, and drops the values their
    /// entries hold out of line.
    fn free_all<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        page: PageId,
    ) -> Result<(), FlatError> {
        let old = pages.take(page)?;
        match view(&old)? {
            FlatPage::Leaf(leaf) => {
                for (_, value) in leaf {
                    if let FlatValue::OutOfLine(old) = value {
                        pages.drop_value(page, place(old)?)?;
                    }
                }
            }
            FlatPage::Branch(branch) => {
                for index in 0..=branch.len() {
                    self.free_all(pages, child(&branch, index)?)?;
                }
            }
        }
        pages.free(page)
    }
}
