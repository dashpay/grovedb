mod acceptance;
mod chains;
mod checks;
mod races;
mod select;

use std::{os::unix::fs::FileExt, sync::Arc};

use lignin_format::{
    delta::{Delta, DeltaRunWriter},
    flat_page::{FlatLeafBuilder, FlatValue},
    manifest::Manifest,
    segment::Compression,
    tree_page::{CellContent, CellValue, HashField, Side, TreeCell, TreeLink, TreePageMut},
    CellAddress, Checksum, FileId, FormatError, SlotId,
};
use lignin_io::{sim::SimRng, StdVfs, Vfs};
use tempfile::TempDir;

use super::*;
use crate::{
    cache::{CacheError, OwnerId, PoolConfig},
    segment::{FileNames, ManifestStore, SegmentStoreError, SegmentWriter},
};

const V1: FormatVersion = FormatVersion::V1;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// The image of the tree page at the start of `frame`.
fn tree_image(frame: &[u8]) -> Vec<u8> {
    TreePage::from_validated(frame, FRAME_FORMAT)
        .expect("a tree page")
        .to_image()
}

/// The bytes of a cell whose two hashes are `hashes` and its complement,
/// which are the hashes of nothing.
fn cell_bytes(key: &[u8], value: &[u8], hashes: u8, left: Option<TreeLink<'_>>) -> Vec<u8> {
    CellContent {
        key,
        feature: &[],
        value_hash: &[hashes; 32],
        kv_hash: &[!hashes; 32],
        left,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(value),
        fragment_root: left.is_some(),
    }
    .encode()
    .expect("a cell")
}

fn cell(bytes: &[u8]) -> TreeCell<'_> {
    TreeCell::decode(bytes, V1).expect("a cell")
}

/// A link to a cell of another page.
fn link<'a>(to: u32, hash: &'a [u8; 32]) -> TreeLink<'a> {
    TreeLink {
        child: CellAddress::new(page(to), slot(1)),
        hash,
        heights: (2, 1),
        child_key_len: 4,
        aggregate: &[],
    }
}

/// A tree page as the transaction that writes it has it: a page in a
/// buffer, changed through `TreePageMut`.
struct Written {
    frame: Vec<u8>,
    size: TreePageSize,
    id: PageId,
}

impl Written {
    fn new(size: TreePageSize, id: PageId) -> Self {
        let mut frame = vec![0; size.get()];
        TreePageMut::init(&mut frame, size, id, 0).expect("a page");
        Self { frame, size, id }
    }

    /// A page with `cells` cells of about `value_len` bytes of value each,
    /// every third of them with a link. The values compress well.
    fn filled(size: TreePageSize, id: PageId, cells: u16, value_len: usize) -> Self {
        Self::with_values(size, id, cells, |n| {
            vec![n as u8; value_len + usize::from(n % 7)]
        })
    }

    /// The same page with values that do not compress.
    fn incompressible(size: TreePageSize, id: PageId, cells: u16, value_len: usize) -> Self {
        let mut rng = rng(u64::from(id.get()));
        Self::with_values(size, id, cells, |_| noise(value_len, &mut rng))
    }

    fn with_values(
        size: TreePageSize,
        id: PageId,
        cells: u16,
        mut value: impl FnMut(u16) -> Vec<u8>,
    ) -> Self {
        let mut written = Self::new(size, id);
        let hash = [0xA7; 32];
        for n in 1..=cells {
            let key = format!("key-{n:05}");
            let left = (n % 3 == 0).then(|| link(100 + u32::from(n), &hash));
            let bytes = cell_bytes(key.as_bytes(), &value(n), n as u8, left);
            written.apply(&Delta::Insert {
                slot: slot(n),
                cell: cell(&bytes),
            });
        }
        written
    }

    fn page(&self) -> TreePage<'_> {
        TreePage::from_validated(&self.frame, V1).expect("a tree page")
    }

    fn image(&self) -> Vec<u8> {
        self.page().to_image()
    }

    /// Makes `change` to the page.
    fn apply(&mut self, change: &Delta<'_>) {
        let mut page = TreePageMut::open(&mut self.frame, self.size, V1).expect("a page");
        change.apply(&mut page).expect("the change is made");
    }

    /// Makes `changes` to the page and returns the run that records them.
    fn change(&mut self, changes: &[Delta<'_>]) -> Vec<u8> {
        let mut run = Vec::new();
        let mut records = DeltaRunWriter::new(self.id, None, &mut run);
        for change in changes {
            self.apply(change);
            records.push(change).expect("the change is written");
        }
        records.finish().expect("a run");
        run
    }
}

/// The image of a flat leaf with `entries` entries, which differ by `seed`.
/// The values compress well.
fn flat_image(size: TreePageSize, entries: usize, seed: u8) -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(b"flat/", size.image_capacity()).expect("a leaf");
    for n in 0..entries {
        let key = format!("flat/{n:04}");
        let value = vec![seed.wrapping_add(n as u8); 20 + n % 5];
        leaf.push(key.as_bytes(), FlatValue::Inline(&value))
            .expect("the entry fits");
    }
    leaf.finish()
}

