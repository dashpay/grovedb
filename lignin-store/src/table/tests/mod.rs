mod acceptance;
mod chains;
mod checkpoint;
mod crash;
mod disk_faults;
mod errors;
mod model;

use std::{path::Path, sync::Arc};

use lignin_format::{
    manifest::{LastFileIds, Manifest, TableCheckpoint},
    wal::MAX_LOG_OFFSET,
    CommitVersion, FileId, PageId, PageSize,
};
use lignin_io::{
    sim::{Fault, SimError, SimVfs},
    Vfs,
};

use super::*;
use crate::segment::{
    tests::sim::{self, db_dir, shared},
    BlockLocation, FileNames, ManifestStore, SegmentStore, SegmentStoreError,
};

/// A log that is durable through every version and up to its last place,
/// for the tests that are not about the log.
const LOG: LogDurable = LogDurable {
    version: CommitVersion::MAX,
    place: match LogPlace::new(FileId::MAX, MAX_LOG_OFFSET) {
        Ok(place) => Some(place),
        Err(_) => None,
    },
};

/// A log that is durable through the commit of `raw_version`, with nothing
/// said of the place that is.
fn log(raw_version: u64) -> LogDurable {
    LogDurable {
        version: version(raw_version),
        place: None,
    }
}

fn id(raw: u32) -> FileId {
    FileId::new(raw).expect("not zero")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// A location that no block was written to: enough for the table itself,
/// which reads no block.
fn at(file: u32, offset: u64) -> BlockLocation {
    BlockLocation {
        file: id(file),
        offset,
        len: 64,
    }
}

fn base(raw_version: u64, location: BlockLocation) -> Link {
    Link::base(version(raw_version), location)
}

fn delta(raw_version: u64, location: BlockLocation) -> Link {
    Link::delta(version(raw_version), location)
}

fn free(raw_version: u64) -> Link {
    Link::free(version(raw_version))
}

fn chain_of(table: &PageTable, raw_page: u32) -> Vec<Link> {
    let mut chain = Vec::new();
    table.chain(page(raw_page), &mut chain);
    chain
}

/// What a reader of `raw_page` at `raw_version` is given: the version its
/// page version was born at, and the blocks.
///
/// [`PageTable::read_version`] must give the same, with the version of the
/// link that follows the newest one the reader is given, or the version of
/// the free link that says the page has no version there.
fn read_at(
    table: &PageTable,
    raw_page: u32,
    raw_version: u64,
) -> Option<(CommitVersion, Vec<BlockLocation>)> {
    let asked = version(raw_version);
    let mut chain = vec![at(99, 20)];
    let born = table.read(page(raw_page), asked, &mut chain);
    assert_eq!(born.is_none(), chain.is_empty());

    let mut again = vec![at(98, 20)];
    let visible = table.read_version(page(raw_page), asked, &mut again);
    assert_eq!(again, chain);
    let links = chain_of(table, raw_page);
    let newest = links.iter().rposition(|link| link.version <= asked);
    let expected = match (born, newest) {
        (Some(born), Some(newest)) => Ok(Visible {
            born,
            next: links.get(newest + 1).map(|link| link.version),
        }),
        (Some(_), None) => panic!("a page version with no link at {asked}"),
        (None, newest) => Err(newest
            .map(|newest| links[newest])
            .filter(|link| link.kind == LinkKind::Free)
            .map(|link| link.version)),
    };
    assert_eq!(visible, expected, "page {raw_page} at {asked}");
    if let Ok(Visible {
        next: Some(next), ..
    }) = visible
    {
        assert!(next > asked);
    }
    born.map(|born| (born, chain))
}

/// Every link of the table, in the order a checkpoint holds them.
fn all_links(table: &PageTable) -> Vec<(PageId, Link)> {
    let mut links = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        next = table.scan(from, &mut links);
    }
    links
}

fn empty_manifest() -> Manifest {
    Manifest::new(PageSize::DEFAULT, LastFileIds::default(), None, Vec::new())
        .expect("a valid manifest")
}

fn names_in(fs: &dyn Vfs, dir: &Path) -> Vec<String> {
    fs.read_dir(dir)
        .expect("the directory is listed")
        .into_iter()
        .map(|entry| entry.name.to_string_lossy().into_owned())
        .collect()
}

fn table_dir() -> std::path::PathBuf {
    db_dir().join("table")
}

/// The parts of one session on a database directory.
struct Db {
    manifest: ManifestStore,
    segments: SegmentStore,
    checkpoints: TableCheckpoints,
}

impl Db {
    fn create(fs: &SimVfs) -> Self {
        let manifest = ManifestStore::create(shared(fs), db_dir(), empty_manifest())
            .expect("the manifest is created");
        Self::with(manifest).expect("the directory opens")
    }

    fn open(fs: &SimVfs) -> Result<Self, PageTableError> {
        Self::with(ManifestStore::open(shared(fs), db_dir())?)
    }

    fn with(manifest: ManifestStore) -> Result<Self, PageTableError> {
        let segments = SegmentStore::open(&manifest)?;
        let checkpoints = TableCheckpoints::open(&manifest)?;
        Ok(Self {
            manifest,
            segments,
            checkpoints,
        })
    }

    fn named(&self) -> Option<TableCheckpoint> {
        self.manifest
            .manifest()
            .expect("not in doubt")
            .table_checkpoint()
    }

    fn load(&self) -> Result<(PageTable, Option<CheckpointInfo>), PageTableError> {
        let manifest = self.manifest.manifest()?;
        PageTable::load(&**self.manifest.vfs(), self.manifest.dir(), manifest)
    }

    fn checkpoint(
        &mut self,
        table: &PageTable,
        covers: u64,
    ) -> Result<CheckpointInfo, PageTableError> {
        let written = self
            .checkpoints
            .write(table, version(covers), &CountSample::default())?;
        self.checkpoints
            .install(&written, LOG, &mut self.manifest, &self.segments)?;
        Ok(written)
    }
}

fn store_error(error: PageTableError) -> SegmentStoreError {
    match error {
        PageTableError::Store(error) => error,
        other => panic!("expected an error of the segment store, got {other:?}"),
    }
}

fn arc(fs: &SimVfs) -> Arc<dyn Vfs> {
    shared(fs)
}

fn io_error(error: &PageTableError) -> Option<&std::io::Error> {
    match error {
        PageTableError::Io { source, .. } | PageTableError::NotDeleted { source, .. } => {
            Some(source)
        }
        _ => None,
    }
}

/// Whether `error` is the file layer's report that the power is off.
fn is_power_loss(error: &PageTableError) -> bool {
    match error {
        PageTableError::Store(error) => sim::is_power_loss(error),
        error => io_error(error).is_some_and(SimError::is_power_loss),
    }
}

/// The number of the operation whose injected failure `error` reports.
fn injected_op(error: &PageTableError) -> Option<(Fault, u64)> {
    match error {
        PageTableError::Store(error) => sim::injected_op(error),
        error => match SimError::of(io_error(error)?)? {
            SimError::Injected { fault, op, .. } => Some((*fault, *op)),
            SimError::PowerLoss => None,
        },
    }
}
