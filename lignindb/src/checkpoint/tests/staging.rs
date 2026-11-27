//! The staging directory of a creation (`CREATING_SUFFIX`): which
//! directory under that name a creation builds in and a deletion removes,
//! and that every other one is left as it is, entry for entry.

use std::{
    collections::BTreeMap,
    io::ErrorKind,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use lignin_io::{sim::CrashPoint, Vfs};

use super::{
    block, built_in, checkpointer, checkpoints_in, crash, files, open, open_on, staging,
    store_with_log, turn, Before, Meddling, DIR, TIDYING_OPS,
};
use crate::{
    checkpoint::{self, AtPath, Beside, CheckpointError, Finished, BUILDING_DIR_NAME},
    snapshot::Versions,
    txn::tests::{
        model::Committed,
        store::{Db, Rng},
        World,
    },
};

/// What a file or a directory is besides its name and what it holds: an
/// entry that was made in a directory and removed again shows in the
/// directory's modification time, and a file that was written again in
/// its own.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    inode: u64,
    mode: u32,
    modified: (i64, i64),
}

/// One entry of a directory, with everything of it that a call could
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File(Vec<u8>, Node),
    Dir(Node),
    Link(PathBuf),
}

/// The directory `dir` with everything under it, each entry by its path
/// inside `dir` and `dir` itself by the empty path: names, kinds, the
/// targets of links and the bytes of files, and for each file and
/// directory its [`Node`]. No link is followed.
fn exactly(dir: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut found = BTreeMap::new();
    let mut paths = vec![PathBuf::new()];
    while let Some(inside) = paths.pop() {
        let path = dir.join(&inside);
        let meta = std::fs::symlink_metadata(&path).expect("an entry");
        let node = Node {
            inode: meta.ino(),
            mode: meta.mode(),
            modified: (meta.mtime(), meta.mtime_nsec()),
        };
        let entry = if meta.file_type().is_symlink() {
            Entry::Link(std::fs::read_link(&path).expect("a link"))
        } else if meta.is_dir() {
            for listed in std::fs::read_dir(&path).expect("a directory") {
                paths.push(inside.join(listed.expect("an entry").file_name()));
            }
            Entry::Dir(node)
        } else {
            Entry::File(std::fs::read(&path).expect("a file"), node)
        };
        found.insert(inside, entry);
    }
    found
}

/// What lies under the name of a staging directory and is not one that a
/// creation left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Foreign {
    /// Files, a directory with a file, an empty directory and a link.
    Unrelated,
    /// Files under names that another store gives its own, a lock file
    /// among them.
    NamesOfAnotherStore,
    /// A whole database of this engine, closed: a checkpoint that was
    /// made elsewhere and given this name.
    AWholeDatabase,
    /// A file under the name of the lock file that holds something.
    ALockFileThatHoldsSomething,
    /// What a creation left, and a file of somebody's beside it.
    AFileBesideWhatACreationLeft,
    /// A link where a creation makes the directory it builds in.
    ALinkWhereACheckpointIsBuilt,
    /// A file under that name.
    AFileWhereACheckpointIsBuilt,
}

const FOREIGN: [Foreign; 7] = [
    Foreign::Unrelated,
    Foreign::NamesOfAnotherStore,
    Foreign::AWholeDatabase,
    Foreign::ALockFileThatHoldsSomething,
    Foreign::AFileBesideWhatACreationLeft,
    Foreign::ALinkWhereACheckpointIsBuilt,
    Foreign::AFileWhereACheckpointIsBuilt,
];

/// A store with three blocks, the directory of its checkpoints, and a
/// directory outside it that links lead to.
struct Scene {
    db: Arc<Db>,
    versions: Versions,
    /// What the store holds.
    model: Committed,
    checkpoints: PathBuf,
    outside: tempfile::TempDir,
    serial: u64,
}

impl Scene {
    fn on(store: (Arc<Db>, Versions)) -> Self {
        let (db, versions) = store;
        let model = {
            let mut world = World::new(&db, &versions);
            let mut rng = Rng::new(301);
            for round in 0..3 {
                block(&mut world, &mut rng, round * 20, 20);
                turn(&db, &versions);
            }
            world.model.clone()
        };
        let outside = tempfile::tempdir().expect("a directory");
        std::fs::create_dir(outside.path().join("theirs")).expect("a directory");
        std::fs::write(outside.path().join("theirs").join("kept"), b"as it was").expect("a file");
        Self {
            checkpoints: checkpoints_in(&db),
            db,
            versions,
            model,
            outside,
            serial: 0,
        }
    }

    fn new() -> Self {
        Self::on(store_with_log(24 << 10, 128))
    }

    /// A name among the checkpoints that nothing has had.
    fn fresh(&mut self, what: &str) -> PathBuf {
        self.serial += 1;
        self.checkpoints.join(format!("{what}-{}", self.serial))
    }

    /// A checkpoint of the store at `target`, made under another name
    /// and given that one: the name beside `target` is not touched.
    fn place(&mut self, target: &Path) {
        let made = self.fresh("made");
        (checkpointer(&self.db, &self.versions).create(&made)).expect("a checkpoint");
        std::fs::rename(&made, target).expect("a rename");
    }

