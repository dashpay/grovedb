use super::*;

fn injected(fs: &SimVfs) -> Vec<(OpKind, Fault, Effect)> {
    fs.trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::Op(OpRecord {
                kind,
                outcome: Outcome::Injected { fault, effect },
                ..
            }) => Some((kind, fault, effect)),
            _ => None,
        })
        .collect()
}

type Operation = Box<dyn Fn(&SimVfs) -> io::Result<()>>;

/// One call of each operation that succeeds on [`populated`].
fn every_operation() -> Vec<(OpKind, Operation)> {
    fn open(fs: &SimVfs, path: &str, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        fs.open(p(path), mode)
    }
    let write = OpenMode::Write(Create::Never);
    let append = OpenMode::Append(Create::Never);
    vec![
        (
            OpKind::Open,
            Box::new(move |fs| open(fs, "/file", write).map(drop)),
        ),
        (
            OpKind::Rename,
            Box::new(|fs| fs.rename(p("/file"), p("/renamed"))),
        ),
        (
            OpKind::HardLink,
            Box::new(|fs| fs.hard_link(p("/file"), p("/link"))),
        ),
        (
            OpKind::RemoveFile,
            Box::new(|fs| fs.remove_file(p("/file"))),
        ),
        (OpKind::CreateDir, Box::new(|fs| fs.create_dir(p("/new")))),
        (OpKind::RemoveDir, Box::new(|fs| fs.remove_dir(p("/dir")))),
        (
            OpKind::ReadDir,
            Box::new(|fs| fs.read_dir(p("/")).map(drop)),
        ),
        (OpKind::SyncDir, Box::new(|fs| fs.sync_dir(p("/")))),
        (OpKind::Lock, Box::new(|fs| fs.lock(p("/LOCK")).map(drop))),
        (
            OpKind::Read,
            Box::new(move |fs| open(fs, "/file", write)?.read_at(&mut [0; 4], 0).map(drop)),
        ),
        (
            OpKind::Write,
            Box::new(move |fs| open(fs, "/file", write)?.write_at(b"written", 2)),
        ),
        (
            OpKind::Append,
            Box::new(move |fs| open(fs, "/file", append)?.append(b"appended").map(drop)),
        ),
        (
            OpKind::SyncData,
            Box::new(move |fs| open(fs, "/file", write)?.sync_data()),
        ),
        (
            OpKind::SyncAll,
            Box::new(move |fs| open(fs, "/file", write)?.sync_all()),
        ),
        (
            OpKind::Len,
            Box::new(move |fs| open(fs, "/file", write)?.len().map(drop)),
        ),
        (
            OpKind::SetLen,
            Box::new(move |fs| open(fs, "/file", write)?.set_len(100)),
        ),
    ]
}

fn populated(seed: u64) -> SimVfs {
    let fs = sim(seed);
    drop(create(&fs, "/file", b"content"));
    fs.create_dir(p("/dir")).unwrap();
    drop(fs.lock(p("/LOCK")).unwrap());
    fs
}

#[test]
fn every_operation_can_fail_with_eio_and_most_leave_nothing_behind() {
    for (kind, operation) in every_operation() {
        let fs = populated(1);
        let before = fs.fingerprint();
        fs.add_rule(FaultRule::new(Fault::Eio).on(kind));
        let error = operation(&fs).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other, "{kind}");
        let Some(SimError::Injected {
            fault: Fault::Eio,
            op,
            kind: failed,
            path,
        }) = SimError::of(&error)
        else {
            panic!("{kind}: {error}");
        };
        assert_eq!((*failed, *op), (kind, fs.stats().ops));
        assert!(path.starts_with('/'));
        assert_eq!(SimError::injected(&error), Some(Fault::Eio));
        assert!(!SimError::is_power_loss(&error));
        assert!(error.to_string().starts_with("simulated EIO in operation"));

        assert_eq!(fs.stats().faults, 1);
        let [(traced, Fault::Eio, effect)] = injected(&fs)[..] else {
            panic!("{kind}: {:?}", fs.trace());
        };
        assert_eq!(traced, kind);
        match kind {
            OpKind::Write => assert!(matches!(effect, Effect::Wrote(0 | 7))),
            OpKind::Append => assert!(matches!(
                effect,
                Effect::AppendUndone(0 | 8) | Effect::AppendTorn(0 | 8)
            )),
            OpKind::SyncData | OpKind::SyncAll => assert_eq!(effect, Effect::SyncFailed(1)),
            _ => {
                assert_eq!(effect, Effect::Nothing);
                assert_eq!(fs.fingerprint(), before, "{kind}");
            }
        }
        check(&fs);

        operation(&fs).unwrap();
        assert_eq!(fs.stats().faults, 1);
        check(&fs);
    }
}

