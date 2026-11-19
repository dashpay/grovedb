//! The entries of flat leaves as the owners of values, for the value log's
//! cleaner.

use std::cell::RefCell;

use lignin_format::{
    flat_page::FlatPage,
    value_log::{ValuePlace, ValuePointer},
    wal::ValueMove,
    CommitVersion, PageId,
};
use lignin_store::{
    cache::BufferPool,
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    values::{OwnersError, PointerRewrite, Refusal, Rewrite, ValueOwners},
};

use super::pages::FlatConfig;

/// What a pointer rewrite needs of whoever keeps the log and the versions.
pub trait StepLog {
    /// The latest commit version, read as it is at the call.
    fn latest(&self) -> CommitVersion;

    /// Appends the record of a page step to the log. The step is made only
    /// if this returns `Ok`, and it is called under the page step's lock,
    /// so that the record stands in the log where the step was made
    /// (`log-format.md` LG25).
    fn append(&self, record: &[u8]) -> Result<(), OwnersError>;
}

impl<L: StepLog + ?Sized> StepLog for &L {
    fn latest(&self) -> CommitVersion {
        (**self).latest()
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        (**self).append(record)
    }
}

/// The flat leaves of a store, as the cleaner of the value log asks for
/// the owners of values (`lignin_store::values`).
///
/// A value's back pointer names a page, and the entry that owns the value
/// is the one whose out-of-line value is the pointer to it
/// (`value-log-format.md` O1). A page that is not a flat leaf holds no such
/// entry here: tree pages have owners of their own (TREE-08).
///
/// The caller holds an operation guard while it uses this.
#[derive(Debug)]
pub struct FlatOwners<'s, L> {
    loader: PageLoader<'s>,
    pool: &'s BufferPool,
    config: FlatConfig,
    log: L,
    buffers: RefCell<LoadBuffers>,
}

impl<'s, L: StepLog> FlatOwners<'s, L> {
    /// The owners among the flat leaves that `loader` reads into `pool`,
    /// which are of the sizes `config`. `log` takes the record of each
    /// rewrite.
    pub fn new(loader: PageLoader<'s>, pool: &'s BufferPool, config: FlatConfig, log: L) -> Self {
        Self {
            loader,
            pool,
            config,
            log,
            buffers: RefCell::default(),
        }
    }

    /// The current version of `page`, if the page exists.
    fn current(
        &self,
        page: PageId,
        latest: CommitVersion,
    ) -> Result<Option<lignin_store::cache::PageRef<'s>>, OwnersError> {
        match self
            .loader
            .get(page, latest, &mut self.buffers.borrow_mut())
        {
            Ok(frame) => Ok(Some(frame)),
            Err(LoadError::NoPage { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// Whether the frame holds a flat leaf with an entry whose value is at
/// `pointer`.
fn holds(frame: &[u8], pointer: ValuePointer) -> bool {
    FlatPage::from_validated(frame, FRAME_FORMAT)
        .ok()
        .and_then(FlatPage::leaf)
        .is_some_and(|leaf| pointer.find_in_flat_leaf(&leaf).is_some())
}

impl<L: StepLog> ValueOwners for FlatOwners<'_, L> {
    /// The version that bore the page version that was looked at.
    type Found = CommitVersion;

    fn find(
        &self,
        page: PageId,
        pointer: ValuePointer,
    ) -> Result<Option<CommitVersion>, OwnersError> {
        let frame = self.current(page, self.log.latest())?;
        Ok(frame
            .filter(|frame| holds(frame, pointer))
            .map(|frame| frame.born()))
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: CommitVersion,
    ) -> Result<Rewrite, OwnersError> {
        let (page, from, to) = (step.page(), step.from(), step.to());
        let latest = self.log.latest();
        let Some(frame) = self.current(page, latest)? else {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        };
        let guard = self.pool.page_step(page);
        if !guard.is_current(&frame) || frame.born() != found || guard.private_copies() != 0 {
            return Ok(Rewrite::Refused(Refusal::Busy));
        }
        if !holds(&frame, from) {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        }
        // A flat page is written whole: the step puts a new image in the
        // place of the old one, which differs in the bytes of the pointer.
        let change = ValueMove::in_flat_leaf(page, ValuePlace::Placed(from), to)?;
        let mut image = Vec::new();
        change.apply_to_flat_leaf(&frame, FRAME_FORMAT, self.config.capacity(), &mut image)?;
        let mut record = Vec::new();
        change.write(&mut record)?;
        let mut blank = self.pool.blank();
        blank.copy_from(&frame);
        blank
            .get_mut(..image.len())
            .ok_or("an image longer than a frame")?
            .copy_from_slice(&image);
        self.log.append(&record)?;
        guard.replace(&frame, blank)?;
        // The step is dated by the latest version as it is now, which a
        // commit to another page can have moved since the page was read.
        Ok(Rewrite::Installed(step.install(self.log.latest())))
    }
}
