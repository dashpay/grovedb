//! The view of a store that write-back works through, what a commit and a
//! page step tell it, and the pass that writes page versions to segments.

use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, Mutex, MutexGuard},
};

use lignin_format::{
    delta::DeltaRun,
    segment::{BlockContent, Compression, FRAME_LEN},
    wal::{Change, Entry, Record, Removed},
    CommitVersion, FormatError, FormatVersion, PageId, PageSize, SlotId,
};

use super::{
    config::{count, max_run_len},
    error::FlushError,
    page::{self, PageKind},
    state::{lock, Changes, Files, FlushState},
};
use crate::{
    alloc::Pins,
    cache::{BufferPool, CacheError, PageRef},
    load::FRAME_FORMAT,
    segment::{BlockLocation, ManifestStore, SegmentStore, SegmentStoreError, SegmentWriter},
    table::{Link, LinkError, LinkKind, LogPlace, PageTable, TableCheckpoints},
    values::{ValueStore, ValueWriter},
    wal::{Appended, LogError, LogStore},
};

/// A run shorter than this is stored as it is: a few hashes do not
/// compress, and the attempt costs more than it could save.
const COMPRESS_FROM: usize = 512;

/// What write-back asks of whoever publishes commits and keeps the version
/// pins and the operation guards (`transactions.md` CM3, PN5, PN6).
pub trait Published: Pins + Sync {
    /// The latest published version, *L*: every commit up to it has
    /// installed its page versions in the buffer pool (CM3 steps 7 and 8).
    fn latest(&self) -> CommitVersion;

    /// Runs `work` under an operation guard (PN5): the blocks that the
    /// page table names when `work` reads it stay readable until `work`
    /// returns, whatever a cleaner does meanwhile.
    fn guarded(&self, work: &mut dyn FnMut());
}

/// Why a regrouping of a page's values failed: the error of whoever
/// implements [`PageValues`].
pub type RegroupError = Box<dyn std::error::Error + Send + Sync>;

/// What write-back asks of the layer that knows what the cells of a tree
/// page are to each other: the order in which a scan reads them, and the
/// regrouping of a page's values in that order (`lignin-tree`,
/// out-of-line values). The store knows no tree, so it is handed this.
pub trait PageValues: Sync {
    /// Adds to `out` the slots of the cells of the tree page in `frame`,
    /// in the order a scan reads their values. Write-back appends the
    /// pending values of a page in that order, so that the values of a new
    /// page lie in the value log as a scan will read them. A cell that is
    /// not listed comes after those that are, in slot order.
    fn scan_order(&self, frame: &[u8], out: &mut Vec<SlotId>);

    /// Regroups the values of the current version of the tree page `page`,
    /// if they lie badly enough to be worth it: appends each of them again
    /// to `to`, back to back in scan order, calls `cover`, which makes the
    /// copies durable and has the manifest cover them, and then gives each
    /// cell the pointer to its copy, each in a page step whose record goes
    /// to the log through [`WriteBack::append_change`]. Returns how many
    /// values it moved. A page that a write transaction holds a private
    /// copy of is left as it is.
    ///
    /// Write-back calls this, under an operation guard, for a page it is
    /// about to write a new base image of, after it has placed the page's
    /// pending values, and then writes the image with the new pointers. It
    /// gives `to` no other value meanwhile.
    fn regroup(
        &self,
        page: PageId,
        to: &mut ValueWriter,
        cover: &mut dyn FnMut(&mut ValueWriter) -> Result<(), FlushError>,
    ) -> Result<u64, RegroupError>;
}

/// The parts of one open store that write-back works on.
#[derive(Clone, Copy)]
pub struct Stores<'s> {
    /// The buffer pool, which holds what is to be written back.
    pub pool: &'s BufferPool,
    /// The page table, which takes the link of every block.
    pub table: &'s PageTable,
    /// The segments, which take the blocks.
    pub segments: &'s SegmentStore,
    /// The value log, which takes the pending values.
    pub values: &'s ValueStore,
    /// The log.
    pub log: &'s LogStore,
    /// The table checkpoints.
    pub checkpoints: &'s TableCheckpoints,
    /// The manifest, behind the lock that makes its replacements one at a
    /// time (level 3 of `transactions.md` LT7).
    pub manifest: &'s Mutex<ManifestStore>,
    /// The published version, the pins and the guards.
    pub versions: &'s dyn Published,
    /// The scan order of tree pages and the regrouping of their values, if
    /// the layer above has them. With `None`, pending values are appended
    /// in slot order and no page is regrouped.
    pub page_values: Option<&'s dyn PageValues>,
}

