//! A model of the page table and of its checkpoints, and a workload that
//! drives a table, its checkpoints and the model together.
//!
//! The model is written from the rules, not from the table: a chain is a
//! list of links, and a checkpoint is a copy of every list. A run stops at
//! the first error, which is how a power loss ends it, and what is then on
//! the file system is checked against the model by [`check_recovered`].
//!
//! The model also keeps what the table is never told: the log, which is
//! what each commit and each background page step did to its page, how
//! much of the log is durable, and the changes each block holds. A crash
//! can leave any whole number of the log's items from the durable ones on,
//! and for each of them the model works out what recovery has of every
//! page: the chain of the checkpoint, and the log replayed on it.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
};

use lignin_format::segment::{BlockContent, Compression};
use lignin_io::sim::SimVfs;

use super::*;
use crate::segment::{
    tests::sim::{db_dir, shared},
    SegmentWriter,
};

const MAX_PAYLOAD: usize = 1 << 16;

/// What the recoveries that were worked out on this thread met.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Recoveries {
    /// Recoveries of one page at one landing version.
    pub(super) pages: usize,
    /// Those from a checkpoint with no link of a page that was in use at
    /// the version the checkpoint covers.
    pub(super) forgotten: usize,
    /// Those in which replay dropped a change it had no image for.
    pub(super) dropped: usize,
    /// Those that gave a page with a page step of background work in it.
    pub(super) steps: usize,
}

thread_local! {
    pub(super) static RECOVERIES: Cell<Recoveries> = const {
        Cell::new(Recoveries { pages: 0, forgotten: 0, dropped: 0, steps: 0 })
    };
}

pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(super) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn is_block(location: BlockLocation) -> bool {
    (8..=(1 << 27) + 4).contains(&location.len)
        && location.offset >= 20
        && location
            .offset
            .checked_add(u64::from(location.len))
            .is_some()
}

/// The chains of every page that has a link, each from its oldest link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ModelTable {
    pub(super) chains: BTreeMap<u32, Vec<Link>>,
    /// The newest version of a free link that a trim removed.
    pub(super) trimmed_free: Option<CommitVersion>,
}

impl ModelTable {
    pub(super) fn of(links: &[(PageId, Link)]) -> Self {
        let mut table = Self::default();
        for (page, link) in links {
            table.chains.entry(page.get()).or_default().push(*link);
        }
        table
    }

    pub(super) fn links(&self) -> Vec<(PageId, Link)> {
        self.chains
            .iter()
            .flat_map(|(raw, chain)| chain.iter().map(|link| (page(*raw), *link)))
            .collect()
    }

    pub(super) fn chain(&self, raw_page: u32) -> &[Link] {
        self.chains.get(&raw_page).map_or(&[], Vec::as_slice)
    }

    pub(super) fn push(&mut self, raw_page: u32, link: Link) -> Result<(), LinkError> {
        if let Some(location) = link.location().filter(|location| !is_block(*location)) {
            return Err(LinkError::InvalidLocation { location });
        }
        let is_delta = matches!(link.kind, LinkKind::Delta(_));
        match self.chain(raw_page).last() {
            None if is_delta => return Err(LinkError::DeltaWithoutBase),
            None => {}
            Some(newest) if link.version < newest.version => {
                return Err(LinkError::OutOfOrder {
                    newest: newest.version,
                    version: link.version,
                });
            }
            Some(newest) if newest.kind == LinkKind::Free => match link.kind {
                LinkKind::Delta(_) => return Err(LinkError::DeltaWithoutBase),
                LinkKind::Free => return Err(LinkError::AlreadyFree),
                LinkKind::Base(_) => {}
            },
            Some(_) => {}
        }
        self.chains.entry(raw_page).or_default().push(link);
        Ok(())
    }

    /// The page version a reader at `at` sees: the links that are not newer
    /// than `at`, from the last base image among them on. Nothing if the
    /// last of them says the page was freed.
    pub(super) fn read(
        &self,
        raw_page: u32,
        at: CommitVersion,
    ) -> Option<(CommitVersion, Vec<BlockLocation>)> {
        let seen: Vec<&Link> = self
            .chain(raw_page)
            .iter()
            .filter(|link| link.version <= at)
            .collect();
        let newest = seen.last()?;
        if newest.kind == LinkKind::Free {
            return None;
        }
        let base = seen
            .iter()
            .rposition(|link| matches!(link.kind, LinkKind::Base(_)))?;
        let blocks = seen[base..]
            .iter()
            .map(|link| link.location().expect("a base image and deltas"))
            .collect();
        Some((newest.version, blocks))
    }

    fn position(&self, raw_page: u32, location: BlockLocation) -> Result<usize, LinkError> {
        self.chain(raw_page)
            .iter()
            .rposition(|link| link.location() == Some(location))
            .ok_or(LinkError::NotInChain { location })
    }

    pub(super) fn consolidate(
        &mut self,
        raw_page: u32,
        through: BlockLocation,
        image: BlockLocation,
    ) -> Result<CommitVersion, LinkError> {
        if !is_block(image) {
            return Err(LinkError::InvalidLocation { location: image });
        }
        let at = self.position(raw_page, through)?;
        let chain = self.chains.get_mut(&raw_page).expect("a chain");
        let born = chain[at].version;
        chain.insert(at + 1, Link::base(born, image));
        Ok(born)
    }