    /// Puts `foreign` at `at`, and returns the name of the first entry
    /// that it lists and no creation puts into a staging directory.
    fn put(&mut self, foreign: Foreign, at: &Path) -> &'static str {
        let write = |name: &str, bytes: &[u8]| {
            let path = at.join(name);
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
            std::fs::write(path, bytes).expect("a file");
        };
        let theirs = self.outside.path().join("theirs");
        match foreign {
            Foreign::Unrelated => {
                write("notes.txt", b"a note");
                write("data/more.bin", &[7; 300]);
                std::fs::create_dir(at.join("empty")).expect("a directory");
                std::os::unix::fs::symlink(theirs.join("kept"), at.join("link")).expect("a link");
                "data"
            }
            Foreign::NamesOfAnotherStore => {
                write("CURRENT", b"MANIFEST-000001\n");
                write("MANIFEST-000001", &[1; 120]);
                write("LOCK", b"");
                write("000001.sst", &[2; 500]);
                "000001.sst"
            }
            Foreign::AWholeDatabase => {
                self.place(at);
                "MANIFEST"
            }
            Foreign::ALockFileThatHoldsSomething => {
                write("LOCK", b"held by somebody");
                "LOCK"
            }
            Foreign::AFileBesideWhatACreationLeft => {
                files::leave_built(at, false);
                write("theirs", b"somebody's");
                "theirs"
            }
            Foreign::ALinkWhereACheckpointIsBuilt => {
                write("LOCK", b"");
                std::os::unix::fs::symlink(&theirs, at.join(BUILDING_DIR_NAME)).expect("a link");
                BUILDING_DIR_NAME
            }
            Foreign::AFileWhereACheckpointIsBuilt => {
                write(BUILDING_DIR_NAME, b"a file");
                BUILDING_DIR_NAME
            }
        }
    }

    /// What the links of the foreign directories lead to is as it was.
    fn outside_is_as_it_was(&self) {
        let theirs = self.outside.path().join("theirs");
        let names: Vec<_> = (std::fs::read_dir(&theirs).expect("the directory"))
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(names, ["kept"]);
        assert_eq!(
            std::fs::read(theirs.join("kept")).expect("the file"),
            b"as it was"
        );
    }

    fn create(&self, target: &Path) -> Result<(), CheckpointError> {
        let created = checkpointer(&self.db, &self.versions).create(target);
        assert_eq!(self.versions.pins_held(), 0);
        created.map(drop)
    }

    fn delete(&self, path: &Path) -> Result<(), CheckpointError> {
        checkpoint::delete(&*self.db.vfs, path)
    }

    /// The checkpoint at `target` opens with what the store holds.
    fn is_the_checkpoint(&self, target: &Path) {
        open(&self.db, target).check(&self.model);
    }

    /// The store holds what it held, and no creation has left a pin.
    fn the_store_is_as_it_was(&self) {
        let mut world = World::new(&self.db, &self.versions);
        world.model = self.model.clone();
        world.check_committed();
        assert_eq!(self.versions.pins_held(), 0);
    }
}

/// Whether `refused` is the refusal of the directory `staged` for its
/// entry `found`, in the words of that error.
fn is_not_staging(refused: &Result<(), CheckpointError>, staged: &Path, found: &str) -> bool {
    let Err(error @ CheckpointError::NotStaging { dir, found: named }) = refused else {
        return false;
    };
    let words = format!(
        "{} holds {found:?}, which no creation of a checkpoint puts into its staging directory: \
         the directory is not taken for what a creation left, and nothing in it was removed",
        staged.display()
    );
    dir == staged && named == found && error.to_string() == words
}

fn is_no_directory(refused: &Result<(), CheckpointError>, path: &Path) -> bool {
    matches!(refused, Err(CheckpointError::NotADirectory { dir }) if dir == path)
}

/// **What lies under the name of a staging directory and was not left
/// there by a creation is as it was after every call that would have
/// taken it for one**, entry for entry: names, kinds, the targets of
/// links, the bytes of files, and the inode, the mode and the
/// modification time of every file and directory, so that nothing was
/// made in it and removed again either.
///
/// - A creation at the target beside it is refused for it, in the words
///   of [`CheckpointError::NotStaging`], and puts nothing at the target.
/// - So is a deletion that is given the directory itself: also a whole
///   database of this engine under that name is not deleted by it.
/// - A deletion of the target beside it, where nothing has the target's
///   name, finds nothing that a creation left
///   ([`CheckpointError::NotADirectory`]).
/// - A deletion of a checkpoint at the target deletes the checkpoint and
///   passes the directory over, with no error.
#[test]
fn what_no_creation_left_under_the_staging_name_is_as_it_was_after_every_call() {
    let mut scene = Scene::new();
    for foreign in FOREIGN {
        let target = scene.fresh("target");
        let staged = staging(&target);
        let found = scene.put(foreign, &staged);
        let before = exactly(&staged);
        let as_it_was = |after: &str| {
            assert_eq!(exactly(&staged), before, "{foreign:?}, after {after}");
        };

        let refused = scene.create(&target);
        assert!(
            is_not_staging(&refused, &staged, found),
            "{foreign:?}: {refused:?}"
        );
        assert!(!target.exists(), "{foreign:?}");
        as_it_was("a creation");

        let refused = scene.delete(&staged);
        assert!(
            is_not_staging(&refused, &staged, found),
            "{foreign:?}: {refused:?}"
        );
        as_it_was("a deletion of the directory");

        let refused = scene.delete(&target);
        assert!(
            is_no_directory(&refused, &target),
            "{foreign:?}: {refused:?}"
        );
        as_it_was("a deletion where nothing has the target's name");

        scene.place(&target);
        as_it_was("a checkpoint was given the target's name");
        (scene.delete(&target)).unwrap_or_else(|error| panic!("{foreign:?}: {error}"));
        assert!(!target.exists(), "{foreign:?}");
        as_it_was("a deletion of the checkpoint");

        // The same once more: no call has made it a creation's.
        let refused = scene.create(&target);
        assert!(
            is_not_staging(&refused, &staged, found),
            "{foreign:?}: {refused:?}"
        );
        as_it_was("the second creation");
        scene.outside_is_as_it_was();
    }
    scene.the_store_is_as_it_was();
}

/// **A deletion of a checkpoint finds what lies beside it only once the
/// checkpoint is gone, and passes over what no creation left there then
/// too.** Nothing has the staging name when the deletion begins. Before
/// the first file of the checkpoint is removed, each foreign directory is
/// given that name: the deletion returns `Ok`, the checkpoint is gone,
/// and the directory is as it was, entry for entry.
#[test]
fn what_comes_under_the_staging_name_while_a_checkpoint_is_deleted_is_passed_over() {
    let mut scene = Scene::new();
    for foreign in FOREIGN {
        let target = scene.fresh("target");
        let (staged, aside) = (staging(&target), scene.fresh("aside"));
        scene.put(foreign, &aside);
        scene.place(&target);
        let before = exactly(&aside);

        let (from, to) = (aside.clone(), staged.clone());
        let came = Mutex::new(false);
        let meddling = Meddling::before_remove(
            scene.db.vfs.clone(),
            Box::new(move |_| {
                let mut came = came.lock().expect("whether it came");
                if !std::mem::replace(&mut *came, true) {
                    std::fs::rename(&from, &to)?;
                }
                Ok(())
            }),
        );
        assert!(!staged.exists(), "{foreign:?}");
        checkpoint::delete(&meddling, &target)
            .unwrap_or_else(|error| panic!("{foreign:?}: {error}"));
        assert!(!target.exists() && !aside.exists(), "{foreign:?}");
        assert_eq!(exactly(&staged), before, "{foreign:?}");
        scene.outside_is_as_it_was();
    }
}

