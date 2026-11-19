//! The page steps of the reclaimer: cells out of a tree page, an entry out
//! of a flat leaf, and a page freed.

use std::{cell::RefCell, collections::BTreeSet};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    flat_page::{FlatLeaf, FlatPage, FlatValue},
    tree_page::{Side, TreePage, TreePageMut},
    value_log::{ValuePlace, ValuePointer},
    wal::{write_reclaim, FlatChange, FlatRun, FlatRunWriter, Removed},
    CellAddress, CommitVersion, PageId, PageSize, SlotId,
};
use lignin_store::{
    alloc::PageAllocator,
    cache::{Blank, BufferPool, PageRef, ValueId},
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    table::{Link, PageTable},
    values::{CountStep, OwnersError, ValueChange, ValueStore, ValueStoreError},
};

use super::error::DropError;
use crate::flat::{FlatPages, FlatTree, StepLog};

/// How often a step reads its page again because another step or a commit
/// came between its look and its lock.
pub(super) const LOOKS: usize = 16;

/// Why a step was not made now. The page is as it was, and the step is
/// tried again later (`transactions.md` PV7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// An open write transaction holds a private copy of the page.
    Copied,
    /// The step would free the page, and the latest commit bore the page
    /// version: a page is freed with a later version than it was born
    /// with, so the step waits for the next commit.
    Newborn,
    /// Other steps and commits changed the page each time it was looked
    /// at.
    Contended,
}

/// What became of a step.
#[derive(Debug)]
pub(super) enum Step<T> {
    Made(T),
    Refused(Busy),
}

/// What a step that took cells out of a tree page did.
#[derive(Debug, Default)]
pub(super) struct CellsRemoved {
    pub cells: u64,
    pub bytes: u64,
    /// The cells in other pages that the removed cells named.
    pub targets: Vec<CellAddress>,
    pub released: Vec<ValuePointer>,
    /// How many of the cells held a value that no value-log file has yet.
    pub pending: u64,
    pub freed: bool,
    /// Why the released pointers could not be counted as reported.
    pub miscount: Option<ValueStoreError>,
}

/// What a step that took entries out of a flat leaf did.
#[derive(Debug)]
pub(super) struct EntriesRemoved {
    pub released: Vec<ValuePointer>,
    /// How many of the entries held a value that no value-log file has
    /// yet.
    pub pending: u64,
    /// Why the released pointers could not be counted as reported.
    pub miscount: Option<ValueStoreError>,
}

/// What a step that looked for an entry of a flat leaf did.
#[derive(Debug)]
pub(super) enum EntryRemoved {
    /// The entry is out of its leaf.
    Removed(Option<ValueStoreError>),
    /// No entry is under the key.
    Absent,
    /// Another entry is under the key than the one that was seen.
    Changed,
}

/// A step on a tree page that is ready to be made.
struct Prepared<'s> {
    /// The page as it was looked at.
    frame: PageRef<'s>,
    /// The cells that go.
    found: Fragment,
    /// The page without them, or `None` if it keeps no cell.
    after: Option<Blank<'s>>,
    /// The records that take them out, if the page keeps a cell.
    run: Vec<u8>,
}

/// The cells of one page that hang below some of its fragment roots.
#[derive(Debug, Default)]
pub(super) struct Fragment {
    /// The cells, in ascending order of their slots.
    pub slots: Vec<SlotId>,
    pub bytes: u64,
    pub targets: Vec<CellAddress>,
    /// The value pointers the cells hold, in ascending order.
    pub released: Vec<ValuePointer>,
    /// The names of the pending values the cells hold.
    pub pending: Vec<ValueId>,
}

pub(super) fn damaged(page: PageId, what: &'static str) -> DropError {
    DropError::Damaged { page, what }
}

/// The error of a step whose record is in the log and that could not be
/// finished.
fn broken(page: PageId) -> impl Fn(OwnersError) -> DropError {
    move |source| DropError::Broken { page, source }
}

