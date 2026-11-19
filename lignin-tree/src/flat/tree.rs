//! The B+tree: get, put, delete and range delete, and how its pages are
//! split, merged and removed.

use std::{cmp::Ordering, collections::BTreeSet};

use lignin_format::{
    flat_page::{shortest_separator, FlatLeaf, FlatPage, FlatPageKind, FlatValue},
    PageId,
};

use super::{
    cursor::{prefix_end, Cursor},
    error::FlatError,
    long_key::{bucket_root, Stored},
    node::{
        branch, branch_entries, child, drop_held, leaf, leaf_entries, place, read_value, view,
        write_branch, write_leaf, BranchEntry, KeyRef, Layout, LeafEntry, ValueRef,
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

/// A child that a range delete reaches, with the bounds of its key range.
type Reached = (PageId, Option<Vec<u8>>, Option<Vec<u8>>);

/// What a range delete did to the pages of one tree.
#[derive(Debug, Default)]
struct Pruned {
    /// The leaves that lost entries and are still there.
    leaves: Vec<PageId>,
    /// Whether a page was freed.
    freed: bool,
}

/// A bucket that a write went through: the tree that holds it, its stored
/// key, and its own tree.
type Passed = (FlatTree, Vec<u8>, FlatTree);

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
        let (mut tree, mut key) = (*self, key);
        let mut way = Way::default();
        loop {
            match self.config.stored(key) {
                Stored::Short(key) => return tree.get_stored(pages, key, out),
                Stored::Long { bucket, rest } => {
                    let Some(next) = tree.bucket(pages, &bucket, &mut way)? else {
                        return Ok(false);
                    };
                    (tree, key) = (next, rest);
                }
            }
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
        let (mut tree, mut key) = (*self, key);
        let mut way = Way::default();
        loop {
            match self.config.stored(key) {
                Stored::Short(key) => return tree.put_stored(pages, key, value),
                Stored::Long { bucket, rest } => {
                    let next = match tree.bucket(pages, &bucket, &mut way)? {
                        Some(next) => next,
                        None => {
                            let next = Self::create(pages, self.config)?;
                            tree.put_stored(pages, &bucket, &next.root.encode())?;
                            next
                        }
                    };
                    (tree, key) = (next, rest);
                }
            }
        }
    }

    /// Removes the record under `key`, and says whether there was one.
    pub fn delete<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
    ) -> Result<bool, FlatError> {
        let (mut tree, mut key) = (*self, key);
        let (mut passed, mut way) = (Vec::new(), Way::default());
        loop {
            match self.config.stored(key) {
                Stored::Short(key) => {
                    if !tree.delete_stored(pages, key)? {
                        return Ok(false);
                    }
                    break;
                }
                Stored::Long { bucket, rest } => {
                    let Some(next) = tree.bucket(pages, &bucket, &mut way)? else {
                        return Ok(false);
                    };
                    passed.push((tree, bucket, next));
                    (tree, key) = (next, rest);
                }
            }
        }
        Self::close_empty(pages, passed)?;
        Ok(true)
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
        let mut ranges = vec![(*self, start, end)];
        let (mut passed, mut way) = (Vec::new(), Way::default());
        while let Some((tree, start, end)) = ranges.pop() {
            if end.is_some_and(|end| end <= start) {
                continue;
            }
            // A bucket that an end of the range lies in loses the keys on
            // the range's side, as a range of its own tree. Between the
            // two ends whole entries leave.
            let mut inside = |pages: &mut P, bucket: &[u8], start, end| {
                if let Some(next) = tree.bucket(pages, bucket, &mut way)? {
                    ranges.push((next, start, end));
                    passed.push((tree, bucket.to_vec(), next));
                }
                Ok::<_, FlatError>(())
            };
            let first = self.config.stored(start);
            let to = match end.map(|end| self.config.stored(end)) {
                None => None,
                Some(Stored::Short(key)) => Some(key.to_vec()),
                Some(Stored::Long { bucket, rest }) => {
                    match &first {
                        Stored::Long {
                            bucket: same,
                            rest: from,
                        } if *same == bucket => {
                            inside(pages, &bucket, from, Some(rest))?;
                            continue;
                        }
                        _ => inside(pages, &bucket, &[], Some(rest))?,
                    }
                    Some(bucket)
                }
            };
            let from = match first {
                Stored::Short(key) => key.to_vec(),
                Stored::Long { mut bucket, rest } => {
                    inside(pages, &bucket, rest, None)?;
                    // No stored key lies between a bucket and this.
                    bucket.push(0);
                    bucket
                }
            };
            tree.delete_range_stored(pages, &from, to.as_deref())?;
        }
        Self::close_empty(pages, passed)
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

/// The trees that a call has gone into on its way through buckets. Each
/// is gone into once: a bucket whose tree is one of them is damage, and
/// would make a key stand for another.
#[derive(Debug, Default)]
struct Way(BTreeSet<PageId>);

impl Way {
    /// Goes from the tree with the root `from` into the tree with the root
    /// `into`, which a bucket of the first names.
    fn go_into(&mut self, from: PageId, into: PageId) -> Result<(), FlatError> {
        self.0.insert(from);
        if self.0.insert(into) {
            return Ok(());
        }
        Err(FlatError::Damaged {
            page: into,
            what: "a bucket of long keys whose tree is a tree it is in",
        })
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
        read_value(pages, &frame, page, slot, value, out)?;
        Ok(true)
    }

    /// The tree of the bucket stored under `key`, if there is one, on the
    /// `way` that led to this tree.
    fn bucket<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        key: &[u8],
        way: &mut Way,
    ) -> Result<Option<Self>, FlatError> {
        let (page, frame) = self.walk(pages, key, None)?;
        let Some(value) = leaf(&frame, page)?.get(key) else {
            return Ok(None);
        };
        let root = bucket_root(page, value)?;
        way.go_into(self.root, root)?;
        Ok(Some(Self::open(root, self.config)))
    }

    /// Whether the keyspace has no record.
    fn is_empty<P: FlatPages + ?Sized>(&self, pages: &P) -> Result<bool, FlatError> {
        let frame = pages.page(self.root)?;
        let root = Self::checked(&frame, self.root, None)?;
        Ok(root.leaf().is_some_and(|leaf| leaf.is_empty()))
    }

    /// The image of `page`, copied, for a look that is to hold no page.
    fn image<P: FlatPages + ?Sized>(pages: &P, page: PageId) -> Result<Vec<u8>, FlatError> {
        let frame = pages.page(page)?;
        Ok(view(&frame)?.as_bytes().to_vec())
    }
}

/// Writing one key.
impl FlatTree {
    /// Frees the trees of the buckets in `passed` that have no record left,
    /// the innermost first, and takes each such bucket out of its tree.
    ///
    /// A tree that is seen empty stays empty: only its writer adds to it.
    fn close_empty<P: FlatPagesMut + ?Sized>(
        pages: &mut P,
        passed: Vec<Passed>,
    ) -> Result<(), FlatError> {
        for (tree, bucket, inner) in passed.into_iter().rev() {
            if inner.is_empty(pages)? {
                pages.take(inner.root)?;
                pages.free(inner.root)?;
                tree.delete_stored(pages, &bucket)?;
            }
        }
        Ok(())
    }

    fn put_stored<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), FlatError> {
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
                drop_held(pages, page, entries[slot].value)?;
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
        drop_held(pages, page, entries.remove(slot).value)?;
        write_leaf(pages, self.config.capacity(), page, &entries)?;
        self.rebalance(pages, steps, page)?;
        Ok(true)
    }
}

