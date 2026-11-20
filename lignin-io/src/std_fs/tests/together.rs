//! A set synced together on the operating system's files: which calls are
//! made for it, and which handles a failure of each of them stops.

use std::{
    cell::{Cell, RefCell},
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

use super::*;
use crate::sim::{SimConfig, SimVfs};

fn written(path: &Path) -> StdFile {
    let file = StdFile::open(path, OpenMode::Write(Create::New)).unwrap();
    file.write_at(b"content", 0).unwrap();
    file
}

/// Whether the handle's syncs fail as they do after a failed sync.
#[track_caller]
fn is_stopped(file: &StdFile) -> bool {
    let mut stopped = Vec::new();
    for sync in [StdFile::sync_data, StdFile::sync_all] {
        let synced = sync(file);
        if let Err(error) = &synced {
            assert_eq!(error.kind(), ErrorKind::Other);
            assert!(error.to_string().contains("an earlier sync"), "{error}");
        }
        stopped.push(synced.is_err());
    }
    assert_eq!(stopped[0], stopped[1]);
    stopped[0]
}

fn full() -> io::Error {
    ErrorKind::StorageFull.into()
}

fn ino(file: &File) -> u64 {
    file.metadata().unwrap().ino()
}

/// The tests that take a turn for a set of their own take it one at a
/// time, so that what one of them sees of [`SYNC_TURNS`] is its own set's
/// doing and not another test's. A set that another test syncs through
/// [`StdVfs`] on a file system that shares a flush is not kept out.
static ONE_SET_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_set_at_a_time() -> MutexGuard<'static, ()> {
    ONE_SET_AT_A_TIME
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Whether a set has the turn, so that no sync that is made alone can begin.
fn a_set_has_the_turn() -> bool {
    let tried = SYNC_TURNS.try_read();
    matches!(tried, Err(std::sync::TryLockError::WouldBlock))
}

/// Whether a sync that is made alone is under way, so that no set can take
/// its turn.
fn a_set_has_to_wait() -> bool {
    let tried = SYNC_TURNS.try_write();
    matches!(tried, Err(std::sync::TryLockError::WouldBlock))
}

/// Whether `dir` is on an APFS volume, as `df` and `mount` say and not as
/// this backend finds out.
#[cfg(target_os = "macos")]
fn is_on_apfs(dir: &Path) -> bool {
    let said = |command: &mut Command| {
        let output = command.output().unwrap();
        assert!(output.status.success(), "{command:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let df = said(Command::new("/bin/df").arg(dir));
    let device = df
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    said(Command::new("/sbin/mount").args(["-t", "apfs"]))
        .lines()
        .any(|line| line.starts_with(&format!("{device} on ")))
}

#[cfg(not(target_os = "macos"))]
fn is_on_apfs(_: &Path) -> bool {
    false
}

/// A set whose items are all on one file system that shares a flush.
fn sharing<'a>(hand_over: FileCall<'a>, flush: FileCall<'a>) -> SetSync<'a> {
    sharing_in(&apart_from_other_syncs, hand_over, flush)
}

/// As [`sharing`], with the turn taken by `turn`.
fn sharing_in<'a>(turn: Turn<'a>, hand_over: FileCall<'a>, flush: FileCall<'a>) -> SetSync<'a> {
    SetSync {
        alone: &|_, _| panic!("no item of this set is synced alone"),
        shared: Some(SharedFlush {
            file_system: &|_| Ok(Some(1)),
            hand_over,
            flush,
            turn,
        }),
    }
}

/// The directories of something like a checkpoint: `root` and six that are
/// made below it.
fn seven_directories(root: &Path) -> Vec<PathBuf> {
    let below = [
        "made",
        "made/wal",
        "made/seg",
        "made/val",
        "made/table",
        "made/tmp",
    ];
    let dirs: Vec<PathBuf> = below.iter().map(|dir| root.join(dir)).collect();
    for dir in &dirs {
        fs::create_dir(dir).unwrap();
    }
    [vec![root.to_path_buf()], dirs].concat()
}

/// Two files and the seven directories, as a list.
fn nine<'a>(
    log: &'a dyn VfsFile,
    manifest: &'a dyn VfsFile,
    dirs: &'a [PathBuf],
) -> Vec<SyncItem<'a>> {
    let mut items = vec![SyncItem::Data(log), SyncItem::All(manifest)];
    items.extend(dirs.iter().map(|dir| SyncItem::Dir(dir)));
    items
}

