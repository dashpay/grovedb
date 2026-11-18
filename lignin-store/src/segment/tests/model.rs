//! A model of what the segment store must do, and a seeded workload that
//! drives a store and the model together.
//!
//! The model is written from the rules, not from the store: it holds the
//! bytes every segment must have, how much of each is durable, and the
//! manifest that must be on disk. A run stops at the first error, which is
//! how a power loss ends it, and what is then on the file system is checked
//! against the model by [`check_recovered`].

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::segment::{write_block, write_footer, IndexEntry, DATA_START};
use lignin_io::OpenMode;

use super::*;

pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> Option<T> {
        (!from.is_empty()).then(|| from[self.below(from.len())])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Created,
    Open,
    Closed,
    Sealed,
}

#[derive(Clone, Debug)]
struct SegmentModel {
    phase: Phase,
    blocks: Vec<Appended>,
    index: Vec<IndexEntry>,
    bytes: Vec<u8>,
    blocks_end: u64,
    durable_len: u64,
    has_writer: bool,
}

impl SegmentModel {
    fn created(raw_id: u32) -> Self {
        Self {
            phase: Phase::Created,
            blocks: Vec::new(),
            index: Vec::new(),
            bytes: header(FileKind::Segment, raw_id).to_vec(),
            blocks_end: DATA_START,
            durable_len: DATA_START,
            has_writer: true,
        }
    }

    fn named(&self, raw_id: u32) -> LiveFile {
        let state = match self.phase {
            Phase::Created | Phase::Open => FileState::Open,
            Phase::Closed => FileState::Closed,
            Phase::Sealed => FileState::Sealed,
        };
        segment(raw_id, state, self.durable_len)
    }

    fn blocks_below(&self, len: u64) -> &[Appended] {
        let count = self
            .blocks
            .iter()
            .take_while(|block| block.location.offset + u64::from(block.location.len) <= len)
            .count();
        &self.blocks[..count]
    }
}

/// What must be true of a database directory.
#[derive(Clone, Debug)]
pub(super) struct Model {
    /// The manifest of the last replacement that returned, or `None` before
    /// the first.
    pub(super) acked: Option<Manifest>,
    /// The manifest a replacement in progress is installing.
    pub(super) pending: Option<Manifest>,
    live: BTreeMap<u32, SegmentModel>,
    dropped: BTreeMap<u32, SegmentModel>,
    last_id: u32,
}

impl Model {
    fn new() -> Self {
        Self {
            acked: None,
            pending: None,
            live: BTreeMap::new(),
            dropped: BTreeMap::new(),
            last_id: 0,
        }
    }

    fn acked(&self) -> &Manifest {
        self.acked.as_ref().expect("a first manifest")
    }

    fn segment(&self, id: u32) -> &SegmentModel {
        self.live
            .get(&id)
            .or_else(|| self.dropped.get(&id))
            .expect("a named segment is known")
    }

    fn describe(&self) -> Manifest {
        let base = self.acked();
        let mut files: Vec<_> = base
            .files()
            .iter()
            .filter(|file| file.kind() != FileKind::Segment)
            .copied()
            .collect();
        files.extend(self.live.iter().map(|(id, segment)| segment.named(*id)));
        let mut last = base.last_file_ids();
        last.segment = last.segment.max(FileId::new(self.last_id));
        Manifest::new(last, base.table_checkpoint(), files).expect("a valid manifest")
    }

    fn replaced(&mut self) {
        let acked = self.pending.take().expect("a replacement in progress");
        let named: BTreeSet<u32> = segments_of(&acked).map(|file| file.id().get()).collect();
        for (id, segment) in &mut self.live {
            if segment.phase == Phase::Created && named.contains(id) {
                segment.phase = Phase::Open;
            }
        }
        self.dropped.retain(|id, _| named.contains(id));
        self.acked = Some(acked);
    }

