#![allow(clippy::single_range_in_vec_init)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    io::{self, ErrorKind},
    ops::Range,
    path::{Path, PathBuf},
};

use super::*;
use crate::vfs::{Create, EntryKind, OpenMode, SyncItem, Vfs, VfsFile};

mod budget;
mod crash_points;
mod differential;
mod directories;
mod durability;
mod faults;
mod many_changes;
mod names_campaign;
mod oracle;
mod rng;
mod schedule;
mod sync_handles;
mod together;
mod trace;
mod write_back;

type Tree = BTreeMap<String, Option<Vec<u8>>>;

/// The stack, in bytes, of the thread on which a test runs what must not
/// call itself once for each of [`DEEP`] things.
const SMALL_STACK: usize = 256 * 1024;

/// How many directories one in the other, or changes of one file, such a
/// test makes. A call that makes a call keeps at least the address it
/// returns to on the stack, so outside Miri this many calls, each made by
/// the one before it, do not fit in [`SMALL_STACK`]. Miri is given fewer,
/// and has no stack to overflow.
const DEEP: usize = if cfg!(miri) { 200 } else { 100_000 };

const _: () = assert!(cfg!(miri) || DEEP * size_of::<usize>() > SMALL_STACK);

/// Runs `test` on a thread whose stack has [`SMALL_STACK`] bytes, and
/// panics as it panics.
fn on_a_small_stack(test: impl FnOnce() + Send + 'static) {
    let thread = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(test)
        .unwrap();
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}

fn sim(seed: u64) -> SimVfs {
    SimVfs::new(SimConfig::new(seed))
}

fn sim_with_sectors(seed: u64, sector_size: u64) -> SimVfs {
    SimVfs::new(SimConfig {
        sector_size,
        ..SimConfig::new(seed)
    })
}

fn p(path: &str) -> &Path {
    Path::new(path)
}

#[track_caller]
fn check(fs: &SimVfs) {
    fs.shared.state().check();
}

#[track_caller]
fn kind<T: Debug>(result: io::Result<T>) -> ErrorKind {
    result.unwrap_err().kind()
}

#[track_caller]
fn read_handle(file: &dyn VfsFile) -> Vec<u8> {
    let mut content = vec![0; file.len().unwrap() as usize];
    file.read_exact_at(&mut content, 0).unwrap();
    content
}

#[track_caller]
fn read(vfs: &dyn Vfs, path: impl AsRef<Path>) -> Vec<u8> {
    read_handle(&*vfs.open(path.as_ref(), OpenMode::Read).unwrap())
}

#[track_caller]
fn create(vfs: &dyn Vfs, path: &str, content: &[u8]) -> Box<dyn VfsFile> {
    let file = vfs.open(p(path), OpenMode::Write(Create::New)).unwrap();
    file.write_at(content, 0).unwrap();
    file
}

#[track_caller]
fn create_durable(vfs: &dyn Vfs, path: &str, content: &[u8]) -> Box<dyn VfsFile> {
    let file = create(vfs, path, content);
    file.sync_data().unwrap();
    vfs.sync_dir(p(path).parent().unwrap()).unwrap();
    file
}

#[track_caller]
fn exists(vfs: &dyn Vfs, path: &str) -> bool {
    match vfs.open(p(path), OpenMode::Read) {
        Ok(_) => true,
        Err(error) => {
            assert_eq!(error.kind(), ErrorKind::NotFound);
            false
        }
    }
}

/// The operations a fault was injected into, with what each left.
fn injected(fs: &SimVfs) -> Vec<(OpKind, Fault, Effect)> {
    fs.trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::Op(OpRecord {
                kind,
                outcome: Outcome::Injected { fault, effect },
                ..
            }) => Some((kind, fault, effect)),
            _ => None,
        })
        .collect()
}

