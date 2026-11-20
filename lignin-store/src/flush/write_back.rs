//! The view of a store that write-back works through, what a commit and a
//! page step tell it, and the pass that writes page versions to segments.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{atomic::Ordering, Mutex, MutexGuard},
};

use lignin_format::{
    delta::DeltaRun,
    segment::{BlockContent, Compression, FRAME_LEN},
    wal::{Change, Entry, FlatRun, Record, Removed},
    CommitVersion, FormatVersion, PageId, PageSize, SlotId,
};

use super::{
    config::{count, max_run_len, Counters},
    error::FlushError,
    page::{self, PageKind},
    state::{lock, Changes, Files, FlushState, Untrimmed},
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
    /// Pages that could not be written because a value of theirs could not
    /// be placed: a write transaction holds a private copy of the page.
    pub blocked: usize,
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

/// How a page version is written to a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// As a run of deltas behind the chain.
    Run,
    /// As a base image.
    Image,
    /// As a base image, because a run would take the chain past a
    /// threshold.
    Consolidation,
}

/// What a pass carries from page to page.
pub(super) struct Round {
    pass: Pass,
    oldest_pin: CommitVersion,
    /// Whether the values of a page are still regrouped in this pass.
    regroups: bool,
    /// The first error that did not end the pass.
    failed: Option<FlushError>,
    /// The pages that their bytes kept from being written.
    failing: BTreeSet<PageId>,
}

impl Round {
    /// Notes an error that does not end the pass.
    pub(super) fn fail(&mut self, error: FlushError) {
        if let FlushError::Page { page, .. } | FlushError::PendingValue { page, .. } = error {
            self.failing.insert(page);
        }
        self.failed.get_or_insert(error);
    }
}

/// The records of one commit group, by kind, for [`FlushStats`].
///
/// [`FlushStats`]: super::FlushStats
#[derive(Debug, Default)]
struct Told {
    node_records: usize,
    node_bytes: usize,
    other_records: usize,
    other_bytes: usize,
}

impl Told {
    fn entry(&mut self, entry: &Entry<'_>) {
        match entry {
            Entry::Tree(run) => {
                self.node_records += run.deltas().len();
                self.node_bytes += run.encoded_len() - DeltaRun::HEADER_LEN;
            }
            Entry::Flat(run) => {
                self.other_records += run.changes().len();
                self.other_bytes += run.as_bytes().len() - FlatRun::HEADER_LEN;
            }
            Entry::Freed(_) | Entry::Released(_) | Entry::Acquired(_) => {}
        }
    }

    fn count(&self, counters: &Counters) {
        count(&counters.groups, 1);
        count(&counters.node_records, self.node_records as u64);
        count(&counters.node_record_bytes, self.node_bytes as u64);
        count(&counters.other_records, self.other_records as u64);
        count(&counters.other_record_bytes, self.other_bytes as u64);
    }
}

/// What a test does after a look.
#[cfg(test)]
pub(super) type AfterTheLook = Box<dyn FnOnce(PageId)>;

#[cfg(test)]
thread_local! {
    /// What a test has run, once, after write-back has looked at a page
    /// (the frames it owes and the page's chain) and before it does
    /// anything with the first of them: what a commit or the pool does to
    /// the page in between.
    pub(super) static AFTER_THE_LOOK: std::cell::RefCell<Option<AfterTheLook>> =
        const { std::cell::RefCell::new(None) };
}

/// What one look at a page works on: the frames of its unwritten versions,
/// oldest first, and what write-back has kept of the page, taken in one
/// page step.
struct Look<'s> {
    frames: Vec<PageRef<'s>>,
    /// Whether the last frame is the page's current version.
    current: bool,
    changes: Vec<Changes>,
    /// The versions that are kept for readers and whose changes no block
    /// holds.
    gap: Vec<u64>,
}

