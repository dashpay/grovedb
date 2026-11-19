//! The tests of the commit, over the store of the write transaction's
//! tests (`txn/tests/store.rs`), whose commit is this module's.

mod acceptance;
mod conflicts;
mod faults;
mod kept;
mod outcomes;
mod pages;
mod schedule;

use std::{collections::BTreeMap, path::Path, sync::Arc};

use lignin_format::{
    flat_page::{FlatPage, FlatValue},
    value_log::{PendingValue, ValuePlace},
    wal::{Entry, LogItem},
    CommitVersion, PageId,
};
use lignin_io::{
    sim::{Event, OpKind, OpRecord, SimConfig, SimVfs},
    Vfs,
};
use lignin_store::{
    alloc::PageState,
    flush::FlushConfig,
    load::{LoadBuffers, FRAME_FORMAT},
    wal::{LogOptions, LogReader},
};
use lignin_tree::node::{Lease, NodePage};

use crate::{
    commit::Published,
    snapshot::Versions,
    txn::tests::{
        model::{self, OwnedBatch, Write},
        store::{Db, SIZE},
        Tx, World,
    },
};

/// The directory of a store on a simulated file system.
pub(super) const DIR: &str = "/db";

/// A simulated file system with the directory of a store in it, which
/// records every operation.
pub(super) fn sim(seed: u64) -> SimVfs {
    let fs = SimVfs::new(SimConfig {
        trace_ops: true,
        ..SimConfig::new(seed)
    });
    fs.create_dir(Path::new(DIR)).expect("a directory");
    fs.sync_dir(Path::new("/")).expect("its name is durable");
    fs
}

/// An empty store on `fs`, with a log that is written as `options` say
/// and a write-back with its defaults.
pub(super) fn store_on(fs: &SimVfs, options: LogOptions) -> (Arc<Db>, Versions) {
    store_with(fs, 256, options, FlushConfig::new(SIZE))
}

/// An empty store on `fs` with a pool of `frames` frames.
pub(super) fn store_with(
    fs: &SimVfs,
    frames: usize,
    options: LogOptions,
    flush: FlushConfig,
) -> (Arc<Db>, Versions) {
    let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
    Db::on(vfs, Path::new(DIR), frames, options, flush, None)
}

/// A batch of flat records and nothing else.
pub(super) fn records(records: Vec<Write>) -> OwnedBatch {
    OwnedBatch {
        records,
        ..OwnedBatch::default()
    }
}

/// The operations of `fs` after the first `from` events of its trace that
/// could change a file or a name: everything but reads, lengths, listings
/// and locks.
pub(super) fn changes_since(fs: &SimVfs, from: usize) -> Vec<OpRecord> {
    let reads = [OpKind::Read, OpKind::Len, OpKind::ReadDir, OpKind::Lock];
    (fs.trace().into_iter().skip(from))
        .filter_map(|event| match event {
            Event::Op(op) if !reads.contains(&op.kind) => Some(op),
            _ => None,
        })
        .collect()
}

/// Calls `each` with every value place that a cell or a flat entry of the
/// pages of the latest version holds, with its page.
fn each_place_held(db: &Db, versions: &Versions, mut each: impl FnMut(PageId, ValuePlace)) {
    let guard = versions.guard();
    let at = guard.latest();
    let loader = db.loader();
    let lease = Lease::take();
    let mut buffers = LoadBuffers::new();
    let (mut from, mut ids) = (Some(PageId::MIN), Vec::new());
    while let Some(start) = from {
        ids.clear();
        from = db.ids.scan(start, &mut ids);
        for (id, state) in &ids {
            if *state != PageState::Live {
                continue;
            }
            let Ok(frame) = loader.get(*id, at, &mut buffers) else {
                continue;
            };
            if let Ok(page) = NodePage::open(&frame, lease) {
                for node in page.nodes() {
                    if let Some(place) = node.value().place() {
                        each(*id, place);
                    }
                }
            } else if let Some(leaf) = FlatPage::decode(&frame, FRAME_FORMAT)
                .ok()
                .and_then(FlatPage::leaf)
            {
                for (_, value) in leaf.iter() {
                    let FlatValue::OutOfLine(place) = value else {
                        continue;
                    };
                    let place = ValuePlace::decode(place.try_into().expect("16 bytes"));
                    if let Ok(place) = place {
                        each(*id, place);
                    }
                }
            }
        }
    }
}

