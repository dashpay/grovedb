//! The tests of the checkpoints, over the store of the write transaction's
//! tests (`txn/tests/store.rs`). A checkpoint is opened as that store opens
//! any directory, by the store's recovery.

mod acceptance;
mod cleaner;
mod crash;
mod files;
mod opener;
mod races;
mod reference;

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use lignin_format::{
    flat_page::{FlatPage, FlatValue},
    tree_page::{Hash, Side},
    value_log::ValuePlace,
    CommitVersion, FileKind, PageId,
};
use lignin_io::{
    sim::{SimConfig, SimVfs},
    DirEntry, OpenMode, SyncItem, Vfs, VfsFile, VfsLock,
};
use lignin_store::{
    alloc::PageState,
    flush::FlushConfig,
    load::{LoadBuffers, FRAME_FORMAT},
    recovery::{RecoveryError, RecoveryReport},
    segment::FileNames,
    wal::LogOptions,
};
use lignin_tree::node::{Lease, NodePage};

use super::{files::building_of, CheckpointOptions, Checkpointer};
use crate::{
    snapshot::Versions,
    txn::{
        tests::{
            model::{self, Committed, OwnedBatch},
            put,
            store::{Db, NoSync, Outcome, Rng, SIZE},
            Tx, World,
        },
        Column, Unit,
    },
};

/// The root Merk of the grove of these tests, the Merk of the documents
/// and the Merk of the references to them.
pub(super) const ROOT: u8 = 50;
pub(super) const DOCUMENTS: u8 = 51;
pub(super) const REFERENCES: u8 = 52;

type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// What the element of a tree holds of the Merk below it.
fn element(items: &Items) -> Vec<u8> {
    let parts: Vec<&[u8]> = items
        .iter()
        .flat_map(|(key, value)| [&key[..], &value[..]])
        .collect();
    model::hash_of(&parts).to_vec()
}

pub(super) fn document_key(serial: u64) -> Vec<u8> {
    let scattered = model::hash_of(&[&serial.to_be_bytes()]);
    [&scattered[..8], &serial.to_be_bytes()[..]].concat()
}

pub(super) fn reference_key(serial: u64) -> Vec<u8> {
    format!(
        "by-age/{:04}/{serial:08}",
        model::hash_of(&[b"age", &serial.to_le_bytes()])[0]
    )
    .into_bytes()
}

/// The unit of the aux records of the grove's root.
pub(super) fn aux() -> Unit {
    model::unit(Column::Aux, ROOT)
}

/// The batch of one operation that changes the documents and the
/// references: the change sets of the two Merks, that of the root Merk,
/// whose elements change with them, the base root key if the root Merk's
/// root changes, and an aux record that says how far the grove has come,
/// as Platform keeps its state (P-26 of `platform-requirements.md`).
pub(super) fn grove_batch<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    documents: impl FnOnce(&mut Items),
    references: impl FnOnce(&mut Items),
    state: &[u8],
) -> OwnedBatch {
    let (mut held_documents, mut held_references) = (Vec::new(), Vec::new());
    let changed_documents = world.merk(tx, DOCUMENTS, |items| {
        documents(items);
        held_documents = element(items);
    });
    let changed_references = world.merk(tx, REFERENCES, |items| {
        references(items);
        held_references = element(items);
    });
    let root = world.merk(tx, ROOT, |items| {
        items.insert(b"documents".to_vec(), held_documents);
        items.insert(b"references".to_vec(), held_references);
    });
    let mut records: Vec<_> = World::root_record(&root).into_iter().collect();
    records.push(put(aux(), b"state", state));
    OwnedBatch {
        merks: vec![changed_documents, changed_references, root],
        records,
        ..OwnedBatch::default()
    }
}

/// Writes `count` documents from `first` on, with their references,
/// through `tx`: each of `len` bytes, which is out of line from 400 on.
pub(super) fn write_documents<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    rng: &mut Rng,
    first: u64,
    count: u64,
    len: usize,
) {
    let serials = first..first + count;
    let batch = grove_batch(
        world,
        tx,
        |items| {
            for serial in serials.clone() {
                items.insert(document_key(serial), model::value(rng, len));
            }
        },
        |items| {
            for serial in serials.clone() {
                items.insert(reference_key(serial), document_key(serial));
            }
        },
        format!("{} documents", first + count).as_bytes(),
    );
    world.apply(tx, &batch).expect("the documents are applied");
}

