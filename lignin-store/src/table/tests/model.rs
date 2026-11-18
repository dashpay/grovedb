//! A model of the page table and of its checkpoints, and a workload that
//! drives a table, its checkpoints and the model together.
//!
//! The model is written from the rules, not from the table: a chain is a
//! list of links, and a checkpoint is a copy of every list. A run stops at
//! the first error, which is how a power loss ends it, and what is then on
//! the file system is checked against the model by [`check_recovered`].
//!
//! The model also keeps what the table is never told: the log, which is
//! what each commit did to its page, the version the log is durable
//! through, and the commits each block holds. A crash can land on any
//! version from the durable one to the last, and for each of them the model
//! works out what recovery has of every page: the chain of the checkpoint,
//! and the log replayed on it.

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
}

thread_local! {
    pub(super) static RECOVERIES: Cell<Recoveries> = const {
        Cell::new(Recoveries { pages: 0, forgotten: 0, dropped: 0 })
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
}

/// What a commit did to its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// It made the page, with its first content.
    Created,
    /// It changed the page.
    Changed,
    /// It freed the page.
    Freed,
}

/// A page as a commit left it: the commits that made it up since it was
/// made, or `None` for a page that is not in use.
type Page = Option<Vec<u64>>;

/// Applies the change of commit `version` to `page`, as it is logged.
/// Returns `false` for a change to a page that is not there.
fn apply(page: &mut Page, version: u64, change: Change) -> bool {
    match (change, page.as_mut()) {
        (Change::Created, _) => *page = Some(vec![version]),
        (Change::Changed, Some(commits)) => commits.push(version),
        (Change::Changed, None) => return false,
        (Change::Freed, _) => *page = None,
    }
    true
}

/// One block that was appended to a segment.
#[derive(Clone, Debug)]
struct Block {
    content: BlockContent,
    payload: Vec<u8>,
    /// The commits whose changes the block holds: all that make up the
    /// page, for a base image, and those of the run, for a delta run.
    holds: Vec<u64>,
}

/// What must be true of a database directory.
#[derive(Clone, Debug, Default)]
pub(super) struct Model {
    pub(super) table: ModelTable,
    /// The last commit version.
    pub(super) version: u64,
    /// The version the log is durable through. A crash lands on it or on
    /// any later version, up to the last.
    pub(super) log_durable: u64,
    /// The log: what each commit did, by its page, in order.
    log: BTreeMap<u32, Vec<(u64, Change)>>,
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
    fn checkpointed(&self, covers: CommitVersion) -> Checkpointed {
        Checkpointed {
            covers,
            links: self.table.links(),
        }
    }

    /// Takes the next version for a commit that does `change` to
    /// `raw_page`, and logs it.
    fn commit(&mut self, raw_page: u32, change: Change) -> u64 {
        self.version += 1;
        let logged = (self.version, change);
        self.log.entry(raw_page).or_default().push(logged);
        self.version
    }

    fn logged(&self, raw_page: u32) -> &[(u64, Change)] {
        self.log.get(&raw_page).map_or(&[], Vec::as_slice)
    }

    /// The page as the commit of version `at` left it.
    fn page_at(&self, raw_page: u32, at: u64) -> Page {
        let mut page = None;
        for (version, change) in self.logged(raw_page) {
            if *version <= at {
                apply(&mut page, *version, *change);
            }
        }
        page
    }

