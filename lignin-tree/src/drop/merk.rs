//! Reclaiming what one dropped or cleared Merk left: the records of the
//! Merks nested in it, its own record, and then its cells.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use lignin_format::{CellAddress, PageId, SlotId};
use lignin_store::values::ValueStoreError;

use super::{
    column::{Column, Sweep, Swept},
    error::DropError,
    step::{seen, value_bytes, Busy, EntryRemoved, Reclaimer, Step},
};
use crate::{
    directory::{record::dropped_key, Directory, DirectoryError, Dropped, MerkId, Prefix},
    flat::{Committed, FlatPages, StepLog},
    OperationGuard,
};

/// How far reclaiming has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Nothing is left to reclaim.
    Done,
    /// The steps that were asked for are made, and more are left.
    More,
    /// What is left waits: the steps that could be made were made, and
    /// each of those that are left was refused for now, the last of them
    /// for this.
    Blocked(Busy),
}

/// What reclaiming has taken out so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// The subtrees that were nested in the dropped one, each by the
    /// prefix it was entered under and its id, the innermost first: what
    /// else is kept by them can go.
    pub merks: Vec<(Prefix, MerkId)>,
    /// The records taken out of the directory.
    pub records: u64,
    /// The flat records of those subtrees that were taken out of their
    /// columns.
    pub flat_records: u64,
    /// The steps that took them out: each one leaf, which keeps its place
    /// in its keyspace, with no entry of the subtree left in it.
    pub flat_leaves: u64,
    /// The cells taken out of their pages.
    pub cells: u64,
    /// The bytes of those cells, with their slots.
    pub cell_bytes: u64,
    /// The tree pages that kept other cells and were written again.
    pub pages_written: u64,
    /// The pages that kept no cell and were freed.
    pub pages_freed: u64,
    /// How many of the freed pages were given no free link. A page that was
    /// made under an id which named another page before, and that
    /// write-back has not written yet, has the free link of that earlier
    /// page as the newest link of its id, and the page table takes no
    /// second one. The pool and the id are told of the free all the same,
    /// as a commit that frees such a page tells them.
    pub pages_never_linked: u64,
    /// The value pointers that the removed cells and flat records held,
    /// which the value log was told of in the step that removed each.
    pub values: u64,
    /// The bytes of the values they pointed at.
    pub value_bytes: u64,
    /// The removed cells and flat records that held a value which no
    /// value-log file had yet: it was in the log group of its commit, to be
    /// placed by write-back, and has no pointer. Nothing is reported for
    /// such a value, since no file counts it.
    pub pending_values: u64,
}

/// What became of one record of the directory.
enum Taken {
    /// It is out.
    Removed,
    /// It was gone already.
    Gone,
    /// It is not as it was read: the caller reads again.
    Again,
    Refused(Busy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The records of the Merks below go, and then the Merk's own.
    Records,
    /// The cells go. Nothing on disk says so any more.
    Cells,
    Done,
    GivenUp,
}

/// What one dropped or cleared Merk left, while it is being reclaimed.
///
/// It is made from the record of the drop ([`Directory::drops`]) once the
/// [`Wait`](super::Wait) for that drop is over, and
/// [`Reclaimer::reclaim`] takes it on step by step. See the
/// [module documentation](super) for the order of the steps and for what
/// it means that the cells go last.
///
/// **From the step that takes the Merk's own record out, this is all that
/// knows the cells that are left.** Whoever holds it calls
/// [`Reclaimer::reclaim`] until it is done. If it is dropped before that,
/// or the process ends, the cells it had not reached stay in their pages:
/// no walk leads to them any more.
#[derive(Debug)]
pub struct Reclaim {
    dropped: Dropped,
    phase: Phase,
    /// The fragment roots whose cells are to go, by page: the Merk's root,
    /// and what the cells that went named in other pages.
    pending: BTreeMap<PageId, BTreeSet<SlotId>>,
    /// The pages to look at next, the last found first.
    next: Vec<PageId>,
    /// Every cell that a cell in another page named: none is named twice.
    named: HashSet<CellAddress>,
    done: Reclaimed,
    miscount: Option<ValueStoreError>,
    /// The nested subtree whose flat records are going, and how far.
    sweep: Option<Sweep>,
}

impl Reclaim {
    /// The reclaiming of what the Merk of `dropped` left.
    pub fn new(dropped: Dropped) -> Self {
        Self {
            dropped,
            phase: Phase::Records,
            pending: BTreeMap::new(),
            next: Vec::new(),
            named: HashSet::new(),
            done: Reclaimed::default(),
            miscount: None,
            sweep: None,
        }
    }