/// Commits `count` documents of 400 bytes from `first` on, with their
/// references, as one block, and returns its version.
pub(super) fn block<'s>(
    world: &mut World<'s>,
    rng: &mut Rng,
    first: u64,
    count: u64,
) -> CommitVersion {
    let tx = world.begin();
    write_documents(world, &tx, rng, first, count, 400);
    match world.commit(tx, &[]) {
        Outcome::Committed(version) => version,
        outcome => panic!("a block was not committed: {outcome:?}"),
    }
}

/// Removes the documents from `first` on, and their references, as one
/// block.
pub(super) fn remove_block<'s>(world: &mut World<'s>, first: u64, count: u64) -> CommitVersion {
    let tx = world.begin();
    let serials = first..first + count;
    let batch = grove_batch(
        world,
        &tx,
        |items| {
            for serial in serials.clone() {
                items.remove(&document_key(serial));
            }
        },
        |items| {
            for serial in serials.clone() {
                items.remove(&reference_key(serial));
            }
        },
        format!("without {first} to {}", first + count).as_bytes(),
    );
    world.apply(&tx, &batch).expect("the removal is applied");
    match world.commit(tx, &[]) {
        Outcome::Committed(version) => version,
        outcome => panic!("a block was not committed: {outcome:?}"),
    }
}

/// Options that never wait for write-back.
pub(super) const NO_CATCH_UP: CheckpointOptions = CheckpointOptions {
    catch_up_above: None,
};

/// What makes checkpoints of `db`, with its defaults.
pub(super) fn checkpointer<'s>(db: &'s Db, versions: &'s Versions) -> Checkpointer<'s> {
    Checkpointer::new(&db.committer(versions))
}

/// A directory for checkpoints inside the directory of `db`, as Platform
/// keeps its own (P-14), on the file layer of `db`.
pub(super) fn checkpoints_in(db: &Db) -> PathBuf {
    let dir = db.dir.join("checkpoints");
    match db.vfs.create_dir(&dir) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
            panic!("the directory of the checkpoints: {error}")
        }
        _ => {}
    }
    (db.vfs.sync_dir(&db.dir)).expect("its name is durable");
    dir
}

/// The directory that a checkpoint at `target` is built in.
pub(super) fn building(target: &Path) -> PathBuf {
    building_of(target).expect("a target with a name")
}