    fn is_live(&self, raw_page: u32) -> bool {
        self.page_at(raw_page, self.version).is_some()
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

    /// What a recovery has of `raw_page` when it loads a checkpoint that
    /// covers `covers` and holds `chain`, and lands on `landing`: what the
    /// chain gives, and on it the logged changes after `covers` and after
    /// the chain's newest link.
    fn recovered(&self, raw_page: u32, chain: &[Link], covers: u64, landing: u64) -> Page {
        let newest = chain.last().map(|link| link.version.get());
        let mut page = self.chain_holds(chain);
        if let Some(newest) = newest {
            assert!(
                newest <= landing,
                "page {raw_page} has a link of version {newest}, and recovery lands on {landing}"
            );
            assert_eq!(
                page,
                self.page_at(raw_page, newest),
                "the chain of page {raw_page} at version {newest}"
            );
        }
        let from = newest.map_or(covers, |newest| newest.max(covers));
        let (mut whole, mut dropped) = (true, false);
        for (version, change) in self.logged(raw_page) {
            if from < *version && *version <= landing {
                whole = apply(&mut page, *version, *change);
                dropped |= !whole;
            }
        }
        let forgotten = chain.is_empty() && self.page_at(raw_page, covers).is_some();
        RECOVERIES.with(|met| {
            let mut recoveries = met.get();
            recoveries.pages += 1;
            recoveries.forgotten += usize::from(forgotten);
            recoveries.dropped += usize::from(dropped);
            met.set(recoveries);
        });
        assert!(
            whole,
            "replay up to {landing} leaves page {raw_page} changed and without an image"
        );
        page
    }

    /// Checks that a recovery that loads `links`, as a checkpoint that
    /// covers `covers`, and lands on `landing` has every page as the commit
    /// of `landing` left it: none that is in use is missing, and none that
    /// is free is there.
    fn check_landing(&self, links: &[(PageId, Link)], covers: u64, landing: u64) {
        let table = ModelTable::of(links);
        let pages: BTreeSet<u32> = table
            .chains
            .keys()
            .chain(self.log.keys())
            .copied()
            .collect();
        for raw_page in pages {
            assert_eq!(
                self.recovered(raw_page, table.chain(raw_page), covers, landing),
                self.page_at(raw_page, landing),
                "page {raw_page} at version {landing}, from a checkpoint that covers {covers}"
            );
        }
    }

    /// Checks every version a crash can land on now.
    fn check_every_landing(&self, links: &[(PageId, Link)], covers: u64) {
        for landing in self.log_durable..=self.version {
            self.check_landing(links, covers, landing);
        }
    }

    /// A crash lost the commits after `landing`.
    fn land_on(&mut self, landing: u64) {
        assert!((self.log_durable..=self.version).contains(&landing));
        self.version = landing;
        for commits in self.log.values_mut() {
            commits.retain(|(version, _)| *version <= landing);
        }
    }

    fn installed(&mut self) {
        self.acked = Some(self.pending.take().expect("an installation in progress"));
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
    /// through. The log is synced only as far as the checkpoint needs, and
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
    /// from the commit version `landing`.
    pub(super) fn crashed(&mut self, fs: &SimVfs, new: bool, landing: u64) {
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
        let covers = info.as_ref().map_or(0, |info| info.covers().get());
        let found = info.as_ref().map(|info| Checkpointed {
            covers: info.covers(),
            links: links.clone(),
        });
        if model.pending.is_some() && found == model.pending {
            model.installed();
        }
        model.pending = None;
        assert_eq!(found, model.acked);
        assert_eq!(info.and_then(|info| info.trimmed_free()), None);
        model.check_landing(&links, covers, model.version);
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
        let replayed = self.replay(covers);
        if replayed.is_err() {
            self.session = None;
        }
        replayed
    }

    /// Does what recovery and the write-back after it do: every page with a
    /// commit after `covers` that its chain does not hold gets a base image
    /// of what it is now, or its free link.
    fn replay(&mut self, covers: u64) -> Result<(), PageTableError> {
        let pages: Vec<u32> = self.model.log.keys().copied().collect();
        for raw_page in pages {
            let newest = self.model.table.chain(raw_page).last().copied();
            let from = newest.map_or(covers, |link| link.version.get().max(covers));
            let Some(&(last, _)) = self.model.logged(raw_page).last() else {
                continue;
            };
            if last <= from {
                continue;
            }
            match self.model.page_at(raw_page, last) {
                Some(commits) => {
                    let payload = self.payload();
                    let image = self.append(raw_page, BlockContent::Base, payload, commits)?;
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
                    commits = self.model.page_at(raw_page, born).expect("a page in use");
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
            Step::SyncLog => self.model.log_durable = self.model.version,
            Step::Checkpoint | Step::CheckpointBehind => {
                let behind = step == Step::CheckpointBehind;
                let mut covers = self.model.written_back();
                if behind {
                    covers = covers.min(self.model.log_durable);
                }
                let covers = version(covers);
                let session = self.session.as_mut().expect("a session");
                let written = session.db.checkpoints.write(&session.table, covers)?;
                let expected = self.model.checkpointed(covers);
                assert_eq!(written.covers(), covers);
                assert_eq!(written.links(), expected.links.len() as u64);
                assert_eq!(
                    written.reaches(),
                    expected.links.iter().map(|(_, link)| link.version).max()
                );
                session.written = Some(written.clone());
                let db = &mut session.db;
                let needed = written.log_needed();
                if !behind {
                    self.model.log_durable = self.model.version;
                } else if needed.get() > self.model.log_durable {
                    let durable = version(self.model.log_durable);
                    let refused =
                        db.checkpoints
                            .install(&written, durable, &mut db.manifest, &db.segments);
                    assert!(
                        matches!(
                            refused,
                            Err(PageTableError::LogBehind { needed: asked, durable: found })
                                if asked == needed && found == durable
                        ),
                        "{refused:?}"
                    );
                    self.model.log_durable = needed.get();
                }
                self.model.pending = Some(expected);
                let durable = version(self.model.log_durable);
                db.checkpoints
                    .install(&written, durable, &mut db.manifest, &db.segments)?;
                session.written = None;
                self.model.installed();
            }
            Step::Abandon => {
                let covers = version(self.model.written_back());
                let session = self.session.as_mut().expect("a session");
                let written = session.db.checkpoints.write(&session.table, covers)?;
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
            44..50 => Step::Change(raw_page),
            50..54 => Step::WriteBack(raw_page),
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
/// version the recovery lands on, from the one the log is durable through
/// to the last, the checkpoint and the log after it give every page as that
/// version has it. Then the new session writes a checkpoint of its own,
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
    });
    let new = found != model.acked;
    if new {
        assert_eq!(
            found, model.pending,
            "neither the old checkpoint nor the new"
        );
    }
    let covers = found.as_ref().map_or(0, |found| found.covers.get());
    model.check_every_landing(&links, covers);

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
