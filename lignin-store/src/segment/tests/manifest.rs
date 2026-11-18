use std::io::ErrorKind;

use lignin_format::manifest::{ManifestError, MAX_LIVE_FILES};

use super::{super::manifest::MAX_FILE_LEN, *};

const TEMPORARY: &str = "MANIFEST.tmp";
const CLOSED: FileState = FileState::Closed;
const OPEN: FileState = FileState::Open;
const SEALED: FileState = FileState::Sealed;
const LAST: [u32; 4] = [45, 310, 12, 122];

fn new_store(manifest: Manifest) -> (FaultyVfs, ManifestStore, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    let store = ManifestStore::create(Arc::new(vfs.clone()), dir.path(), manifest).unwrap();
    vfs.take_calls();
    (vfs, store, dir)
}

fn open_store(vfs: &FaultyVfs, dir: &Path) -> Result<ManifestStore, SegmentStoreError> {
    ManifestStore::open(Arc::new(vfs.clone()), dir)
}

fn full_files() -> Vec<LiveFile> {
    vec![
        live(FileKind::Wal, 44, CLOSED, 1_048_576),
        live(FileKind::Wal, 45, OPEN, 20),
        segment(309, SEALED, 67_108_864),
        segment(310, OPEN, 123_456),
        live(FileKind::ValueLog, 12, CLOSED, 5_000_000_000),
    ]
}

fn full() -> Manifest {
    manifest_of(LAST, Some(table(122, 4116, 9000)), full_files())
}

fn with_file(replacement: LiveFile) -> Manifest {
    let files = full_files()
        .into_iter()
        .map(|file| {
            if (file.kind(), file.id()) == (replacement.kind(), replacement.id()) {
                replacement
            } else {
                file
            }
        })
        .collect();
    manifest_of(LAST, Some(table(122, 4116, 9000)), files)
}

fn generations() -> [Manifest; 4] {
    [
        manifest_of([0, 1, 0, 0], None, vec![segment(1, OPEN, 20)]),
        manifest_of([0, 1, 0, 0], None, vec![segment(1, OPEN, 4096)]),
        manifest_of(
            [1, 2, 0, 1],
            Some(table(1, 500, 7)),
            vec![
                live(FileKind::Wal, 1, OPEN, 20),
                segment(1, SEALED, 5000),
                segment(2, OPEN, 20),
            ],
        ),
        manifest_of(
            [1, 2, 0, 1],
            Some(table(1, 500, 7)),
            vec![segment(2, CLOSED, 28)],
        ),
    ]
}

#[test]
fn the_largest_manifest_is_as_long_as_this_module_expects() {
    assert_eq!(empty_manifest().encode().unwrap().len(), 64);
    let one = manifest_of([0, 1, 0, 0], None, vec![segment(1, OPEN, 20)]);
    assert_eq!(one.encode().unwrap().len(), 64 + 16);

    let files = (1..=MAX_LIVE_FILES as u32)
        .map(|raw| segment(raw, OPEN, 20))
        .collect();
    let largest = manifest_of([0, MAX_LIVE_FILES as u32, 0, 0], None, files);
    assert_eq!(largest.encode().unwrap().len(), MAX_FILE_LEN);
}

#[test]
fn create_writes_the_first_manifest_and_open_reads_it_back() {
    for manifest in [empty_manifest(), full()] {
        let (vfs, store, dir) = new_store(manifest.clone());
        assert_eq!(store.manifest().unwrap(), &manifest);
        assert_eq!(store.dir(), dir.path());
        assert_eq!(names(dir.path()), ["MANIFEST"]);
        assert_eq!(
            fs::read(dir.path().join("MANIFEST")).unwrap(),
            manifest.encode().unwrap()
        );
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), manifest);
        assert_eq!(read_manifest(&**store.vfs(), dir.path()).unwrap(), manifest);

        drop(store);
        let store = open_store(&vfs, dir.path()).unwrap();
        assert_eq!(store.manifest().unwrap(), &manifest);
    }
}

#[test]
fn create_refuses_a_directory_that_has_a_manifest() {
    let (vfs, store, dir) = new_store(full());
    drop(store);
    let error =
        ManifestStore::create(Arc::new(vfs.clone()), dir.path(), empty_manifest()).unwrap_err();
    assert!(
        matches!(error, SegmentStoreError::ManifestExists { dir: found } if found == dir.path())
    );
    assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), full());
}

