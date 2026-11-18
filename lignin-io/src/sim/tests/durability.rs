use super::*;

#[test]
fn a_sync_makes_content_durable_and_nothing_else_does() {
    let fs = sim(1);
    let file = create_durable(&fs, "/file", b"synced");
    file.write_at(b"UNSYNCED!", 0).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.changes().len(), 1);
    assert!(image.names().is_empty());
    assert_eq!(
        read(&recover(&image, &image.lose_all()), "/file"),
        b"synced"
    );
    assert_eq!(
        read(&recover(&image, &image.keep_all()), "/file"),
        b"UNSYNCED!"
    );
    assert_eq!(read(&fs, "/file"), b"UNSYNCED!");

    file.sync_all().unwrap();
    let image = fs.crash_image();
    assert!(image.changes().is_empty());
    assert_eq!(
        read(&recover(&image, &image.lose_all()), "/file"),
        b"UNSYNCED!"
    );
    check(&fs);
}

#[test]
fn a_sync_on_any_handle_covers_every_change_made_before_it() {
    let fs = sim(2);
    let writer = create_durable(&fs, "/file", b"");
    let reader = fs.open(p("/file"), OpenMode::Read).unwrap();
    fs.hard_link(p("/file"), p("/link")).unwrap();
    let linked = fs.open(p("/link"), OpenMode::Read).unwrap();

    writer.write_at(b"one", 0).unwrap();
    writer.set_len(2).unwrap();
    reader.sync_data().unwrap();
    writer.write_at(b"two", 2).unwrap();
    linked.sync_all().unwrap();
    writer.write_at(b"later", 5).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.changes().len(), 1);
    assert_eq!(read(&recover(&image, &image.lose_all()), "/file"), b"ontwo");
}

#[test]
fn syncing_a_file_does_not_sync_its_name_nor_the_other_way_round() {
    let fs = sim(3);
    create(&fs, "/content", b"content").sync_all().unwrap();
    let image = fs.crash_image();
    assert_eq!(tree(&recover(&image, &image.lose_all())), tree_of(&[]));
    assert_eq!(
        tree(&recover(&image, &image.keep_all())),
        tree_of(&[("content", Some("content"))])
    );

    let fs = sim(3);
    drop(create(&fs, "/name", b"content"));
    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("name", Some(""))])
    );
}

#[test]
fn a_replaced_file_is_the_old_or_the_new_one_in_every_crash_state() {
    let fs = sim(4);
    drop(create_durable(&fs, "/MANIFEST", b"old"));
    create(&fs, "/MANIFEST.tmp", b"new").sync_data().unwrap();
    fs.rename(p("/MANIFEST.tmp"), p("/MANIFEST")).unwrap();

    let image = fs.crash_image();
    assert_eq!(
        image
            .names()
            .iter()
            .map(|name| name.kind)
            .collect::<Vec<_>>(),
        [NameChangeKind::Create, NameChangeKind::Rename]
    );
    let mut seen = BTreeSet::new();
    for names in name_subsets(&image) {
        let after = recover(&image, &names_only(names));
        let manifest = String::from_utf8(read(&after, "/MANIFEST")).unwrap();
        seen.insert((manifest, exists(&after, "/MANIFEST.tmp")));
    }
    let state = |manifest: &str, temporary| (manifest.to_owned(), temporary);
    assert_eq!(
        seen,
        BTreeSet::from([state("old", false), state("old", true), state("new", false)])
    );

    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert!(image.names().is_empty());
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("MANIFEST", Some("new"))])
    );
    check(&fs);
}

#[test]
fn a_name_can_survive_without_the_content_that_was_not_synced() {
    let fs = sim(5);
    drop(create_durable(&fs, "/MANIFEST", b"old"));
    drop(create(&fs, "/MANIFEST.tmp", b"new"));
    fs.rename(p("/MANIFEST.tmp"), p("/MANIFEST")).unwrap();
    fs.sync_dir(p("/")).unwrap();

    let image = fs.crash_image();
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("MANIFEST", Some(""))])
    );
}

