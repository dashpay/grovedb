//! Every write boundary of a session of the engine, from one run of it.
//!
//! [`Watcher`] is the file layer a database of the campaign is opened on:
//! a [`Vfs`] over a [`SimVfs`]. It forwards every call. A call that can
//! change what a crash leaves is made under the watcher's lock, and if it
//! did change it, the file system is frozen as a [`CrashImage`] before the
//! lock is given back: with what was expected of the database at that
//! moment, and with the thread that made the call. The database has three
//! threads of its own beside the one that calls it, so the images of a
//! session are in the order in which the file system changed, one for each
//! change, whichever thread made it.
//!
//! **The background threads can be held.** While the watcher is
//! [paused](Watcher::pause), a call that can change the file system waits
//! at the lock. The campaign pauses the watcher while it recovers the crash
//! states of the images it has, so the background work of a database goes
//! on beside the operations of its workload and not in the pauses between
//! them, and the file system is still while it is looked at.
//!
//! **An image is also taken where no file changed.** When the campaign
//! has learned what a call of its own answered, it says what is expected
//! from then on and has the file system frozen as it is when it has the
//! watcher's lock ([`Watcher::acknowledge`]): a crash from then on and
//! before the next change is held to what the answer promised. A change
//! that another thread made after the call returned and before that is in
//! the image.
//!
//! **A process can be killed.** The file layer that a database is opened
//! on ([`Watcher::vfs`]) is that of one process. After
//! [`Watcher::kill`], every call that is begun through it, through a file
//! it opened or through a lock it took fails as with the power off, and
//! changes nothing. A lock is given back when its guard is dropped. The file
//! system stays as the process left it: what it wrote is there for the
//! next process to read, and what it had not synced is as little durable
//! as before, so a crash of the machine after the kill can still lose it.

use std::{
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex, MutexGuard, Once, PoisonError,
    },
};

use lignin_io::{
    sim::{CrashImage, SimError, SimVfs},
    DirEntry, OpenMode, SyncItem, Vfs, VfsFile, VfsLock,
};

use super::model::Expect;
use crate::crash::observe::Sabotage;

/// The thread that made a change of the file system: one of the three
/// that an open database has (`lignindb::db`, "Background work"), or the
/// thread that called the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Actor {
    /// The thread of the workload: commits, flushes, checkpoints, opens
    /// and closes.
    Caller,
    /// Write-back.
    WriteBack,
    /// The reclaimer.
    Reclaimer,
    /// The cleaner.
    Cleaner,
}

impl Actor {
    /// The actor that the thread which calls this is, by the name the
    /// engine gives its threads.
    pub fn current() -> Self {
        match std::thread::current().name() {
            Some("lignindb-write-back") => Self::WriteBack,
            Some("lignindb-reclaim") => Self::Reclaimer,
            Some("lignindb-cleaner") => Self::Cleaner,
            _ => Self::Caller,
        }
    }

    /// Every actor.
    pub const ALL: [Self; 4] = [
        Self::Caller,
        Self::WriteBack,
        Self::Reclaimer,
        Self::Cleaner,
    ];

    /// The name the counts know it by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Caller => "the caller's thread",
            Self::WriteBack => "write-back's thread",
            Self::Reclaimer => "the reclaimer's thread",
            Self::Cleaner => "the cleaner's thread",
        }
    }
}

/// How many panics the background threads of the engine have made in this
/// process since the first call of this, which begins to count them: the
/// threads are known by their names ([`Actor::current`]), in whichever
/// database they are.
///
/// A database that GroveDB owns is closed by the drop of its handle. The
/// drop joins the three threads and answers nothing, so a thread that
/// panicked is not known by it: it is known by the panic hook of the
/// process, which this puts a counter in front of. The hook that was
/// there is called as before.
pub fn engine_panics() -> u64 {
    static PANICS: AtomicU64 = AtomicU64::new(0);
    static HOOKED: Once = Once::new();
    HOOKED.call_once(|| {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if Actor::current() != Actor::Caller {
                PANICS.fetch_add(1, Ordering::SeqCst);
            }
            hook(info);
        }));
    });
    PANICS.load(Ordering::SeqCst)
}