/// The calls of this system for a set of nine, counted. Where the temporary
/// directory is on APFS, which on macOS it is unless it was put elsewhere,
/// they are nine calls that hand an item to the device and one flush.
/// Everywhere else they are the nine single syncs.
#[test]
fn a_set_of_nine_is_nine_calls_and_one_flush_where_a_flush_is_shared() {
    let (_, root, _dir) = setup();
    let dirs = seven_directories(&root);
    let manifest = written(&root.join("made/MANIFEST"));
    let log = written(&root.join("made/wal/1.log"));
    let items = nine(&log, &manifest, &dirs);

    let (alone, handed_over, flushes) = (RefCell::new(Vec::new()), Cell::new(0), Cell::new(0));
    let count_alone = |file: &File, member: Member| {
        alone.borrow_mut().push(member);
        (SYSTEM.alone)(file, member)
    };
    let system = SYSTEM.shared;
    let count_hand_over = |file: &File| {
        handed_over.set(handed_over.get() + 1);
        (system.as_ref().unwrap().hand_over)(file)
    };
    let count_flush = |file: &File| {
        flushes.set(flushes.get() + 1);
        (system.as_ref().unwrap().flush)(file)
    };
    let counted = SetSync {
        alone: &count_alone,
        shared: system.as_ref().map(|shared| SharedFlush {
            file_system: shared.file_system,
            hand_over: &count_hand_over,
            flush: &count_flush,
            turn: shared.turn,
        }),
    };
    sync_together_using(&items, &counted).unwrap();

    let shares = system
        .as_ref()
        .is_some_and(|shared| (shared.file_system)(&log.file).unwrap().is_some());
    assert_eq!(
        shares,
        is_on_apfs(&root),
        "a flush is shared on APFS and nowhere else"
    );
    let mut singly = vec![Member::Data, Member::All];
    singly.extend([Member::Dir; 7]);
    let expected = match shares {
        true => (Vec::new(), 9, 1),
        false => (singly, 0, 0),
    };
    assert_eq!(
        (alone.into_inner(), handed_over.get(), flushes.get()),
        expected
    );
}

#[test]
fn the_flush_of_a_set_is_asked_on_its_last_item() {
    let _alone = one_set_at_a_time();
    let (_, root, _dir) = setup();
    let (first, last) = (written(&root.join("first")), written(&root.join("last")));
    let asked = RefCell::new(Vec::new());
    let flush = |file: &File| {
        asked
            .borrow_mut()
            .push((ino(file), file.metadata()?.is_dir()));
        Ok(())
    };
    let set = sharing(&|_| Ok(()), &flush);

    let ends_with_a_file = [
        SyncItem::Data(&first),
        SyncItem::Dir(&root),
        SyncItem::All(&last),
    ];
    sync_together_using(&ends_with_a_file, &set).unwrap();
    let ends_with_a_directory = [SyncItem::Data(&last), SyncItem::Dir(&root)];
    sync_together_using(&ends_with_a_directory, &set).unwrap();
    sync_together_using(&[], &set).unwrap();

    let root_ino = fs::metadata(&root).unwrap().ino();
    assert_eq!(
        *asked.borrow(),
        [(ino(&last.file), false), (root_ino, true)]
    );
}

#[test]
fn a_failed_flush_of_a_set_stops_every_handle_that_waited_for_it() {
    let _alone = one_set_at_a_time();
    let (vfs, root, _dir) = setup();
    let files = ["a", "b", "c"].map(|name| written(&root.join(name)));
    let outside = written(&root.join("outside"));
    let set = sharing(&|_| Ok(()), &|_| Err(full()));
    let items = [
        SyncItem::Data(&files[0]),
        SyncItem::Dir(&root),
        SyncItem::All(&files[1]),
        SyncItem::Data(&files[2]),
    ];

    let error = sync_together_using(&items, &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);

    assert!(files.iter().all(is_stopped));
    for file in &files {
        let later = vfs.sync_together(&[SyncItem::Dir(&root), SyncItem::Data(file)]);
        assert_eq!(later.unwrap_err().kind(), ErrorKind::Other);
    }
    assert!(!is_stopped(&outside));
    vfs.sync_together(&[SyncItem::Data(&outside), SyncItem::Dir(&root)])
        .unwrap();
}