#[test]
fn a_removed_name_can_come_back() {
    let fs = sim(6);
    drop(create_durable(&fs, "/temporary", b"stale"));
    fs.remove_file(p("/temporary")).unwrap();
    assert!(!exists(&fs, "/temporary"));

    let image = fs.crash_image();
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("temporary", Some("stale"))])
    );
    assert_eq!(tree(&recover(&image, &image.keep_all())), tree_of(&[]));

    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert_eq!(tree(&recover(&image, &image.lose_all())), tree_of(&[]));
    check(&fs);
}

#[test]
fn a_rename_between_directories_is_whole_and_needs_both_synced() {
    let fs = sim(7);
    fs.create_dir(p("/from")).unwrap();
    fs.create_dir(p("/to")).unwrap();
    fs.sync_dir(p("/")).unwrap();
    drop(create_durable(&fs, "/from/file", b"moved"));
    fs.rename(p("/from/file"), p("/to/file")).unwrap();

    let undone = tree_of(&[("from", None), ("from/file", Some("moved")), ("to", None)]);
    let done = tree_of(&[("from", None), ("to", None), ("to/file", Some("moved"))]);
    let states = |fs: &SimVfs| -> Vec<Tree> {
        let image = fs.crash_image();
        name_subsets(&image)
            .map(|names| tree(&recover(&image, &names_only(names))))
            .collect()
    };

    assert_eq!(states(&fs), [undone.clone(), done.clone()]);
    fs.sync_dir(p("/to")).unwrap();
    assert_eq!(states(&fs), [undone.clone(), done.clone()]);
    fs.sync_dir(p("/to")).unwrap();
    assert_eq!(states(&fs), [undone, done.clone()]);
    fs.sync_dir(p("/from")).unwrap();
    assert_eq!(states(&fs), [done]);
    check(&fs);
}

#[test]
fn a_change_that_becomes_durable_makes_the_rename_it_builds_on_durable() {
    let fs = sim(8);
    fs.create_dir(p("/a")).unwrap();
    fs.create_dir(p("/b")).unwrap();
    fs.sync_dir(p("/")).unwrap();
    drop(create_durable(&fs, "/a/x", b"x"));
    fs.rename(p("/a/x"), p("/b/y")).unwrap();
    fs.remove_file(p("/b/y")).unwrap();

    let image = fs.crash_image();
    let removal_alone = names_only(vec![1]);
    assert_eq!(image.names_kept(&removal_alone), [0, 1]);
    let gone = tree_of(&[("a", None), ("b", None)]);
    assert_eq!(tree(&recover(&image, &removal_alone)), gone);

    fs.sync_dir(p("/b")).unwrap();
    check(&fs);
    let image = fs.crash_image();
    assert!(image.names().is_empty());
    assert_eq!(tree(&recover(&image, &image.lose_all())), gone);
}