    pub(super) fn relocate(
        &mut self,
        raw_page: u32,
        from: BlockLocation,
        to: BlockLocation,
    ) -> Result<(), LinkError> {
        if !is_block(to) {
            return Err(LinkError::InvalidLocation { location: to });
        }
        let at = self.position(raw_page, from)?;
        let link = &mut self.chains.get_mut(&raw_page).expect("a chain")[at];
        link.kind = match link.kind {
            LinkKind::Base(_) => LinkKind::Base(to),
            LinkKind::Delta(_) => LinkKind::Delta(to),
            LinkKind::Free => unreachable!("a free link has no location"),
        };
        Ok(())
    }

    /// Removes what no reader at `oldest` or later needs. Returns the links
    /// removed that point to a block, and whether the page has no link left.
    pub(super) fn trim(&mut self, raw_page: u32, oldest: CommitVersion) -> (Vec<Link>, bool) {
        let Some(chain) = self.chains.get_mut(&raw_page) else {
            return (Vec::new(), true);
        };
        let anchor = chain
            .iter()
            .rposition(|link| link.version <= oldest && !matches!(link.kind, LinkKind::Delta(_)));
        let keep_from = match anchor {
            None => 0,
            Some(anchor) if chain[anchor].kind == LinkKind::Free => anchor + 1,
            Some(anchor) => anchor,
        };
        let removed: Vec<Link> = chain.drain(..keep_from).collect();
        let freed = removed
            .iter()
            .filter(|link| link.kind == LinkKind::Free)
            .map(|link| link.version)
            .max();
        self.trimmed_free = self.trimmed_free.max(freed);
        let removed = removed
            .into_iter()
            .filter(|link| link.location().is_some())
            .collect();
        let vacant = chain.is_empty();
        if vacant {
            self.chains.remove(&raw_page);
        }
        (removed, vacant)
    }

    pub(super) fn discard_after(&mut self, newest: CommitVersion) -> u64 {
        let mut discarded = 0;
        self.chains.retain(|_, chain| {
            let before = chain.len();
            chain.retain(|link| link.version <= newest);
            discarded += (before - chain.len()) as u64;
            !chain.is_empty()
        });
        discarded
    }
}

/// What a checkpoint must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Checkpointed {
    pub(super) covers: CommitVersion,
    pub(super) links: Vec<(PageId, Link)>,
    /// The sample of the value counts that was written with it.
    pub(super) sample: CountSample,
}

/// The items of the log that each of its files holds.
const ITEMS_PER_LOG_FILE: usize = 5;

/// Where a log of `items` items ends. Nowhere while it has none.
fn log_end(items: usize) -> Option<LogPlace> {
    let last = items.checked_sub(1)?;
    let file = id(1 + (last / ITEMS_PER_LOG_FILE) as u32);
    let offset = 20 + 64 * (1 + (last % ITEMS_PER_LOG_FILE) as u64);
    Some(LogPlace::new(file, offset).expect("a place"))
}

/// How many items the log has before `place`: what [`log_end`] was given.
fn items_before(place: Option<LogPlace>) -> usize {
    let Some(place) = place else {
        return 0;
    };
    let in_file = ((place.offset() - 20) / 64) as usize;
    (place.file().get() as usize - 1) * ITEMS_PER_LOG_FILE + in_file
}

/// What a commit, or a page step of background work, did to its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// The commit made the page, with its first content.
    Created,
    /// The commit changed the page.
    Changed,
    /// The commit freed the page.
    Freed,
    /// A page step placed or moved a value of the page. It is of no commit.
    Stepped,
}

/// One item of the log, as far as it is about one page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Logged {
    /// How many items the log has before this one.
    at: usize,
    /// The version of the commit, or of the last commit before a page step.
    version: u64,
    /// What names the change in a page: the version of a commit, a number
    /// from [`FIRST_STEP`] on for a page step.
    name: u64,
    change: Change,
}

/// The names of page steps begin here, above every commit version.
const FIRST_STEP: u64 = 1 << 40;

/// A page as the log left it: the changes that made it up since it was
/// made, by their names, or `None` for a page that is not in use.
type Page = Option<Vec<u64>>;

/// The commits among the changes of `page`.
fn commits_of(page: Page) -> Page {
    page.map(|changes| {
        changes
            .into_iter()
            .filter(|name| *name < FIRST_STEP)
            .collect()
    })
}

/// Applies the change `logged` to `page`. Returns `false` for a change to a
/// page that is not there.
fn apply(page: &mut Page, logged: &Logged) -> bool {
    match (logged.change, page.as_mut()) {
        (Change::Created, _) => *page = Some(vec![logged.name]),
        (Change::Changed | Change::Stepped, Some(changes)) => changes.push(logged.name),
        (Change::Changed | Change::Stepped, None) => return false,
        (Change::Freed, _) => *page = None,
    }
    true
}

/// One block that was appended to a segment.
#[derive(Clone, Debug)]
struct Block {
    content: BlockContent,
    payload: Vec<u8>,
    /// The changes the block holds, by their names: all that make up the
    /// page, for a base image, and those of the run, for a delta run.
    holds: Vec<u64>,
}