    fn restarted(&mut self) {
        let acked = self.acked().clone();
        let mut live = BTreeMap::new();
        for named in segments_of(&acked) {
            let id = named.id().get();
            let mut segment = self
                .live
                .remove(&id)
                .or_else(|| self.dropped.remove(&id))
                .expect("a named segment is known");
            let valid_len = named.valid_len();
            if named.state() == FileState::Sealed {
                segment.phase = Phase::Sealed;
            } else {
                segment.phase = Phase::Closed;
                let kept = segment.blocks_below(valid_len).len();
                segment.blocks.truncate(kept);
                segment.index.truncate(segment.blocks.len());
                segment.blocks_end = valid_len;
            }
            segment.durable_len = valid_len;
            segment.has_writer = false;
            live.insert(id, segment);
        }
        self.live = live;
        self.dropped.clear();
        self.last_id = acked.last_file_ids().segment.map_or(0, FileId::get);
    }
}

fn segments_of(manifest: &Manifest) -> impl Iterator<Item = &LiveFile> {
    manifest
        .files()
        .iter()
        .filter(|file| file.kind() == FileKind::Segment)
}

fn first_manifest() -> Manifest {
    manifest_of(
        [3, 0, 2, 1],
        Some(table(1, 100, 5)),
        vec![
            live(FileKind::Wal, 3, FileState::Open, 20),
            live(FileKind::ValueLog, 2, FileState::Closed, 64),
        ],
    )
}

fn with_another_log(manifest: &Manifest) -> Manifest {
    let mut last = manifest.last_file_ids();
    let next = last.wal.map_or(1, |id| id.get() + 1);
    last.wal = FileId::new(next);
    let mut files: Vec<_> = manifest
        .files()
        .iter()
        .map(|file| match (file.kind(), file.state()) {
            (FileKind::Wal, FileState::Open) => {
                live(FileKind::Wal, file.id().get(), FileState::Closed, 20)
            }
            _ => *file,
        })
        .collect();
    files.push(live(FileKind::Wal, next, FileState::Open, 20));
    Manifest::new(last, manifest.table_checkpoint(), files).expect("a valid manifest")
}

#[derive(Clone, Copy)]
enum Step {
    Create,
    Commit,
    AnotherLog,
    Append(u32),
    SyncOne(u32),
    SyncAll,
    Seal(u32),
    DropWriter(u32),
    DropSegment(u32),
    Restart,
}

struct Session {
    manifest: ManifestStore,
    segments: SegmentStore,
    writers: BTreeMap<u32, SegmentWriter>,
}

/// One run of the workload on one file system.
pub(super) struct Run {
    pub(super) model: Model,
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    rng: Rng,
    session: Option<Session>,
    pub(super) steps: usize,
    /// How often each kind of step was taken.
    pub(super) taken: BTreeMap<&'static str, usize>,
}

impl Run {
    pub(super) fn new(vfs: Arc<dyn Vfs>, dir: &Path, seed: u64) -> Self {
        Self {
            model: Model::new(),
            vfs,
            dir: dir.to_path_buf(),
            rng: Rng::new(seed),
            session: None,
            steps: 0,
            taken: BTreeMap::new(),
        }
    }

    /// Takes steps until `steps` have been taken or one fails.
    pub(super) fn run(&mut self, steps: usize) -> Result<(), SegmentStoreError> {
        while self.steps < steps {
            self.step()?;
            self.steps += 1;
        }
        Ok(())
    }

    fn step(&mut self) -> Result<(), SegmentStoreError> {
        let kind = self.take_step()?;
        *self.taken.entry(kind).or_default() += 1;
        Ok(())
    }

    fn take_step(&mut self) -> Result<&'static str, SegmentStoreError> {
        let Some(session) = &mut self.session else {
            self.open()?;
            return Ok("open");
        };
        let (model, rng) = (&mut self.model, &mut self.rng);
        let writable: Vec<u32> = session
            .writers
            .keys()
            .copied()
            .filter(|id| model.live[id].phase == Phase::Open)
            .collect();
        let idle: Vec<u32> = model
            .live
            .iter()
            .filter(|(_, segment)| !segment.has_writer)
            .map(|(id, _)| *id)
            .collect();
        let writers: Vec<u32> = session.writers.keys().copied().collect();
        let otherwise = if writers.len() < 3 {
            Step::Create
        } else {
            Step::Commit
        };
        let step = match rng.below(100) {
            0..8 => otherwise,
            8..24 => Step::Commit,
            24..29 => Step::AnotherLog,
            29..70 => rng.pick(&writable).map_or(otherwise, Step::Append),
            70..76 => rng.pick(&writable).map_or(otherwise, Step::SyncOne),
            76..81 => Step::SyncAll,
            81..87 => rng.pick(&writable).map_or(otherwise, Step::Seal),
            87..91 => rng.pick(&writers).map_or(otherwise, Step::DropWriter),
            91..96 => rng.pick(&idle).map_or(otherwise, Step::DropSegment),
            _ => Step::Restart,
        };