    /// The record of the drop.
    pub const fn dropped(&self) -> Dropped {
        self.dropped
    }

    /// What was taken out so far.
    pub const fn reclaimed(&self) -> &Reclaimed {
        &self.done
    }

    /// Whether nothing is left to reclaim.
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Whether the Merk's own record is taken out and cells are left: from
    /// then on only this knows where they are.
    pub fn holds_cells(&self) -> bool {
        self.phase == Phase::Cells
    }

    /// How many fragments are known and not removed yet.
    pub fn pending(&self) -> usize {
        self.pending.values().map(BTreeSet::len).sum()
    }

    /// The first count of live values that did not take the pointers of a
    /// step as they were reported, if one did not. The step was made all
    /// the same, and the value log keeps the file it could not count
    /// (`lignin_store::values`).
    pub const fn miscount(&self) -> Option<&ValueStoreError> {
        self.miscount.as_ref()
    }

    /// A cell whose fragment is to go: the Merk's root, or what a cell that
    /// went named in another page. Returns whether it is named for the
    /// first time. A cell that two cells name is not what a write leaves,
    /// and by the second time its slot may hold another cell.
    fn found(&mut self, address: CellAddress) -> bool {
        if !self.named.insert(address) {
            return false;
        }
        let page = address.page();
        self.pending.entry(page).or_default().insert(address.slot());
        self.next.push(page);
        true
    }

    fn miscounted(&mut self, miscount: Option<ValueStoreError>) {
        if self.miscount.is_none() {
            self.miscount = miscount;
        }
    }
}

impl<'s, L: StepLog> Reclaimer<'s, L> {
    /// Takes the reclaiming of `reclaim` on by at most `steps` page steps,
    /// and says how far it has come. A look at a record that was not as it
    /// was read counts as a step, so that a call ends.
    ///
    /// `directory` is the directory as the latest version has it, with the
    /// Merk of `reclaim` among its dropped ones, `columns` are every flat
    /// keyspace that holds records of subtrees ([`Column`]), and `guard`
    /// is the guard of the caller's operation. The
    /// [`Wait`](super::Wait) for the drop is over.
    ///
    /// An error says that the step it came from changed nothing, and the
    /// call can be made again, with one exception: after
    /// [`DropError::Broken`] the reclaiming is given up, a later call fails
    /// with [`DropError::GivenUp`], and what was left stays where it is.
    ///
    /// A page that is not what a drop leaves ([`DropError::Damaged`]) is
    /// not changed, and the cells in the other pages go all the same: the
    /// call ends with the error of the first such page, and what hangs
    /// below that page stays.
    pub fn reclaim<G: OperationGuard + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        columns: &[Column<'_>],
        guard: &G,
        steps: usize,
    ) -> Result<Progress, DropError> {
        let made = match reclaim.phase {
            Phase::Done => return Ok(Progress::Done),
            Phase::GivenUp => return Err(DropError::GivenUp),
            Phase::Records | Phase::Cells => {
                self.advance(reclaim, directory, columns, guard, steps)
            }
        };
        if matches!(made, Err(DropError::Broken { .. })) {
            reclaim.phase = Phase::GivenUp;
        }
        made
    }