/// What must be true of a database directory.
#[derive(Clone, Debug, Default)]
pub(super) struct Model {
    pub(super) table: ModelTable,
    /// The last commit version.
    pub(super) version: u64,
    /// The log: for each of its items, the version of the last commit up to
    /// it. An item is the group of a commit or the record of a page step.
    pub(super) items: Vec<u64>,
    /// How many items of the log are durable. A crash leaves them, and any
    /// number of the items after them, in order.
    pub(super) durable_items: usize,
    /// How many page steps were made, in every session.
    steps_made: u64,
    /// The log: what each item did, by its page, in order.
    log: BTreeMap<u32, Vec<Logged>>,
    /// The commits that are not written back, by their page: the buffer
    /// pool holds their changes, and no link does.
    held: BTreeMap<u32, Vec<u64>>,
    /// Every block appended, by its segment and offset.
    blocks: BTreeMap<(u32, u64), Block>,
    /// The segments in the store.
    segments: BTreeSet<u32>,
    /// The checkpoint of the last installation that returned, if any.
    pub(super) acked: Option<Checkpointed>,
    /// The checkpoint an installation in progress is naming.
    pub(super) pending: Option<Checkpointed>,
}

impl Model {
    fn checkpointed(&self, covers: CommitVersion, sample: &CountSample) -> Checkpointed {
        Checkpointed {
            covers,
            links: self.table.links(),
            sample: sample.clone(),
        }
    }

    fn log_item(&mut self, raw_page: u32, name: u64, change: Change) {
        let logged = Logged {
            at: self.items.len(),
            version: self.version,
            name,
            change,
        };
        self.items.push(self.version);
        self.log.entry(raw_page).or_default().push(logged);
    }

    /// Takes the next version for a commit that does `change` to
    /// `raw_page`, and logs it.
    fn commit(&mut self, raw_page: u32, change: Change) -> u64 {
        self.version += 1;
        self.log_item(raw_page, self.version, change);
        self.version
    }

    /// Logs a page step of background work on `raw_page`, and returns its
    /// name.
    fn step(&mut self, raw_page: u32) -> u64 {
        self.steps_made += 1;
        let name = FIRST_STEP + self.steps_made;
        self.log_item(raw_page, name, Change::Stepped);
        name
    }

    fn logged(&self, raw_page: u32) -> &[Logged] {
        self.log.get(&raw_page).map_or(&[], Vec::as_slice)
    }

    /// The version of the last commit among the first `items` of the log.
    pub(super) fn version_at(&self, items: usize) -> u64 {
        items.checked_sub(1).map_or(0, |last| self.items[last])
    }

    /// How many items the log has up to the group of the commit of
    /// `version`, with that group.
    fn items_through(&self, version: u64) -> usize {
        let before = self.items.iter().take_while(|at| **at < version).count();
        (before + usize::from(version > 0)).min(self.items.len())
    }

    /// The version the log is durable through.
    pub(super) fn log_durable(&self) -> u64 {
        self.version_at(self.durable_items)
    }

    /// How far the log is durable.
    fn durable(&self) -> LogDurable {
        LogDurable {
            version: version(self.log_durable()),
            place: log_end(self.durable_items),
        }
    }

    /// Syncs the log through the commit of `version` and up to `place`, and
    /// no further.
    fn sync_log(&mut self, version: u64, place: Option<LogPlace>) {
        let through = self.items_through(version).max(items_before(place));
        assert!(through <= self.items.len());
        self.durable_items = self.durable_items.max(through);
    }

    /// The page as the first `items` of the log left it.
    fn page_at(&self, raw_page: u32, items: usize) -> Page {
        let mut page = None;
        for logged in self.logged(raw_page) {
            if logged.at < items {
                apply(&mut page, logged);
            }
        }
        page
    }

    /// The page as it is now.
    fn page_now(&self, raw_page: u32) -> Page {
        self.page_at(raw_page, self.items.len())
    }

    fn is_live(&self, raw_page: u32) -> bool {
        self.page_now(raw_page).is_some()
    }

    /// The newest version a checkpoint can cover: every change up to it is
    /// in a block, or is a change to a page that was freed since.
    fn written_back(&self) -> u64 {
        let first_held = self.held.values().filter_map(|held| held.first()).min();
        first_held.map_or(self.version, |first| first - 1)
    }

    /// What the blocks of `chain` give a reader of its newest link.
    fn chain_holds(&self, chain: &[Link]) -> Page {
        let base = chain
            .iter()
            .rposition(|link| !matches!(link.kind, LinkKind::Delta(_)))?;
        let mut commits = Vec::new();
        for link in &chain[base..] {
            let location = link.location()?;
            commits.extend(&self.blocks[&(location.file.get(), location.offset)].holds);
        }
        Some(commits)
    }