/// A call that takes a staging directory for what a creation left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Creation,
    /// Of a checkpoint that is at the target.
    DeletionOfTheCheckpoint,
    /// Of the target, where nothing has its name.
    DeletionWhereNothingIs,
    /// Of the staging directory, given itself.
    DeletionOfTheDirectory,
    /// [`checkpoint::finish`] at the target, where a checkpoint is.
    Finishing,
}

/// A call to the file layer before which the directory is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    /// The creation's making of the staging directory, where there is
    /// none: after its look at the target.
    ItsMaking,
    /// The taking of the lock, after the first look at what a creation
    /// left there.
    TheLock,
    /// The sync of the staging directory, after the look under the lock
    /// and before the look that the first removal follows.
    TheRemoval,
}

/// What a creation left under the staging name before the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Left {
    /// Nothing: no directory has the name.
    Nothing,
    /// Its lock file, and the directory it built in.
    WithItsLockFile,
    /// The directory it built in, without the lock file.
    WithoutALockFile,
    /// The directory, empty.
    Empty,
}

/// What is done to the directory at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// It is moved away, where it is there, and a foreign directory is
    /// given its name.
    Another(Foreign),
    /// A file of somebody's is put into it.
    AFileInIt,
    /// Something is written into its lock file.
    ItsLockFileIsWritten,
}

/// What is done once, before the call of `boundary` on `path`.
struct Trap {
    boundary: Boundary,
    path: PathBuf,
    change: Box<dyn FnOnce() + Send>,
}

/// The hook of one boundary: it springs the trap that is set for it.
fn hook(trap: &Arc<Mutex<Option<Trap>>>, boundary: Boundary) -> Option<Before> {
    let trap = trap.clone();
    Some(Box::new(move |path: &Path| {
        let mut set = trap.lock().expect("the trap");
        if (set.as_ref()).is_some_and(|trap| trap.boundary == boundary && trap.path == path) {
            (set.take().expect("the trap").change)();
        }
        Ok(())
    }))
}

/// `held` without the entries `names`, and without the directory itself:
/// what a snapshot is compared by where the directory was given an entry.
fn without(mut held: BTreeMap<PathBuf, Entry>, names: &[&str]) -> BTreeMap<PathBuf, Entry> {
    held.remove(Path::new(""));
    for name in names {
        held.remove(Path::new(name));
    }
    held
}

