use std::sync::Arc;

use super::*;

fn power_loss<T: Debug>(result: io::Result<T>) {
    let error = result.unwrap_err();
    assert!(SimError::is_power_loss(&error), "{error}");
    assert_eq!(error.kind(), ErrorKind::Other);
}

#[test]
fn the_power_goes_off_after_the_chosen_operation() {
    let fs = sim(1);
    let file = create(&fs, "/file", b"12");
    assert_eq!(fs.stats().ops, 2);
    fs.crash_at(CrashPoint::AfterOp(4));
    assert!(!fs.is_powered_off());

    file.write_at(b"3", 2).unwrap();
    assert!(!fs.is_powered_off());
    file.write_at(b"4", 3).unwrap();
    assert!(fs.is_powered_off());

    power_loss(file.write_at(b"5", 4));
    power_loss(file.read_at(&mut [0; 4], 0));
    power_loss(file.len());
    power_loss(file.set_len(0));
    power_loss(file.sync_data());
    power_loss(file.sync_all());
    power_loss(file.append(b"x"));
    power_loss(fs.open(p("/file"), OpenMode::Read));
    power_loss(fs.rename(p("/file"), p("/other")));
    power_loss(fs.hard_link(p("/file"), p("/other")));
    power_loss(fs.remove_file(p("/file")));
    power_loss(fs.create_dir(p("/dir")));
    power_loss(fs.remove_dir(p("/dir")));
    power_loss(fs.read_dir(p("/")));
    power_loss(fs.sync_dir(p("/")));
    power_loss(fs.lock(p("/LOCK")));

    let stats = fs.stats();
    assert_eq!((stats.ops, stats.changes, stats.faults), (4, 4, 0));
    assert_eq!(fs.trace(), [Event::PowerOff { after_op: 4 }]);
    drop(file);
    check(&fs);

    let image = fs.crash_image();
    assert_eq!(image.ops(), 4);
    assert_eq!(read(&recover(&image, &image.keep_all()), "/file"), b"1234");
}

#[test]
fn the_power_goes_off_after_the_chosen_sync() {
    let fs = sim(2);
    let file = create(&fs, "/file", b"content");
    fs.crash_at(CrashPoint::AfterSync(3));
    file.sync_data().unwrap();
    file.write_at(b"CONTENT", 0).unwrap();
    assert_eq!(kind(fs.sync_dir(p("/missing"))), ErrorKind::NotFound);
    assert_eq!(fs.stats().syncs, 2);
    file.write_at(b"more", 7).unwrap();
    file.sync_all().unwrap();
    power_loss(fs.sync_dir(p("/")));

    assert_eq!(fs.stats().syncs, 3);
    let image = fs.crash_image();
    assert!(image.changes().is_empty());
    assert_eq!(image.names().len(), 1);
    assert_eq!(
        tree(&recover(&image, &image.keep_all())),
        tree_of(&[("file", Some("CONTENTmore"))])
    );
}

#[test]
fn the_power_goes_off_after_the_chosen_change() {
    let fs = sim(3);
    let file = create(&fs, "/file", b"content");
    assert_eq!(fs.stats().changes, 2);
    fs.crash_at(CrashPoint::AfterChange(5));

    file.len().unwrap();
    file.write_at(&[], 0).unwrap();
    file.set_len(7).unwrap();
    fs.rename(p("/file"), p("/file")).unwrap();
    assert_eq!(kind(fs.remove_file(p("/missing"))), ErrorKind::NotFound);
    drop(fs.open(p("/file"), OpenMode::Read).unwrap());
    assert_eq!(fs.stats().changes, 2);

    file.sync_data().unwrap();
    file.sync_data().unwrap();
    assert_eq!(fs.stats().changes, 3);
    fs.sync_dir(p("/")).unwrap();
    fs.sync_dir(p("/")).unwrap();
    assert_eq!(fs.stats().changes, 4);
    drop(fs.lock(p("/file")).unwrap());
    assert_eq!(fs.stats().changes, 4);
    drop(fs.lock(p("/LOCK")).unwrap());
    assert_eq!(fs.stats().changes, 5);
    power_loss(file.len());
}