    /// What a recovery has of `raw_page` when it loads the checkpoint
    /// `found`, which holds `chain` of the page, and the crash left the
    /// first `landing` items of the log: what the chain gives, and on it
    /// the logged changes of the commits after the version the checkpoint
    /// covers and after the chain's newest link, and the page steps from
    /// the place of the checkpoint's sample on that the page lacks.
    fn recovered(&self, raw_page: u32, chain: &[Link], found: Found, landing: usize) -> Page {
        let newest = chain.last().map(|link| link.version.get());
        let mut page = self.chain_holds(chain);
        if let Some(newest) = newest {
            let landed = self.version_at(landing);
            assert!(
                newest <= landed,
                "page {raw_page} has a link of version {newest}, and recovery lands on {landed}"
            );
            let as_committed = self.page_at(raw_page, self.items_through(newest));
            assert_eq!(
                commits_of(page.clone()),
                commits_of(as_committed),
                "the chain of page {raw_page} at version {newest}"
            );
        }
        let from = newest.map_or(found.covers, |newest| newest.max(found.covers));
        let (mut whole, mut dropped) = (true, false);
        for logged in self.logged(raw_page) {
            if logged.at >= landing {
                break;
            }
            let due = if logged.change == Change::Stepped {
                // A page step has no version: it is looked at from the
                // place of the sample on, and made where the page lacks it.
                logged.at >= found.sampled
                    && newest.is_none_or(|newest| newest <= logged.version)
                    && page
                        .as_ref()
                        .is_none_or(|changes| !changes.contains(&logged.name))
            } else {
                logged.version > from
            };
            if due {
                whole = apply(&mut page, logged);
                dropped |= !whole;
            }
        }
        let covered = self.items_through(found.covers);
        let forgotten = chain.is_empty() && self.page_at(raw_page, covered).is_some();
        RECOVERIES.with(|met| {
            let mut recoveries = met.get();
            recoveries.pages += 1;
            recoveries.forgotten += usize::from(forgotten);
            recoveries.dropped += usize::from(dropped);
            recoveries.steps += usize::from(
                page.as_ref()
                    .is_some_and(|changes| changes.iter().any(|name| *name >= FIRST_STEP)),
            );
            met.set(recoveries);
        });
        assert!(
            whole,
            "replay of {landing} items leaves page {raw_page} changed and without an image"
        );
        page
    }

    /// Checks that a recovery that loads `links`, as the checkpoint `found`,
    /// and is left the first `landing` items of the log has every page as
    /// those items left it: none that is in use is missing, none that is
    /// free is there, and each holds the page steps whose records are among
    /// the items, and no other.
    fn check_landing(&self, links: &[(PageId, Link)], found: Found, landing: usize) {
        assert!(
            found.sampled <= landing,
            "a sample taken after {} items, and a log of {landing}",
            found.sampled
        );
        let table = ModelTable::of(links);
        let pages: BTreeSet<u32> = table
            .chains
            .keys()
            .chain(self.log.keys())
            .copied()
            .collect();
        for raw_page in pages {
            assert_eq!(
                self.recovered(raw_page, table.chain(raw_page), found, landing),
                self.page_at(raw_page, landing),
                "page {raw_page} after {landing} items of the log, from a checkpoint that covers \
                 {} and was sampled after {} items",
                found.covers,
                found.sampled
            );
        }
    }

    /// Checks every log a crash can leave now.
    fn check_every_landing(&self, links: &[(PageId, Link)], found: Found) {
        for landing in self.durable_items..=self.items.len() {
            self.check_landing(links, found, landing);
        }
    }

    /// A crash lost the items of the log after the first `landing`.
    fn land_on(&mut self, landing: usize) {
        assert!((self.durable_items..=self.items.len()).contains(&landing));
        self.items.truncate(landing);
        self.version = self.version_at(landing);
        for logged in self.log.values_mut() {
            logged.retain(|logged| logged.at < landing);
        }
    }

    fn installed(&mut self) {
        self.acked = Some(self.pending.take().expect("an installation in progress"));
    }
}

/// What a recovery reads from the checkpoint it loads, apart from the
/// chains: the version it covers and how many items of the log were before
/// the place of its sample. Both are 0 where no checkpoint is named.
#[derive(Clone, Copy, Debug)]
struct Found {
    covers: u64,
    sampled: usize,
}

impl Found {
    fn of(info: Option<&CheckpointInfo>) -> Self {
        Self {
            covers: info.map_or(0, |info| info.covers().get()),
            sampled: items_before(info.and_then(|info| info.log_kept_from())),
        }
    }
}

fn segments_of(links: &[(PageId, Link)]) -> BTreeSet<u32> {
    links
        .iter()
        .filter_map(|(_, link)| Some(link.location()?.file.get()))
        .collect()
}

/// One step of a workload. A number is a page id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// The segment being written is sealed, and the blocks that follow go
    /// to a new one.
    Segment,
    /// A commit writes a base image of the page.
    Base(u32),
    /// A commit writes a delta of the page, or a base image if it has none.
    Delta(u32),
    /// A commit frees the page.
    Free(u32),
    /// A commit changes the page, and the change stays in the buffer pool.
    Change(u32),
    /// A page step of background work places or moves a value of the page:
    /// one record in the log, of no commit. The page is written back with
    /// the change.
    Background(u32),
    /// The changes of the page that the buffer pool holds are written as
    /// one delta run.
    WriteBack(u32),
    /// The chain of the page is consolidated into a base image.
    Consolidate(u32),
    /// The oldest block of the page is copied and its link switched.
    Relocate(u32),
    /// The chain of the page is trimmed to what the latest version needs.
    Trim(u32),
    /// The segments are synced and the manifest names them.
    Sync,
    /// The log is synced through the last commit.
    SyncLog,
    /// The log is synced through the last commit, and a checkpoint of all
    /// that is written back is written and installed.
    Checkpoint,
    /// A checkpoint is written that covers no more than the log is durable
    /// through, with a sample that was taken where the durable part of the
    /// log ends. The log is synced only as far as the checkpoint needs, and
    /// then the checkpoint is installed.
    CheckpointBehind,
    /// A checkpoint is written and discarded.
    Abandon,
    /// The segments that neither the table nor the checkpoint in force
    /// points into are dropped, and the manifest is replaced.
    DropUnused,
    /// The session ends and another one loads the checkpoint.
    Restart,
}

