//! The cells of tree pages as the owners of values: for the value log's
//! cleaner, and for regrouping the values of a page.

use std::cell::RefCell;

use lignin_format::{
    tree_page::{TreePage, TreePageMut},
    value_log::{ValuePlace, ValuePointer},
    wal::ValueMove,
    CommitVersion, PageId, PageSize,
};
use lignin_store::{
    cache::{BufferPool, PageRef, PageStep},
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    values::{
        OwnersError, PointerRewrite, Refusal, Rewrite, ValueChange, ValueHold, ValueStore,
        ValueStoreError, ValueWriter,
    },
};

use super::{
    error::ValueError,
    order::{pointers_in_order, Locality, RegroupPolicy},
    read::{read_placed, ValueBuf},
};
use crate::flat::StepLog;

/// The tree page at the start of `frame`, or `None` for a frame that holds
/// a flat page.
fn tree_page(frame: &[u8]) -> Option<TreePage<'_>> {
    TreePage::from_validated(frame, FRAME_FORMAT).ok()
}

/// The tree pages of a store, as the cleaner of the value log asks for the
/// owners of values (`lignin_store::values`), and as the flusher has the
/// values of a page regrouped.
///
/// A value's back pointer names a page, and the cell that owns the value is
/// the one that holds the pointer to it (`value-log-format.md` O1). A page
/// that is not a tree page holds no such cell here: flat leaves have owners
/// of their own ([`FlatOwners`](crate::flat::FlatOwners)).
///
/// Every change it makes is one page step on the current version of a page
/// (`transactions.md` PV7), which writes its record to the log under the
/// step's lock and takes no commit version (PV6).
///
/// The caller holds an operation guard while it uses this.
#[derive(Debug)]
pub struct TreeOwners<'s, L> {
    loader: PageLoader<'s>,
    pool: &'s BufferPool,
    page_size: PageSize,
    log: L,
    buffers: RefCell<LoadBuffers>,
}

impl<'s, L: StepLog> TreeOwners<'s, L> {
    /// The owners among the tree pages that `loader` reads into `pool`,
    /// which are pages of `page_size`. `log` takes the record of each step.
    pub fn new(loader: PageLoader<'s>, pool: &'s BufferPool, page_size: PageSize, log: L) -> Self {
        Self {
            loader,
            pool,
            page_size,
            log,
            buffers: RefCell::default(),
        }
    }