#[test]
fn a_crash_point_already_passed_takes_the_power_at_once() {
    let fs = sim(4);
    drop(create(&fs, "/file", b""));
    fs.crash_at(CrashPoint::AfterOp(100));
    fs.crash_at(CrashPoint::AfterOp(1));
    assert!(fs.is_powered_off());
    assert_eq!(fs.trace(), [Event::PowerOff { after_op: 2 }]);

    fs.crash_at(CrashPoint::AfterSync(0));
    fs.power_off();
    assert_eq!(fs.trace().len(), 1);

    let fs = sim(4);
    fs.crash_at(CrashPoint::AfterChange(0));
    power_loss(fs.read_dir(p("/")));
    assert_eq!(fs.stats(), SimStats::default());

    let fs = sim(4);
    fs.clone().power_off();
    assert!(fs.is_powered_off());
    assert_eq!(fs.trace(), [Event::PowerOff { after_op: 0 }]);
}

#[test]
fn a_later_crash_point_replaces_an_earlier_one() {
    let fs = sim(5);
    fs.crash_at(CrashPoint::AfterOp(1));
    fs.crash_at(CrashPoint::AfterOp(3));
    fs.read_dir(p("/")).unwrap();
    fs.read_dir(p("/")).unwrap();
    fs.read_dir(p("/")).unwrap();
    power_loss(fs.read_dir(p("/")));
}

fn three_changes(seed: u64) -> (SimVfs, Box<dyn VfsFile>) {
    let fs = sim_with_sectors(seed, 4);
    let file = create_durable(&fs, "/file", b"........");
    file.write_at(b"aaaaaa", 0).unwrap();
    drop(create(&fs, "/other", b""));
    file.set_len(10).unwrap();
    (fs, file)
}

#[test]
fn an_image_lists_the_unsynced_changes_in_the_order_they_were_made() {
    let (fs, _file) = three_changes(6);
    let image = fs.crash_image();
    assert_eq!((image.seed(), image.ops(), image.sector_size()), (6, 8, 4));

    let [write, set_len] = image.changes() else {
        panic!("{:?}", image.changes());
    };
    assert_eq!(
        (write.op, write.path.as_str(), write.sectors),
        (5, "/file", 2)
    );
    assert_eq!(write.kind, FileChangeKind::Write { offset: 0, len: 6 });
    assert_eq!((set_len.op, set_len.sectors), (8, 0));
    assert_eq!(set_len.kind, FileChangeKind::SetLen { len: 10 });
    assert_eq!(write.file, set_len.file);
    assert!(!write.unknown && !set_len.unknown);
    assert_eq!(format!("{write:?}"), "#5 write offset 0 len 6 /file");

    let [created] = image.names() else {
        panic!("{:?}", image.names());
    };
    assert_eq!((created.op, created.kind), (6, NameChangeKind::Create));
    assert_eq!((created.path.as_str(), &created.to), ("/other", &None));

    let shown = format!("{image:?}");
    assert!(shown.starts_with("CrashImage { seed: 6, ops: 8, names: ["));
    assert!(shown.contains("#8 set_len 10 /file"));
}