struct Session {
    db: Db,
    table: PageTable,
    writer: Option<SegmentWriter>,
    /// A checkpoint that was written and is not known to be installed.
    written: Option<CheckpointInfo>,
}

/// One run of a workload on one file system.
pub(super) struct Run {
    pub(super) model: Model,
    fs: SimVfs,
    rng: Rng,
    session: Option<Session>,
    pub(super) steps: usize,
}

impl Run {
    pub(super) fn new(fs: &SimVfs, seed: u64) -> Self {
        Self {
            model: Model::default(),
            fs: fs.clone(),
            rng: Rng::new(seed),
            session: None,
            steps: 0,
        }
    }

    /// Goes on after a crash, on the file system `fs` that the crash left,
    /// with the first `landing` items of the log.
    pub(super) fn crashed(&mut self, fs: &SimVfs, new: bool, landing: usize) {
        self.session = None;
        self.fs = fs.clone();
        if new {
            self.model.installed();
        }
        self.model.pending = None;
        self.model.land_on(landing);
    }

    /// Ends the session, as a process that exits does.
    pub(super) fn end_session(&mut self) {
        self.session = None;
    }

    /// Starts a session: opens the directory, or creates its first manifest,
    /// and loads the checkpoint in force, which must be the one the model
    /// has. Then it replays the log on it up to the last commit.
    fn open(&mut self) -> Result<(), PageTableError> {
        let db = match Db::open(&self.fs) {
            Err(PageTableError::Store(SegmentStoreError::NoManifest { .. })) => {
                assert!(self.model.acked.is_none());
                let manifest = ManifestStore::create(shared(&self.fs), db_dir(), empty_manifest())?;
                Db::with(manifest)?
            }
            db => db?,
        };
        let (table, info) = db.load()?;
        let model = &mut self.model;
        let links = all_links(&table);
        let read = Found::of(info.as_ref());
        let found = info.as_ref().map(|info| Checkpointed {
            covers: info.covers(),
            links: links.clone(),
            sample: info.sample().clone(),
        });
        if model.pending.is_some() && found == model.pending {
            model.installed();
        }
        model.pending = None;
        assert_eq!(found, model.acked);
        assert_eq!(info.and_then(|info| info.trimmed_free()), None);
        model.check_landing(&links, read, model.items.len());
        model.table = ModelTable::of(&links);
        model.held.clear();
        model.segments = db
            .manifest
            .manifest()?
            .files()
            .iter()
            .map(|file| file.id().get())
            .collect();
        self.session = Some(Session {
            db,
            table,
            writer: None,
            written: None,
        });
        let replayed = self.replay();
        if replayed.is_err() {
            self.session = None;
        }
        replayed
    }

    /// Does what recovery and the write-back after it do: every page whose
    /// chain does not give it as the log left it gets a base image of what
    /// it is now, or its free link.
    fn replay(&mut self) -> Result<(), PageTableError> {
        let pages: Vec<u32> = self.model.log.keys().copied().collect();
        for raw_page in pages {
            let chain = self.model.table.chain(raw_page);
            let newest = chain.last().copied();
            let now = self.model.page_now(raw_page);
            if now == self.model.chain_holds(chain) {
                continue;
            }
            let last = self.model.logged(raw_page).iter().rev();
            let last = last
                .map(|logged| logged.version)
                .next()
                .expect("a logged change");
            match now {
                Some(changes) => {
                    let payload = self.payload();
                    let image = self.append(raw_page, BlockContent::Base, payload, changes)?;
                    self.push(raw_page, Link::base(version(last), image));
                }
                None if newest.is_some_and(|link| link.kind != LinkKind::Free) => {
                    self.push(raw_page, Link::free(version(last)));
                }
                None => {}
            }
        }
        Ok(())
    }

    fn session(&mut self) -> &mut Session {
        self.session.as_mut().expect("a session")
    }

    fn new_segment(&mut self) -> Result<(), PageTableError> {
        let session = self.session.as_mut().expect("a session");
        if let Some(mut writer) = session.writer.take() {
            writer.seal()?;
        }
        let writer = session.db.segments.create()?;
        let raw_id = writer.file_id().get();
        session.writer = Some(writer);
        self.model.segments.insert(raw_id);
        session.db.segments.commit(&mut session.db.manifest)?;
        Ok(())
    }

    /// Appends one block of `raw_page` to the segment being written.
    fn append(
        &mut self,
        raw_page: u32,
        content: BlockContent,
        payload: Vec<u8>,
        holds: Vec<u64>,
    ) -> Result<BlockLocation, PageTableError> {
        if self.session().writer.is_none() {
            self.new_segment()?;
        }
        let writer = self.session().writer.as_mut().expect("a writer");
        let location = writer.append(page(raw_page), content, Compression::Raw, &payload)?;
        let block = Block {
            content,
            payload,
            holds,
        };
        self.model
            .blocks
            .insert((location.file.get(), location.offset), block);
        Ok(location)
    }

