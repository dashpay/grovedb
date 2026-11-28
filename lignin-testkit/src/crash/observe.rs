//! Every write boundary of a run, from one run.
//!
//! [`Observer`] is a [`Vfs`] over a [`SimVfs`]. It forwards every call,
//! and after each call that changed what a crash can leave it freezes the
//! file system as a [`CrashImage`], with what was expected of the store at
//! that moment. The image is what `SimVfs::crash_at(CrashPoint::AfterChange(n))`
//! would leave behind, without the run being made again for each `n`: a
//! crash inside an operation is no separate case, and nothing that happens
//! after the power is off changes what is on the device
//! (`lignin_io::sim`, "Crashing").
//!
//! It can also lie ([`Sabotage`]), which is how the campaign is shown to
//! find what it is there to find.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

use lignin_io::{
    sim::{CrashImage, SimVfs},
    Create, DirEntry, OpenMode, Vfs, VfsFile, VfsLock,
};

use super::model::Expect;

/// A defect planted in the file layer under the store: a call that reports
/// a success it did not deliver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sabotage {
    /// A sync of a file whose path contains `part` returns `Ok` and makes
    /// nothing durable, once `after` syncs through the same handle have
    /// been made: with 1, the sync that makes a new file's header durable
    /// is honest, and the syncs of what is appended later are not.
    LostSync {
        /// A part of the path.
        part: &'static str,
        /// How many syncs of a handle are made before the lies begin.
        after: u64,
    },
    /// A sync of a directory whose path ends in this text returns `Ok`
    /// and makes no change of names durable.
    LostDirSync(&'static str),
    /// Beside each handle that is opened to write a file whose path
    /// contains this text, the file layer opens a second one to read it,
    /// and makes through the second every sync that is asked of the first
    /// after its first sync. So the sync that makes a new file's header
    /// durable goes through the handle that wrote the header, and the
    /// syncs of what is appended later go through a handle that appended
    /// nothing: the two handles that the stores kept of a segment and of
    /// a value-log file before they synced through the one that appends.
    /// A sync through the second handle is a sync of the file, and its
    /// answer is given as it is. The second handle is told of no
    /// write-back that failed where the file layer tells the handles
    /// that wrote and no other (`lignin_io::sim::Told::Writers`). A
    /// handle whose second one could not be opened is synced through
    /// itself every time, as with no defect planted.
    OtherHandle(&'static str),
}

/// A crash image, with the moment it was taken at.
#[derive(Clone, Debug)]
pub struct Taken {
    /// What a crash at that moment has to work with.
    pub image: CrashImage,
    /// What was expected of the store then.
    pub expect: Expect,
    /// The number of the write boundary: how many operations had changed
    /// what a crash can leave.
    pub boundary: u64,
    /// What operations saw of the file system then
    /// ([`SimVfs::fingerprint`]), where a write-back has failed since the
    /// store was last recovered ([`Observer::recovered`]). Storage can
    /// then lack bytes that the session still reads and that no crash
    /// plan keeps, so the plan that keeps every unsynced change leaves
    /// what a killed process leaves only where its state is this one.
    /// `None` where no write-back has failed since.
    pub seen: Option<u64>,
}

struct Watch {
    changes: u64,
    expect: Expect,
    taken: Vec<Taken>,
    sabotage: Option<Sabotage>,
    lies: u64,
    /// How many faults the file system had injected at the last call.
    faults: u64,
    /// How many of them went into a call whose failure lasts.
    lasting: u64,
    /// How many removals of a file a fault failed.
    removals: u64,
    /// How many write-backs the file system had failed at the last call.
    write_backs: u64,
    /// Whether one has failed since the store was last recovered.
    write_back_failed: bool,
    /// The files whose write-back failed, each until a sync of it is
    /// answered.
    unreported: BTreeSet<u64>,
    /// How many handles were opened: each has its number.
    handles: u64,
    /// How many files were told apart: each has its number.
    files: u64,
    /// The file that each path names, of the paths a handle was opened by
    /// and those a rename or a link gave such a file.
    named: BTreeMap<PathBuf, u64>,
    /// The handles that wrote to each file since the last sync of it that
    /// was answered.
    written_by: BTreeMap<u64, BTreeSet<u64>>,
    /// The first sync that was asked through a handle of a file to which
    /// another handle had written since its last sync: the path that
    /// handle was opened by.
    foreign_sync: Option<PathBuf>,
}

impl Watch {
    /// The number of the file that `path` names.
    fn file_at(&mut self, path: &Path) -> u64 {
        if let Some(file) = self.named.get(path) {
            return *file;
        }
        self.files += 1;
        self.named.insert(path.to_path_buf(), self.files);
        self.files
    }

    /// The file or directory at `from` is at `to` now, in place of what
    /// was there. Where both paths name one file nothing has changed, as
    /// the file layer has it. A file that lost its name is still the file
    /// its open handles are on, and no file that is made later has its
    /// number.
    fn renamed(&mut self, from: &Path, to: &Path) {
        let one_file = (self.named.get(from)).is_some_and(|file| self.named.get(to) == Some(file));
        if from == to || one_file {
            return;
        }
        let under = |names: &BTreeMap<PathBuf, u64>, dir: &Path| -> Vec<PathBuf> {
            let inside = names.keys().filter(|name| name.starts_with(dir));
            inside.cloned().collect()
        };
        for replaced in under(&self.named, to) {
            self.named.remove(&replaced);
        }
        for name in under(&self.named, from) {
            let rest = name.strip_prefix(from).expect("it begins with the path");
            let moved = match rest.as_os_str().is_empty() {
                true => to.to_path_buf(),
                false => to.join(rest),
            };
            if let Some(file) = self.named.remove(&name) {
                self.named.insert(moved, file);
            }
        }
    }
}

/// Whether a sync that fails of the file at `path` lasts for the session of
/// the store. A failed sync cannot be made good (`lignin_io::vfs`), and
/// two kinds of file have nothing to fall back on:
///
/// - a file of the log: the log makes nothing more durable in the session,
///   and every later sync fails (`lignin_store::wal`, "When the file layer
///   fails", `LogError::SyncFailed`)
/// - a segment, write-back's or the cleaner's: chains point to blocks that
///   cannot be made durable, and no table checkpoint is installed until
///   the store is opened again (`lignin_store::flush` and
///   `lignin_store::cleaner`, "When something fails")
///
/// A value-log file whose sync fails is given up for another, a table
/// checkpoint is written again, and a manifest that was left in doubt is
/// opened again by the next call that needs it.
fn lasts(path: &Path) -> bool {
    let named = |part: std::path::Component<'_>| {
        let part = part.as_os_str();
        part == "wal" || part == "seg"
    };
    path.components().any(named)
}