#[test]
fn a_directory_without_a_manifest_does_not_open() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    fs::write(dir.path().join(TEMPORARY), full().encode().unwrap()).unwrap();
    for missing in [dir.path().to_path_buf(), dir.path().join("absent")] {
        let error = open_store(&vfs, &missing).unwrap_err();
        assert!(matches!(error, SegmentStoreError::NoManifest { dir } if dir == missing));
        let error = read_manifest(&vfs, &missing).unwrap_err();
        assert!(matches!(error, SegmentStoreError::NoManifest { dir } if dir == missing));
    }
    assert_eq!(names(dir.path()), [TEMPORARY]);
}

#[test]
fn replace_writes_a_temporary_file_syncs_it_renames_it_and_syncs_the_directory() {
    let (vfs, mut store, dir) = new_store(empty_manifest());
    let [first, ..] = generations();
    replace(&mut store, first.clone()).unwrap();

    let temporary = TEMPORARY.to_owned();
    assert_eq!(
        vfs.take_calls(),
        [
            (Op::RemoveFile, temporary.clone()),
            (Op::Open, temporary.clone()),
            (Op::Write, temporary.clone()),
            (Op::SyncAll, temporary.clone()),
            (Op::Rename, temporary),
            (Op::SyncDir, dir_name(dir.path())),
        ]
    );
    assert_eq!(store.manifest().unwrap(), &first);
}

#[test]
fn each_replacement_becomes_the_manifest_on_disk() {
    let (vfs, mut store, dir) = new_store(empty_manifest());
    for manifest in generations() {
        replace(&mut store, manifest.clone()).unwrap();
        assert_eq!(store.manifest().unwrap(), &manifest);
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), manifest);
        assert_eq!(names(dir.path()), ["MANIFEST"]);
    }
    drop(store);
    let [.., last] = generations();
    assert_eq!(
        open_store(&vfs, dir.path()).unwrap().manifest().unwrap(),
        &last
    );
}

#[test]
fn an_equal_manifest_is_not_written_again() {
    let (vfs, mut store, _dir) = new_store(full());
    replace(&mut store, full()).unwrap();
    assert_eq!(vfs.take_calls(), []);
    assert_eq!(store.manifest().unwrap(), &full());
}

#[test]
fn open_makes_the_manifest_durable_and_removes_a_stale_temporary_file() {
    let (vfs, store, dir) = new_store(full());
    drop(store);
    fs::write(dir.path().join(TEMPORARY), b"left by a crash").unwrap();

    let store = open_store(&vfs, dir.path()).unwrap();
    let manifest = "MANIFEST".to_owned();
    assert_eq!(
        vfs.take_calls(),
        [
            (Op::Open, manifest.clone()),
            (Op::Len, manifest.clone()),
            (Op::Read, manifest),
            (Op::SyncDir, dir_name(dir.path())),
            (Op::RemoveFile, TEMPORARY.to_owned()),
        ]
    );
    assert_eq!(store.manifest().unwrap(), &full());
    assert_eq!(names(dir.path()), ["MANIFEST"]);
}

#[test]
fn a_stale_temporary_file_does_not_stop_a_replacement_or_a_first_manifest() {
    let (vfs, mut store, dir) = new_store(empty_manifest());
    let [first, second, third, _] = generations();
    let stale = [first.encode().unwrap(), vec![0xAB; 100_000], Vec::new()];
    for (content, next) in stale.iter().zip([first, second.clone(), third]) {
        fs::write(dir.path().join(TEMPORARY), content).unwrap();
        replace(&mut store, next.clone()).unwrap();
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), next);
        assert_eq!(names(dir.path()), ["MANIFEST"]);
    }

    let fresh = tempfile::tempdir().unwrap();
    fs::write(fresh.path().join(TEMPORARY), second.encode().unwrap()).unwrap();
    let store = ManifestStore::create(Arc::new(vfs.clone()), fresh.path(), full()).unwrap();
    assert_eq!(store.manifest().unwrap(), &full());
    assert_eq!(read_manifest(&vfs, fresh.path()).unwrap(), full());
    assert_eq!(names(fresh.path()), ["MANIFEST"]);
}

#[test]
fn reading_a_manifest_changes_nothing() {
    let (vfs, store, dir) = new_store(full());
    drop(store);
    fs::write(dir.path().join(TEMPORARY), b"left by a crash").unwrap();
    assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), full());
    let manifest = "MANIFEST".to_owned();
    assert_eq!(
        vfs.take_calls(),
        [
            (Op::Open, manifest.clone()),
            (Op::Len, manifest.clone()),
            (Op::Read, manifest)
        ]
    );
    assert_eq!(names(dir.path()), ["MANIFEST", TEMPORARY]);
}

