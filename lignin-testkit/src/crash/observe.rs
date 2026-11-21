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
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

use lignin_io::{
    sim::{CrashImage, SimVfs},
    DirEntry, OpenMode, Vfs, VfsFile, VfsLock,
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
        }
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

    /// Looks at the file system after a call on `path`, a sync of that
    /// file if `synced`: a fault that was injected into the call is
    /// counted, and an image is taken if the call changed what a crash can
    /// leave.
    fn after(&self, path: &Path, synced: bool) {
        let mut watch = self.watch();
        let stats = self.fs.stats();
        if stats.faults != watch.faults {
            watch.faults = stats.faults;
            watch.lasting += u64::from(synced && lasts(path));
        }
        if stats.changes != watch.changes {
            watch.changes = stats.changes;
            let taken = Taken {
                image: self.fs.crash_image(),
                expect: watch.expect.clone(),
                boundary: stats.changes,
            };
            watch.taken.push(taken);
        }
    }

    fn observed<T>(&self, path: &Path, result: io::Result<T>) -> io::Result<T> {
        self.after(path, false);
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
        let file = self.observed(path, self.fs.open(path, mode))?;
        Ok(Box::new(ObservedFile {
            file,
            path: path.to_path_buf(),
            syncs: AtomicU64::new(0),
            observer: self.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.observed(to, self.fs.rename(from, to))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.observed(link, self.fs.hard_link(original, link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let before = self.fs.stats().faults;
        let removed = self.fs.remove_file(path);
        if self.fs.stats().faults > before {
            self.watch().removals += 1;
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
    path: PathBuf,
    /// How many syncs were asked of this handle.
    syncs: AtomicU64,
    observer: Observer,
}

impl ObservedFile {
    fn sync(&self, sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>) -> io::Result<()> {
        let made = self.syncs.fetch_add(1, Ordering::SeqCst);
        if self.observer.loses_sync(&self.path, made) {
            return Ok(());
        }
        let synced = sync(&*self.file);
        self.observer.after(&self.path, true);
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
        (self.observer).observed(&self.path, self.file.read_at(buf, offset))
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        (self.observer).observed(&self.path, self.file.write_at(buf, offset))
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.observer.observed(&self.path, self.file.append(buf))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync(|file| file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync(|file| file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.observer.observed(&self.path, self.file.len())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.observer.observed(&self.path, self.file.set_len(len))
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
}
