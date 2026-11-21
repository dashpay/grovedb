//! What a recovery makes durable, sync by sync: which files and
//! directories it syncs, in which order, and which of the syncs it hands to
//! the file layer as one set ([`Vfs::sync_together`]), so that a file layer
//! whose sync ends in a flush of the whole device makes that flush once
//! for the set.

use std::{any::Any, io, sync::Mutex};

use lignin_io::{DirEntry, OpenMode, SyncItem, VfsFile, VfsLock};

use super::*;
use crate::segment::FileNames;

/// The syncs of one call of the file layer, each as what was synced and
/// its path in the database directory: one for a single sync, and as many
/// as the list had for a set.
type Call = Vec<String>;

/// A file layer over the operating system's files that keeps a list of the
/// syncs it is asked for, call by call.
#[derive(Debug)]
struct Recording {
    db: PathBuf,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl Recording {
    fn new(db: &Path) -> Arc<Self> {
        Arc::new(Self {
            db: db.to_path_buf(),
            calls: Arc::default(),
        })
    }

    fn name(&self, path: &Path) -> String {
        let inside = path.strip_prefix(&self.db).expect("in the database");
        match inside.to_string_lossy().into_owned() {
            name if name.is_empty() => ".".to_owned(),
            name => name,
        }
    }

    /// The calls since the last time this was asked.
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut self.calls.lock().expect("the calls"))
    }
}

#[derive(Debug)]
struct Recorded {
    inner: Box<dyn VfsFile>,
    name: String,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl VfsFile for Recorded {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.inner.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        let call = vec![format!("Data {}", self.name)];
        self.calls.lock().expect("the calls").push(call);
        self.inner.sync_data()
    }

    fn sync_all(&self) -> io::Result<()> {
        let call = vec![format!("All {}", self.name)];
        self.calls.lock().expect("the calls").push(call);
        self.inner.sync_all()
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}

/// The file of this file layer that `file` is.
fn recorded(file: &dyn VfsFile) -> &Recorded {
    let any: &dyn Any = file;
    any.downcast_ref().expect("a file of this file layer")
}

impl Vfs for Recording {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        Ok(Box::new(Recorded {
            inner: StdVfs.open(path, mode)?,
            name: self.name(path),
            calls: self.calls.clone(),
        }))
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
        let call = vec![format!("Dir {}", self.name(path))];
        self.calls.lock().expect("the calls").push(call);
        StdVfs.sync_dir(path)
    }

    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        let mut call = Call::new();
        for item in items {
            match item {
                SyncItem::Data(file) => {
                    let file = recorded(*file);
                    call.push(format!("Data {}", file.name));
                    file.inner.sync_data()?;
                }
                SyncItem::All(file) => {
                    let file = recorded(*file);
                    call.push(format!("All {}", file.name));
                    file.inner.sync_all()?;
                }
                SyncItem::Dir(path) => {
                    call.push(format!("Dir {}", self.name(path)));
                    StdVfs.sync_dir(path)?;
                }
            }
        }
        self.calls.lock().expect("the calls").push(call);
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock_existing(path)
    }
}

/// A store with something in every kind of file, whose session was killed:
/// a value in the value log, a page in a segment, a table checkpoint, and
/// a commit behind it that only the log holds.
fn killed() -> (Dir, Model) {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", long(1, 600))]);
    session.place_all(&[(1, 1)]);
    session.write_back(1, 1);
    session.checkpoint(1);
    session.commit(&[insert((2, 1), b"b", inline(&[3; 20]))]);
    session.sync();
    let model = session.kill();
    (dir, model)
}

/// Recovers the store of `dir` through a file layer that records its
/// syncs. Returns the store, the calls the recovery made, and the last
/// two of them as every recovery makes them: this session's first log
/// file with its name and the manifest that names it, as one set, and then
/// the manifest under its name.
fn recovered(dir: &Dir) -> (Recovered, Vec<Call>, [Call; 2]) {
    let vfs = Recording::new(&dir.path);
    let manifest = ManifestStore::open(vfs.clone(), &dir.path).expect("a manifest");
    let store = recover(manifest, &options())
        .unwrap_or_else(|error| panic!("the store does not recover: {error}"));
    let first = store.log.end().expect("a log file").file();
    let log_file = format!("Data wal/{}", FileNames::WAL.file_name(first));
    let last = [
        call(&[&log_file, "Dir wal", "All MANIFEST.tmp"]),
        call(&["Dir ."]),
    ];
    (store, vfs.take(), last)
}

fn call(syncs: &[&str]) -> Call {
    syncs.iter().map(|sync| (*sync).to_owned()).collect()
}

#[test]
fn a_recovery_of_a_store_that_was_closed_makes_five_syncs_in_three_calls() {
    let (dir, model) = killed();
    // A session that ends as a close ends it: the log is durable to its
    // end, and a manifest names every file as it is.
    {
        let mut store = dir.open();
        store.log.sync().expect("the log is durable");
        store.log.commit(&mut store.manifest).expect("named");
    }

    let (store, calls, [named, installed]) = recovered(&dir);
    check(&store, &model, &[]);
    // First the manifest that was found, and with it the name of every
    // directory beside it. Then the two calls of every recovery.
    assert_eq!(calls, [call(&["Dir ."]), named, installed]);
}

#[test]
fn a_recovery_after_a_kill_adds_the_syncs_of_what_the_session_left_open() {
    let (dir, model) = killed();
    let (store, calls, last) = recovered(&dir);
    check(&store, &model, &[]);
    assert_eq!(calls.first(), Some(&call(&["Dir ."])));
    assert_eq!(calls[calls.len() - 2..], last);
    // Between them: the manifest that closes the segment and the
    // value-log file the session left open, and the end of its log file,
    // which no sync is known to cover, written again under its name.
    let between: Vec<&str> = calls[1..calls.len() - 2]
        .iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let rewritten = between.iter().position(|sync| sync.ends_with(".log.tmp"));
    let rewritten = rewritten.expect("the log file is written again");
    assert!(between[rewritten].starts_with("All wal/"), "{between:?}");
    assert_eq!(between[rewritten + 1], "Dir wal", "{between:?}");
    assert_eq!(
        between[..rewritten],
        ["All MANIFEST.tmp", "Dir ."],
        "one manifest closes what was left open"
    );
    assert_eq!(between.len(), rewritten + 2, "{between:?}");
}