/// The image of a flat leaf that is as full as a page gets, with values
/// that do not compress.
fn full_flat_image(size: TreePageSize, seed: u64) -> Vec<u8> {
    let mut rng = rng(seed);
    let mut leaf = FlatLeafBuilder::new(b"flat/", size.image_capacity()).expect("a leaf");
    for n in 0.. {
        let key = format!("flat/{n:06}");
        let value = noise(180, &mut rng);
        if leaf
            .push(key.as_bytes(), FlatValue::Inline(&value))
            .is_err()
        {
            break;
        }
    }
    leaf.finish()
}

fn noise(len: usize, rng: &mut SimRng) -> Vec<u8> {
    (0..len).map(|_| rng.next_u64() as u8).collect()
}

/// A database directory with one segment that takes blocks, and the page
/// table of its pages.
struct Db {
    size: TreePageSize,
    table: PageTable,
    segments: SegmentStore,
    writer: SegmentWriter,
    _manifest: ManifestStore,
    dir: TempDir,
}

impl Db {
    fn new(size: TreePageSize) -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(size, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest =
            ManifestStore::create(vfs, dir.path(), empty).expect("the manifest is created");
        let segments = SegmentStore::open(&manifest).expect("the segments open");
        let writer = segments.create().expect("a segment");
        segments
            .commit(&mut manifest)
            .expect("the segment is named");
        Self {
            size,
            table: PageTable::new(),
            segments,
            writer,
            _manifest: manifest,
            dir,
        }
    }

    /// A pool of `frames` fixed frames.
    fn pool(&self, frames: usize) -> BufferPool {
        let pool = BufferPool::new(PoolConfig {
            page_size: self.size,
            capacity: frames * (self.size.get() + 512),
        })
        .expect("a pool");
        assert_eq!(pool.frames(), frames);
        pool
    }

    fn loader<'s>(&'s self, pool: &'s BufferPool) -> PageLoader<'s> {
        PageLoader::new(pool, &self.table, &self.segments, self.size).expect("a loader")
    }

    fn append(
        &mut self,
        of: PageId,
        content: BlockContent,
        compression: Compression,
        payload: &[u8],
    ) -> BlockLocation {
        self.writer
            .append(of, content, compression, payload)
            .expect("the block is appended")
    }

    /// Stores `image` as the base image of `of` that the commit `born` left.
    fn base(
        &mut self,
        of: PageId,
        born: u64,
        compression: Compression,
        image: &[u8],
    ) -> BlockLocation {
        let location = self.append(of, BlockContent::Base, compression, image);
        self.table
            .push(of, Link::base(version(born), location))
            .expect("the link is pushed");
        location
    }

    /// Stores `run` as the run that brings `of` to the version `born`.
    fn delta(
        &mut self,
        of: PageId,
        born: u64,
        compression: Compression,
        run: &[u8],
    ) -> BlockLocation {
        let location = self.append(of, BlockContent::Delta, compression, run);
        self.table
            .push(of, Link::delta(version(born), location))
            .expect("the link is pushed");
        location
    }

    fn segment_file(&self) -> std::fs::File {
        let path = FileNames::SEGMENT.path(self.dir.path(), FileId::MIN);
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("the segment's file")
    }

    /// Changes the body of the block at `location`, and gives the block the
    /// checksum of what it then holds: a block that is whole and wrong.
    fn rewrite(&self, location: BlockLocation, change: impl FnOnce(&mut [u8])) {
        let mut bytes = self.stored(location);
        let (frame, body) = bytes.split_at_mut(8);
        change(body);
        let checksum = Checksum::of(&[&frame[..4], body]);
        frame[4..].copy_from_slice(&checksum.encode());
        self.segment_file()
            .write_all_at(&bytes, location.offset)
            .expect("the block is rewritten");
    }

    /// The bytes of the block at `location`, as the file has them.
    fn stored(&self, location: BlockLocation) -> Vec<u8> {
        let mut bytes = vec![0; location.len as usize];
        self.segment_file()
            .read_exact_at(&mut bytes, location.offset)
            .expect("the block is read");
        bytes
    }
}

/// The page that `loader` gives for `of` at `at`, from a pool that does not
/// hold it.
fn load(db: &Db, of: PageId, at: u64) -> Result<Vec<u8>, LoadError> {
    let pool = db.pool(2);
    let loaded = db
        .loader(&pool)
        .get(of, version(at), &mut LoadBuffers::new())?;
    Ok(loaded.to_vec())
}

/// The format error of a block whose payload was refused, with its place.
fn payload_error(error: LoadError) -> (BlockLocation, FormatError) {
    match error {
        LoadError::Payload {
            location, source, ..
        } => (location, source),
        other => panic!("expected a refused payload, got {other:?}"),
    }
}

/// The format error the segment store found in a block, with the block.
fn block_error(error: LoadError) -> (BlockLocation, FormatError) {
    match error {
        LoadError::Block {
            location,
            source: SegmentStoreError::Format { offset, source, .. },
            ..
        } => {
            assert_eq!(offset, location.offset);
            (location, source)
        }
        other => panic!("expected a block that does not read, got {other:?}"),
    }
}

fn rng(seed: u64) -> SimRng {
    SimRng::derive(seed, 0x5705, 0)
}
