//! A checkpoint's directory, recovered while the live database goes on
//! appending to the files the two share. A checkpoint links the files of
//! the live database, and its manifest names each one closed, or sealed if
//! it was, at the length the sample found (CK5 of `transactions.md`). The
//! live database still writes behind those lengths: its next commit to
//! the last log file, and blocks and values to the segment and the
//! value-log file it has open.
//!
//! Recovery opens each of those three files for reading, the log's twice,
//! and here the file layer has all three grow before every open of a file
//! of the checkpoint for reading.

use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::{self, Write},
    sync::Mutex,
};

use lignin_format::{
    manifest::{FileState, LiveFile},
    wal::LogStart,
    FileKind,
};
use lignin_io::{DirEntry, OpenMode, VfsFile, VfsLock};

use super::*;
use crate::{
    segment::{read_manifest, FileNames},
    wal::LogReader,
};

/// One file that the live database and the checkpoint share, and what the
/// live database appends to it from the sample on.
#[derive(Debug)]
struct Shared {
    kind: FileKind,
    /// The name the live database has for the file.
    live: PathBuf,
    /// The name the checkpoint has for it.
    linked: PathBuf,
    /// The length the checkpoint's manifest gives it.
    named_len: u64,
    /// Whether the live database appends to it.
    grows: bool,
    /// The appends that are still to come, in order.
    next: VecDeque<Vec<u8>>,
    /// How long the file was at each open of it for reading under the
    /// checkpoint's name.
    seen: Vec<u64>,
}

/// A file layer on which the live database makes its next append to each
/// file it is still writing, of those it shares with a checkpoint, before
/// every open for reading of a file of the checkpoint: between any two
/// looks that an open of the checkpoint takes at a file.
#[derive(Debug)]
struct Beside {
    checkpoint: PathBuf,
    shared: Mutex<Vec<Shared>>,
}

impl Beside {
    fn append_all(&self, opened: &Path) -> io::Result<()> {
        let mut shared = self.shared.lock().expect("the files");
        for file in shared.iter_mut() {
            if let Some(bytes) = file.next.pop_front() {
                let mut live = OpenOptions::new().append(true).open(&file.live)?;
                live.write_all(&bytes)?;
            }
            if file.linked == opened {
                file.seen.push(std::fs::metadata(opened)?.len());
            }
        }
        Ok(())
    }

    /// What became of the file of `kind` that the live database appends
    /// to: how many appends it took, and how long it was at each open of
    /// it, which was longer every time than the checkpoint names it.
    fn grown(&self, kind: FileKind) -> (usize, Vec<u64>) {
        let shared = self.shared.lock().expect("the files");
        let mut of_kind = shared.iter().filter(|file| file.kind == kind && file.grows);
        let file = of_kind.next().expect("a file of the kind");
        assert!(
            of_kind.next().is_none(),
            "one file of the kind {kind:?} grows"
        );
        assert!(
            file.seen.iter().all(|&len| len > file.named_len),
            "{kind:?}, named with {} bytes: {:?}",
            file.named_len,
            file.seen
        );
        (APPENDS - file.next.len(), file.seen.clone())
    }
}

impl Vfs for Beside {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        if matches!(mode, OpenMode::Read) && path.starts_with(&self.checkpoint) {
            self.append_all(path)?;
        }
        StdVfs.open(path, mode)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdVfs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        StdVfs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock_existing(path)
    }
}

/// The groups of the next `count` commits of a database whose log ends at
/// the offset `end` of a file with the start record `start`, after the
/// commit `latest`. Each is a commit that changes nothing (LG17 of
/// `log-format.md`): a group of the next version, at its address.
fn next_groups(start: LogStart, end: u64, latest: CommitVersion, count: usize) -> Vec<Vec<u8>> {
    let (mut at, mut version) = (end, latest);
    let group = |_| {
        version = version.next().expect("a version");
        let address = start.address(at).expect("an address");
        let mut bytes = Vec::new();
        GroupWriter::new(version, address, &mut bytes)
            .expect("a group")
            .finish();
        at += bytes.len() as u64;
        bytes
    };
    (0..count).map(group).collect()
}

/// How many appends the live database has ready for each file.
const APPENDS: usize = 200;