#[test]
fn only_operations_that_can_need_space_fail_with_enospc() {
    let needs_space = [
        OpKind::Rename,
        OpKind::HardLink,
        OpKind::CreateDir,
        OpKind::SyncDir,
        OpKind::Write,
        OpKind::Append,
        OpKind::SyncData,
        OpKind::SyncAll,
        OpKind::SetLen,
    ];
    for (kind, operation) in every_operation() {
        let fs = populated(2);
        fs.add_rule(FaultRule::new(Fault::Enospc).forever());
        let result = operation(&fs);
        if needs_space.contains(&kind) {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::StorageFull, "{kind}");
            assert_eq!(SimError::injected(&error), Some(Fault::Enospc));
            assert!(error.to_string().starts_with("simulated ENOSPC"));
        } else {
            result.unwrap();
            assert_eq!(fs.stats().faults, 0, "{kind}");
        }
    }
}

#[test]
fn a_full_disk_refuses_what_grows_and_works_again_when_space_returns() {
    let fs = populated(3);
    let file = fs.open(p("/file"), OpenMode::Write(Create::Never)).unwrap();
    fs.add_rule(
        FaultRule::new(Fault::Enospc)
            .from_op(fs.stats().ops + 1)
            .forever(),
    );

    let full = |result: io::Result<()>| assert_eq!(kind(result), ErrorKind::StorageFull);
    full(
        fs.open(p("/created"), OpenMode::Write(Create::New))
            .map(drop),
    );
    full(
        fs.open(p("/created"), OpenMode::Append(Create::IfMissing))
            .map(drop),
    );
    full(fs.lock(p("/NEW_LOCK")).map(drop));
    full(file.set_len(8));
    assert!(!exists(&fs, "/created") && !exists(&fs, "/NEW_LOCK"));

    file.set_len(7).unwrap();
    file.set_len(3).unwrap();
    assert_eq!(file.len().unwrap(), 3);
    drop(
        fs.open(p("/file"), OpenMode::Write(Create::IfMissing))
            .unwrap(),
    );
    drop(fs.lock(p("/LOCK")).unwrap());
    fs.remove_dir(p("/dir")).unwrap();
    fs.remove_file(p("/LOCK")).unwrap();
    assert_eq!(read(&fs, "/file"), b"con");

    fs.clear_rules();
    let faults = fs.stats().faults;
    file.write_at(b"tent and more", 3).unwrap();
    file.sync_all().unwrap();
    fs.sync_dir(p("/")).unwrap();
    assert_eq!(fs.stats().faults, faults);
    let image = fs.crash_image();
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("file", Some("content and more"))])
    );
}