fn tree_under(vfs: &dyn Vfs, root: &Path) -> Tree {
    fn walk(vfs: &dyn Vfs, root: &Path, dir: &Path, tree: &mut Tree) {
        for entry in vfs.read_dir(&root.join(dir)).unwrap() {
            let path = dir.join(&entry.name);
            let shown = path.to_string_lossy().into_owned();
            if entry.kind == EntryKind::Dir {
                tree.insert(shown, None);
                walk(vfs, root, &path, tree);
            } else {
                tree.insert(shown, Some(read(vfs, root.join(&path))));
            }
        }
    }
    let mut tree = Tree::new();
    walk(vfs, root, Path::new(""), &mut tree);
    tree
}

fn tree(vfs: &dyn Vfs) -> Tree {
    tree_under(vfs, p("/"))
}

fn tree_of(entries: &[(&str, Option<&str>)]) -> Tree {
    let content = |content: &str| content.as_bytes().to_vec();
    entries
        .iter()
        .map(|(path, file)| (path.to_string(), file.map(content)))
        .collect()
}

#[track_caller]
fn recover(image: &CrashImage, plan: &CrashPlan) -> SimVfs {
    let fs = image.recover(plan).unwrap();
    check(&fs);
    fs
}

fn names_only(names: Vec<usize>) -> CrashPlan {
    CrashPlan {
        names,
        changes: Vec::new(),
    }
}

fn changes_only(changes: &[usize]) -> CrashPlan {
    CrashPlan {
        names: Vec::new(),
        changes: changes.iter().copied().map(Survivor::whole).collect(),
    }
}

fn tear(sectors: &[Range<u64>], len: u64) -> Tear {
    Tear {
        sectors: sectors.to_vec(),
        len,
    }
}

fn torn(change: usize, tear: Tear) -> CrashPlan {
    CrashPlan {
        names: Vec::new(),
        changes: vec![Survivor {
            change,
            tear: Some(tear),
        }],
    }
}

fn name_subsets(image: &CrashImage) -> impl Iterator<Item = Vec<usize>> + '_ {
    let count = image.names().len();
    (0..1u32 << count).map(move |mask| (0..count).filter(|name| mask >> name & 1 == 1).collect())
}

/// Every plan of an image small enough to have them counted.
#[track_caller]
fn every_plan(image: &CrashImage) -> Vec<CrashPlan> {
    image.every_plan(2_000_000).unwrap()
}

struct CheckOnDrop(SimVfs);

impl Drop for CheckOnDrop {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            check(&self.0);
        }
    }
}

mod conformance {
    mod at_the_root {
        use super::super::*;

        fn setup() -> (SimVfs, PathBuf, CheckOnDrop) {
            let fs = sim(1);
            (fs.clone(), PathBuf::from("/"), CheckOnDrop(fs))
        }

        crate::conformance::conformance_tests!(setup());
    }

    mod with_one_byte_sectors_and_every_operation_traced {
        use super::super::*;

        fn setup() -> (SimVfs, PathBuf, CheckOnDrop) {
            let fs = SimVfs::new(SimConfig {
                sector_size: 1,
                trace_ops: true,
                ..SimConfig::new(2)
            });
            fs.create_dir(p("db")).unwrap();
            fs.create_dir(p("db/nested")).unwrap();
            (fs.clone(), PathBuf::from("db/nested"), CheckOnDrop(fs))
        }

        crate::conformance::conformance_tests!(setup());
    }

    mod after_a_crash {
        use super::super::*;

        fn setup() -> (SimVfs, PathBuf, CheckOnDrop) {
            let before = sim(3);
            before.create_dir(p("/wal")).unwrap();
            let log = before
                .open(p("/wal/1.log"), OpenMode::Append(Create::New))
                .unwrap();
            log.append(&[7; 3_000]).unwrap();
            log.sync_data().unwrap();
            before.sync_dir(p("/")).unwrap();
            log.append(&[8; 3_000]).unwrap();
            create(&before, "/MANIFEST.tmp", b"next");
            before.hard_link(p("/wal/1.log"), p("/linked")).unwrap();

            let fs = before.crash(11);
            fs.create_dir(p("/suite")).unwrap();
            (fs.clone(), PathBuf::from("/suite"), CheckOnDrop(fs))
        }

        crate::conformance::conformance_tests!(setup());
    }
}