    /// A sample of the value counts, as one taken when the log had `at`
    /// items: the counts of up to three value-log files.
    fn sample(&mut self, at: usize) -> CountSample {
        let mut file = 0;
        let counts = (0..self.rng.below(4))
            .map(|_| {
                file += 1 + self.rng.below(3) as u32;
                ValueCount {
                    file: id(file),
                    values: self.rng.below(1000),
                    bytes: self.rng.next() >> 20,
                    miscounted: self.rng.below(5) == 0,
                }
            })
            .collect();
        CountSample {
            place: log_end(at),
            counts,
        }
    }

    fn payload(&mut self) -> Vec<u8> {
        let len = self.rng.below(90) as usize;
        (0..len).map(|_| self.rng.next() as u8).collect()
    }

    fn push(&mut self, raw_page: u32, link: Link) {
        let pushed = self.session().table.push(page(raw_page), link);
        assert_eq!(pushed, self.model.table.push(raw_page, link));
        pushed.expect("the workload makes links that fit");
    }

    /// Takes one step, on the table and on the model.
    pub(super) fn take(&mut self, step: Step) -> Result<(), PageTableError> {
        if self.session.is_none() {
            self.open()?;
        }
        let newest = |model: &Model, raw_page: u32| model.table.chain(raw_page).last().copied();
        match step {
            Step::Segment => self.new_segment()?,
            Step::Base(raw_page) | Step::Delta(raw_page) => {
                let live = self.model.is_live(raw_page);
                let content = if live && matches!(step, Step::Delta(_)) {
                    BlockContent::Delta
                } else {
                    BlockContent::Base
                };
                let change = if live {
                    Change::Changed
                } else {
                    Change::Created
                };
                let born = self.model.commit(raw_page, change);
                let mut commits = self.model.held.remove(&raw_page).unwrap_or_default();
                commits.push(born);
                if content == BlockContent::Base {
                    commits = self.model.page_now(raw_page).expect("a page in use");
                }
                let payload = self.payload();
                let location = self.append(raw_page, content, payload, commits)?;
                let born = version(born);
                let link = match content {
                    BlockContent::Base => Link::base(born, location),
                    BlockContent::Delta => Link::delta(born, location),
                };
                self.push(raw_page, link);
            }
            Step::Free(raw_page) => {
                if self.model.is_live(raw_page) {
                    let freed = self.model.commit(raw_page, Change::Freed);
                    self.model.held.remove(&raw_page);
                    self.push(raw_page, Link::free(version(freed)));
                }
            }
            Step::Change(raw_page) => {
                if self.model.is_live(raw_page) {
                    let born = self.model.commit(raw_page, Change::Changed);
                    self.model.held.entry(raw_page).or_default().push(born);
                }
            }
            Step::Background(raw_page) => {
                // Only a page that is written back: the block that follows
                // holds the step, and what the buffer pool holds of a page
                // goes to a segment in the order it was made.
                let written_back = newest(&self.model, raw_page)
                    .filter(|link| link.kind != LinkKind::Free)
                    .filter(|_| !self.model.held.contains_key(&raw_page));
                if let Some(link) = written_back {
                    let name = self.model.step(raw_page);
                    let payload = self.payload();
                    let run = self.append(raw_page, BlockContent::Delta, payload, vec![name])?;
                    self.push(raw_page, Link::delta(link.version, run));
                }
            }
            Step::WriteBack(raw_page) => {
                if let Some(commits) = self.model.held.remove(&raw_page) {
                    let born = version(*commits.last().expect("a commit"));
                    let payload = self.payload();
                    let run = self.append(raw_page, BlockContent::Delta, payload, commits)?;
                    self.push(raw_page, Link::delta(born, run));
                }
            }
            Step::Consolidate(raw_page) => {
                let through = newest(&self.model, raw_page).and_then(|link| link.location());
                if let Some(through) = through {
                    let commits = self.model.chain_holds(self.model.table.chain(raw_page));
                    let commits = commits.expect("a chain that ends in a block");
                    let payload = self.payload();
                    let image = self.append(raw_page, BlockContent::Base, payload, commits)?;
                    let table = &self.session.as_ref().expect("a session").table;
                    let born = table.consolidate(page(raw_page), through, image);
                    assert_eq!(born, self.model.table.consolidate(raw_page, through, image));
                    born.expect("the newest link is in the chain");
                }
            }
            Step::Relocate(raw_page) => {
                let from = self.model.table.chain(raw_page).first().copied();
                if let Some(from) = from.and_then(|link| link.location()) {
                    let block = self.model.blocks[&(from.file.get(), from.offset)].clone();
                    let to = self.append(raw_page, block.content, block.payload, block.holds)?;
                    let table = &self.session.as_ref().expect("a session").table;
                    let moved = table.relocate(page(raw_page), from, to);
                    assert_eq!(moved, self.model.table.relocate(raw_page, from, to));
                    moved.expect("the oldest link is in the chain");
                }
            }
            Step::Trim(raw_page) => {
                let oldest = version(self.model.version);
                let mut removed = Vec::new();
                let table = &self.session.as_ref().expect("a session").table;
                let vacant = table.trim(page(raw_page), oldest, &mut removed);
                let (expected, expected_vacant) = self.model.table.trim(raw_page, oldest);
                removed.reverse();
                assert_eq!((removed, vacant), (expected, expected_vacant));
            }
            Step::Sync => {
                let db = &mut self.session.as_mut().expect("a session").db;
                db.segments.sync()?;
                db.segments.commit(&mut db.manifest)?;
            }
            Step::SyncLog => self.model.durable_items = self.model.items.len(),
            Step::Checkpoint | Step::CheckpointBehind => {
                let behind = step == Step::CheckpointBehind;
                let mut covers = self.model.written_back();
                if behind {
                    covers = covers.min(self.model.log_durable());
                }
                let covers = version(covers);
                let sampled_at = if behind {
                    self.model.durable_items
                } else {
                    self.model.items.len()
                };
                let sample = self.sample(sampled_at);
                let end = log_end(self.model.items.len());
                // As if the items after the sample came into the log while
                // the chains were copied, each before the chain of its page
                // was: the log ends at the sample when the copy begins.
                let asked = Cell::new(false);
                let log_end = || {
                    if asked.replace(true) {
                        end
                    } else {
                        sample.place
                    }
                };
                let session = self.session.as_mut().expect("a session");
                let written =
                    session
                        .db
                        .checkpoints
                        .write(&session.table, covers, &sample, log_end)?;
                assert!(asked.get());
                let expected = self.model.checkpointed(covers, &sample);
                assert_eq!(written.covers(), covers);
                assert_eq!(written.sample(), &sample);
                assert_eq!(written.log_kept_from(), sample.place);
                assert_eq!(written.log_place_needed(), end);
                assert_eq!(written.links(), expected.links.len() as u64);
                assert_eq!(
                    written.reaches(),
                    expected.links.iter().map(|(_, link)| link.version).max()
                );
                session.written = Some(written.clone());
                let db = &mut session.db;
                let needed = written.log_needed();
                let durable = self.model.durable();
                if !behind {
                    self.model.durable_items = self.model.items.len();
                } else if needed > durable.version || end > durable.place {
                    let refused =
                        db.checkpoints
                            .install(&written, durable, &mut db.manifest, &db.segments);
                    let expected = if needed > durable.version {
                        PageTableError::LogBehind {
                            needed,
                            durable: durable.version,
                        }
                    } else {
                        PageTableError::LogBehindCopy {
                            needed: end.expect("a log with an item"),
                            durable: durable.place,
                        }
                    };
                    assert_eq!(
                        refused.map_err(|error| error.to_string()),
                        Err(expected.to_string())
                    );
                    self.model.sync_log(needed.get(), end);
                }
                self.model.pending = Some(expected);
                let durable = self.model.durable();
                db.checkpoints
                    .install(&written, durable, &mut db.manifest, &db.segments)?;
                session.written = None;
                self.model.installed();
            }
            Step::Abandon => {
                let covers = version(self.model.written_back());
                let sample = self.sample(self.model.items.len());
                let end = sample.place;
                let session = self.session.as_mut().expect("a session");
                let written =
                    session
                        .db
                        .checkpoints
                        .write(&session.table, covers, &sample, || end)?;
                session.written = Some(written.clone());
                session
                    .db
                    .checkpoints
                    .discard(written, &session.db.manifest)?;
                session.written = None;
            }
            Step::DropUnused => {
                let model = &mut self.model;
                let session = self.session.as_mut().expect("a session");
                let mut used = segments_of(&model.table.links());
                used.extend(
                    model
                        .acked
                        .iter()
                        .flat_map(|acked| segments_of(&acked.links)),
                );
                used.extend(session.writer.as_ref().map(|writer| writer.file_id().get()));
                let unused: Vec<u32> = model.segments.difference(&used).copied().collect();
                for raw_id in unused {
                    session.db.segments.drop_segment(id(raw_id))?;
                    model.segments.remove(&raw_id);
                }
                session.db.segments.commit(&mut session.db.manifest)?;
            }
            Step::Restart => {
                self.session = None;
                self.open()?;
            }
        }
        self.steps += 1;
        Ok(())
    }