#[test]
fn an_operation_that_fails_anyway_gets_no_fault() {
    let fs = populated(4);
    fs.add_rule(FaultRule::new(Fault::Eio).forever());
    let read_only = {
        fs.clear_rules();
        let handle = fs.open(p("/file"), OpenMode::Read).unwrap();
        fs.add_rule(FaultRule::new(Fault::Eio).forever());
        handle
    };

    assert_eq!(
        kind(fs.open(p("/missing"), OpenMode::Read)),
        ErrorKind::NotFound
    );
    assert_eq!(kind(fs.create_dir(p("/dir"))), ErrorKind::AlreadyExists);
    assert_eq!(kind(fs.remove_dir(p("/file"))), ErrorKind::NotADirectory);
    assert_eq!(kind(read_only.write_at(b"x", 0)), ErrorKind::InvalidInput);
    assert_eq!(kind(read_only.append(b"x")), ErrorKind::InvalidInput);
    assert_eq!(kind(read_only.set_len(1)), ErrorKind::InvalidInput);
    assert_eq!(fs.stats().faults, 0);
    assert!(fs.trace().is_empty());

    assert_eq!(
        SimError::injected(&read_only.len().unwrap_err()),
        Some(Fault::Eio)
    );
    assert_eq!(fs.stats().faults, 1);
}

#[test]
fn a_failed_write_leaves_whole_sectors_from_its_start() {
    let mut seen = BTreeSet::new();
    for seed in 0..60 {
        let fs = sim_with_sectors(seed, 8);
        let file = create_durable(&fs, "/file", &[b'-'; 32]);
        fs.add_rule(FaultRule::new(Fault::Enospc).on(OpKind::Write));
        assert_eq!(kind(file.write_at(&[b'w'; 20], 5)), ErrorKind::StorageFull);

        let [(OpKind::Write, Fault::Enospc, Effect::Wrote(written))] = injected(&fs)[..] else {
            panic!("{:?}", fs.trace());
        };
        assert!([0, 3, 11, 19, 20].contains(&written), "{written}");
        let mut expected = vec![b'-'; 32];
        expected[5..5 + written as usize].fill(b'w');
        assert_eq!(read(&fs, "/file"), expected);

        let image = fs.crash_image();
        assert_eq!(image.changes().len(), usize::from(written > 0));
        assert_eq!(
            read(&recover(&image, &image.lose_all()), "/file"),
            [b'-'; 32]
        );
        assert_eq!(read(&recover(&image, &image.keep_all()), "/file"), expected);
        seen.insert(written);
    }
    assert_eq!(seen, BTreeSet::from([0, 3, 11, 19, 20]));
}

#[test]
fn a_failed_write_of_nothing_leaves_nothing() {
    let fs = sim(21);
    let file = create(&fs, "/file", b"content");
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::WRITES).forever());
    file.write_at(&[], 3).unwrap_err();
    drop(file);
    let log = fs
        .open(p("/file"), OpenMode::Append(Create::Never))
        .unwrap();
    log.append(&[]).unwrap_err();
    assert_eq!(
        injected(&fs)
            .iter()
            .map(|(_, _, effect)| *effect)
            .collect::<Vec<_>>(),
        [Effect::Wrote(0), Effect::AppendUndone(0)]
    );
    assert_eq!(fs.crash_image().changes().len(), 1);
    assert_eq!(read(&fs, "/file"), b"content");
}

#[test]
fn a_failed_append_is_taken_back_but_a_crash_can_bring_it_back() {
    let mut seen = BTreeSet::new();
    for seed in 0..40 {
        let fs = sim_with_sectors(seed, 4);
        let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
        fs.sync_dir(p("/")).unwrap();
        log.append(b"committed").unwrap();
        log.sync_data().unwrap();
        fs.add_rule(
            FaultRule::new(Fault::Enospc)
                .on(OpKind::Append)
                .torn_append(false),
        );
        assert_eq!(kind(log.append(b"0123456789")), ErrorKind::StorageFull);

        let [(OpKind::Append, Fault::Enospc, Effect::AppendUndone(written))] = injected(&fs)[..]
        else {
            panic!("{:?}", fs.trace());
        };
        assert!([0, 3, 7, 10].contains(&written));
        seen.insert(written);
        assert_eq!(log.len().unwrap(), 9);
        assert_eq!(read(&fs, "/log"), b"committed");

        let image = fs.crash_image();
        if written == 0 {
            assert!(image.changes().is_empty());
        } else {
            let kinds: Vec<_> = image.changes().iter().map(|change| change.kind).collect();
            let undone = [
                FileChangeKind::Write {
                    offset: 9,
                    len: written,
                },
                FileChangeKind::SetLen { len: 9 },
            ];
            assert_eq!(kinds, undone);
            assert_eq!(image.changes()[0].op, image.changes()[1].op);
            assert_eq!(
                read(&recover(&image, &image.keep_all()), "/log"),
                b"committed"
            );
            let brought_back = CrashPlan {
                names: Vec::new(),
                changes: vec![Survivor::whole(0)],
            };
            let mut torn = b"committed".to_vec();
            torn.extend(&b"0123456789"[..written as usize]);
            assert_eq!(read(&recover(&image, &brought_back), "/log"), torn);
        }

        assert_eq!(log.append(b" next").unwrap(), 9);
        assert_eq!(read(&fs, "/log"), b"committed next");
        check(&fs);
    }
    assert_eq!(seen, BTreeSet::from([0, 3, 7, 10]));
}