/// The file `F` is durable at `/d1/a` and is renamed to `/d2/b`. Whatever
/// is done to either name afterwards, and whatever a crash keeps of it, the
/// file is at the one name or the other, unless it was removed there.
#[test]
fn a_rename_is_never_left_half_done_by_a_later_change_of_its_names() {
    let renamed = || {
        let fs = sim(9);
        fs.create_dir(p("/d1")).unwrap();
        fs.create_dir(p("/d2")).unwrap();
        fs.sync_dir(p("/")).unwrap();
        drop(create_durable(&fs, "/d1/a", b"F"));
        drop(create_durable(&fs, "/d1/c", b"C"));
        fs.rename(p("/d1/a"), p("/d2/b")).unwrap();
        fs
    };
    let names_of_the_file = |fs: &SimVfs| -> BTreeSet<Vec<String>> {
        let image = fs.crash_image();
        name_subsets(&image)
            .map(|names| {
                let after = tree(&recover(&image, &names_only(names)));
                let is_the_file =
                    |(_, content): &(&String, &Option<Vec<u8>>)| content.as_deref() == Some(b"F");
                after
                    .iter()
                    .filter(is_the_file)
                    .map(|(path, _)| path.clone())
                    .collect()
            })
            .collect()
    };
    let either = BTreeSet::from([vec!["d1/a".to_owned()], vec!["d2/b".to_owned()]]);
    let moved = BTreeSet::from([vec!["d2/b".to_owned()]]);

    let fs = renamed();
    assert_eq!(names_of_the_file(&fs), either);

    let fs = renamed();
    drop(create(&fs, "/d1/a", b"new"));
    assert_eq!(names_of_the_file(&fs), either);
    let image = fs.crash_image();
    assert_eq!(image.names_kept(&names_only(vec![1])), [0, 1]);
    fs.sync_dir(p("/d1")).unwrap();
    check(&fs);
    assert_eq!(names_of_the_file(&fs), moved);
    assert!(fs.crash_image().names().is_empty());

    let fs = renamed();
    fs.hard_link(p("/d1/c"), p("/d1/a")).unwrap();
    assert_eq!(names_of_the_file(&fs), either);

    let fs = renamed();
    create(&fs, "/other", b"other").sync_data().unwrap();
    fs.rename(p("/other"), p("/d2/b")).unwrap();
    let image = fs.crash_image();
    let replaced = recover(&image, &image.keep_all());
    assert_eq!(read(&replaced, "/d2/b"), b"other");
    let without_the_replacement =
        BTreeSet::from([vec!["d1/a".to_owned()], vec!["d2/b".to_owned()], Vec::new()]);
    assert_eq!(names_of_the_file(&fs), without_the_replacement);
    for names in name_subsets(&image) {
        let after = tree(&recover(&image, &names_only(names.clone())));
        let replaced = after.get("d2/b") == Some(&Some(b"other".to_vec()));
        assert_eq!(
            after.values().any(|file| file.as_deref() == Some(b"F")),
            !replaced
        );
    }
}

#[test]
fn a_file_renamed_into_a_new_directory_is_not_lost_with_it() {
    let fs = sim(10);
    drop(create_durable(&fs, "/file", b"F"));
    fs.create_dir(p("/new")).unwrap();
    fs.create_dir(p("/new/inner")).unwrap();
    fs.rename(p("/file"), p("/new/inner/file")).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names_kept(&names_only(vec![2])), [0, 1, 2]);
    let states: BTreeSet<Tree> = name_subsets(&image)
        .map(|names| tree(&recover(&image, &names_only(names))))
        .collect();
    let (dir, file) = (None, Some("F"));
    assert_eq!(
        states,
        BTreeSet::from([
            tree_of(&[("file", file)]),
            tree_of(&[("file", file), ("new", dir)]),
            tree_of(&[("file", file), ("new", dir), ("new/inner", dir)]),
            tree_of(&[("new", dir), ("new/inner", dir), ("new/inner/file", file)]),
        ])
    );

    fs.sync_dir(p("/new/inner")).unwrap();
    assert_eq!(fs.crash_image().names().len(), 3);
    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert!(image.names().is_empty());
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("new", dir), ("new/inner", dir), ("new/inner/file", file)])
    );
    check(&fs);
}

#[test]
fn a_directory_is_lost_or_comes_back_with_everything_in_it() {
    let fs = sim(9);
    fs.create_dir(p("/dir")).unwrap();
    drop(create_durable(&fs, "/dir/file", b"inside"));
    let full = tree_of(&[("dir", None), ("dir/file", Some("inside"))]);

    let image = fs.crash_image();
    assert_eq!(image.names().len(), 1);
    assert_eq!(tree(&recover(&image, &image.lose_all())), tree_of(&[]));
    assert_eq!(tree(&recover(&image, &image.keep_all())), full);

    fs.sync_dir(p("/")).unwrap();
    fs.remove_file(p("/dir/file")).unwrap();
    fs.remove_dir(p("/dir")).unwrap();
    assert_eq!(tree(&fs), tree_of(&[]));
    let image = fs.crash_image();
    let kinds: Vec<_> = image.names().iter().map(|name| name.kind).collect();
    assert_eq!(kinds, [NameChangeKind::Remove, NameChangeKind::RemoveDir]);
    assert_eq!(tree(&recover(&image, &image.lose_all())), full);
    assert_eq!(
        tree(&recover(&image, &names_only(vec![0]))),
        tree_of(&[("dir", None)])
    );
    assert_eq!(tree(&recover(&image, &names_only(vec![1]))), tree_of(&[]));
    assert_eq!(tree(&recover(&image, &image.keep_all())), tree_of(&[]));

    fs.sync_dir(p("/")).unwrap();
    check(&fs);
    let image = fs.crash_image();
    assert!(image.names().is_empty());
    assert_eq!(tree(&recover(&image, &image.lose_all())), tree_of(&[]));
}