/// The file layer of an observed run. A clone is another handle on the
/// same observer.
#[derive(Clone)]
pub struct Observer {
    fs: SimVfs,
    watch: Arc<Mutex<Watch>>,
}

impl fmt::Debug for Observer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Observer").field("fs", &self.fs).finish()
    }
}

impl Observer {
    /// An observer of `fs`, which expects `expect` of the store until it
    /// is told otherwise. What `fs` holds now is no boundary: the first
    /// image is taken after the next change.
    pub fn new(fs: SimVfs, expect: Expect) -> Self {
        let stats = fs.stats();
        let watch = Watch {
            changes: stats.changes,
            expect,
            taken: Vec::new(),
            sabotage: None,
            lies: 0,
            faults: stats.faults,
            lasting: 0,
            removals: 0,
            write_backs: stats.write_backs,
            write_back_failed: false,
            unreported: BTreeSet::new(),
            handles: 0,
            files: 0,
            named: BTreeMap::new(),
            written_by: BTreeMap::new(),
            foreign_sync: None,
        };
        Self {
            fs,
            watch: Arc::new(Mutex::new(watch)),
        }
    }

    /// The same, with a defect planted in the file layer.
    pub fn sabotaged(fs: SimVfs, expect: Expect, sabotage: Sabotage) -> Self {
        let observer = Self::new(fs, expect);
        observer.watch().sabotage = Some(sabotage);
        observer
    }

    fn watch(&self) -> MutexGuard<'_, Watch> {
        self.watch.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The file system under the observer.
    pub fn fs(&self) -> &SimVfs {
        &self.fs
    }

    /// The observer as the file layer a store is opened on.
    pub fn vfs(&self) -> Arc<dyn Vfs> {
        Arc::new(self.clone())
    }

    /// What is expected of the store now.
    pub fn expect(&self) -> Expect {
        self.watch().expect.clone()
    }

    /// Changes what is expected of the store from now on.
    pub fn expecting(&self, change: impl FnOnce(&mut Expect)) {
        change(&mut self.watch().expect);
    }

    /// The images taken since the last call, in the order of their
    /// boundaries.
    pub fn drain(&self) -> Vec<Taken> {
        std::mem::take(&mut self.watch().taken)
    }

    /// An image of the file system as it is now, whether or not anything
    /// changed since the last one.
    pub fn now(&self) -> Taken {
        let watch = self.watch();
        Taken {
            image: self.fs.crash_image(),
            expect: watch.expect.clone(),
            boundary: watch.changes,
            seen: watch.write_back_failed.then(|| self.fs.fingerprint()),
        }
    }

    /// Tells the observer that a recovery returned the store with no
    /// write-back failing meanwhile. What such a store reads of its files
    /// is durable: the manifest, the other files up to the lengths the
    /// manifest gives them, and the log to its end
    /// (`lignin_store::recovery`, step 9). So the images from here on are
    /// of a file system that has what the session reads, until the next
    /// write-back fails.
    pub fn recovered(&self) {
        self.watch().write_back_failed = false;
    }

    /// How many write boundaries the file system has passed.
    pub fn boundaries(&self) -> u64 {
        self.watch().changes
    }

    /// How many calls the planted defect answered with a lie.
    pub fn lies(&self) -> u64 {
        self.watch().lies
    }

    /// How many faults were injected that last for the session of the
    /// store: the syncs of a file of the log or of a segment that failed.
    /// Every other fault fails the call it was injected into, and what it
    /// leaves the store gets over in the same session: a file whose append
    /// or creation failed is given up for another, a deletion is made
    /// again, and a manifest whose replacement was left in doubt is opened
    /// again by the next call that needs it.
    pub fn lasting_faults(&self) -> u64 {
        self.watch().lasting
    }