/// The cells of `page` that are reached from the fragment roots `roots`
/// without leaving the page, with what they name in other pages and what
/// they hold out of line.
///
/// A root is a cell that nothing in its page names, and every other cell
/// of a fragment is named once, by a cell of the fragment. A page in which
/// that does not hold is not walked.
pub(super) fn fragment(page: &TreePage<'_>, roots: &[SlotId]) -> Result<Fragment, DropError> {
    let id = page.id();
    let mut found = Fragment::default();
    let mut seen = BTreeSet::new();
    let mut next = Vec::new();
    for &root in roots {
        let cell = page
            .cell(root)
            .ok_or(damaged(id, "no cell where a pointer of the subtree leads"))?;
        if !cell.is_fragment_root() {
            return Err(damaged(
                id,
                "a cell that another page names is not marked as the root of its fragment",
            ));
        }
        if !seen.insert(root) {
            return Err(damaged(id, "a cell that two pointers name"));
        }
        next.push(root);
    }
    while let Some(slot) = next.pop() {
        let cell = page
            .cell(slot)
            .ok_or(damaged(id, "a pointer that names a free slot of its page"))?;
        found.bytes += cell.encoded_len() as u64 + SlotId::ENCODED_LEN as u64;
        let links = [Side::Left, Side::Right].map(|side| cell.link(side).map(|link| link.child));
        for address in links.into_iter().chain(cell.hosted_roots()).flatten() {
            if address.page() != id {
                found.targets.push(address);
                continue;
            }
            let inside = address.slot();
            let below = page
                .cell(inside)
                .ok_or(damaged(id, "a pointer that names a free slot of its page"))?;
            if below.is_fragment_root() {
                return Err(damaged(
                    id,
                    "a cell that its page names is marked as the root of a fragment",
                ));
            }
            if !seen.insert(inside) {
                return Err(damaged(id, "a cell that two pointers name"));
            }
            next.push(inside);
        }
        match cell.value_place() {
            Some(ValuePlace::Placed(pointer)) => found.released.push(pointer),
            Some(ValuePlace::Pending(pending)) => found.pending.push(pending.id()),
            None => {}
        }
    }
    found.slots = seen.into_iter().collect();
    sort_released(id, &mut found.released)?;
    Ok(found)
}

/// Puts the value pointers that the cells or entries of `page` hold in the
/// order a record lists them in. No two hold one pointer.
pub(super) fn sort_released(page: PageId, released: &mut [ValuePointer]) -> Result<(), DropError> {
    released.sort_unstable_by_key(|pointer| (pointer.file(), pointer.offset()));
    let twice = released
        .windows(2)
        .any(|pair| (pair[0].file(), pair[0].offset()) == (pair[1].file(), pair[1].offset()));
    match twice {
        true => Err(damaged(page, "one value pointer that two hold")),
        false => Ok(()),
    }
}

/// The bytes of the values that `released` pointed at.
pub(super) fn value_bytes(released: &[ValuePointer]) -> u64 {
    let lens = released
        .iter()
        .map(|pointer| u64::from(pointer.value_len()));
    lens.sum()
}

/// The entry under one key of a flat leaf, as it was read: what tells it
/// from another entry that is under the key later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Seen {
    Inline(Vec<u8>),
    /// The 16 bytes of the place of a value out of line.
    OutOfLine(Vec<u8>),
}

impl Seen {
    fn of(value: FlatValue<'_>) -> Self {
        match value {
            FlatValue::Inline(bytes) => Self::Inline(bytes.to_vec()),
            FlatValue::OutOfLine(place) => Self::OutOfLine(place.to_vec()),
        }
    }
}

/// The entry under the stored key `key` of `tree`, as `pages` have it.
pub(super) fn seen<P: FlatPages + ?Sized>(
    pages: &P,
    tree: &FlatTree,
    key: &[u8],
) -> Result<Option<Seen>, DropError> {
    let (mut page, mut above) = (tree.root(), None::<u8>);
    loop {
        let frame = pages.page(page)?;
        match flat_page(&frame, page, above)? {
            FlatPage::Leaf(leaf) => return Ok(leaf.get(key).map(Seen::of)),
            FlatPage::Branch(branch) => {
                above = Some(branch.level());
                page = branch
                    .child_for(key)
                    .ok_or(damaged(page, "a branch that names no child"))?;
            }
        }
    }
}