#[test]
fn a_failure_before_the_rename_keeps_the_old_manifest_and_can_be_retried() {
    let [first, second, ..] = generations();
    for (op, expected_op) in [
        (Op::RemoveFile, "remove"),
        (Op::Open, "create"),
        (Op::Write, "write"),
        (Op::SyncAll, "sync"),
    ] {
        let (vfs, mut store, dir) = new_store(first.clone());
        vfs.fail(op, TEMPORARY);
        let error = replace(&mut store, second.clone()).unwrap_err();
        assert_io(&error, expected_op, TEMPORARY);

        assert_eq!(store.manifest().unwrap(), &first, "{op:?}");
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), first);
        assert_eq!(names(dir.path()), ["MANIFEST"], "{op:?}");

        replace(&mut store, second.clone()).unwrap();
        assert_eq!(store.manifest().unwrap(), &second);
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), second);
        assert_eq!(names(dir.path()), ["MANIFEST"]);
    }
}

#[test]
fn a_failed_rename_or_directory_sync_leaves_the_manifest_in_doubt_until_it_is_opened_again() {
    let [first, second, third, _] = generations();
    for (op, expected_op, survivor) in [
        (Op::Rename, "rename", &first),
        (Op::SyncDir, "sync the directory", &second),
    ] {
        let (vfs, mut store, dir) = new_store(first.clone());
        let on = if op == Op::Rename {
            TEMPORARY.to_owned()
        } else {
            dir_name(dir.path())
        };
        vfs.fail(op, &on);
        let error = replace(&mut store, second.clone()).unwrap_err();
        assert_io(&error, expected_op, &on);
        vfs.take_calls();

        let in_doubt = |error| matches!(error, SegmentStoreError::ManifestInDoubt { dir: found } if found == dir.path());
        assert!(in_doubt(store.manifest().unwrap_err()), "{op:?}");
        assert!(in_doubt(replace(&mut store, second.clone()).unwrap_err()));
        assert!(in_doubt(replace(&mut store, first.clone()).unwrap_err()));
        assert!(in_doubt(SegmentStore::open(&store).unwrap_err()));
        assert_eq!(vfs.take_calls(), [], "a manifest in doubt is not acted on");

        drop(store);
        let mut store = open_store(&vfs, dir.path()).unwrap();
        assert_eq!(store.manifest().unwrap(), survivor, "{op:?}");
        assert_eq!(names(dir.path()), ["MANIFEST"]);
        replace(&mut store, second.clone()).unwrap();
        replace(&mut store, third.clone()).unwrap();
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), third);
    }
}

#[test]
fn a_first_manifest_that_fails_leaves_none_or_a_whole_one() {
    for op in [
        Op::RemoveFile,
        Op::Open,
        Op::Write,
        Op::SyncAll,
        Op::Rename,
        Op::SyncDir,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let vfs = faulty_std();
        let create = || ManifestStore::create(Arc::new(vfs.clone()), dir.path(), full());
        let on = if op == Op::SyncDir {
            dir_name(dir.path())
        } else {
            TEMPORARY.to_owned()
        };
        vfs.fail(op, &on);
        assert!(matches!(
            create().unwrap_err(),
            SegmentStoreError::Io { .. }
        ));

        if op == Op::SyncDir {
            assert!(matches!(
                create().unwrap_err(),
                SegmentStoreError::ManifestExists { .. }
            ));
        } else {
            assert!(!dir.path().join("MANIFEST").exists(), "{op:?}");
            assert_eq!(create().unwrap().manifest().unwrap(), &full());
        }
        assert_eq!(
            open_store(&vfs, dir.path()).unwrap().manifest().unwrap(),
            &full()
        );
        assert_eq!(names(dir.path()), ["MANIFEST"], "{op:?}");
    }
}