    /// The current version of `page`, if the page exists.
    fn current(&self, page: PageId) -> Result<Option<PageRef<'s>>, LoadError> {
        let latest = self.log.latest();
        match self
            .loader
            .get(page, latest, &mut self.buffers.borrow_mut())
        {
            Ok(frame) => Ok(Some(frame)),
            Err(LoadError::NoPage { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// One page step: the cell of the page in `frame` that holds `from`
    /// gets `to`, if `frame` is still how the current version of the page
    /// is stored, is the version `found` bore, and no write transaction
    /// holds a private copy of the page.
    ///
    /// The record is appended once the checks have passed and the changed
    /// page is made, and the frame is replaced only if the log took it.
    fn repoint(
        &self,
        step: &PageStep<'s>,
        frame: &PageRef<'s>,
        found: CommitVersion,
        from: ValuePointer,
        to: ValuePointer,
    ) -> Result<Result<(), Refusal>, ValueError> {
        if !step.is_current(frame) || frame.born() != found || step.private_copies() != 0 {
            return Ok(Err(Refusal::Busy));
        }
        let Some(slot) = tree_page(frame).and_then(|page| page.find_value_pointer(&from)) else {
            return Ok(Err(Refusal::OwnerGone));
        };
        let change = ValueMove::in_tree(step.page(), slot, ValuePlace::Placed(from), to)?;
        let delta = change.delta().expect("a move in a tree page has a record");
        let mut record = Vec::new();
        change.write(&mut record)?;
        let mut blank = self.pool.blank();
        blank.copy_from(frame);
        delta.apply(&mut TreePageMut::open(
            &mut blank,
            self.page_size,
            FRAME_FORMAT,
        )?)?;
        self.log.append(&record).map_err(ValueError::Log)?;
        step.replace(frame, blank)
            .map_err(|refusal| ValueError::Log(refusal.into()))?;
        Ok(Ok(()))
    }

    /// The first half of regrouping the values of the tree page `page`:
    /// copies each value its cells hold in the value log to the file of the
    /// writer `to`, in the order a scan reads them
    /// ([`scan_order`](super::scan_order)), back to back, with `page` as
    /// their owner as before.
    ///
    /// Returns `None`, and copies nothing, if `page` is no tree page, or if
    /// `policy` does not ask for it: the values lie well enough as they
    /// are.
    ///
    /// `to` is a writer of `store`. Nothing points at the copies yet.
    /// Before [`Self::install_grouped`], the caller makes them durable and
    /// has the manifest cover them: it syncs `to` and replaces the manifest
    /// ([`ValueStore::commit`]). `to` is given no other value between the
    /// first copy and the last, since it is borrowed here.
    ///
    /// If this fails part way, the copies made so far are dead space.
    pub fn copy_grouped(
        &self,
        store: &ValueStore,
        page: PageId,
        to: &mut ValueWriter,
        policy: RegroupPolicy,
    ) -> Result<Option<Grouped>, ValueError> {
        let Some(frame) = self.current(page)? else {
            return Ok(None);
        };
        let Some(pointers) = tree_page(&frame).map(|page| pointers_in_order(&page)) else {
            return Ok(None);
        };
        let before = Locality::of_pointers(&pointers);
        if !policy.applies(before) {
            return Ok(None);
        }
        let mut grouped = Grouped {
            page,
            born: frame.born(),
            before,
            moves: Vec::with_capacity(pointers.len()),
            hold: ValueHold::new(),
        };
        let mut buf = ValueBuf::new();
        for from in pointers {
            let value = read_placed(store, from, page, &mut buf)?;
            let copy = to.append(&mut grouped.hold, page, value)?;
            grouped.moves.push((from, copy));
        }
        Ok(Some(grouped))
    }

    /// The second half of regrouping: gives each cell the pointer to the
    /// copy of its value, each in one page step.
    ///
    /// A step takes the value log's lock on its counts, then the page
    /// step's lock, checks the page as a pointer rewrite of the cleaner is
    /// checked (PV7, CL5), writes its record, installs the pointer, and
    /// counts one value fewer in the old file and one more in the new
    /// (`lignin_store::values`). A step that is refused installs and counts
    /// nothing: the cell keeps its pointer, the copy is dead space, and
    /// the value reads as before. Once a step is refused because the page
    /// is superseded or copied, no later step of the call is tried: they
    /// are refused with it.
    ///
    /// `store` is the value log that `grouped` was copied in. Fails with
    /// [`ValueError::NotCovered`], and installs nothing, if its durable
    /// manifest does not cover every copy.
    ///
    /// If a step fails, the steps made before it stand and no later one is
    /// made. A step that could not be counted as reported does not stop
    /// the others, and the call then fails with that error, before any
    /// other.
    pub fn install_grouped(
        &self,
        store: &ValueStore,
        grouped: Grouped,
    ) -> Result<Regrouped, ValueError> {
        let Grouped {
            page,
            born,
            before,
            moves,
            hold,
        } = grouped;
        if moves.iter().any(|(_, copy)| !store.covers(*copy)) {
            return Err(ValueError::NotCovered { page });
        }
        let mut done = Regrouped {
            before,
            after: before,
            ..Regrouped::default()
        };
        let mut miscount = None;
        let mut failed = None;
        for (index, &(from, to)) in moves.iter().enumerate() {
            match self.step(store, page, born, from, to, &mut miscount) {
                Ok(Ok(())) => done.moved += 1,
                Ok(Err(Refusal::OwnerGone)) => done.gone += 1,
                Ok(Err(Refusal::Busy)) => {
                    done.busy += (moves.len() - index) as u64;
                    break;
                }
                Err(error) => {
                    failed = Some(error);
                    break;
                }
            }
        }
        drop(hold);
        if let Some(error) = miscount.map(ValueError::Store).or(failed) {
            return Err(error);
        }
        if let Some(frame) = self.current(page)? {
            done.after = tree_page(&frame).map_or(before, |page| Locality::of(&page));
        }
        Ok(done)
    }

    /// One step of a regrouping: the counts lock, the page step, and the
    /// count of the move inside it.
    fn step(
        &self,
        store: &ValueStore,
        page: PageId,
        born: CommitVersion,
        from: ValuePointer,
        to: ValuePointer,
        miscount: &mut Option<ValueStoreError>,
    ) -> Result<Result<(), Refusal>, ValueError> {
        let Some(frame) = self.current(page)? else {
            return Ok(Err(Refusal::OwnerGone));
        };
        let counts = store.step()?;
        let step = self.pool.page_step(page);
        if let Err(refusal) = self.repoint(&step, &frame, born, from, to)? {
            return Ok(Err(refusal));
        }
        let counted = counts.apply(&ValueChange::moved(from, to), self.log.latest());
        *miscount = miscount.take().or(counted.err());
        Ok(Ok(()))
    }
}

/// The values of one page, copied in scan order and not yet owned there:
/// the first half of a regrouping, from [`TreeOwners::copy_grouped`].
///
/// It holds the file the copies are in. Dropping it gives the copies up:
/// they are dead space.
#[derive(Debug)]
pub struct Grouped {
    page: PageId,
    born: CommitVersion,
    before: Locality,
    moves: Vec<(ValuePointer, ValuePointer)>,
    hold: ValueHold,
}

impl Grouped {
    /// The page whose values were copied.
    pub const fn page(&self) -> PageId {
        self.page
    }

    /// How many values were copied.
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    /// Whether no value was copied.
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// How the page's values lay when they were copied.
    pub const fn before(&self) -> Locality {
        self.before
    }
}

/// What a regrouping of one page did, from
/// [`TreeOwners::install_grouped`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regrouped {
    /// Values whose cells hold the pointers to the copies.
    pub moved: u64,
    /// Values whose cell no longer held the pointer when its step was to
    /// be made.
    pub gone: u64,
    /// Values whose step was refused for now: a commit had superseded the
    /// page since its values were copied, or a write transaction held a
    /// private copy of it.
    pub busy: u64,
    /// How the page's values lay before.
    pub before: Locality,
    /// How they lie now.
    pub after: Locality,
}

impl<L: StepLog> lignin_store::values::ValueOwners for TreeOwners<'_, L> {
    /// The version that bore the page version that was looked at.
    type Found = CommitVersion;

    fn find(
        &self,
        page: PageId,
        pointer: ValuePointer,
    ) -> Result<Option<CommitVersion>, OwnersError> {
        let frame = self.current(page)?;
        let holds = |frame: &PageRef<'_>| {
            tree_page(frame).is_some_and(|page| page.find_value_pointer(&pointer).is_some())
        };
        Ok(frame.filter(holds).map(|frame| frame.born()))
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: CommitVersion,
    ) -> Result<Rewrite, OwnersError> {
        let (page, from, to) = (step.page(), step.from(), step.to());
        let Some(frame) = self.current(page)? else {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        };
        let guard = self.pool.page_step(page);
        match self.repoint(&guard, &frame, found, from, to)? {
            Ok(()) => Ok(Rewrite::Installed(step.install(self.log.latest()))),
            Err(refusal) => Ok(Rewrite::Refused(refusal)),
        }
    }
}