/// A sync of a handle of the set fails after the set looked at the handle
/// and before the set's turn: the set fails with what the handle remembers,
/// and hands nothing over, not the items before that one in the list
/// either.
#[test]
fn a_sync_that_fails_before_the_turn_of_its_set_fails_the_set() {
    let _alone = one_set_at_a_time();
    let (_, root, _dir) = setup();
    let files = ["a", "b"].map(|name| written(&root.join(name)));
    let asked = Cell::new(0);
    let the_second_fails_meanwhile = |_: &File| {
        asked.set(asked.get() + 1);
        if asked.get() == 2 {
            files[1].sync_using(|_| Err(full())).unwrap_err();
        }
        Ok(Some(1))
    };
    let set = SetSync {
        alone: &|_, _| panic!("no item is synced alone"),
        shared: Some(SharedFlush {
            file_system: &the_second_fails_meanwhile,
            hand_over: &|_| panic!("no item is handed over"),
            flush: &|_| panic!("no flush is made"),
            turn: &apart_from_other_syncs,
        }),
    };
    let items = [SyncItem::Data(&files[0]), SyncItem::Data(&files[1])];

    let error = sync_together_using(&items, &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("an earlier sync"), "{error}");
    assert_eq!(asked.get(), 2);
    assert!(!is_stopped(&files[0]) && is_stopped(&files[1]));
}

/// The failure of a flush, which counts how often its text was made and
/// how often a set had the turn when it was.
#[derive(Debug, Default)]
struct Told {
    made: Arc<AtomicUsize>,
    made_in_the_turn: Arc<AtomicUsize>,
}

impl fmt::Display for Told {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.made.fetch_add(1, Ordering::Relaxed);
        if a_set_has_the_turn() {
            self.made_in_the_turn.fetch_add(1, Ordering::Relaxed);
        }
        f.write_str("the flush failed")
    }
}

impl std::error::Error for Told {}

/// From the first item that a set hands over to the record of its flush's
/// failure no other sync runs, and the set takes one turn for it, not one
/// after another. A sync that is made alone cannot begin; the
/// record has the failure's text, which is made in the turn; and a sync on
/// another thread that is started during the flush waits: of a handle of the
/// set it then fails, and of another handle it is made once the failure is
/// on record.
#[test]
fn a_set_has_the_turn_from_its_first_hand_over_to_the_record_of_its_failed_flush() {
    let _alone = one_set_at_a_time();
    let (_, root, _dir) = setup();
    let files = ["a", "b", "c"].map(|name| written(&root.join(name)));
    let outside = written(&root.join("outside"));
    let items = [
        SyncItem::Data(&files[0]),
        SyncItem::Dir(&root),
        SyncItem::Data(&files[1]),
        SyncItem::All(&files[2]),
        SyncItem::Data(&files[0]),
    ];
    let told = Told::default();
    let (made, made_in_the_turn) = (told.made.clone(), told.made_in_the_turn.clone());
    let (handed_over, told) = (Cell::new(0), Cell::new(Some(told)));
    let turns = Cell::new(0);
    let turn = || {
        turns.set(turns.get() + 1);
        apart_from_other_syncs()
    };

    std::thread::scope(|scope| {
        let beside = RefCell::new(Vec::new());
        let hand_over = |_: &File| {
            assert!(turns.get() == 1 && a_set_has_the_turn());
            handed_over.set(handed_over.get() + 1);
            Ok(())
        };
        let flush = |_: &File| {
            assert!(turns.get() == 1 && a_set_has_the_turn());
            assert_eq!(handed_over.get(), 5);
            let of_the_set = || files[1].sync_using(|_| Ok(()));
            let of_another = || {
                outside.sync_using(|_| {
                    let on_record = guard(&files[1].sync_failure)?.is_some();
                    assert!(on_record, "a sync ran before the failure was on record");
                    Ok(())
                })
            };
            let mut beside = beside.borrow_mut();
            beside.push(scope.spawn(of_the_set));
            beside.push(scope.spawn(of_another));
            Err(io::Error::other(told.take().unwrap()))
        };
        sync_together_using(&items, &sharing_in(&turn, &hand_over, &flush)).unwrap_err();
        assert_eq!(turns.get(), 1);

        let (made, in_the_turn) = (&made, &made_in_the_turn);
        let (made, in_the_turn) = (
            made.load(Ordering::Relaxed),
            in_the_turn.load(Ordering::Relaxed),
        );
        assert!(made > 0 && made == in_the_turn, "{in_the_turn} of {made}");
        let mut beside = beside
            .into_inner()
            .into_iter()
            .map(|sync| sync.join().unwrap());
        assert_eq!(beside.next().unwrap().unwrap_err().kind(), ErrorKind::Other);
        beside.next().unwrap().unwrap();
    });
    assert!(files.iter().all(is_stopped));
    assert!(!is_stopped(&outside));
}