        match step {
            Step::Create => {
                let writer = session.segments.create()?;
                let id = writer.file_id().get();
                assert_eq!(id, model.last_id + 1);
                model.last_id = id;
                model.live.insert(id, SegmentModel::created(id));
                session.writers.insert(id, writer);
                Ok("create")
            }
            Step::Commit => {
                model.pending = Some(model.describe());
                session.segments.commit(&mut session.manifest)?;
                model.replaced();
                Ok("commit")
            }
            Step::AnotherLog => {
                model.pending = Some(with_another_log(model.acked()));
                session
                    .manifest
                    .update(|current| Ok::<_, SegmentStoreError>(with_another_log(current)))?;
                session.segments.manifest_replaced(&session.manifest)?;
                model.replaced();
                Ok("another log")
            }
            Step::Append(id) => {
                for _ in 0..1 + rng.below(4) {
                    append(session, model, rng, id)?;
                }
                Ok("append")
            }
            Step::SyncOne(id) => {
                let synced = session.writers.get_mut(&id).expect("a writer").sync()?;
                let segment = model.live.get_mut(&id).expect("a segment");
                assert_eq!(synced, segment.blocks_end);
                segment.durable_len = synced;
                Ok("sync a segment")
            }
            Step::SyncAll => {
                session.segments.sync()?;
                for segment in model.live.values_mut() {
                    if matches!(segment.phase, Phase::Created | Phase::Open) {
                        segment.durable_len = segment.blocks_end;
                    }
                }
                Ok("sync all")
            }
            Step::Seal(id) => {
                let sealed_len = session.writers.get_mut(&id).expect("a writer").seal()?;
                let segment = model.live.get_mut(&id).expect("a segment");
                write_footer(&segment.index, segment.blocks_end, &mut segment.bytes)
                    .expect("a footer");
                assert_eq!(sealed_len, segment.bytes.len() as u64);
                segment.phase = Phase::Sealed;
                segment.durable_len = sealed_len;
                Ok("seal")
            }
            Step::DropWriter(id) => {
                session.writers.remove(&id);
                model.live.get_mut(&id).expect("a segment").has_writer = false;
                Ok("drop a writer")
            }
            Step::DropSegment(id) => {
                session.segments.drop_segment(super::id(id))?;
                let segment = model.live.remove(&id).expect("a segment");
                model.dropped.insert(id, segment);
                Ok("drop a segment")
            }
            Step::Restart => {
                self.session = None;
                self.model.restarted();
                self.open()?;
                Ok("restart")
            }
        }
    }

    fn open(&mut self) -> Result<(), SegmentStoreError> {
        let manifest = if self.model.acked.is_none() {
            self.model.pending = Some(first_manifest());
            let manifest = ManifestStore::create(self.vfs.clone(), &self.dir, first_manifest())?;
            self.model.replaced();
            manifest
        } else {
            ManifestStore::open(self.vfs.clone(), &self.dir)?
        };
        let segments = SegmentStore::open(&manifest)?;
        self.session = Some(Session {
            manifest,
            segments,
            writers: BTreeMap::new(),
        });
        Ok(())
    }

    /// Checks the running store against the model.
    pub(super) fn check_session(&self) {
        let Some(session) = &self.session else {
            return;
        };
        let model = &self.model;
        let named: Vec<_> = model
            .live
            .iter()
            .map(|(id, segment)| segment.named(*id))
            .collect();
        assert_eq!(session.segments.segments_named().unwrap(), named);
        assert_eq!(session.manifest.manifest().unwrap(), model.acked());
        assert_eq!(
            &read_manifest(&*self.vfs, &self.dir).unwrap(),
            model.acked()
        );
        for (id, segment) in &model.live {
            check_blocks(&session.segments, *id, segment, segment.blocks_end);
        }
        for id in model.dropped.keys() {
            let gone = session.segments.walk(super::id(*id), MAX);
            assert!(matches!(
                gone.err().unwrap(),
                SegmentStoreError::UnknownSegment { .. }
            ));
        }
    }
}