#[test]
fn open_reports_what_the_file_layer_refuses() {
    for (op, on_directory, expected_op) in [
        (Op::Open, false, "open"),
        (Op::Len, false, "measure"),
        (Op::Read, false, "read"),
        (Op::SyncDir, true, "sync the directory"),
    ] {
        let (vfs, store, dir) = new_store(full());
        drop(store);
        let on = if on_directory {
            dir_name(dir.path())
        } else {
            "MANIFEST".to_owned()
        };
        vfs.fail(op, &on);
        assert_io(&open_store(&vfs, dir.path()).unwrap_err(), expected_op, &on);
        assert_eq!(
            open_store(&vfs, dir.path()).unwrap().manifest().unwrap(),
            &full()
        );
    }

    let (vfs, store, dir) = new_store(full());
    drop(store);
    fs::write(dir.path().join(TEMPORARY), b"stale").unwrap();
    vfs.fail(Op::RemoveFile, TEMPORARY);
    assert_io(
        &open_store(&vfs, dir.path()).unwrap_err(),
        "remove",
        TEMPORARY,
    );

    let (vfs, store, dir) = new_store(full());
    drop(store);
    vfs.fail(Op::Open, "MANIFEST");
    let error = ManifestStore::create(Arc::new(vfs.clone()), dir.path(), full()).unwrap_err();
    assert_io(&error, "open", "MANIFEST");
}

#[test]
fn a_directory_named_manifest_is_not_a_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    fs::create_dir(dir.path().join("MANIFEST")).unwrap();
    let is_a_directory = |error| {
        matches!(
            error,
            SegmentStoreError::Io { op: "open", source, .. } if source.kind() == ErrorKind::IsADirectory
        )
    };
    assert!(is_a_directory(open_store(&vfs, dir.path()).unwrap_err()));
    assert!(is_a_directory(read_manifest(&vfs, dir.path()).unwrap_err()));
    let created = ManifestStore::create(Arc::new(vfs.clone()), dir.path(), full());
    assert!(is_a_directory(created.unwrap_err()));
}

#[test]
fn a_damaged_manifest_does_not_open() {
    let bytes = full().encode().unwrap();
    let mut flipped = bytes.clone();
    flipped[40] ^= 0x10;
    let mut longer = bytes.clone();
    longer.push(0);
    type Check = fn(&FormatError) -> bool;
    let cases: [(Vec<u8>, Check); 6] = [
        (Vec::new(), |error| {
            matches!(
                error,
                FormatError::Truncated {
                    needed: 20,
                    available: 0
                }
            )
        }),
        (bytes[..bytes.len() - 1].to_vec(), |error| {
            matches!(error, FormatError::Truncated { .. })
        }),
        (flipped, |error| {
            matches!(error, FormatError::ChecksumMismatch { .. })
        }),
        (longer, |error| {
            matches!(
                error,
                FormatError::Manifest(ManifestError::LengthMismatch { .. })
            )
        }),
        (header(FileKind::Segment, 1).to_vec(), |error| {
            matches!(
                error,
                FormatError::Manifest(ManifestError::NotAManifest {
                    found: FileKind::Segment
                })
            )
        }),
        (vec![0; 4096], |error| {
            matches!(error, FormatError::BadMagic)
        }),
    ];
    for (content, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let vfs = faulty_std();
        fs::write(dir.path().join("MANIFEST"), &content).unwrap();
        for error in [
            open_store(&vfs, dir.path()).unwrap_err(),
            read_manifest(&vfs, dir.path()).unwrap_err(),
        ] {
            let SegmentStoreError::Format {
                path,
                offset,
                source,
            } = error
            else {
                panic!("{error:?}");
            };
            assert_eq!((path, offset), (dir.path().join("MANIFEST"), 0));
            assert!(expected(&source), "{source:?}");
        }
        assert_eq!(fs::read(dir.path().join("MANIFEST")).unwrap(), content);
    }
}

#[test]
fn no_more_of_a_manifest_is_read_than_the_longest_one_and_a_byte() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    let mut content = full().encode().unwrap();
    content.resize(MAX_FILE_LEN + 4096, 0);
    fs::write(dir.path().join("MANIFEST"), &content).unwrap();

    let (_, error) = format_error(read_manifest(&vfs, dir.path()).unwrap_err());
    assert_eq!(
        error,
        FormatError::Manifest(ManifestError::LengthMismatch {
            expected: full().encode().unwrap().len() as u64,
            found: MAX_FILE_LEN as u64 + 1,
        })
    );
}

