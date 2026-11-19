//! Replay: the items of the log, in order, made to the pages that the
//! table checkpoint gives, to the counts of the value log and to what is
//! known of the page ids (`page-table.md` K3, K11, K13; `log-format.md`
//! LG15, LG16, LG28, LG29; `transactions.md` DU2, DU5).

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use lignin_format::{
    delta::{Delta, DeltaRun},
    flat_page::FlatPage,
    segment::{BlockContent, Compression},
    tree_page::TreePage,
    wal::{
        Change, CheckpointMark, Entry, FlatChange, FlatRun, Group, LogItem, LogPlace, Reclaim,
        Removed, ValueMove, WalError,
    },
    CommitVersion, FormatError, PageId, PageSize,
};

use super::{error::RecoveryError, RecoveryReport};
use crate::{
    alloc,
    cache::{BufferPool, OwnerId, PageRef, ValueId},
    flush::{
        page::{self, PageKind},
        FlushState,
    },
    load::{LoadBuffers, PageLoader, FRAME_FORMAT},
    segment::{BlockLocation, ManifestStore, SegmentStore, SegmentStoreError, SegmentWriter},
    table::{Link, LinkKind, PageTable},
    values::{net, Miscount, ValueChange, ValueRecovery, ValueStoreError},
    wal::{LogEnd, LogRead, LogStore},
};

/// Who the private copies belong to that replay installs its pages
/// through. No transaction is open while a store is recovered, and no copy
/// is left when recovery returns.
const OWNER: OwnerId = OwnerId::new(u64::MAX);

/// A page as replay has rebuilt it so far.
#[derive(Debug)]
struct Rebuilt {
    /// The page, as a frame holds it.
    bytes: Vec<u8>,
    /// The version the page version is born at: the newest replayed commit
    /// that changed the page, or, for a page that only background changes
    /// were replayed on, the version of the link it was read at.
    born: CommitVersion,
    /// The oldest replayed commit whose change is here and in no block.
    /// `None` for a page that only background changes were replayed on.
    oldest: Option<CommitVersion>,
}

impl Rebuilt {
    fn changed_by(&mut self, version: CommitVersion) {
        self.born = version;
        self.oldest.get_or_insert(version);
    }

    /// Applies the run of a flat page to the page, which becomes the image
    /// the run gives. `image` is memory for that image. A run that starts
    /// from an image of its own does not read the page. On an error the
    /// page is as it was.
    fn apply_flat(
        &mut self,
        run: &FlatRun<'_>,
        capacity: usize,
        image: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        image.clear();
        run.apply(Some(&self.bytes), FRAME_FORMAT, capacity, image)?;
        let (start, rest) = self.bytes.split_at_mut(image.len());
        start.copy_from_slice(image);
        rest.fill(0);
        Ok(())
    }
}

/// What a store is given for one recovery, and what replay keeps while it
/// reads the log.
pub(super) struct Replay<'s> {
    pool: &'s BufferPool,
    table: &'s PageTable,
    segments: &'s SegmentStore,
    flush: &'s FlushState,
    loader: PageLoader<'s>,
    size: PageSize,
    /// The version the table checkpoint covers, or 0 with no checkpoint.
    covers: CommitVersion,
    /// The place of the checkpoint's sample of the value counts.
    place: Option<LogPlace>,
    /// How many rebuilt pages replay keeps before it writes some to a
    /// segment.
    max_pages: usize,
    /// The number of rebuilt pages at which the next such write is made.
    spill_at: usize,
    /// The items that were replayed since the last one.
    since_spill: usize,

    /// The pages that replay has changed and no block holds.
    pages: HashMap<PageId, Rebuilt>,
    /// The pages that hold the change of a replayed commit, whether replay
    /// keeps them or has written them to a segment. The chain that the
    /// checkpoint copied lacks that commit, and so every background change
    /// that was made to the page after it.
    replayed: HashSet<PageId>,
    /// The pages that had a change dropped because the table has no image
    /// of them, and that no later item has freed or made again (K11).
    dropped: BTreeSet<PageId>,
    counts: ValueRecovery,
    ids: alloc::Recovery,
    /// The version of the last group that was read.
    last_group: Option<CommitVersion>,
    /// Whether the mark of the checkpoint's sample was found at its place.
    marked: bool,
    /// The segment that rebuilt pages are written to while the log is
    /// read.
    writer: Option<SegmentWriter>,
    buffers: LoadBuffers,
    image: Vec<u8>,
    /// Page buffers that are not in use.
    spare: Vec<Vec<u8>>,
    report: RecoveryReport,
}