#[test]
fn the_plans_of_an_image_cover_the_ways_a_crash_can_go() {
    let (fs, _file) = three_changes(7);
    let image = fs.crash_image();
    let whole = |changes: &[usize]| changes.iter().copied().map(Survivor::whole).collect();
    let plan = |names: &[usize], changes: &[usize]| CrashPlan {
        names: names.to_vec(),
        changes: whole(changes),
    };

    assert_eq!(image.lose_all(), plan(&[], &[]));
    assert_eq!(image.keep_all(), plan(&[0], &[0, 1]));
    assert_eq!(image.keep_through(4), plan(&[], &[]));
    assert_eq!(image.keep_through(5), plan(&[], &[0]));
    assert_eq!(image.keep_through(6), plan(&[0], &[0]));
    assert_eq!(image.keep_through(u64::MAX), image.keep_all());

    let holes: Vec<CrashPlan> = image.holes().collect();
    assert_eq!(
        holes,
        [plan(&[0], &[1]), plan(&[], &[0, 1]), plan(&[0], &[0])]
    );

    let prefixes: Vec<CrashPlan> = image.prefixes().collect();
    let tears = image.tears(0);
    assert_eq!(tears.len(), 4);
    assert_eq!(prefixes.len(), 4 + tears.len());
    assert_eq!(prefixes[0], image.lose_all());
    for (torn, tear) in prefixes[1..5].iter().zip(&tears) {
        let survivor = Survivor {
            change: 0,
            tear: Some(tear.clone()),
        };
        assert_eq!(torn.names, []);
        assert_eq!(torn.changes, [survivor]);
    }
    assert_eq!(
        prefixes[5..],
        [plan(&[], &[0]), plan(&[0], &[0]), plan(&[0], &[0, 1])]
    );

    let reorders: Vec<CrashPlan> = image.reorders().collect();
    assert_eq!(reorders, [plan(&[0], &[1, 0])]);
    let states: BTreeSet<Tree> = prefixes
        .iter()
        .chain(&holes)
        .chain(&reorders)
        .map(|plan| tree(&recover(&image, plan)))
        .collect();
    assert_eq!(states.len(), 7);
}

#[test]
fn a_sampled_plan_depends_on_the_seed_the_image_and_the_draw() {
    let (fs, file) = three_changes(8);
    let image = fs.crash_image();
    assert_eq!(image.sample(3), image.sample(3));
    assert_eq!(image.sample(3), fs.crash_image().sample(3));
    assert_eq!(fs.crash(3).fingerprint(), fs.crash(3).fingerprint());
    let plans: BTreeSet<String> = (0..64)
        .map(|draw| format!("{:?}", image.sample(draw)))
        .collect();
    assert!(plans.len() > 8, "{}", plans.len());
    let states: BTreeSet<u64> = (0..64).map(|draw| fs.crash(draw).fingerprint()).collect();
    assert!(states.len() > 8, "{}", states.len());

    let other_seed = three_changes(9).0.crash_image();
    assert!((0..64).any(|draw| other_seed.sample(draw) != image.sample(draw)));
    file.len().unwrap();
    let later = fs.crash_image();
    assert!((0..64).any(|draw| later.sample(draw) != image.sample(draw)));

    let odds = |lose, tear, reorder| CrashOdds {
        lose,
        tear,
        reorder,
    };
    let (never, always) = (Chance::NEVER, Chance::ALWAYS);
    for draw in 0..32 {
        assert_eq!(
            image.sample_with(draw, &odds(never, never, never)),
            image.keep_all()
        );
        assert_eq!(
            image.sample_with(draw, &odds(always, never, always)),
            image.lose_all()
        );
        let torn = image.sample_with(draw, &odds(never, always, never));
        assert!(torn.changes[0].tear.is_some() && torn.changes[1].tear.is_none());
        recover(&image, &torn);
    }
    let reordered = (0..32)
        .map(|draw| image.sample_with(draw, &odds(never, never, always)))
        .filter(|plan| plan.changes[0].change == 1)
        .count();
    assert!((4..28).contains(&reordered), "{reordered}");
    assert_eq!(SimConfig::new(0).crash, CrashOdds::default());
}

#[test]
fn sampled_tears_come_in_every_shape() {
    let fs = sim_with_sectors(10, 2);
    let file = create_durable(&fs, "/file", b"..");
    file.write_at(b"abcdefgh", 1).unwrap();
    let image = fs.crash_image();
    let odds = CrashOdds {
        lose: Chance::NEVER,
        tear: Chance::ALWAYS,
        reorder: Chance::NEVER,
    };

    let mut shapes = BTreeSet::new();
    for draw in 0..400 {
        let plan = image.sample_with(draw, &odds);
        let tear = plan.changes[0].tear.clone().unwrap();
        recover(&image, &plan);
        let sectors: u64 = tear
            .sectors
            .iter()
            .map(|range| range.end - range.start)
            .sum();
        shapes.insert((
            tear.sectors.len().min(2),
            sectors == 5,
            tear.len == 9,
            tear.len == 0,
        ));
    }
    assert!(shapes.contains(&(0, false, false, true)));
    assert!(shapes.contains(&(0, false, true, false)));
    assert!(shapes.contains(&(1, false, false, false)));
    assert!(shapes.contains(&(1, false, true, false)));
    assert!(shapes.contains(&(1, true, false, true)));
    assert!(shapes.contains(&(2, false, true, false)));
    assert!(shapes.contains(&(2, false, false, false)));
}

