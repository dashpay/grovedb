use super::*;

fn traced(seed: u64) -> SimVfs {
    let fs = SimVfs::new(SimConfig {
        trace_ops: true,
        sector_size: 4,
        ..SimConfig::new(seed)
    });
    let file = create(&fs, "/file", b"0123456789");
    file.read_at(&mut [0; 4], 2).unwrap();
    file.set_len(12).unwrap();
    fs.add_rule(FaultRule::new(Fault::Enospc).on(OpKind::Write));
    file.write_at(b"abcdefgh", 2).unwrap_err();
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::SyncData));
    file.sync_data().unwrap_err();
    file.sync_data().unwrap_err();
    drop(file);
    fs.rename(p("/file"), p("/log")).unwrap();
    fs.rename(p("/file"), p("/log")).unwrap_err();
    let log = fs.open(p("/log"), OpenMode::Append(Create::Never)).unwrap();
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Append)
            .torn_append(false),
    );
    log.append(b"undone").unwrap_err();
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpKind::Append)
            .torn_append(true),
    );
    log.append(b"torn").unwrap_err();
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Len));
    log.len().unwrap_err();
    log.sync_all().unwrap();
    fs.hard_link(p("/log"), p("/link")).unwrap();
    fs.create_dir(p("/dir")).unwrap();
    fs.remove_file(p("/link")).unwrap();
    fs.remove_dir(p("/dir")).unwrap();
    let reader = fs.open(p("/log"), OpenMode::Read).unwrap();
    drop(log);
    let writer = fs.open(p("/log"), OpenMode::Write(Create::Never)).unwrap();
    writer.write_at(b"WRITTEN", 1).unwrap();
    writer.set_len(6).unwrap();
    writer.write_at(b"!", 7).unwrap();
    fs.crash_at(CrashPoint::AfterOp(fs.stats().ops + 1));
    reader.len().unwrap();
    reader.len().unwrap_err();
    fs
}

#[test]
fn a_trace_reads_as_one_line_for_each_thing_that_happened() {
    let fs = traced(47);
    let lines: Vec<String> = fs.trace().iter().map(Event::to_string).collect();
    assert_eq!(lines, EXPECTED_LINES, "{}", lines.join("\n"));
    assert_eq!(fs.trace_digest(), digest(&fs.trace()));

    let image = fs.crash_image();
    let plan = CrashPlan {
        names: vec![0, 2, 4],
        changes: vec![
            Survivor::whole(2),
            Survivor {
                change: 0,
                tear: Some(Tear {
                    sectors: vec![1..2],
                    len: 8,
                }),
            },
        ],
    };
    let after = image.recover(&plan).unwrap();
    let crash = after.trace()[0].to_string();
    assert_eq!(crash, EXPECTED_CRASH, "{crash}");
}

const EXPECTED_LINES: [&str; 26] = [
    "#1 open /file: ok",
    "#2 write_at /file offset 0 len 10: ok",
    "#3 read_at /file offset 2 len 4: ok",
    "#4 set_len /file len 12: ok",
    "#5 write_at /file offset 2 len 8: injected ENOSPC, 2 bytes were written",
    "#6 sync_data /file: injected EIO, 3 unsynced changes left unknown",
    "#7 sync_data /file: failed (Other)",
    "#8 rename /file -> /log: ok",
    "#9 rename /file -> /log: failed (NotFound)",
    "#10 open /log: ok",
    "#11 append /log offset 12 len 6: injected EIO, 6 bytes were appended and taken back",
    "#12 append /log offset 12 len 4: injected EIO, 4 bytes stay appended and the handle is broken",
    "#13 len /log: injected EIO",
    "#14 the sync of /log settles what a failed sync left unknown:\n  #2 write offset 0 len 10 \
     /file: survives whole, applied at 0\n  #4 set_len 12 /file: lost\n  #5 write offset 2 len 2 \
     /file: survives torn (sectors 0..1, length 0), applied at 1",
    "#14 sync_all /log: ok",
    "#15 hard_link /log -> /link: ok",
    "#16 create_dir /dir: ok",
    "#17 remove_file /link: ok",
    "#18 remove_dir /dir: ok",
    "#19 open /log: ok",
    "#20 open /log: ok",
    "#21 write_at /log offset 1 len 7: ok",
    "#22 set_len /log len 6: ok",
    "#23 write_at /log offset 7 len 1: ok",
    "#24 len /log: ok",
    "power off after #24",
];
const EXPECTED_CRASH: &str = "\
crash of the file system with seed 0x000000000000002f after #24:
  #1 create /file: survives
  #8 rename /file -> /log: lost
  #15 hard_link /log -> /link: survives
  #16 create_dir /dir: lost
  #17 remove_file /link: survives
  #18 remove_dir /dir: lost
  #21 write offset 1 len 7 /log: survives torn (sectors 1..2, length 8), applied at 1
  #22 set_len 6 /log: lost
  #23 write offset 7 len 1 /log: survives whole, applied at 0";