    /// How many removals of a file a fault failed: the file stays, and the
    /// store removes it later or its next session does.
    pub fn removals_failed(&self) -> u64 {
        self.watch().removals
    }

    /// The path by which the handle was opened that first, since the last
    /// call, was asked for a sync of a file to which another handle of
    /// this observer had written since the last sync of the file that was
    /// answered. The file layer's contract has such a sync prove nothing
    /// of what the other handle wrote (`lignin_io::vfs`, "A write-back can
    /// fail unreported").
    ///
    /// - A handle has written to a file once it has made a call that can
    ///   change it: a `write_at` or an `append` of one byte or more, or a
    ///   `set_len` to another length than the file has, whatever the call
    ///   returned. A call that failed can have left bytes. The observer
    ///   asks the file for its length before a `set_len`, which is one
    ///   call of the file layer more than its caller made; a length it is
    ///   not told counts as another.
    /// - What was written counts until a sync of the file is answered:
    ///   with `Ok`, which covers it absent a failed write-back, or with an
    ///   error, after which no sync covers it (`lignin_io::vfs`,
    ///   "Durability").
    /// - A file is followed from the first handle this observer opens on
    ///   it, under every name that a rename or a link gives it. It is
    ///   another file than one that is made under one of its names later.
    /// - The campaign calls the file layer from one thread. What another
    ///   thread wrote while a sync was in flight would be taken for
    ///   written before it.
    pub fn take_foreign_sync(&self) -> Option<PathBuf> {
        self.watch().foreign_sync.take()
    }

    /// The handle `handle` wrote to the file `file`.
    fn wrote(&self, file: u64, handle: u64) {
        let mut watch = self.watch();
        watch.written_by.entry(file).or_default().insert(handle);
    }

    /// A sync of the file `file` is asked through the handle `handle`,
    /// which was opened by `path`.
    fn syncing(&self, path: &Path, file: u64, handle: u64) {
        let mut watch = self.watch();
        let foreign = (watch.written_by.get(&file))
            .is_some_and(|by| by.iter().any(|writer| *writer != handle));
        if foreign && watch.foreign_sync.is_none() {
            watch.foreign_sync = Some(path.to_path_buf());
        }
    }

    /// A sync of the file `file` was answered: nothing that was written
    /// before it is left for a later sync to cover, and a write-back of
    /// the file that failed is behind it.
    fn synced(&self, file: u64) {
        let mut watch = self.watch();
        watch.written_by.remove(&file);
        watch.unreported.remove(&file);
    }

    /// The planted defect had a sync of the file `file` made through the
    /// handle beside the one it was asked of, which answered `synced`. The
    /// defect has acted where the answer is `Ok` and a write-back of the
    /// file had failed with no sync of the file answered since: the handle
    /// that wrote would have been told.
    fn substituted(&self, file: u64, synced: &io::Result<()>) {
        let mut watch = self.watch();
        watch.lies += u64::from(synced.is_ok() && watch.unreported.contains(&file));
    }

    /// Looks at the file system after a call on `path`, a call on the
    /// open file `file` if it names one, and a sync of that file if
    /// `synced`: a fault that was injected into the call is counted, and
    /// an image is taken if the call changed what a crash can leave.
    fn after(&self, path: &Path, file: Option<u64>, synced: bool) {
        let mut watch = self.watch();
        let stats = self.fs.stats();
        if stats.faults != watch.faults {
            watch.faults = stats.faults;
            watch.lasting += u64::from(synced && lasts(path));
        }
        if stats.write_backs != watch.write_backs {
            // A write-back fails after an operation on its open file.
            watch.write_backs = stats.write_backs;
            watch.write_back_failed = true;
            watch.unreported.extend(file);
        }
        if stats.changes != watch.changes {
            watch.changes = stats.changes;
            let taken = Taken {
                image: self.fs.crash_image(),
                expect: watch.expect.clone(),
                boundary: stats.changes,
                seen: watch.write_back_failed.then(|| self.fs.fingerprint()),
            };
            watch.taken.push(taken);
        }
    }

    fn observed<T>(&self, path: &Path, result: io::Result<T>) -> io::Result<T> {
        self.after(path, None, false);
        result
    }

    /// Whether the planted defect swallows a sync of the file at `path`,
    /// through a handle that has made `made` syncs.
    fn loses_sync(&self, path: &Path, made: u64) -> bool {
        let mut watch = self.watch();
        let lost = match watch.sabotage {
            Some(Sabotage::LostSync { part, after }) => {
                made >= after && path.to_string_lossy().contains(part)
            }
            _ => false,
        };
        watch.lies += u64::from(lost);
        lost
    }

    /// The handle that the planted defect opens beside one that was just
    /// opened by `path` in `mode`, if it opens one: the syncs of the first
    /// after its first are made through it.
    fn handle_beside(&self, path: &Path, mode: OpenMode) -> Option<Box<dyn VfsFile>> {
        let sabotage = self.watch().sabotage;
        match sabotage {
            Some(Sabotage::OtherHandle(part))
                if mode != OpenMode::Read && path.to_string_lossy().contains(part) =>
            {
                self.fs.open(path, OpenMode::Read).ok()
            }
            _ => None,
        }
    }

