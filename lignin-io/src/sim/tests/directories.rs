//! The rename of a directory: one change of names, which a crash keeps whole
//! or not at all, with everything in the directory as durable as it was.

use super::*;
use crate::sim::trace::Fnv;

/// What a crash can leave of the names, whatever it does to the files.
fn states(fs: &SimVfs) -> BTreeSet<Tree> {
    let image = fs.crash_image();
    name_subsets(&image)
        .map(|names| tree(&recover(&image, &names_only(names))))
        .collect()
}

#[test]
fn a_directory_rename_is_whole_and_needs_both_parents_synced() {
    let fs = sim(41);
    for dir in ["/from", "/to", "/from/moved"] {
        fs.create_dir(p(dir)).unwrap();
    }
    drop(create_durable(&fs, "/from/moved/file", b"inside"));
    fs.sync_dir(p("/from")).unwrap();
    fs.sync_dir(p("/")).unwrap();
    assert!(fs.crash_image().names().is_empty());

    fs.rename(p("/from/moved"), p("/to/moved")).unwrap();
    let (dir, file) = (None, Some("inside"));
    let undone = tree_of(&[
        ("from", dir),
        ("from/moved", dir),
        ("from/moved/file", file),
        ("to", dir),
    ]);
    let done = tree_of(&[
        ("from", dir),
        ("to", dir),
        ("to/moved", dir),
        ("to/moved/file", file),
    ]);
    let either = BTreeSet::from([undone, done.clone()]);

    assert_eq!(states(&fs), either);
    fs.sync_dir(p("/to/moved")).unwrap();
    assert_eq!(states(&fs), either);
    fs.sync_dir(p("/to")).unwrap();
    assert_eq!(states(&fs), either);
    fs.sync_dir(p("/from")).unwrap();
    assert_eq!(states(&fs), BTreeSet::from([done]));
    check(&fs);
}

#[test]
fn the_fingerprint_tells_in_which_directory_a_name_is() {
    let trees = [["/a/b", "/a/c"], ["/a/b", "/c"], ["/b", "/c"]];
    let fingerprints = trees.map(|files| {
        let fs = sim(46);
        fs.create_dir(p("/a")).unwrap();
        for (file, content) in files.into_iter().zip([b"b", b"c"]) {
            drop(create(&fs, file, content));
        }
        fs.fingerprint()
    });
    assert_eq!(BTreeSet::from(fingerprints).len(), 3);
}

/// `/a` with the file `b`, the directory `d` that holds the file `g`, and
/// `e`, which is a second name of the file `/c`. The directory comes before
/// a file in the root, and so does `d` in `/a`.
fn two_levels_and_a_link() -> SimVfs {
    let fs = sim(48);
    fs.create_dir(p("/a")).unwrap();
    fs.create_dir(p("/a/d")).unwrap();
    for (file, content) in [("/c", b"y"), ("/a/d/g", b"z"), ("/a/b", b"x")] {
        drop(create(&fs, file, content));
    }
    fs.hard_link(p("/c"), p("/a/e")).unwrap();
    fs
}

#[test]
fn the_fingerprint_is_of_each_directory_whole_where_its_name_stands() {
    let mut expected = Fnv::new();
    let name = |hasher: &mut Fnv, name: &str| {
        hasher.u64(name.len() as u64);
        hasher.bytes(name.as_bytes());
    };
    let content = |hasher: &mut Fnv, content: &[u8]| {
        hasher.u64(4);
        hasher.u64(content.len() as u64);
        hasher.bytes(content);
    };
    let (dir_begins, dir_ends, file_seen) = (1, 2, 3);
    name(&mut expected, "a");
    expected.u64(dir_begins);
    {
        name(&mut expected, "b");
        content(&mut expected, b"x");
        name(&mut expected, "d");
        expected.u64(dir_begins);
        {
            name(&mut expected, "g");
            content(&mut expected, b"z");
        }
        expected.u64(dir_ends);
        name(&mut expected, "e");
        content(&mut expected, b"y");
    }
    expected.u64(dir_ends);
    name(&mut expected, "c");
    expected.u64(file_seen);
    expected.u64(2);

    assert_eq!(two_levels_and_a_link().fingerprint(), expected.0);
}

