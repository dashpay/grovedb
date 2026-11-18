mod clean;
mod counts;
mod crash;
mod disk_faults;
mod errors;
#[allow(dead_code)]
#[path = "../../segment/tests/faulty.rs"]
mod faulty;
mod pages;
mod run;
#[allow(dead_code)]
#[path = "../../segment/tests/sim.rs"]
mod sim;
mod store;

use std::{path::Path, sync::Arc};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    FileId, FileKind, PageId,
};
use lignin_io::Vfs;

use super::*;
use crate::segment::{read_manifest, FileNames, ManifestStore, SegmentStoreError};

fn file_id(raw: u32) -> FileId {
    FileId::new(raw).expect("not zero")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
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