#[test]
fn a_plan_that_does_not_fit_its_image_is_refused() {
    let (fs, _file) = three_changes(11);
    let image = fs.crash_image();
    let refused = |plan: CrashPlan| image.recover(&plan).unwrap_err().to_string();
    let torn = |change, sectors: &[Range<u64>], len| CrashPlan {
        names: Vec::new(),
        changes: vec![Survivor {
            change,
            tear: Some(Tear {
                sectors: sectors.to_vec(),
                len,
            }),
        }],
    };
    let names = |names: &[usize]| CrashPlan {
        names: names.to_vec(),
        changes: Vec::new(),
    };
    let prefix = "the crash plan does not fit the image: ";

    assert_eq!(
        refused(names(&[1])),
        format!("{prefix}there is no change of names at place 1")
    );
    assert_eq!(
        refused(names(&[0, 0])),
        format!("{prefix}the change of names at place 0 is kept twice")
    );
    let twice = CrashPlan {
        names: Vec::new(),
        changes: vec![Survivor::whole(1), Survivor::whole(1)],
    };
    assert_eq!(
        refused(twice),
        format!("{prefix}the change of a file at place 1 is kept twice")
    );
    assert_eq!(
        refused(torn(2, &[], 0)),
        format!("{prefix}there is no change of a file at place 2")
    );
    assert_eq!(
        refused(torn(1, &[], 0)),
        format!("{prefix}the change at place 1 is not a write")
    );
    for sectors in [&[0..3][..], &[1..1], &[1..2, 0..1], &[0..2, 1..2]] {
        let reason = refused(torn(0, sectors, 0));
        assert!(
            reason.contains("which are not rising ranges within its 2 sectors"),
            "{reason}"
        );
    }
    for len in [7, 5, 3] {
        let image = match len {
            3 => {
                let fs = sim_with_sectors(11, 4);
                create_durable(&fs, "/file", b"")
                    .write_at(b"abc", 3)
                    .unwrap();
                fs.crash_image()
            }
            _ => image.clone(),
        };
        let reason = image.recover(&torn(0, &[], len)).unwrap_err().to_string();
        assert!(
            reason.contains(&format!("has length {len}, which is neither 0 nor")),
            "{reason}"
        );
    }

    recover(&image, &torn(0, &[0..1, 1..2], 4));
    recover(&image, &torn(0, &[1..2], 6));
    let error: Box<dyn std::error::Error> = Box::new(image.recover(&names(&[9])).unwrap_err());
    assert!(error.source().is_none());
}