#[test]
fn a_recovered_file_system_numbers_each_directory_whole_where_its_name_stands() {
    let fs = two_levels_and_a_link();
    let image = fs.crash_image();
    let recovered = recover(&image, &image.keep_all());
    assert_eq!(tree(&recovered), tree(&fs));

    for (len, file) in (2..).zip(["/a/b", "/a/d/g", "/c", "/a/e"]) {
        let mode = OpenMode::Write(Create::Never);
        recovered.open(p(file), mode).unwrap().set_len(len).unwrap();
    }
    let image = recovered.crash_image();
    let numbers: Vec<u64> = image.changes().iter().map(|change| change.file).collect();
    assert_eq!(numbers, [2, 4, 5, 5]);
}

#[test]
fn a_tree_far_deeper_than_a_stack_is_fingerprinted_and_survives_a_crash() {
    on_a_small_stack(|| {
        let fs = sim(47);
        fs.create_dir(p("/tree")).unwrap();
        drop(create_durable(&fs, "/tree/leaf", b"leaf"));
        fs.sync_dir(p("/")).unwrap();
        for _ in 1..DEEP {
            fs.create_dir(p("/above")).unwrap();
            fs.rename(p("/tree"), p("/above/tree")).unwrap();
            fs.rename(p("/above"), p("/tree")).unwrap();
            fs.sync_dir(p("/")).unwrap();
        }
        let image = fs.crash_image();
        assert!(image.names().is_empty() && image.changes().is_empty());
        let deepest = PathBuf::from("/tree".repeat(DEEP));
        let (leaf, below) = (deepest.join("leaf"), deepest.join("below"));
        assert_eq!(read(&fs, &leaf), b"leaf");
        let durable = fs.fingerprint();

        let file = fs.open(&leaf, OpenMode::Write(Create::Never)).unwrap();
        file.write_at(b"LEAF", 0).unwrap();
        fs.create_dir(&below).unwrap();
        assert_ne!(fs.fingerprint(), durable);

        let image = fs.crash_image();
        let lost = recover(&image, &image.lose_all());
        assert_eq!(lost.fingerprint(), durable);
        assert_eq!(read(&lost, &leaf), b"leaf");
        assert_eq!(kind(lost.read_dir(&below)), ErrorKind::NotFound);
        let kept = recover(&image, &image.keep_all());
        assert_eq!(kept.fingerprint(), fs.fingerprint());
        assert_eq!(read(&kept, &leaf), b"LEAF");
        assert!(kept.read_dir(&below).unwrap().is_empty());
        check(&fs);
    });
}

#[test]
fn a_renamed_directory_takes_what_is_in_it_as_durable_as_it_was() {
    let fs = sim(42);
    fs.create_dir(p("/old")).unwrap();
    let kept = create_durable(&fs, "/old/kept", b"synced!!");
    fs.sync_dir(p("/")).unwrap();
    kept.write_at(b"UNSYNCED", 0).unwrap();
    drop(create(&fs, "/old/new", b"new"));
    fs.rename(p("/old"), p("/renamed")).unwrap();

    let image = fs.crash_image();
    let mut left_at = BTreeMap::new();
    for plan in every_plan(&image) {
        let left = tree(&recover(&image, &plan));
        let at = match (left.contains_key("old"), left.contains_key("renamed")) {
            (true, false) => "old",
            (false, true) => "renamed",
            names => panic!("the directory has {names:?} of its two names in {left:?}"),
        };
        let kept = left[&format!("{at}/kept")].clone().unwrap();
        assert!(kept == b"synced!!" || kept == b"UNSYNCED", "{left:?}");
        let new = left.get(&format!("{at}/new")).cloned().flatten();
        assert_eq!(left.len(), 2 + usize::from(new.is_some()), "{left:?}");
        left_at
            .entry(at)
            .or_insert_with(BTreeSet::new)
            .insert((kept, new));
    }
    assert_eq!(left_at["old"], left_at["renamed"]);
    assert_eq!(left_at["old"].len(), 8, "{left_at:?}");

    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert_eq!(image.names().len(), 1);
    assert_eq!(image.names()[0].path, "/old/new");
    assert_eq!(image.changes().len(), 2);
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("renamed", None), ("renamed/kept", Some("synced!!"))])
    );

    kept.sync_data().unwrap();
    let image = fs.crash_image();
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("renamed", None), ("renamed/kept", Some("UNSYNCED"))])
    );
    check(&fs);
}