/// What the reclaimer works on: the pages of a store, its page ids, the
/// counts of its value log and its log.
///
/// Every change it makes is one page step on the current version of one
/// page (`transactions.md` PV7, CL6): it is refused under a private copy,
/// writes its record to the log under the step's lock, takes no commit
/// version, and changes the counts of the value pointers it released in
/// the same step (PV6). It holds the lock on the counts from the record to
/// the count, as every background change does, so that a sample of the
/// counts finds every step before it made (`log-format.md` LG28, LG29).
///
/// The caller holds an operation guard while it uses this.
#[derive(Debug)]
pub struct Reclaimer<'s, L> {
    pub(super) loader: PageLoader<'s>,
    pub(super) pool: &'s BufferPool,
    table: &'s PageTable,
    ids: &'s PageAllocator,
    pub(super) values: &'s ValueStore,
    page_size: PageSize,
    pub(super) log: L,
    buffers: RefCell<LoadBuffers>,
}

impl<'s, L: StepLog> Reclaimer<'s, L> {
    /// A reclaimer of the pages of `page_size` that `loader` reads into
    /// `pool`, whose chains are in `table`, whose ids are `ids` and whose
    /// values out of line are in `values`. `log` takes the record of each
    /// step and says which version is the latest.
    pub fn new(
        loader: PageLoader<'s>,
        pool: &'s BufferPool,
        table: &'s PageTable,
        ids: &'s PageAllocator,
        values: &'s ValueStore,
        page_size: PageSize,
        log: L,
    ) -> Self {
        Self {
            loader,
            pool,
            table,
            ids,
            values,
            page_size,
            log,
            buffers: RefCell::default(),
        }
    }

    /// The latest version, as the log has it now.
    pub fn latest(&self) -> CommitVersion {
        self.log.latest()
    }