/// What two neighbouring pages hold together, as one page has it.
enum Joined<'a> {
    Leaves(Vec<LeafEntry<'a>>),
    /// The level, the first child, and the separators: those of the left
    /// branch, the separator that the branch above had between the two,
    /// with the first child of the right branch, and those of the right
    /// branch.
    Branches(u8, PageId, Vec<BranchEntry<'a>>),
}

impl<'a> Joined<'a> {
    /// The neighbours `left` and `right` below one branch, which has
    /// `between` between them, as one page.
    fn of(left: FlatPage<'a>, between: &'a [u8], right: FlatPage<'a>) -> Result<Self, FlatError> {
        match (left, right) {
            (FlatPage::Leaf(left), FlatPage::Leaf(right)) => Ok(Self::Leaves(
                [leaf_entries(&left), leaf_entries(&right)].concat(),
            )),
            (FlatPage::Branch(left), FlatPage::Branch(right)) => {
                let mut entries = branch_entries(&left)?;
                entries.push(BranchEntry {
                    key: KeyRef::Whole(between),
                    child: child(&right, 0)?,
                });
                entries.extend(branch_entries(&right)?);
                Ok(Self::Branches(left.level(), child(&left, 0)?, entries))
            }
            // Two pages of one level are of one kind.
            (left, _) => Err(FlatError::Damaged {
                page: left.id(),
                what: "a leaf and a branch below one branch",
            }),
        }
    }

    /// The length of the page that holds them.
    fn len(&self) -> usize {
        match self {
            Self::Leaves(entries) => Layout::of(entries).len(),
            Self::Branches(_, _, entries) => Layout::of(entries).len(),
        }
    }
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
        let (slot, level, left, right, separator) = {
            let frame = pages.page(parent)?;
            let branch = branch(&frame, parent)?;
            let Some(slot) = branch.len().checked_sub(1).map(|last| index.min(last)) else {
                return Ok(Merge::NoNeighbour);
            };
            let separator = branch.separator(slot).map(|key| key.to_vec());
            let (left, right) = (child(&branch, slot)?, child(&branch, slot + 1)?);
            (
                slot,
                branch.level(),
                left,
                right,
                separator.unwrap_or_default(),
            )
        };
        // What the two make together, read from their images.
        let join = |first, second| {
            let first = Self::checked(first, left, Some(level))?;
            let second = Self::checked(second, right, Some(level))?;
            Joined::of(first, &separator, second)
        };

        // The two are looked at before they are taken, one after the
        // other: a page that stays as it is costs no copy of its own.
        let (first, second) = (Self::image(pages, left)?, Self::image(pages, right)?);
        if join(&first, &second)?.len() > self.config.merge_limit() {
            return Ok(Merge::TooLong);
        }

        let (first, second) = (pages.take(left)?, pages.take(right)?);
        match join(&first, &second)? {
            Joined::Leaves(entries) => write_leaf(pages, capacity, left, &entries)?,
            Joined::Branches(level, head, entries) => {
                write_branch(pages, capacity, left, level, head, &entries)?;
            }
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
            let (only, level) = {
                let frame = pages.page(self.root)?;
                match view(&frame)? {
                    FlatPage::Branch(branch) if branch.is_empty() => {
                        (child(&branch, 0)?, branch.level())
                    }
                    _ => return Ok(()),
                }
            };
            pages.take(self.root)?;
            let old = pages.take(only)?;
            match Self::checked(&old, only, Some(level))? {
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
        let mut pruned = Pruned::default();
        self.prune(
            pages,
            self.root,
            None,
            (None, None),
            (start, end),
            &mut pruned,
        )?;
        if pruned.leaves.is_empty() && !pruned.freed {
            return Ok(());
        }
        self.collapse_root(pages)?;
        // A leaf at an end of the range that lost entries without leaving
        // is looked at, and where pages left, the leaf at each end, which
        // took over their key range.
        for edge in [Some(start), end].into_iter().flatten() {
            let (steps, page) = self.descend(pages, edge)?;
            if pruned.freed || pruned.leaves.contains(&page) {
                self.rebalance(pages, steps, page)?;
            }
        }
        Ok(())
    }

    /// The positions of the entries of `leaf` whose keys are from `start`
    /// up to `end`.
    fn span(leaf: &FlatLeaf<'_>, start: &[u8], end: Option<&[u8]>) -> (usize, usize) {
        let at = |key| leaf.search(key).unwrap_or_else(|at| at);
        let from = at(start);
        (from, end.map_or(leaf.len(), at).max(from))
    }

    /// Removes the keys of `range` from the page `page`, which is below a
    /// branch of the level `above`, and from the pages below it, whose keys
    /// lie between `bounds`. Returns whether the page is gone: its whole
    /// key range was in the range, and it was freed with everything below
    /// it.
    fn prune<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        page: PageId,
        above: Option<u8>,
        bounds: (Option<&[u8]>, Option<&[u8]>),
        range: (&[u8], Option<&[u8]>),
        pruned: &mut Pruned,
    ) -> Result<bool, FlatError> {
        let capacity = self.config.capacity();
        let ((lower, upper), (start, end)) = (bounds, range);
        let covered = lower.map_or(start.is_empty(), |lower| lower >= start)
            && end.is_none_or(|end| upper.is_some_and(|upper| upper <= end));
        if covered && page != self.root {
            self.free_all(pages, page, above)?;
            pruned.freed = true;
            return Ok(true);
        }

        // The children the range reaches, as the page is read. A leaf is
        // taken only if the range reaches an entry of it.
        let reached = {
            let frame = pages.page(page)?;
            match Self::checked(&frame, page, above)? {
                FlatPage::Leaf(leaf) => {
                    let (from, to) = Self::span(&leaf, start, end);
                    if from == to {
                        return Ok(false);
                    }
                    None
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
                    let children = (first..=last)
                        .map(|index| {
                            let lower = index.checked_sub(1).and_then(separator);
                            Ok((child(&branch, index)?, lower, separator(index)))
                        })
                        .collect::<Result<Vec<Reached>, FlatError>>()?;
                    Some((first, branch.level(), children))
                }
            }
        };
        let Some((first, level, children)) = reached else {
            // Which entries leave is read from the leaf as it was taken: a
            // step of background work can have taken entries out since.
            let old = pages.take(page)?;
            let leaf = leaf(&old, page)?;
            let (from, to) = Self::span(&leaf, start, end);
            let mut entries = leaf_entries(&leaf);
            for entry in entries.drain(from..to) {
                self.release(pages, page, &entry)?;
            }
            if from < to {
                write_leaf(pages, capacity, page, &entries)?;
                pruned.leaves.push(page);
            }
            return Ok(false);
        };

        let mut gone = Vec::with_capacity(children.len());
        for (child, below, over) in &children {
            let bounds = (below.as_deref().or(lower), over.as_deref().or(upper));
            gone.push(self.prune(pages, *child, Some(level), bounds, range, pruned)?);
        }
        if !gone.contains(&true) {
            return Ok(false);
        }
        let old = pages.take(page)?;
        let branch = branch(&old, page)?;
        let same = children
            .iter()
            .enumerate()
            .all(|(at, (child, ..))| branch.child(first + at) == Some(*child));
        if !same {
            return Err(FlatError::Damaged {
                page,
                what: "a branch that is not as it was read",
            });
        }
        // A child that stays keeps the separator to its left, unless it is
        // the first that stays.
        let mut kept = (0..=branch.len())
            .filter(|index| index.checked_sub(first).and_then(|at| gone.get(at)) != Some(&true));
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
        Ok(false)
    }

    /// An entry of the leaf `page` leaves the tree: its value out of line
    /// is dropped, and the tree of a bucket is freed.
    fn release<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        page: PageId,
        entry: &LeafEntry<'_>,
    ) -> Result<(), FlatError> {
        if !self.config.is_bucket(entry.key.len()) {
            return drop_held(pages, page, entry.value);
        }
        let root = match entry.value {
            ValueRef::Inline(root) => FlatValue::Inline(root),
            ValueRef::Held { place, .. } | ValueRef::New(place) => FlatValue::OutOfLine(place),
        };
        self.free_all(pages, bucket_root(page, root)?, None)
    }

    /// Frees `page`, which is below a branch of the level `above`, and
    /// every page below it, with the trees of the buckets among their
    /// entries, and drops the values the entries hold out of line.
    fn free_all<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        page: PageId,
        above: Option<u8>,
    ) -> Result<(), FlatError> {
        let mut left = vec![(page, above)];
        while let Some((page, above)) = left.pop() {
            let old = pages.take(page)?;
            match Self::checked(&old, page, above)? {
                FlatPage::Leaf(leaf) => {
                    for (key, value) in leaf {
                        if self.config.is_bucket(key.len()) {
                            left.push((bucket_root(page, value)?, None));
                        } else if let FlatValue::OutOfLine(held) = value {
                            pages.drop_value(page, place(held)?)?;
                        }
                    }
                }
                FlatPage::Branch(branch) => {
                    for index in 0..=branch.len() {
                        left.push((child(&branch, index)?, Some(branch.level())));
                    }
                }
            }
            pages.free(page)?;
        }
        Ok(())
    }
}