/// Whether nothing has the name `path` in `vfs`.
pub(super) fn gone(vfs: &dyn Vfs, path: &Path) -> bool {
    matches!(vfs.read_dir(path), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

/// A database that was opened from a directory.
pub(super) struct Opened {
    pub(super) db: Arc<Db>,
    pub(super) versions: Versions,
    pub(super) report: RecoveryReport,
}

/// Opens the directory `dir` of `vfs` as the store opens any directory.
/// `made` is the store the directory is a checkpoint of, or the one that
/// made it.
pub(super) fn open_on(vfs: Arc<dyn Vfs>, dir: &Path, made: &Db) -> Result<Opened, RecoveryError> {
    let options = LogOptions::default();
    let (db, versions, report) = Db::open(vfs, dir, 256, options, FlushConfig::new(SIZE), made)?;
    Ok(Opened {
        db,
        versions,
        report,
    })
}

/// Opens the checkpoint of `made` in `dir`.
pub(super) fn open(made: &Db, dir: &Path) -> Opened {
    open_on(made.vfs.clone(), dir, made).expect("the checkpoint opens")
}

impl Opened {
    /// The opened database with the model of what it holds.
    pub(super) fn world(&self, model: &Committed) -> World<'_> {
        let mut world = World::new(&self.db, &self.versions);
        assert_eq!(
            world.latest().get(),
            model.version,
            "the version of the model"
        );
        world.model = model.clone();
        world
    }

    /// Checks that the database holds exactly what `model` says: every
    /// record by its key and under a cursor, every node with its hashes,
    /// and the tree pages.
    pub(super) fn check(&self, model: &Committed) {
        self.world(model).check_committed();
        assert_eq!(counted(&self.db), pointers_held(&self.db, &self.versions));
    }
}

/// The root hash of the grove at the version `at`: the hash of the root
/// node of the root Merk, made of what its cell stores as the hash rules of
/// these tests make it. `None` for a grove with no node.
pub(super) fn root_hash(db: &Db, versions: &Versions, at: CommitVersion) -> Option<Hash> {
    let root = *db.roots(versions, at).get(&model::subtree(ROOT))?;
    let _guard = versions.guard();
    let frame = (db.loader())
        .get(root.page(), at, &mut LoadBuffers::new())
        .expect("the page of the root");
    let page = NodePage::open(&frame, Lease::take()).expect("a tree page");
    let node = page.node(root.slot()).expect("the root's cell");
    let none = [0; 32];
    let child = |side| node.link(side).map_or(&none, |link| link.hash);
    Some(model::hash_of(&[
        node.kv_hash(),
        child(Side::Left),
        child(Side::Right),
    ]))
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

/// For each value-log file, how many cells of the pages of the latest
/// version hold a pointer into it: tree cells and the entries of flat
/// leaves. It is what the counts of live values say where they agree with
/// the pages (`transactions.md` T-CK4).
pub(super) fn pointers_held(db: &Db, versions: &Versions) -> BTreeMap<u32, u64> {
    let mut held = BTreeMap::new();
    let mut count = |place: ValuePlace| {
        if let ValuePlace::Placed(pointer) = place {
            *held.entry(pointer.file().get()).or_default() += 1;
        }
    };
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
                        count(place);
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
                        count(place);
                    }
                }
            }
        }
    }
    held
}

/// What each version of a store holds, as the models of its commits say.
#[derive(Default)]
pub(super) struct History(Vec<Committed>);

impl History {
    /// Notes what the version of `model` holds.
    pub(super) fn note(&mut self, model: &Committed) {
        let at = model.version as usize;
        if self.0.len() <= at {
            self.0.resize(at + 1, Committed::default());
        }
        self.0[at] = model.clone();
    }

    pub(super) fn at(&self, version: CommitVersion) -> &Committed {
        let model = &self.0[version.get() as usize];
        assert_eq!(model.version, version.get(), "no model of {version}");
        model
    }
}

/// An empty store in a temporary directory of its own, whose log turns to
/// another file every `log_file_len` bytes. Its syncs do nothing, as those
/// of [`Db::new`]: what a crash leaves of a checkpoint is tested on the
/// simulated file system ([`store_on`]).
pub(super) fn store_with_log(log_file_len: u64, frames: usize) -> (Arc<Db>, Versions) {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().to_path_buf();
    let options = LogOptions {
        target_file_len: log_file_len,
        ..LogOptions::default()
    };
    Db::on(
        Arc::new(NoSync),
        &path,
        frames,
        options,
        FlushConfig::new(SIZE),
        Some(dir),
    )
}

/// What the background loop of write-back does between blocks: the log's
/// files are kept going and a pass writes back what it can.
pub(super) fn turn(db: &Db, versions: &Versions) {
    let write_back = db.write_back(versions);
    write_back.roll_log().expect("the log is rolled");
    write_back.pass().expect("a pass of write-back");
}

/// The directory of a store on a simulated file system.
pub(super) const DIR: &str = "/db";

/// A simulated file system with the directory of a store in it.
pub(super) fn sim(seed: u64) -> SimVfs {
    let fs = SimVfs::new(SimConfig::new(seed));
    fs.create_dir(Path::new(DIR)).expect("a directory");
    fs.sync_dir(Path::new("/")).expect("its name is durable");
    fs
}

/// An empty store on `fs`.
pub(super) fn store_on(fs: &SimVfs) -> (Arc<Db>, Versions) {
    let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
    let (options, flush) = (LogOptions::default(), FlushConfig::new(SIZE));
    Db::on(vfs, Path::new(DIR), 256, options, flush, None)
}