impl<'s> Replay<'s> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        pool: &'s BufferPool,
        table: &'s PageTable,
        segments: &'s SegmentStore,
        flush: &'s FlushState,
        loader: PageLoader<'s>,
        size: PageSize,
        counts: ValueRecovery,
        covers: CommitVersion,
        place: Option<LogPlace>,
        max_pages: usize,
    ) -> Self {
        let max_pages = max_pages.max(1);
        Self {
            pool,
            table,
            segments,
            flush,
            loader,
            size,
            covers,
            place,
            max_pages,
            spill_at: max_pages,
            since_spill: 0,
            pages: HashMap::new(),
            replayed: HashSet::new(),
            dropped: BTreeSet::new(),
            counts,
            ids: alloc::Recovery::new(),
            last_group: None,
            marked: false,
            writer: None,
            buffers: LoadBuffers::new(),
            image: Vec::new(),
            spare: Vec::new(),
            report: RecoveryReport {
                covers,
                ..RecoveryReport::default()
            },
        }
    }

    /// Replays one item of the log. `manifest` is the store's, which names
    /// the segment that rebuilt pages are written to when there are more
    /// of them than replay keeps.
    pub(super) fn item(
        &mut self,
        read: LogRead<'_>,
        manifest: &mut ManifestStore,
    ) -> Result<(), RecoveryError> {
        // What the counts include is decided by the place alone (DU5), and
        // so is which background changes are looked at (LG28).
        let counted = self.place.is_none_or(|place| read.at >= place);
        let len = read.end.offset().saturating_sub(read.at.offset());
        self.report.items += 1;
        self.report.log_bytes += len;
        let mut group = None;
        match read.item {
            LogItem::Commit(commit) => group = self.commit(read.at, &commit, counted)?,
            LogItem::Change(change) => self.change(read.at, &change, counted)?,
            LogItem::Mark(mark) => self.mark(read.end, &mark)?,
        }
        // From the first commit that the checkpoint does not cover on, the
        // log is held until a later checkpoint covers it.
        if self.report.commits > 0 {
            self.flush
                .replayed(group.map(|version| (version, read.at)), len);
        }
        // More pages than replay keeps are written to a segment: when
        // that many have come since the last time, and again once as many
        // items were replayed as pages are kept, because a page that could
        // not be written then, for its pending value, may have had the
        // value placed since.
        self.since_spill += 1;
        let kept = self.pages.len();
        if kept >= self.spill_at || (kept >= self.max_pages && self.since_spill >= kept) {
            self.spill(manifest)?;
        }
        Ok(())
    }

    /// The group of a commit. Returns its version if the checkpoint does
    /// not cover it.
    fn commit(
        &mut self,
        at: LogPlace,
        group: &Group<'_>,
        counted: bool,
    ) -> Result<Option<CommitVersion>, RecoveryError> {
        let version = group.version();
        if self.last_group.is_none() && self.covers.next().is_some_and(|next| version > next) {
            // The log is kept from the group of the commit after the one
            // the checkpoint covers (K1): its first group is that one or
            // an earlier one.
            return Err(RecoveryError::CommitsMissing {
                covers: self.covers,
                found: version,
            });
        }
        self.last_group = Some(version);
        let replayed = version > self.covers;
        let mut change = ValueChange::new();
        for entry in group.entries() {
            match entry {
                Entry::Tree(run) if replayed => self.tree_run(at, version, &run)?,
                Entry::Flat(run) if replayed => self.flat_run(at, version, &run)?,
                Entry::Freed(page) if replayed => self.freed(page, version)?,
                // A commit that the checkpoint covers: every chain holds
                // what it did (K1).
                Entry::Tree(_) | Entry::Flat(_) | Entry::Freed(_) => {}
                Entry::Released(pointer) => change.release(pointer),
                Entry::Acquired(pointer) => change.acquire(pointer),
            }
        }
        if counted {
            self.count(at, &change)?;
        }
        if !replayed {
            return Ok(None);
        }
        self.report.commits += 1;
        Ok(Some(version))
    }

    /// Whether the chain of `page` already holds what the commit `version`
    /// did to it (K3).
    fn has(&self, page: PageId, version: CommitVersion) -> bool {
        self.table
            .newest(page)
            .is_some_and(|link| link.version >= version)
    }

    /// The page that the chain of `page` gives, if it gives one: `None`
    /// for a page with no link, and for one whose newest link is a free
    /// link.
    fn load(&mut self, at: LogPlace, page: PageId) -> Result<Option<PageRef<'s>>, RecoveryError> {
        let Some(link) = self.newest_in_use(page) else {
            return Ok(None);
        };
        self.loader
            .get(page, link.version, &mut self.buffers)
            .map(Some)
            .map_err(|source| RecoveryError::Page { page, at, source })
    }

    fn newest_in_use(&self, page: PageId) -> Option<Link> {
        self.table
            .newest(page)
            .filter(|link| link.kind != LinkKind::Free)
    }

    /// The page `page` among the pages replay changes. If it is not there,
    /// it is put there, born at `born`, with bytes that mean nothing yet.
    fn rebuilt(&mut self, page: PageId, born: CommitVersion) -> &mut Rebuilt {
        let (spare, size) = (&mut self.spare, self.size.get());
        self.pages.entry(page).or_insert_with(|| Rebuilt {
            bytes: spare.pop().unwrap_or_else(|| vec![0; size]),
            born,
            oldest: None,
        })
    }

    /// Takes the page in `frame`, which replay does not have yet, among the
    /// pages it changes.
    fn take_in(&mut self, page: PageId, frame: &PageRef<'_>) -> &mut Rebuilt {
        let rebuilt = self.rebuilt(page, frame.born());
        rebuilt.bytes.copy_from_slice(frame);
        rebuilt
    }

    /// The page `page` for a change of a commit: as replay has it, or as
    /// its chain gives it, or, for a change that makes the page (`fresh`),
    /// a page of zeros. `None` if the change needs a page and there is no
    /// image of it.
    fn page_to_change(
        &mut self,
        at: LogPlace,
        page: PageId,
        fresh: bool,
    ) -> Result<Option<&mut Rebuilt>, RecoveryError> {
        if fresh {
            // Nothing of what the id named before is in the new page.
            let rebuilt = self.rebuilt(page, CommitVersion::EMPTY);
            rebuilt.bytes.fill(0);
            rebuilt.oldest = None;
            return Ok(Some(rebuilt));
        }
        if !self.pages.contains_key(&page) {
            let Some(frame) = self.load(at, page)? else {
                return Ok(None);
            };
            return Ok(Some(self.take_in(page, &frame)));
        }
        Ok(self.pages.get_mut(&page))
    }

    /// A change of a commit to a page that there is no image of is
    /// dropped (K11).
    fn drop_change(&mut self, page: PageId) {
        self.dropped.insert(page);
        self.report.changes_dropped += 1;
    }

    fn tree_run(
        &mut self,
        at: LogPlace,
        version: CommitVersion,
        run: &DeltaRun<'_>,
    ) -> Result<(), RecoveryError> {
        let page = run.page();
        self.ids.wrote(page, version);
        if self.has(page, version) {
            return Ok(());
        }
        let size = self.size;
        let Some(rebuilt) = self.page_to_change(at, page, run.created().is_some())? else {
            self.drop_change(page);
            return Ok(());
        };
        run.apply(&mut rebuilt.bytes, size, FRAME_FORMAT)
            .map_err(|source| RecoveryError::Change { page, at, source })?;
        rebuilt.changed_by(version);
        self.replayed.insert(page);
        self.dropped.remove(&page);
        Ok(())
    }

    fn flat_run(
        &mut self,
        at: LogPlace,
        version: CommitVersion,
        run: &FlatRun<'_>,
    ) -> Result<(), RecoveryError> {
        let page = run.page();
        self.ids.wrote(page, version);
        if self.has(page, version) {
            return Ok(());
        }
        let capacity = self.size.image_capacity();
        let mut image = std::mem::take(&mut self.image);
        // A run that starts from an image needs no page before it.
        let Some(rebuilt) = self.page_to_change(at, page, run.image().is_some())? else {
            self.image = image;
            self.drop_change(page);
            return Ok(());
        };
        let applied = rebuilt.apply_flat(run, capacity, &mut image);
        if applied.is_ok() {
            rebuilt.changed_by(version);
        }
        self.image = image;
        applied.map_err(|source| RecoveryError::Change { page, at, source })?;
        self.replayed.insert(page);
        self.dropped.remove(&page);
        Ok(())
    }

    /// A commit freed `page`.
    fn freed(&mut self, page: PageId, version: CommitVersion) -> Result<(), RecoveryError> {
        self.ids.freed(page, version);
        if self.has(page, version) {
            return Ok(());
        }
        self.free_page(page, version)
    }

    /// Frees `page` at `version`, as the commit or the page step that
    /// freed it did: the free link goes to the page table before the pool
    /// is told, and nothing of the page is kept.
    fn free_page(&mut self, page: PageId, version: CommitVersion) -> Result<(), RecoveryError> {
        if let Some(rebuilt) = self.pages.remove(&page) {
            self.spare.push(rebuilt.bytes);
        }
        self.replayed.remove(&page);
        self.dropped.remove(&page);
        if self.newest_in_use(page).is_some() {
            self.table
                .push(page, Link::free(version))
                .map_err(|source| RecoveryError::Link { page, source })?;
            self.pool.free(page, version)?;
        }
        self.report.pages_freed += 1;
        Ok(())
    }

    /// Applies the change of one record to the counts of the value log.
    ///
    /// The value log reports the first pointer of a change that it could
    /// not count, and counts the releases first. So the releases and the
    /// acquired pointers of a record are handed over one after the other,
    /// which is the order they are counted in: a release that no count
    /// included, which recovery goes on from, then cannot hide a pointer
    /// to a value that is not there, which it does not go on from. A
    /// pointer that the record both releases and acquires is in neither
    /// part, as the value log counts it.
    fn count(&mut self, at: LogPlace, change: &ValueChange) -> Result<(), RecoveryError> {
        if change.released().is_empty() || change.acquired().is_empty() {
            return self.count_part(at, change);
        }
        let (released, acquired) = net(change);
        let mut part = ValueChange::new();
        released
            .into_iter()
            .for_each(|pointer| part.release(pointer));
        self.count_part(at, &part)?;
        let mut part = ValueChange::new();
        acquired
            .into_iter()
            .for_each(|pointer| part.acquire(pointer));
        self.count_part(at, &part)
    }

    /// A release that a count did not include leaves its file kept for
    /// good, which is the value log's own answer to a count that is wrong,
    /// and is reported. A pointer to a value that is not there is not
    /// something to go on from.
    fn count_part(&mut self, at: LogPlace, change: &ValueChange) -> Result<(), RecoveryError> {
        if change.is_empty() {
            return Ok(());
        }
        match self.counts.apply(change) {
            Ok(()) => Ok(()),
            Err(ValueStoreError::Miscounted {
                why: Miscount::CountIsZero | Miscount::BytesDisagree,
                ..
            }) => {
                self.report.miscounts += 1;
                Ok(())
            }
            Err(ValueStoreError::Miscounted { pointer, why }) => {
                Err(RecoveryError::ValueNotThere { pointer, at, why })
            }
            Err(other) => Err(other.into()),
        }
    }

    /// The record of a background change. `counted` says that it is at or
    /// after the place of the sample.
    ///
    /// From the place on, every record is looked at: for the counts, and
    /// for its page by LG28.
    ///
    /// Before the place the sample includes what the record did to the
    /// counts, and the chain that the checkpoint copied holds what it did
    /// to its page (K13). That is said of a page whose chain was copied.
    /// It is not so for a page of which replay has something that no
    /// copied chain holds, and the record of such a page is looked at for
    /// the page, by the same rule ([`Self::beyond_the_copy`]).
    fn change(
        &mut self,
        at: LogPlace,
        change: &Change<'_>,
        counted: bool,
    ) -> Result<(), RecoveryError> {
        if counted {
            let mut pointers = ValueChange::new();
            match change {
                Change::Value(step) => {
                    step.released()
                        .into_iter()
                        .for_each(|from| pointers.release(from));
                    pointers.acquire(step.acquired());
                }
                Change::Reclaim(reclaim) => reclaim
                    .released()
                    .for_each(|pointer| pointers.release(pointer)),
            }
            // What the record does to the counts is applied whatever
            // becomes of its change to the page (DU5).
            self.count(at, &pointers)?;
        } else if !self.beyond_the_copy(change) {
            return Ok(());
        }
        self.report.changes += 1;

        // LG28, 1: a page whose newest link is of a version above the last
        // group before the record has the change.
        let page = change.page();
        let before = self.last_group.unwrap_or(self.covers);
        if self
            .table
            .newest(page)
            .is_some_and(|link| link.version > before)
        {
            return Ok(());
        }
        match change {
            Change::Value(step) => self.value_step(at, step),
            Change::Reclaim(reclaim) => self.reclaim(at, reclaim),
        }
    }

    /// Whether replay has, of the page of `change`, what no chain that the
    /// checkpoint copied holds, so that the change is to be made although
    /// its record is before the place of the sample.
    ///
    /// - **The page holds the change of a replayed commit.** Replay made
    ///   that change because the chain lacked it (K3). A block that held
    ///   a background change made after the commit would hold the commit
    ///   too, under a link of its version or a later one. So the chain
    ///   lacks every background change that followed the commit, and the
    ///   commits that follow in the log were made on a page that had
    ///   them: a cell that a reclaiming step took out gave its slot to a
    ///   later commit.
    /// - **The page had a change dropped, and the record frees it.** The
    ///   checkpoint has lost the page (K10, K11), and no commit freed it:
    ///   the record is all that says the page is gone, and with which
    ///   version.
    ///
    /// Both come about only where the checkpoint lacks the chain of a
    /// page that was freed and trimmed away after commits it does not
    /// cover had changed it. A page that was in use when its chain was
    /// copied holds every background change before the place (K1).
    fn beyond_the_copy(&self, change: &Change<'_>) -> bool {
        let page = change.page();
        let frees = matches!(change, Change::Reclaim(reclaim) if reclaim.freed().is_some());
        self.replayed.contains(&page) || (frees && self.dropped.contains(&page))
    }

    /// A background change to a page that there is no image of. It is
    /// dropped like a commit's (K11) when the table has no link of the
    /// page. A page whose newest link is a free link is freed, and the
    /// change is of what it was before.
    fn no_image(&mut self, page: PageId) {
        if self.table.newest(page).is_none() {
            self.drop_change(page);
        }
    }

    /// Whether a page step is still to be made to `page`, which `due` tells
    /// from the bytes of the page as replay has it or as its chain gives
    /// it. A page that the step is due on is taken among the pages replay
    /// changes. A step on a page that there is no image of is not made.
    fn due_step(
        &mut self,
        at: LogPlace,
        page: PageId,
        due: impl Fn(&[u8]) -> Result<bool, FormatError>,
    ) -> Result<bool, RecoveryError> {
        let refused = |source| RecoveryError::Change { page, at, source };
        if let Some(rebuilt) = self.pages.get(&page) {
            return due(&rebuilt.bytes).map_err(refused);
        }
        let Some(frame) = self.load(at, page)? else {
            self.no_image(page);
            return Ok(false);
        };
        let is_due = due(&frame).map_err(refused)?;
        if is_due {
            self.take_in(page, &frame);
        }
        Ok(is_due)
    }

    /// LG28, 2: the placing or the move of a value is made exactly when
    /// the cell still holds what the record says it held.
    fn value_step(&mut self, at: LogPlace, step: &ValueMove) -> Result<(), RecoveryError> {
        let page = step.page();
        let size = self.size;
        if !self.due_step(at, page, |frame| page::is_due(frame, step))? {
            return Ok(());
        }
        let Some(rebuilt) = self.pages.get_mut(&page) else {
            return Ok(());
        };
        page::apply_step(&mut rebuilt.bytes, size, step)
            .map_err(|source| RecoveryError::Change { page, at, source })?;
        self.report.steps += 1;
        Ok(())
    }

    /// LG28, 2, for a reclaiming step: it is made exactly when what it
    /// takes out is there, or, for a step that frees its page with no run,
    /// when the page is not freed.
    fn reclaim(&mut self, at: LogPlace, reclaim: &Reclaim<'_>) -> Result<(), RecoveryError> {
        let page = reclaim.page();
        let (size, capacity) = (self.size, self.size.image_capacity());
        let refused = |source| RecoveryError::Change { page, at, source };
        let removed = reclaim.removed();
        let is_due = |frame: &[u8]| match &removed {
            Some(removed) => first_is_there(frame, removed),
            None => Ok(true),
        };

        let Some(version) = reclaim.freed() else {
            // Cells leave, and the page stays.
            if !self.due_step(at, page, is_due)? {
                return Ok(());
            }
            let (Some(rebuilt), Some(removed)) = (self.pages.get_mut(&page), removed) else {
                return Ok(());
            };
            match removed {
                Removed::Tree(run) => run.apply(&mut rebuilt.bytes, size, FRAME_FORMAT),
                Removed::Flat(run) => rebuilt.apply_flat(&run, capacity, &mut self.image),
            }
            .map_err(refused)?;
            self.report.steps += 1;
            return Ok(());
        };

        // The step frees the page, and the cells go with it.
        let due = match self.pages.get(&page) {
            Some(rebuilt) => Some(is_due(&rebuilt.bytes).map_err(refused)?),
            None => match self.load(at, page)? {
                Some(frame) => Some(is_due(&frame).map_err(refused)?),
                None => None,
            },
        };
        match due {
            Some(true) => {
                self.ids.freed(page, version);
                self.free_page(page, version)?;
                self.report.steps += 1;
            }
            // A page that had the step would be freed. This one is in use,
            // and the cells that the step took out of it are not there.
            Some(false) => return Err(RecoveryError::StepOnAnotherPage { page, at }),
            // The checkpoint has lost the page (K10), and this is the step
            // that freed it: what was dropped of the page for want of an
            // image is settled.
            None if self.table.newest(page).is_none() => {
                self.ids.freed(page, version);
                self.free_page(page, version)?;
            }
            // The page is freed already.
            None => {}
        }
        Ok(())
    }

    /// A checkpoint mark. The one that ends at the place of the sample is
    /// the mark of the checkpoint that carries the sample (LG29).
    fn mark(&mut self, end: LogPlace, mark: &CheckpointMark) -> Result<(), RecoveryError> {
        let Some(place) = self.place.filter(|&place| place == end) else {
            return Ok(());
        };
        if mark.covers() != self.covers {
            return Err(RecoveryError::WrongMark {
                place,
                mark: mark.covers(),
                covers: self.covers,
            });
        }
        self.marked = true;
        Ok(())
    }

    /// Writes the rebuilt pages that can be written to a segment, as base
    /// images, and keeps none of them: a page with a pending value cannot
    /// be, and stays (`page-delta.md` PD16).
    ///
    /// Each page goes the way a commit and write-back take it: it is
    /// installed in the pool, its block is appended, its link is pushed,
    /// and the pool is told that it is written. So the pool and the page
    /// table agree at every moment, and the pool can evict the page.
    fn spill(&mut self, manifest: &mut ManifestStore) -> Result<(), RecoveryError> {
        let mut ids: Vec<PageId> = self.pages.keys().copied().collect();
        ids.sort_unstable();
        for page in ids {
            let Some(rebuilt) = self.pages.get(&page) else {
                continue;
            };
            let not_a_page = |source| RecoveryError::Rebuilt { page, source };
            if has_pending(&rebuilt.bytes).map_err(not_a_page)? {
                continue;
            }
            self.image.clear();
            page::write_image(&rebuilt.bytes, &mut self.image).map_err(not_a_page)?;
            let Some(rebuilt) = self.pages.remove(&page) else {
                continue;
            };
            let frame = self.install(page, &rebuilt, Vec::new())?;
            let location = self.append(manifest, page)?;
            self.table
                .push(page, Link::base(rebuilt.born, location))
                .map_err(|source| RecoveryError::Link { page, source })?;
            self.pool.mark_written(&frame)?;
            self.spare.push(rebuilt.bytes);
            self.report.pages_spilled += 1;
        }
        self.spare.truncate(self.max_pages);
        self.spill_at = self.pages.len() + self.max_pages;
        self.since_spill = 0;
        Ok(())
    }

    /// Appends the image in `self.image` to the segment that replay
    /// writes, as the base image of `page`.
    fn append(
        &mut self,
        manifest: &mut ManifestStore,
        page: PageId,
    ) -> Result<BlockLocation, RecoveryError> {
        let segment_len = self.flush.config().segment_len;
        if let Some(mut full) = self.writer.take_if(|writer| writer.end() >= segment_len) {
            full.seal()?;
        }
        loop {
            let writer = match &mut self.writer {
                Some(writer) => writer,
                none => {
                    // A segment takes blocks once a manifest names it.
                    let writer = self.segments.create()?;
                    self.segments.commit(manifest)?;
                    none.insert(writer)
                }
            };
            match writer.append(page, BlockContent::Base, Compression::Lz4, &self.image) {
                Ok(location) => return Ok(location),
                Err(SegmentStoreError::SegmentFull { .. }) => {
                    if let Some(mut full) = self.writer.take() {
                        full.seal()?;
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Puts a rebuilt page into the pool as the current version of its
    /// page, which write-back owes to a segment, and returns it pinned.
    fn install(
        &self,
        page: PageId,
        rebuilt: &Rebuilt,
        attached: Vec<(ValueId, Arc<[u8]>)>,
    ) -> Result<PageRef<'s>, RecoveryError> {
        let pool = self.pool;
        // The version under the rebuilt one is in the pool if a load of
        // this recovery put it there and it was not evicted since.
        let under = self
            .newest_in_use(page)
            .and_then(|link| pool.get(page, link.version).ok());
        if let (Some(old), None) = (&under, rebuilt.oldest) {
            // Only background changes were replayed on the page: its
            // frame is replaced, and the page version stays the one it
            // was (PV4).
            let step = pool.page_step(page);
            let mut blank = pool.blank();
            blank.copy_from_slice(&rebuilt.bytes);
            for (id, bytes) in attached {
                blank.attach(id, bytes);
            }
            return step
                .replace(old, blank)
                .map_err(|source| RecoveryError::Step { page, source });
        }
        // A copy of the version under it, as a commit makes one, or a new
        // page where the pool holds none.
        let copied = under
            .as_ref()
            .and_then(|old| pool.make_private(OWNER, page, old.born()).ok());
        let copy = match copied {
            Some(copy) => copy,
            None => pool.new_private(OWNER, page)?,
        };
        {
            let mut latch = copy.write()?;
            latch.copy_from_slice(&rebuilt.bytes);
            for (id, bytes) in attached {
                latch.attach(id, bytes);
            }
        }
        Ok(pool.install(&copy, rebuilt.born)?)
    }

    /// What the end of the log says of the replay, once every item was
    /// read: the version recovery lands on, if the log and the table
    /// checkpoint are of one store.
    pub(super) fn ended(&mut self, end: &LogEnd) -> Result<CommitVersion, RecoveryError> {
        let landed = end.latest();
        let covers = self.covers;
        if landed < covers {
            return Err(RecoveryError::LogBehindCheckpoint { covers, landed });
        }
        if self.last_group.is_none() && landed > covers {
            return Err(RecoveryError::CommitsMissing {
                covers,
                found: landed,
            });
        }
        if let Some(place) = self.place.filter(|_| !self.marked) {
            return Err(RecoveryError::MarkMissing { place });
        }
        if let Some(&page) = self.dropped.first() {
            return Err(RecoveryError::ChangesWithoutPage {
                page,
                pages: self.dropped.len(),
            });
        }
        // The log goes first (K4): nothing is discarded by its version.
        let links = self.table.discard_after(landed);
        if links != 0 {
            return Err(RecoveryError::LinksAfterLanding { links, landed });
        }
        self.report.landed = landed;
        Ok(landed)
    }

    /// Puts every rebuilt page that is left into the pool, with the bytes
    /// of its pending values, which are read from `log`, and tells
    /// write-back which commits each of them holds.
    pub(super) fn install_rebuilt(&mut self, log: &LogStore) -> Result<(), RecoveryError> {
        let mut rebuilt: Vec<(PageId, Rebuilt)> = self.pages.drain().collect();
        rebuilt.sort_unstable_by_key(|(page, _)| *page);
        let mut record = Vec::new();
        for (page, rebuilt) in rebuilt {
            let cells = page::pending_cells(&rebuilt.bytes)
                .map_err(|source| RecoveryError::Rebuilt { page, source })?;
            let mut attached = Vec::with_capacity(cells.len());
            for cell in cells {
                // A reader is answered from the frame, never from the
                // log (FL7).
                let value = log.read_pending(&cell.pending, page, &mut record)?;
                let bytes = value.bytes().ok_or(RecoveryError::NamedValue { page })?;
                attached.push((cell.pending.id(), Arc::from(bytes.to_vec())));
                self.report.pending_values += 1;
            }
            self.install(page, &rebuilt, attached)?;
            self.flush.replayed_page(page, rebuilt.oldest);
            self.report.pages_rebuilt += 1;
        }
        Ok(())
    }

    /// Seals the segment that replay wrote, if it wrote one.
    pub(super) fn close(&mut self) -> Result<(), RecoveryError> {
        if let Some(mut writer) = self.writer.take() {
            writer.seal()?;
        }
        Ok(())
    }

    /// The counts of the value log, what replay found of the page ids, and
    /// what it did.
    pub(super) fn into_parts(self) -> (ValueRecovery, alloc::Recovery, RecoveryReport) {
        (self.counts, self.ids, self.report)
    }
}

/// Whether the page in `frame` has a value that is not in the value log
/// yet. Such a page is written to no segment (`page-delta.md` PD16). It is
/// asked before the image of a page is made, which is the dearer of the
/// two: replay asks again and again of pages whose values wait.
fn has_pending(frame: &[u8]) -> Result<bool, FormatError> {
    Ok(match page::kind_of(frame) {
        PageKind::Tree => TreePage::from_validated(frame, FRAME_FORMAT)?.has_pending_value(),
        PageKind::Flat => FlatPage::from_validated(frame, FRAME_FORMAT)?.has_pending_value(),
    })
}

/// Whether the page in `frame` still has what a reclaiming step took out
/// of it first. A step is made whole or not at all, and background work
/// adds no cell, so the first cell tells (LG28).
fn first_is_there(frame: &[u8], removed: &Removed<'_>) -> Result<bool, FormatError> {
    match (removed, page::kind_of(frame)) {
        (Removed::Tree(run), PageKind::Tree) => {
            let tree = TreePage::from_validated(frame, FRAME_FORMAT)?;
            Ok(run.deltas().next().is_some_and(|first| match first {
                Delta::Remove { slot } => tree.cell(slot).is_some(),
                _ => false,
            }))
        }
        (Removed::Flat(run), PageKind::Flat) => {
            let key = match run.changes().next() {
                Some(FlatChange::Remove { key } | FlatChange::RemoveRange { key, .. }) => key,
                _ => return Ok(false),
            };
            let flat = FlatPage::from_validated(frame, FRAME_FORMAT)?;
            if let Some(leaf) = flat.leaf() {
                return Ok(leaf.search(key).is_ok());
            }
            Ok(flat.branch().is_some_and(|branch| {
                (0..branch.len()).any(|slot| {
                    branch
                        .separator(slot)
                        .is_some_and(|separator| separator.cmp_bytes(key).is_eq())
                })
            }))
        }
        _ => Err(WalError::WrongPageKind.into()),
    }
}