    /// The current version of `page`, or `None` if the page is no page.
    pub(super) fn current(&self, page: PageId) -> Result<Option<PageRef<'s>>, DropError> {
        self.at(page, self.log.latest())
    }

    /// The version of `page` that a reader at `at` sees, or `None` if the
    /// page is no page there.
    pub(super) fn at(
        &self,
        page: PageId,
        at: CommitVersion,
    ) -> Result<Option<PageRef<'s>>, DropError> {
        match self.loader.get(page, at, &mut self.buffers.borrow_mut()) {
            Ok(frame) => Ok(Some(frame)),
            Err(LoadError::NoPage { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// A step that takes the cells below `roots` out of the tree page
    /// `page`, made ready on the page as it is now. Nothing is changed.
    fn prepare(&self, page: PageId, roots: &[SlotId]) -> Result<Prepared<'s>, DropError> {
        let frame = self
            .current(page)?
            .ok_or(damaged(page, "no page where a cell of the subtree was"))?;
        let view = TreePage::from_validated(&frame, FRAME_FORMAT)
            .map_err(|_| damaged(page, "no tree page where a cell of the subtree was"))?;
        if view.id() != page {
            return Err(damaged(page, "the image of another page"));
        }
        let found = fragment(&view, roots)?;
        let mut run = Vec::new();
        if found.slots.len() == view.cell_count() {
            let after = None;
            return Ok(Prepared {
                frame,
                found,
                after,
                run,
            });
        }
        let mut blank = self.pool.blank();
        blank.copy_from(&frame);
        let mut copy = TreePageMut::open(&mut blank, self.page_size, FRAME_FORMAT)?;
        let mut removes = DeltaRunWriter::new(page, None, &mut run);
        for &slot in &found.slots {
            let remove = Delta::Remove { slot };
            remove.apply(&mut copy)?;
            removes.push(&remove)?;
        }
        removes.finish()?;
        for name in &found.pending {
            blank.detach(*name);
        }
        TreePage::from_validated(&blank, FRAME_FORMAT)?
            .check_fragments()
            .map_err(|_| damaged(page, "a cell that stays names a cell of the subtree"))?;
        let after = Some(blank);
        Ok(Prepared {
            frame,
            found,
            after,
            run,
        })
    }

    /// Why a step that takes the cells below `roots` out of `page` would be
    /// refused now, if it would: without a change, and without a promise.
    /// The step is made ready as [`Self::remove_cells`] makes it, so a page
    /// that such a step fails on fails here.
    pub(super) fn would_wait(
        &self,
        page: PageId,
        roots: &[SlotId],
    ) -> Result<Option<Busy>, DropError> {
        let ready = self.prepare(page, roots)?;
        if self.pool.page_step(page).private_copies() != 0 {
            return Ok(Some(Busy::Copied));
        }
        let newborn = ready.after.is_none() && self.log.latest() <= ready.frame.born();
        Ok(newborn.then_some(Busy::Newborn))
    }

    /// Gives the page its free link, tells the pool, and retires the id:
    /// the page was freed with `version` by a step whose record is in the
    /// log. The page table goes first, as the pool asks.
    fn free(&self, page: PageId, version: CommitVersion) -> Result<(), DropError> {
        let broken = broken(page);
        self.table
            .push(page, Link::free(version))
            .map_err(|error| broken(error.into()))?;
        self.pool
            .free(page, version)
            .map_err(|error| broken(error.into()))?;
        self.ids
            .free(page, version)
            .map_err(|error| broken(error.into()))
    }

    /// Takes `released` off the counts of live values, at the end of a step
    /// that is made. A count that does not take them as reported is the
    /// value log's to keep safe, and does not undo the step.
    fn count(&self, counts: CountStep<'_>, released: &[ValuePointer]) -> Option<ValueStoreError> {
        let mut change = ValueChange::new();
        for &pointer in released {
            change.release(pointer);
        }
        counts.apply(&change, self.log.latest()).err()
    }

    /// One page step: takes the cells that hang below the fragment roots
    /// `roots` of the tree page `page`, inside that page, out of it. A
    /// page that keeps no cell is freed in the same step, with the latest
    /// version.
    ///
    /// An error other than [`DropError::Broken`] says that nothing was
    /// changed.
    pub(super) fn remove_cells(
        &self,
        page: PageId,
        roots: &[SlotId],
    ) -> Result<Step<CellsRemoved>, DropError> {
        for _ in 0..LOOKS {
            // A page that a writer holds is not looked at any closer.
            if self.pool.page_step(page).private_copies() != 0 {
                return Ok(Step::Refused(Busy::Copied));
            }
            // What the page is afterwards is made before any lock is taken.
            let Prepared {
                frame,
                found,
                after,
                run,
            } = self.prepare(page, roots)?;
            let empties = after.is_none();

            let counts = self.values.step()?;
            let step = self.pool.page_step(page);
            if step.private_copies() != 0 {
                return Ok(Step::Refused(Busy::Copied));
            }
            if !step.is_current(&frame) {
                continue;
            }
            let latest = self.log.latest();
            if empties && latest <= frame.born() {
                return Ok(Step::Refused(Busy::Newborn));
            }
            let freed = empties.then_some(latest);
            let removed = match empties {
                true => None,
                false => Some(Removed::Tree(DeltaRun::decode(&run, FRAME_FORMAT)?)),
            };
            let mut record = Vec::new();
            let released = found.released.iter().copied();
            write_reclaim(page, removed, freed, released, &mut record)?;
            self.log.append(&record).map_err(DropError::Store)?;
            if let Some(blank) = after {
                let replaced = step.replace(&frame, blank);
                replaced.map_err(|refusal| broken(page)(refusal.into()))?;
            }
            drop(step);
            if let Some(version) = freed {
                self.free(page, version)?;
            }
            let miscount = self.count(counts, &found.released);
            return Ok(Step::Made(CellsRemoved {
                cells: found.slots.len() as u64,
                bytes: found.bytes,
                targets: found.targets,
                released: found.released,
                pending: found.pending.len() as u64,
                freed: empties,
                miscount,
            }));
        }
        Ok(Step::Refused(Busy::Contended))
    }

    /// The leaf of `tree` whose key range holds the stored key `key`, as
    /// the latest version has it.
    fn leaf_of(&self, tree: &FlatTree, key: &[u8]) -> Result<(PageId, PageRef<'s>), DropError> {
        let (mut page, mut above) = (tree.root(), None::<u8>);
        loop {
            let frame = self
                .current(page)?
                .ok_or(damaged(page, "a flat tree that names no page"))?;
            let FlatPage::Branch(branch) = flat_page(&frame, page, above)? else {
                return Ok((page, frame));
            };
            above = Some(branch.level());
            page = branch
                .child_for(key)
                .ok_or(damaged(page, "a branch that names no child"))?;
        }
    }

    /// Takes the entry under `key` out of its leaf in `tree`, if it is the
    /// entry that was `seen`, in one page step
    /// ([`Self::remove_entries`]).
    ///
    /// **The entry is taken out only as it was seen** (PV7, check 2): a
    /// write can have put another record under the key since, and that one
    /// is not the reclaimer's. An entry whose value is out of line is known
    /// by the place of that value, which write-back and the value log's
    /// cleaner change without changing the record. So
    /// [`EntryRemoved::Changed`] says that the caller looks again, not
    /// that the record is another one.
    ///
    /// `key` is no longer than the tree stores a key as it is: an entry of
    /// the tree of a bucket is not taken out here.
    pub(super) fn remove_entry(
        &self,
        tree: &FlatTree,
        key: &[u8],
        seen: &Seen,
    ) -> Result<Step<EntryRemoved>, DropError> {
        let config = tree.config();
        if key.len() > config.key_limit() {
            return Err(DropError::Record("a key that is kept in a bucket"));
        }
        for _ in 0..LOOKS {
            let (page, frame) = self.leaf_of(tree, key)?;
            let Some(value) = flat_leaf(&frame, page)?.get(key) else {
                return Ok(Step::Made(EntryRemoved::Absent));
            };
            if Seen::of(value) != *seen {
                return Ok(Step::Made(EntryRemoved::Changed));
            }
            match self.remove_entries(&frame, config.capacity(), &[key.to_vec()])? {
                None => continue,
                Some(Step::Refused(busy)) => return Ok(Step::Refused(busy)),
                Some(Step::Made(removed)) => {
                    return Ok(Step::Made(EntryRemoved::Removed(removed.miscount)))
                }
            }
        }
        Ok(Step::Refused(Busy::Contended))
    }

    /// One page step: takes the entries under `keys`, in ascending order,
    /// out of the flat leaf in `frame`, and releases the value pointers
    /// they held. `capacity` is the longest image of a page.
    ///
    /// The leaf keeps its place in the tree and its prefix, also with no
    /// entry left: a page step changes one leaf and nothing else
    /// ([`flat`](crate::flat), choice 12).
    ///
    /// Returns `None` if `frame` is not how the page is stored any more:
    /// the caller looks again. An error other than [`DropError::Broken`]
    /// says that nothing was changed.
    pub(super) fn remove_entries(
        &self,
        frame: &PageRef<'s>,
        capacity: usize,
        keys: &[Vec<u8>],
    ) -> Result<Option<Step<EntriesRemoved>>, DropError> {
        let page = frame.page();
        let leaf = flat_leaf(frame, page)?;
        let (mut released, mut pending) = (Vec::new(), Vec::new());
        let mut run = Vec::new();
        let mut removes = FlatRunWriter::new(page, &mut run);
        for key in keys {
            let value = leaf.get(key);
            let value = value.ok_or(damaged(page, "no entry where one was read"))?;
            if let FlatValue::OutOfLine(bytes) = value {
                match place_of(page, bytes)? {
                    ValuePlace::Placed(pointer) => released.push(pointer),
                    ValuePlace::Pending(named) => pending.push(named.id()),
                }
            }
            removes.push(&FlatChange::Remove { key })?;
        }
        removes.finish()?;
        sort_released(page, &mut released)?;
        let removes = FlatRun::decode(&run, FRAME_FORMAT)?;
        let mut image = Vec::new();
        removes.apply(Some(leaf.as_bytes()), FRAME_FORMAT, capacity, &mut image)?;
        let mut blank = self.pool.blank();
        blank.copy_from(frame);
        let (head, tail) = blank
            .split_at_mut_checked(image.len())
            .ok_or(damaged(page, "an image longer than a frame"))?;
        head.copy_from_slice(&image);
        tail.fill(0);
        for name in &pending {
            blank.detach(*name);
        }
        let pending = pending.len() as u64;

        let counts = self.values.step()?;
        let step = self.pool.page_step(page);
        if step.private_copies() != 0 {
            return Ok(Some(Step::Refused(Busy::Copied)));
        }
        if !step.is_current(frame) {
            return Ok(None);
        }
        let mut record = Vec::new();
        let removed = Some(Removed::Flat(removes));
        write_reclaim(page, removed, None, released.iter().copied(), &mut record)?;
        self.log.append(&record).map_err(DropError::Store)?;
        let replaced = step.replace(frame, blank);
        replaced.map_err(|refusal| broken(page)(refusal.into()))?;
        drop(step);
        let miscount = self.count(counts, &released);
        Ok(Some(Step::Made(EntriesRemoved {
            released,
            pending,
            miscount,
        })))
    }

    /// One page step: frees the flat page in `frame`, which a range delete
    /// unlinked with its entries in it, and releases the value pointers
    /// `released` that its entries hold, in ascending order.
    ///
    /// Returns `None` if `frame` is not how the page is stored any more:
    /// the caller looks again. An error other than [`DropError::Broken`]
    /// says that nothing was changed.
    pub(super) fn free_unlinked(
        &self,
        frame: &PageRef<'s>,
        released: &[ValuePointer],
    ) -> Result<Option<Step<Option<ValueStoreError>>>, DropError> {
        let page = frame.page();
        let counts = self.values.step()?;
        let step = self.pool.page_step(page);
        if step.private_copies() != 0 {
            return Ok(Some(Step::Refused(Busy::Copied)));
        }
        if !step.is_current(frame) {
            return Ok(None);
        }
        let latest = self.log.latest();
        if latest <= frame.born() {
            return Ok(Some(Step::Refused(Busy::Newborn)));
        }
        let mut record = Vec::new();
        let listed = released.iter().copied();
        write_reclaim(page, None, Some(latest), listed, &mut record)?;
        self.log.append(&record).map_err(DropError::Store)?;
        drop(step);
        self.free(page, latest)?;
        Ok(Some(Step::Made(self.count(counts, released))))
    }
}

/// The flat page in `frame`, which was read as `page`, below a branch of
/// the level `above`.
pub(super) fn flat_page(
    frame: &[u8],
    page: PageId,
    above: Option<u8>,
) -> Result<FlatPage<'_>, DropError> {
    let read = FlatPage::from_validated(frame, FRAME_FORMAT)
        .map_err(|_| damaged(page, "no flat page where a flat tree names one"))?;
    if read.id() != page {
        return Err(damaged(page, "the image of another page"));
    }
    if above.is_some_and(|above| above.checked_sub(1) != Some(read.level())) {
        return Err(damaged(
            page,
            "a page that is not one level below the branch that names it",
        ));
    }
    Ok(read)
}

fn flat_leaf(frame: &[u8], page: PageId) -> Result<FlatLeaf<'_>, DropError> {
    flat_page(frame, page, None)?
        .leaf()
        .ok_or(damaged(page, "a branch where a leaf must be"))
}

/// The place that the 16 bytes of a value out of line are.
pub(super) fn place_of(page: PageId, bytes: &[u8]) -> Result<ValuePlace, DropError> {
    let bytes = bytes
        .try_into()
        .map_err(|_| damaged(page, "a value out of line that is no place"))?;
    Ok(ValuePlace::decode(bytes)?)
}