#[test]
fn a_recovered_file_system_starts_afresh_and_remembers_where_it_came_from() {
    let config = SimConfig {
        sector_size: 4,
        max_file_len: 1_000,
        faults: FaultOdds {
            eio: Chance::one_in(1_000),
            ..FaultOdds::NONE
        },
        crash: CrashOdds {
            lose: Chance::new(1, 3),
            ..CrashOdds::default()
        },
        trace_ops: false,
        seed: 12,
    };
    let fs = SimVfs::new(config.clone());
    let file = create_durable(&fs, "/file", b"........");
    file.write_at(b"aaaaaa", 0).unwrap();
    fs.rename(p("/file"), p("/renamed")).unwrap();
    let lock = fs.lock(p("/LOCK")).unwrap();
    fs.add_rule(FaultRule::new(Fault::Eio).forever());
    fs.crash_at(CrashPoint::AfterOp(1_000));

    let image = fs.crash_image();
    let plan = CrashPlan {
        names: vec![1],
        changes: vec![Survivor {
            change: 0,
            tear: Some(image.tears(0)[1].clone()),
        }],
    };
    let after = recover(&image, &plan);
    assert_eq!(after.stats(), SimStats::default());
    assert!(!after.is_powered_off());
    let seed = after.config().seed;
    assert_ne!(seed, 12);
    assert_eq!(after.config(), SimConfig { seed, ..config });
    assert_eq!(seed, recover(&image, &image.lose_all()).config().seed);

    let [Event::Crash(record)] = &after.trace()[..] else {
        panic!("{:?}", after.trace());
    };
    assert_eq!((record.seed, record.ops), (12, image.ops()));
    let names: Vec<_> = record
        .names
        .iter()
        .map(|name| (name.change.kind, name.survives))
        .collect();
    assert_eq!(
        names,
        [
            (NameChangeKind::Rename, false),
            (NameChangeKind::Create, true)
        ]
    );
    assert_eq!(record.names[0].change.to.as_deref(), Some("/renamed"));
    let [change] = &record.changes[..] else {
        panic!("{:?}", record.changes);
    };
    assert_eq!(change.path, "/file");
    assert_eq!(
        change.fate,
        Fate::Survives {
            position: 0,
            tear: Some(Tear {
                sectors: vec![0..1],
                len: 4
            })
        }
    );

    assert_eq!(
        tree(&after),
        tree_of(&[("LOCK", Some("")), ("file", Some("aaaa...."))])
    );
    drop(after.lock(p("/LOCK")).unwrap());
    after.sync_dir(p("/")).unwrap();
    assert_eq!(after.stats().faults, 0);
    assert_eq!(after.trace().len(), 1);
    drop(lock);
}

#[test]
fn a_file_the_plan_does_not_touch_is_shared_and_copied_only_when_written() {
    let fs = sim(13);
    let untouched = create_durable(&fs, "/untouched", &[1; 4_096]);
    let changed = create_durable(&fs, "/changed", &[2; 4_096]);
    changed.write_at(b"change", 0).unwrap();
    let buffers = |fs: &SimVfs, path: &str| fs.shared.state().buffers(p(path));

    let image = fs.crash_image();
    let after = recover(&image, &image.keep_all());
    let (before, durable_before) = buffers(&fs, "/untouched");
    let (content, durable) = buffers(&after, "/untouched");
    assert!(!Arc::ptr_eq(&before, &durable_before));
    assert!(Arc::ptr_eq(&durable_before, &durable) && Arc::ptr_eq(&content, &durable));
    let (content, durable) = buffers(&after, "/changed");
    assert!(Arc::ptr_eq(&content, &durable));
    assert!(!Arc::ptr_eq(&durable, &buffers(&fs, "/changed").1));
    drop((content, durable, before, durable_before));

    let reopened = after
        .open(p("/untouched"), OpenMode::Write(Create::Never))
        .unwrap();
    reopened.write_at(b"after", 0).unwrap();
    let (content, durable) = buffers(&after, "/untouched");
    assert!(!Arc::ptr_eq(&content, &durable));
    assert!(Arc::ptr_eq(&durable, &buffers(&fs, "/untouched").1));
    drop((content, durable));

    untouched.write_at(b"before", 0).unwrap();
    untouched.sync_data().unwrap();
    reopened.sync_data().unwrap();
    assert_eq!(read(&fs, "/untouched")[..8], *b"before\x01\x01");
    assert_eq!(read(&after, "/untouched")[..8], *b"after\x01\x01\x01");
    let kept = recover(&image, &image.lose_all());
    assert_eq!(read(&kept, "/untouched"), [1; 4_096]);
}

