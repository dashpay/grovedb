//! One append system call per commit, counted by the kernel.
//!
//! `/proc/self/io` counts the `write` system calls of this process and the
//! bytes they were given. The test commits groups to a log on the
//! operating system's files and reads the two counters around each commit:
//! one call, of exactly the group's bytes. It runs alone in its binary, so
//! that nothing else writes while it counts.
//!
//! To see the calls themselves, give the test a directory and trace it:
//!
//! ```text
//! mkdir /tmp/wal && LIGNIN_WAL_DIR=/tmp/wal strace -f -y \
//!     -e trace=write,pwrite64,writev,fsync,fdatasync \
//!     -P /tmp/wal/wal/00000001.log \
//!     cargo test -p lignin-store --test wal_syscalls
//! ```

#![cfg(target_os = "linux")]

use std::{path::Path, sync::Arc};

use lignin_format::{
    delta::Delta,
    manifest::Manifest,
    tree_page::HashField,
    wal::{GroupWriter, LogItem},
    PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    segment::ManifestStore,
    wal::{LogEnd, LogOptions, LogReader, LogStore, SyncPolicy},
};

/// The `write` system calls this process has made, and the bytes they were
/// given.
fn writes() -> Option<(u64, u64)> {
    let io = std::fs::read_to_string("/proc/self/io").ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = io.lines().find(|line| line.starts_with(name))?;
        line.split_whitespace().nth(1)?.parse().ok()
    };
    Some((field("syscw:")?, field("wchar:")?))
}

fn commit(log: &LogStore, values: usize, pages: u32) -> usize {
    let at = log.next_group().expect("a place");
    let mut bytes = Vec::new();
    let mut group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
    let owner = PageId::new(9).expect("a page");
    for value in 0..values {
        group
            .value(owner, &vec![value as u8; 700 + 13 * value])
            .expect("a value");
    }
    for page in 1..=pages {
        let page = PageId::new(page).expect("a page");
        group
            .tree_run_with(page, None, |run| {
                run.push(&Delta::Hash {
                    slot: SlotId::MIN,
                    field: HashField::KeyValue,
                    hash: &[page.get() as u8; 32],
                })
            })
            .expect("a run");
    }
    group.finish();

    let before = writes();
    let appended = log.append_group(&at, &bytes).expect("an append");
    let after = writes();
    log.sync_for_commit(&appended).expect("no sync is due");
    if let (Some((calls, chars)), Some((calls_after, chars_after))) = (before, after) {
        assert_eq!(calls_after - calls, 1, "one write system call for a commit");
        assert_eq!(
            chars_after - chars,
            bytes.len() as u64,
            "of the whole group"
        );
    }
    bytes.len()
}

#[test]
fn a_commit_is_one_write_system_call() {
    let kept;
    let dir = match std::env::var_os("LIGNIN_WAL_DIR") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            kept = tempfile::tempdir().expect("a directory");
            kept.path().to_path_buf()
        }
    };
    let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
    let empty = Manifest::new(PageSize::DEFAULT, Default::default(), None, Vec::new()).unwrap();
    let mut manifest = ManifestStore::create(vfs.clone(), &dir, empty).expect("a manifest");
    let options = LogOptions {
        sync: SyncPolicy::Never,
        ..LogOptions::default()
    };
    let log = LogStore::open(&manifest, &LogEnd::default(), options).expect("a log");
    log.roll(&mut manifest).expect("a first file");

    if writes().is_none() {
        eprintln!("/proc/self/io cannot be read here: the calls are not counted");
    }
    let mut longest = 0;
    for round in 0..300 {
        // From a commit that changes nothing to a block of many pages with
        // several values: one record or a dozen, one call.
        let len = commit(&log, round % 5, (round % 40) as u32);
        longest = longest.max(len);
    }
    assert!(longest > 4000, "{longest}");

    let mut reader = LogReader::new(vfs, Path::new(&dir), manifest.manifest().unwrap());
    let mut commits = 0;
    while let Some(is_commit) = reader
        .next_item(|read| matches!(read.item, LogItem::Commit(_)))
        .expect("the log reads")
    {
        commits += u32::from(is_commit);
    }
    assert_eq!(commits, 300);
}