    fn loses_dir_sync(&self, path: &Path) -> bool {
        let mut watch = self.watch();
        let lost = match watch.sabotage {
            Some(Sabotage::LostDirSync(end)) => path.to_string_lossy().ends_with(end),
            _ => false,
        };
        watch.lies += u64::from(lost);
        lost
    }
}

impl Vfs for Observer {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let file = match self.fs.open(path, mode) {
            Ok(file) => file,
            Err(error) => return self.observed(path, Err(error)),
        };
        let (number, id) = {
            let mut watch = self.watch();
            if let OpenMode::Write(Create::New) | OpenMode::Append(Create::New) = mode {
                watch.named.remove(path);
            }
            watch.handles += 1;
            (watch.file_at(path), watch.handles)
        };
        let beside = self.handle_beside(path, mode);
        // An open is a call on its file: a write-back can fail after it.
        self.after(path, Some(number), false);
        Ok(Box::new(ObservedFile {
            file,
            beside,
            path: path.to_path_buf(),
            number,
            id,
            syncs: AtomicU64::new(0),
            observer: self.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let renamed = self.fs.rename(from, to);
        if renamed.is_ok() {
            self.watch().renamed(from, to);
        }
        self.observed(to, renamed)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        let linked = self.fs.hard_link(original, link);
        if linked.is_ok() {
            let mut watch = self.watch();
            let file = watch.file_at(original);
            watch.named.insert(link.to_path_buf(), file);
        }
        self.observed(link, linked)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let before = self.fs.stats().faults;
        let removed = self.fs.remove_file(path);
        if self.fs.stats().faults > before {
            self.watch().removals += 1;
        }
        if removed.is_ok() {
            self.watch().named.remove(path);
        }
        self.observed(path, removed)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.observed(path, self.fs.create_dir(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.observed(path, self.fs.remove_dir(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.observed(path, self.fs.read_dir(path))
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if self.loses_dir_sync(path) {
            return Ok(());
        }
        self.observed(path, self.fs.sync_dir(path))
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.observed(path, self.fs.lock(path))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.observed(path, self.fs.lock_existing(path))
    }
}

struct ObservedFile {
    file: Box<dyn VfsFile>,
    /// The handle that a planted defect opened beside this one
    /// ([`Sabotage::OtherHandle`]).
    beside: Option<Box<dyn VfsFile>>,
    path: PathBuf,
    /// The number of the file among those its observer tells apart.
    number: u64,
    /// The number of this handle among those of its observer.
    id: u64,
    /// How many syncs were asked of this handle.
    syncs: AtomicU64,
    observer: Observer,
}

impl ObservedFile {
    fn observed<T>(&self, result: io::Result<T>) -> io::Result<T> {
        (self.observer).after(&self.path, Some(self.number), false);
        result
    }

    /// Notes a call that was made and answered `made`: its handle has
    /// written to the file if `writes` says that the call can change it.
    fn change<T>(&self, writes: bool, made: io::Result<T>) -> io::Result<T> {
        if writes {
            self.observer.wrote(self.number, self.id);
        }
        self.observed(made)
    }

    fn sync(&self, sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>) -> io::Result<()> {
        let made = self.syncs.fetch_add(1, Ordering::SeqCst);
        (self.observer).syncing(&self.path, self.number, self.id);
        if self.observer.loses_sync(&self.path, made) {
            self.observer.synced(self.number);
            return Ok(());
        }
        let synced = match &self.beside {
            Some(beside) if made >= 1 => {
                let synced = sync(&**beside);
                self.observer.substituted(self.number, &synced);
                synced
            }
            _ => sync(&*self.file),
        };
        self.observer.synced(self.number);
        (self.observer).after(&self.path, Some(self.number), true);
        synced
    }
}

impl fmt::Debug for ObservedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObservedFile({})", self.path.display())
    }
}

impl VfsFile for ObservedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.observed(self.file.read_at(buf, offset))
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.change(!buf.is_empty(), self.file.write_at(buf, offset))
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.change(!buf.is_empty(), self.file.append(buf))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync(|file| file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync(|file| file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.observed(self.file.len())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        let another = self.file.len().map_or(true, |now| now != len);
        self.change(another, self.file.set_len(len))
    }
}

#[cfg(test)]
mod tests {
    use lignin_io::{
        sim::{CrashPoint, SimConfig},
        Create,
    };

    use super::*;

    fn fs() -> SimVfs {
        let fs = SimVfs::new(SimConfig::new(3));
        fs.create_dir(Path::new("/db")).expect("a directory");
        fs.sync_dir(Path::new("/")).expect("synced");
        fs
    }

    /// A short life of two files and a few names.
    fn run(vfs: &dyn Vfs) {
        let log = vfs
            .open(Path::new("/db/log"), OpenMode::Append(Create::New))
            .expect("opened");
        log.append(b"one").expect("appended");
        assert_eq!(log.len().expect("a length"), 3);
        log.sync_data().expect("synced");
        vfs.sync_dir(Path::new("/db")).expect("synced");
        log.append(b"two").expect("appended");
        let mut read = [0; 6];
        assert_eq!(log.read_at(&mut read, 0).expect("read"), 6);
        let page = vfs
            .open(Path::new("/db/page"), OpenMode::Write(Create::IfMissing))
            .expect("opened");
        page.write_at(&[7; 600], 0).expect("written");
        page.set_len(700).expect("grown");
        page.sync_all().expect("synced");
        vfs.rename(Path::new("/db/page"), Path::new("/db/page.2"))
            .expect("renamed");
        vfs.hard_link(Path::new("/db/page.2"), Path::new("/db/page.3"))
            .expect("linked");
        vfs.remove_file(Path::new("/db/page.2")).expect("removed");
        vfs.create_dir(Path::new("/db/sub")).expect("made");
        vfs.remove_dir(Path::new("/db/sub")).expect("removed");
        assert_eq!(vfs.read_dir(Path::new("/db")).expect("listed").len(), 2);
        drop(vfs.lock(Path::new("/db/LOCK")).expect("locked"));
    }

    #[test]
    fn an_image_is_taken_at_every_boundary_and_is_what_a_crash_there_leaves() {
        let observer = Observer::new(fs(), Expect::of_new_store());
        assert!(format!("{observer:?}").starts_with("Observer"));
        run(&*observer.vfs());
        let taken = observer.drain();
        let changes = observer.fs().stats().changes;
        assert_eq!(observer.boundaries(), changes);
        assert!(observer.drain().is_empty());
        // One image for each operation that changed what a crash can
        // leave, and none for the reads.
        let boundaries: Vec<u64> = taken.iter().map(|taken| taken.boundary).collect();
        let first = boundaries[0];
        assert_eq!(boundaries, (first..=changes).collect::<Vec<_>>());
        assert!(taken.len() >= 12, "{} images", taken.len());

        // The power going off after that many changes leaves the same.
        for taken in &taken {
            let again = fs();
            again.crash_at(CrashPoint::AfterChange(taken.boundary));
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&again)));
            assert!(again.is_powered_off(), "boundary {}", taken.boundary);
            let crashed = again.crash_image();
            for plan in [crashed.lose_all(), crashed.keep_all()] {
                let left = crashed.recover(&plan).expect("fits").fingerprint();
                let plans = [taken.image.lose_all(), taken.image.keep_all()];
                let same = plans
                    .iter()
                    .any(|plan| taken.image.recover(plan).expect("fits").fingerprint() == left);
                assert!(same, "boundary {}", taken.boundary);
            }
            assert_eq!(crashed.changes().len(), taken.image.changes().len());
            assert_eq!(crashed.names().len(), taken.image.names().len());
        }

        // An image of now is of the last boundary.
        let now = observer.now();
        assert_eq!(now.boundary, changes);
        assert!(!now.expect.created);
    }

    #[test]
    fn a_sync_of_the_log_or_of_a_segment_that_fails_lasts() {
        use lignin_io::sim::{Fault, FaultRule, OpSet};

        let observer = Observer::new(fs(), Expect::of_new_store());
        let (fs, vfs) = (observer.fs().clone(), observer.vfs());
        for dir in ["/db/wal", "/db/seg", "/db/val"] {
            vfs.create_dir(Path::new(dir)).expect("made");
        }
        let append = OpenMode::Append(Create::New);
        let open = |path: &str| vfs.open(Path::new(path), append).expect("opened");
        let fail = |ops: OpSet| fs.add_rule(FaultRule::new(Fault::Eio).on(ops));
        let counts = || (fs.stats().faults, observer.lasting_faults());

        // What the store gets over in the same session: a write and a read
        // that fail, in the log as anywhere, the sync of a value-log file,
        // of the manifest and of a directory, and a deletion.
        let (log, values, manifest) = (
            open("/db/wal/1.log"),
            open("/db/val/1.val"),
            open("/db/MANIFEST.tmp"),
        );
        fail(OpSet::WRITES);
        assert!(log.append(b"item").is_err());
        fail(OpSet::READS);
        assert!(log.len().is_err());
        fail(OpSet::SYNCS);
        assert!(values.sync_data().is_err());
        fail(OpSet::SYNCS);
        assert!(manifest.sync_all().is_err());
        fail(OpSet::SYNCS);
        assert!(vfs.sync_dir(Path::new("/db/wal")).is_err());
        assert_eq!(observer.removals_failed(), 0);
        fail(OpSet::NAMES);
        assert!(vfs.remove_file(Path::new("/db/wal/1.log")).is_err());
        assert_eq!(counts(), (6, 0));
        // The removal that failed is counted, and the one that is made
        // again is not.
        assert_eq!(observer.removals_failed(), 1);
        vfs.remove_file(Path::new("/db/wal/1.log"))
            .expect("removed");
        assert_eq!(observer.removals_failed(), 1);

        // What lasts: the sync of a file of the log, and of a segment.
        let (log, segment) = (open("/db/wal/2.log"), open("/db/seg/1.seg"));
        fail(OpSet::SYNCS);
        assert!(log.sync_data().is_err());
        assert_eq!(counts(), (7, 1));
        fail(OpSet::SYNCS);
        assert!(segment.sync_all().is_err());
        assert_eq!(counts(), (8, 2));
        // A call with no fault counts nothing.
        vfs.create_dir(Path::new("/db/table")).expect("made");
        assert_eq!(counts(), (8, 2));
    }

    #[test]
    fn what_is_expected_goes_with_the_image() {
        let observer = Observer::new(fs(), Expect::of_new_store());
        let vfs = observer.vfs();
        vfs.create_dir(Path::new("/db/a")).expect("made");
        observer.expecting(|expect| expect.created = true);
        assert!(observer.expect().created);
        vfs.create_dir(Path::new("/db/b")).expect("made");
        let taken = observer.drain();
        let created: Vec<bool> = taken.iter().map(|taken| taken.expect.created).collect();
        assert_eq!(created, [false, true]);
    }

    #[test]
    fn a_planted_defect_swallows_the_syncs_it_names() {
        let lost = |after| Sabotage::LostSync { part: "log", after };
        let observer = Observer::sabotaged(fs(), Expect::of_new_store(), lost(0));
        run(&*observer.vfs());
        assert_eq!(observer.lies(), 1);
        // The appends to the log are still unsynced, both of them.
        let image = observer.fs().crash_image();
        let unsynced = |path: &str| image.changes().iter().filter(|c| c.path == path).count();
        assert_eq!(unsynced("/db/log"), 2);
        assert_eq!(unsynced("/db/page"), 0);
        // A defect that begins after a handle's first sync leaves that one
        // honest: the log's one sync is made.
        let later = Observer::sabotaged(fs(), Expect::of_new_store(), lost(1));
        run(&*later.vfs());
        assert_eq!(later.lies(), 0);
        let log = later
            .vfs()
            .open(Path::new("/db/log"), OpenMode::Append(Create::Never))
            .expect("opened");
        log.sync_all().expect("synced");
        log.append(b"three").expect("appended");
        log.sync_data().expect("a lie");
        assert_eq!(later.lies(), 1);
        assert_eq!(later.fs().crash_image().changes().len(), 1);

        let observer =
            Observer::sabotaged(fs(), Expect::of_new_store(), Sabotage::LostDirSync("db"));
        run(&*observer.vfs());
        assert_eq!(observer.lies(), 1);
        let honest = Observer::new(fs(), Expect::of_new_store());
        run(&*honest.vfs());
        let names = |observer: &Observer| observer.fs().crash_image().names().len();
        assert!(names(&observer) > names(&honest));
        assert_eq!(honest.lies(), 0);
        let file = honest
            .vfs()
            .open(Path::new("/db/log"), OpenMode::Read)
            .expect("opened");
        assert_eq!(format!("{file:?}"), "ObservedFile(/db/log)");
    }

    /// A segment as the store writes it: the header synced, then blocks,
    /// with the write-back failing after the second of them and telling
    /// the handle that wrote alone.
    fn written(observer: &Observer) -> Box<dyn VfsFile> {
        use lignin_io::sim::{Fault, Told, WriteBackRule};

        let (fs, vfs) = (observer.fs().clone(), observer.vfs());
        vfs.create_dir(Path::new("/db/seg")).expect("made");
        let path = Path::new("/db/seg/1.seg");
        let file = vfs
            .open(path, OpenMode::Append(Create::New))
            .expect("opened");
        file.append(b"header").expect("appended");
        file.sync_data().expect("synced");
        let rule = WriteBackRule::new(Fault::Eio).at(path).skip(1);
        fs.add_write_back(rule.told(Told::Writers).forgotten(false));
        file.append(b"one").expect("appended");
        assert_eq!(observer.now().seen, None);
        file.append(b"two").expect("appended");
        file.append(b"three").expect("appended");
        assert_eq!(fs.stats().write_backs, 1);
        file
    }

    #[test]
    fn an_image_has_what_was_seen_once_a_write_back_has_failed() {
        use lignin_io::sim::UnknownFate;

        // A file system on which the bytes of a write-back that failed
        // have the fate `unknown`.
        let with = |unknown| {
            let fs = SimVfs::new(SimConfig {
                unknown,
                ..SimConfig::new(3)
            });
            fs.create_dir(Path::new("/db")).expect("a directory");
            fs.sync_dir(Path::new("/")).expect("synced");
            Observer::new(fs, Expect::of_new_store())
        };
        let observer = with(UnknownFate::Lost);
        let file = written(&observer);
        let taken = observer.drain();
        let failed: Vec<bool> = taken.iter().map(|taken| taken.seen.is_some()).collect();
        // The directory, the open, the header, its sync and the first
        // block; then the block the failure came after, and the third.
        assert_eq!(failed, [false, false, false, false, false, true, true]);
        // What was seen is the file system of that moment. The plan that
        // keeps every unsynced change leaves another where storage lost
        // the two blocks before the failure, and the same where it kept
        // them.
        let kept = |taken: &Taken| {
            let all = taken.image.keep_all();
            Some(taken.image.recover(&all).expect("fits").fingerprint())
        };
        let seen = Some(observer.fs().fingerprint());
        let last = taken.last().expect("an image");
        assert_eq!((last.seen, observer.now().seen), (seen, seen));
        assert_ne!(kept(last), seen);
        let whole = with(UnknownFate::Kept);
        let _file = written(&whole);
        let last = whole.now();
        assert!(last.seen.is_some());
        assert_eq!(kept(&last), last.seen);
        // The handle that wrote is told, and no defect acted.
        assert!(file.sync_data().is_err());
        assert_eq!(observer.lies(), 0);
        observer.recovered();
        assert_eq!(observer.now().seen, None);
        observer.vfs().create_dir(Path::new("/db/b")).expect("made");
        assert!(observer.drain().iter().all(|taken| taken.seen.is_none()));
    }

    #[test]
    fn a_sync_through_a_handle_that_did_not_write_is_found() {
        use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet, Told, WriteBackRule};

        let observer = Observer::new(fs(), Expect::of_new_store());
        let vfs = observer.vfs();
        let path = Path::new("/db/file");
        let found = || observer.take_foreign_sync();
        let append = OpenMode::Append(Create::New);
        let writer = vfs.open(path, append).expect("opened");
        let reader = vfs.open(path, OpenMode::Read).expect("opened");

        // Through the handle that wrote, and through any handle where
        // there is nothing to cover.
        writer.append(b"one").expect("appended");
        writer.sync_data().expect("synced");
        reader.sync_data().expect("synced");
        assert_eq!(found(), None);

        // Through another handle, over a change the writer made: found,
        // and said once.
        writer.append(b"two").expect("appended");
        reader.sync_all().expect("synced");
        assert_eq!(found(), Some(path.to_path_buf()));
        assert_eq!(found(), None);
        // That sync was answered, so nothing is left for the next.
        reader.sync_data().expect("synced");
        assert_eq!(found(), None);

        // A file that is removed and made again under its name is another
        // file: the new one owes nothing to the handle that wrote the
        // first, and the first is still the file its handles are on.
        writer.append(b"three").expect("appended");
        vfs.remove_file(path).expect("removed");
        let again = vfs.open(path, append).expect("opened");
        again.append(b"four").expect("appended");
        again.sync_data().expect("synced");
        assert_eq!(found(), None);
        reader.sync_data().expect("synced");
        assert_eq!(found(), Some(path.to_path_buf()));

        // A file is the same file under the name a rename gives it,
        // whichever name a handle was opened by.
        let temporary = Path::new("/db/file.tmp");
        let create = OpenMode::Write(Create::New);
        let replacement = vfs.open(temporary, create).expect("opened");
        replacement.write_at(b"new", 0).expect("written");
        replacement.set_len(2).expect("cut");
        vfs.rename(temporary, path).expect("renamed");
        let other = vfs.open(path, OpenMode::Read).expect("opened");
        other.sync_data().expect("synced");
        assert_eq!(found(), Some(path.to_path_buf()));
        replacement.write_at(b"er", 2).expect("written");
        replacement.sync_all().expect("synced");
        assert_eq!(found(), None);
        // The file that had the name before the rename is another file.
        again.append(b"five").expect("appended");
        other.sync_data().expect("synced");
        assert_eq!(found(), None);
        // A rename that changes nothing leaves the file what it was.
        replacement.write_at(b"!", 4).expect("written");
        vfs.rename(path, path).expect("renamed");
        other.sync_data().expect("synced");
        assert_eq!(found(), Some(path.to_path_buf()));

        // A call that changes nothing leaves nothing to cover.
        replacement.set_len(5).expect("the length it has");
        replacement.write_at(&[], 0).expect("nothing is written");
        other.sync_data().expect("synced");
        assert_eq!(found(), None);

        // Nor does a sync that fails: no later sync covers the changes
        // that were made before it.
        replacement.write_at(b"?", 5).expect("written");
        (observer.fs()).add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
        assert!(replacement.sync_all().is_err());
        other.sync_data().expect("synced");
        assert_eq!(found(), None);

        // A change of length that is one is a change of its handle. One
        // that is none is not, also where a write-back fails after it and
        // settles what another handle wrote: that handle made every
        // change, and its sync is told.
        let settled = Path::new("/db/settled");
        let first = vfs.open(settled, create).expect("opened");
        let second = (vfs.open(settled, OpenMode::Write(Create::Never))).expect("opened");
        first.write_at(b"a byte", 0).expect("written");
        second.set_len(7).expect("grown");
        first.sync_all().expect("synced");
        assert_eq!(found(), Some(settled.to_path_buf()));
        first.write_at(b"A", 0).expect("written");
        let rule = WriteBackRule::new(Fault::Eio).on(OpKind::SetLen);
        (observer.fs()).add_write_back(rule.told(Told::Writers).forgotten(false));
        second.set_len(7).expect("the length it has");
        assert_eq!(observer.fs().stats().write_backs, 1);
        assert!(first.sync_all().is_err());
        assert_eq!(found(), None);

        // Under a link, and in a directory that was renamed, a file is
        // the same file as well.
        let dir = Path::new("/db/built");
        vfs.create_dir(dir).expect("made");
        let copy = vfs.open(&dir.join("copy"), create).expect("opened");
        copy.write_at(b"a copy", 0).expect("written");
        vfs.hard_link(&dir.join("copy"), &dir.join("link"))
            .expect("linked");
        vfs.rename(dir, Path::new("/db/placed")).expect("renamed");
        let placed = Path::new("/db/placed/link");
        let linked = vfs.open(placed, OpenMode::Read).expect("opened");
        linked.sync_data().expect("synced");
        assert_eq!(found(), Some(placed.to_path_buf()));
        // A rename of one of its names onto the other changes nothing.
        copy.write_at(b"more", 6).expect("written");
        vfs.rename(Path::new("/db/placed/copy"), placed)
            .expect("renamed");
        linked.sync_data().expect("synced");
        assert_eq!(found(), Some(placed.to_path_buf()));
        // One name of the two is removed: the file is still the file.
        copy.write_at(b"most", 10).expect("written");
        vfs.remove_file(Path::new("/db/placed/copy"))
            .expect("removed");
        linked.sync_data().expect("synced");
        assert_eq!(found(), Some(placed.to_path_buf()));
    }