/// Two sets with a handle in common take their turns one after the other:
/// the second, started while the first hands over, hands nothing over until
/// the first has made its flush and put a failure of it on record. So a
/// handle is stopped by the failed flush that its bytes were handed over
/// for, and left alone by one that failed after its own had been made.
#[test]
fn two_sets_with_a_handle_in_common_do_not_mix() {
    let _alone = one_set_at_a_time();
    for the_first_fails in [false, true] {
        let (_, root, _dir) = setup();
        let [a, b, c] = ["a", "b", "c"].map(|name| written(&root.join(name)));
        let events = Mutex::new(Vec::new());
        let event = |what: &'static str| events.lock().unwrap().push(what);
        let (second_hands_over, first_waits) = mpsc::channel();
        let second_hands_over = Mutex::new(second_hands_over);

        let second = std::thread::scope(|scope| {
            let (started, second) = (Cell::new(false), Cell::new(None));
            let the_second_set = || {
                let hand_over = |_: &File| {
                    let _ = second_hands_over.lock().unwrap().send(());
                    event("the second hands over");
                    Ok(())
                };
                let flush = |_: &File| {
                    event("the second flushes");
                    match the_first_fails {
                        true => Ok(()),
                        false => Err(full()),
                    }
                };
                let items = [SyncItem::Data(&a), SyncItem::Data(&c)];
                sync_together_using(&items, &sharing(&hand_over, &flush))
            };
            let hand_over = |_: &File| {
                event("the first hands over");
                if !started.replace(true) {
                    second.set(Some(scope.spawn(the_second_set)));
                }
                Ok(())
            };
            let flush = |_: &File| {
                let waited = first_waits.recv_timeout(Duration::from_millis(100));
                assert_eq!(waited, Err(mpsc::RecvTimeoutError::Timeout));
                event("the first flushes");
                match the_first_fails {
                    true => Err(full()),
                    false => Ok(()),
                }
            };
            let items = [SyncItem::Data(&a), SyncItem::Data(&b)];
            let first = sync_together_using(&items, &sharing(&hand_over, &flush));
            assert_eq!(first.is_err(), the_first_fails);
            second.take().unwrap().join().unwrap()
        });

        let of_the_first = [
            "the first hands over",
            "the first hands over",
            "the first flushes",
        ];
        let of_the_second = [
            "the second hands over",
            "the second hands over",
            "the second flushes",
        ];
        let events = events.into_inner().unwrap();
        let stopped = [&a, &b, &c].map(is_stopped);
        if the_first_fails {
            assert_eq!(second.unwrap_err().kind(), ErrorKind::Other);
            assert_eq!(events, of_the_first);
            assert_eq!(stopped, [true, true, false]);
        } else {
            assert_eq!(second.unwrap_err().kind(), ErrorKind::StorageFull);
            assert_eq!(events, [of_the_first, of_the_second].concat());
            assert_eq!(stopped, [true, false, true]);
        }
    }
}

/// A sync that is made alone keeps a set from its turn while it runs: of a
/// file, of a directory, and each single sync of a set.
#[test]
fn a_sync_that_is_made_alone_keeps_a_set_from_its_turn() {
    let (_, root, _dir) = setup();
    let file = written(&root.join("file"));
    let seen = Cell::new(0);
    let look = |_: &File| {
        assert!(a_set_has_to_wait());
        seen.set(seen.get() + 1);
        Ok(())
    };

    file.sync_using(look).unwrap();
    sync_dir_using(&open_dir(&root).unwrap(), look).unwrap();
    let singly = SetSync {
        alone: &|file, _| look(file),
        shared: None,
    };
    let items = [SyncItem::Data(&file), SyncItem::Dir(&root)];
    sync_together_using(&items, &singly).unwrap();
    assert_eq!(seen.get(), 4);
}