impl std::fmt::Debug for Stores<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stores").finish_non_exhaustive()
    }
}

/// What one pass did ([`WriteBack::pass`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pass {
    /// Page versions whose block was written and linked.
    pub written: usize,
    /// Pending values that were given their pointer.
    pub placed: usize,
    /// Page versions that were kept in the pool for their readers instead
    /// of being written.
    pub held: usize,
    /// Page versions that still wait: the pool lists them as owed.
    pub waiting: usize,
}

impl Pass {
    /// Whether the pass moved anything on.
    pub fn progressed(&self) -> bool {
        self.written + self.placed + self.held > 0
    }
}

/// Write-back for one open store: the stores and the state, as one handle
/// that can be copied to every thread. See the
/// [module documentation](super).
#[derive(Debug, Clone, Copy)]
pub struct WriteBack<'s> {
    pub(super) stores: Stores<'s>,
    pub(super) state: &'s FlushState,
    pub(super) size: PageSize,
}

fn is_tree(frame: &[u8]) -> bool {
    page::kind_of(frame) == PageKind::Tree
}

/// What became of one page version that a pass looked at.
enum Outcome {
    /// Its block is linked.
    Written(Link),
    /// The page table took no link: a commit freed the page meanwhile.
    Refused,
}

impl<'s> WriteBack<'s> {
    /// Write-back for `stores`, with `state` as what it keeps between
    /// calls. The page size is the manifest's, and must be the size of the
    /// pool's frames ([`FlushError::FrameSize`]).
    pub fn new(stores: Stores<'s>, state: &'s FlushState) -> Result<Self, FlushError> {
        let size = lock(stores.manifest).manifest()?.page_size();
        if stores.pool.frame_len() != size.get() {
            return Err(FlushError::FrameSize {
                page_size: size.get(),
                frame_len: stores.pool.frame_len(),
            });
        }
        Ok(Self {
            stores,
            state,
            size,
        })
    }

