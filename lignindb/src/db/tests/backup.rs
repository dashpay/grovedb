//! OPS-08: a consistent backup from a checkpoint. The checkpoint is made
//! of a database that goes on committing, writing back and reclaiming
//! while the backup is copied from it: the copy waits, with its plan made
//! and nothing copied, until commits were made beside it. What is restored
//! from the backup is the database as the checkpoint's version had it,
//! sound by the physical checker.

use std::{
    fmt, io,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
};

use lignin_io::{
    sim::{SimConfig, SimVfs},
    DirEntry, OpenMode, Vfs, VfsFile, VfsLock,
};

use super::{full::sound, *};
use crate::{
    backup::{
        tests::world::{check_reads, write, State},
        Backup, BackupError,
    },
    checkpoint::{self, Checkpointer},
    txn::tests::store::Rng,
};

const LIVE: &str = "/db";
const CHECKPOINT: &str = "/db/checkpoints/7";
const BACKUP: &str = "/backups/one";

fn on(fs: &SimVfs) -> Options {
    Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    }
}

/// What a copy waits for, once.
type Wait = Box<dyn FnOnce() + Send>;

/// A file layer in which the first making of a directory is followed by a
/// wait: a copy into it makes its target when it has measured every file
/// of its source, and before it copies one.
struct Gate {
    fs: SimVfs,
    after_the_first_directory: Mutex<Option<Wait>>,
}

impl fmt::Debug for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gate").finish_non_exhaustive()
    }
}

impl Vfs for Gate {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.fs.open(path, mode)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.fs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.fs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.fs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let made = self.fs.create_dir(path);
        let wait = self
            .after_the_first_directory
            .lock()
            .expect("the wait")
            .take();
        if let Some(wait) = wait {
            wait();
        }
        made
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.fs.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.fs.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.fs.lock_existing(path)
    }
}

/// The commits that are made while a copy waits inside.
const INSIDE: u64 = 5;

/// Opens the database in `dir` of `fs`, which must hold one, and checks it
/// against `state`: what a reader reads, and every physical rule.
fn check_database(fs: &SimVfs, dir: &str, state: &State, context: &str) {
    let options = Options {
        create_if_missing: false,
        ..on(fs)
    };
    let db = Db::open(dir, &options).unwrap_or_else(|error| panic!("{context}: {error}"));
    halt_background(&db);
    check_reads(&db, state, context);
    sound(&db, context);
}