    #[test]
    fn a_planted_defect_syncs_through_a_handle_that_is_told_nothing() {
        use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet, Told, WriteBackRule};

        let other = Sabotage::OtherHandle("seg/");
        let planted = || Observer::sabotaged(fs(), Expect::of_new_store(), other);
        let append = OpenMode::Append(Create::New);
        let fails = |rule: WriteBackRule| rule.told(Told::Writers).forgotten(false);
        let observer = planted();
        let file = written(&observer);
        // The sync of the header went through the handle that wrote it.
        // The next one goes through the handle beside it, which is told
        // nothing.
        assert_eq!(observer.lies(), 0);
        file.sync_data()
            .expect("the handle beside was told nothing");
        assert_eq!(observer.lies(), 1);
        // The defect acts once for a write-back that failed.
        file.append(b"four").expect("appended");
        file.sync_data().expect("synced");
        assert_eq!(observer.lies(), 1);

        // The handle beside is on the file, whatever becomes of its name:
        // the sync covers what was appended to the file, and nothing of
        // the file that has its name by then.
        let observer = planted();
        let file = written(&observer);
        let vfs = observer.vfs();
        let (path, moved) = (Path::new("/db/seg/1.seg"), Path::new("/db/seg/9.seg"));
        vfs.rename(path, moved).expect("renamed");
        let again = vfs.open(path, append).expect("opened");
        again.append(b"of another file").expect("appended");
        for dir in ["/db", "/db/seg"] {
            vfs.sync_dir(Path::new(dir)).expect("synced");
        }
        let durable = |path: &Path| {
            let image = observer.now().image;
            let lost = image.recover(&image.lose_all()).expect("fits");
            let file = lost.open(path, OpenMode::Read).expect("opened");
            file.len().expect("a length")
        };
        assert_eq!(durable(path), 0);
        assert!(durable(moved) < 17);
        file.sync_data()
            .expect("the handle beside was told nothing");
        assert_eq!(observer.lies(), 1);
        assert_eq!((durable(path), durable(moved)), (0, 17));

