//! Many unsynced changes of names: these tests count the changes that are
//! looked at to find what a change builds on, for three sequences of
//! operations. The count is no count of what the simulator's ordered maps
//! do for each change looked at, and no time.

use super::*;
use crate::sim::state::with_what_they_build_on_counted as built_on;

const CHANGES: u64 = if cfg!(miri) { 400 } else { 100_000 };

/// A durable file, and then `changes` unsynced changes of its one name: the
/// file is removed and made again by turns.
fn one_name_changed(changes: u64) -> SimVfs {
    let fs = sim(41);
    drop(create_durable(&fs, "/MANIFEST", b"first"));
    for change in 0..changes {
        if change % 2 == 0 {
            fs.remove_file(p("/MANIFEST")).unwrap();
        } else {
            let made = fs.open(p("/MANIFEST"), OpenMode::Write(Create::New));
            drop(made.unwrap());
        }
    }
    fs
}

#[test]
fn one_name_chain_counts_seeds_and_predecessors() {
    let fs = one_name_changed(CHANGES);
    let image = fs.crash_image();
    let names = image.names();
    assert_eq!(names.len() as u64, CHANGES);

    // What a sync of the directory asks for: every change, with what it
    // builds on. Each is looked at once as asked for, and each but the
    // last once more as what the next one builds on.
    let every = names.iter().map(|name| name.op);
    let (kept, looked_at) = built_on(&image.dirs, &image.name_ops, every);
    assert_eq!((kept.len() as u64, looked_at), (CHANGES, 2 * CHANGES - 1));

    // What a crash that keeps the last change asks for: each is looked at
    // once.
    let last = names.len() - 1;
    let (kept, looked_at) = built_on(&image.dirs, &image.name_ops, [names[last].op]);
    assert_eq!((kept.len() as u64, looked_at), (CHANGES, CHANGES));

    // The crash states are what they were: a change is kept with every one
    // before it.
    assert_eq!(image.names_kept(&names_only(vec![last])).len(), names.len());
    assert_eq!(
        image.names_kept(&names_only(vec![last / 2])).len(),
        last / 2 + 1
    );
    assert_eq!(
        read(&recover(&image, &image.lose_all()), "/MANIFEST"),
        b"first"
    );
    assert_eq!(read(&recover(&image, &image.keep_all()), "/MANIFEST"), b"");
    assert!(!exists(
        &recover(&image, &names_only(vec![last - 1])),
        "/MANIFEST"
    ));
    let drawn = image.names_kept(&image.sample(0));
    assert_eq!(drawn, (0..drawn.len()).collect::<Vec<_>>());

    // And the sync makes them all durable.
    fs.sync_dir(p("/")).unwrap();
    let synced = fs.crash_image();
    assert!(synced.names().is_empty());
    assert_eq!(
        read(&recover(&synced, &synced.lose_all()), "/MANIFEST"),
        b""
    );
    check(&fs);
}

#[test]
fn the_changes_looked_at_grow_with_the_changes_kept() {
    let counts: &[usize] = if cfg!(miri) { &[12] } else { &[50, 100, 200] };
    for &count in counts {
        // A name renamed to and fro: each rename builds on the one before
        // it by both of its names.
        let fs = sim(42);
        drop(create(&fs, "/a", b""));
        for turn in 0..count {
            let (from, to) = if turn % 2 == 0 {
                ("/a", "/b")
            } else {
                ("/b", "/a")
            };
            fs.rename(p(from), p(to)).unwrap();
        }
        check(&fs);
        let image = fs.crash_image();
        let last = image.names()[count].op;
        let (kept, looked_at) = built_on(&image.dirs, &image.name_ops, [last]);
        assert_eq!((kept.len(), looked_at), (count + 1, 2 * count as u64));

        // A directory that is emptied and removed: the removal builds on
        // the last change of each name that was in it.
        let fs = sim(43);
        fs.create_dir(p("/d")).unwrap();
        for file in 0..count {
            let path = format!("/d/{file}");
            drop(create(&fs, &path, b""));
            fs.remove_file(p(&path)).unwrap();
        }
        fs.remove_dir(p("/d")).unwrap();
        check(&fs);
        let image = fs.crash_image();
        let removal = image.names()[2 * count + 1].op;
        let (kept, looked_at) = built_on(&image.dirs, &image.name_ops, [removal]);
        assert_eq!(
            (kept.len(), looked_at),
            (2 * count + 2, 2 * count as u64 + 2)
        );
    }
}