#[test]
fn a_digest_tells_traces_apart() {
    assert_eq!(digest(&[]), 0xCBF2_9CE4_8422_2325);

    let op = |change: &dyn Fn(&mut OpRecord)| {
        let mut record = OpRecord {
            op: 7,
            kind: OpKind::Write,
            path: "/file".to_owned(),
            to: None,
            offset: 16,
            len: 4,
            outcome: Outcome::Ok,
        };
        change(&mut record);
        Event::Op(record)
    };
    let injected =
        |fault, effect| op(&|record| record.outcome = Outcome::Injected { fault, effect });
    let fate = |kind, fate| ChangeFate {
        op: 3,
        path: "/file".to_owned(),
        kind,
        fate,
    };
    let write = FileChangeKind::Write { offset: 0, len: 9 };
    let torn = |sectors: &[Range<u64>], len| Fate::Survives {
        position: 0,
        tear: Some(Tear {
            sectors: sectors.to_vec(),
            len,
        }),
    };
    let whole = |position| Fate::Survives {
        position,
        tear: None,
    };
    let settled = |changes: Vec<ChangeFate>| Event::Settled {
        op: 9,
        path: "/file".to_owned(),
        changes,
    };
    let name = |kind, to: Option<&str>, survives| NameFate {
        change: NameChange {
            op: 2,
            kind,
            path: "/a".to_owned(),
            to: to.map(str::to_owned),
        },
        survives,
    };
    let crash = |seed, ops, names: Vec<NameFate>, changes: Vec<ChangeFate>| {
        Event::Crash(CrashRecord {
            seed,
            ops,
            names,
            changes,
        })
    };

    let mut events = vec![
        op(&|_| {}),
        op(&|record| record.op = 8),
        op(&|record| record.kind = OpKind::Append),
        op(&|record| record.path = "/other".to_owned()),
        op(&|record| record.to = Some(String::new())),
        op(&|record| record.to = Some("/file".to_owned())),
        op(&|record| record.offset = 17),
        op(&|record| record.len = 5),
        injected(Fault::Eio, Effect::Nothing),
        injected(Fault::Enospc, Effect::Nothing),
        injected(Fault::Eio, Effect::Wrote(0)),
        injected(Fault::Eio, Effect::Wrote(1)),
        injected(Fault::Eio, Effect::AppendUndone(1)),
        injected(Fault::Eio, Effect::AppendTorn(1)),
        injected(Fault::Eio, Effect::SyncFailed(1)),
        settled(vec![]),
        settled(vec![fate(write, Fate::Lost)]),
        settled(vec![fate(write, whole(0))]),
        settled(vec![fate(write, whole(1))]),
        settled(vec![fate(write, torn(&[], 0))]),
        settled(vec![fate(write, torn(&[], 9))]),
        settled(vec![fate(write, torn(&[0..1], 0))]),
        settled(vec![fate(write, torn(&[0..2], 0))]),
        settled(vec![fate(write, torn(&[1..2], 0))]),
        settled(vec![fate(write, torn(&[0..1, 2..3], 0))]),
        settled(vec![fate(
            FileChangeKind::Write { offset: 1, len: 9 },
            Fate::Lost,
        )]),
        settled(vec![fate(
            FileChangeKind::Write { offset: 0, len: 8 },
            Fate::Lost,
        )]),
        settled(vec![fate(FileChangeKind::SetLen { len: 9 }, Fate::Lost)]),
        settled(vec![fate(FileChangeKind::SetLen { len: 8 }, Fate::Lost)]),
        Event::PowerOff { after_op: 7 },
        Event::PowerOff { after_op: 8 },
        crash(1, 7, vec![], vec![]),
        crash(2, 7, vec![], vec![]),
        crash(1, 8, vec![], vec![]),
        crash(1, 7, vec![], vec![fate(write, Fate::Lost)]),
        crash(1, 7, vec![name(NameChangeKind::Create, None, true)], vec![]),
        crash(
            1,
            7,
            vec![name(NameChangeKind::Create, None, false)],
            vec![],
        ),
        crash(
            1,
            7,
            vec![name(NameChangeKind::Rename, Some("/b"), true)],
            vec![],
        ),
        crash(
            1,
            7,
            vec![name(NameChangeKind::Link, Some("/b"), true)],
            vec![],
        ),
        crash(
            1,
            7,
            vec![name(NameChangeKind::Link, Some("/c"), true)],
            vec![],
        ),
        crash(1, 7, vec![name(NameChangeKind::Remove, None, true)], vec![]),
        crash(
            1,
            7,
            vec![name(NameChangeKind::CreateDir, None, true)],
            vec![],
        ),
        crash(
            1,
            7,
            vec![name(NameChangeKind::RemoveDir, None, true)],
            vec![],
        ),
    ];
    let failures = [
        ErrorKind::NotFound,
        ErrorKind::AlreadyExists,
        ErrorKind::ResourceBusy,
        ErrorKind::IsADirectory,
        ErrorKind::NotADirectory,
        ErrorKind::DirectoryNotEmpty,
        ErrorKind::WouldBlock,
        ErrorKind::UnexpectedEof,
        ErrorKind::InvalidInput,
        ErrorKind::FileTooLarge,
        ErrorKind::PermissionDenied,
        ErrorKind::Other,
        ErrorKind::TimedOut,
    ];
    events.extend(
        failures
            .into_iter()
            .map(|kind| op(&|record| record.outcome = Outcome::Failed(kind))),
    );

    let digests: BTreeSet<u64> = events
        .iter()
        .map(|event| digest(std::slice::from_ref(event)))
        .collect();
    assert_eq!(digests.len(), events.len());
    assert_ne!(
        digest(&events[..2]),
        digest(&[events[1].clone(), events[0].clone()])
    );
    assert_eq!(digest(&events), digest(&events.clone()));
}