/// **What is decided under the lock is decided of the directory that is
/// there then, and again right before the first removal.** A directory
/// that comes under the staging name, and a file that comes into what a
/// creation left there, after a first look and before the takeover, is
/// preserved: at each boundary of the file layer between the first look
/// and the removal, for a creation, for each deletion that removes what a
/// creation left, and for [`checkpoint::finish`].
///
/// - Before the staging directory is made, where the creation found
///   nothing at the target: a foreign directory comes under its name, and
///   the creation is refused for it.
/// - Before the lock is taken, after the first look found what a creation
///   left: the look under the lock finds the foreign directory, the file,
///   or a lock file that holds something. Where what a creation left
///   listed its lock file, a foreign directory with no lock file is given
///   none: the taking of the lock fails.
/// - Before the removal, under the lock, while the names are made
///   durable: the look before the first removal does not find the
///   directory whose lock is held ([`CheckpointError::Replaced`]), or
///   finds the file, or the lock file that holds something.
///
/// In each case nothing of the foreign directory is removed, and it is as
/// it was, entry for entry, with one exception that the file layer leaves
/// no way around: **where what a creation left had no lock file, and the
/// foreign directory that takes its place before the lock has none
/// either, the call that takes the lock makes an empty one in it**, and
/// does nothing else to it. What a creation left is as it was where it
/// was moved to, or beside the file that came into it, but for a lock
/// file that it was given. Nothing is at the target.
///
/// A creation and a deletion of the staging directory itself answer the
/// look's error. A deletion of a checkpoint at the target deletes it and
/// passes over what it finds under the staging name then, with no error.
/// A deletion where nothing has the target's name finds nothing that a
/// creation left where the look read what no creation puts there, and
/// answers the look's error otherwise. And [`checkpoint::finish`] leaves
/// the checkpoint as it was and says that what is beside it is no
/// creation's.
#[test]
fn what_comes_between_the_first_look_and_the_takeover_is_preserved() {
    let trap: Arc<Mutex<Option<Trap>>> = Arc::default();
    let mut scene = Scene::on(files::store_on_meddling(|plain, _| Meddling {
        before_create_dir: hook(&trap, Boundary::ItsMaking),
        before_lock: hook(&trap, Boundary::TheLock),
        before_sync_dir: hook(&trap, Boundary::TheRemoval),
        ..Meddling::plain(plain)
    }));
    let with_a_lock_file = Foreign::NamesOfAnotherStore;
    let without_one = Foreign::Unrelated;
    let has_a_lock_file = |foreign: Foreign| foreign != without_one;
    let another = |foreign| (Call::Creation, Boundary::ItsMaking, Left::Nothing, foreign);
    let mut cases = vec![
        another(Change::Another(with_a_lock_file)),
        another(Change::Another(Foreign::AWholeDatabase)),
    ];
    for call in [
        Call::Creation,
        Call::DeletionOfTheCheckpoint,
        Call::DeletionWhereNothingIs,
        Call::DeletionOfTheDirectory,
        Call::Finishing,
    ] {
        for (boundary, left) in [
            (Boundary::TheLock, Left::WithItsLockFile),
            (Boundary::TheLock, Left::WithoutALockFile),
            (Boundary::TheLock, Left::Empty),
            (Boundary::TheRemoval, Left::WithItsLockFile),
            (Boundary::TheRemoval, Left::WithoutALockFile),
        ] {
            for change in [
                Change::Another(with_a_lock_file),
                Change::Another(without_one),
                Change::Another(Foreign::AWholeDatabase),
                Change::AFileInIt,
            ] {
                cases.push((call, boundary, left, change));
            }
            if left == Left::WithItsLockFile {
                cases.push((call, boundary, left, Change::ItsLockFileIsWritten));
            }
        }
    }

    let mut given_a_lock_file = 0;
    for (call, boundary, left, change) in cases {
        let context = format!("{call:?}, {boundary:?}, {left:?}, {change:?}");
        let target = scene.fresh("target");
        let (staged, aside, away) = (staging(&target), scene.fresh("aside"), scene.fresh("away"));
        match left {
            Left::Nothing => {}
            Left::WithItsLockFile => files::leave_built(&staged, false),
            Left::WithoutALockFile => files::leave_built(&staged, true),
            Left::Empty => std::fs::create_dir(&staged).expect("a directory"),
        }
        let checkpoint = matches!(call, Call::DeletionOfTheCheckpoint | Call::Finishing);
        if checkpoint {
            scene.place(&target);
        }
        let was_left = (left != Left::Nothing).then(|| exactly(&staged));
        let was_at_the_target = checkpoint.then(|| exactly(&target));
        let (foreign, found) = match change {
            Change::Another(foreign) => (Some(foreign), scene.put(foreign, &aside)),
            Change::AFileInIt => (None, "theirs"),
            Change::ItsLockFileIsWritten => (None, "LOCK"),
        };
        let theirs = foreign.map(|_| exactly(&aside));

        let path = match boundary {
            Boundary::ItsMaking | Boundary::TheRemoval => staged.clone(),
            Boundary::TheLock => staged.join("LOCK"),
        };
        let (at, from, to) = (staged.clone(), aside.clone(), away.clone());
        let change: Box<dyn FnOnce() + Send> = match foreign {
            Some(_) => Box::new(move || {
                if left != Left::Nothing {
                    std::fs::rename(&at, &to).expect("it is moved away");
                }
                std::fs::rename(&from, &at).expect("another is given its name");
            }),
            None => Box::new(move || {
                std::fs::write(at.join(found), b"put there meanwhile").expect("a file");
            }),
        };
        *trap.lock().expect("the trap") = Some(Trap {
            boundary,
            path,
            change,
        });
        let finished = match call {
            Call::Finishing => Some(checkpoint::finish(&*scene.db.vfs, &target)),
            _ => None,
        };
        let done = match call {
            Call::Creation => scene.create(&target),
            Call::DeletionOfTheCheckpoint | Call::DeletionWhereNothingIs => scene.delete(&target),
            Call::DeletionOfTheDirectory => scene.delete(&staged),
            Call::Finishing => Ok(()),
        };
        assert!(
            trap.lock().expect("the trap").is_none(),
            "{context}: {done:?}"
        );

        // What the look that met the change says. A lock that is taken
        // where what a creation left had no lock file makes one where
        // there is none: in the foreign directory, where that has come.
        #[derive(Debug, PartialEq, Eq)]
        enum Look {
            NotStaging,
            NoLockFile,
            Replaced,
        }
        let makes_a_lock_file = matches!(left, Left::WithoutALockFile | Left::Empty);
        let look = match (boundary, foreign) {
            (Boundary::TheLock, Some(foreign))
                if !has_a_lock_file(foreign) && !makes_a_lock_file =>
            {
                Look::NoLockFile
            }
            (Boundary::TheRemoval, Some(_)) => Look::Replaced,
            _ => Look::NotStaging,
        };
        match call {
            Call::Creation | Call::DeletionOfTheDirectory | Call::DeletionWhereNothingIs => {
                let said = match &look {
                    Look::NotStaging if call == Call::DeletionWhereNothingIs => {
                        is_no_directory(&done, &target)
                    }
                    Look::NotStaging => is_not_staging(&done, &staged, found),
                    Look::NoLockFile => matches!(
                        &done,
                        Err(CheckpointError::Io { action: "lock", path, source })
                            if *path == staged.join("LOCK")
                                && source.kind() == ErrorKind::NotFound
                    ),
                    Look::Replaced => {
                        matches!(&done, Err(CheckpointError::Replaced { dir }) if *dir == staged)
                    }
                };
                assert!(said, "{context}: {done:?}");
            }
            Call::DeletionOfTheCheckpoint => assert!(done.is_ok(), "{context}: {done:?}"),
            Call::Finishing => {
                let finished = finished.expect("it was asked");
                assert!(
                    matches!(
                        finished,
                        Ok(Finished {
                            at: AtPath::Database,
                            beside: Beside::NotStaging
                        })
                    ),
                    "{context}: {finished:?}"
                );
            }
        }
        match (call, &was_at_the_target) {
            (Call::Finishing, Some(was)) => assert_eq!(exactly(&target), *was, "{context}"),
            _ => assert!(!target.exists(), "{context}"),
        }

        match (&theirs, &was_left) {
            // The foreign directory is as it was, and so is what a
            // creation left, where it was moved to: each but for a lock
            // file that the taking of the lock made in it.
            (Some(theirs), was_left) => {
                let given = boundary == Boundary::TheLock
                    && makes_a_lock_file
                    && !foreign.is_some_and(has_a_lock_file);
                match given {
                    false => assert_eq!(exactly(&staged), *theirs, "{context}"),
                    true => {
                        let now = exactly(&staged);
                        assert!(
                            matches!(&now[Path::new("LOCK")], Entry::File(bytes, _) if bytes.is_empty()),
                            "{context}"
                        );
                        assert_eq!(
                            without(now, &["LOCK"]),
                            without(theirs.clone(), &[]),
                            "{context}"
                        );
                        given_a_lock_file += 1;
                    }
                }
                if let Some(was_left) = was_left {
                    match boundary == Boundary::TheRemoval && makes_a_lock_file {
                        false => assert_eq!(exactly(&away), *was_left, "{context}"),
                        true => assert_eq!(
                            without(exactly(&away), &["LOCK"]),
                            without(was_left.clone(), &[]),
                            "{context}"
                        ),
                    }
                }
            }
            // Their file is there, or what they wrote into the lock file,
            // and everything else a creation left is as it was beside it,
            // but for a lock file that it was given.
            (None, was_left) => {
                let now = exactly(&staged);
                assert!(
                    matches!(&now[Path::new(found)], Entry::File(bytes, _)
                        if bytes == b"put there meanwhile"),
                    "{context}"
                );
                let was_left = was_left.clone().expect("what a creation left");
                let given: &[&str] = match makes_a_lock_file {
                    true => &[found, "LOCK"],
                    false => &[found],
                };
                assert_eq!(
                    without(now, given),
                    without(was_left, &[found]),
                    "{context}"
                );
            }
        }
        scene.outside_is_as_it_was();
    }
    // The exception was met: for each call, a foreign directory with no
    // lock file in the place of what a creation left with none, with the
    // directory that was built in and empty.
    assert_eq!(given_a_lock_file, 5 * 2);
    scene.the_store_is_as_it_was();
}