/// Makes the directory of a checkpoint of the database in `live`, whose
/// session has ended, as a creation at that moment makes it: a hard link
/// to every file the manifest names, the log's files up to the end of the
/// log, and a manifest that names each one closed, or sealed if it was,
/// at the length it has for the live database. Returns the directory, and
/// the file layer on which the live database goes on appending.
fn link(live: &Dir, log_end: LogPlace) -> (TempDir, Arc<Beside>) {
    let sampled = read_manifest(&*live.vfs, &live.path).expect("a manifest");
    let log = LogReader::new(live.vfs.clone(), &live.path, &sampled)
        .into_end()
        .expect("the log reads");
    assert_eq!(log.place(), Some(log_end));
    let last = *log.files().last().expect("a log file");
    let target = tempfile::tempdir().expect("a directory");
    let mut shared = Vec::new();
    let mut take = |names: FileNames, kind: FileKind, id, named_len, next: Vec<Vec<u8>>| {
        let (original, linked) = (names.path(&live.path, id), names.path(target.path(), id));
        std::fs::create_dir_all(names.dir(target.path())).expect("a directory");
        std::fs::hard_link(&original, &linked).expect("a link");
        shared.push(Shared {
            kind,
            live: original,
            linked,
            named_len,
            grows: !next.is_empty(),
            next: next.into(),
            seen: Vec::new(),
        });
    };

    let table = sampled.table_checkpoint();
    if let Some(table) = table {
        let (id, len) = (table.file(), table.valid_len());
        take(
            FileNames::PAGE_TABLE,
            FileKind::PageTable,
            id,
            len,
            Vec::new(),
        );
    }
    let mut files = Vec::new();
    for file in sampled.files() {
        let (names, state) = match (file.kind(), file.state()) {
            (FileKind::Wal, _) => continue,
            (FileKind::Segment, FileState::Sealed) => (FileNames::SEGMENT, FileState::Sealed),
            (FileKind::Segment, _) => (FileNames::SEGMENT, FileState::Closed),
            (FileKind::ValueLog, _) => (FileNames::VALUE_LOG, FileState::Closed),
            (kind, _) => panic!("a manifest names a file of the kind {kind:?}"),
        };
        // What the live database writes behind the length it is named
        // with is no record that a reader of this manifest is given.
        let next = match state {
            FileState::Sealed => Vec::new(),
            _ => vec![vec![0xA5; 96]; APPENDS],
        };
        let (kind, id, len) = (file.kind(), file.id(), file.valid_len());
        take(names, kind, id, len, next);
        files.push(LiveFile::new(kind, id, state, len).expect("named"));
    }
    for file in log.files() {
        let (id, len) = (file.file(), file.valid_len());
        let next = if id == last.file() {
            next_groups(last.start(), len, log.latest(), APPENDS)
        } else {
            Vec::new()
        };
        take(FileNames::WAL, FileKind::Wal, id, len, next);
        files.push(LiveFile::new(FileKind::Wal, id, FileState::Closed, len).expect("named"));
    }

    let first = Manifest::new(sampled.page_size(), sampled.last_file_ids(), table, files)
        .expect("a manifest");
    ManifestStore::create(live.vfs.clone(), target.path(), first).expect("a manifest");
    let beside = Beside {
        checkpoint: target.path().to_path_buf(),
        shared: Mutex::new(shared),
    };
    (target, Arc::new(beside))
}

#[test]
fn a_checkpoint_recovers_while_the_live_database_appends_to_every_file_they_share() {
    let live = Dir::new();
    let small = RecoveryOptions {
        log: LogOptions {
            target_file_len: 200,
            ..LogOptions::default()
        },
        ..options()
    };
    let mut session = Session::begin_with(&live, Model::default(), &small);
    session.commit(&[
        insert((1, 1), b"a", long(1, 600)),
        insert((2, 1), b"b", long(2, 700)),
    ]);
    session.place_all(&[(1, 1), (2, 1)]);
    session.write_back(1, 1);
    session.write_back(2, 1);
    session.turn();
    session.checkpoint(1);
    // Behind the table checkpoint, for replay: a commit with a value that
    // is placed, and a commit that makes a page.
    session.commit(&[
        insert((1, 2), b"e", inline(&[6; 30])),
        insert((2, 2), b"c", long(3, 800)),
    ]);
    session.place((2, 2));
    session.commit(&[insert((3, 1), b"d", inline(&[5; 40]))]);
    session.sync();
    let sampled = session.latest();
    let log_end = session.store.log.end().expect("the end of the log");
    let model = session.kill();

    let (target, beside) = link(&live, log_end);
    let open = || {
        let manifest = ManifestStore::open(beside.clone(), target.path()).expect("a manifest");
        recover(manifest, &options())
            .unwrap_or_else(|error| panic!("the checkpoint does not recover: {error}"))
    };
    let store = open();
    assert_eq!(store.landed(), sampled);
    assert_eq!(store.log.latest(), sampled);
    assert_eq!(store.report.commits, 2);
    check(&store, &model, &[]);

    // The last log file was opened twice, by the reader and at the
    // adoption, and it had grown before each open. The segment and the
    // value-log file were opened too, each longer than it is named.
    let (commits, log) = beside.grown(FileKind::Wal);
    assert!(
        log.len() >= 2 && log.windows(2).all(|two| two[0] < two[1]),
        "{log:?}"
    );
    for kind in [FileKind::Segment, FileKind::ValueLog] {
        assert!(!beside.grown(kind).1.is_empty(), "{kind:?}");
    }
    // What the log file took meanwhile is whole commits of the live
    // database: a reader of that database's log is given every one.
    let theirs = read_manifest(&*live.vfs, &live.path).expect("a manifest");
    let theirs = LogReader::new(live.vfs.clone(), &live.path, &theirs)
        .into_end()
        .expect("the live log reads");
    assert_eq!(theirs.latest().get(), sampled.get() + commits as u64);

    // The checkpoint is opened again, as the database it is by now, and
    // the files it shares still grow.
    drop(store);
    let again = open();
    assert_eq!(again.landed(), sampled);
    check(&again, &model, &[]);
    for kind in [FileKind::Wal, FileKind::Segment, FileKind::ValueLog] {
        let (appends, looks) = beside.grown(kind);
        assert!(appends < APPENDS, "{kind:?}: the appends ran out");
        assert!(looks.len() >= 2, "{kind:?}: {looks:?}");
    }
}