/// The numbered files of the store in the directory `dir` of `vfs`, each
/// by its path inside `dir`.
pub(super) fn store_files(vfs: &dyn Vfs, dir: &Path) -> BTreeSet<PathBuf> {
    let mut files = BTreeSet::new();
    for kind in ["wal", "seg", "val", "table"] {
        let Ok(entries) = vfs.read_dir(&dir.join(kind)) else {
            continue;
        };
        files.extend(
            entries
                .iter()
                .map(|entry| Path::new(kind).join(&entry.name)),
        );
    }
    files
}

/// The names of the numbered files of `kind` that a manifest lists.
pub(super) fn names_of(kind: FileKind) -> FileNames {
    match kind {
        FileKind::Wal => FileNames::WAL,
        FileKind::Segment => FileNames::SEGMENT,
        _ => FileNames::VALUE_LOG,
    }
}

/// The bytes of the file `path` of `vfs`.
pub(super) fn bytes_of(vfs: &dyn Vfs, path: &Path) -> Vec<u8> {
    let file = vfs.open(path, OpenMode::Read).expect("the file opens");
    let mut bytes = vec![0; file.len().expect("a length") as usize];
    file.read_exact_at(&mut bytes, 0).expect("the file is read");
    bytes
}

/// Locks that a test holds, or that are taken for it.
pub(super) type Locks = Arc<Mutex<Vec<Box<dyn VfsLock>>>>;

/// What a test has done right before an operation on a path. An error is
/// what the operation then fails with, undone.
pub(super) type Before = Box<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

/// Writes the new file `path` of `vfs` with `bytes`, durably but for its
/// name.
pub(super) fn write_file(vfs: &dyn Vfs, path: &Path, bytes: &[u8]) {
    let mode = OpenMode::Append(lignin_io::Create::New);
    let file = vfs.open(path, mode).expect("the file is created");
    file.append(bytes).expect("the file is written");
    file.sync_all().expect("the file is synced");
}

/// Everything under the directory `dir` of `vfs`: each file by its path
/// inside `dir` with its bytes, and each directory with none.
pub(super) fn everything_in(vfs: &dyn Vfs, dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(inside) = dirs.pop() {
        for entry in vfs.read_dir(&dir.join(&inside)).expect("a directory") {
            let path = inside.join(&entry.name);
            if entry.kind == lignin_io::EntryKind::Dir {
                dirs.push(path.clone());
                found.insert(path, None);
            } else {
                let bytes = bytes_of(vfs, &dir.join(&path));
                found.insert(path, Some(bytes));
            }
        }
    }
    found
}

/// A file layer that does what `inner` does, but for what a test has it
/// do otherwise.
pub(super) struct Meddling {
    pub(super) inner: Arc<dyn Vfs>,
    /// No hard link is made into this directory: one fails with this kind,
    /// as a file system does that cannot make it.
    pub(super) no_links: Option<(PathBuf, io::ErrorKind)>,
    /// Before the lock in a file is taken.
    pub(super) before_lock: Option<Before>,
    /// Before a file is removed.
    pub(super) before_remove: Option<Before>,
    /// Before a directory is listed.
    pub(super) before_list: Option<Before>,
    /// Before a directory is made.
    pub(super) before_create_dir: Option<Before>,
    /// Under this directory the file system keeps a name in another
    /// spelling than it is given in, as HFS+ does: a name that is given
    /// with [`ONE_LETTER`] is listed with [`TWO_LETTERS`], and either
    /// spelling names the same entry.
    pub(super) respelled: Option<PathBuf>,
    /// Whether the listings under [`Self::respelled`] are in the other
    /// spelling now: a test turns it on at a moment of its choice, as if
    /// somebody had renamed what is there to the other spelling then.
    pub(super) respelling: Arc<AtomicBool>,
    /// Under this directory the file system does not tell capital letters
    /// from small ones, as far as a rename of a name onto itself goes: a
    /// name that its directory does not list, and that differs in case
    /// alone from one it does list, is found there.
    pub(super) folds: Option<PathBuf>,
    /// The lock of a path at which [`Vfs::open`] made a new file with
    /// `Create::New` is refused, as it is where new files are made so that
    /// they cannot be opened again for writing.
    pub(super) made_read_only: Option<Mutex<BTreeSet<PathBuf>>>,
    /// What the listing of the directory above it leaves out, though it
    /// is there: a name that the file system finds and does not list so.
    pub(super) unlisted: Option<PathBuf>,
    /// How many syncs each set had that was asked for
    /// ([`Vfs::sync_together`]).
    pub(super) sets: Mutex<Vec<usize>>,
}