#[test]
fn a_backup_from_a_checkpoint_is_the_database_at_the_checkpoints_version() {
    for seed in 0..6u64 {
        let context = format!("seed {seed}");
        let fs = SimVfs::new(SimConfig::new(seed));
        let elsewhere = SimVfs::new(SimConfig::new(seed + 50));
        elsewhere
            .create_dir(Path::new("/backups"))
            .expect("a directory");
        let db = Db::open(LIVE, &on(&fs)).expect("a new database");
        let mut state = State::new();
        let mut rng = Rng::new(seed);
        for number in 1..=25 {
            write(&db, &mut state, &mut rng, number);
        }

        // The checkpoint, in a directory inside the database's own, as a
        // node makes it (P-14). It is not opened.
        fs.create_dir(Path::new("/db/checkpoints"))
            .expect("a directory");
        let created = {
            let committer = db.shared.committer();
            Checkpointer::new(&committer)
                .create(Path::new(CHECKPOINT))
                .unwrap_or_else(|error| panic!("{context}: {error}"))
        };
        let at = state.clone();
        assert_eq!(created.version.get(), at.version(), "{context}");

        // The live database goes on while the backup is copied: commits,
        // write-back and the reclaiming of what they drop, on its own
        // threads and on one more. The copy waits when it has made its
        // target, until that thread has committed beside it.
        let done = AtomicBool::new(false);
        let live = Mutex::new((state, rng));
        let (inside, is_inside) = mpsc::channel();
        let (written, are_written) = mpsc::channel();
        let gate = Gate {
            fs: elsewhere.clone(),
            after_the_first_directory: Mutex::new(Some(Box::new(move || {
                let _ = inside.send(());
                // A writer that has gone lets the copy go on.
                let _ = are_written.recv();
            }))),
        };
        let (copied, beside) = std::thread::scope(|threads| {
            let (db, live, done) = (&db, &live, &done);
            let writer = threads.spawn(move || {
                let mut live = live.lock().expect("the writer's state");
                let (state, rng) = &mut *live;
                let mut number = 26;
                let mut commit = || {
                    write(db, state, rng, number);
                    number += 1;
                };
                while is_inside.try_recv().is_err() {
                    if done.load(Ordering::Acquire) {
                        return 0;
                    }
                    commit();
                }
                for _ in 0..INSIDE {
                    commit();
                }
                let _ = written.send(());
                while !done.load(Ordering::Acquire) {
                    commit();
                }
                INSIDE
            });
            let copied = {
                // The writer ends with the copy, also with one that panics.
                let _ending = Ending(done);
                Backup::between(Arc::new(fs.clone()), Arc::new(gate))
                    .copy(Path::new(CHECKPOINT), Path::new(BACKUP))
            };
            (copied, writer.join().expect("the writer ends"))
        });
        let copied = copied.unwrap_or_else(|error| panic!("{context}: {error}"));
        assert_eq!(beside, INSIDE, "{context}: the copy did not wait");
        assert_eq!(copied.version, created.version, "{context}");
        let (state, _) = live.into_inner().expect("the writer's state");
        assert!(state.version() >= at.version() + INSIDE, "{context}");

        // The backup is the database of the checkpoint's version, whatever
        // was committed since, and it stays so after a power cut.
        check_database(&elsewhere, BACKUP, &at, &format!("{context}, the backup"));
        let image = elsewhere.crash_image();
        let cut = image.recover(&image.lose_all()).expect("the plan fits");
        check_database(
            &cut,
            BACKUP,
            &at,
            &format!("{context}, the backup after a power cut"),
        );

        // A checkpoint that a handle has open is not copied. It is opened
        // beside the live database, whose background work goes on.
        let open = Db::open(CHECKPOINT, &on(&fs)).expect("the checkpoint opens");
        let refused =
            Backup::new(Arc::new(fs.clone())).copy(Path::new(CHECKPOINT), Path::new("/second"));
        assert!(
            matches!(refused, Err(BackupError::Open { .. })),
            "{context}: {refused:?}"
        );
        halt_background(&open);
        check_reads(&open, &at, &format!("{context}, the checkpoint"));
        drop(open);

        // The checkpoint goes, and the live database and the backup are as
        // they were.
        checkpoint::delete(&fs, Path::new(CHECKPOINT)).expect("the checkpoint is deleted");
        check_reads(&db, &state, &format!("{context}, the live database"));
        db.close().expect("the session ends");
        check_database(&fs, LIVE, &state, &format!("{context}, the live database"));

        // A restore, where the database was lost: into a new place, and
        // what is restored is written like any database.
        let restored = SimVfs::new(SimConfig::new(seed + 90));
        Backup::between(Arc::new(elsewhere.clone()), Arc::new(restored.clone()))
            .copy(Path::new(BACKUP), Path::new(LIVE))
            .unwrap_or_else(|error| panic!("{context}: the restore: {error}"));
        check_database(&restored, LIVE, &at, &format!("{context}, restored"));
        let db = Db::open(LIVE, &on(&restored)).expect("the restored database");
        let mut state = at;
        let mut rng = Rng::new(seed + 3);
        for number in 100..115 {
            write(&db, &mut state, &mut rng, number);
        }
        db.close().expect("the session ends");
        check_database(
            &restored,
            LIVE,
            &state,
            &format!("{context}, written after the restore"),
        );
    }
}
