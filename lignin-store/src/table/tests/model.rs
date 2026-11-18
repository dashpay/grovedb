//! A model of the page table and of its checkpoints, and a workload that
//! drives a table, its checkpoints and the model together.
//!
//! The model is written from the rules, not from the table: a chain is a
//! list of links, and a checkpoint is a copy of every list. A run stops at
//! the first error, which is how a power loss ends it, and what is then on
//! the file system is checked against the model by [`check_recovered`].

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::segment::{BlockContent, Compression};
use lignin_io::sim::SimVfs;

use super::*;
use crate::segment::{
    tests::sim::{db_dir, shared},
    SegmentWriter,
};

const MAX_PAYLOAD: usize = 1 << 16;

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
        let removed = chain
            .drain(..keep_from)
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

/// What must be true of a database directory.
#[derive(Clone, Debug, Default)]
pub(super) struct Model {
    pub(super) table: ModelTable,
    /// The last commit version.
    pub(super) version: u64,
    /// The content and the payload of every block appended, by its segment
    /// and offset.
    blocks: BTreeMap<(u32, u64), (BlockContent, Vec<u8>)>,
    /// The segments in the store.
    segments: BTreeSet<u32>,
    /// The checkpoint of the last installation that returned, if any.
    pub(super) acked: Option<Checkpointed>,
    /// The checkpoint an installation in progress is naming.
    pub(super) pending: Option<Checkpointed>,
}

impl Model {
    fn checkpointed(&self) -> Checkpointed {
        Checkpointed {
            covers: version(self.version),
            links: self.table.links(),
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
    /// The chain of the page is consolidated into a base image.
    Consolidate(u32),
    /// The oldest block of the page is copied and its link switched.
    Relocate(u32),
    /// The chain of the page is trimmed to what the latest version needs.
    Trim(u32),
    /// The segments are synced and the manifest names them.
    Sync,
    /// A checkpoint is written and installed.
    Checkpoint,
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

    /// Goes on after a crash, on the file system `fs` that the crash left.
    pub(super) fn crashed(&mut self, fs: &SimVfs, new: bool) {
        self.session = None;
        self.fs = fs.clone();
        if new {
            self.model.installed();
        }
        self.model.pending = None;
    }

    /// Ends the session, as a process that exits does.
    pub(super) fn end_session(&mut self) {
        self.session = None;
    }

    /// Starts a session: opens the directory, or creates its first manifest,
    /// and loads the checkpoint in force, which must be the one the model
    /// has.
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
        let found = info.as_ref().map(|info| Checkpointed {
            covers: info.covers(),
            links: all_links(&table),
        });
        if model.pending.is_some() && found == model.pending {
            model.installed();
        }
        model.pending = None;
        assert_eq!(found, model.acked);
        model.table = ModelTable::of(model.acked.as_ref().map_or(&[], |acked| &acked.links));
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
    ) -> Result<BlockLocation, PageTableError> {
        if self.session().writer.is_none() {
            self.new_segment()?;
        }
        let writer = self.session().writer.as_mut().expect("a writer");
        let location = writer.append(page(raw_page), content, Compression::Raw, &payload)?;
        self.model
            .blocks
            .insert((location.file.get(), location.offset), (content, payload));
        Ok(location)
    }

    fn payload(&mut self) -> Vec<u8> {
        let len = self.rng.below(90) as usize;
        (0..len).map(|_| self.rng.next() as u8).collect()
    }

    fn commit(&mut self, raw_page: u32, link: Link) {
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
                let live =
                    newest(&self.model, raw_page).is_some_and(|link| link.kind != LinkKind::Free);
                let content = if live && matches!(step, Step::Delta(_)) {
                    BlockContent::Delta
                } else {
                    BlockContent::Base
                };
                let payload = self.payload();
                let location = self.append(raw_page, content, payload)?;
                self.model.version += 1;
                let born = version(self.model.version);
                let link = match content {
                    BlockContent::Base => Link::base(born, location),
                    BlockContent::Delta => Link::delta(born, location),
                };
                self.commit(raw_page, link);
            }
            Step::Free(raw_page) => {
                if newest(&self.model, raw_page).is_some_and(|link| link.kind != LinkKind::Free) {
                    self.model.version += 1;
                    self.commit(raw_page, Link::free(version(self.model.version)));
                }
            }
            Step::Consolidate(raw_page) => {
                let through = newest(&self.model, raw_page).and_then(|link| link.location());
                if let Some(through) = through {
                    let payload = self.payload();
                    let image = self.append(raw_page, BlockContent::Base, payload)?;
                    let table = &self.session.as_ref().expect("a session").table;
                    let born = table.consolidate(page(raw_page), through, image);
                    assert_eq!(born, self.model.table.consolidate(raw_page, through, image));
                    born.expect("the newest link is in the chain");
                }
            }
            Step::Relocate(raw_page) => {
                let from = self.model.table.chain(raw_page).first().copied();
                if let Some(from) = from.and_then(|link| link.location()) {
                    let (content, payload) =
                        self.model.blocks[&(from.file.get(), from.offset)].clone();
                    let to = self.append(raw_page, content, payload)?;
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
            Step::Checkpoint => {
                let covers = version(self.model.version);
                let session = self.session.as_mut().expect("a session");
                let written = session.db.checkpoints.write(&session.table, covers)?;
                let expected = self.model.checkpointed();
                assert_eq!(written.covers(), covers);
                assert_eq!(written.links(), expected.links.len() as u64);
                assert_eq!(
                    written.reaches(),
                    expected.links.iter().map(|(_, link)| link.version).max()
                );
                self.model.pending = Some(expected);
                session.written = Some(written.clone());
                let db = &mut session.db;
                db.checkpoints
                    .install(&written, &mut db.manifest, &db.segments)?;
                session.written = None;
                self.model.installed();
            }
            Step::Abandon => {
                let covers = version(self.model.version);
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
            0..4 => Step::Segment,
            4..22 => Step::Base(raw_page),
            22..46 => Step::Delta(raw_page),
            46..52 => Step::Free(raw_page),
            52..60 => Step::Consolidate(raw_page),
            60..67 => Step::Relocate(raw_page),
            67..77 => Step::Trim(raw_page),
            77..83 => Step::Sync,
            83..91 => Step::Checkpoint,
            91..93 => Step::Abandon,
            93..97 => Step::DropUnused,
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
        if of_a_segment || !matches!(step, Step::Checkpoint | Step::Abandon) {
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
/// appended there; and `table/` holds no other checkpoint. Then the new
/// session writes a checkpoint of its own, which a crash right after it
/// leaves in force.
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
            (content, appended.1.as_slice()),
            "{page:?} {link:?}"
        );
        assert_eq!(appended.0, content);
    }

    let named = db.named();
    let files: Vec<String> = named
        .iter()
        .map(|named| FileNames::PAGE_TABLE.file_name(named.file()))
        .collect();
    assert_eq!(names_in(fs, &table_dir()), files);

    let covers = found.as_ref().map_or(0, |found| found.covers.get());
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