    fn advance<G: OperationGuard + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        columns: &[Column<'_>],
        guard: &G,
        steps: usize,
    ) -> Result<Progress, DropError> {
        let mut left = steps;
        while reclaim.phase == Phase::Records {
            if left == 0 {
                return Ok(Progress::More);
            }
            if let Some(busy) = self.records(reclaim, directory, columns, guard, &mut left)? {
                return Ok(Progress::Blocked(busy));
            }
        }
        self.cells(reclaim, left)
    }

    /// What one subtree left outside the tree pages: the flat records and
    /// then the directory records of an innermost subtree below the dropped
    /// Merk, or, when none is left below, the dropped Merk's own record.
    /// Returns why it could not be done now, if it could not.
    ///
    /// Each record of the directory is read from the pages of one version,
    /// and is then taken out of the latest version only as it was read.
    fn records<G: OperationGuard + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        columns: &[Column<'_>],
        guard: &G,
        left: &mut usize,
    ) -> Result<Option<Busy>, DropError> {
        let pages = Committed::new(self.loader, self.values, guard, self.log.latest());
        // The first Merk below, and the first below that one, down to one
        // that has none below it. No Merk is deeper than an entry can say.
        let mut parent = reclaim.dropped.id;
        let mut innermost = None;
        for _ in 0..=usize::from(u16::MAX) {
            let Some(child) = directory.children(parent).next(&pages)? else {
                break;
            };
            let below = child.id;
            innermost = Some((parent, child));
            parent = below;
        }
        let Some((parent, child)) = innermost else {
            return self.own_record(reclaim, directory, &pages, left);
        };

        // The flat records go before the entry: while the entry is there,
        // it says whose they are, and a reclaiming that begins again finds
        // them by it.
        let begun = reclaim.sweep.take().filter(|sweep| sweep.id == child.id);
        let mut sweep = begun.unwrap_or(Sweep {
            id: child.id,
            column: 0,
            from: Vec::new(),
        });
        let mut swept = Swept {
            left: *left,
            ..Swept::default()
        };
        let prefix = &child.prefix;
        let waits = self.sweep_subtree(directory, columns, prefix, guard, &mut sweep, &mut swept);
        *left = swept.left;
        let done = &mut reclaim.done;
        done.flat_records += swept.records;
        done.flat_leaves += swept.leaves;
        done.values += swept.values;
        done.value_bytes += swept.value_bytes;
        done.pending_values += swept.pending;
        reclaim.miscounted(swept.miscount);
        let unfinished = sweep.column < columns.len();
        reclaim.sweep = Some(sweep);
        if let Some(busy) = waits? {
            return Ok(Some(busy));
        }
        if unfinished {
            return Ok(None);
        }

        let pages = Committed::new(self.loader, self.values, guard, self.log.latest());
        for key in directory.left_by(&pages, parent, &child)? {
            if *left == 0 {
                return Ok(None);
            }
            match self.remove_record(reclaim, directory, &pages, &key)? {
                Taken::Refused(busy) => return Ok(Some(busy)),
                Taken::Removed => *left -= 1,
                Taken::Gone => {}
                // Which records the subtree has left is asked again.
                Taken::Again => {
                    *left -= 1;
                    return Ok(None);
                }
            }
        }
        reclaim.done.merks.push((child.prefix, child.id));
        reclaim.sweep = None;
        Ok(None)
    }

    /// Takes the record under `key` out of the directory, as `pages` have
    /// it.
    fn remove_record<P: FlatPages + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        pages: &P,
        key: &[u8],
    ) -> Result<Taken, DropError> {
        let Some(seen) = seen(pages, directory.tree(), key)? else {
            return Ok(Taken::Gone);
        };
        match self.remove_entry(directory.tree(), key, &seen)? {
            Step::Refused(busy) => Ok(Taken::Refused(busy)),
            Step::Made(EntryRemoved::Removed(miscount)) => {
                reclaim.done.records += 1;
                reclaim.miscounted(miscount);
                Ok(Taken::Removed)
            }
            Step::Made(EntryRemoved::Absent) => Ok(Taken::Gone),
            Step::Made(EntryRemoved::Changed) => Ok(Taken::Again),
        }
    }