#[test]
fn a_replacement_may_do_what_the_life_of_a_file_allows() {
    let table = Some(table(122, 4116, 9000));
    let mut without_two = full_files();
    without_two.retain(|file| ![44, 309].contains(&file.id().get()));
    let mut with_new = full_files();
    with_new.extend([live(FileKind::Wal, 46, OPEN, 20), segment(312, OPEN, 20)]);

    let allowed = [
        with_file(segment(310, OPEN, 200_000)),
        with_file(segment(310, CLOSED, 123_456)),
        with_file(segment(310, CLOSED, 200_000)),
        with_file(segment(310, SEALED, 200_000)),
        with_file(live(FileKind::Wal, 45, CLOSED, 20)),
        with_file(live(FileKind::Wal, 45, CLOSED, 9000)),
        manifest_of(LAST, table, without_two),
        manifest_of(LAST, table, Vec::new()),
        manifest_of([46, 312, 12, 122], table, with_new),
        manifest_of([50, 320, 13, 130], table, full_files()),
        manifest_of(
            [45, 310, 12, 123],
            Some(super::table(123, 20, 9001)),
            full_files(),
        ),
        manifest_of(
            [45, 310, 12, 130],
            Some(super::table(130, 8000, 9000)),
            full_files(),
        ),
        manifest_of(LAST, None, full_files()),
    ];
    for next in allowed {
        let (vfs, mut store, dir) = new_store(full());
        replace(&mut store, next.clone()).unwrap();
        assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), next);
    }

    let (vfs, mut store, dir) = new_store(manifest_of(LAST, None, full_files()));
    let named_again = manifest_of(
        [45, 310, 12, 123],
        Some(super::table(123, 64, 1)),
        full_files(),
    );
    replace(&mut store, named_again.clone()).unwrap();
    assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), named_again);
}

#[test]
fn a_replacement_that_breaks_the_life_of_a_file_is_refused_and_writes_nothing() {
    let table = Some(table(122, 4116, 9000));
    let without = |kind: FileKind, raw_id: u32| {
        let mut files = full_files();
        files.retain(|file| (file.kind(), file.id()) != (kind, id(raw_id)));
        files
    };
    let with_extra = |extra: LiveFile| {
        let mut files = full_files();
        files.push(extra);
        files
    };
    let reused = |kind, raw_id| ManifestChange::FileIdReused {
        kind,
        id: id(raw_id),
    };
    let shortened = |kind, raw_id| ManifestChange::FileShortened {
        kind,
        id: id(raw_id),
    };
    let finished = |kind, raw_id| ManifestChange::FinishedFileChanged {
        kind,
        id: id(raw_id),
    };
    let (wal, seg, val) = (FileKind::Wal, FileKind::Segment, FileKind::ValueLog);

    let refused = [
        (
            manifest_of([44, 310, 12, 122], table, without(wal, 45)),
            ManifestChange::LastFileIdLowered { kind: wal },
        ),
        (
            manifest_of([45, 309, 12, 122], table, without(seg, 310)),
            ManifestChange::LastFileIdLowered { kind: seg },
        ),
        (
            manifest_of([45, 310, 0, 122], table, without(val, 12)),
            ManifestChange::LastFileIdLowered { kind: val },
        ),
        (
            manifest_of([45, 310, 12, 121], None, full_files()),
            ManifestChange::LastFileIdLowered {
                kind: FileKind::PageTable,
            },
        ),
        (
            manifest_of(LAST, table, with_extra(live(wal, 43, CLOSED, 20))),
            reused(wal, 43),
        ),
        (
            manifest_of(LAST, table, with_extra(segment(308, SEALED, 48))),
            reused(seg, 308),
        ),
        (
            manifest_of(LAST, table, with_extra(live(val, 11, OPEN, 20))),
            reused(val, 11),
        ),
        (
            manifest_of(LAST, Some(super::table(121, 4116, 9000)), full_files()),
            reused(FileKind::PageTable, 121),
        ),
        (with_file(segment(310, OPEN, 100_000)), shortened(seg, 310)),
        (with_file(segment(310, CLOSED, 28)), shortened(seg, 310)),
        (with_file(segment(310, SEALED, 48)), shortened(seg, 310)),
        (
            with_file(segment(309, SEALED, 67_108_863)),
            shortened(seg, 309),
        ),
        (with_file(live(wal, 44, CLOSED, 20)), shortened(wal, 44)),
        (with_file(live(wal, 44, OPEN, 1_048_576)), finished(wal, 44)),
        (
            with_file(live(wal, 44, CLOSED, 1_048_577)),
            finished(wal, 44),
        ),
        (
            with_file(segment(309, CLOSED, 67_108_864)),
            finished(seg, 309),
        ),
        (
            with_file(segment(309, OPEN, 67_108_864)),
            finished(seg, 309),
        ),
        (
            with_file(segment(309, SEALED, 67_108_865)),
            finished(seg, 309),
        ),
        (
            with_file(live(val, 12, OPEN, 5_000_000_000)),
            finished(val, 12),
        ),
        (
            manifest_of(LAST, Some(super::table(122, 4117, 9000)), full_files()),
            ManifestChange::TableCheckpointChanged { id: id(122) },
        ),
        (
            manifest_of(LAST, Some(super::table(122, 4116, 9001)), full_files()),
            ManifestChange::TableCheckpointChanged { id: id(122) },
        ),
    ];

    let (vfs, mut store, dir) = new_store(full());
    for (next, expected) in refused {
        match replace(&mut store, next) {
            Err(SegmentStoreError::ManifestChange(change)) => assert_eq!(change, expected),
            other => panic!("expected {expected:?}, got {other:?}"),
        }
        assert_eq!(vfs.take_calls(), [], "{expected:?}");
        assert_eq!(store.manifest().unwrap(), &full());
    }
    assert_eq!(read_manifest(&vfs, dir.path()).unwrap(), full());

    let (_vfs, mut store, _dir) = new_store(manifest_of(LAST, None, full_files()));
    let stale_table = manifest_of(LAST, Some(super::table(122, 20, 0)), full_files());
    assert!(matches!(
        replace(&mut store, stale_table),
        Err(SegmentStoreError::ManifestChange(change)) if change == reused(FileKind::PageTable, 122)
    ));
}