/// **The staging directory is looked at once more before its lock file is
/// removed.** What an earlier creation built in it is removed entry by
/// entry, which takes as long as it takes. Here the directory is changed
/// right before the last of those removals, the one of the directory that
/// was built in, which is empty by then:
///
/// - A file of somebody's comes into the staging directory. A deletion
///   does not remove the lock file beside it: the directory stays, with
///   its lock file and their file, and a deletion that was given the
///   directory, or the target where nothing has its name, answers that
///   it did not find the directory as it has to be
///   ([`CheckpointError::Replaced`]). A creation builds in the
///   directory, finds the file before it gives its checkpoint a name,
///   and takes away what it built and nothing else.
/// - The staging directory is moved away, and a foreign directory is
///   given its name. The removal finds nothing to remove there, which is
///   the error of each of those calls, and no lock file of the foreign
///   directory is removed: it is as it was, entry for entry.
///
/// A deletion of a checkpoint at the target has deleted the checkpoint by
/// then, and returns no error for what it finds under the staging name.
#[test]
fn what_comes_while_a_staging_directory_is_emptied_keeps_its_lock_file() {
    let trap: Arc<Mutex<Option<Trap>>> = Arc::default();
    let mut scene = Scene::on(files::store_on_meddling(|plain, _| Meddling {
        before_remove_dir: hook(&trap, Boundary::TheRemoval),
        ..Meddling::plain(plain)
    }));
    for call in [
        Call::Creation,
        Call::DeletionOfTheCheckpoint,
        Call::DeletionWhereNothingIs,
        Call::DeletionOfTheDirectory,
        Call::Finishing,
    ] {
        for foreign in [None, Some(Foreign::NamesOfAnotherStore)] {
            let context = format!("{call:?}, {foreign:?}");
            let target = scene.fresh("target");
            let (staged, aside, away) =
                (staging(&target), scene.fresh("aside"), scene.fresh("away"));
            files::leave_built(&staged, false);
            let checkpoint = matches!(call, Call::DeletionOfTheCheckpoint | Call::Finishing);
            if checkpoint {
                scene.place(&target);
            }
            let theirs = foreign.map(|foreign| {
                scene.put(foreign, &aside);
                exactly(&aside)
            });
            let (at, from, to) = (staged.clone(), aside.clone(), away.clone());
            let change: Box<dyn FnOnce() + Send> = match foreign {
                Some(_) => Box::new(move || {
                    std::fs::rename(&at, &to).expect("it is moved away");
                    std::fs::rename(&from, &at).expect("another is given its name");
                }),
                None => Box::new(move || {
                    std::fs::write(at.join("theirs"), b"put there meanwhile").expect("a file");
                }),
            };
            *trap.lock().expect("the trap") = Some(Trap {
                boundary: Boundary::TheRemoval,
                path: built_in(&target),
                change,
            });
            let finished = match call {
                Call::Finishing => Some(checkpoint::finish(&*scene.db.vfs, &target)),
                _ => None,
            };
            let done = match call {
                Call::Creation => scene.create(&target),
                Call::DeletionOfTheCheckpoint | Call::DeletionWhereNothingIs => {
                    scene.delete(&target)
                }
                Call::DeletionOfTheDirectory => scene.delete(&staged),
                Call::Finishing => Ok(()),
            };
            assert!(
                trap.lock().expect("the trap").is_none(),
                "{context}: {done:?}"
            );

            let gone = |done: &Result<(), CheckpointError>| {
                matches!(
                    done,
                    Err(CheckpointError::Io { action: "remove the directory", path, source })
                        if *path == built_in(&target) && source.kind() == ErrorKind::NotFound
                )
            };
            match (call, foreign) {
                // The creation built in the directory that their file had
                // come into, and found the file before the rename.
                (Call::Creation, None) => assert!(
                    matches!(&done, Err(CheckpointError::Replaced { dir }) if *dir == staged),
                    "{context}: {done:?}"
                ),
                (Call::DeletionOfTheDirectory | Call::DeletionWhereNothingIs, None) => assert!(
                    matches!(&done, Err(CheckpointError::Replaced { dir }) if *dir == staged),
                    "{context}: {done:?}"
                ),
                (
                    Call::Creation | Call::DeletionOfTheDirectory | Call::DeletionWhereNothingIs,
                    Some(_),
                ) => {
                    assert!(gone(&done), "{context}: {done:?}")
                }
                (Call::DeletionOfTheCheckpoint, _) => {
                    assert!(done.is_ok(), "{context}: {done:?}")
                }
                (Call::Finishing, _) => {
                    let finished = finished.expect("it was asked");
                    assert!(
                        matches!(
                            finished,
                            Ok(Finished {
                                at: AtPath::Database,
                                beside: Beside::NotStaging
                            })
                        ),
                        "{context}: {finished:?}"
                    );
                }
            }
            assert_eq!(target.exists(), call == Call::Finishing, "{context}");

            match &theirs {
                // The foreign directory is as it was. What a creation
                // left is where it was moved to, with its lock file and
                // the directory that was built in, emptied.
                Some(theirs) => {
                    assert_eq!(exactly(&staged), *theirs, "{context}");
                    let left: Vec<PathBuf> = exactly(&away).into_keys().collect();
                    let held = ["", "LOCK", BUILDING_DIR_NAME].map(PathBuf::from);
                    assert_eq!(left, held, "{context}");
                }
                // Their file is there, beside the lock file, which is
                // empty, and nothing else is.
                None => {
                    let now = exactly(&staged);
                    assert_eq!(
                        now.keys().collect::<Vec<_>>(),
                        ["", "LOCK", "theirs"].map(Path::new),
                        "{context}"
                    );
                    assert!(
                        matches!(&now[Path::new("LOCK")], Entry::File(bytes, _) if bytes.is_empty()),
                        "{context}"
                    );
                    assert!(
                        matches!(&now[Path::new("theirs")], Entry::File(bytes, _)
                            if bytes == b"put there meanwhile"),
                        "{context}"
                    );
                }
            }
            scene.outside_is_as_it_was();
        }
    }
    scene.the_store_is_as_it_was();
}