        // A write-back that fails after the open of another handle on the
        // file is one of the file's.
        let observer = planted();
        let vfs = observer.vfs();
        vfs.create_dir(Path::new("/db/seg")).expect("made");
        let path = Path::new("/db/seg/2.seg");
        let file = vfs.open(path, append).expect("opened");
        file.append(b"header").expect("appended");
        file.sync_data().expect("synced");
        file.append(b"block").expect("appended");
        let rule = WriteBackRule::new(Fault::Eio).on(OpKind::Open).at(path);
        observer.fs().add_write_back(fails(rule));
        let _reader = vfs.open(path, OpenMode::Read).expect("opened");
        assert_eq!(observer.fs().stats().write_backs, 1);
        file.sync_data()
            .expect("the handle beside was told nothing");
        assert_eq!(observer.lies(), 1);

        // The defect has not acted where the sync through the handle
        // beside fails for a reason of its own.
        let observer = planted();
        let file = written(&observer);
        (observer.fs()).add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
        assert!(file.sync_data().is_err());
        assert_eq!(observer.lies(), 0);
        // Nor where no write-back failed.
        let observer = planted();
        let vfs = observer.vfs();
        vfs.create_dir(Path::new("/db/seg")).expect("made");
        let file = (vfs.open(Path::new("/db/seg/2.seg"), append)).expect("opened");
        for block in [b"header", b"blocks"] {
            file.append(block).expect("appended");
            file.sync_data().expect("synced");
        }
        assert_eq!(observer.lies(), 0);
        // Nor at a header: a write-back that fails under it is told to
        // the sync of the handle that wrote it.
        let path = Path::new("/db/seg/3.seg");
        let file = vfs.open(path, append).expect("opened");
        observer
            .fs()
            .add_write_back(fails(WriteBackRule::new(Fault::Eio).at(path)));
        file.append(b"header").expect("appended");
        assert_eq!(observer.fs().stats().write_backs, 1);
        assert!(file.sync_data().is_err());
        assert_eq!(observer.lies(), 0);
        // The files that the defect does not name are synced through the
        // handle that wrote.
        let observer =
            Observer::sabotaged(fs(), Expect::of_new_store(), Sabotage::OtherHandle("val/"));
        let file = written(&observer);
        assert!(file.sync_data().is_err());
        assert_eq!(observer.lies(), 0);
    }
}
