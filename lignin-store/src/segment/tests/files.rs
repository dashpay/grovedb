use std::ffi::OsStr;

use lignin_io::{Create, OpenMode};

use super::*;

const ALL: [FileNames; 4] = [
    FileNames::WAL,
    FileNames::SEGMENT,
    FileNames::VALUE_LOG,
    FileNames::PAGE_TABLE,
];

#[test]
fn names_are_the_ones_the_plan_lists() {
    let db = Path::new("db");
    assert_eq!(
        FileNames::WAL.path(db, id(45)),
        Path::new("db/wal/00000045.log")
    );
    assert_eq!(
        FileNames::SEGMENT.path(db, id(310)),
        Path::new("db/seg/00000310.seg")
    );
    assert_eq!(
        FileNames::VALUE_LOG.path(db, id(12)),
        Path::new("db/val/00000012.val")
    );
    assert_eq!(
        FileNames::PAGE_TABLE.path(db, id(122)),
        Path::new("db/table/00000122.pt")
    );
    assert_eq!(FileNames::SEGMENT.dir(db), Path::new("db/seg"));
    assert_eq!(
        ALL.map(|names| names.kind()),
        [
            FileKind::Wal,
            FileKind::Segment,
            FileKind::ValueLog,
            FileKind::PageTable
        ]
    );
}

#[test]
fn an_id_is_padded_to_eight_digits_and_written_in_full_above_that() {
    let names = FileNames::SEGMENT;
    assert_eq!(names.file_name(id(1)), "00000001.seg");
    assert_eq!(names.file_name(id(99_999_999)), "99999999.seg");
    assert_eq!(names.file_name(id(100_000_000)), "100000000.seg");
    assert_eq!(names.file_name(FileId::MAX), "4294967294.seg");
}

#[test]
fn a_name_parses_to_the_id_that_gives_that_name() {
    let noise = random(4000, 3);
    let samples = [1, 2, 9, 10, 310, 99_999_999, 100_000_000, u32::MAX - 2]
        .into_iter()
        .chain([FileId::MAX.get()])
        .chain(noise.chunks(4).map(|chunk| {
            u32::from_le_bytes(chunk.try_into().unwrap()).clamp(1, FileId::MAX.get())
        }));
    for raw in samples {
        for names in ALL {
            let name = names.file_name(id(raw));
            assert_eq!(names.parse(OsStr::new(&name)), Some(id(raw)), "{name}");
        }
    }
    // The reserved id names no file, so the name it would give is not the
    // name of a file of the store.
    for names in ALL {
        let last = names.file_name(FileId::MAX);
        let reserved = last.replace("4294967294", "4294967295");
        assert_ne!(reserved, last);
        assert_eq!(names.parse(OsStr::new(&reserved)), None, "{reserved}");
    }
}

#[test]
fn no_other_spelling_names_a_file() {
    let names = FileNames::SEGMENT;
    for name in [
        "",
        ".seg",
        "seg",
        "00000310",
        "00000310.",
        "00000310seg",
        "310.seg",
        "0000310.seg",
        "000000310.seg",
        "0100000000.seg",
        "00000000.seg",
        "000000000.seg",
        "4294967296.seg",
        "99999999999999999999.seg",
        "00000310.SEG",
        "00000310.log",
        "00000310.seg.tmp",
        "00000310.seg ",
        " 0000310.seg",
        "+0000310.seg",
        "-0000310.seg",
        "0000031a.seg",
        "0x000310.seg",
        "００００031０.seg",
        "seg/00000310.seg",
        "MANIFEST",
    ] {
        assert_eq!(names.parse(OsStr::new(name)), None, "{name:?}");
    }
    assert_eq!(FileNames::WAL.parse(OsStr::new("00000310.seg")), None);
    assert_eq!(
        FileNames::PAGE_TABLE.parse(OsStr::new("00000310.pt")),
        Some(id(310))
    );

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(names.parse(OsStr::from_bytes(b"0000031\xFF.seg")), None);
    }
}

#[test]
fn a_directory_is_listed_and_made_if_missing_and_synced_when_it_was_made() {
    for names in ALL {
        let dir = tempfile::tempdir().unwrap();
        let vfs = faulty_std();
        let kind_dir = dir_name(&names.dir(dir.path()));
        let listed = (Op::ReadDir, kind_dir.clone());
        let synced = (Op::SyncDir, dir_name(dir.path()));
        // What the manifest store of the directory would hand out: its
        // open has synced the database directory.
        let dir_sync = DirSync::new();
        let list = || list_or_create_dir(&vfs, dir.path(), &names, &dir_sync);

        assert_eq!(list().unwrap(), []);
        assert_eq!(
            vfs.take_calls(),
            [
                listed.clone(),
                (Op::CreateDir, kind_dir.clone()),
                synced.clone(),
            ]
        );

        // The directory is there, and its name is durable: nothing is
        // synced for it again.
        create_file(&vfs, dir.path(), &names, id(3)).unwrap();
        vfs.take_calls();
        let entries = list().unwrap();
        assert_eq!(names.parse(&entries[0].name), Some(id(3)));
        assert_eq!(entries.len(), 1);
        assert_eq!(vfs.take_calls(), std::slice::from_ref(&listed));

        vfs.fail(Op::ReadDir, &kind_dir);
        assert_io(&list().unwrap_err(), "list", &kind_dir);
        vfs.take_calls();
    }
}