#[test]
fn an_append_that_cannot_be_taken_back_breaks_the_handle() {
    let mut seen = BTreeSet::new();
    for seed in 0..40 {
        let fs = sim_with_sectors(seed, 4);
        let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
        log.append(b"committed").unwrap();
        fs.add_rule(
            FaultRule::new(Fault::Eio)
                .on(OpKind::Append)
                .torn_append(true),
        );
        assert_eq!(
            SimError::injected(&log.append(b"0123456789").unwrap_err()),
            Some(Fault::Eio)
        );

        let [(OpKind::Append, Fault::Eio, Effect::AppendTorn(written))] = injected(&fs)[..] else {
            panic!("{:?}", fs.trace());
        };
        seen.insert(written);
        let mut torn = b"committed".to_vec();
        torn.extend(&b"0123456789"[..written as usize]);
        assert_eq!(read(&fs, "/log"), torn);

        for _ in 0..2 {
            let error = log.append(b"x").unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Other);
            assert!(SimError::of(&error).is_none());
            assert!(error.to_string().contains("could not be undone"));
        }
        assert_eq!(fs.stats().faults, 1);
        assert_eq!(read(&fs, "/log"), torn);
        log.sync_data().unwrap();
        log.set_len(9).unwrap();
        assert_eq!(kind(log.append(b"x")), ErrorKind::Other);
        drop(log);

        let log = fs.open(p("/log"), OpenMode::Append(Create::Never)).unwrap();
        assert_eq!(log.append(b"!").unwrap(), 9);
        check(&fs);
    }
    assert_eq!(seen, BTreeSet::from([0, 3, 7, 10]));
}

#[test]
fn whether_a_failed_append_stays_is_drawn_from_the_odds_unless_the_rule_says() {
    let outcome = |seed: u64, torn_append: Chance| {
        let fs = SimVfs::new(SimConfig {
            faults: FaultOdds {
                torn_append,
                ..FaultOdds::NONE
            },
            ..SimConfig::new(seed)
        });
        let log = fs.open(p("/log"), OpenMode::Append(Create::New)).unwrap();
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Append));
        log.append(b"appended").unwrap_err();
        matches!(injected(&fs)[..], [(_, _, Effect::AppendTorn(_))])
    };
    assert!((0..20).all(|seed| !outcome(seed, Chance::NEVER)));
    assert!((0..20).all(|seed| outcome(seed, Chance::ALWAYS)));
    let torn = (0..200)
        .filter(|seed| outcome(*seed, FaultOdds::NONE.torn_append))
        .count();
    assert!((25..75).contains(&torn), "{torn}");
}

#[test]
fn a_failed_directory_sync_makes_nothing_durable_and_can_be_repeated() {
    let fs = sim(5);
    drop(create(&fs, "/file", b""));
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::SyncDir));
    fs.sync_dir(p("/")).unwrap_err();
    let image = fs.crash_image();
    assert_eq!(image.names().len(), 1);
    assert_eq!(tree(&recover(&image, &image.lose_all())), tree_of(&[]));

    fs.sync_dir(p("/")).unwrap();
    let image = fs.crash_image();
    assert_eq!(
        tree(&recover(&image, &image.lose_all())),
        tree_of(&[("file", Some(""))])
    );
}