/// A crash image, with the moment it was taken at.
#[derive(Clone, Debug)]
pub struct Taken {
    /// What a crash at that moment has to work with.
    pub image: CrashImage,
    /// What was expected of the database then.
    pub expect: Arc<Expect>,
    /// The number of the write boundary: how many operations had changed
    /// what a crash of this file system can leave.
    pub boundary: u64,
    /// The number of the image among those the watcher took of its file
    /// system, from 1, in the order it took them: an image of a change,
    /// one of no change, and one of now each have a number of their own.
    pub number: u64,
    /// The thread whose call made the change. Of an image of no change,
    /// the thread that made the last one before it.
    pub actor: Actor,
    /// Whether the image is of no change: it was taken when the campaign
    /// learned what a call of its own had answered
    /// ([`Watcher::acknowledge`]), with what is expected from then on, or
    /// when it looked at the file system as it was ([`Watcher::now`]).
    pub acknowledged: bool,
}

struct Watch {
    changes: u64,
    /// How many of the changes calls of the caller's thread made.
    by_caller: u64,
    /// The thread that made the last change.
    last: Actor,
    /// The process that is alive: how many were killed before it.
    process: u64,
    expect: Arc<Expect>,
    taken: Vec<Taken>,
    paused: bool,
    sabotage: Option<Sabotage>,
    /// How many images were taken.
    images: u64,
    lies: u64,
    /// How many images had been taken when the planted defect first lied.
    first_lie: Option<u64>,
    /// How many faults the file system injected into calls of each actor,
    /// in the order of [`Actor::ALL`].
    faults: [u64; 4],
}

impl Watch {
    /// Counts a lie of the planted defect, if `lost` says it was one.
    fn lied(&mut self, lost: bool) {
        if lost {
            self.lies += 1;
            self.first_lie.get_or_insert(self.images);
        }
    }

    /// An image of `fs` as it is, with what is expected now.
    fn image(&mut self, fs: &SimVfs, actor: Actor, acknowledged: bool) -> Taken {
        self.images += 1;
        Taken {
            image: fs.crash_image(),
            expect: self.expect.clone(),
            boundary: self.changes,
            number: self.images,
            actor,
            acknowledged,
        }
    }
}

struct Shared {
    watch: Mutex<Watch>,
    resumed: Condvar,
    /// [`Watch::process`] again, for the calls that change nothing and so
    /// do not take the lock.
    process: AtomicU64,
}

/// The file layer of an observed session. A clone is another handle on the
/// same watcher.
#[derive(Clone)]
pub struct Watcher {
    fs: SimVfs,
    shared: Arc<Shared>,
}

impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher").field("fs", &self.fs).finish()
    }
}

impl Watcher {
    /// A watcher of `fs`, which expects `expect` of the database until it
    /// is told otherwise. What `fs` holds now is no boundary: the first
    /// image is taken after the next change.
    pub fn new(fs: SimVfs, expect: Expect, sabotage: Option<Sabotage>) -> Self {
        let watch = Watch {
            changes: fs.stats().changes,
            by_caller: 0,
            last: Actor::Caller,
            process: 0,
            expect: Arc::new(expect),
            taken: Vec::new(),
            paused: false,
            sabotage,
            images: 0,
            lies: 0,
            first_lie: None,
            faults: [0; 4],
        };
        Self {
            fs,
            shared: Arc::new(Shared {
                watch: Mutex::new(watch),
                resumed: Condvar::new(),
                process: AtomicU64::new(0),
            }),
        }
    }