    /// Takes the steps of `script`, until one fails.
    pub(super) fn script(&mut self, script: &[Step]) -> Result<(), PageTableError> {
        script.iter().try_for_each(|step| self.take(*step))
    }

    /// The next step of the drawn workload.
    fn draw(&mut self) -> Step {
        let raw_page = 1 + self.rng.below(6) as u32;
        match self.rng.below(100) {
            0..3 => Step::Segment,
            3..17 => Step::Base(raw_page),
            17..35 => Step::Delta(raw_page),
            35..44 => Step::Free(raw_page),
            44..48 => Step::Change(raw_page),
            48..51 => Step::Background(raw_page),
            51..54 => Step::WriteBack(raw_page),
            54..60 => Step::Consolidate(raw_page),
            60..65 => Step::Relocate(raw_page),
            65..77 => Step::Trim(raw_page),
            77..81 => Step::Sync,
            81..84 => Step::SyncLog,
            84..88 => Step::Checkpoint,
            88..92 => Step::CheckpointBehind,
            92..94 => Step::Abandon,
            94..97 => Step::DropUnused,
            _ => Step::Restart,
        }
    }

    /// Takes drawn steps until `steps` have been taken or one fails.
    pub(super) fn run(&mut self, steps: usize) -> Result<(), PageTableError> {
        while self.steps < steps {
            let step = self.draw();
            self.take(step)?;
        }
        Ok(())
    }