impl Meddling {
    /// `inner`, as it is but for the count of the sets.
    pub(super) fn plain(inner: Arc<dyn Vfs>) -> Self {
        Self {
            inner,
            no_links: None,
            before_lock: None,
            before_remove: None,
            before_list: None,
            before_create_dir: None,
            respelled: None,
            respelling: Arc::new(AtomicBool::new(true)),
            folds: None,
            made_read_only: None,
            unlisted: None,
            sets: Mutex::default(),
        }
    }

    /// `inner`, with the names under the directory `under` listed in
    /// another spelling than they are given in.
    pub(super) fn respelled(inner: Arc<dyn Vfs>, under: PathBuf) -> Self {
        Self {
            respelled: Some(under),
            ..Self::plain(inner)
        }
    }

    /// `inner`, with a name under the directory `under` found, by a rename
    /// onto itself, where one is listed that differs from it in case alone.
    pub(super) fn folding(inner: Arc<dyn Vfs>, under: PathBuf) -> Self {
        Self {
            folds: Some(under),
            ..Self::plain(inner)
        }
    }

    /// `inner`, with `path` left out of the listing of the directory
    /// above it.
    pub(super) fn not_listing(inner: Arc<dyn Vfs>, path: PathBuf) -> Self {
        Self {
            unlisted: Some(path),
            ..Self::plain(inner)
        }
    }

    /// `path` as the directory above it lists it, where that differs from
    /// the name given in the case of its letters alone and the file
    /// system takes the two for one name.
    fn folded(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let (Some(under), Some(dir), Some(name)) = (&self.folds, path.parent(), path.file_name())
        else {
            return Ok(None);
        };
        if !path.starts_with(under) {
            return Ok(None);
        }
        let folded = |name: &std::ffi::OsStr| name.to_string_lossy().to_lowercase();
        let beside = self.inner.read_dir(dir)?;
        if beside.iter().any(|entry| entry.name == name) {
            return Ok(None);
        }
        let listed = (beside.iter()).find(|entry| folded(&entry.name) == folded(name));
        Ok(listed.map(|entry| dir.join(&entry.name)))
    }

    /// Refuses a file that was made so that it cannot be opened again for
    /// writing ([`Self::made_read_only`]).
    fn writable(&self, path: &Path) -> io::Result<()> {
        match &self.made_read_only {
            Some(made) if made.lock().expect("the files").contains(path) => {
                Err(io::ErrorKind::PermissionDenied.into())
            }
            _ => Ok(()),
        }
    }

    /// `path` as `inner` is given it.
    fn kept(&self, path: &Path) -> PathBuf {
        kept_under(self.respelled.as_deref(), path)
    }

    /// `inner`, with no hard link into the directory `under`.
    pub(super) fn no_links(inner: Arc<dyn Vfs>, under: PathBuf, kind: io::ErrorKind) -> Self {
        Self {
            no_links: Some((under, kind)),
            ..Self::plain(inner)
        }
    }

    /// `inner`, with `before` run before each lock is taken.
    pub(super) fn before_lock(inner: Arc<dyn Vfs>, before: Before) -> Self {
        Self {
            before_lock: Some(before),
            ..Self::plain(inner)
        }
    }

    /// `inner`, with `before` run before each file is removed.
    pub(super) fn before_remove(inner: Arc<dyn Vfs>, before: Before) -> Self {
        Self {
            before_remove: Some(before),
            ..Self::plain(inner)
        }
    }

    /// `inner`, with `before` run before each directory is listed.
    pub(super) fn before_list(inner: Arc<dyn Vfs>, before: Before) -> Self {
        Self {
            before_list: Some(before),
            ..Self::plain(inner)
        }
    }
}

impl std::fmt::Debug for Meddling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Meddling")
            .field("inner", &self.inner)
            .field("no_links", &self.no_links)
            .finish_non_exhaustive()
    }
}