    fn watch(&self) -> MutexGuard<'_, Watch> {
        let watch = &self.shared.watch;
        watch.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The lock under which `process` changes the file system: taken once
    /// the watcher is not paused. A process that was killed gets the
    /// error of a machine whose power is off, and does not wait.
    fn changing(&self, process: u64) -> io::Result<MutexGuard<'_, Watch>> {
        let mut watch = self.watch();
        loop {
            if watch.process != process {
                return Err(killed());
            }
            if !watch.paused {
                return Ok(watch);
            }
            let resumed = &self.shared.resumed;
            watch = resumed.wait(watch).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// `Err` if `process` was killed: for a call that changes nothing,
    /// which does not wait for the lock. One that began before the kill
    /// returned can be answered after it.
    fn alive(&self, process: u64) -> io::Result<()> {
        match self.shared.process.load(Ordering::SeqCst) == process {
            true => Ok(()),
            false => Err(killed()),
        }
    }

    /// Kills the process that the file layer of the last [`Self::vfs`] is
    /// of: a call of it that changes the file system and is being made is
    /// waited for, and every call that it begins from here on fails and
    /// changes nothing, also one that was held by a
    /// [pause](Self::pause). The file layer of the next [`Self::vfs`] is
    /// that of a new process, which finds every change of the old one,
    /// synced or not.
    pub fn kill(&self) {
        let mut watch = self.watch();
        watch.process += 1;
        self.shared.process.store(watch.process, Ordering::SeqCst);
        drop(watch);
        self.shared.resumed.notify_all();
    }

    /// The file system under the watcher.
    pub fn fs(&self) -> &SimVfs {
        &self.fs
    }

    /// The file layer that a database is opened on: that of the process
    /// which is alive now.
    pub fn vfs(&self) -> Arc<dyn Vfs> {
        Arc::new(Process {
            watcher: self.clone(),
            number: self.watch().process,
        })
    }

    /// What is expected of the database now.
    pub fn expect(&self) -> Arc<Expect> {
        self.watch().expect.clone()
    }

    /// Changes what is expected of the database from now on: every image
    /// that is taken after this call returns has the change.
    pub fn expecting(&self, change: impl FnOnce(&mut Expect)) {
        change(Arc::make_mut(&mut self.watch().expect));
    }

    /// Changes what is expected of the database from now on, as
    /// [`Self::expecting`] does, and takes an image of the file system at
    /// that moment with the new expectation: what a crash leaves that
    /// comes when a call of the campaign has answered, whether or not a
    /// file changes afterwards.
    ///
    /// The image is taken when the campaign has the answer, which is after
    /// the call returned: a change that another thread makes between the
    /// two is in it.
    pub fn acknowledge(&self, change: impl FnOnce(&mut Expect)) {
        let mut watch = self.watch();
        change(Arc::make_mut(&mut watch.expect));
        let last = watch.last;
        let taken = watch.image(&self.fs, last, true);
        watch.taken.push(taken);
    }

    /// Holds every call that can change the file system, from the next one
    /// on, until [`Self::resume`]. A call that is being made is waited for:
    /// when this returns, the file system is still.
    pub fn pause(&self) {
        self.watch().paused = true;
    }

    /// Lets the calls go on that [`Self::pause`] held.
    pub fn resume(&self) {
        self.watch().paused = false;
        self.shared.resumed.notify_all();
    }

    /// The images taken since the last call, in the order of their
    /// boundaries.
    pub fn drain(&self) -> Vec<Taken> {
        std::mem::take(&mut self.watch().taken)
    }

    /// An image of the file system as it is now, whether or not anything
    /// changed since the last one.
    pub fn now(&self) -> Taken {
        let mut watch = self.watch();
        let last = watch.last;
        watch.image(&self.fs, last, true)
    }

    /// How many write boundaries the file system has passed.
    pub fn boundaries(&self) -> u64 {
        self.watch().changes
    }

    /// How many of the write boundaries calls of the caller's thread made.
    /// A call of that thread that leaves the number as it was changed no
    /// file and no name.
    pub fn boundaries_of_the_caller(&self) -> u64 {
        self.watch().by_caller
    }

    /// How many calls the planted defect answered with a lie.
    pub fn lies(&self) -> u64 {
        self.watch().lies
    }

    /// How many images had been taken when the planted defect first lied:
    /// the images with a higher [number](Taken::number) are of a file
    /// system that was told of a sync that was not made.
    pub fn first_lie(&self) -> Option<u64> {
        self.watch().first_lie
    }

    /// How many faults the file system injected into calls of each actor
    /// that can change it, in the order of [`Actor::ALL`]: a fault fails
    /// the next call of its kind, whichever thread makes it.
    pub fn faults_met(&self) -> [u64; 4] {
        self.watch().faults
    }

    /// Makes a call of `process` that can change what a crash leaves, and
    /// takes an image if it did.
    fn changed<T>(&self, process: u64, call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let mut watch = self.changing(process)?;
        let before = self.fs.stats().faults;
        let result = call();
        let stats = self.fs.stats();
        if stats.faults > before {
            let actor = Actor::current();
            let at = Actor::ALL.iter().position(|other| *other == actor);
            watch.faults[at.expect("every actor is listed")] += stats.faults - before;
        }
        let changes = stats.changes;
        if changes != watch.changes {
            watch.changes = changes;
            let actor = Actor::current();
            watch.by_caller += u64::from(actor == Actor::Caller);
            watch.last = actor;
            let taken = watch.image(&self.fs, actor, false);
            watch.taken.push(taken);
        }
        result
    }

    /// Whether the planted defect swallows a sync that `process` asks of
    /// the file at `path`, through a handle that has made `made` syncs.
    fn loses_sync(&self, process: u64, path: &Path, made: u64) -> io::Result<bool> {
        let mut watch = self.changing(process)?;
        let lost = match watch.sabotage {
            Some(Sabotage::LostSync { part, after }) => {
                made >= after && path.to_string_lossy().contains(part)
            }
            _ => false,
        };
        watch.lied(lost);
        Ok(lost)
    }

    fn loses_dir_sync(&self, process: u64, path: &Path) -> io::Result<bool> {
        let mut watch = self.changing(process)?;
        let lost = match watch.sabotage {
            Some(Sabotage::LostDirSync(end)) => path.to_string_lossy().ends_with(end),
            _ => false,
        };
        watch.lied(lost);
        Ok(lost)
    }
}

/// What a call of a process gets that was killed: the error of a machine
/// whose power is off, which is what the process has seen the last of.
fn killed() -> io::Error {
    io::Error::other(SimError::PowerLoss)
}

/// The file layer of one process on the file system of a watcher.
struct Process {
    watcher: Watcher,
    number: u64,
}

impl fmt::Debug for Process {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Process({})", self.number)
    }
}