#[test]
fn no_crash_puts_a_directory_into_itself() {
    let fs = sim(43);
    for (dir, file, content) in [("/a", "/a/in_a", b"A"), ("/b", "/b/in_b", b"B")] {
        fs.create_dir(p(dir)).unwrap();
        drop(create_durable(&fs, file, content));
    }
    fs.sync_dir(p("/")).unwrap();

    fs.rename(p("/a"), p("/b/a")).unwrap();
    fs.rename(p("/b/a"), p("/a")).unwrap();
    fs.rename(p("/b"), p("/a/b")).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names_kept(&names_only(vec![2])), [0, 1, 2]);
    assert_eq!(image.names_kept(&names_only(vec![1])), [0, 1]);
    assert_eq!(image.names_kept(&names_only(vec![0])), [0]);
    let (dir, a, b) = (None, Some("A"), Some("B"));
    assert_eq!(
        states(&fs),
        BTreeSet::from([
            tree_of(&[("a", dir), ("a/in_a", a), ("b", dir), ("b/in_b", b)]),
            tree_of(&[("b", dir), ("b/a", dir), ("b/a/in_a", a), ("b/in_b", b)]),
            tree_of(&[("a", dir), ("a/b", dir), ("a/b/in_b", b), ("a/in_a", a)]),
        ])
    );
    for plan in image.holes().chain(image.prefixes()) {
        assert_eq!(tree(&recover(&image, &plan)).len(), 4);
    }
    check(&fs);
}

#[test]
fn a_rename_that_replaces_an_empty_directory_is_kept_with_what_emptied_it() {
    let fs = sim(44);
    fs.create_dir(p("/target")).unwrap();
    fs.create_dir(p("/next")).unwrap();
    drop(create_durable(&fs, "/target/old", b"old"));
    drop(create_durable(&fs, "/next/new", b"new"));
    fs.sync_dir(p("/")).unwrap();

    assert_eq!(
        kind(fs.rename(p("/next"), p("/target"))),
        ErrorKind::DirectoryNotEmpty
    );
    fs.remove_file(p("/target/old")).unwrap();
    fs.rename(p("/next"), p("/target")).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names_kept(&names_only(vec![1])), [0, 1]);
    let (dir, old, new) = (None, Some("old"), Some("new"));
    assert_eq!(
        states(&fs),
        BTreeSet::from([
            tree_of(&[
                ("next", dir),
                ("next/new", new),
                ("target", dir),
                ("target/old", old),
            ]),
            tree_of(&[("next", dir), ("next/new", new), ("target", dir)]),
            tree_of(&[("target", dir), ("target/new", new)]),
        ])
    );

    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert!(image.names().is_empty());
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("target", dir), ("target/new", new)])
    );
    check(&fs);
}

#[test]
fn a_rename_into_a_directory_is_kept_with_the_rename_that_moved_the_directory() {
    let fs = sim(45);
    fs.create_dir(p("/d")).unwrap();
    drop(create_durable(&fs, "/file", b"F"));
    assert!(fs.crash_image().names().is_empty());

    fs.rename(p("/d"), p("/e")).unwrap();
    fs.rename(p("/file"), p("/e/file")).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names_kept(&names_only(vec![1])), [0, 1]);
    let (dir, file) = (None, Some("F"));
    assert_eq!(
        states(&fs),
        BTreeSet::from([
            tree_of(&[("d", dir), ("file", file)]),
            tree_of(&[("e", dir), ("file", file)]),
            tree_of(&[("e", dir), ("e/file", file)]),
        ])
    );

    fs.sync_dir(p("/e")).unwrap();
    assert_eq!(fs.crash_image().names().len(), 2);
    fs.sync_dir(p("/")).unwrap();
    assert!(fs.crash_image().names().is_empty());
    check(&fs);
}

