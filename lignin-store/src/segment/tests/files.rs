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
    assert_eq!(names.file_name(id(u32::MAX)), "4294967295.seg");
}

#[test]
fn a_name_parses_to_the_id_that_gives_that_name() {
    let noise = random(4000, 3);
    let samples = [1, 2, 9, 10, 310, 99_999_999, 100_000_000, u32::MAX - 1]
        .into_iter()
        .chain([u32::MAX])
        .chain(
            noise
                .chunks(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()).max(1)),
        );
    for raw in samples {
        for names in ALL {
            let name = names.file_name(id(raw));
            assert_eq!(names.parse(OsStr::new(&name)), Some(id(raw)), "{name}");
        }
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
fn a_directory_is_listed_made_if_missing_and_synced_either_way() {
    for names in ALL {
        let dir = tempfile::tempdir().unwrap();
        let vfs = faulty_std();
        let kind_dir = dir_name(&names.dir(dir.path()));
        let synced = (Op::SyncDir, dir_name(dir.path()));

        assert_eq!(list_or_create_dir(&vfs, dir.path(), &names).unwrap(), []);
        assert_eq!(
            vfs.take_calls(),
            [
                (Op::ReadDir, kind_dir.clone()),
                (Op::CreateDir, kind_dir.clone()),
                synced.clone(),
            ]
        );

        create_file(&vfs, dir.path(), &names, id(3)).unwrap();
        vfs.take_calls();
        let listed = list_or_create_dir(&vfs, dir.path(), &names).unwrap();
        assert_eq!(names.parse(&listed[0].name), Some(id(3)));
        assert_eq!(listed.len(), 1);
        assert_eq!(
            vfs.take_calls(),
            [(Op::ReadDir, kind_dir.clone()), synced.clone()]
        );

        for (op, on, expected_op) in [
            (Op::ReadDir, &kind_dir, "list"),
            (Op::SyncDir, &synced.1, "sync the directory"),
        ] {
            vfs.fail(op, on);
            let error = list_or_create_dir(&vfs, dir.path(), &names).unwrap_err();
            assert_io(&error, expected_op, on);
        }
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