#[test]
fn a_directory_made_and_removed_between_syncs_leaves_nothing() {
    let fs = sim(10);
    fs.create_dir(p("/dir")).unwrap();
    drop(create(&fs, "/dir/file", b"inside"));
    fs.remove_file(p("/dir/file")).unwrap();
    fs.remove_dir(p("/dir")).unwrap();
    fs.create_dir(p("/dir")).unwrap();
    drop(create(&fs, "/dir/other", b"other"));
    check(&fs);

    let image = fs.crash_image();
    assert_eq!(image.names().len(), 6);
    let first_dir_with_its_file = names_only(vec![0, 1]);
    assert_eq!(
        tree(&recover(&image, &first_dir_with_its_file)),
        tree_of(&[("dir", None), ("dir/file", Some(""))])
    );
    let second_dir_with_the_first_file = names_only(vec![1, 4]);
    assert_eq!(
        tree(&recover(&image, &second_dir_with_the_first_file)),
        tree_of(&[("dir", None)])
    );

    fs.sync_dir(p("/")).unwrap();
    check(&fs);
    let image = fs.crash_image();
    assert_eq!(image.names().len(), 1);
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("dir", None)])
    );
}

#[test]
fn removing_a_directory_makes_the_changes_that_emptied_it_durable() {
    let fs = sim(11);
    fs.create_dir(p("/a")).unwrap();
    fs.create_dir(p("/b")).unwrap();
    fs.sync_dir(p("/")).unwrap();
    drop(create_durable(&fs, "/a/x", b"x"));
    drop(create_durable(&fs, "/a/gone", b"gone"));
    fs.rename(p("/a/x"), p("/b/y")).unwrap();
    fs.remove_file(p("/a/gone")).unwrap();
    fs.remove_dir(p("/a")).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names().len(), 3);
    assert_eq!(image.names_kept(&names_only(vec![2])), [0, 1, 2]);
    let moved = tree_of(&[("b", None), ("b/y", Some("x"))]);
    assert_eq!(tree(&recover(&image, &names_only(vec![2]))), moved);
    assert_eq!(
        tree(&recover(&image, &names_only(vec![1]))),
        tree_of(&[("a", None), ("a/x", Some("x")), ("b", None)])
    );

    fs.sync_dir(p("/")).unwrap();
    check(&fs);
    let image = fs.crash_image();
    assert!(image.names().is_empty());
    assert_eq!(tree(&recover(&image, &image.lose_all())), moved);
}

#[test]
fn once_everything_is_synced_nothing_is_unsynced() {
    for seed in 0..if cfg!(miri) { 3 } else { 200 } {
        let mut rng = SimRng::new(seed);
        let fs = sim(seed);
        let dirs = ["/d", "/e", "/d/e"];
        let files = ["/a", "/b", "/d/a", "/d/b", "/e/a", "/d/e/a"];
        for step in 0..40 {
            let (file, other) = (
                p(files[rng.below(6) as usize]),
                p(files[rng.below(6) as usize]),
            );
            let dir = p(dirs[rng.below(3) as usize]);
            let _ = match rng.below(9) {
                0 | 1 => fs
                    .open(file, OpenMode::Append(Create::IfMissing))
                    .and_then(|file| file.append(&[step])),
                2 | 3 => fs.rename(file, other).map(|()| 0),
                4 => fs.hard_link(file, other).map(|()| 0),
                5 => fs.remove_file(file).map(|()| 0),
                6 | 7 => fs.create_dir(dir).map(|()| 0),
                _ => fs.remove_dir(dir).map(|()| 0),
            };
            check(&fs);
        }

        for path in tree(&fs).keys() {
            let path = Path::new("/").join(path);
            if let Ok(file) = fs.open(&path, OpenMode::Read) {
                file.sync_all().unwrap();
            } else {
                fs.sync_dir(&path).unwrap();
            }
        }
        fs.sync_dir(p("/")).unwrap();
        check(&fs);

        let image = fs.crash_image();
        assert!(image.names().is_empty(), "seed {seed}: {:?}", image.names());
        assert!(image.changes().is_empty(), "seed {seed}");
        let synced = recover(&image, &image.lose_all());
        assert_eq!(synced.fingerprint(), fs.fingerprint(), "seed {seed}");
    }
}

