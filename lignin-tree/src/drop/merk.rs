//! Reclaiming what one dropped or cleared Merk left: the records of the
//! Merks nested in it, its own record, and then its cells.

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::{PageId, SlotId};

use super::{
    error::DropError,
    step::{Busy, Reclaimer, Step},
};
use crate::{
    directory::{record::dropped_key, Directory, DirectoryError, Dropped, MerkId, Prefix},
    flat::{Committed, StepLog},
    OperationGuard,
};

/// How far reclaiming has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Nothing is left to reclaim.
    Done,
    /// The steps that were asked for are made, and more are left.
    More,
    /// What is left waits: every step that could be tried was refused for
    /// now, the last of them for this.
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
    /// The cells taken out of their pages.
    pub cells: u64,
    /// The bytes of those cells, with their slots.
    pub cell_bytes: u64,
    /// The tree pages that kept other cells and were written again.
    pub pages_written: u64,
    /// The pages that kept no cell and were freed.
    pub pages_freed: u64,
    /// The value pointers the removed cells held, which the value log was
    /// told of in the step that removed each cell.
    pub values: u64,
    /// The bytes of the values they pointed at.
    pub value_bytes: u64,
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
    done: Reclaimed,
}

impl Reclaim {
    /// The reclaiming of what the Merk of `dropped` left.
    pub fn new(dropped: Dropped) -> Self {
        Self {
            dropped,
            phase: Phase::Records,
            pending: BTreeMap::new(),
            next: Vec::new(),
            done: Reclaimed::default(),
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

    fn found(&mut self, page: PageId, slot: SlotId) {
        self.pending.entry(page).or_default().insert(slot);
        self.next.push(page);
    }
}

impl<'s, L: StepLog> Reclaimer<'s, L> {
    /// Takes the reclaiming of `reclaim` on by at most `steps` page steps,
    /// and says how far it has come.
    ///
    /// `directory` is the directory as the latest version has it, with the
    /// Merk of `reclaim` among its dropped ones, and `guard` the guard of
    /// the caller's operation. The [`Wait`](super::Wait) for the drop is
    /// over.
    ///
    /// After an error the reclaiming is given up: a later call fails with
    /// [`DropError::GivenUp`], and what was left stays where it is.
    pub fn reclaim<G: OperationGuard + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        guard: &G,
        steps: usize,
    ) -> Result<Progress, DropError> {
        let made = match reclaim.phase {
            Phase::Done => return Ok(Progress::Done),
            Phase::GivenUp => return Err(DropError::GivenUp),
            Phase::Records | Phase::Cells => self.advance(reclaim, directory, guard, steps),
        };
        if made.is_err() {
            reclaim.phase = Phase::GivenUp;
        }
        made
    }

    fn advance<G: OperationGuard + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        guard: &G,
        steps: usize,
    ) -> Result<Progress, DropError> {
        let mut left = steps;
        while reclaim.phase == Phase::Records {
            if left == 0 {
                return Ok(Progress::More);
            }
            if let Some(busy) = self.records(reclaim, directory, guard, &mut left)? {
                return Ok(Progress::Blocked(busy));
            }
        }
        self.cells(reclaim, left)
    }

    /// One Merk's records: those of an innermost Merk below the dropped
    /// one, or, when none is left below, the dropped Merk's own. Returns
    /// why it could not be done now, if it could not.
    fn records<G: OperationGuard + ?Sized>(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
        guard: &G,
        left: &mut usize,
    ) -> Result<Option<Busy>, DropError> {
        let pages = Committed::new(self.loader, self.values, guard, self.log.latest());
        let id = reclaim.dropped.id;
        // The first Merk below, and the first below that one, down to one
        // that has none below it. No Merk is deeper than an entry can say.
        let mut parent = id;
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
            return self.own_record(reclaim, directory);
        };
        for key in directory.left_by(&pages, parent, &child)? {
            match self.remove_entry(directory.tree(), &key)? {
                Step::Made(removed) => reclaim.done.records += u64::from(removed),
                Step::Refused(busy) => return Ok(Some(busy)),
            }
            *left = left.saturating_sub(1);
        }
        reclaim.done.merks.push((child.prefix, child.id));
        Ok(None)
    }

    /// The dropped Merk's own record, which goes when no record of a Merk
    /// below it is left, and before any of its cells.
    fn own_record(
        &self,
        reclaim: &mut Reclaim,
        directory: &Directory,
    ) -> Result<Option<Busy>, DropError> {
        let Dropped { id, root } = reclaim.dropped;
        // The cells are known by nothing but this from the next step on.
        // It is not taken while the first of them could not follow.
        if root.is_some_and(|root| self.is_copied(root.page())) {
            return Ok(Some(Busy::Copied));
        }
        match self.remove_entry(directory.tree(), &dropped_key(id))? {
            Step::Refused(busy) => return Ok(Some(busy)),
            Step::Made(false) => return Err(DirectoryError::NotDropped { id }.into()),
            Step::Made(true) => reclaim.done.records += 1,
        }
        match root {
            Some(root) => {
                reclaim.found(root.page(), root.slot());
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
        let mut left = steps;
        while let Some(page) = reclaim.next.pop() {
            let Some(roots) = reclaim.pending.get(&page) else {
                continue;
            };
            if left == 0 {
                reclaim.next.push(page);
                break;
            }
            left -= 1;
            let roots: Vec<SlotId> = roots.iter().copied().collect();
            match self.remove_cells(page, &roots)? {
                Step::Refused(busy) => {
                    waiting.push(page);
                    refused = Some(busy);
                }
                Step::Made(removed) => {
                    reclaim.pending.remove(&page);
                    let done = &mut reclaim.done;
                    done.cells += removed.cells;
                    done.cell_bytes += removed.bytes;
                    done.values += removed.released.len() as u64;
                    done.value_bytes += removed
                        .released
                        .iter()
                        .map(|pointer| u64::from(pointer.value_len()))
                        .sum::<u64>();
                    match removed.freed {
                        true => done.pages_freed += 1,
                        false => done.pages_written += 1,
                    }
                    for target in removed.targets {
                        reclaim.found(target.page(), target.slot());
                    }
                }
            }
        }
        let blocked = reclaim.next.is_empty() && !waiting.is_empty();
        // The pages that were refused are looked at again by the next call,
        // after what was found since.
        waiting.reverse();
        waiting.append(&mut reclaim.next);
        reclaim.next = waiting;
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