#[test]
fn a_fingerprint_tells_states_apart() {
    let build = |steps: &dyn Fn(&SimVfs)| {
        let fs = sim(14);
        steps(&fs);
        fs.fingerprint()
    };
    let base = build(&|fs| {
        drop(create(fs, "/a", b"content"));
        fs.create_dir(p("/dir")).unwrap();
        drop(create(fs, "/dir/b", b"content"));
    });
    let same = build(&|fs| {
        fs.create_dir(p("/dir")).unwrap();
        drop(create(fs, "/dir/b", b"content"));
        drop(create(fs, "/temporary", b""));
        fs.rename(p("/temporary"), p("/a")).unwrap();
        let file = fs.open(p("/a"), OpenMode::Append(Create::Never)).unwrap();
        file.append(b"content").unwrap();
        file.sync_data().unwrap();
    });
    assert_eq!(base, same);

    let others = [
        build(&|_| {}),
        build(&|fs| {
            drop(create(fs, "/a", b"content"));
            fs.create_dir(p("/dir")).unwrap();
            drop(create(fs, "/dir/b", b"CONTENT"));
        }),
        build(&|fs| {
            drop(create(fs, "/a", b"content"));
            fs.create_dir(p("/dir")).unwrap();
            fs.hard_link(p("/a"), p("/dir/b")).unwrap();
        }),
        build(&|fs| {
            drop(create(fs, "/a", b"content"));
            fs.create_dir(p("/dir")).unwrap();
            drop(create(fs, "/b", b"content"));
        }),
        build(&|fs| {
            drop(create(fs, "/a", b"content"));
            fs.create_dir(p("/dir")).unwrap();
            fs.create_dir(p("/dir/b")).unwrap();
        }),
        build(&|fs| {
            drop(create(fs, "/a", b"content"));
            fs.create_dir(p("/dir")).unwrap();
            drop(create(fs, "/dir/b", b"content\0"));
        }),
    ];
    let distinct: BTreeSet<u64> = others.into_iter().chain([base]).collect();
    assert_eq!(distinct.len(), 7);
}

#[test]
fn a_file_stops_growing_at_the_longest_the_simulator_holds() {
    let fs = SimVfs::new(SimConfig {
        max_file_len: 100,
        ..SimConfig::new(15)
    });
    let file = create(&fs, "/file", &[1; 60]);
    assert_eq!(kind(file.write_at(&[2; 41], 60)), ErrorKind::FileTooLarge);
    assert_eq!(kind(file.write_at(&[2; 1], 100)), ErrorKind::FileTooLarge);
    assert_eq!(
        kind(file.write_at(&[2; 1], u64::MAX)),
        ErrorKind::FileTooLarge
    );
    assert_eq!(kind(file.write_at(&[], 101)), ErrorKind::FileTooLarge);
    assert_eq!(kind(file.set_len(101)), ErrorKind::FileTooLarge);
    assert_eq!(kind(file.set_len(u64::MAX)), ErrorKind::FileTooLarge);
    file.write_at(&[2; 40], 60).unwrap();
    file.set_len(100).unwrap();
    assert_eq!(file.read_at(&mut [0; 8], u64::MAX).unwrap(), 0);
    drop(file);

    let log = fs
        .open(p("/file"), OpenMode::Append(Create::Never))
        .unwrap();
    assert_eq!(kind(log.append(b"x")), ErrorKind::FileTooLarge);
    assert_eq!(log.append(b"").unwrap(), 100);
    assert_eq!(fs.stats().faults, 0);
    assert_eq!(SimConfig::new(0).max_file_len, 1 << 30);
}