#[test]
fn names_of_one_file_share_its_content_across_a_crash() {
    let fs = sim(12);
    let file = create_durable(&fs, "/original", b"shared");
    fs.hard_link(p("/original"), p("/link")).unwrap();
    file.write_at(b"SHARED", 0).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.names().len(), 1);
    assert_eq!(image.names()[0].kind, NameChangeKind::Link);
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("original", Some("shared"))])
    );
    assert_eq!(
        tree(&recover(&image, &names_only(vec![0]))),
        tree_of(&[("link", Some("shared")), ("original", Some("shared"))])
    );

    let after = recover(&image, &image.keep_all());
    assert_eq!(read(&after, "/link"), b"SHARED");
    let through_link = after
        .open(p("/link"), OpenMode::Write(Create::Never))
        .unwrap();
    through_link.write_at(b"one file", 0).unwrap();
    assert_eq!(read(&after, "/original"), b"one file");
    assert_eq!(read(&fs, "/original"), b"SHARED");
}

#[test]
fn a_file_with_no_name_left_does_not_survive() {
    let fs = sim(13);
    let file = create_durable(&fs, "/file", b"content");
    fs.remove_file(p("/file")).unwrap();
    fs.sync_dir(p("/")).unwrap();
    file.write_at(b"written after", 0).unwrap();
    check(&fs);

    let image = fs.crash_image();
    assert!(image.changes().is_empty());
    assert_eq!(tree(&recover(&image, &image.keep_all())), tree_of(&[]));
    assert_eq!(read_handle(&*file), b"written after");
}

#[test]
fn a_process_that_exits_loses_nothing() {
    let fs = sim(14);
    let lock = fs.lock(p("/LOCK")).unwrap();
    let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
    log.append(b"never synced").unwrap();
    drop((lock, log));
    check(&fs);

    let lock = fs.lock(p("/LOCK")).unwrap();
    let log = fs.open(p("/log"), OpenMode::Append(Create::Never)).unwrap();
    assert_eq!(log.append(b"!").unwrap(), 12);
    assert_eq!(read(&fs, "/log"), b"never synced!");
    drop(lock);
}

#[test]
fn a_write_tears_only_between_sectors_and_damages_nothing_next_to_it() {
    let fs = sim_with_sectors(15, 4);
    let file = create_durable(&fs, "/file", &[b'a'; 16]);
    file.write_at(&[b'b'; 10], 3).unwrap();

    let image = fs.crash_image();
    assert_eq!(image.sector_size(), 4);
    assert_eq!(image.changes()[0].sectors, 4);
    let pieces = [3..4, 4..8, 8..12, 12..13];
    let whole_pieces = |content: &[u8]| {
        assert_eq!(content.len(), 16);
        assert!(content[..3]
            .iter()
            .chain(&content[13..])
            .all(|byte| *byte == b'a'));
        for piece in pieces.clone() {
            let piece = &content[piece];
            assert!(piece.iter().all(|byte| *byte == piece[0]));
        }
    };

    let mut seen = BTreeSet::new();
    for plan in every_plan(&image) {
        let content = read(&recover(&image, &plan), "/file");
        whole_pieces(&content);
        seen.insert(content);
    }
    assert_eq!(seen.len(), 16);
    for draw in 0..200 {
        whole_pieces(&read(&fs.crash(draw), "/file"));
    }
}