#[test]
fn an_update_is_made_from_the_current_manifest_and_a_failed_one_writes_nothing() {
    #[derive(Debug)]
    enum Outer {
        Store(SegmentStoreError),
        Own,
    }
    impl From<SegmentStoreError> for Outer {
        fn from(error: SegmentStoreError) -> Self {
            Self::Store(error)
        }
    }

    let (vfs, mut store, dir) = new_store(full());
    let mut seen = None;
    let own: Result<(), Outer> = store.update(|current| {
        seen = Some(current.clone());
        Err(Outer::Own)
    });
    assert!(matches!(own, Err(Outer::Own)));
    assert_eq!(seen, Some(full()));

    let lowered = manifest_of([45, 310, 12, 121], None, full_files());
    let refused: Result<(), Outer> = store.update(|_| Ok(lowered));
    assert!(matches!(
        refused,
        Err(Outer::Store(SegmentStoreError::ManifestChange(
            ManifestChange::LastFileIdLowered {
                kind: FileKind::PageTable
            }
        )))
    ));
    assert_eq!(vfs.take_calls(), []);
    assert_eq!(store.manifest().unwrap(), &full());

    let with_log = |current: &Manifest| {
        let mut files = current.files().to_vec();
        files.push(live(FileKind::Wal, 46, OPEN, 20));
        let mut last = current.last_file_ids();
        last.wal = Some(id(46));
        Ok::<_, Outer>(Manifest::new(last, current.table_checkpoint(), files).unwrap())
    };
    store.update(with_log).unwrap();
    let on_disk = read_manifest(&vfs, dir.path()).unwrap();
    assert_eq!(on_disk.files().len(), full_files().len() + 1);
    assert_eq!(on_disk.last_file_ids(), last_ids([46, 310, 12, 122]));
    assert_eq!(store.manifest().unwrap(), &on_disk);

    vfs.fail(Op::SyncDir, &dir_name(dir.path()));
    let in_doubt: Result<(), Outer> = store.update(|_| Ok(full_with_longer_segment()));
    assert!(matches!(
        in_doubt,
        Err(Outer::Store(SegmentStoreError::Io { .. }))
    ));
    let mut asked = false;
    let later: Result<(), Outer> = store.update(|current| {
        asked = true;
        Ok(current.clone())
    });
    assert!(matches!(
        later,
        Err(Outer::Store(SegmentStoreError::ManifestInDoubt { .. }))
    ));
    assert!(!asked, "a manifest in doubt is not offered as a base");
}

fn full_with_longer_segment() -> Manifest {
    let mut files = full_files();
    files.push(live(FileKind::Wal, 46, OPEN, 20));
    let files = files
        .into_iter()
        .map(|file| {
            if file.id() == id(310) {
                segment(310, OPEN, 200_000)
            } else {
                file
            }
        })
        .collect();
    manifest_of([46, 310, 12, 122], Some(table(122, 4116, 9000)), files)
}