/// What became of a superseded page version that was to be kept for its
/// readers.
enum Held {
    /// The pool keeps it, and its changes go into the next block.
    Kept,
    /// The pool does not list it as owed any more: a commit freed the
    /// page, and the pool may have dropped the frame since.
    NotOwed,
    /// It is the current version of its page again, to the pool.
    Current,
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
    /// calls. `page_size` is the store's, from its manifest, and must be
    /// the size of the pool's frames ([`FlushError::FrameSize`]).
    ///
    /// It takes no lock, so a view can be made wherever one is needed.
    pub fn new(
        stores: Stores<'s>,
        state: &'s FlushState,
        page_size: PageSize,
    ) -> Result<Self, FlushError> {
        if stores.pool.frame_len() != page_size.get() {
            return Err(FlushError::FrameSize {
                page_size: page_size.get(),
                frame_len: stores.pool.frame_len(),
            });
        }
        Ok(Self {
            stores,
            state,
            size: page_size,
        })
    }

    /// The state this handle works on.
    pub fn state(&self) -> &'s FlushState {
        self.state
    }

    /// The stores this handle works on.
    pub fn stores(&self) -> Stores<'s> {
        self.stores
    }

    /// The manifest, settled: if a replacement left it in doubt, it is
    /// opened again first, and every store is told of the manifest that is
    /// in force.
    ///
    /// **Each of the four stores is told whatever another one answers.**
    /// The manifest is settled once it has been opened again, and the next
    /// call finds nothing in doubt and tells nobody: a store that was left
    /// out because one before it could not delete a file would go on from
    /// the manifest of before the doubt until a replacement of its own.
    /// The call returns the first error once all four have been told. A
    /// store that answers that a file was not deleted has heard, and
    /// tries the deletion again when it is told the next time. One that
    /// refuses because it is read-only or of another directory has not,
    /// and refuses its own replacements as well.
    ///
    /// If the manifest cannot be opened again, the call ends with that
    /// error, no store is told, and the doubt stays for the next call.
    pub(crate) fn manifest(&self) -> Result<MutexGuard<'s, ManifestStore>, FlushError> {
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
            let told_segments = segments.manifest_replaced(&manifest);
            let told_values = values.manifest_replaced(&manifest);
            let told_log = log.manifest_replaced(&manifest);
            let told_checkpoints = checkpoints.manifest_replaced(&manifest);
            told_segments?;
            told_values?;
            told_log?;
            told_checkpoints?;
        }
        Ok(manifest)
    }

    /// Settles the manifest for whoever answers for the session: what
    /// write-back does by itself where it next needs the manifest, and
    /// nothing else. If a replacement left the manifest in doubt, it is
    /// opened again, and each of the four stores is told of the manifest
    /// that is in force. With nothing in doubt this makes no operation on
    /// a file.
    ///
    /// It waits for the lock on the manifest, so it is for a thread that
    /// holds no lock of the store. Where the manifest cannot be opened, it
    /// is in doubt as before, no store was told, and the call can be made
    /// again. Where it is opened and a store answers an error, every store
    /// was told all the same, the first such error is returned, and the
    /// manifest is in doubt no more: the next call finds nothing to
    /// settle.
    pub fn settle_manifest(&self) -> Result<(), FlushError> {
        self.manifest().map(drop)
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
    /// place write-back does not know. **So a commit goes on whatever this
    /// returns.** It fails only for bytes that are no group, which the log
    /// has refused before, and has then kept nothing of the commit.
    pub fn committed(
        &self,
        version: CommitVersion,
        at: LogPlace,
        group: &[u8],
    ) -> Result<(), FlushError> {
        let mut parts = Vec::new();
        let mut offset = 0;
        let mut told = Told::default();
        while offset < group.len() {
            let (record, len) = Record::decode(&group[offset..], FormatVersion::CURRENT)
                .map_err(|source| FlushError::Log(LogError::Refused { offset, source }))?;
            match record {
                Record::Part(part) => parts.push(part),
                Record::Value(part) => told.node_bytes += part.data().len(),
                _ => {}
            }
            offset += len;
        }

        let logged = group.len() as u64;
        let mut pages = 0;
        let mut kept = lock(&self.state.kept);
        let before = self
            .state
            .logged_in_all
            .fetch_add(logged, Ordering::Relaxed);
        kept.group_at(version.get(), at, before);
        for entry in parts.iter().flat_map(|part| part.entries()) {
            told.entry(&entry);
            match entry {
                Entry::Tree(run) => {
                    match run.created() {
                        None => {
                            let records = &run.as_bytes()[DeltaRun::HEADER_LEN..];
                            kept.committed(run.page(), version.get(), records);
                        }
                        Some(_) => kept.forget(run.page()),
                    }
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
        self.state.logged.fetch_add(logged, Ordering::Relaxed);
        told.count(&self.state.counters);
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
        let logged = record.len() as u64;
        self.state.logged.fetch_add(logged, Ordering::Relaxed);
        self.state
            .logged_in_all
            .fetch_add(logged, Ordering::Relaxed);
        Ok(appended)
    }

    /// Notes that an append to the log failed. Until the log has been
    /// rolled it takes no item (`transactions.md` CM9 item 5), and rolling
    /// it is write-back's ([`Self::roll_log`]): the loop is woken for it.
    pub(super) fn append_failed(&self, error: &LogError) {
        let cut_needed = matches!(
            error,
            LogError::TakenBack { .. } | LogError::NotAppended { .. } | LogError::NoActiveFile
        );
        if cut_needed {
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
    /// at the page, to place one value, and to push the link of a block
    /// while the pool still owes its version (`page-table.md` T18).
    ///
    /// An error that is about one page does not end the pass: the other
    /// pages are written, and the pass returns the first such error when
    /// it is done. Whatever it returns, what it wrote and linked stands,
    /// and it can be called again.
    pub fn pass(&self) -> Result<Pass, FlushError> {
        let (pass, result) = self.pass_through();
        result.map(|()| pass)
    }

    /// One pass, with what it did even if it ended with an error.
    pub(super) fn pass_through(&self) -> (Pass, Result<(), FlushError>) {
        let mut files = lock(&self.state.files);
        let mut round = Round {
            pass: Pass::default(),
            oldest_pin: self.stores.versions.oldest_pin(),
            regroups: self.stores.page_values.is_some(),
            failed: None,
            failing: BTreeSet::new(),
        };
        let ended = self.pass_with(&mut files, &mut round);
        let Round {
            mut pass,
            failed,
            failing,
            ..
        } = round;
        pass.waiting = self.owed().values().map(Vec::len).sum();
        self.state.owed.store(pass.waiting, Ordering::Relaxed);
        self.state.failing.store(failing.len(), Ordering::Relaxed);
        self.state.blocked.store(pass.blocked, Ordering::Relaxed);
        (pass, failed.map_or(ended, Err))
    }

    fn pass_with(&self, files: &mut Files, round: &mut Round) -> Result<(), FlushError> {
        let owed = self.owed();
        // The pages that have no pending value are written whatever
        // becomes of the values of the others.
        if let Err(error) = self.append_pending(files, &owed, round) {
            round.fail(error);
        }
        match self.place_ready() {
            Ok(placed) => round.pass.placed = placed,
            Err(error) => round.fail(error),
        }
        let mut ended = Ok(());
        for (&page, versions) in &owed {
            match self.write_page(files, page, versions, round) {
                Ok(()) => {}
                Err(error) if error.is_of_one_page() => round.fail(error),
                Err(error) => {
                    ended = Err(error);
                    break;
                }
            }
        }
        self.trim(files);
        ended
    }

    /// Writes what is owed of one page, oldest version first.
    fn write_page(
        &self,
        files: &mut Files,
        page: PageId,
        owed: &[CommitVersion],
        round: &mut Round,
    ) -> Result<(), FlushError> {
        // A regrouping puts other frames in place of the current
        // version's, so the page is then looked at again, for that version.
        match self.write_owed(files, page, owed, round, false)? {
            None => Ok(()),
            Some(born) => self.write_owed(files, page, &[born], round, true).map(drop),
        }
    }

    /// One look at a page: its owed versions are written, oldest first.
    /// Returns the version to look at again if the values of the page's
    /// current version were regrouped on the way. `again` says that this
    /// is such a second look.
    fn write_owed(
        &self,
        files: &mut Files,
        page: PageId,
        owed: &[CommitVersion],
        round: &mut Round,
        again: bool,
    ) -> Result<Option<CommitVersion>, FlushError> {
        let Stores { pool, table, .. } = self.stores;
        // The frames and what is kept of the page are taken in one page
        // step, so that the records are those that lead to the frames.
        let mut look = {
            let step = pool.page_step(page);
            let frames: Vec<PageRef<'s>> = owed
                .iter()
                .filter_map(|&born| pool.get_version(page, born))
                .collect();
            let current = frames.last().is_some_and(|frame| step.is_current(frame));
            let kept = lock(&self.state.kept);
            Look {
                frames,
                current,
                changes: kept.changes(page),
                gap: kept.folded(page),
            }
        };
        // The chain is kept out of `files`: reading a chain uses that
        // buffer of it for its own.
        files.links.clear();
        table.chain(page, &mut files.links);
        let mut links = std::mem::take(&mut files.links);
        let looked = self.write_frames(files, page, &mut look, &mut links, round, again);
        files.links = links;
        looked
    }

    fn write_frames(
        &self,
        files: &mut Files,
        page: PageId,
        look: &mut Look<'s>,
        links: &mut Vec<Link>,
        round: &mut Round,
        again: bool,
    ) -> Result<Option<CommitVersion>, FlushError> {
        #[cfg(test)]
        if let Some(change) = AFTER_THE_LOOK.with(|hook| hook.borrow_mut().take()) {
            change(page);
        }
        for (index, frame) in look.frames.iter().enumerate() {
            let born = frame.born();
            if let Some(freed) = links.last().filter(|link| link.kind == LinkKind::Free)
                && born < freed.version
            {
                // Nothing older than a free link is written back (T18),
                // and nothing that was kept of it is needed.
                lock(&self.state.kept).freed(page, freed.version.get());
                continue;
            }
            let is_current = look.current && index + 1 == look.frames.len();
            let cells = page::pending_cells(frame).map_err(FlushError::page(page))?;
            let pending = !cells.is_empty();
            let attached = frame.attachments().next().is_some();
            let stripped;
            let mut frame = frame;
            if is_current {
                if pending {
                    round.pass.blocked += 1;
                    return Ok(None);
                }
                if attached {
                    let Some(without) = self.strip(page, frame)? else {
                        round.pass.blocked += 1;
                        return Ok(None);
                    };
                    stripped = without;
                    frame = &stripped;
                }
            } else {
                let successor = look.frames.get(index + 1).map(PageRef::born);
                let unneeded = successor.is_some_and(|next| round.oldest_pin >= next);
                if pending || attached || unneeded {
                    match self.hold(page, frame)? {
                        Held::Kept => {
                            if !look.gap.contains(&born.get()) {
                                look.gap.push(born.get());
                            }
                            round.pass.held += 1;
                        }
                        Held::NotOwed => {}
                        Held::Current => return Ok(None),
                    }
                    continue;
                }
            }

            let form = self.plan(files, page, frame, links, &look.changes, &look.gap)?;
            // A page that is about to get a new base image has its values
            // regrouped first, and is then looked at again.
            if form != Form::Run && is_current && round.regroups && !again && is_tree(frame) {
                match self.regroup(files, page) {
                    Ok(0) => {}
                    Ok(_) => return Ok(Some(born)),
                    Err(error) => {
                        // The steps that were made stand, so the page is
                        // looked at again and written as it is. Nothing
                        // more is regrouped in this pass.
                        round.regroups = false;
                        round.fail(error);
                        return Ok(Some(born));
                    }
                }
            }
            match self.store(files, page, frame, form)? {
                Outcome::Written(link) => {
                    links.push(link);
                    look.gap.clear();
                    let mut kept = lock(&self.state.kept);
                    kept.unfold(page, born.get());
                    kept.wrote(page, &look.changes, born.get());
                    round.pass.written += 1;
                }
                Outcome::Refused => {
                    lock(&self.state.kept).forget(page);
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    /// Keeps the superseded page version in `frame` in the pool for its
    /// readers, with its changes noted for the block of the next version.
    ///
    /// The note is made before the pool is told: the version leaves the
    /// pool's list of what is owed only once it is noted, so whoever reads
    /// that list first and the notes second finds it in one of them
    /// ([`Self::floor`]).
    ///
    /// A commit can free the page between the look that pinned `frame` and
    /// this call. Then nothing is owed of the page ([`Held::NotOwed`]):
    /// the pool has taken the version off its list
    /// ([`CacheError::NotUnwritten`]), or has dropped the frame as well,
    /// once no reader could ask for it ([`CacheError::Replaced`]). No page
    /// step makes the second answer here: a step replaces the current
    /// version of a page, and this is called for a superseded one.
    fn hold(&self, page: PageId, frame: &PageRef<'s>) -> Result<Held, FlushError> {
        let born = frame.born().get();
        let noted = lock(&self.state.kept).fold(page, born);
        let refused = match self.stores.pool.hold_for_readers(frame) {
            Ok(()) => {
                count(&self.state.counters.held_for_readers, 1);
                return Ok(Held::Kept);
            }
            Err(refused) => refused,
        };
        if noted {
            lock(&self.state.kept).unfold_one(page, born);
        }
        match refused {
            CacheError::NotUnwritten { .. } | CacheError::Replaced { .. } => Ok(Held::NotOwed),
            CacheError::StillCurrent { .. } => Ok(Held::Current),
            error => Err(error.into()),
        }
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
        let writer = self.value_writer(&mut files.values)?;
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

    /// The value-log file that write-back fills. A new one is made if
    /// there is none, or the one there is is full.
    pub(super) fn value_writer<'f>(
        &self,
        writer: &'f mut Option<ValueWriter>,
    ) -> Result<&'f mut ValueWriter, FlushError> {
        let values = self.stores.values;
        if writer.as_ref().is_none_or(ValueWriter::is_full) {
            if let Some(full) = writer.take() {
                full.finish()?;
            }
            let made = values.create()?;
            values.commit(&mut *self.manifest()?)?;
            *writer = Some(made);
        }
        Ok(writer.as_mut().expect("a writer was just made"))
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
    /// how the page is to be written: as the run that is then in
    /// `files.run`, behind the chain `links`, or as that image.
    fn plan(
        &self,
        files: &mut Files,
        page: PageId,
        frame: &PageRef<'s>,
        links: &[Link],
        changes: &[Changes],
        gap: &[u64],
    ) -> Result<Form, FlushError> {
        let born = frame.born();
        files.image.clear();
        let pending = page::write_image(frame, &mut files.image).map_err(FlushError::page(page))?;
        if pending {
            return Err(FlushError::PendingValue { page, born });
        }
        if !is_tree(frame) {
            return Ok(Form::Image);
        }
        self.run_for(files, page, born, links, changes, gap)
    }

    /// Writes the page version in `frame` to a segment, as the run in
    /// `files.run` or as the image in `files.image`, pushes its link and
    /// tells the pool.
    fn store(
        &self,
        files: &mut Files,
        page: PageId,
        frame: &PageRef<'s>,
        form: Form,
    ) -> Result<Outcome, FlushError> {
        let Stores { pool, table, .. } = self.stores;
        let born = frame.born();
        let (link, location) = if form == Form::Run {
            let compression = match files.run.len() {
                len if len < COMPRESS_FROM => Compression::Raw,
                _ => Compression::Lz4,
            };
            let run = std::mem::take(&mut files.run);
            let location = self.append(files, page, BlockContent::Delta, compression, &run);
            files.run = run;
            let location = location?;
            (Link::delta(born, location), location)
        } else {
            let location = self.append_image(files, page)?;
            (Link::base(born, location), location)
        };

        let counters = &self.state.counters;
        // The second look (`page-table.md` T18). The pass took the frame
        // a while ago, and the page can have been freed since. A free
        // link refuses the block's link by its version, but a page that
        // was made under an id used before, and freed before it had a
        // link, was given none: the newest link of its id is the free
        // link of the page before it, which is older than this block.
        // So the link is pushed in a page step, in which the pool is
        // asked whether it still owes this version: `BufferPool::free`
        // takes that lock, and takes what is owed of the page off its
        // lists. The id may name another page by now, which is why the
        // question is about the version and not about the page.
        let pushed = {
            let step = pool.page_step(page);
            match step.owes(born) {
                true => table.push(page, link),
                false => {
                    count(&counters.links_refused, 1);
                    return Ok(Outcome::Refused);
                }
            }
        };
        match pushed {
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
        if form == Form::Run {
            count(&counters.delta_blocks, 1);
            count(&counters.delta_bytes, files.run.len() as u64);
            count(&counters.stored_bytes, u64::from(location.len));
        } else {
            self.linked_image(files, page, born, location, form == Form::Consolidation);
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

    /// Appends the image in `files.image` to a segment, as the base image
    /// of `page`.
    pub(super) fn append_image(
        &self,
        files: &mut Files,
        page: PageId,
    ) -> Result<BlockLocation, FlushError> {
        let image = std::mem::take(&mut files.image);
        let location = self.append(files, page, BlockContent::Base, Compression::Lz4, &image);
        files.image = image;
        location
    }

    /// Counts the base image in `files.image`, whose link of version
    /// `version` the page table took, and notes that the older links of
    /// the chain can be trimmed once no reader asks for a version before
    /// it.
    pub(super) fn linked_image(
        &self,
        files: &mut Files,
        page: PageId,
        version: CommitVersion,
        location: BlockLocation,
        consolidation: bool,
    ) {
        let counters = &self.state.counters;
        count(&counters.base_blocks, 1);
        count(&counters.base_bytes, files.image.len() as u64);
        count(&counters.stored_bytes, u64::from(location.len));
        count(&counters.consolidations, u64::from(consolidation));
        (files.untrimmed.entry(page))
            .and_modify(|waits| waits.newest = version)
            .or_insert(Untrimmed {
                newest: version,
                trimmed_at: None,
            });
    }

    /// Puts into `files.run` the run that brings the page from the newest
    /// link of `links` to the page in the frame whose image is
    /// `files.image`, and says whether the page is written as that run.
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
    ) -> Result<Form, FlushError> {
        let config = &self.state.config;
        let Some(newest) = links.last() else {
            return Ok(Form::Image);
        };
        let through = newest.version.get();
        if newest.kind == LinkKind::Free || through > born.get() || config.max_chain_deltas == 0 {
            return Ok(Form::Image);
        }
        // Every version of the page behind the newest link whose changes no
        // block holds has its records here, and nothing else has.
        if gap.iter().any(|&folded| folded <= through) {
            return Ok(Form::Image);
        }
        files.run.clear();
        files.run.extend_from_slice(&page.encode());
        for &version in gap.iter().chain([&born.get()]) {
            let Some(kept) = changes.iter().find(|kept| kept.born == version) else {
                return Ok(Form::Image);
            };
            files.run.extend_from_slice(&kept.records);
        }
        if files.run.len() == DeltaRun::HEADER_LEN
            || files.run.len() > max_run_len(self.size)
            || files.run.len() >= files.image.len()
        {
            return Ok(Form::Image);
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
            return Ok(Form::Consolidation);
        }

        // The page as the chain gives it. A frame of the version of the
        // newest link is that page, unless the frame is itself owed, which
        // is when the link and the frame are of one version. The frame
        // need not lie as a loader's does: what a run gives depends on
        // the content of the page alone (`page-delta.md` PD15).
        let in_pool = (through < born.get())
            .then(|| self.stores.pool.get_version(page, newest.version))
            .flatten();
        match in_pool {
            Some(before) => {
                files.scratch.clear();
                files.scratch.extend_from_slice(&before);
            }
            None => {
                if self.read_chain(files, page, newest)?.is_none() {
                    return Ok(Form::Image);
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
            return Ok(Form::Image);
        }
        Ok(Form::Run)
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
        let segment_len = self.state.config.segment_len;
        if let Some(full) = files.segment.take_if(|writer| writer.end() >= segment_len) {
            self.seal(files, full)?;
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
                let full = files.segment.take().expect("the segment that is full");
                self.seal(files, full)?;
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

    /// Seals a segment that takes no more blocks. If that fails, the
    /// segment is one that failed.
    pub(super) fn seal(
        &self,
        files: &mut Files,
        mut writer: SegmentWriter,
    ) -> Result<(), FlushError> {
        if let Err(error) = writer.seal() {
            self.segment_failed(files, writer);
            return Err(error.into());
        }
        Ok(())
    }

    /// Ends the use of a segment that an append or a seal failed. What was
    /// appended to it is synced. If that sync fails, the writer is kept and
    /// not dropped: dropping it would cut the segment at what was synced,
    /// and the blocks behind that, which chains point to, could no longer
    /// be read.
    pub(super) fn segment_failed(&self, files: &mut Files, mut writer: SegmentWriter) {
        if writer.sync().is_err() {
            self.state.segments_failed.store(true, Ordering::Release);
            count(&self.state.counters.segment_syncs_failed, 1);
            files.failed.push(writer);
        }
    }

    /// Removes from the chain of each of `pages` the links that no reader
    /// at the version `oldest` or later needs (`page-table.md` T11),
    /// between two passes, and returns the removed links that point to a
    /// block. Their blocks are counted as dead space
    /// ([`FlushState::dead_space`]).
    ///
    /// The caller vouches for `oldest`: no version pin is below it, and
    /// none will be. A caller that also waits until every operation that
    /// was running when it found that has ended spares the operations
    /// that hold no pin and read the latest version of their beginning.
    ///
    /// This is for whoever cleans segments, which trims the chains that a
    /// pass does not: those of freed pages, and those an earlier session
    /// left. **A chain is trimmed between passes because a trim can take
    /// the free link of a page away**, with everything older. A pass looks
    /// at the page versions that were unwritten when it began, and what
    /// tells it, before it writes a block, that one of them is of a page
    /// that a commit has freed since is that free link (T18). Without it
    /// the pass could write the block for nothing: the link is refused
    /// where it is pushed, because the pool owes the version no more. A
    /// pass that begins afterwards does not list such a version: the pool
    /// takes it off its list before the freeing commit is published, and
    /// `oldest` is no version that is not published.
    ///
    /// It waits for a pass that is under way, and holds no other lock of
    /// the store while it does.
    pub fn trim_chains(&self, pages: &[PageId], oldest: CommitVersion) -> Vec<Link> {
        let _files = lock(&self.state.files);
        let mut removed = Vec::new();
        for &page in pages {
            self.stores.table.trim(page, oldest, &mut removed);
        }
        if !removed.is_empty() {
            let mut kept = lock(&self.state.kept);
            removed.iter().for_each(|link| kept.died(link));
        }
        removed
    }

    /// Seals the segment `file` if it is the one that write-back is
    /// filling, so that the next block goes to a new segment. Returns
    /// whether it was.
    ///
    /// A segment is not cleaned while it is written. Whoever cleans
    /// segments asks for this when the segment that is being filled holds
    /// more dead blocks than it can leave there, or has become so large a
    /// part of the store that it could not be moved in one piece. It
    /// waits for a pass that is under way.
    pub fn seal_segment(&self, file: lignin_format::FileId) -> Result<bool, FlushError> {
        let mut files = lock(&self.state.files);
        let Some(writer) = files.segment.take_if(|writer| writer.file_id() == file) else {
            return Ok(false);
        };
        self.seal(&mut files, writer)?;
        Ok(true)
    }

    /// Closes the value-log file `file` if it is the one that write-back
    /// is filling, so that the next value goes to a new file. Returns
    /// whether it was.
    ///
    /// A value-log file is not cleaned while it is written. Whoever cleans
    /// the value log asks for this when the file that is being filled
    /// holds more dead values than it can leave there, or has become too
    /// large a part of the value log. It waits for a pass that is under
    /// way. The values of the file that wait for their page step keep
    /// their holds.
    pub fn close_value_file(&self, file: lignin_format::FileId) -> Result<bool, FlushError> {
        let mut files = lock(&self.state.files);
        let Some(writer) = files.values.take_if(|writer| writer.file_id() == file) else {
            return Ok(false);
        };
        writer.finish()?;
        Ok(true)
    }

    /// Removes the links that no reader needs any more from the chains of
    /// the pages that got a base image, once no reader can ask for a
    /// version before that image's (`page-table.md` T11).
    ///
    /// A page stays in the list while its newest image is newer than the
    /// version the chains may be trimmed at, which is found by the rule
    /// the cleaner's trims go by too
    /// ([`TrimBound`](super::trim_bound::TrimBound)), and a later pass
    /// trims it. Meanwhile its chain is trimmed at that version, once for
    /// each version: the links under an older image of the page go as
    /// soon as no reader is below that image, whatever newer images the
    /// page has got since. A page that gets an image in every pass would
    /// keep them all otherwise, while operations follow one another
    /// closely enough that the version is always a pass old.
    fn trim(&self, files: &mut Files) {
        if files.untrimmed.is_empty() {
            return;
        }
        let Some(at) = files.trim_bound.ask(self.stores.versions) else {
            return;
        };
        let mut removed = Vec::new();
        files.untrimmed.retain(|&page, waits| {
            if waits.trimmed_at < Some(at) {
                self.stores.table.trim(page, at, &mut removed);
                waits.trimmed_at = Some(at);
            }
            waits.newest > at
        });
        if !removed.is_empty() {
            let mut kept = lock(&self.state.kept);
            removed.iter().for_each(|link| kept.died(link));
        }
    }
}