/// The letter `e` with an acute accent as one letter, and as the letter
/// and a combining accent: two spellings that HFS+ takes for one name. It
/// keeps and lists the second whichever it was given.
pub(super) const ONE_LETTER: &str = "\u{e9}";
pub(super) const TWO_LETTERS: &str = "e\u{301}";

/// `path` in the one spelling that the file layer under a [`Meddling`]
/// is given for a name under the directory `respelled`.
fn kept_under(respelled: Option<&Path>, path: &Path) -> PathBuf {
    match respelled {
        Some(under) if path.starts_with(under) => {
            let path = path.to_str().expect("a path of the tests");
            PathBuf::from(path.replace(TWO_LETTERS, ONE_LETTER))
        }
        _ => path.to_path_buf(),
    }
}

/// A guard of a [`Meddling`] that lists names in another spelling: it
/// reads a path as that file layer does.
#[derive(Debug)]
struct RespelledLock {
    lock: Box<dyn VfsLock>,
    under: PathBuf,
}

impl VfsLock for RespelledLock {
    fn is_at(&self, path: &Path) -> io::Result<bool> {
        self.lock.is_at(&kept_under(Some(&self.under), path))
    }
}

impl Meddling {
    fn guard(&self, lock: Box<dyn VfsLock>) -> Box<dyn VfsLock> {
        match &self.respelled {
            Some(under) => Box::new(RespelledLock {
                lock,
                under: under.clone(),
            }),
            None => lock,
        }
    }
}

impl Vfs for Meddling {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let file = self.inner.open(&self.kept(path), mode)?;
        let new = matches!(
            mode,
            OpenMode::Write(lignin_io::Create::New) | OpenMode::Append(lignin_io::Create::New)
        );
        if let (true, Some(made)) = (new, &self.made_read_only) {
            (made.lock().expect("the files")).insert(path.to_path_buf());
        }
        Ok(file)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match self.folded(from)? {
            Some(listed) if from == to => self.inner.rename(&listed, &listed),
            _ => self.inner.rename(&self.kept(from), &self.kept(to)),
        }
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        match &self.no_links {
            Some((under, kind)) if link.starts_with(under) => Err((*kind).into()),
            _ => (self.inner).hard_link(&self.kept(original), &self.kept(link)),
        }
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if let Some(before) = &self.before_remove {
            before(path)?;
        }
        self.inner.remove_file(&self.kept(path))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        if let Some(before) = &self.before_create_dir {
            before(path)?;
        }
        self.inner.create_dir(&self.kept(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(&self.kept(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        if let Some(before) = &self.before_list {
            before(path)?;
        }
        let mut entries = self.inner.read_dir(&self.kept(path))?;
        if let Some(unlisted) = &self.unlisted
            && unlisted.parent() == Some(path)
        {
            entries.retain(|entry| Some(&*entry.name) != unlisted.file_name());
        }
        if (self.respelled.as_deref()).is_some_and(|under| path.starts_with(under))
            && self.respelling.load(Ordering::SeqCst)
        {
            for entry in &mut entries {
                let name = entry.name.to_str().expect("a name of the tests");
                entry.name = name.replace(ONE_LETTER, TWO_LETTERS).into();
            }
            entries.sort_by(|one, other| one.name.cmp(&other.name));
        }
        Ok(entries)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(&self.kept(path))
    }

    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        (self.sets.lock().expect("the sets")).push(items.len());
        let dirs: Vec<Option<PathBuf>> = (items.iter())
            .map(|item| match item {
                SyncItem::Dir(path) => Some(self.kept(path)),
                _ => None,
            })
            .collect();
        let kept: Vec<SyncItem<'_>> = (items.iter().zip(&dirs))
            .map(|(item, dir)| match dir {
                Some(dir) => SyncItem::Dir(dir),
                None => *item,
            })
            .collect();
        self.inner.sync_together(&kept)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.writable(path)?;
        if let Some(before) = &self.before_lock {
            before(path)?;
        }
        Ok(self.guard(self.inner.lock(&self.kept(path))?))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.writable(path)?;
        if let Some(before) = &self.before_lock {
            before(path)?;
        }
        Ok(self.guard(self.inner.lock_existing(&self.kept(path))?))
    }
}
