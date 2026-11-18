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
    CommitVersion, FileId, PageId,
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
fn read_at(
    table: &PageTable,
    raw_page: u32,
    raw_version: u64,
) -> Option<(CommitVersion, Vec<BlockLocation>)> {
    let mut chain = vec![at(99, 20)];
    let born = table.read(page(raw_page), version(raw_version), &mut chain);
    assert_eq!(born.is_none(), chain.is_empty());
    born.map(|born| (born, chain))
}

/// Every link of the table, in the order a checkpoint holds them.
fn all_links(table: &PageTable) -> Vec<(PageId, Link)> {
    let mut links = Vec::new();
    let mut slot = Some(0);
    while let Some(from) = slot {
        slot = table.collect(from, &mut links);
    }
    links
}

fn empty_manifest() -> Manifest {
    Manifest::new(LastFileIds::default(), None, Vec::new()).expect("a valid manifest")
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
        let written = self.checkpoints.write(table, version(covers))?;
        self.checkpoints
            .install(&written, &mut self.manifest, &self.segments)?;
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