#[test]
fn changes_and_tears_print_what_they_are() {
    let shown = |sectors: &[Range<u64>], len| {
        Tear {
            sectors: sectors.to_vec(),
            len,
        }
        .to_string()
    };
    assert_eq!(shown(&[], 0), "sectors none, length 0");
    assert_eq!(shown(&[0..2], 1_024), "sectors 0..2, length 1024");
    assert_eq!(shown(&[0..1, 3..5], 0), "sectors 0..1, 3..5, length 0");

    let kinds = [
        (NameChangeKind::Create, None, "#4 create /a"),
        (NameChangeKind::Link, Some("/b"), "#4 hard_link /a -> /b"),
        (NameChangeKind::Remove, None, "#4 remove_file /a"),
        (NameChangeKind::Rename, Some("/b"), "#4 rename /a -> /b"),
        (NameChangeKind::CreateDir, None, "#4 create_dir /a"),
        (NameChangeKind::RemoveDir, None, "#4 remove_dir /a"),
    ];
    for (kind, to, expected) in kinds {
        let change = NameChange {
            op: 4,
            kind,
            path: "/a".to_owned(),
            to: to.map(str::to_owned),
        };
        assert_eq!(change.to_string(), expected);
    }
    assert_eq!(
        FileChangeKind::Write { offset: 3, len: 4 }.to_string(),
        "write offset 3 len 4"
    );
    assert_eq!(FileChangeKind::SetLen { len: 9 }.to_string(), "set_len 9");
    assert_eq!(Fault::Enospc.to_string(), "ENOSPC");
    assert_eq!(Fault::Eio.to_string(), "EIO");
}