    /// The dropped Merk's own record, which goes when no record of a Merk
    /// below it is left, and before any of its cells.
    fn own_record<P: FlatPages + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        pages: &P,
        left: &mut usize,
    ) -> Result<Option<Busy>, DropError> {
        let Dropped { id, root } = reclaim.dropped;
        let key = dropped_key(id);
        if seen(pages, directory.tree(), &key)?.is_none() {
            return Err(DirectoryError::NotDropped { id }.into());
        }
        // The cells are known by nothing but this from the next step on.
        // It is not taken while the first of them could not follow, or
        // while the root is not what a drop leaves.
        if let Some(root) = root
            && let Some(busy) = self.would_wait(root.page(), &[root.slot()])?
        {
            return Ok(Some(busy));
        }
        match self.remove_record(reclaim, directory, pages, &key)? {
            Taken::Refused(busy) => return Ok(Some(busy)),
            Taken::Gone => return Err(DirectoryError::NotDropped { id }.into()),
            Taken::Again => {
                *left -= 1;
                return Ok(None);
            }
            Taken::Removed => *left -= 1,
        }
        match root {
            Some(root) => {
                reclaim.found(root);
                reclaim.phase = Phase::Cells;
            }
            None => reclaim.phase = Phase::Done,
        }
        Ok(None)
    }

    /// The cells, from the top down: a cell goes after the cell that names
    /// it, and with every cell of its page that hangs below it there.
    fn cells(&self, reclaim: &mut Reclaim, steps: usize) -> Result<Progress, DropError> {
        let mut waiting = Vec::new();
        let mut refused = None;
        let mut failed = None;
        let mut left = steps;
        while let Some(page) = reclaim.next.pop() {
            let Some(roots) = reclaim.pending.get(&page) else {
                continue;
            };
            if left == 0 {
                reclaim.next.push(page);
                break;
            }
            let roots: Vec<SlotId> = roots.iter().copied().collect();
            match self.remove_cells(page, &roots) {
                Err(error) => {
                    // The page is looked at again by the next call. A
                    // page that is not what a drop leaves does not keep
                    // the others from going.
                    waiting.push(page);
                    let of_the_page = matches!(error, DropError::Damaged { .. });
                    failed.get_or_insert(error);
                    if !of_the_page {
                        break;
                    }
                }
                Ok(Step::Refused(busy)) => {
                    waiting.push(page);
                    refused = Some(busy);
                }
                Ok(Step::Made(removed)) => {
                    left -= 1;
                    reclaim.pending.remove(&page);
                    let done = &mut reclaim.done;
                    done.cells += removed.cells;
                    done.cell_bytes += removed.bytes;
                    done.values += removed.released.len() as u64;
                    done.value_bytes += value_bytes(&removed.released);
                    done.pending_values += removed.pending;
                    match removed.freed {
                        true => done.pages_freed += 1,
                        false => done.pages_written += 1,
                    }
                    done.pages_never_linked += u64::from(removed.never_linked);
                    reclaim.miscounted(removed.miscount);
                    for target in removed.targets {
                        if !reclaim.found(target) {
                            let what = "a cell that two cells of the subtree name";
                            failed.get_or_insert(super::step::damaged(target.page(), what));
                        }
                    }
                }
            }
        }
        let blocked = reclaim.next.is_empty() && !waiting.is_empty();
        // The pages that were refused are looked at again after what was
        // found since.
        waiting.reverse();
        waiting.append(&mut reclaim.next);
        reclaim.next = waiting;
        if let Some(error) = failed {
            return Err(error);
        }
        if reclaim.pending.is_empty() {
            reclaim.next.clear();
            reclaim.phase = Phase::Done;
            return Ok(Progress::Done);
        }
        match (blocked, refused) {
            (true, Some(busy)) => Ok(Progress::Blocked(busy)),
            _ => Ok(Progress::More),
        }
    }
}