#[test]
fn a_torn_write_keeps_its_length_apart_from_its_bytes() {
    let fs = sim_with_sectors(16, 8);
    let file = create_durable(&fs, "/file", b"0123456789");
    file.write_at(b"ABCDEFGH", 6).unwrap();

    let image = fs.crash_image();
    let tears = image.tears(0);
    assert_eq!(
        tears,
        [
            tear(&[], 14),
            tear(&[0..1], 8),
            tear(&[0..1], 14),
            tear(&[0..2], 0)
        ]
    );
    let contents: Vec<Vec<u8>> = tears
        .into_iter()
        .map(|tear| read(&recover(&image, &torn(0, tear)), "/file"))
        .collect();
    assert_eq!(
        contents,
        [
            b"0123456789\0\0\0\0".to_vec(),
            b"012345AB89".to_vec(),
            b"012345AB89\0\0\0\0".to_vec(),
            b"012345ABCD".to_vec(),
        ]
    );
}

#[test]
fn an_append_lands_at_the_offset_it_returned() {
    let fs = sim(17);
    let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
    fs.sync_dir(p("/")).unwrap();
    assert_eq!(log.append(b"first").unwrap(), 0);
    assert_eq!(log.append(b"second").unwrap(), 5);

    let image = fs.crash_image();
    assert_eq!(
        read(&recover(&image, &changes_only(&[1])), "/log"),
        b"\0\0\0\0\0second"
    );
    assert_eq!(
        read(&recover(&image, &changes_only(&[0])), "/log"),
        b"first"
    );
    assert_eq!(
        read(&recover(&image, &changes_only(&[1, 0])), "/log"),
        b"firstsecond"
    );
}

#[test]
fn changes_of_length_survive_one_by_one_in_the_order_of_the_plan() {
    let fs = sim(18);
    let file = create_durable(&fs, "/file", b"0123456789");
    file.set_len(4).unwrap();
    file.set_len(8).unwrap();
    assert_eq!(read(&fs, "/file"), b"0123\0\0\0\0");

    let image = fs.crash_image();
    let lens: Vec<_> = image.changes().iter().map(|change| change.kind).collect();
    assert_eq!(
        lens,
        [
            FileChangeKind::SetLen { len: 4 },
            FileChangeKind::SetLen { len: 8 }
        ]
    );
    assert!(image.tears(0).is_empty());
    let content = |changes: &[usize]| read(&recover(&image, &changes_only(changes)), "/file");
    assert_eq!(content(&[]), b"0123456789");
    assert_eq!(content(&[0]), b"0123");
    assert_eq!(content(&[1]), b"01234567");
    assert_eq!(content(&[0, 1]), b"0123\0\0\0\0");
    assert_eq!(content(&[1, 0]), b"0123");
}

#[test]
fn a_reordered_write_lets_the_older_bytes_win() {
    let fs = sim(19);
    let file = create_durable(&fs, "/file", b"----------");
    file.write_at(b"aaaa", 0).unwrap();
    file.write_at(b"bbbb", 2).unwrap();
    file.write_at(b"cc", 8).unwrap();

    let image = fs.crash_image();
    let reordered: Vec<Vec<u8>> = image
        .reorders()
        .map(|plan| read(&recover(&image, &plan), "/file"))
        .collect();
    assert_eq!(reordered, [b"aaaabb--cc".to_vec()]);
    assert_eq!(
        read(&recover(&image, &image.keep_all()), "/file"),
        b"aabbbb--cc"
    );
}

