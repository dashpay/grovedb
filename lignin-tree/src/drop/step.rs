//! The page steps of the reclaimer: cells out of a tree page, an entry out
//! of a flat leaf, and a page freed.

use std::{cell::RefCell, collections::BTreeSet};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    flat_page::{FlatLeaf, FlatPage, FlatValue},
    tree_page::{Side, TreePage, TreePageMut},
    value_log::{ValuePlace, ValuePointer},
    wal::{write_reclaim, FlatChange, FlatRun, FlatRunWriter, Removed},
    CellAddress, CommitVersion, FormatVersion, PageId, PageSize, SlotId,
};
use lignin_store::{
    alloc::PageAllocator,
    cache::{BufferPool, PageRef, ValueId},
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    table::{Link, PageTable},
    values::{OwnersError, ValueChange, ValueStore},
};

use super::error::DropError;
use crate::flat::{FlatConfig, FlatTree, StepLog};

/// How often a step reads its page again because another step or a commit
/// came between its look and its lock.
const LOOKS: usize = 16;

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
    pub freed: bool,
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

fn damaged(page: PageId, what: &'static str) -> DropError {
    DropError::Damaged { page, what }
}

fn store(error: impl Into<OwnersError>) -> DropError {
    DropError::Store(error.into())
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
    pub(super) buffers: RefCell<LoadBuffers>,
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
        let latest = self.log.latest();
        match self
            .loader
            .get(page, latest, &mut self.buffers.borrow_mut())
        {
            Ok(frame) => Ok(Some(frame)),
            Err(LoadError::NoPage { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Whether a step on `page` would be refused now for a private copy.
    pub(super) fn is_copied(&self, page: PageId) -> bool {
        self.pool.page_step(page).private_copies() != 0
    }

    /// Gives the page its free link, tells the pool, and retires the id:
    /// the page was freed with `version` by a step whose record is in the
    /// log. The page table goes first, as the pool asks.
    fn free(&self, page: PageId, version: CommitVersion) -> Result<(), DropError> {
        self.table.push(page, Link::free(version)).map_err(store)?;
        self.pool.free(page, version).map_err(store)?;
        self.ids.free(page, version).map_err(store)
    }

    /// One page step: takes the cells that hang below the fragment roots
    /// `roots` of the tree page `page`, inside that page, out of it. A
    /// page that keeps no cell is freed in the same step, with the latest
    /// version.
    pub(super) fn remove_cells(
        &self,
        page: PageId,
        roots: &[SlotId],
    ) -> Result<Step<CellsRemoved>, DropError> {
        for _ in 0..LOOKS {
            let frame = self
                .current(page)?
                .ok_or(damaged(page, "no page where a cell of the subtree was"))?;
            let view = TreePage::from_validated(&frame, FRAME_FORMAT)
                .map_err(|_| damaged(page, "no tree page where a cell of the subtree was"))?;
            if view.id() != page {
                return Err(damaged(page, "the image of another page"));
            }
            let found = fragment(&view, roots)?;
            let empties = found.slots.len() == view.cell_count();

            // What the page is afterwards is made before any lock is taken.
            let mut after = None;
            let mut run = Vec::new();
            if !empties {
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
                after = Some(blank);
            }

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
            write_reclaim(
                page,
                removed,
                freed,
                found.released.iter().copied(),
                &mut record,
            )?;
            self.log.append(&record).map_err(DropError::Store)?;
            if let Some(blank) = after {
                step.replace(&frame, blank).map_err(store)?;
            }
            drop(step);
            if let Some(version) = freed {
                self.free(page, version)?;
            }
            let mut change = ValueChange::new();
            for &pointer in &found.released {
                change.release(pointer);
            }
            counts.apply(&change, self.log.latest())?;
            return Ok(Step::Made(CellsRemoved {
                cells: found.slots.len() as u64,
                bytes: found.bytes,
                targets: found.targets,
                released: found.released,
                freed: empties,
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
            let read = flat_page(&frame, page)?;
            if above.is_some_and(|above| above.checked_sub(1) != Some(read.level())) {
                return Err(damaged(
                    page,
                    "a page that is not one level below the branch that names it",
                ));
            }
            let FlatPage::Branch(branch) = read else {
                return Ok((page, frame));
            };
            above = Some(branch.level());
            page = branch
                .child_for(key)
                .ok_or(damaged(page, "a branch that names no child"))?;
        }
    }

    /// One page step: takes the entry under `key` out of its leaf in
    /// `tree`, and releases the value pointer it held. Returns whether
    /// there was such an entry.
    ///
    /// The leaf keeps its place in the tree and its prefix, also with no
    /// entry left: a page step changes one leaf and nothing else
    /// ([`flat`](crate::flat), choice 12). `key` is no longer than the
    /// tree stores a key as it is: an entry of the tree of a bucket is not
    /// taken out here.
    pub(super) fn remove_entry(
        &self,
        tree: &FlatTree,
        key: &[u8],
    ) -> Result<Step<bool>, DropError> {
        let config = tree.config();
        if key.len() > config.key_limit() {
            return Err(DropError::Record("a key that is kept in a bucket"));
        }
        for _ in 0..LOOKS {
            let (page, frame) = self.leaf_of(tree, key)?;
            let leaf = flat_leaf(&frame, page)?;
            let Some(value) = leaf.get(key) else {
                return Ok(Step::Made(false));
            };
            let place = match value {
                FlatValue::Inline(_) => None,
                FlatValue::OutOfLine(bytes) => Some(place_of(page, bytes)?),
            };
            let mut run = Vec::new();
            let mut removes = FlatRunWriter::new(page, &mut run);
            removes.push(&FlatChange::Remove { key })?;
            removes.finish()?;
            let removes = FlatRun::decode(&run, FRAME_FORMAT)?;
            let mut image = Vec::new();
            removes.apply(
                Some(leaf.as_bytes()),
                FRAME_FORMAT,
                config.capacity(),
                &mut image,
            )?;
            let mut blank = self.pool.blank();
            blank.copy_from(&frame);
            let (head, tail) = blank
                .split_at_mut_checked(image.len())
                .ok_or(damaged(page, "an image longer than a frame"))?;
            head.copy_from_slice(&image);
            tail.fill(0);
            if let Some(ValuePlace::Pending(pending)) = place {
                blank.detach(pending.id());
            }
            let released = place.and_then(|place| place.pointer());

            let counts = self.values.step()?;
            let step = self.pool.page_step(page);
            if step.private_copies() != 0 {
                return Ok(Step::Refused(Busy::Copied));
            }
            if !step.is_current(&frame) {
                continue;
            }
            let mut record = Vec::new();
            write_reclaim(
                page,
                Some(Removed::Flat(removes)),
                None,
                released,
                &mut record,
            )?;
            self.log.append(&record).map_err(DropError::Store)?;
            step.replace(&frame, blank).map_err(store)?;
            drop(step);
            let mut change = ValueChange::new();
            if let Some(pointer) = released {
                change.release(pointer);
            }
            counts.apply(&change, self.log.latest())?;
            return Ok(Step::Made(true));
        }
        Ok(Step::Refused(Busy::Contended))
    }

    /// One page step: frees the flat page in `frame`, which a range delete
    /// unlinked with its entries in it, and releases the value pointers
    /// `released` that its entries hold, in ascending order.
    ///
    /// Returns `None` if `frame` is not how the page is stored any more:
    /// the caller looks again.
    pub(super) fn free_unlinked(
        &self,
        frame: &PageRef<'s>,
        released: &[ValuePointer],
    ) -> Result<Option<Step<()>>, DropError> {
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
        write_reclaim(
            page,
            None,
            Some(latest),
            released.iter().copied(),
            &mut record,
        )?;
        self.log.append(&record).map_err(DropError::Store)?;
        drop(step);
        self.free(page, latest)?;
        let mut change = ValueChange::new();
        for &pointer in released {
            change.release(pointer);
        }
        counts.apply(&change, self.log.latest())?;
        Ok(Some(Step::Made(())))
    }
}

/// The flat page in `frame`, which was read as `page`.
pub(super) fn flat_page<'a>(frame: &'a [u8], page: PageId) -> Result<FlatPage<'a>, DropError> {
    let read = FlatPage::from_validated(frame, FRAME_FORMAT)
        .map_err(|_| damaged(page, "no flat page where a flat tree names one"))?;
    if read.id() != page {
        return Err(damaged(page, "the image of another page"));
    }
    Ok(read)
}

fn flat_leaf<'a>(frame: &'a [u8], page: PageId) -> Result<FlatLeaf<'a>, DropError> {
    flat_page(frame, page)?
        .leaf()
        .ok_or(damaged(page, "a branch where a leaf must be"))
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

/// The place that the 16 bytes of a value out of line are.
pub(super) fn place_of(page: PageId, bytes: &[u8]) -> Result<ValuePlace, DropError> {
    let bytes = bytes
        .try_into()
        .map_err(|_| damaged(page, "a value out of line that is no place"))?;
    Ok(ValuePlace::decode(bytes)?)
}

/// The sizes of the flat pages of a store, for whoever reads them without
/// a tree.
pub(super) const fn is_bucket(config: &FlatConfig, stored_key_len: usize) -> bool {
    stored_key_len > config.key_limit()
}

const _: FormatVersion = FRAME_FORMAT;
