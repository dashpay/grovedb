mod crash;
mod disk_faults;
mod errors;
pub(crate) mod faulty;
mod files;
mod manifest;
mod model;
pub(crate) mod sim;
mod store;
mod writer;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{
    manifest::{FileState, LastFileIds, LiveFile, Manifest, TableCheckpoint},
    segment::{BlockContent, Compression},
    CommitVersion, FileHeader, FileId, FileKind, FormatError, FormatVersion, PageId, PageSize,
};
use lignin_io::{StdVfs, Vfs};
use tempfile::TempDir;

use self::faulty::{FaultyVfs, Op};
use super::*;

const MAX: usize = 1 << 20;

fn id(raw: u32) -> FileId {
    FileId::new(raw).expect("not zero")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn last_ids(last: [u32; 4]) -> LastFileIds {
    let [wal, segment, value_log, page_table] = last.map(FileId::new);
    LastFileIds {
        wal,
        segment,
        value_log,
        page_table,
    }
}

fn live(kind: FileKind, raw_id: u32, state: FileState, valid_len: u64) -> LiveFile {
    LiveFile::new(kind, id(raw_id), state, valid_len).expect("valid entry")
}

fn segment(raw_id: u32, state: FileState, valid_len: u64) -> LiveFile {
    live(FileKind::Segment, raw_id, state, valid_len)
}

fn table(raw_id: u32, valid_len: u64, covers: u64) -> TableCheckpoint {
    let covers = CommitVersion::new(covers).expect("a version");
    TableCheckpoint::new(id(raw_id), valid_len, covers).expect("valid checkpoint")
}

fn manifest_of(last: [u32; 4], table: Option<TableCheckpoint>, files: Vec<LiveFile>) -> Manifest {
    Manifest::new(PageSize::DEFAULT, last_ids(last), table, files).expect("valid manifest")
}

fn replace(store: &mut ManifestStore, next: Manifest) -> Result<(), SegmentStoreError> {
    store.update(|_| Ok(next))
}

fn empty_manifest() -> Manifest {
    manifest_of([0; 4], None, Vec::new())
}

fn random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn repetitive(len: usize, seed: u64) -> Vec<u8> {
    let word = random(7, seed);
    word.iter().copied().cycle().take(len).collect()
}

fn header(kind: FileKind, raw_id: u32) -> [u8; FileHeader::ENCODED_LEN] {
    FileHeader::new(kind, FormatVersion::CURRENT, raw_id)
        .expect("valid header")
        .encode()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn dir_name(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().into_owned()
}

fn faulty_std() -> FaultyVfs {
    FaultyVfs::new(Arc::new(StdVfs))
}

/// One block as a test appended it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Appended {
    location: BlockLocation,
    page: PageId,
    content: BlockContent,
    payload: Vec<u8>,
}

/// A database directory with its manifest and segment store, on the
/// operating system's files behind a [`FaultyVfs`].
struct Db {
    vfs: FaultyVfs,
    manifest: ManifestStore,
    segments: SegmentStore,
    dir: TempDir,
}

impl Db {
    fn new() -> Self {
        Self::with_manifest(empty_manifest(), |_| {})
    }

    /// A directory whose first manifest is `manifest`, after `prepare` has
    /// put files into it.
    fn with_manifest(manifest: Manifest, prepare: impl FnOnce(&Path)) -> Self {
        Self::try_with_manifest(manifest, prepare).expect("the segments open")
    }

    fn try_with_manifest(
        manifest: Manifest,
        prepare: impl FnOnce(&Path),
    ) -> Result<Self, (SegmentStoreError, TempDir)> {
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path());
        let vfs = faulty_std();
        let manifest = ManifestStore::create(Arc::new(vfs.clone()), dir.path(), manifest)
            .expect("the manifest is created");
        match SegmentStore::open(&manifest) {
            Ok(segments) => {
                vfs.take_calls();
                Ok(Self {
                    vfs,
                    manifest,
                    segments,
                    dir,
                })
            }
            Err(error) => Err((error, dir)),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn arc_vfs(&self) -> Arc<dyn Vfs> {
        Arc::new(self.vfs.clone())
    }

    fn seg_dir(&self) -> PathBuf {
        self.path().join("seg")
    }

    fn seg_path(&self, raw_id: u32) -> PathBuf {
        FileNames::SEGMENT.path(self.path(), id(raw_id))
    }

    /// Ends the session and starts another on the same directory.
    fn reopen(self) -> Self {
        self.try_reopen().expect("the directory opens again")
    }

    fn try_reopen(self) -> Result<Self, SegmentStoreError> {
        let Self {
            vfs,
            manifest,
            segments,
            dir,
        } = self;
        drop((manifest, segments));
        let manifest = ManifestStore::open(Arc::new(vfs.clone()), dir.path())?;
        let segments = SegmentStore::open(&manifest)?;
        vfs.take_calls();
        Ok(Self {
            vfs,
            manifest,
            segments,
            dir,
        })
    }

    fn commit(&mut self) {
        self.segments
            .commit(&mut self.manifest)
            .expect("the manifest is replaced");
    }

    /// A new segment that the manifest names, so it takes blocks.
    fn writer(&mut self) -> SegmentWriter {
        let writer = self.segments.create().expect("a segment is created");
        self.commit();
        writer
    }

    fn durable(&self) -> &Manifest {
        self.manifest.manifest().expect("not in doubt")
    }

    fn on_disk(&self) -> Manifest {
        read_manifest(&self.vfs, self.path()).expect("a manifest on disk")
    }

    fn read(&self, location: BlockLocation) -> Result<(BlockContent, Vec<u8>), SegmentStoreError> {
        let mut buf = Vec::new();
        let block = self.segments.read(location, MAX, &mut buf)?;
        let payload = block.payload().expect("decompresses").into_owned();
        Ok((block.content(), payload))
    }

    #[track_caller]
    fn assert_readable(&self, blocks: &[Appended]) {
        for block in blocks {
            let (content, payload) = self.read(block.location).expect("the block is read");
            assert_eq!((content, &payload), (block.content, &block.payload));
        }
    }

    fn walked(
        &self,
        raw_id: u32,
    ) -> Result<Vec<(BlockLocation, BlockContent, Vec<u8>)>, SegmentStoreError> {
        let mut walk = self.segments.walk(id(raw_id), MAX)?;
        let mut blocks = Vec::new();
        while let Some((location, block)) = walk.next_block()? {
            let payload = block.payload().expect("decompresses").into_owned();
            blocks.push((location, block.content(), payload));
        }
        Ok(blocks)
    }

    #[track_caller]
    fn assert_walk_gives(&self, raw_id: u32, blocks: &[Appended]) {
        let expected: Vec<_> = blocks
            .iter()
            .map(|block| (block.location, block.content, block.payload.clone()))
            .collect();
        assert_eq!(
            self.walked(raw_id).expect("the segment is walked"),
            expected
        );
    }
}

/// Appends `count` blocks of mixed kinds and sizes, starting the pattern at
/// `seed`.
fn append_blocks(writer: &mut SegmentWriter, count: usize, seed: u64) -> Vec<Appended> {
    (0..count as u64)
        .map(|n| {
            let seed = seed + n;
            let len = match seed % 5 {
                0 => 0,
                1 => 1 + (seed as usize * 37) % 90,
                2 => 4096,
                3 => 300 + (seed as usize * 911) % 5000,
                _ => 13,
            };
            let payload = if seed.is_multiple_of(2) {
                repetitive(len, seed)
            } else {
                random(len, seed)
            };
            let content = if seed.is_multiple_of(3) {
                BlockContent::Base
            } else {
                BlockContent::Delta
            };
            let compression = if seed % 4 < 2 {
                Compression::Lz4
            } else {
                Compression::Raw
            };
            let page = page(1 + (seed % 9) as u32);
            let location = writer
                .append(page, content, compression, &payload)
                .expect("the block is appended");
            Appended {
                location,
                page,
                content,
                payload,
            }
        })
        .collect()
}

fn format_error(error: SegmentStoreError) -> (u64, FormatError) {
    match error {
        SegmentStoreError::Format { offset, source, .. } => (offset, source),
        other => panic!("expected a format error, got {other:?}"),
    }
}

#[track_caller]
fn assert_io(error: &SegmentStoreError, expected_op: &str, expected_name: &str) {
    match error {
        SegmentStoreError::Io { op, path, source } => {
            assert_eq!((*op, dir_name(path).as_str()), (expected_op, expected_name));
            assert_eq!(source.kind(), faulty::INJECTED);
        }
        other => panic!("expected an I/O error, got {other:?}"),
    }
}