/// A name that was given and not synced is no more durable for a rename
/// over it: a crash that loses the rename can lose the name with it.
#[test]
fn a_name_that_was_not_durable_can_be_gone_after_a_rename_over_it() {
    let fs = sim(47);
    fs.create_dir(p("/src")).unwrap();
    drop(create_durable(&fs, "/src/file", b"F"));
    fs.sync_dir(p("/")).unwrap();

    fs.create_dir(p("/dst")).unwrap();
    fs.rename(p("/src"), p("/dst")).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names_kept(&names_only(vec![1])), [0, 1]);
    let (dir, file) = (None, Some("F"));
    assert_eq!(
        states(&fs),
        BTreeSet::from([
            tree_of(&[("src", dir), ("src/file", file)]),
            tree_of(&[("dst", dir), ("src", dir), ("src/file", file)]),
            tree_of(&[("dst", dir), ("dst/file", file)]),
        ])
    );
    check(&fs);
}

/// A directory is made under another name, synced with what is in it, and
/// renamed into place. Whenever the power goes and whatever the crash keeps,
/// the place has the whole directory or nothing. Leave one sync out, and
/// some crash shows a directory that is not whole.
#[test]
fn a_directory_built_under_another_name_appears_whole_or_not_at_all() {
    let build = |fs: &SimVfs, careless: bool| -> io::Result<()> {
        fs.create_dir(p("/made.new"))?;
        fs.create_dir(p("/made.new/wal"))?;
        let manifest = fs.open(p("/made.new/MANIFEST"), OpenMode::Write(Create::New))?;
        manifest.write_at(b"manifest", 0)?;
        let log = fs.open(p("/made.new/wal/1.log"), OpenMode::Append(Create::New))?;
        log.append(&[7; 1_000])?;
        let mut synced = vec![
            SyncItem::Data(&*log),
            SyncItem::Data(&*manifest),
            SyncItem::Dir(p("/made.new")),
        ];
        if !careless {
            synced.push(SyncItem::Dir(p("/made.new/wal")));
        }
        fs.sync_together(&synced)?;
        fs.rename(p("/made.new"), p("/made"))?;
        fs.sync_dir(p("/"))
    };
    let whole = Tree::from([
        ("made".to_owned(), None),
        ("made/MANIFEST".to_owned(), Some(b"manifest".to_vec())),
        ("made/wal".to_owned(), None),
        ("made/wal/1.log".to_owned(), Some(vec![7; 1_000])),
    ]);

    for careless in [false, true] {
        let ops = {
            let fs = sim(46);
            build(&fs, careless).unwrap();
            fs.stats().ops
        };
        let (mut whole_ones, mut none, mut broken) = (0, 0, 0);
        for crash_after in 0..=ops {
            let fs = sim(46);
            fs.crash_at(CrashPoint::AfterOp(crash_after));
            let built = build(&fs, careless);
            assert_eq!(built.is_ok(), crash_after == ops);
            let image = fs.crash_image();
            let sampled = (0..if cfg!(miri) { 2 } else { 20 }).map(|draw| image.sample(draw));
            let plans = image.prefixes().chain(image.holes()).chain(sampled);
            for plan in plans {
                let made: Tree = tree(&recover(&image, &plan))
                    .into_iter()
                    .filter(|(path, _)| path == "made" || path.starts_with("made/"))
                    .collect();
                if made.is_empty() {
                    none += 1;
                } else if made == whole {
                    whole_ones += 1;
                } else {
                    broken += 1;
                }
            }
        }
        assert!(
            whole_ones > 0 && none > 0,
            "{whole_ones} whole, {none} none"
        );
        assert_eq!(broken > 0, careless, "{broken} that are not whole");
    }
}
