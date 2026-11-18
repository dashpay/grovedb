#![allow(clippy::single_range_in_vec_init)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    io::{self, ErrorKind},
    ops::Range,
    path::{Path, PathBuf},
};

use super::*;
use crate::vfs::{Create, EntryKind, OpenMode, Vfs, VfsFile};

mod crash_points;
mod differential;
mod durability;
mod faults;
mod oracle;
mod rng;
mod schedule;
mod trace;

type Tree = BTreeMap<String, Option<Vec<u8>>>;

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
fn read(vfs: &dyn Vfs, path: impl AsRef<Path>) -> Vec<u8> {
    let file = vfs.open(path.as_ref(), OpenMode::Read).unwrap();
    let mut content = vec![0; file.len().unwrap() as usize];
    file.read_exact_at(&mut content, 0).unwrap();
    content
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

fn name_subsets(image: &CrashImage) -> impl Iterator<Item = Vec<usize>> + '_ {
    let count = image.names().len();
    (0..1u32 << count).map(move |mask| (0..count).filter(|name| mask >> name & 1 == 1).collect())
}

fn all_tears(change: &FileChange, sector: u64) -> Vec<Tear> {
    let FileChangeKind::Write { offset, len } = change.kind else {
        return Vec::new();
    };
    let end = offset + len;
    let lens = std::iter::once(0)
        .chain((offset + 1..=end).filter(|at| at.is_multiple_of(sector) || *at == end));
    let lens: Vec<u64> = lens.collect();
    let mut tears = Vec::new();
    for mask in 0..1u32 << change.sectors {
        let mut sectors: Vec<Range<u64>> = Vec::new();
        for index in (0..change.sectors).filter(|index| mask >> index & 1 == 1) {
            match sectors.last_mut() {
                Some(last) if last.end == index => last.end += 1,
                _ => sectors.push(index..index + 1),
            }
        }
        for len in &lens {
            tears.push(Tear {
                sectors: sectors.clone(),
                len: *len,
            });
        }
    }
    tears
}

/// Every plan for the file changes of `image`: every choice of survivors, in
/// every order, each write whole or with every tear there is.
fn all_change_plans(image: &CrashImage) -> Vec<CrashPlan> {
    fn extend(
        options: &[Vec<Option<Tear>>],
        used: &mut Vec<bool>,
        current: &mut Vec<Survivor>,
        plans: &mut Vec<CrashPlan>,
    ) {
        plans.push(CrashPlan {
            names: Vec::new(),
            changes: current.clone(),
        });
        for change in 0..options.len() {
            if used[change] {
                continue;
            }
            used[change] = true;
            for tear in &options[change] {
                current.push(Survivor {
                    change,
                    tear: tear.clone(),
                });
                extend(options, used, current, plans);
                current.pop();
            }
            used[change] = false;
        }
    }

    let options: Vec<Vec<Option<Tear>>> = image
        .changes()
        .iter()
        .map(|change| {
            let tears = all_tears(change, image.sector_size()).into_iter().map(Some);
            std::iter::once(None).chain(tears).collect()
        })
        .collect();
    let mut plans = Vec::new();
    extend(
        &options,
        &mut vec![false; options.len()],
        &mut Vec::new(),
        &mut plans,
    );
    plans
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