impl Process {
    fn changed<T>(&self, call: impl FnOnce(&SimVfs) -> io::Result<T>) -> io::Result<T> {
        let fs = &self.watcher.fs;
        self.watcher.changed(self.number, || call(fs))
    }

    fn held(&self, lock: Box<dyn VfsLock>) -> HeldLock {
        HeldLock {
            lock,
            watcher: self.watcher.clone(),
            process: self.number,
        }
    }
}

impl Vfs for Process {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let file = self.changed(|fs| fs.open(path, mode))?;
        Ok(Box::new(WatchedFile {
            file,
            path: path.to_path_buf(),
            syncs: AtomicU64::new(0),
            watcher: self.watcher.clone(),
            process: self.number,
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.changed(|fs| fs.rename(from, to))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.changed(|fs| fs.hard_link(original, link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.changed(|fs| fs.remove_file(path))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.changed(|fs| fs.create_dir(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.changed(|fs| fs.remove_dir(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.watcher.alive(self.number)?;
        self.watcher.fs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if self.watcher.loses_dir_sync(self.number, path)? {
            return Ok(());
        }
        self.changed(|fs| fs.sync_dir(path))
    }

    // The syncs of a list one after another, as the trait has it, and a
    // list with nothing in it too is refused once the process was killed.
    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        self.watcher.alive(self.number)?;
        for item in items {
            match item {
                SyncItem::Data(file) => file.sync_data()?,
                SyncItem::All(file) => file.sync_all()?,
                SyncItem::Dir(path) => self.sync_dir(path)?,
            }
        }
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        let lock = self.changed(|fs| fs.lock(path))?;
        Ok(Box::new(self.held(lock)))
    }

    // A fault that is planted on the names fails a lock too: the call is
    // made like those that change the file system, so that the fault is
    // counted for the thread it met. It changes nothing a crash can leave,
    // and no image is taken of it.
    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        let lock = self.changed(|fs| fs.lock_existing(path))?;
        Ok(Box::new(self.held(lock)))
    }
}

/// A lock that a process holds: it answers while the process is alive.
/// Dropping it gives the lock back, whether the process is alive or not,
/// as the end of a process does.
struct HeldLock {
    lock: Box<dyn VfsLock>,
    watcher: Watcher,
    process: u64,
}

impl fmt::Debug for HeldLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HeldLock({:?})", self.lock)
    }
}

impl VfsLock for HeldLock {
    fn is_at(&self, path: &Path) -> io::Result<bool> {
        self.watcher.alive(self.process)?;
        self.lock.is_at(path)
    }
}

struct WatchedFile {
    file: Box<dyn VfsFile>,
    path: PathBuf,
    /// How many syncs were asked of this handle.
    syncs: AtomicU64,
    watcher: Watcher,
    /// The process that opened it.
    process: u64,
}

impl WatchedFile {
    fn changed<T>(&self, call: impl FnOnce(&dyn VfsFile) -> io::Result<T>) -> io::Result<T> {
        self.watcher.changed(self.process, || call(&*self.file))
    }

    fn sync(&self, sync: impl FnOnce(&dyn VfsFile) -> io::Result<()>) -> io::Result<()> {
        let made = self.syncs.fetch_add(1, Ordering::SeqCst);
        if self.watcher.loses_sync(self.process, &self.path, made)? {
            return Ok(());
        }
        self.changed(sync)
    }
}

impl fmt::Debug for WatchedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WatchedFile({})", self.path.display())
    }
}

impl VfsFile for WatchedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.watcher.alive(self.process)?;
        self.file.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.changed(|file| file.write_at(buf, offset))
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.changed(|file| file.append(buf))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync(|file| file.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync(|file| file.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.watcher.alive(self.process)?;
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.changed(|file| file.set_len(len))
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

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

    fn watcher() -> Watcher {
        Watcher::new(fs(), Expect::exactly(0), None)
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
        let lock = vfs.lock(Path::new("/db/LOCK")).expect("locked");
        assert!(vfs.lock_existing(Path::new("/db/LOCK")).is_err());
        drop(lock);
    }

    #[test]
    fn an_image_is_taken_at_every_boundary_and_is_what_a_crash_there_leaves() {
        let watcher = watcher();
        assert!(format!("{watcher:?}").starts_with("Watcher"));
        run(&*watcher.vfs());
        let taken = watcher.drain();
        let changes = watcher.fs().stats().changes;
        assert_eq!(watcher.boundaries(), changes);
        assert!(watcher.drain().is_empty());
        // One image for each operation that changed what a crash can
        // leave, and none for the reads.
        let boundaries: Vec<u64> = taken.iter().map(|taken| taken.boundary).collect();
        let first = boundaries[0];
        assert_eq!(boundaries, (first..=changes).collect::<Vec<_>>());
        assert!(taken.len() >= 12, "{} images", taken.len());
        assert!(taken.iter().all(|taken| taken.actor == Actor::Caller));

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

        // The images are numbered in the order they were taken.
        let numbers: Vec<u64> = taken.iter().map(|taken| taken.number).collect();
        assert_eq!(numbers, (1..=taken.len() as u64).collect::<Vec<_>>());
        // An image of now is of the last boundary, and of no change: it
        // has the next number, and so has an image of an answer.
        let now = watcher.now();
        assert_eq!((now.boundary, now.actor), (changes, Actor::Caller));
        assert!(now.acknowledged);
        assert_eq!(now.number, taken.len() as u64 + 1);
        watcher.acknowledge(|expect| expect.latest = 1);
        let answered = watcher.drain();
        assert_eq!(answered.len(), 1);
        let answered = &answered[0];
        assert_eq!(
            (answered.boundary, answered.number),
            (changes, now.number + 1)
        );
        assert!(answered.acknowledged && answered.expect.latest == 1);
        let file = watcher
            .vfs()
            .open(Path::new("/db/log"), OpenMode::Read)
            .expect("opened");
        assert_eq!(format!("{file:?}"), "WatchedFile(/db/log)");
    }

    #[test]
    fn what_is_expected_goes_with_the_image() {
        let watcher = watcher();
        let vfs = watcher.vfs();
        vfs.create_dir(Path::new("/db/a")).expect("made");
        watcher.expecting(|expect| expect.latest = 4);
        assert_eq!(watcher.expect().latest, 4);
        vfs.create_dir(Path::new("/db/b")).expect("made");
        let taken = watcher.drain();
        let latest: Vec<usize> = taken.iter().map(|taken| taken.expect.latest).collect();
        assert_eq!(latest, [0, 4]);
    }

    /// The thread that makes a change is named with its image: the threads
    /// of a database are known by the names the engine gives them.
    #[test]
    fn an_image_names_the_thread_that_made_the_change() {
        let watcher = watcher();
        let names = [
            ("lignindb-write-back", Actor::WriteBack),
            ("lignindb-reclaim", Actor::Reclaimer),
            ("lignindb-cleaner", Actor::Cleaner),
            ("somebody", Actor::Caller),
        ];
        for (index, (name, _)) in names.iter().enumerate() {
            let vfs = watcher.vfs();
            let made = std::thread::Builder::new()
                .name(name.to_string())
                .spawn(move || vfs.create_dir(Path::new(&format!("/db/{index}"))))
                .expect("a thread");
            made.join().expect("joined").expect("made");
        }
        let actors: Vec<Actor> = watcher.drain().iter().map(|taken| taken.actor).collect();
        assert_eq!(actors, names.map(|(_, actor)| actor));
        assert_eq!(Actor::ALL.map(Actor::name).len(), 4);
        assert_eq!(Actor::current(), Actor::Caller);
    }

    /// A fault that fails a call is counted for the thread that made the
    /// call.
    #[test]
    fn a_fault_is_counted_for_the_thread_it_met() {
        use lignin_io::sim::{Fault, FaultRule, OpSet};

        let watcher = watcher();
        let fail = || {
            let rule = FaultRule::new(Fault::Eio).on(OpSet::NAMES);
            watcher.fs().add_rule(rule);
        };
        fail();
        assert!(watcher.vfs().create_dir(Path::new("/db/a")).is_err());
        fail();
        let vfs = watcher.vfs();
        let cleaner = std::thread::Builder::new()
            .name("lignindb-cleaner".to_string())
            .spawn(move || vfs.create_dir(Path::new("/db/b")))
            .expect("a thread");
        assert!(cleaner.join().expect("joined").is_err());
        // A call that no fault fails counts nothing.
        watcher.vfs().create_dir(Path::new("/db/c")).expect("made");
        assert_eq!(watcher.faults_met(), [1, 0, 0, 1]);
    }

    /// While the watcher is paused, a call that changes the file system
    /// waits, and a read does not.
    #[test]
    fn a_paused_watcher_holds_every_change_and_no_read() {
        let watcher = watcher();
        let vfs = watcher.vfs();
        vfs.create_dir(Path::new("/db/before")).expect("made");
        watcher.pause();
        let before = watcher.fs().stats().changes;
        let (begun, has_begun) = mpsc::channel();
        let (done, is_done) = mpsc::channel();
        let other = watcher.vfs();
        let changer = std::thread::spawn(move || {
            begun.send(()).expect("told");
            other.create_dir(Path::new("/db/held")).expect("made");
            done.send(()).expect("told");
        });
        has_begun.recv().expect("the thread runs");
        // A read is answered while the change is held.
        assert_eq!(vfs.read_dir(Path::new("/db")).expect("listed").len(), 1);
        // The change is not made for as long as the watcher is paused:
        // the thread has nothing else to wait for.
        assert!(is_done.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(watcher.fs().stats().changes, before);
        watcher.resume();
        is_done.recv().expect("the change is made");
        changer.join().expect("joined");
        assert_eq!(watcher.fs().stats().changes, before + 1);
        assert_eq!(watcher.drain().len(), 2);
    }

    /// A process that is killed changes nothing any more, through its
    /// file layer or through a file it opened, and reads nothing: not a
    /// call that was held when it was killed either. What it wrote and did
    /// not sync is there for the next process, and is no more durable
    /// than it was.
    #[test]
    fn a_killed_process_changes_nothing_and_leaves_what_it_wrote_unsynced() {
        let watcher = watcher();
        let old = watcher.vfs();
        let path = Path::new("/db/log");
        let log = old
            .open(path, OpenMode::Append(Create::New))
            .expect("opened");
        old.sync_dir(Path::new("/db")).expect("synced");
        log.append(b"one").expect("appended");
        log.sync_data().expect("synced");
        log.append(b"two").expect("appended");
        let lock = old.lock(Path::new("/db/LOCK")).expect("locked");
        assert!(lock.is_at(Path::new("/db/LOCK")).expect("asked"));
        // A change of the process is held, and the process is killed.
        watcher.pause();
        let held = watcher.vfs();
        let changer = std::thread::spawn(move || held.create_dir(Path::new("/db/held")));
        watcher.kill();
        let refused = changer
            .join()
            .expect("joined")
            .expect_err("the process was killed");
        assert!(SimError::is_power_loss(&refused));
        watcher.resume();
        let changes = watcher.fs().stats().changes;
        watcher.drain();

        let power_is_off = |error: io::Error| assert!(SimError::is_power_loss(&error));
        power_is_off(log.append(b"more").expect_err("killed"));
        power_is_off(log.write_at(b"more", 0).expect_err("killed"));
        power_is_off(log.set_len(0).expect_err("killed"));
        power_is_off(log.sync_data().expect_err("killed"));
        power_is_off(log.sync_all().expect_err("killed"));
        power_is_off(log.len().expect_err("killed"));
        power_is_off(log.read_at(&mut [0; 3], 0).expect_err("killed"));
        power_is_off(old.open(path, OpenMode::Read).expect_err("killed"));
        power_is_off(
            old.rename(path, Path::new("/db/other"))
                .expect_err("killed"),
        );
        power_is_off(
            old.hard_link(path, Path::new("/db/other"))
                .expect_err("killed"),
        );
        power_is_off(old.remove_file(path).expect_err("killed"));
        power_is_off(old.create_dir(Path::new("/db/sub")).expect_err("killed"));
        power_is_off(old.remove_dir(Path::new("/db/sub")).expect_err("killed"));
        power_is_off(old.sync_dir(Path::new("/db")).expect_err("killed"));
        power_is_off(old.sync_together(&[]).expect_err("killed"));
        let together = [SyncItem::Data(&*log), SyncItem::Dir(Path::new("/db"))];
        power_is_off(old.sync_together(&together).expect_err("killed"));
        power_is_off(old.read_dir(Path::new("/db")).expect_err("killed"));
        power_is_off(old.lock(Path::new("/db/LOCK")).expect_err("killed"));
        power_is_off(old.lock_existing(path).expect_err("killed"));
        power_is_off(lock.is_at(Path::new("/db/LOCK")).expect_err("killed"));
        assert!(format!("{lock:?}").starts_with("HeldLock("));
        assert_eq!(watcher.fs().stats().changes, changes);
        assert!(watcher.drain().is_empty());
        assert!(!watcher.fs().is_powered_off());
        assert_eq!(format!("{old:?}"), "Process(0)");

        // The next process reads all six bytes, and a crash of the machine
        // can still lose the three that were not synced. The lock is the
        // dead process's until its guard is dropped.
        let new = watcher.vfs();
        assert!(new.lock(Path::new("/db/LOCK")).is_err());
        drop(lock);
        let again = new.lock(Path::new("/db/LOCK")).expect("locked");
        assert!(again.is_at(Path::new("/db/LOCK")).expect("asked"));
        let log = new.open(path, OpenMode::Read).expect("opened");
        assert_eq!(log.len().expect("a length"), 6);
        let image = watcher.fs().crash_image();
        let len_after = |plan| {
            let left = image.recover(&plan).expect("fits");
            let log = left.open(path, OpenMode::Read).expect("opened");
            log.len().expect("a length")
        };
        assert_eq!(len_after(image.lose_all()), 3);
        assert_eq!(len_after(image.keep_all()), 6);
        new.create_dir(Path::new("/db/new")).expect("made");
        assert_eq!(watcher.drain().len(), 1);
    }

    #[test]
    fn a_planted_defect_swallows_the_syncs_it_names() {
        let lost = |after| Some(Sabotage::LostSync { part: "log", after });
        let watcher = Watcher::new(fs(), Expect::exactly(0), lost(0));
        run(&*watcher.vfs());
        assert_eq!(watcher.lies(), 1);
        // The lie came after the images of the log's creation and of its
        // first append, and before every later one.
        assert_eq!(watcher.first_lie(), Some(2));
        // The appends to the log are still unsynced, both of them.
        let image = watcher.fs().crash_image();
        let unsynced = |path: &str| image.changes().iter().filter(|c| c.path == path).count();
        assert_eq!(unsynced("/db/log"), 2);
        assert_eq!(unsynced("/db/page"), 0);
        // A defect that begins after a handle's first sync leaves that one
        // honest.
        let later = Watcher::new(fs(), Expect::exactly(0), lost(1));
        run(&*later.vfs());
        assert_eq!(later.lies(), 0);

        let dir = Some(Sabotage::LostDirSync("db"));
        let watcher = Watcher::new(fs(), Expect::exactly(0), dir);
        run(&*watcher.vfs());
        assert_eq!(watcher.lies(), 1);
        let honest = self::watcher();
        run(&*honest.vfs());
        let names = |watcher: &Watcher| watcher.fs().crash_image().names().len();
        assert!(names(&watcher) > names(&honest));
        assert_eq!(honest.lies(), 0);
    }
}
