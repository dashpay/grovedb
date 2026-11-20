//! A set synced together on the operating system's files: which calls are
//! made for it, and which handles a failure of each of them stops.

use std::{
    cell::{Cell, RefCell},
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

/// A set whose items are all on one file system that shares a flush.
fn sharing<'a>(hand_over: FileCall<'a>, flush: FileCall<'a>) -> SetSync<'a> {
    SetSync {
        alone: &|_, _| panic!("no item of this set is synced alone"),
        shared: Some(SharedFlush {
            file_system: &|_| Ok(Some(1)),
            hand_over,
            flush,
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

/// The calls of this system for a set of nine, counted. Where the file
/// system of the temporary directory shares a flush, which on macOS is APFS,
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
        }),
    };
    sync_together_using(&items, &counted).unwrap();

    let shares = system
        .as_ref()
        .is_some_and(|shared| (shared.file_system)(&log.file).unwrap().is_some());
    assert_eq!(
        shares,
        cfg!(target_os = "macos"),
        "on macOS this test wants its temporary directory on APFS"
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

/// A handle of the set is synced by another thread while the set waits for
/// its flush, and that sync fails: the flush, which succeeds, does not vouch
/// for the handle.
#[test]
fn a_sync_that_fails_while_its_handle_waits_for_the_flush_fails_the_set() {
    let (_, root, _dir) = setup();
    let files = ["a", "b"].map(|name| written(&root.join(name)));
    let flushes = Cell::new(0);
    let fails_meanwhile = |_: &File| {
        flushes.set(flushes.get() + 1);
        files[0].sync_using(|_| Err(full())).unwrap_err();
        Ok(())
    };
    let set = sharing(&|_| Ok(()), &fails_meanwhile);
    let items = [SyncItem::Data(&files[0]), SyncItem::Data(&files[1])];

    let error = sync_together_using(&items, &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("an earlier sync"), "{error}");
    assert_eq!(flushes.get(), 1);
    assert!(is_stopped(&files[0]) && !is_stopped(&files[1]));
}

#[test]
fn a_failed_sync_of_one_item_stops_that_handle_and_the_set() {
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

        let error = sync_together_using(&items, &set).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert_eq!((calls.get(), flushes.get()), (3, 0));
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
    let set = SetSync {
        alone: &sync_alone,
        shared: Some(SharedFlush {
            file_system: &file_system,
            hand_over: &hand_over,
            flush: &flush,
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

    flushed.borrow_mut().clear();
    second_flush_fails.set(true);
    let error = sync_together_using(&items, &set).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);
    assert_eq!(flushed.borrow().len(), 2);
    assert!(!is_stopped(&one) && !is_stopped(&alone));
    assert!(two.iter().all(is_stopped));
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