/// A handle whose syncs were stopped fails a set with what it remembers,
/// before its file system is asked about. A handle that syncs, on a file
/// system that cannot be told, fails the set with that error and is not
/// stopped by it: no sync of it was tried.
#[test]
fn a_stopped_handle_fails_a_set_before_its_file_system_is_asked() {
    let (_, root, _dir) = setup();
    let (fine, failed) = (written(&root.join("fine")), written(&root.join("failed")));
    failed.sync_using(|_| Err(full())).unwrap_err();
    let asked = Cell::new(0);
    let cannot_tell = |_: &File| {
        asked.set(asked.get() + 1);
        Err(ErrorKind::PermissionDenied.into())
    };
    let set = SetSync {
        alone: &|_, _| panic!("no item is synced"),
        shared: Some(SharedFlush {
            file_system: &cannot_tell,
            hand_over: &|_| panic!("no item is handed over"),
            flush: &|_| panic!("no flush is made"),
            turn: &|| panic!("no turn is taken"),
        }),
    };

    let error = sync_together_using(&[SyncItem::Data(&failed)], &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert_eq!(asked.get(), 0);

    let error = sync_together_using(&[SyncItem::Data(&fine)], &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    assert_eq!(asked.get(), 1);
    assert!(!is_stopped(&fine));
}

#[test]
fn a_failed_sync_of_one_item_stops_that_handle_and_the_set() {
    let _alone = one_set_at_a_time();
    for shared in [false, true] {
        let (_, root, _dir) = setup();
        let files = ["a", "b", "c"].map(|name| written(&root.join(name)));
        let (calls, flushes) = (Cell::new(0), Cell::new(0));
        let second_fails = || {
            calls.set(calls.get() + 1);
            match calls.get() {
                2 => Err(full()),
                _ => Ok(()),
            }
        };
        let flush = |_: &File| {
            flushes.set(flushes.get() + 1);
            Ok(())
        };
        let hand_over = |_: &File| second_fails();
        let alone = |_: &File, _| second_fails();
        let set = match shared {
            true => sharing(&hand_over, &flush),
            false => SetSync {
                alone: &alone,
                shared: None,
            },
        };
        let items = [
            SyncItem::Data(&files[0]),
            SyncItem::All(&files[1]),
            SyncItem::Data(&files[2]),
        ];

        let error = sync_together_using(&items, &set).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StorageFull);
        assert_eq!((calls.get(), flushes.get()), (2, 0));
        assert_eq!(files.each_ref().map(is_stopped), [false, true, false]);

        // Where a flush is shared, the stopped handle is seen before anything
        // is handed over. Where none is, it is seen after the single sync of
        // the item before it.
        let error = sync_together_using(&items, &set).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        let calls_by_then = if shared { 2 } else { 3 };
        assert_eq!((calls.get(), flushes.get()), (calls_by_then, 0));
    }
}

#[test]
fn a_handle_whose_sync_failed_fails_the_set_it_is_in() {
    let (vfs, root, _dir) = setup();
    let (fine, failed) = (written(&root.join("fine")), written(&root.join("failed")));
    failed.sync_using(|_| Err(full())).unwrap_err();

    let items = [
        SyncItem::Data(&fine),
        SyncItem::Data(&failed),
        SyncItem::Dir(&root),
    ];
    for _ in 0..2 {
        let error = vfs.sync_together(&items).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(error.to_string().contains("an earlier sync"), "{error}");
    }
    assert!(!is_stopped(&fine));
}

#[test]
fn a_file_of_another_file_layer_in_a_set_is_synced_by_its_own_call() {
    let _alone = one_set_at_a_time();
    let (vfs, root, _dir) = setup();
    let own = written(&root.join("own"));
    let sim = SimVfs::new(SimConfig::new(1));
    let foreign = sim
        .open(Path::new("/foreign"), OpenMode::Write(Create::New))
        .unwrap();
    foreign.write_at(b"content", 0).unwrap();

    vfs.sync_together(&[
        SyncItem::Data(&own),
        SyncItem::Data(&*foreign),
        SyncItem::All(&*foreign),
        SyncItem::Dir(&root),
    ])
    .unwrap();
    assert_eq!(sim.stats().syncs, 2);
    assert!(sim.crash_image().changes().is_empty());

    let flushes = Cell::new(0);
    let fail = |_: &File| {
        flushes.set(flushes.get() + 1);
        Err(full())
    };
    let set = sharing(&|_| Ok(()), &fail);
    sync_together_using(&[SyncItem::Data(&*foreign)], &set).unwrap();
    assert_eq!(flushes.get(), 0);

    let with_one_of_its_own = [SyncItem::All(&*foreign), SyncItem::Data(&own)];
    let error = sync_together_using(&with_one_of_its_own, &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);
    assert_eq!(flushes.get(), 1);
    assert_eq!(sim.stats().syncs, 4);
    foreign.sync_data().unwrap();
    assert!(is_stopped(&own));
}

/// Three file systems, told apart here by the directory an item is in: two
/// that share a flush each, and one that shares none.
#[test]
#[cfg_attr(miri, ignore = "Miri has no inode numbers")]
fn a_set_is_flushed_once_for_each_file_system_that_shares_a_flush() {
    let _alone = one_set_at_a_time();
    let (_, root, _dir) = setup();
    let dirs = ["one", "two", "alone"].map(|name| root.join(name));
    for dir in &dirs {
        fs::create_dir(dir).unwrap();
    }
    let one = written(&dirs[0].join("file"));
    let two = ["first", "second"].map(|name| written(&dirs[1].join(name)));
    let alone = written(&dirs[2].join("file"));
    let inos = |files: &[&StdFile], dir: &Path| -> Vec<u64> {
        let dir = fs::metadata(dir).unwrap().ino();
        files
            .iter()
            .map(|file| ino(&file.file))
            .chain([dir])
            .collect()
    };
    let on_one = inos(&[&one], &dirs[0]);
    let on_two = inos(&[&two[0], &two[1]], &dirs[1]);
    let file_system = |file: &File| {
        let file = ino(file);
        Ok(match (on_one.contains(&file), on_two.contains(&file)) {
            (true, _) => Some(1),
            (_, true) => Some(2),
            _ => None,
        })
    };

    let (synced_alone, handed_over) = (RefCell::new(Vec::new()), RefCell::new(Vec::new()));
    let flushed = RefCell::new(Vec::new());
    let second_flush_fails = Cell::new(false);
    let sync_alone = |file: &File, _| {
        synced_alone.borrow_mut().push(ino(file));
        Ok(())
    };
    let hand_over = |file: &File| {
        handed_over.borrow_mut().push(ino(file));
        Ok(())
    };
    let flush = |file: &File| {
        flushed.borrow_mut().push(ino(file));
        match second_flush_fails.get() && flushed.borrow().len() == 2 {
            true => Err(full()),
            false => Ok(()),
        }
    };
    let turns = Cell::new(0);
    let turn = || {
        turns.set(turns.get() + 1);
        apart_from_other_syncs()
    };
    let set = SetSync {
        alone: &sync_alone,
        shared: Some(SharedFlush {
            file_system: &file_system,
            hand_over: &hand_over,
            flush: &flush,
            turn: &turn,
        }),
    };
    let items = [
        SyncItem::Data(&one),
        SyncItem::Data(&two[0]),
        SyncItem::Data(&alone),
        SyncItem::Dir(&dirs[1]),
        SyncItem::Dir(&dirs[2]),
        SyncItem::Dir(&dirs[0]),
        SyncItem::All(&two[1]),
    ];

    sync_together_using(&items, &set).unwrap();
    let alone_inos = inos(&[&alone], &dirs[2]);
    assert_eq!(*synced_alone.borrow(), alone_inos);
    assert_eq!(handed_over.borrow().len(), 5);
    assert_eq!(*flushed.borrow(), [on_one[1], ino(&two[1].file)]);
    assert_eq!(turns.get(), 2);

    flushed.borrow_mut().clear();
    second_flush_fails.set(true);
    let error = sync_together_using(&items, &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);
    assert_eq!(flushed.borrow().len(), 2);
    assert!(!is_stopped(&one) && !is_stopped(&alone));
    assert!(two.iter().all(is_stopped));
}

/// The calls of [`StdVfs`] and of its files take their turns themselves.
/// While a turn is held as a set holds it, none of these calls ends: a sync
/// of a file's data, of all of a file, of a directory, a set. And while a
/// turn is held as a single sync holds it, the set ends on Linux, where its
/// items share no flush, and does not end on APFS, where they share one.
#[test]
fn the_calls_of_the_file_layer_wait_for_a_turn_that_is_held() {
    let _alone = one_set_at_a_time();
    let (vfs, root, _dir) = setup();
    let file = vfs
        .open(&root.join("file"), OpenMode::Write(Create::New))
        .unwrap();
    file.write_at(b"content", 0).unwrap();
    let a_while = Duration::from_millis(100);
    let nothing_ended = Err(mpsc::RecvTimeoutError::Timeout);
    let together = || vfs.sync_together(&[SyncItem::Data(&*file), SyncItem::Dir(&root)]);
    let calls: [&(dyn Fn() -> io::Result<()> + Sync); 4] = [
        &|| file.sync_data(),
        &|| file.sync_all(),
        &|| vfs.sync_dir(&root),
        &together,
    ];

    std::thread::scope(|scope| {
        let (began, begins) = mpsc::channel();
        let (ended, ends) = mpsc::channel();
        let turn = apart_from_other_syncs();
        for call in calls {
            let (began, ended) = (began.clone(), ended.clone());
            scope.spawn(move || {
                began.send(()).unwrap();
                ended.send(call().is_ok()).unwrap();
            });
        }
        drop((began, ended));
        for _ in calls {
            begins.recv().unwrap();
        }
        assert_eq!(ends.recv_timeout(a_while), nothing_ended);
        drop(turn);
        for _ in calls {
            assert!(ends.recv().unwrap());
        }

        let shares = is_on_apfs(&root);
        if shares || cfg!(target_os = "linux") {
            let (began, begins) = mpsc::channel();
            let (ended, ends) = mpsc::channel();
            let turn = beside_other_syncs();
            scope.spawn(move || {
                began.send(()).unwrap();
                ended.send(together().is_ok()).unwrap();
            });
            begins.recv().unwrap();
            if shares {
                assert_eq!(ends.recv_timeout(a_while), nothing_ended);
                drop(turn);
                assert!(ends.recv().unwrap());
            } else {
                assert!(ends.recv().unwrap());
                drop(turn);
            }
        }
    });
}

fn report(what: &str, mut times: Vec<Duration>) {
    times.sort();
    let ms = |at: usize| times[at].as_secs_f64() * 1e3;
    let count = times.len();
    println!(
        "{what}: n={count} min {:.3} median {:.3} p90 {:.3} max {:.3} ms",
        ms(0),
        ms(count / 2),
        ms(count * 9 / 10),
        ms(count - 1)
    );
}

/// The time of one set of nine against the same nine syncs made one by one,
/// each after every one of the nine was changed. Run it where the temporary
/// directory is on the disk the answer is wanted for:
///
/// ```text
/// cargo test -p lignin-io -- --ignored --nocapture measure_a_set
/// ```
#[test]
#[ignore = "a measurement: it prints times and asserts none"]
fn measure_a_set_of_nine_against_nine_single_syncs() {
    const ROUNDS: usize = 200;
    let (vfs, root, _dir) = setup();
    let dirs = seven_directories(&root);
    let append = OpenMode::Append(Create::New);
    let manifest = vfs.open(&root.join("made/MANIFEST"), append).unwrap();
    let log = vfs.open(&root.join("made/wal/1.log"), append).unwrap();
    for dir in &dirs {
        fs::write(dir.join("name 0"), b"").unwrap();
    }
    let items = nine(&*log, &*manifest, &dirs);
    vfs.sync_together(&items).unwrap();

    let (mut one_by_one, mut together) = (Vec::new(), Vec::new());
    for round in 0..2 * ROUNDS {
        log.append(&[7; 4096]).unwrap();
        manifest.append(&[9; 512]).unwrap();
        for dir in &dirs {
            let (old, new) = (format!("name {round}"), format!("name {}", round + 1));
            fs::rename(dir.join(old), dir.join(new)).unwrap();
        }
        let started = Instant::now();
        if round % 2 == 0 {
            log.sync_data().unwrap();
            manifest.sync_all().unwrap();
            for dir in &dirs {
                vfs.sync_dir(dir).unwrap();
            }
            one_by_one.push(started.elapsed());
        } else {
            vfs.sync_together(&items).unwrap();
            together.push(started.elapsed());
        }
    }
    report("nine single syncs", one_by_one);
    report("one set of nine", together);
}