/// For each value-log file, how many cells of the pages of the latest
/// version hold a pointer into it: tree cells and the entries of flat
/// leaves, those that a dropped Merk left among them. It is what the
/// counts of live values say, if every commit released what its cells
/// stopped holding and nothing else (`transactions.md` PV6).
pub(super) fn pointers_held(db: &Db, versions: &Versions) -> BTreeMap<u32, u64> {
    let mut held = BTreeMap::new();
    each_place_held(db, versions, |_, place| {
        if let ValuePlace::Placed(pointer) = place {
            *held.entry(pointer.file().get()).or_default() += 1;
        }
    });
    held
}

/// Every pending value that a cell or a flat entry of the pages of the
/// latest version holds, with its page: the values that commits carried
/// in their groups and that write-back has not placed.
pub(super) fn pending_held(db: &Db, versions: &Versions) -> Vec<(PageId, PendingValue)> {
    let mut held = Vec::new();
    each_place_held(db, versions, |page, place| {
        if let ValuePlace::Pending(pending) = place {
            held.push((page, pending));
        }
    });
    held
}

/// The writes of `tx`, as its model has them.
pub(super) fn writes_of(tx: &Tx<'_>) -> Vec<Write> {
    (tx.model.borrow().writes.iter())
        .map(|(write, ..)| write.clone())
        .collect()
}

/// Commits `tx` through the commit path and tells the model, and returns
/// what the commit reports.
pub(super) fn commit_reported<'s>(world: &mut World<'s>, tx: Tx<'s>) -> Box<Published> {
    let writes = writes_of(&tx);
    let committer = world.db.committer(world.versions);
    let published = (world.db.commit_as(&committer, &tx.txn, None))
        .expect("the commit is made")
        .expect("it takes a version");
    assert_eq!(world.model.commit(&writes), published.version.get());
    published
}

/// The counts of live values of the store, by file, without the files
/// that hold none.
pub(super) fn counted(db: &Db) -> BTreeMap<u32, u64> {
    (db.values.counts().into_iter())
        .inspect(|count| assert!(!count.miscounted, "{count:?}"))
        .filter(|count| count.values > 0)
        .map(|count| (count.file.get(), count.values))
        .collect()
}

/// What one group of the log says, as a recovery reads it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub(super) version: u64,
    /// The values, each with the page that owns it.
    pub(super) values: Vec<(u32, Vec<u8>)>,
    pub(super) tree_runs: Vec<u32>,
    pub(super) flat_runs: Vec<u32>,
    pub(super) freed: Vec<u32>,
    pub(super) released: usize,
}

/// Every group of the log of the store in `dir` of `vfs`, as a recovery
/// that opens the manifest in force reads it (`transactions.md` CM9, DU2):
/// the log's own reader, up to the last whole item.
pub(super) fn groups(db: &Db) -> Vec<Group> {
    let manifest = db.manifest.lock().expect("the manifest");
    let named = manifest.manifest().expect("a manifest").clone();
    drop(manifest);
    read_groups(db.vfs.clone(), &db.dir, &named)
}

pub(super) fn read_groups(
    vfs: Arc<dyn Vfs>,
    dir: &Path,
    manifest: &lignin_format::manifest::Manifest,
) -> Vec<Group> {
    let mut reader = LogReader::new(vfs, dir, manifest);
    let mut groups = Vec::new();
    loop {
        let read = reader.next_item(|read| match read.item {
            LogItem::Commit(group) => {
                let mut said = Group {
                    version: group.version().get(),
                    ..Group::default()
                };
                for (_, value) in group.values() {
                    let bytes = value.bytes().expect("the value's bytes").to_vec();
                    said.values.push((value.owner().get(), bytes));
                }
                for entry in group.entries() {
                    match entry {
                        Entry::Tree(run) => said.tree_runs.push(run.page().get()),
                        Entry::Flat(run) => said.flat_runs.push(run.page().get()),
                        Entry::Freed(page) => said.freed.push(page.get()),
                        Entry::Released(_) => said.released += 1,
                        Entry::Acquired(_) => panic!("a commit acquires no pointer"),
                    }
                }
                Some(said)
            }
            _ => None,
        });
        match read.expect("the log is read") {
            Some(Some(group)) => groups.push(group),
            Some(None) => {}
            None => return groups,
        }
    }
}

pub(super) fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// The unit of the aux records of the subtree `name`.
pub(super) fn aux(name: u8) -> crate::txn::Unit {
    model::unit(crate::txn::Column::Aux, name)
}