    /// After a step that failed while the disk failed and did not crash:
    /// does what a caller of the checkpoints does, so that the session can
    /// go on. A manifest in doubt is opened again and everyone is told, and
    /// a checkpoint that was written and is not installed is discarded.
    /// Returns `false` if the session cannot go on, because the failure was
    /// not one of the checkpoint's own.
    pub(super) fn settle(
        &mut self,
        step: Step,
        error: &PageTableError,
    ) -> Result<bool, PageTableError> {
        let Some(session) = self.session.as_mut() else {
            return Ok(false);
        };
        let of_a_segment = matches!(
            error,
            PageTableError::Store(SegmentStoreError::Io { path, .. })
                if path.starts_with(FileNames::SEGMENT.dir(db_dir()))
        );
        let of_a_checkpoint = matches!(
            step,
            Step::Checkpoint | Step::CheckpointBehind | Step::Abandon
        );
        if of_a_segment || !of_a_checkpoint {
            self.session = None;
            self.model.pending = None;
            return Ok(false);
        }
        let db = &mut session.db;
        if db.manifest.manifest().is_err() {
            db.manifest = ManifestStore::open(shared(&self.fs), db_dir())?;
        }
        let told = db.segments.manifest_replaced(&db.manifest);
        let removed = db.checkpoints.manifest_replaced(&db.manifest);
        let named = db.manifest.manifest()?.table_checkpoint();
        if let Some(written) = session.written.take() {
            if named == Some(written.named()) && self.model.pending.is_some() {
                self.model.installed();
            } else {
                db.checkpoints.discard(written, &db.manifest)?;
            }
        }
        self.model.pending = None;
        told?;
        removed?;
        Ok(true)
    }

    /// Checks the session against the model: the table holds the model's
    /// links, the manifest names the model's checkpoint, and `table/` holds
    /// that checkpoint's file and no other.
    pub(super) fn check_session(&self) {
        let session = self.session.as_ref().expect("a session");
        assert_eq!(all_links(&session.table), self.model.table.links());
        assert_eq!(session.table.links(), self.model.table.links().len() as u64);
        assert_eq!(session.table.pages(), self.model.table.chains.len() as u64);
        assert_eq!(session.table.trimmed_free(), self.model.table.trimmed_free);
        let named = session.db.named();
        assert_eq!(
            named.map(|named| named.covers()),
            self.model.acked.as_ref().map(|acked| acked.covers)
        );
        let files: Vec<String> = named
            .iter()
            .map(|named| FileNames::PAGE_TABLE.file_name(named.file()))
            .collect();
        assert_eq!(names_in(&self.fs, &table_dir()), files);
    }
}

/// Opens the database directory on `fs` as a new session does, and checks
/// what it finds against `model`: the checkpoint in force is the one of the
/// last installation that returned, or the one an installation was naming;
/// it loads; every block it points to is in a segment and holds what was
/// appended there; and `table/` holds no other checkpoint. Whichever
/// part of the log the crash leaves, from the durable items to all of them,
/// the checkpoint and the log after it give every page as those items left
/// it, with the page steps whose records are among them and no other. Then the new session writes a checkpoint of its own,
/// which a crash right after it leaves in force.
///
/// Returns whether the checkpoint found is the one that was being installed.
pub(super) fn check_recovered(fs: &SimVfs, model: &Model) -> bool {
    let mut db = match Db::open(fs) {
        Err(PageTableError::Store(SegmentStoreError::NoManifest { .. })) => {
            assert!(model.acked.is_none(), "the manifest is gone");
            return false;
        }
        db => db.expect("the directory opens"),
    };
    let (table, info) = db.load().expect("the checkpoint loads");
    let links = all_links(&table);
    let found = info.as_ref().map(|info| Checkpointed {
        covers: info.covers(),
        links: links.clone(),
        sample: info.sample().clone(),
    });
    let new = found != model.acked;
    if new {
        assert_eq!(
            found, model.pending,
            "neither the old checkpoint nor the new"
        );
    }
    let covers = found.as_ref().map_or(0, |found| found.covers.get());
    model.check_every_landing(&links, Found::of(info.as_ref()));

    let mut buf = Vec::new();
    for (page, link) in &links {
        let Some(location) = link.location() else {
            continue;
        };
        let block = db
            .segments
            .read(location, MAX_PAYLOAD, &mut buf)
            .unwrap_or_else(|error| panic!("{page:?} {link:?}: {error}"));
        let content = match link.kind {
            LinkKind::Delta(_) => BlockContent::Delta,
            _ => BlockContent::Base,
        };
        let appended = &model.blocks[&(location.file.get(), location.offset)];
        assert_eq!(
            (block.content(), &*block.payload().expect("a payload")),
            (content, appended.payload.as_slice()),
            "{page:?} {link:?}"
        );
        assert_eq!(appended.content, content);
    }

    let named = db.named();
    let files: Vec<String> = named
        .iter()
        .map(|named| FileNames::PAGE_TABLE.file_name(named.file()))
        .collect();
    assert_eq!(names_in(fs, &table_dir()), files);

    let next = db
        .checkpoint(&table, covers + 1)
        .expect("the next checkpoint is installed");
    assert!(named.is_none_or(|named| named.file() < next.file()));
    let image = fs.crash_image();
    let durable = image
        .recover(&image.lose_all())
        .expect("a plan of the image");
    let after = Db::open(&durable).expect("the directory opens after the next crash");
    let (again, info) = after.load().expect("the next checkpoint loads");
    assert_eq!(info, Some(next));
    assert_eq!(all_links(&again), links);
    new
}