#[test]
fn a_rule_fails_the_operations_it_names_and_no_others() {
    let fs = populated(6);
    let file = fs.open(p("/file"), OpenMode::Write(Create::Never)).unwrap();
    let other = create(&fs, "/other", b"");
    let syncs = |file: &dyn VfsFile, count| -> Vec<bool> {
        (0..count).map(|_| file.len().is_ok()).collect()
    };

    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Len)
            .at("/file")
            .skip(2)
            .times(2),
    );
    assert_eq!(syncs(&*other, 3), [true; 3]);
    assert_eq!(syncs(&*file, 6), [true, true, false, false, true, true]);
    assert_eq!(fs.stats().faults, 2);

    fs.clear_rules();
    let next = fs.stats().ops + 1;
    fs.add_rule(FaultRule::new(Fault::Eio).from_op(next + 2));
    assert_eq!(syncs(&*file, 4), [true, true, false, true]);

    fs.clear_rules();
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::READS | OpKind::Write.into())
            .forever(),
    );
    assert!(file.len().is_err() && file.read_at(&mut [0], 0).is_err());
    assert!(fs.read_dir(p("/")).is_err() && file.write_at(b"x", 0).is_err());
    file.set_len(0).unwrap();
    file.sync_data().unwrap();
    fs.clear_rules();
    assert_eq!(syncs(&*file, 2), [true, true]);
}

#[test]
fn a_rule_with_a_path_sees_both_names_of_a_rename_and_a_link() {
    for target in ["/file", "renamed"] {
        let fs = populated(7);
        fs.add_rule(FaultRule::new(Fault::Eio).at(target).forever());
        fs.hard_link(p("/LOCK"), p("/elsewhere")).unwrap();
        for result in [
            fs.rename(p("/file"), p("/renamed")),
            fs.hard_link(p("/file"), p("/renamed")),
        ] {
            assert_eq!(SimError::injected(&result.unwrap_err()), Some(Fault::Eio));
        }
    }

    let fs = populated(7);
    fs.add_rule(FaultRule::new(Fault::Eio).at("/dir/../file").forever());
    fs.hard_link(p("/file"), p("/link")).unwrap();
    fs.rename(p("/file"), p("/renamed")).unwrap();
    assert_eq!(fs.stats().faults, 0);
}

#[test]
fn the_first_rule_whose_turn_it_is_decides_and_the_others_still_count() {
    let fs = populated(8);
    let file = fs.open(p("/file"), OpenMode::Write(Create::Never)).unwrap();
    fs.add_rule(FaultRule::new(Fault::Enospc).on(OpKind::Write).skip(1));
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Write).times(2));
    fs.add_rule(FaultRule::new(Fault::Enospc).on(OpKind::Write).skip(3));

    let faults: Vec<Option<Fault>> = (0..5)
        .map(|_| {
            file.write_at(b"x", 0)
                .err()
                .and_then(|error| SimError::injected(&error))
        })
        .collect();
    assert_eq!(
        faults,
        [
            Some(Fault::Eio),
            Some(Fault::Enospc),
            None,
            Some(Fault::Enospc),
            None
        ]
    );
}