fn append(
    session: &mut Session,
    model: &mut Model,
    rng: &mut Rng,
    id: u32,
) -> Result<(), SegmentStoreError> {
    let len = [0, 1, 9, 60, 200, 700, 3000][rng.below(7)];
    let payload = if rng.below(2) == 0 {
        random(len, rng.next())
    } else {
        repetitive(len, rng.next())
    };
    let content = [BlockContent::Base, BlockContent::Delta][rng.below(2)];
    let compression = [Compression::Raw, Compression::Lz4][rng.below(2)];
    let page = page(1 + rng.below(20) as u32);

    let writer = session.writers.get_mut(&id).expect("a writer");
    let location = writer.append(page, content, compression, &payload)?;

    let segment = model.live.get_mut(&id).expect("a segment");
    let offset = segment.bytes.len() as u64;
    let tag = write_block(content, compression, &payload, &mut segment.bytes).expect("a block");
    let expected = BlockLocation {
        file: super::id(id),
        offset,
        len: tag.block_len() as u32,
    };
    assert_eq!(location, expected);
    segment.index.push(IndexEntry { page, tag });
    segment.blocks_end = segment.bytes.len() as u64;
    segment.blocks.push(Appended {
        location,
        page,
        content,
        payload,
    });
    Ok(())
}

fn check_blocks(segments: &SegmentStore, raw_id: u32, model: &SegmentModel, readable: u64) {
    let expected = model.blocks_below(readable);
    let mut walk = segments.walk(id(raw_id), MAX).unwrap();
    let mut buf = Vec::new();
    for block in expected {
        let (location, walked) = walk.next_block().unwrap().expect("another block");
        assert_eq!(location, block.location);
        assert_eq!(walked.content(), block.content);
        assert_eq!(walked.payload().unwrap(), block.payload.as_slice());
        let read = segments.read(block.location, MAX, &mut buf).unwrap();
        assert_eq!(read.payload().unwrap(), block.payload.as_slice());
    }
    assert!(walk.next_block().unwrap().is_none());
    for block in &model.blocks[expected.len()..] {
        assert!(matches!(
            segments.read(block.location, MAX, &mut buf).unwrap_err(),
            SegmentStoreError::OutOfBounds { .. }
        ));
    }
    if model.phase == Phase::Sealed {
        let index = segments.index(id(raw_id), &mut buf).unwrap();
        let listed: Vec<_> = index
            .blocks()
            .map(|block| (block.page, block.offset))
            .collect();
        let appended: Vec<_> = model
            .blocks
            .iter()
            .map(|block| (block.page, block.location.offset))
            .collect();
        assert_eq!(listed, appended);
    }
}

fn read_file(vfs: &dyn Vfs, path: &Path) -> Option<Vec<u8>> {
    let file = vfs.open(path, OpenMode::Read).ok()?;
    let mut bytes = vec![0; file.len().unwrap() as usize];
    file.read_exact_at(&mut bytes, 0).unwrap();
    Some(bytes)
}

fn file_names(vfs: &dyn Vfs, dir: &Path) -> Vec<String> {
    vfs.read_dir(dir)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name.to_string_lossy().into_owned())
        .collect()
}