/// How far a creation had come when it was cut, as its staging directory
/// and its target say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    /// Nothing is there: the creation had made nothing that a cut kept.
    Nothing,
    /// The staging directory is made, and is empty.
    DirectoryMade,
    /// Its lock file is in it, and nothing else.
    LockTaken,
    /// The directory the checkpoint is built in is made, which marks the
    /// staging directory: it is empty or holds its own lock file.
    MarkMade,
    /// Files of the checkpoint are in it, and no manifest.
    PartlyWritten,
    /// The whole checkpoint is in it, with its manifest, under the name
    /// it was built under: before it was given the name of the target,
    /// or after that name was taken back.
    Whole,
    /// The checkpoint has the name of the target.
    Named,
}

/// The stage that `state` shows of a creation at `target`. A staging
/// directory lists nothing but what a creation puts into it, whatever
/// the cut kept.
fn stage_of(state: &dyn Vfs, target: &Path, context: &str) -> Stage {
    let names = |dir: &Path| -> Option<Vec<String>> {
        let entries = state.read_dir(dir).ok()?;
        let name = |entry: lignin_io::DirEntry| entry.name.into_string().expect("a name");
        Some(entries.into_iter().map(name).collect())
    };
    let staged = names(&staging(target));
    for name in staged.iter().flatten() {
        assert!(
            name == "LOCK" || name == BUILDING_DIR_NAME,
            "{context}: the staging directory holds {name:?}"
        );
    }
    let built = names(&built_in(target));
    if names(target).is_some() {
        assert!(built.is_none(), "{context}: both names are there");
        return Stage::Named;
    }
    match (staged, built) {
        (None, _) => Stage::Nothing,
        (Some(staged), None) if staged.is_empty() => Stage::DirectoryMade,
        (Some(_), None) => Stage::LockTaken,
        (Some(_), Some(built)) if built.iter().all(|name| name == "LOCK") => Stage::MarkMade,
        (Some(_), Some(built)) if built.iter().any(|name| name == "MANIFEST") => Stage::Whole,
        (Some(_), Some(_)) => Stage::PartlyWritten,
    }
}

/// **What takes the place of a staging directory once its lock file has
/// gone is not removed.** The lock file of what a creation left is
/// removed, and then the directory, by a removal that fails where the
/// directory is not empty. Here the emptied directory is moved away
/// before that removal, and a foreign directory has its name: the removal
/// answers that it is not empty, and the foreign directory is as it was,
/// entry for entry. A deletion that has deleted the checkpoint at the
/// target reads what is under the name then and returns no error, and
/// [`checkpoint::finish`] says that what is beside the checkpoint is no
/// creation's. A deletion of the staging directory itself, and one of
/// the target where nothing has its name, answer the removal's error.
#[test]
fn what_takes_the_place_of_a_staging_directory_whose_lock_file_went_is_not_removed() {
    let trap: Arc<Mutex<Option<Trap>>> = Arc::default();
    let mut scene = Scene::on(files::store_on_meddling(|plain, _| Meddling {
        before_remove_dir: hook(&trap, Boundary::TheRemoval),
        ..Meddling::plain(plain)
    }));
    for call in [
        Call::DeletionOfTheCheckpoint,
        Call::DeletionWhereNothingIs,
        Call::DeletionOfTheDirectory,
        Call::Finishing,
    ] {
        let context = format!("{call:?}");
        let target = scene.fresh("target");
        let (staged, aside, away) = (staging(&target), scene.fresh("aside"), scene.fresh("away"));
        files::leave_built(&staged, false);
        if matches!(call, Call::DeletionOfTheCheckpoint | Call::Finishing) {
            scene.place(&target);
        }
        scene.put(Foreign::NamesOfAnotherStore, &aside);
        let theirs = exactly(&aside);
        let (at, from, to) = (staged.clone(), aside.clone(), away.clone());
        *trap.lock().expect("the trap") = Some(Trap {
            boundary: Boundary::TheRemoval,
            path: staged.clone(),
            change: Box::new(move || {
                std::fs::rename(&at, &to).expect("it is moved away");
                std::fs::rename(&from, &at).expect("another is given its name");
            }),
        });
        let finished = match call {
            Call::Finishing => Some(checkpoint::finish(&*scene.db.vfs, &target)),
            _ => None,
        };
        let done = match call {
            Call::DeletionOfTheCheckpoint | Call::DeletionWhereNothingIs => scene.delete(&target),
            Call::DeletionOfTheDirectory => scene.delete(&staged),
            Call::Creation | Call::Finishing => Ok(()),
        };
        assert!(
            trap.lock().expect("the trap").is_none(),
            "{context}: {done:?}"
        );

        match call {
            Call::DeletionOfTheCheckpoint => assert!(done.is_ok(), "{context}: {done:?}"),
            Call::Finishing => {
                let finished = finished.expect("it was asked");
                assert!(
                    matches!(
                        finished,
                        Ok(Finished {
                            at: AtPath::Database,
                            beside: Beside::NotStaging
                        })
                    ),
                    "{context}: {finished:?}"
                );
            }
            _ => assert!(
                matches!(
                    &done,
                    Err(CheckpointError::Io { action: "remove the directory", path, source })
                        if *path == staged && source.kind() == ErrorKind::DirectoryNotEmpty
                ),
                "{context}: {done:?}"
            ),
        }
        assert_eq!(target.exists(), call == Call::Finishing, "{context}");
        assert_eq!(exactly(&staged), theirs, "{context}");
        // What a creation left is where it was moved to, and holds
        // nothing any more.
        let left: Vec<PathBuf> = exactly(&away).into_keys().collect();
        assert_eq!(left, [PathBuf::from("")], "{context}");
        scene.outside_is_as_it_was();
    }
    scene.the_store_is_as_it_was();
}