    /// The state this handle works on.
    pub fn state(&self) -> &'s FlushState {
        self.state
    }

    /// The manifest, settled: if a replacement left it in doubt, it is
    /// opened again first, and every store is told of the manifest that is
    /// in force.
    pub(super) fn manifest(&self) -> Result<MutexGuard<'s, ManifestStore>, FlushError> {
        let mut manifest = lock(self.stores.manifest);
        if manifest.manifest().is_err() {
            let reopened = ManifestStore::open(manifest.vfs().clone(), manifest.dir())?;
            *manifest = reopened;
            let Stores {
                segments,
                values,
                log,
                checkpoints,
                ..
            } = self.stores;
            segments.manifest_replaced(&manifest)?;
            values.manifest_replaced(&manifest)?;
            log.manifest_replaced(&manifest)?;
            checkpoints.manifest_replaced(&manifest)?;
        }
        Ok(manifest)
    }

    /// Tells write-back of a commit: its version, the place its group
    /// begins at in the log, and the group's bytes as they were appended.
    ///
    /// The committer calls this once [`LogStore::append_group`] has
    /// returned `Ok` and **before it installs the first page** (CM3, between
    /// steps 6 and 7). Write-back keeps the records of each tree page the
    /// commit changed until a block holds them, and the place for as long
    /// as no table checkpoint covers the commit.
    ///
    /// A commit that is not told of loses nothing: its pages are written
    /// as base images, and no log file is dropped behind a commit whose
    /// place write-back does not know.
    pub fn committed(
        &self,
        version: CommitVersion,
        at: LogPlace,
        group: &[u8],
    ) -> Result<(), FlushError> {
        let refused = |offset: usize| {
            move |source: FormatError| FlushError::Log(LogError::Refused { offset, source })
        };
        let mut parts = Vec::new();
        let mut offset = 0;
        while offset < group.len() {
            let (record, len) = Record::decode(&group[offset..], FormatVersion::CURRENT)
                .map_err(refused(offset))?;
            if let Record::Part(part) = record {
                parts.push(part);
            }
            offset += len;
        }

        let mut pages = 0;
        let mut kept = lock(&self.state.kept);
        kept.group_at(version.get(), at);
        for entry in parts.iter().flat_map(|part| part.entries()) {
            match entry {
                Entry::Tree(run) if run.created().is_none() => {
                    let records = &run.as_bytes()[DeltaRun::HEADER_LEN..];
                    kept.committed(run.page(), version.get(), records);
                    pages += 1;
                }
                Entry::Tree(run) => {
                    kept.forget(run.page());
                    pages += 1;
                }
                Entry::Flat(run) => {
                    if run.image().is_some() {
                        kept.forget(run.page());
                    }
                    pages += 1;
                }
                Entry::Freed(page) => kept.forget(page),
                Entry::Released(_) | Entry::Acquired(_) => {}
            }
        }
        drop(kept);
        self.state.owed.fetch_add(pages, Ordering::Relaxed);
        self.state
            .logged
            .fetch_add(group.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Appends the record of a background change to the log, and keeps
    /// what it does to a tree page for the block that will hold it. This is
    /// [`LogStore::append_change`] for every page step: of write-back
    /// itself, of the value log's cleaner, of a regrouping, and of whoever
    /// removes what a dropped subtree left. It has the shape of the tree
    /// layer's `StepLog::append`.
    ///
    /// The step calls this under its own lock on the page, after its
    /// checks, and installs its change only when this has returned `Ok`,
    /// as for the log itself. The change is to the current version of the
    /// page the record names.
    ///
    /// A page step whose record does not come this way is still written
    /// back correctly: its page is then written as a base image.
    pub fn append_change(&self, record: &[u8]) -> Result<Appended, LogError> {
        let appended = self
            .stores
            .log
            .append_change(record)
            .inspect_err(|error| self.append_failed(error))?;
        if let Ok((Record::Change(change), _)) = Record::decode(record, FormatVersion::CURRENT) {
            // Under the step's lock the current version cannot change.
            let born = |page| {
                let current = self.stores.pool.get(page, CommitVersion::MAX).ok();
                current.map(|frame| frame.born().get())
            };
            match change {
                Change::Value(step) => {
                    let delta = step.delta().zip(born(step.page()));
                    if let Some((delta, born)) = delta {
                        let mut records = Vec::with_capacity(delta.encoded_len());
                        if delta.encode_into(&mut records).is_ok() {
                            lock(&self.state.kept).stepped(step.page(), born, &records);
                        }
                    }
                }
                Change::Reclaim(reclaim) => match (reclaim.freed(), reclaim.removed()) {
                    (Some(_), _) => lock(&self.state.kept).forget(reclaim.page()),
                    (None, Some(Removed::Tree(run))) => {
                        if let Some(born) = born(reclaim.page()) {
                            let records = &run.as_bytes()[DeltaRun::HEADER_LEN..];
                            lock(&self.state.kept).stepped(reclaim.page(), born, records);
                        }
                    }
                    (None, _) => {}
                },
            }
        }
        self.state
            .logged
            .fetch_add(record.len() as u64, Ordering::Relaxed);
        Ok(appended)
    }

    /// Notes that an append to the log failed. Until the log has been
    /// rolled it takes no item (`transactions.md` CM9 item 5), and rolling
    /// it is write-back's ([`Self::roll_log`]).
    pub(super) fn append_failed(&self, error: &LogError) {
        let cut_needed = matches!(
            error,
            LogError::TakenBack { .. } | LogError::NotAppended { .. } | LogError::NoActiveFile
        );
        if cut_needed {
            self.state.roll_wanted.store(true, Ordering::Release);
            self.state.wake();
        }
    }

    /// The page versions the pool lists as owed, by page, each page's
    /// versions oldest first.
    pub(super) fn owed(&self) -> BTreeMap<PageId, Vec<CommitVersion>> {
        let pool = self.stores.pool;
        let mut versions = Vec::new();
        pool.unwritten_versions(&mut versions);
        let mut owed: BTreeMap<PageId, Vec<CommitVersion>> = BTreeMap::new();
        let mut pages = Vec::new();
        for version in versions {
            pages.clear();
            pool.unwritten_pages(version, &mut pages);
            for &page in &pages {
                owed.entry(page).or_default().push(version);
            }
        }
        owed
    }

    /// One pass of write-back: every pending value that can be placed is
    /// placed, and every page version the pool lists as owed is written to
    /// a segment and linked, or kept for its readers, or left for a later
    /// pass.
    ///
    /// One pass runs at a time, and a second call waits for the first. A
    /// pass waits for no commit, and holds a page's step lock only to look
    /// at the page and to place one value.
    ///
    /// If it fails, what it had written and linked stands, and it can be
    /// called again.
    pub fn pass(&self) -> Result<Pass, FlushError> {
        let mut files = lock(&self.state.files);
        let mut pass = Pass::default();
        let result = self.pass_with(&mut files, &mut pass);
        pass.waiting = self.owed().values().map(Vec::len).sum();
        self.state.owed.store(pass.waiting, Ordering::Relaxed);
        drop(files);

        let mut room = lock(&self.state.room);
        room.passes += 1;
        room.progress = pass.progressed();
        drop(room);
        self.state.room_changed.notify_all();
        result.map(|()| pass)
    }

    fn pass_with(&self, files: &mut Files, pass: &mut Pass) -> Result<(), FlushError> {
        let owed = self.owed();
        let oldest_pin = self.stores.versions.oldest_pin();
        pass.placed = self.place_pending(files, &owed)?;
        for (&page, versions) in &owed {
            self.write_page(files, page, versions, oldest_pin, pass)?;
        }
        self.trim(files);
        Ok(())
    }

    /// Writes what is owed of one page, oldest version first.
    fn write_page(
        &self,
        files: &mut Files,
        page: PageId,
        owed: &[CommitVersion],
        oldest_pin: CommitVersion,
        pass: &mut Pass,
    ) -> Result<(), FlushError> {
        // A regrouping puts other frames in place of the current
        // version's, so the page is then looked at again, for that version.
        match self.write_owed(files, page, owed, oldest_pin, pass, false)? {
            None => Ok(()),
            Some(born) => self
                .write_owed(files, page, &[born], oldest_pin, pass, true)
                .map(drop),
        }
    }

    /// One look at a page: its owed versions are written, oldest first.
    /// Returns the version to look at again if the values of the page's
    /// current version were regrouped on the way.
    fn write_owed(
        &self,
        files: &mut Files,
        page: PageId,
        owed: &[CommitVersion],
        oldest_pin: CommitVersion,
        pass: &mut Pass,
        regrouped: bool,
    ) -> Result<Option<CommitVersion>, FlushError> {
        let Stores { pool, table, .. } = self.stores;
        // The frames and what is kept of the page are taken in one page
        // step, so that the records are those that lead to the frames.
        let (frames, current, changes, mut gap) = {
            let step = pool.page_step(page);
            let frames: Vec<PageRef<'s>> = owed
                .iter()
                .filter_map(|&born| pool.get_version(page, born))
                .collect();
            let current = frames.last().is_some_and(|frame| step.is_current(frame));
            let kept = lock(&self.state.kept);
            (frames, current, kept.changes(page), kept.folded(page))
        };
        files.links.clear();
        table.chain(page, &mut files.links);
        let mut links = std::mem::take(&mut files.links);

        let mut result = Ok(None);
        for (index, frame) in frames.iter().enumerate() {
            let born = frame.born();
            if let Some(freed) = links.last().filter(|link| link.kind == LinkKind::Free)
                && born < freed.version
            {
                // Nothing older than a free link is written back (T18).
                continue;
            }
            let is_current = current && index + 1 == frames.len();
            let stripped;
            let mut frame = frame;
            let pending = match page::pending_cells(frame) {
                Ok(cells) => !cells.is_empty(),
                Err(source) => {
                    result = Err(FlushError::Page { page, source });
                    break;
                }
            };
            let attached = frame.attachments().next().is_some();
            if is_current {
                if pending {
                    break;
                }
                if attached {
                    match self.strip(page, frame) {
                        Ok(Some(without)) => {
                            stripped = without;
                            frame = &stripped;
                        }
                        Ok(None) => break,
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
            } else {
                let successor = frames.get(index + 1).map(PageRef::born);
                let unneeded = successor.is_some_and(|next| oldest_pin >= next);
                if pending || attached || unneeded {
                    match pool.hold_for_readers(frame) {
                        Ok(()) => {
                            lock(&self.state.kept).fold(page, born.get());
                            gap.push(born.get());
                            pass.held += 1;
                            count(&self.state.counters.held_for_readers, 1);
                        }
                        Err(CacheError::NotUnwritten { .. }) => {}
                        Err(CacheError::StillCurrent { .. }) => break,
                        Err(error) => {
                            result = Err(error.into());
                            break;
                        }
                    }
                    continue;
                }
            }
            let written = self.plan(files, page, frame, &links, &changes, &gap).and_then(|as_run| {
                // A page that is about to get a new base image has its
                // values regrouped first, and is then looked at again.
                let regroup = !as_run && is_current && !regrouped && is_tree(frame);
                if regroup && self.regroup(files, page)? > 0 {
                    return Ok(None);
                }
                self.store(files, page, frame, as_run).map(Some)
            });
            match written {
                Ok(Some(Outcome::Written(link))) => {
                    links.push(link);
                    gap.clear();
                    let mut kept = lock(&self.state.kept);
                    kept.unfold(page, born.get());
                    kept.wrote(page, &changes, born.get());
                    pass.written += 1;
                }
                Ok(Some(Outcome::Refused)) => {
                    lock(&self.state.kept).forget(page);
                    break;
                }
                Ok(None) => {
                    result = Ok(Some(born));
                    break;
                }
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        files.links = links;
        result
    }

    /// Has the values of the current version of `page` regrouped, if the
    /// store was given what does that. Returns how many values were moved.
    fn regroup(&self, files: &mut Files, page: PageId) -> Result<u64, FlushError> {
        let Stores {
            values,
            versions,
            page_values,
            ..
        } = self.stores;
        let Some(page_values) = page_values else {
            return Ok(0);
        };
        self.value_writer(files)?;
        let writer = files.values.as_mut().expect("a writer was just made");
        let mut cover = |writer: &mut ValueWriter| -> Result<(), FlushError> {
            writer.sync()?;
            values.commit(&mut *self.manifest()?)?;
            Ok(())
        };
        let mut moved = Ok(0);
        versions.guarded(&mut || moved = page_values.regroup(page, writer, &mut cover));
        match moved {
            Ok(moved) => {
                count(&self.state.counters.values_regrouped, moved);
                Ok(moved)
            }
            Err(source) => {
                // The copies are given up, and the next value goes to
                // another file: a sync of this one may have failed.
                files.values = None;
                Err(FlushError::Regroup { page, source })
            }
        }
    }

    /// Makes sure that write-back has a value-log file that takes values.
    pub(super) fn value_writer(&self, files: &mut Files) -> Result<(), FlushError> {
        let values = self.stores.values;
        if files.values.as_ref().is_none_or(|writer| writer.is_full()) {
            if let Some(writer) = files.values.take() {
                writer.finish()?;
            }
            let writer = values.create()?;
            values.commit(&mut *self.manifest()?)?;
            files.values = Some(writer);
        }
        Ok(())
    }

    /// Puts a frame without its attached values in place of `frame`, the
    /// current version of `page`, which has no pending value left that they
    /// could belong to. What the page says does not change, so the step has
    /// no record (`transactions.md` PV4). Returns the new frame, or `None`
    /// if the step has to wait for a write transaction that holds a private
    /// copy of the page.
    fn strip(&self, page: PageId, frame: &PageRef<'s>) -> Result<Option<PageRef<'s>>, FlushError> {
        let pool = self.stores.pool;
        let step = pool.page_step(page);
        if !step.is_current(frame) || step.private_copies() > 0 {
            count(&self.state.counters.steps_put_off, 1);
            return Ok(None);
        }
        let mut blank = pool.blank();
        blank.copy_from(frame);
        let ids: Vec<_> = frame.attachments().map(|(id, _)| id).collect();
        for id in ids {
            blank.detach(id);
        }
        step.replace(frame, blank)
            .map(Some)
            .map_err(|source| FlushError::StepRefused { page, source })
    }

    /// Puts the image of the page in `frame` into `files.image`, and says
    /// whether the page is to be written as the run that is then in
    /// `files.run`, behind the chain `links`, or as that image.
    fn plan(
        &self,
        files: &mut Files,
        page: PageId,
        frame: &PageRef<'s>,
        links: &[Link],
        changes: &[Changes],
        gap: &[u64],
    ) -> Result<bool, FlushError> {
        let born = frame.born();
        files.image.clear();
        let pending =
            page::write_image(frame, &mut files.image).map_err(FlushError::page(page))?;
        if pending {
            return Err(FlushError::PendingValue { page, born });
        }
        Ok(is_tree(frame) && self.run_for(files, page, born, links, changes, gap)?)
    }

    /// Writes the page version in `frame` to a segment, as the run in
    /// `files.run` or as the image in `files.image`, pushes its link and
    /// tells the pool.
    fn store(
        &self,
        files: &mut Files,
        page: PageId,
        frame: &PageRef<'s>,
        as_run: bool,
    ) -> Result<Outcome, FlushError> {
        let Stores { pool, table, .. } = self.stores;
        let born = frame.born();
        let (content, location) = if as_run {
            let compression = match files.run.len() {
                len if len < COMPRESS_FROM => Compression::Raw,
                _ => Compression::Lz4,
            };
            let run = std::mem::take(&mut files.run);
            let location = self.append(files, page, BlockContent::Delta, compression, &run);
            files.run = run;
            (BlockContent::Delta, location?)
        } else {
            let image = std::mem::take(&mut files.image);
            let location = self.append(files, page, BlockContent::Base, Compression::Lz4, &image);
            files.image = image;
            (BlockContent::Base, location?)
        };

        let counters = &self.state.counters;
        let link = match content {
            BlockContent::Delta => Link::delta(born, location),
            BlockContent::Base => Link::base(born, location),
        };
        match table.push(page, link) {
            Ok(()) => {}
            Err(LinkError::OutOfOrder { .. }) => {
                count(&counters.links_refused, 1);
                return Ok(Outcome::Refused);
            }
            Err(source) => {
                return Err(FlushError::Link {
                    page,
                    location,
                    source,
                });
            }
        }
        count(&counters.stored_bytes, u64::from(location.len));
        match content {
            BlockContent::Delta => {
                count(&counters.delta_blocks, 1);
                count(&counters.delta_bytes, files.run.len() as u64);
            }
            BlockContent::Base => {
                count(&counters.base_blocks, 1);
                count(&counters.base_bytes, files.image.len() as u64);
                files.untrimmed.insert(page, born);
            }
        }
        match pool.mark_written(frame) {
            // A page step replaced the frame while its block was written:
            // the block holds the page as it was before the step, and the
            // pool still lists the page, for the frame the step made.
            Ok(()) | Err(CacheError::Replaced { .. } | CacheError::NotUnwritten { .. }) => {
                Ok(Outcome::Written(link))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Puts into `files.run` the run that brings the page from the newest
    /// link of `links` to the page in the frame whose image is
    /// `files.image`, and says whether the page may be written as that run.
    ///
    /// It may when the chain has a base image for it to follow, when
    /// write-back has kept the records of every version between that link
    /// and the frame, when the chain stays within the thresholds, and when
    /// the run, applied to the page as the chain gives it, gives the page
    /// in the frame.
    fn run_for(
        &self,
        files: &mut Files,
        page: PageId,
        born: CommitVersion,
        links: &[Link],
        changes: &[Changes],
        gap: &[u64],
    ) -> Result<bool, FlushError> {
        let config = &self.state.config;
        let Some(newest) = links.last() else {
            return Ok(false);
        };
        let through = newest.version.get();
        if newest.kind == LinkKind::Free || through > born.get() || config.max_chain_deltas == 0 {
            return Ok(false);
        }
        // Every version of the page behind the newest link whose changes no
        // block holds has its records here, and nothing else has.
        if gap.iter().any(|&folded| folded <= through) {
            return Ok(false);
        }
        files.run.clear();
        files.run.extend_from_slice(&page.encode());
        for &version in gap.iter().chain([&born.get()]) {
            let Some(kept) = changes.iter().find(|kept| kept.born == version) else {
                return Ok(false);
            };
            files.run.extend_from_slice(&kept.records);
        }
        if files.run.len() == DeltaRun::HEADER_LEN
            || files.run.len() > max_run_len(self.size)
            || files.run.len() >= files.image.len()
        {
            return Ok(false);
        }

        let base = links
            .iter()
            .rposition(|link| !matches!(link.kind, LinkKind::Delta(_)))
            .unwrap_or(0);
        let deltas = links.len() - base - 1;
        let bytes: usize = links[base + 1..]
            .iter()
            .filter_map(Link::location)
            .map(|location| location.len as usize)
            .sum();
        if deltas + 1 > config.max_chain_deltas
            || bytes + FRAME_LEN + files.run.len() > config.max_chain_bytes
        {
            count(&self.state.counters.consolidations, 1);
            return Ok(false);
        }

        // The page as the chain gives it. A frame of the version of the
        // newest link is that page, unless the frame is itself owed, which
        // is when the link and the frame are of one version.
        files.scratch.clear();
        files.scratch.resize(self.size.get(), 0);
        let in_pool = (through < born.get())
            .then(|| self.stores.pool.get_version(page, newest.version))
            .flatten();
        match in_pool {
            Some(before) => files.scratch.copy_from_slice(&before),
            None => {
                if self.read_chain(files, page, newest)?.is_none() {
                    return Ok(false);
                }
            }
        }
        let applies = DeltaRun::decode(&files.run, FRAME_FORMAT)
            .and_then(|run| run.apply(&mut files.scratch, self.size, FRAME_FORMAT))
            .and_then(|()| {
                files.other.clear();
                page::write_image(&files.scratch, &mut files.other)
            });
        if !matches!(applies, Ok(false)) || files.other != files.image {
            count(&self.state.counters.runs_refused, 1);
            return Ok(false);
        }
        Ok(true)
    }

    /// Builds in `files.scratch` the page as its chain gives it up to the
    /// link `through`, from the blocks. `None` if the chain no longer has
    /// that link, or no base image under it.
    pub(super) fn read_chain(
        &self,
        files: &mut Files,
        page: PageId,
        through: &Link,
    ) -> Result<Option<PageKind>, FlushError> {
        let Stores {
            table,
            segments,
            versions,
            ..
        } = self.stores;
        let size = self.size;
        let Files {
            scratch,
            chain,
            links,
            ..
        } = files;
        let mut result = Ok(None);
        versions.guarded(&mut || {
            links.clear();
            table.chain(page, links);
            let found = links.iter().rposition(|link| link == through);
            let Some(readable) = found.and_then(|at| page::readable(links, at)) else {
                return;
            };
            scratch.clear();
            scratch.resize(size.get(), 0);
            result = page::materialize(segments, size, page, readable, scratch, chain).map(Some);
        });
        result
    }

    /// Appends one block to the segment write-back is filling, and starts
    /// a segment first if it has none or the one it has is full.
    pub(super) fn append(
        &self,
        files: &mut Files,
        page: PageId,
        content: BlockContent,
        compression: Compression,
        payload: &[u8],
    ) -> Result<BlockLocation, FlushError> {
        let segments = self.stores.segments;
        let full = files
            .segment
            .as_ref()
            .is_some_and(|writer| writer.end() >= self.state.config.segment_len);
        if full && let Some(mut writer) = files.segment.take() {
            match writer.seal() {
                Ok(_) => {}
                Err(error) => {
                    self.segment_failed(files, writer);
                    return Err(error.into());
                }
            }
        }
        if files.segment.is_none() {
            let writer = segments.create()?;
            segments.commit(&mut *self.manifest()?)?;
            files.segment = Some(writer);
        }
        let writer = files.segment.as_mut().expect("a segment was just made");
        match writer.append(page, content, compression, payload) {
            Ok(location) => Ok(location),
            Err(SegmentStoreError::SegmentFull { .. }) => {
                let mut writer = files.segment.take().expect("the segment that is full");
                if let Err(error) = writer.seal() {
                    self.segment_failed(files, writer);
                    return Err(error.into());
                }
                self.append(files, page, content, compression, payload)
            }
            Err(error) => {
                // What was appended stays readable, and a sync of the
                // store still makes it durable. The next block goes to
                // another segment.
                let writer = files.segment.take().expect("the segment that failed");
                self.segment_failed(files, writer);
                Err(error.into())
            }
        }
    }

    /// Ends the use of a segment that an append or a seal failed. What was
    /// appended to it is synced. If that sync fails, the writer is kept and
    /// not dropped: dropping it would cut the segment at what was synced,
    /// and the blocks behind that, which chains point to, could no longer
    /// be read.
    pub(super) fn segment_failed(&self, files: &mut Files, mut writer: SegmentWriter) {
        if writer.sync().is_err() {
            self.state.segments_failed.store(true, Ordering::Release);
            files.failed.push(writer);
        }
    }

    /// Removes the links that no reader needs any more from the chains of
    /// the pages that got a base image, once the oldest pin has reached
    /// that image's version (`page-table.md` T11).
    fn trim(&self, files: &mut Files) {
        let oldest = self.stores.versions.oldest_pin();
        let mut removed = Vec::new();
        files.untrimmed.retain(|&page, version| {
            if *version > oldest {
                return true;
            }
            self.stores.table.trim(page, oldest, &mut removed);
            false
        });
        if !removed.is_empty() {
            let mut kept = lock(&self.state.kept);
            removed.iter().for_each(|link| kept.died(link));
        }
    }
}