/// Checks what a stopped run left on `vfs`, which is the file system the run
/// was on or one a crash made of it, and then that the database can be used
/// again. Returns which manifest it found: `false` for the last one a
/// replacement returned, `true` for the one that was being installed.
pub(super) fn check_recovered(vfs: Arc<dyn Vfs>, dir: &Path, model: &Model) -> bool {
    for (id, segment) in &model.live {
        let path = FileNames::SEGMENT.path(dir, super::id(*id));
        let bytes = read_file(&*vfs, &path).expect("a created segment keeps its name");
        let durable = segment.durable_len as usize;
        assert!(bytes.len() >= durable, "segment {id} lost synced bytes");
        assert!(
            bytes[..durable] == segment.bytes[..durable],
            "segment {id} has other bytes than the ones synced"
        );
    }

    let manifest = match ManifestStore::open(vfs.clone(), dir) {
        Ok(manifest) => manifest,
        Err(SegmentStoreError::NoManifest { .. }) => {
            assert!(model.acked.is_none(), "the manifest is gone");
            let created = ManifestStore::create(vfs.clone(), dir, first_manifest());
            assert_eq!(created.unwrap().manifest().unwrap(), &first_manifest());
            return false;
        }
        Err(error) => panic!("the manifest does not open: {error}"),
    };
    let found = manifest.manifest().unwrap().clone();
    let is_pending = model.pending.as_ref() == Some(&found);
    assert!(
        is_pending || model.acked.as_ref() == Some(&found),
        "the manifest is neither the old one nor the new one: {found:?}"
    );
    assert!(!file_names(&*vfs, dir).contains(&ManifestStore::TEMP_FILE_NAME.to_owned()));

    let segments = SegmentStore::open(&manifest).expect("the segments open");
    let mut named = Vec::new();
    for file in segments_of(&found) {
        let id = file.id().get();
        let segment = model.segment(id);
        let bytes = read_file(&*vfs, &FileNames::SEGMENT.path(dir, file.id())).unwrap();
        let valid = file.valid_len() as usize;
        assert!(bytes[..valid] == segment.bytes[..valid]);
        let adopted = SegmentModel {
            phase: if file.state() == FileState::Sealed {
                Phase::Sealed
            } else {
                Phase::Closed
            },
            ..segment.clone()
        };
        check_blocks(&segments, id, &adopted, file.valid_len());
        named.push(FileNames::SEGMENT.file_name(file.id()));
    }
    assert_eq!(file_names(&*vfs, &FileNames::SEGMENT.dir(dir)), named);

    go_on(manifest, segments, &found);
    let again = ManifestStore::open(vfs, dir).expect("the manifest opens again");
    SegmentStore::open(&again).expect("the segments open again");
    is_pending
}

fn go_on(mut manifest: ManifestStore, segments: SegmentStore, found: &Manifest) {
    let mut writer = segments.create().expect("a new segment");
    let next_id = found.last_file_ids().segment.map_or(1, |id| id.get() + 1);
    assert_eq!(writer.file_id(), id(next_id));
    segments
        .commit(&mut manifest)
        .expect("the manifest names it");
    let blocks = append_blocks(&mut writer, 5, u64::from(next_id));
    let sealed_len = writer.seal().expect("the segment is sealed");
    segments
        .commit(&mut manifest)
        .expect("the manifest names it sealed");

    let named = manifest.manifest().unwrap();
    assert!(named
        .files()
        .contains(&segment(next_id, FileState::Sealed, sealed_len)));
    assert!(segments_of(named)
        .all(|file| file.id().get() == next_id || file.state() != FileState::Open));
    let mut buf = Vec::new();
    for block in &blocks {
        let read = segments.read(block.location, MAX, &mut buf).unwrap();
        assert_eq!(read.payload().unwrap(), block.payload.as_slice());
    }
}

pub(super) const STEP_KINDS: [&str; 11] = [
    "open",
    "create",
    "commit",
    "another log",
    "append",
    "sync a segment",
    "sync all",
    "seal",
    "drop a writer",
    "drop a segment",
    "restart",
];

#[test]
fn a_random_workload_with_restarts_keeps_to_the_model() {
    let mut taken = BTreeMap::new();
    for seed in 0..12 {
        let dir = tempfile::tempdir().unwrap();
        let vfs: Arc<dyn Vfs> = Arc::new(FaultyVfs::without_syncs(Arc::new(StdVfs)));
        let mut run = Run::new(vfs.clone(), dir.path(), seed);
        for _ in 0..20 {
            let target = run.steps + 25;
            run.run(target).unwrap();
            run.check_session();
        }
        let Run {
            model,
            session,
            taken: steps,
            ..
        } = run;
        drop(session);
        assert!(!check_recovered(vfs, dir.path(), &model), "seed {seed}");
        for (kind, count) in steps {
            *taken.entry(kind).or_insert(0) += count;
        }
    }
    assert_eq!(taken["open"], 12);
    for kind in STEP_KINDS {
        assert!(
            taken.get(kind).is_some_and(|count| *count >= 12),
            "{kind}: {taken:?}"
        );
    }
}