/// **The staging directory whose lock file goes is the one whose lock is
/// held.** Here, once what was built in what a creation left is removed,
/// that directory is moved away, and a directory with an empty lock file
/// of its own and nothing else, which the rule admits, has its name. The
/// last look does not find the lock file that is held there
/// ([`CheckpointError::Replaced`], which is each call's error), and
/// neither that lock file nor the directory is removed: it is as it was,
/// entry for entry.
#[test]
fn a_directory_with_another_lock_file_in_the_place_of_a_staging_directory_is_not_removed() {
    let first: Arc<Mutex<Option<Trap>>> = Arc::default();
    let second: Arc<Mutex<Option<Trap>>> = Arc::default();
    let mut scene = Scene::on(files::store_on_meddling(|plain, _| Meddling {
        before_remove_dir: hook(&first, Boundary::TheRemoval),
        before_list: hook(&second, Boundary::TheRemoval),
        ..Meddling::plain(plain)
    }));
    for call in [
        Call::DeletionOfTheCheckpoint,
        Call::DeletionWhereNothingIs,
        Call::DeletionOfTheDirectory,
        Call::Finishing,
    ] {
        let context = format!("{call:?}");
        let target = scene.fresh("target");
        let (staged, aside, away) = (staging(&target), scene.fresh("aside"), scene.fresh("away"));
        files::leave_built(&staged, false);
        let checkpoint = matches!(call, Call::DeletionOfTheCheckpoint | Call::Finishing);
        if checkpoint {
            scene.place(&target);
        }
        std::fs::create_dir(&aside).expect("a directory");
        std::fs::write(aside.join("LOCK"), b"").expect("a file");
        let theirs = exactly(&aside);
        // The directory above is listed first by the look that follows
        // the removal of what was built: the exchange is made before
        // that listing, and set up when that removal begins.
        let (at, from, to) = (staged.clone(), aside.clone(), away.clone());
        let exchange = Trap {
            boundary: Boundary::TheRemoval,
            path: staged.parent().expect("a directory").to_path_buf(),
            change: Box::new(move || {
                std::fs::rename(&at, &to).expect("it is moved away");
                std::fs::rename(&from, &at).expect("another is given its name");
            }),
        };
        let then = second.clone();
        *first.lock().expect("the trap") = Some(Trap {
            boundary: Boundary::TheRemoval,
            path: built_in(&target),
            change: Box::new(move || *then.lock().expect("the trap") = Some(exchange)),
        });
        let done = match call {
            Call::DeletionOfTheCheckpoint | Call::DeletionWhereNothingIs => scene.delete(&target),
            Call::DeletionOfTheDirectory => scene.delete(&staged),
            Call::Finishing => checkpoint::finish(&*scene.db.vfs, &target).map(drop),
            Call::Creation => Ok(()),
        };
        for trap in [&first, &second] {
            assert!(
                trap.lock().expect("the trap").is_none(),
                "{context}: {done:?}"
            );
        }

        assert!(
            matches!(&done, Err(CheckpointError::Replaced { dir }) if *dir == staged),
            "{context}: {done:?}"
        );
        assert_eq!(target.exists(), call == Call::Finishing, "{context}");
        assert_eq!(exactly(&staged), theirs, "{context}");
        // What a creation left is where it was moved to, with its lock
        // file and without what was built in it.
        let left: Vec<PathBuf> = exactly(&away).into_keys().collect();
        assert_eq!(left, ["", "LOCK"].map(PathBuf::from), "{context}");
        scene.outside_is_as_it_was();
    }
    scene.the_store_is_as_it_was();
}

/// **Every stage at which a creation is cut is taken over by the next
/// creation at the target, and removed by a deletion.** The power is cut
/// after each operation of a creation, and of the states a crash can
/// leave then, the one that keeps every change, the one that loses every
/// unsynced one and three between are looked at: the staging directory
/// lists nothing but its lock file and the directory the checkpoint is
/// built in, and the stage is one of [`Stage`]. On each state the next
/// creation makes its checkpoint, which opens at its version, and a
/// deletion of the target leaves neither name. And each state is
/// recovered a second time and given to a deletion of the target alone,
/// which removes what the cut left under both names. Each stage is met: the
/// directory made, the lock taken, the mark made, the files partly
/// written, the whole checkpoint under the name it was built under, and
/// the checkpoint at the target.
///
/// And where the checkpoint was given the name of the target and the
/// name was taken back, with neither rename durable, it is whole in the
/// staging directory again, and at the target in some of the states a
/// cut leaves then: both are dealt with in the same way.
#[test]
fn every_stage_of_a_cut_creation_is_taken_over_or_removed() {
    use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};

    /// Looks at states that a crash of `made` can leave now: the one
    /// that keeps every change, the one that loses every unsynced one,
    /// and `between` of those that keep the first changes of names.
    fn look_at(
        made: &crash::Made,
        between: usize,
        context: &str,
        met: &mut BTreeMap<Stage, usize>,
    ) {
        let image = made.fs.crash_image();
        let mut plans = vec![image.keep_all(), image.lose_all()];
        plans.extend(image.prefixes().take(between));
        for (index, plan) in plans.iter().enumerate() {
            let Ok(state) = image.recover(plan) else {
                continue;
            };
            let context = format!("{context}, crash plan {index}");
            let stage = stage_of(&state, &made.target, &context);
            *met.entry(stage).or_default() += 1;
            let live = open_on(Arc::new(state.clone()), Path::new(DIR), &made.db)
                .unwrap_or_else(|error| panic!("{context}: the live store: {error}"));
            made.check_leftover(&state, &live, &context);
            // And the same state once more, as the cut left it: a
            // deletion of the target removes what is there of both names,
            // with no creation before it.
            let untouched = image.recover(plan).expect("the same state");
            match checkpoint::delete(&untouched, &made.target) {
                Ok(()) => assert!(stage != Stage::Nothing, "{context}"),
                Err(CheckpointError::NotADirectory { .. }) => {
                    assert_eq!(stage, Stage::Nothing, "{context}")
                }
                Err(error) => panic!("{context}: the deletion: {error}"),
            }
            for path in [made.target.clone(), staging(&made.target)] {
                assert!(untouched.read_dir(&path).is_err(), "{context}: {path:?}");
            }
        }
    }
    let mut met: BTreeMap<Stage, usize> = BTreeMap::new();

    let whole = crash::made(33, crash::Files::Linked);
    let from = whole.fs.stats().ops;
    whole.maker().create(&whole.target).expect("a checkpoint");
    let ops = whole.fs.stats().ops - from;
    for cut in 1..=ops {
        let made = crash::made(33, crash::Files::Linked);
        assert_eq!(made.fs.stats().ops, from, "the same store");
        made.fs.crash_at(CrashPoint::AfterOp(from + cut));
        let created = made.maker().create(&made.target);
        assert_eq!(
            created.is_ok(),
            cut + TIDYING_OPS >= ops,
            "cut {cut} of {ops}"
        );
        look_at(&made, 3, &format!("cut {cut} of {ops}"), &mut met);
    }
    for stage in [
        Stage::DirectoryMade,
        Stage::LockTaken,
        Stage::MarkMade,
        Stage::PartlyWritten,
        Stage::Whole,
        Stage::Named,
    ] {
        assert!(met.contains_key(&stage), "{stage:?} was not met: {met:?}");
    }

    // The name is given and taken back: every sync of the directory above
    // fails once the set of syncs is made.
    let made = crash::made(33, crash::Files::Linked);
    let above = made.target.parent().expect("a directory").to_path_buf();
    let sync = FaultRule::new(Fault::Eio).on(OpSet::of(&[OpKind::SyncDir]));
    made.fs.add_rule(sync.skip(1).forever().at(&above));
    let failed = made.maker().create(&made.target);
    made.fs.clear_rules();
    assert!(
        matches!(
            &failed,
            Err(CheckpointError::Io {
                action: "make durable the name of",
                ..
            })
        ),
        "{failed:?}"
    );
    assert_eq!(stage_of(&made.fs, &made.target, "taken back"), Stage::Whole);
    let before = met.clone();
    look_at(&made, usize::MAX, "the name was taken back", &mut met);
    for stage in [Stage::Whole, Stage::Named] {
        assert!(met[&stage] > before[&stage], "{stage:?}: {met:?}");
    }
    println!("the stages of a cut creation, by the states that showed them: {met:?}");
}