#[test]
fn paths_are_taken_from_the_root_and_must_be_plain() {
    let fs = sim(16);
    fs.create_dir(p("dir")).unwrap();
    drop(create(&fs, "/dir/./file", b"content"));
    assert_eq!(read(&fs, "dir/file"), b"content");
    assert_eq!(read(&fs, "./dir//file"), b"content");

    for path in ["/dir/../dir/file", "..", "/dir/file/.."] {
        assert_eq!(
            kind(fs.open(p(path), OpenMode::Read)),
            ErrorKind::InvalidInput
        );
        assert_eq!(kind(fs.read_dir(p(path))), ErrorKind::InvalidInput);
        assert_eq!(kind(fs.sync_dir(p(path))), ErrorKind::InvalidInput);
        assert_eq!(
            kind(fs.rename(p("/dir/file"), p(path))),
            ErrorKind::InvalidInput
        );
    }
    let error = fs.remove_file(p("/dir/..")).unwrap_err();
    assert!(
        error.to_string().starts_with("/dir/.. has a `..`"),
        "{error}"
    );

    for root in ["/", "", "."] {
        let path = p(root);
        assert_eq!(kind(fs.open(path, OpenMode::Read)), ErrorKind::IsADirectory);
        assert_eq!(kind(fs.lock(path)), ErrorKind::IsADirectory);
        assert_eq!(kind(fs.remove_file(path)), ErrorKind::IsADirectory);
        assert_eq!(kind(fs.rename(path, p("/x"))), ErrorKind::IsADirectory);
        assert_eq!(
            kind(fs.rename(p("/dir/file"), path)),
            ErrorKind::IsADirectory
        );
        assert_eq!(
            kind(fs.hard_link(path, p("/x"))),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            kind(fs.hard_link(p("/dir/file"), path)),
            ErrorKind::AlreadyExists
        );
        assert_eq!(kind(fs.create_dir(path)), ErrorKind::AlreadyExists);
        assert_eq!(kind(fs.remove_dir(path)), ErrorKind::ResourceBusy);
        assert_eq!(fs.read_dir(path).unwrap().len(), 1);
        fs.sync_dir(path).unwrap();
    }
}

#[test]
fn a_directory_where_a_file_is_needed_and_a_file_where_a_directory_is_needed_are_refused() {
    let fs = sim(17);
    fs.create_dir(p("/dir")).unwrap();
    drop(create(&fs, "/file", b""));

    assert_eq!(
        kind(fs.rename(p("/dir"), p("/other"))),
        ErrorKind::IsADirectory
    );
    assert_eq!(
        kind(fs.rename(p("/file"), p("/dir"))),
        ErrorKind::IsADirectory
    );
    assert_eq!(
        kind(fs.hard_link(p("/dir"), p("/other"))),
        ErrorKind::PermissionDenied
    );
    assert_eq!(kind(fs.remove_file(p("/dir"))), ErrorKind::IsADirectory);
    assert_eq!(kind(fs.lock(p("/dir"))), ErrorKind::IsADirectory);
    let new = OpenMode::Write(Create::New);
    assert_eq!(kind(fs.open(p("/dir"), new)), ErrorKind::AlreadyExists);

    assert_eq!(kind(fs.open(p("/file/x"), new)), ErrorKind::NotADirectory);
    assert_eq!(kind(fs.create_dir(p("/file/x"))), ErrorKind::NotADirectory);
    assert_eq!(
        kind(fs.rename(p("/file"), p("/file/x"))),
        ErrorKind::NotADirectory
    );
    assert_eq!(
        kind(fs.hard_link(p("/file/x"), p("/y"))),
        ErrorKind::NotADirectory
    );
    assert_eq!(kind(fs.read_dir(p("/file/x"))), ErrorKind::NotADirectory);
    assert_eq!(kind(fs.lock(p("/file/LOCK"))), ErrorKind::NotADirectory);
    assert_eq!(
        kind(fs.rename(p("/file"), p("/missing/x"))),
        ErrorKind::NotFound
    );
    assert_eq!(
        kind(fs.hard_link(p("/file"), p("/missing/x"))),
        ErrorKind::NotFound
    );
    assert_eq!(tree(&fs), tree_of(&[("dir", None), ("file", Some(""))]));
    check(&fs);
}

#[test]
fn handles_and_images_show_what_they_are() {
    let fs = sim(18);
    let file = create(&fs, "/file", b"");
    let lock = fs.lock(p("/LOCK")).unwrap();
    assert_eq!(
        format!("{fs:?}"),
        "SimVfs { seed: 18, ops: 3, powered_off: false, .. }"
    );
    assert!(format!("{file:?}").starts_with("SimFile("));
    assert!(format!("{lock:?}").starts_with("SimLock("));
    assert_eq!(fs.config(), SimConfig::new(18));
}

#[test]
#[should_panic(expected = "a sector has at least one byte")]
fn a_sector_has_at_least_one_byte() {
    sim_with_sectors(19, 0);
}
