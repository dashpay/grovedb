//! Replay: the items of the log, in order, made to the pages that the
//! table checkpoint gives, to the counts of the value log and to what is
//! known of the page ids (`page-table.md` K3, K11, K13; `log-format.md`
//! LG15, LG16, LG28, LG29; `transactions.md` DU2, DU5).

use std::{
    collections::{BTreeSet, HashMap},
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
    values::{Miscount, ValueChange, ValueRecovery, ValueStoreError},
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

    /// Makes the page the flat page whose image is `image`.
    fn put_image(&mut self, image: &[u8]) {
        let (start, rest) = self.bytes.split_at_mut(image.len());
        start.copy_from_slice(image);
        rest.fill(0);
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
    segment_len: u64,

    /// The pages that replay has changed and no block holds.
    pages: HashMap<PageId, Rebuilt>,
    /// The pages that had a change dropped because the table has no image
    /// of them, and that no later item has freed or made again (K11).
    dropped: BTreeSet<PageId>,
    counts: ValueRecovery,
    ids: alloc::Recovery,
    /// The version of the last group that was read.
    last_group: Option<CommitVersion>,
    /// Whether the mark of the checkpoint's sample was found at its place.
    marked: bool,
    /// Whether a commit that the checkpoint does not cover was read.
    uncovered: bool,
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
            segment_len: flush.config().segment_len,
            pages: HashMap::new(),
            dropped: BTreeSet::new(),
            counts,
            ids: alloc::Recovery::new(),
            last_group: None,
            marked: false,
            uncovered: false,
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
            LogItem::Change(change) if counted => self.change(read.at, &change)?,
            LogItem::Change(_) => {}
            LogItem::Mark(mark) => self.mark(read.end, &mark)?,
        }
        if self.uncovered {
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
        self.uncovered = true;
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

    fn buffer(&mut self) -> Vec<u8> {
        self.spare.pop().unwrap_or_else(|| vec![0; self.size.get()])
    }

    /// Takes the page in `frame` among the pages replay changes.
    fn take_in(&mut self, page: PageId, frame: &PageRef<'_>) -> &mut Rebuilt {
        let mut bytes = self.buffer();
        bytes.copy_from_slice(frame);
        self.pages.entry(page).or_insert(Rebuilt {
            bytes,
            born: frame.born(),
            oldest: None,
        })
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
            if !self.pages.contains_key(&page) {
                let bytes = self.buffer();
                self.pages.insert(
                    page,
                    Rebuilt {
                        bytes,
                        born: CommitVersion::EMPTY,
                        oldest: None,
                    },
                );
            }
            let rebuilt = self.pages.get_mut(&page);
            if let Some(rebuilt) = rebuilt {
                // Nothing of what the id named before is in the new page.
                rebuilt.bytes.fill(0);
                rebuilt.oldest = None;
            }
            return Ok(self.pages.get_mut(&page));
        }
        if !self.pages.contains_key(&page) {
            let Some(frame) = self.load(at, page)? else {
                return Ok(None);
            };
            self.take_in(page, &frame);
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
        // A run that starts from an image needs no page before it.
        let whole = run.image().is_some();
        let mut image = std::mem::take(&mut self.image);
        image.clear();
        let applied = self.page_to_change(at, page, whole).map(|rebuilt| {
            rebuilt.map(|rebuilt| {
                let base = (!whole).then_some(&rebuilt.bytes[..]);
                run.apply(base, FRAME_FORMAT, capacity, &mut image)
                    .map(|()| {
                        rebuilt.put_image(&image);
                        rebuilt.changed_by(version);
                    })
            })
        });
        self.image = image;
        match applied? {
            Some(applied) => {
                applied.map_err(|source| RecoveryError::Change { page, at, source })?;
                self.dropped.remove(&page);
            }
            None => self.drop_change(page),
        }
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
    /// A release that a count did not include leaves its file kept for
    /// good, which is the value log's own answer to a count that is wrong,
    /// and is reported. A pointer to a value that is not there is not
    /// something to go on from.
    fn count(&mut self, at: LogPlace, change: &ValueChange) -> Result<(), RecoveryError> {
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

    /// The record of a background change, from the place of the sample on.
    fn change(&mut self, at: LogPlace, change: &Change<'_>) -> Result<(), RecoveryError> {
        self.report.changes += 1;
        let mut counted = ValueChange::new();
        match change {
            Change::Value(step) => {
                step.released()
                    .into_iter()
                    .for_each(|from| counted.release(from));
                counted.acquire(step.acquired());
            }
            Change::Reclaim(reclaim) => reclaim
                .released()
                .for_each(|pointer| counted.release(pointer)),
        }
        // What the record does to the counts is applied whatever becomes
        // of its change to the page (DU2).
        self.count(at, &counted)?;

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

    /// A background change to a page that there is no image of. It is
    /// dropped like a commit's (K11) when the table has no link of the
    /// page. A page whose newest link is a free link is freed, and the
    /// change is of what it was before.
    fn no_image(&mut self, page: PageId) {
        if self.table.newest(page).is_none() {
            self.drop_change(page);
        }
    }

    /// LG28, 2: the placing or the move of a value is made exactly when
    /// the cell still holds what the record says it held.
    fn value_step(&mut self, at: LogPlace, step: &ValueMove) -> Result<(), RecoveryError> {
        let page = step.page();
        let size = self.size;
        let refused = |source| RecoveryError::Change { page, at, source };
        if !self.pages.contains_key(&page) {
            let Some(frame) = self.load(at, page)? else {
                self.no_image(page);
                return Ok(());
            };
            if !page::is_due(&frame, step).map_err(refused)? {
                return Ok(());
            }
            self.take_in(page, &frame);
        }
        let Some(rebuilt) = self.pages.get_mut(&page) else {
            return Ok(());
        };
        if page::is_due(&rebuilt.bytes, step).map_err(refused)? {
            page::apply_step(&mut rebuilt.bytes, size, step).map_err(refused)?;
            self.report.steps += 1;
        }
        Ok(())
    }

    /// LG28, 2, for a reclaiming step: it is made exactly when what it
    /// takes out is there, or, for a step that frees its page with no run,
    /// when the page is not freed.
    fn reclaim(&mut self, at: LogPlace, reclaim: &Reclaim<'_>) -> Result<(), RecoveryError> {
        let page = reclaim.page();
        let size = self.size;
        let capacity = size.image_capacity();
        let refused = |source| RecoveryError::Change { page, at, source };
        if !self.pages.contains_key(&page) {
            let Some(frame) = self.load(at, page)? else {
                match reclaim.freed() {
                    // The page is freed already, or the checkpoint has
                    // lost it, and this is the step that freed it.
                    Some(version) if self.table.newest(page).is_none() => {
                        self.ids.freed(page, version);
                        self.dropped.remove(&page);
                    }
                    Some(_) => {}
                    None => self.no_image(page),
                }
                return Ok(());
            };
            let due = match reclaim.removed() {
                Some(removed) => first_is_there(&frame, &removed).map_err(refused)?,
                None => true,
            };
            if !due {
                return Ok(());
            }
            if reclaim.freed().is_none() {
                self.take_in(page, &frame);
            }
        }
        if let Some(version) = reclaim.freed() {
            // The cells go with the page.
            let due = match (self.pages.get(&page), reclaim.removed()) {
                (Some(rebuilt), Some(removed)) => {
                    first_is_there(&rebuilt.bytes, &removed).map_err(refused)?
                }
                _ => true,
            };
            if due {
                self.ids.freed(page, version);
                self.free_page(page, version)?;
                self.report.steps += 1;
            }
            return Ok(());
        }
        let (Some(rebuilt), Some(removed)) = (self.pages.get_mut(&page), reclaim.removed()) else {
            return Ok(());
        };
        if !first_is_there(&rebuilt.bytes, &removed).map_err(refused)? {
            return Ok(());
        }
        match removed {
            Removed::Tree(run) => run
                .apply(&mut rebuilt.bytes, size, FRAME_FORMAT)
                .map_err(refused)?,
            Removed::Flat(run) => {
                let mut image = std::mem::take(&mut self.image);
                image.clear();
                let applied = run.apply(Some(&rebuilt.bytes), FRAME_FORMAT, capacity, &mut image);
                if applied.is_ok() {
                    rebuilt.put_image(&image);
                }
                self.image = image;
                applied.map_err(refused)?;
            }
        }
        self.report.steps += 1;
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
        let segment_len = self.segment_len;
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
        &mut self,
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
        let mut ids: Vec<PageId> = self.pages.keys().copied().collect();
        ids.sort_unstable();
        let mut record = Vec::new();
        for page in ids {
            let Some(rebuilt) = self.pages.remove(&page) else {
                continue;
            };
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
/// yet. Such a page is written to no segment (`page-delta.md` PD16).
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