/// **What a creation of a build before the staging directory had a
/// directory of its own for the checkpoint left**: the files of the
/// checkpoint directly under the staging name. An empty one, and one with
/// its lock file alone, is what a creation of this build leaves too, and
/// is taken over and removed. One that holds a part of a checkpoint, or
/// a whole one, is not told from a database that somebody put there: it
/// is refused by a creation and by a deletion of the directory, passed
/// over by a deletion of the target, and is as it was after each. Once
/// the directory is removed, as the message's row in the operator's
/// documents says, the creation makes its checkpoint.
#[test]
fn what_a_creation_of_an_earlier_build_left_is_taken_over_where_it_holds_nothing() {
    let mut scene = Scene::new();
    let a_manifest = std::fs::read(scene.db.dir.join("MANIFEST")).expect("a manifest");
    type Left<'a> = &'a [(&'a str, &'a [u8])];
    let leave = |staged: &Path, files: Left<'_>| {
        std::fs::create_dir(staged).expect("a directory");
        for (name, bytes) in files {
            let path = staged.join(name);
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
            std::fs::write(path, bytes).expect("a file");
        }
    };

    let holding_nothing: [Left<'_>; 2] = [&[], &[("LOCK", b"")]];
    for files in holding_nothing {
        let target = scene.fresh("target");
        let staged = staging(&target);
        leave(&staged, files);
        scene.create(&target).expect("a checkpoint");
        assert!(!staged.exists());
        scene.is_the_checkpoint(&target);
        scene.delete(&target).expect("the checkpoint is deleted");
        for given in [&target, &staged] {
            leave(&staged, files);
            scene.delete(given).expect("what was left is removed");
            assert!(!staged.exists() && !target.exists());
        }
    }

    let holding_a_part: [(Left<'_>, &str); 3] = [
        (&[("LOCK", b""), ("seg/00000001.seg", b"a part")], "seg"),
        (&[("MANIFEST.tmp", b"a part")], "MANIFEST.tmp"),
        (
            &[
                ("LOCK", b""),
                ("MANIFEST", &a_manifest),
                ("wal/00000001.log", b"a log"),
            ],
            "MANIFEST",
        ),
    ];
    for (files, found) in holding_a_part {
        let target = scene.fresh("target");
        let staged = staging(&target);
        leave(&staged, files);
        let before = exactly(&staged);
        for refused in [scene.create(&target), scene.delete(&staged)] {
            assert!(is_not_staging(&refused, &staged, found), "{refused:?}");
            assert_eq!(exactly(&staged), before);
        }
        let refused = scene.delete(&target);
        assert!(is_no_directory(&refused, &target), "{refused:?}");
        scene.place(&target);
        scene.delete(&target).expect("the checkpoint is deleted");
        assert!(!target.exists());
        assert_eq!(exactly(&staged), before);

        // The operator's step, and the call again.
        std::fs::remove_dir_all(&staged).expect("the directory is removed");
        scene.create(&target).expect("a checkpoint");
        assert!(!staged.exists());
        scene.is_the_checkpoint(&target);
    }
    scene.the_store_is_as_it_was();
}

/// **The checkpoint is the directory that was built, and nothing of the
/// staging directory comes with it**: what has the name of the target
/// holds an empty lock file, the manifest, and the directories and files
/// that the manifest names, and nothing else; and nothing has the staging
/// name once the creation has returned.
#[test]
fn a_checkpoint_holds_nothing_of_its_staging_directory() {
    let mut scene = Scene::new();
    let target = scene.fresh("target");
    scene.create(&target).expect("a checkpoint");
    assert!(!staging(&target).exists());
    let held = exactly(&target);
    let top: Vec<&str> = (held.keys())
        .filter(|path| path.components().count() == 1)
        .map(|path| path.to_str().expect("a name"))
        .collect();
    for name in &top {
        assert!(
            ["LOCK", "MANIFEST", "seg", "table", "val", "wal"].contains(name),
            "{top:?}"
        );
    }
    assert!(
        top.contains(&"LOCK") && top.contains(&"MANIFEST"),
        "{top:?}"
    );
    assert!(
        matches!(&held[Path::new("LOCK")], Entry::File(bytes, _) if bytes.is_empty()),
        "the lock file holds something"
    );
    assert!(!held.keys().any(|path| path.ends_with(BUILDING_DIR_NAME)));
    scene.is_the_checkpoint(&target);
}