#[test]
fn a_directory_whose_sync_failed_is_synced_by_the_next_listing() {
    for names in ALL {
        let dir = tempfile::tempdir().unwrap();
        let vfs = faulty_std();
        let kind_dir = dir_name(&names.dir(dir.path()));
        let listed = (Op::ReadDir, kind_dir.clone());
        let created = (Op::CreateDir, kind_dir.clone());
        let synced = (Op::SyncDir, dir_name(dir.path()));
        let dir_sync = DirSync::new();
        let list = |kind: &FileNames| list_or_create_dir(&vfs, dir.path(), kind, &dir_sync);

        // The directory is made and the sync of its name fails: the name
        // is there and is not durable.
        vfs.fail(Op::SyncDir, &synced.1);
        assert_io(&list(&names).unwrap_err(), "sync the directory", &synced.1);
        assert_eq!(
            vfs.take_calls(),
            [listed.clone(), created.clone(), synced.clone()]
        );
        // The next listing finds the directory and still owes the sync,
        // and the one after that owes nothing.
        assert_eq!(list(&names).unwrap(), []);
        assert_eq!(vfs.take_calls(), [listed.clone(), synced.clone()]);
        assert_eq!(list(&names).unwrap(), []);
        assert_eq!(vfs.take_calls(), std::slice::from_ref(&listed));

        // A creation that fails may have made the directory all the same,
        // so the sync is owed after it as well, and the listing of another
        // kind's directory makes it.
        let other = ALL.into_iter().find(|other| *other != names).unwrap();
        let other_dir = dir_name(&other.dir(dir.path()));
        vfs.fail(Op::CreateDir, &other_dir);
        assert_io(
            &list(&other).unwrap_err(),
            "create the directory",
            &other_dir,
        );
        vfs.take_calls();
        assert_eq!(list(&names).unwrap(), []);
        assert_eq!(vfs.take_calls(), [listed, synced]);
    }
}

#[test]
fn a_created_file_holds_its_header_and_is_durable_before_it_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    for names in ALL {
        fs::create_dir(names.dir(dir.path())).unwrap();
        vfs.take_calls();
        let file = create_file(&vfs, dir.path(), &names, id(7)).unwrap();

        let name = names.file_name(id(7));
        assert_eq!(
            vfs.take_calls(),
            [
                (Op::Open, name.clone()),
                (Op::Append, name.clone()),
                (Op::SyncData, name.clone()),
                (Op::SyncDir, dir_name(&names.dir(dir.path()))),
            ]
        );
        let path = names.path(dir.path(), id(7));
        assert_eq!(fs::read(&path).unwrap(), header(names.kind(), 7));
        assert_eq!(
            file.append(b"next").unwrap(),
            FileHeader::ENCODED_LEN as u64
        );
    }
}

#[test]
fn creating_a_file_that_exists_fails_and_leaves_it() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    fs::create_dir(dir.path().join("seg")).unwrap();
    let path = FileNames::SEGMENT.path(dir.path(), id(3));
    fs::write(&path, b"already here").unwrap();

    let error = create_file(&vfs, dir.path(), &FileNames::SEGMENT, id(3)).unwrap_err();
    match error {
        SegmentStoreError::Io { op, source, .. } => {
            assert_eq!(op, "create");
            assert_eq!(source.kind(), std::io::ErrorKind::AlreadyExists);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"already here");
}

#[test]
fn creating_a_file_without_its_directory_fails() {
    let dir = tempfile::tempdir().unwrap();
    let error = create_file(&faulty_std(), dir.path(), &FileNames::SEGMENT, id(3)).unwrap_err();
    assert!(matches!(
        error,
        SegmentStoreError::Io { op: "create", source, .. } if source.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn a_file_whose_creation_fails_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    fs::create_dir(dir.path().join("seg")).unwrap();
    let name = FileNames::SEGMENT.file_name(id(1));

    for (op, on, expected_op) in [
        (Op::Append, name.as_str(), "append to"),
        (Op::SyncData, name.as_str(), "sync"),
        (Op::SyncDir, "seg", "sync the directory"),
    ] {
        vfs.fail(op, on);
        let error = create_file(&vfs, dir.path(), &FileNames::SEGMENT, id(1)).unwrap_err();
        assert_io(&error, expected_op, on);
        assert_eq!(names(&dir.path().join("seg")), [""; 0], "{op:?}");
    }

    let file = create_file(&vfs, dir.path(), &FileNames::SEGMENT, id(1)).unwrap();
    assert_eq!(file.len().unwrap(), FileHeader::ENCODED_LEN as u64);
    drop(file);
    let _ = vfs
        .open(
            &FileNames::SEGMENT.path(dir.path(), id(1)),
            OpenMode::Append(Create::Never),
        )
        .expect("no append handle is left open");
}