#[test]
fn a_failed_sync_cannot_be_made_good() {
    let (mut kept, mut lost) = (0, 0);
    for seed in 0..40 {
        let fs = sim(seed);
        let file = create_durable(&fs, "/file", b"durable");
        let other = fs.open(p("/file"), OpenMode::Write(Create::Never)).unwrap();
        file.write_at(b"UNKNOWN", 0).unwrap();
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
        assert_eq!(
            SimError::injected(&file.sync_data().unwrap_err()),
            Some(Fault::Eio)
        );

        let image = fs.crash_image();
        assert!(image.changes()[0].unknown);
        assert_eq!(
            read(&recover(&image, &image.lose_all()), "/file"),
            b"durable"
        );
        assert_eq!(
            read(&recover(&image, &image.keep_all()), "/file"),
            b"UNKNOWN"
        );

        for sync in [VfsFile::sync_data, VfsFile::sync_all] {
            let error = sync(&*file).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Other);
            assert!(SimError::of(&error).is_none());
        }
        assert_eq!(fs.crash_image().changes().len(), 1);

        other.write_at(b"!", 7).unwrap();
        other.sync_data().unwrap();
        check(&fs);
        assert_eq!(read(&fs, "/file"), b"UNKNOWN!");

        let image = fs.crash_image();
        assert!(image.changes().is_empty());
        let durable = read(&recover(&image, &image.lose_all()), "/file");
        let settled = fs.trace().into_iter().find_map(|event| match event {
            Event::Settled { changes, path, .. } => {
                assert_eq!(path, "/file");
                Some(changes)
            }
            _ => None,
        });
        let settled = settled.unwrap();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].kind, FileChangeKind::Write { offset: 0, len: 7 });
        match &settled[0].fate {
            Fate::Lost => {
                assert_eq!(durable, b"durable!");
                lost += 1;
            }
            Fate::Survives { tear: None, .. } => {
                assert_eq!(durable, b"UNKNOWN!");
                kept += 1;
            }
            Fate::Survives {
                tear: Some(tear), ..
            } => {
                let content = match tear.sectors.is_empty() {
                    true => b"durable!",
                    false => b"UNKNOWN!",
                };
                assert_eq!(durable, content);
            }
        }
    }
    assert!(kept > 0 && lost > 0, "{kept} kept, {lost} lost");
}

#[test]
fn a_change_made_after_a_failed_sync_is_covered_by_the_next_one() {
    let fs = sim(20);
    let file = create_durable(&fs, "/file", b"");
    let other = fs.open(p("/file"), OpenMode::Write(Create::Never)).unwrap();
    fs.add_rule(FaultRule::new(Fault::Enospc).on(OpKind::SyncAll));
    assert_eq!(kind(file.sync_all()), ErrorKind::StorageFull);
    assert!(fs.trace().iter().any(|event| matches!(
        event,
        Event::Op(OpRecord {
            outcome: Outcome::Injected {
                effect: Effect::SyncFailed(0),
                ..
            },
            ..
        })
    )));

    file.write_at(b"after the failure", 0).unwrap();
    other.sync_data().unwrap();
    assert!(!fs
        .trace()
        .iter()
        .any(|event| matches!(event, Event::Settled { .. })));
    let image = fs.crash_image();
    assert_eq!(
        read(&recover(&image, &image.lose_all()), "/file"),
        b"after the failure"
    );
}

#[test]
fn what_became_of_unknown_changes_can_be_set_without_changing_the_schedule() {
    let run = |unknown: UnknownFate| {
        let fs = SimVfs::new(SimConfig {
            unknown,
            trace_ops: true,
            ..SimConfig::new(21)
        });
        let file = create_durable(&fs, "/file", b"durable");
        let other = fs.open(p("/file"), OpenMode::Write(Create::Never)).unwrap();
        file.write_at(b"UNKNOWN", 0).unwrap();
        file.set_len(9).unwrap();
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
        file.sync_data().unwrap_err();
        other.write_at(b"!", 9).unwrap();
        other.sync_data().unwrap();

        let image = fs.crash_image();
        let durable = read(&recover(&image, &image.lose_all()), "/file");
        let operations: Vec<Event> = fs
            .trace()
            .into_iter()
            .filter(|event| matches!(event, Event::Op(_)))
            .collect();
        (durable, digest(&operations))
    };

    let (lost, schedule) = run(UnknownFate::Lost);
    assert_eq!(lost, b"durable\0\0!");
    let (kept, same_schedule) = run(UnknownFate::Kept);
    assert_eq!(kept, b"UNKNOWN\0\0!");
    assert_eq!(schedule, same_schedule);

    let drawn: BTreeSet<Vec<u8>> = (0..40)
        .map(|draw| {
            let (durable, same_schedule) = run(UnknownFate::Drawn(draw));
            assert_eq!(schedule, same_schedule);
            assert_eq!(durable.len(), 10);
            durable
        })
        .collect();
    assert!(drawn.contains(&lost) && drawn.contains(&kept));
    assert_eq!(run(UnknownFate::Drawn(3)), run(UnknownFate::Drawn(3)));
    assert_eq!(SimConfig::new(0).unknown, UnknownFate::Drawn(0));
}
