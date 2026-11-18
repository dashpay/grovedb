mod clean;
mod counts;
mod crash;
mod disk_faults;
mod errors;
mod pages;
mod run;
mod store;

use std::{path::Path, sync::Arc};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    value_log::ValuePointer,
    CommitVersion, FileHeader, FileId, FileKind, FormatVersion, PageId,
};
use lignin_io::Vfs;

use super::*;
use crate::segment::{
    read_manifest,
    tests::{faulty, sim},
    FileNames, ManifestStore, SegmentStoreError,
};

const V1: FormatVersion = FormatVersion::V1;
const HEADER: u64 = FileHeader::ENCODED_LEN as u64;

fn file_id(raw: u32) -> FileId {
    FileId::new(raw).expect("not zero")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn empty_manifest() -> Manifest {
    Manifest::new(Default::default(), None, Vec::new()).expect("a valid manifest")
}

fn value_file(raw_id: u32, state: FileState, valid_len: u64) -> LiveFile {
    LiveFile::new(FileKind::ValueLog, file_id(raw_id), state, valid_len).expect("a valid entry")
}

/// The error of the file layer that `error` reports, also when the model of
/// the pages met it inside a pointer rewrite.
fn io_error(error: &ValueStoreError) -> Option<&std::io::Error> {
    match error {
        ValueStoreError::Owners(inner) => inner.downcast_ref::<ValueStoreError>()?.io_error(),
        other => other.io_error(),
    }
}

/// Whether `error` is the file layer's report that the power is off.
fn lost_power(error: &ValueStoreError) -> bool {
    io_error(error).is_some_and(lignin_io::sim::SimError::is_power_loss)
}

/// Owners that say every value is live and take every rewrite.
struct Everything;

impl ValueOwners for Everything {
    fn holds(&self, _: PageId, _: ValuePointer) -> Result<bool, OwnersError> {
        Ok(true)
    }

    fn rewrite(&self, step: PointerRewrite<'_>) -> Result<Rewrite, OwnersError> {
        Ok(Rewrite::Installed(step.install(version(1))))
    }
}

/// The count of live values of the file `raw_id`, if `values` holds it.
fn live(values: &ValueStore, raw_id: u32) -> Option<u64> {
    values.count(file_id(raw_id)).map(|count| count.values)
}