#[test]
fn faults_by_chance_come_as_often_as_the_odds_say() {
    let fs = SimVfs::new(SimConfig {
        faults: FaultOdds {
            eio: Chance::new(1, 4),
            enospc: Chance::new(1, 2),
            ..FaultOdds::NONE
        },
        ..SimConfig::new(9)
    });
    assert_eq!(fs.config().faults.eio, Chance::new(1, 4));
    let file = loop {
        if let Ok(file) = fs.open(p("/file"), OpenMode::Write(Create::IfMissing)) {
            break file;
        }
    };
    let before = fs.stats().faults;
    let traced_before = injected(&fs).len() as u64;
    let tally = |operation: &dyn Fn() -> io::Result<()>| {
        let faults: Vec<Fault> = (0..4_000)
            .filter_map(|_| operation().err())
            .map(|error| SimError::injected(&error).unwrap())
            .collect();
        let eio = faults.iter().filter(|fault| **fault == Fault::Eio).count();
        (eio, faults.len() - eio)
    };
    let (eio, enospc) = tally(&|| file.len().map(drop));
    assert!((850..1_150).contains(&eio) && enospc == 0, "{eio} {enospc}");
    let (eio, enospc) = tally(&|| file.write_at(b"x", 0));
    assert!((850..1_150).contains(&eio), "{eio}");
    assert!((1_300..1_700).contains(&enospc), "{enospc}");
    assert_eq!(
        fs.stats().faults - before,
        injected(&fs).len() as u64 - traced_before
    );

    fs.set_faults(FaultOdds::NONE);
    assert_eq!(fs.config().faults, FaultOdds::default());
    assert!((0..500).all(|_| file.write_at(b"x", 0).is_ok() && file.len().is_ok()));
}

#[test]
fn faults_by_chance_hit_only_the_operations_they_are_set_for() {
    let fs = SimVfs::new(SimConfig {
        faults: FaultOdds {
            eio: Chance::ALWAYS,
            eio_ops: OpSet::SYNCS,
            enospc: Chance::ALWAYS,
            enospc_ops: OpSet::WRITES,
            ..FaultOdds::NONE
        },
        ..SimConfig::new(10)
    });
    fs.create_dir(p("/dir")).unwrap();
    let file = fs.open(p("/file"), OpenMode::Write(Create::New)).unwrap();
    assert_eq!(kind(file.write_at(b"x", 0)), ErrorKind::StorageFull);
    assert_eq!(kind(file.set_len(4)), ErrorKind::StorageFull);
    file.set_len(0).unwrap();
    assert_eq!(kind(file.sync_data()), ErrorKind::Other);
    assert_eq!(kind(fs.sync_dir(p("/"))), ErrorKind::Other);
    fs.rename(p("/file"), p("/dir/file")).unwrap();
    assert_eq!(file.len().unwrap(), 0);
}

#[test]
fn op_sets_name_their_operations() {
    assert!(OpKind::ALL.iter().all(|kind| OpSet::ALL.contains(*kind)));
    assert!(OpKind::ALL.iter().all(|kind| !OpSet::NONE.contains(*kind)));
    let groups = [OpSet::READS, OpSet::WRITES, OpSet::SYNCS, OpSet::NAMES];
    for kind in OpKind::ALL {
        let holders = groups.iter().filter(|group| group.contains(kind)).count();
        assert_eq!(holders, 1, "{kind}");
        assert!(OpSet::from(kind).contains(kind));
        assert_eq!(OpKind::ALL[kind.code() as usize], kind);
    }
    let all = groups
        .into_iter()
        .fold(OpSet::NONE, |all, group| all | group);
    assert!(OpKind::ALL.iter().all(|kind| all.contains(*kind)));
    assert_eq!(
        format!("{:?}", OpSet::SYNCS),
        "{SyncDir, SyncData, SyncAll}"
    );
    let names: Vec<String> = OpKind::ALL.iter().map(OpKind::to_string).collect();
    assert_eq!(
        names.join(" "),
        "open rename hard_link remove_file create_dir remove_dir read_dir sync_dir lock read_at \
         write_at append sync_data sync_all len set_len"
    );
}

#[test]
fn an_error_of_another_kind_is_not_the_simulators() {
    let plain = io::Error::from(ErrorKind::NotFound);
    let foreign = io::Error::other("something else");
    for error in [&plain, &foreign] {
        assert!(SimError::of(error).is_none());
        assert!(SimError::injected(error).is_none());
        assert!(!SimError::is_power_loss(error));
    }
    let power_loss = io::Error::from(SimError::PowerLoss);
    assert_eq!(power_loss.kind(), ErrorKind::Other);
    assert!(SimError::is_power_loss(&power_loss));
    assert!(SimError::injected(&power_loss).is_none());
    assert_eq!(power_loss.to_string(), "the simulated power is off");
}
