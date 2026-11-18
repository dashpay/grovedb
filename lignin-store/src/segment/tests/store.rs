use std::{
    io::ErrorKind,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
};

use lignin_format::{
    segment::{self as format, IndexEntry, SegmentError, DATA_START},
    Checksum,
};

use super::*;

const CLOSED: FileState = FileState::Closed;
const OPEN: FileState = FileState::Open;
const SEALED: FileState = FileState::Sealed;

/// The bytes of a segment file with one raw base block for each payload, and
/// the offset each block starts at.
fn segment_bytes(raw_id: u32, payloads: &[&[u8]], sealed: bool) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = header(FileKind::Segment, raw_id).to_vec();
    let mut offsets = Vec::new();
    let mut index = Vec::new();
    for (n, payload) in payloads.iter().enumerate() {
        offsets.push(bytes.len() as u64);
        let tag =
            format::write_block(BlockContent::Base, Compression::Raw, payload, &mut bytes).unwrap();
        index.push(IndexEntry {
            page: page(n as u32 + 1),
            tag,
        });
    }
    if sealed {
        let index_offset = bytes.len() as u64;
        format::write_footer(&index, index_offset, &mut bytes).unwrap();
    }
    (bytes, offsets)
}

fn write_segment(dir: &Path, raw_id: u32, bytes: &[u8]) {
    let seg_dir = dir.join("seg");
    if !seg_dir.exists() {
        fs::create_dir(&seg_dir).unwrap();
    }
    fs::write(FileNames::SEGMENT.path(dir, id(raw_id)), bytes).unwrap();
}

fn location(raw_id: u32, offset: u64, len: u32) -> BlockLocation {
    BlockLocation {
        file: id(raw_id),
        offset,
        len,
    }
}

fn one_segment(state: FileState, valid_len: u64) -> Manifest {
    manifest_of([0, 1, 0, 0], None, vec![segment(1, state, valid_len)])
}

fn append_to(
    writer: &mut SegmentWriter,
    blocks: &mut Vec<Appended>,
    payload: Vec<u8>,
    compression: Compression,
) {
    let location = writer
        .append(page(1), BlockContent::Base, compression, &payload)
        .unwrap();
    blocks.push(Appended {
        location,
        page: page(1),
        content: BlockContent::Base,
        payload,
    });
}

#[test]
fn a_new_database_gets_its_segment_directory_once() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = faulty_std();
    let manifest =
        ManifestStore::create(Arc::new(vfs.clone()), dir.path(), empty_manifest()).unwrap();
    vfs.take_calls();

    let segments = SegmentStore::open(&manifest).unwrap();
    let seg = "seg".to_owned();
    assert_eq!(
        vfs.take_calls(),
        [
            (Op::ReadDir, seg.clone()),
            (Op::CreateDir, seg.clone()),
            (Op::SyncDir, dir_name(dir.path())),
        ]
    );
    assert_eq!(names(dir.path()), ["MANIFEST", "seg"]);
    assert_eq!(segments.segments_named().unwrap(), []);
    assert_eq!(segments.dir(), dir.path());

    drop(segments);
    SegmentStore::open(&manifest).unwrap();
    assert_eq!(vfs.take_calls(), [(Op::ReadDir, seg)]);
}

#[test]
fn a_segment_directory_that_cannot_be_listed_or_made_fails_the_open() {
    for (op, on_seg, expected_op) in [
        (Op::ReadDir, true, "list"),
        (Op::CreateDir, true, "create the directory"),
        (Op::SyncDir, false, "sync the directory"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let vfs = faulty_std();
        let manifest =
            ManifestStore::create(Arc::new(vfs.clone()), dir.path(), empty_manifest()).unwrap();
        let on = if on_seg {
            "seg".to_owned()
        } else {
            dir_name(dir.path())
        };
        vfs.fail(op, &on);
        assert_io(
            &SegmentStore::open(&manifest).unwrap_err(),
            expected_op,
            &on,
        );
        SegmentStore::open(&manifest).unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("seg"), b"a file").unwrap();
    let manifest =
        ManifestStore::create(Arc::new(faulty_std()), dir.path(), empty_manifest()).unwrap();
    assert!(matches!(
        SegmentStore::open(&manifest).unwrap_err(),
        SegmentStoreError::Io { op: "list", source, .. } if source.kind() == ErrorKind::NotADirectory
    ));
}

#[test]
fn a_created_segment_is_its_header_and_takes_blocks_once_a_manifest_names_it() {
    let mut db = Db::new();
    let mut writer = db.segments.create().unwrap();
    let name = FileNames::SEGMENT.file_name(id(1));
    assert_eq!(
        db.vfs.take_calls(),
        [
            (Op::Open, name.clone()),
            (Op::Append, name.clone()),
            (Op::SyncData, name.clone()),
            (Op::SyncDir, "seg".to_owned()),
            (Op::Open, name),
        ]
    );
    assert_eq!(
        (writer.file_id(), writer.end(), writer.block_count()),
        (id(1), 20, 0)
    );
    assert_eq!(
        fs::read(db.seg_path(1)).unwrap(),
        header(FileKind::Segment, 1)
    );
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, 20)]
    );
    assert_eq!(db.durable(), &empty_manifest());

    let not_named = |error| matches!(error, SegmentStoreError::NotNamed { file } if file == id(1));
    let refused = writer.append(page(1), BlockContent::Base, Compression::Raw, b"early");
    assert!(not_named(refused.unwrap_err()));
    assert!(not_named(writer.seal().unwrap_err()));
    assert_eq!(writer.sync().unwrap(), 20);
    assert_eq!(fs::metadata(db.seg_path(1)).unwrap().len(), 20);

    db.commit();
    assert_eq!(db.on_disk(), one_segment(OPEN, 20));
    let blocks = append_blocks(&mut writer, 40, 1);
    assert_eq!(blocks[0].location.offset, DATA_START);
    for pair in blocks.windows(2) {
        let [before, after] = pair else {
            unreachable!()
        };
        assert_eq!(
            after.location.offset,
            before.location.offset + u64::from(before.location.len)
        );
    }
    assert_eq!(writer.block_count(), 40);
    assert_eq!(writer.end(), fs::metadata(db.seg_path(1)).unwrap().len());
    db.assert_readable(&blocks);
    db.assert_walk_gives(1, &blocks);
}

#[test]
fn segment_ids_go_up_and_several_segments_are_written_at_once() {
    let mut db = Db::new();
    let mut first = db.segments.create().unwrap();
    let mut second = db.segments.create().unwrap();
    assert_eq!((first.file_id(), second.file_id()), (id(1), id(2)));
    db.commit();
    assert_eq!(
        db.on_disk(),
        manifest_of(
            [0, 2, 0, 0],
            None,
            vec![segment(1, OPEN, 20), segment(2, OPEN, 20)]
        )
    );

    let mut blocks = Vec::new();
    for round in 0..6 {
        blocks.extend(append_blocks(&mut first, 3, round * 10));
        blocks.extend(append_blocks(&mut second, 4, round * 10 + 5));
    }
    db.assert_readable(&blocks);

    let mut third = db.writer();
    assert_eq!(third.file_id(), id(3));
    blocks.extend(append_blocks(&mut third, 5, 77));
    db.assert_readable(&blocks);
}

#[test]
fn describing_keeps_everything_but_the_segments_of_the_base_manifest() {
    let other_files = vec![
        live(FileKind::Wal, 5, OPEN, 20),
        live(FileKind::ValueLog, 3, CLOSED, 500),
    ];
    let checkpoint = Some(table(2, 100, 9));
    let mut db = Db::with_manifest(
        manifest_of([5, 40, 3, 2], checkpoint, other_files.clone()),
        |_| {},
    );
    let writer = db.segments.create().unwrap();
    assert_eq!(writer.file_id(), id(41));
    db.commit();

    let mut files = other_files.clone();
    files.push(segment(41, OPEN, 20));
    assert_eq!(db.on_disk(), manifest_of([5, 41, 3, 2], checkpoint, files));

    let stale_base = manifest_of(
        [9, 50, 3, 2],
        None,
        vec![segment(7, CLOSED, 28), live(FileKind::Wal, 9, OPEN, 20)],
    );
    assert_eq!(
        db.segments.describe(&stale_base).unwrap(),
        manifest_of(
            [9, 50, 3, 2],
            None,
            vec![live(FileKind::Wal, 9, OPEN, 20), segment(41, OPEN, 20)]
        )
    );
}

#[test]
fn a_manifest_names_what_was_synced_and_nothing_past_it() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let mut blocks = append_blocks(&mut writer, 6, 1);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, 20)]
    );

    let synced = writer.sync().unwrap();
    assert_eq!(synced, writer.end());
    blocks.extend(append_blocks(&mut writer, 6, 30));
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, synced)]
    );
    db.commit();
    assert_eq!(db.on_disk(), one_segment(OPEN, synced));

    db.segments.sync().unwrap();
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, writer.end())]
    );
    db.vfs.take_calls();
    db.segments.sync().unwrap();
    assert_eq!(writer.sync().unwrap(), writer.end());
    assert_eq!(db.vfs.take_calls(), [], "nothing new to sync");
    db.commit();
    assert_eq!(db.on_disk(), one_segment(OPEN, writer.end()));
    db.assert_readable(&blocks);
}

#[test]
fn a_segment_left_open_is_closed_at_its_valid_length_by_the_next_session() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let named = append_blocks(&mut writer, 10, 1);
    let valid_len = writer.sync().unwrap();
    db.commit();
    let unnamed = append_blocks(&mut writer, 5, 100);
    writer.sync().unwrap();
    db.assert_readable(&unnamed);
    drop(writer);
    let file_len = fs::metadata(db.seg_path(1)).unwrap().len();
    assert!(file_len > valid_len);

    let mut db = db.reopen();
    assert_eq!(db.durable(), &one_segment(OPEN, valid_len));
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, valid_len)]
    );
    db.assert_readable(&named);
    db.assert_walk_gives(1, &named);
    for block in &unnamed {
        assert!(matches!(
            db.read(block.location).unwrap_err(),
            SegmentStoreError::OutOfBounds { location, end } if location == block.location && end == valid_len
        ));
    }

    let mut writer = db.writer();
    assert_eq!(writer.file_id(), id(2));
    assert_eq!(
        db.on_disk(),
        manifest_of(
            [0, 2, 0, 0],
            None,
            vec![segment(1, CLOSED, valid_len), segment(2, OPEN, 20)]
        )
    );
    let more = append_blocks(&mut writer, 5, 200);
    db.assert_readable(&more);
    assert_eq!(fs::metadata(db.seg_path(1)).unwrap().len(), file_len);

    let db = db.reopen();
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, valid_len), segment(2, CLOSED, 20)]
    );
}

#[test]
fn a_sealed_segment_is_read_the_same_in_the_next_session() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let blocks = append_blocks(&mut writer, 25, 4);
    let sealed_len = writer.seal().unwrap();
    db.commit();
    drop(writer);

    let db = db.reopen();
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, SEALED, sealed_len)]
    );
    db.assert_readable(&blocks);
    db.assert_walk_gives(1, &blocks);
    let mut buf = Vec::new();
    let index = db.segments.index(id(1), &mut buf).unwrap();
    let listed: Vec<_> = index
        .blocks()
        .map(|block| (block.page, block.offset, block.tag.block_len() as u32))
        .collect();
    let appended: Vec<_> = blocks
        .iter()
        .map(|block| (block.page, block.location.offset, block.location.len))
        .collect();
    assert_eq!(listed, appended);
}

#[test]
fn opening_removes_segment_files_the_manifest_does_not_name_and_nothing_else() {
    let mut db = Db::new();
    let mut sealed = db.writer();
    append_blocks(&mut sealed, 3, 1);
    sealed.seal().unwrap();
    let mut open = db.writer();
    let kept = append_blocks(&mut open, 3, 9);
    open.sync().unwrap();
    db.commit();
    drop((sealed, open));

    let never_named = db.segments.create().unwrap();
    assert_eq!(never_named.file_id(), id(3));
    drop(never_named);
    db.segments.drop_segment(id(1)).unwrap();
    let without_first = db.segments.describe(db.durable()).unwrap();
    assert_eq!(
        without_first,
        manifest_of(
            [0, 3, 0, 0],
            None,
            vec![
                segment(
                    2,
                    OPEN,
                    20 + kept.iter().map(|b| u64::from(b.location.len)).sum::<u64>()
                ),
                segment(3, OPEN, 20)
            ]
        )
    );
    let named_second_only = manifest_of(
        [0, 2, 0, 0],
        None,
        vec![*without_first.files().first().unwrap()],
    );
    replace(&mut db.manifest, named_second_only.clone()).unwrap();

    let seg_dir = db.seg_dir();
    fs::write(
        seg_dir.join("00000004.seg"),
        &header(FileKind::Segment, 4)[..7],
    )
    .unwrap();
    fs::write(seg_dir.join("00000005.seg"), b"").unwrap();
    fs::write(seg_dir.join("notes.txt"), b"not ours").unwrap();
    fs::write(seg_dir.join("000000006.seg"), b"not a segment name").unwrap();
    fs::write(seg_dir.join("00000002.seg.tmp"), b"not a segment name").unwrap();
    fs::create_dir(seg_dir.join("00000007.seg")).unwrap();
    assert_eq!(
        names(&seg_dir),
        [
            "000000006.seg",
            "00000001.seg",
            "00000002.seg",
            "00000002.seg.tmp",
            "00000003.seg",
            "00000004.seg",
            "00000005.seg",
            "00000007.seg",
            "notes.txt",
        ]
    );

    let db = db.reopen();
    assert_eq!(db.durable(), &named_second_only);
    assert_eq!(
        names(&seg_dir),
        [
            "000000006.seg",
            "00000002.seg",
            "00000002.seg.tmp",
            "00000007.seg",
            "notes.txt",
        ]
    );
    db.assert_readable(&kept);
    assert_eq!(names(db.path()), ["MANIFEST", "seg"]);
}

#[test]
fn an_unnamed_segment_above_the_last_id_that_holds_blocks_stops_the_open() {
    let (with_blocks, _) = segment_bytes(3, &[b"written by someone"], false);
    let (header_only, _) = segment_bytes(4, &[], false);
    let (named, _) = segment_bytes(2, &[b"named"], false);
    let manifest = manifest_of([0, 2, 0, 0], None, vec![segment(2, CLOSED, 33)]);
    let prepare = |dir: &Path| {
        write_segment(dir, 1, &header(FileKind::Segment, 1));
        write_segment(dir, 2, &named);
        write_segment(dir, 3, &with_blocks);
        write_segment(dir, 4, &header_only);
    };

    let Err((error, dir)) = Db::try_with_manifest(manifest.clone(), prepare) else {
        panic!("the open succeeded");
    };
    let stray = FileNames::SEGMENT.path(dir.path(), id(3));
    assert!(matches!(
        error,
        SegmentStoreError::UnnamedSegmentHoldsBlocks { path, len } if path == stray && len == with_blocks.len() as u64
    ));
    assert_eq!(
        names(&dir.path().join("seg")),
        [
            "00000001.seg",
            "00000002.seg",
            "00000003.seg",
            "00000004.seg"
        ]
    );

    fs::remove_file(stray).unwrap();
    let manifest_store = ManifestStore::open(Arc::new(faulty_std()), dir.path()).unwrap();
    SegmentStore::open(&manifest_store).unwrap();
    assert_eq!(names(&dir.path().join("seg")), ["00000002.seg"]);
}

#[test]
fn an_unnamed_file_that_cannot_be_measured_or_removed_fails_the_open() {
    for (op, expected_op) in [
        (Op::Open, "measure"),
        (Op::Len, "measure"),
        (Op::RemoveFile, "remove"),
    ] {
        let mut db = Db::new();
        drop(db.writer());
        drop(db.segments.create().unwrap());
        let name = FileNames::SEGMENT.file_name(id(2));
        db.vfs.fail(op, &name);
        let Db { vfs, dir, .. } = db;

        let manifest = ManifestStore::open(Arc::new(vfs.clone()), dir.path()).unwrap();
        assert_io(
            &SegmentStore::open(&manifest).unwrap_err(),
            expected_op,
            &name,
        );
        assert_eq!(
            names(&dir.path().join("seg")),
            ["00000001.seg", "00000002.seg"]
        );
        SegmentStore::open(&manifest).unwrap();
        assert_eq!(names(&dir.path().join("seg")), ["00000001.seg"]);
    }
}

#[test]
fn a_named_segment_that_is_missing_short_or_not_itself_stops_the_open() {
    let (good, _) = segment_bytes(1, &[b"one", b"two"], false);
    let good_len = good.len() as u64;
    let (other_id, _) = segment_bytes(2, &[b"one", b"two"], false);
    let mut other_kind = good.clone();
    other_kind[..20].copy_from_slice(&header(FileKind::ValueLog, 1));
    let mut damaged = good.clone();
    damaged[9] ^= 1;

    type Check = fn(&SegmentStoreError) -> bool;
    let cases: [(Option<Vec<u8>>, Check); 6] = [
        (
            None,
            |error| matches!(error, SegmentStoreError::Io { op: "open", source, .. } if source.kind() == ErrorKind::NotFound),
        ),
        (Some(good[..good.len() - 1].to_vec()), |error| {
            matches!(
                error,
                SegmentStoreError::FileTooShort {
                    valid_len: 42,
                    len: 41,
                    ..
                }
            )
        }),
        (Some(Vec::new()), |error| {
            matches!(
                error,
                SegmentStoreError::FileTooShort {
                    valid_len: 42,
                    len: 0,
                    ..
                }
            )
        }),
        (Some(other_id), |error| {
            matches!(
                error,
                SegmentStoreError::Format {
                    offset: 0,
                    source: FormatError::Segment(SegmentError::WrongFileId {
                        expected: 1,
                        found: 2
                    }),
                    ..
                }
            )
        }),
        (Some(other_kind), |error| {
            matches!(
                error,
                SegmentStoreError::Format {
                    source: FormatError::Segment(SegmentError::NotASegment {
                        found: FileKind::ValueLog
                    }),
                    ..
                }
            )
        }),
        (Some(damaged), |error| {
            matches!(
                error,
                SegmentStoreError::Format {
                    source: FormatError::ChecksumMismatch { .. },
                    ..
                }
            )
        }),
    ];
    assert_eq!(good_len, 42);

    for (content, expected) in cases {
        let manifest = manifest_of([0, 5, 0, 0], None, vec![segment(1, CLOSED, good_len)]);
        let prepare = |dir: &Path| {
            write_segment(dir, 4, &header(FileKind::Segment, 4));
            if let Some(content) = &content {
                write_segment(dir, 1, content);
            }
        };
        let Err((error, dir)) = Db::try_with_manifest(manifest.clone(), prepare) else {
            panic!("the open succeeded");
        };
        assert!(expected(&error), "{error:?}");
        assert!(
            dir.path().join("seg/00000004.seg").exists(),
            "nothing is removed"
        );

        let read_only = SegmentStore::open_read_only(Arc::new(faulty_std()), dir.path(), &manifest);
        assert!(expected(&read_only.unwrap_err()));
    }

    for (op, expected_op) in [(Op::Len, "measure"), (Op::Read, "read")] {
        let db = Db::with_manifest(one_segment(CLOSED, good_len), |dir| {
            write_segment(dir, 1, &good)
        });
        db.vfs.fail(op, "00000001.seg");
        let error = db.try_reopen().err().expect("the open fails");
        assert_io(&error, expected_op, "00000001.seg");
    }
}

#[test]
fn a_block_is_read_only_through_its_exact_location() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let raw = writer
        .append(
            page(1),
            BlockContent::Base,
            Compression::Raw,
            &random(100, 1),
        )
        .unwrap();
    let compressed = writer
        .append(
            page(2),
            BlockContent::Delta,
            Compression::Lz4,
            &repetitive(4096, 2),
        )
        .unwrap();
    let empty = writer
        .append(page(3), BlockContent::Delta, Compression::Raw, b"")
        .unwrap();
    let end = writer.end();
    assert_eq!((raw.offset, raw.len), (20, 108));
    assert!(compressed.len < 200);
    assert_eq!((empty.len, empty.offset + 8), (8, end));

    assert_eq!(db.read(raw).unwrap(), (BlockContent::Base, random(100, 1)));
    assert_eq!(
        db.read(compressed).unwrap(),
        (BlockContent::Delta, repetitive(4096, 2))
    );
    assert_eq!(db.read(empty).unwrap(), (BlockContent::Delta, Vec::new()));

    let out_of_bounds = [
        location(1, 0, 20),
        location(1, 19, 8),
        location(1, end, 8),
        location(1, end - 7, 8),
        location(1, raw.offset, (end - raw.offset) as u32 + 1),
        location(1, u64::MAX - 3, 8),
        location(1, u64::MAX, u32::MAX),
    ];
    for bad in out_of_bounds {
        assert!(matches!(
            db.read(bad).unwrap_err(),
            SegmentStoreError::OutOfBounds { location, end: found } if location == bad && found == end
        ));
    }
    assert!(matches!(
        db.read(location(2, 20, 8)).unwrap_err(),
        SegmentStoreError::UnknownSegment { file } if file == id(2)
    ));

    let wrong_length = [
        (
            location(1, raw.offset, raw.len - 1),
            FormatError::Truncated {
                needed: 108,
                available: 107,
            },
        ),
        (
            location(1, raw.offset, 3),
            FormatError::Truncated {
                needed: 8,
                available: 3,
            },
        ),
        (
            location(1, raw.offset, raw.len + 1),
            FormatError::Segment(SegmentError::LengthMismatch {
                expected: 108,
                found: 109,
            }),
        ),
    ];
    for (bad, expected) in wrong_length {
        assert_eq!(
            format_error(db.read(bad).unwrap_err()),
            (raw.offset, expected)
        );
    }
    let (offset, error) = format_error(db.read(location(1, raw.offset + 1, raw.len)).unwrap_err());
    assert_eq!(offset, raw.offset + 1);
    assert!(
        !matches!(
            error,
            FormatError::Segment(SegmentError::LengthMismatch { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn nothing_longer_than_the_caller_accepts_is_read() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let raw = writer
        .append(
            page(1),
            BlockContent::Base,
            Compression::Raw,
            &random(100, 1),
        )
        .unwrap();
    let compressed = writer
        .append(
            page(2),
            BlockContent::Base,
            Compression::Lz4,
            &repetitive(4096, 2),
        )
        .unwrap();
    db.vfs.take_calls();

    let mut buf = Vec::new();
    let error = db.segments.read(raw, 99, &mut buf).unwrap_err();
    assert_eq!(
        format_error(error),
        (raw.offset, FormatError::RecordTooLong { len: 100, max: 99 })
    );
    assert_eq!(db.vfs.take_calls(), [], "refused before the file is read");
    assert_eq!(
        db.segments.read(raw, 100, &mut buf).unwrap().payload_len(),
        100
    );

    let error = db.segments.read(compressed, 4095, &mut buf).unwrap_err();
    assert_eq!(
        format_error(error),
        (
            compressed.offset,
            FormatError::RecordTooLong {
                len: 4096,
                max: 4095
            }
        )
    );
    assert_eq!(
        db.segments
            .read(compressed, 4096, &mut buf)
            .unwrap()
            .payload_len(),
        4096
    );
    assert_eq!(
        db.segments
            .read(compressed, usize::MAX, &mut buf)
            .unwrap()
            .payload()
            .unwrap(),
        repetitive(4096, 2)
    );

    db.vfs.fail(Op::Read, "00000001.seg");
    assert_io(
        &db.segments.read(raw, MAX, &mut buf).unwrap_err(),
        "read",
        "00000001.seg",
    );
    assert_eq!(db.read(raw).unwrap().1, random(100, 1));
}

#[test]
fn a_footer_is_read_only_from_a_sealed_segment_and_checked() {
    let (sealed, _) = segment_bytes(1, &[b"one", b"two", b"three"], true);
    let (unsealed, _) = segment_bytes(2, &[b"one", b"two"], false);
    let sealed_len = sealed.len() as u64;
    let mut damaged = sealed.clone();
    let index_at = damaged.len() - 20 - 3 * 8 - 8;
    damaged[index_at + 8] ^= 0x40;
    let files = vec![
        segment(1, SEALED, sealed_len),
        segment(2, CLOSED, unsealed.len() as u64),
        segment(3, SEALED, sealed_len - 1),
        segment(4, SEALED, sealed_len),
        segment(5, SEALED, sealed_len),
    ];
    let mut cut_short = sealed.clone();
    cut_short[..20].copy_from_slice(&header(FileKind::Segment, 3));
    damaged[..20].copy_from_slice(&header(FileKind::Segment, 4));
    let mut zero_trailer = sealed.clone();
    zero_trailer[..20].copy_from_slice(&header(FileKind::Segment, 5));
    let trailer_at = zero_trailer.len() - 20;
    zero_trailer[trailer_at..].fill(0);

    let db = Db::with_manifest(manifest_of([0, 5, 0, 0], None, files), |dir| {
        write_segment(dir, 1, &sealed);
        write_segment(dir, 2, &unsealed);
        write_segment(dir, 3, &cut_short);
        write_segment(dir, 4, &damaged);
        write_segment(dir, 5, &zero_trailer);
    });
    let mut buf = Vec::new();
    let index = db.segments.index(id(1), &mut buf).unwrap();
    assert_eq!(index.len(), 3);
    assert_eq!(
        index.blocks().map(|block| block.offset).collect::<Vec<_>>(),
        [20, 31, 42]
    );

    assert!(matches!(
        db.segments.index(id(2), &mut buf).unwrap_err(),
        SegmentStoreError::NotSealed { file } if file == id(2)
    ));
    assert!(matches!(
        db.segments.index(id(9), &mut buf).unwrap_err(),
        SegmentStoreError::UnknownSegment { file } if file == id(9)
    ));

    let (offset, _) = format_error(db.segments.index(id(3), &mut buf).unwrap_err());
    assert_eq!(offset, sealed_len - 21);
    assert!(db.segments.walk(id(3), MAX).is_err());

    let (offset, error) = format_error(db.segments.index(id(4), &mut buf).unwrap_err());
    assert_eq!(offset, index_at as u64);
    assert!(matches!(error, FormatError::ChecksumMismatch { .. }));
    assert_eq!(db.walked(4).unwrap().len(), 3);

    let (offset, error) = format_error(db.segments.index(id(5), &mut buf).unwrap_err());
    assert_eq!(offset, trailer_at as u64);
    assert!(matches!(error, FormatError::ChecksumMismatch { .. }));

    db.vfs.fail(Op::Read, "00000001.seg");
    assert_io(
        &db.segments.index(id(1), &mut buf).unwrap_err(),
        "read",
        "00000001.seg",
    );
    db.vfs.fail_after(1, Op::Read, "00000001.seg");
    assert_io(
        &db.segments.index(id(1), &mut buf).unwrap_err(),
        "read",
        "00000001.seg",
    );
    db.vfs.fail(Op::Read, "00000001.seg");
    assert_io(
        &db.segments.walk(id(1), MAX).err().unwrap(),
        "read",
        "00000001.seg",
    );
    assert_eq!(db.segments.index(id(1), &mut buf).unwrap().len(), 3);
}

#[test]
fn a_walk_reads_blocks_of_every_size_across_its_window() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let mut blocks = Vec::new();
    for (n, len) in [
        0, 1, 70_000, 262_136, 0, 262_144, 5, 300_000, MAX, 9, 0, 131_072, 131_072, 40,
    ]
    .into_iter()
    .enumerate()
    {
        append_to(
            &mut writer,
            &mut blocks,
            random(len, n as u64),
            Compression::Raw,
        );
        append_to(
            &mut writer,
            &mut blocks,
            repetitive(len, n as u64),
            Compression::Lz4,
        );
    }
    for n in 0..3000 {
        append_to(
            &mut writer,
            &mut blocks,
            random(n % 50, n as u64),
            Compression::Raw,
        );
    }
    let end = writer.end();

    db.assert_walk_gives(1, &blocks);
    let mut walk = db.segments.walk(id(1), MAX).unwrap();
    assert_eq!(walk.offset(), 20);
    let mut count = 0;
    while let Some((location, _)) = walk.next_block().unwrap() {
        assert_eq!(location, blocks[count].location);
        count += 1;
        assert_eq!(walk.offset(), location.offset + u64::from(location.len));
    }
    assert_eq!((count, walk.offset()), (blocks.len(), end));
    assert!(walk.next_block().unwrap().is_none());

    let mut walk = db.segments.walk(id(1), MAX).unwrap();
    append_to(&mut writer, &mut blocks, random(10, 1), Compression::Raw);
    let mut seen = 0;
    while walk.next_block().unwrap().is_some() {
        seen += 1;
    }
    assert_eq!(
        seen,
        blocks.len() - 1,
        "a walk ends where the blocks ended when it began"
    );

    writer.seal().unwrap();
    db.assert_walk_gives(1, &blocks);
    assert!(matches!(
        db.segments.walk(id(2), MAX).err().unwrap(),
        SegmentStoreError::UnknownSegment { .. }
    ));
}

#[test]
fn a_walk_stops_at_the_first_byte_that_is_not_in_a_whole_block() {
    let payloads: [&[u8]; 4] = [b"", b"first", &[7; 3000], b"last"];
    let (bytes, mut offsets) = segment_bytes(1, &payloads, false);
    let (sealed, _) = segment_bytes(1, &payloads, true);
    let len = bytes.len() as u64;
    offsets.push(len);

    let walk_error = |content: &[u8], state, valid_len, max_payload_len| {
        let db = Db::with_manifest(one_segment(state, valid_len), |dir| {
            write_segment(dir, 1, content)
        });
        let mut walk = db.segments.walk(id(1), max_payload_len).unwrap();
        let mut whole = 0;
        loop {
            match walk.next_block() {
                Ok(Some(_)) => whole += 1,
                Ok(None) => panic!("the walk found no fault"),
                Err(error) => {
                    assert_eq!(
                        walk.offset(),
                        offsets[whole],
                        "the walk stays at the bad block"
                    );
                    let (offset, error) = format_error(error);
                    assert_eq!(offset, offsets[whole]);
                    return (whole, error);
                }
            }
        }
    };

    assert_eq!(
        walk_error(&bytes, CLOSED, len - 1, MAX),
        (
            3,
            FormatError::Truncated {
                needed: 12,
                available: 11
            }
        )
    );
    assert_eq!(
        walk_error(&bytes, CLOSED, offsets[2] + 2, MAX),
        (
            2,
            FormatError::Truncated {
                needed: 8,
                available: 2
            }
        )
    );
    assert_eq!(
        walk_error(&bytes, CLOSED, offsets[2] + 100, MAX),
        (
            2,
            FormatError::Truncated {
                needed: 3008,
                available: 100
            }
        )
    );
    assert_eq!(
        walk_error(&bytes, CLOSED, len, 2999),
        (
            2,
            FormatError::RecordTooLong {
                len: 3000,
                max: 2999
            }
        )
    );
    assert_eq!(
        walk_error(&sealed, CLOSED, sealed.len() as u64, MAX).0,
        4,
        "a footer is not a block"
    );
    assert!(matches!(
        walk_error(&sealed, CLOSED, sealed.len() as u64, MAX).1,
        FormatError::Segment(SegmentError::UnexpectedRecordKind { code: 5 })
    ));

    let mut damaged = bytes.clone();
    damaged[offsets[2] as usize + 2000] ^= 1;
    assert!(matches!(
        walk_error(&damaged, CLOSED, len, MAX),
        (2, FormatError::ChecksumMismatch { .. })
    ));
    let mut zeroed = bytes.clone();
    zeroed[offsets[1] as usize..].fill(0);
    assert!(matches!(
        walk_error(&zeroed, CLOSED, len, MAX),
        (1, FormatError::ChecksumMismatch { .. })
    ));

    let empty_lz4 = |trailing: &[u8]| {
        let tag = (2u32 << 27).to_le_bytes();
        let mut bytes = header(FileKind::Segment, 1).to_vec();
        offsets_check(&bytes);
        bytes.extend_from_slice(&tag);
        bytes.extend_from_slice(&Checksum::of(&[&tag]).encode());
        bytes.extend_from_slice(trailing);
        bytes
    };
    fn offsets_check(bytes: &[u8]) {
        assert_eq!(bytes.len(), 20);
    }
    for trailing in [&[][..], &[0; 40]] {
        let content = empty_lz4(trailing);
        let db = Db::with_manifest(one_segment(CLOSED, content.len() as u64), |dir| {
            write_segment(dir, 1, &content)
        });
        let error = db.walked(1).unwrap_err();
        assert!(matches!(
            format_error(error),
            (20, FormatError::Truncated { needed: 13, .. })
        ));
    }
}

#[test]
fn a_walk_that_cannot_read_can_be_asked_again() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let mut blocks = Vec::new();
    for n in 0..60 {
        let len = [90_000, 300_000, 12, 0][n % 4];
        append_to(
            &mut writer,
            &mut blocks,
            random(len, n as u64),
            Compression::Raw,
        );
    }
    let mut walk = db.segments.walk(id(1), MAX).unwrap();

    let (mut seen, mut failures, mut retrying) = (0, 0, false);
    loop {
        if !retrying && db.vfs.pending_faults() == 0 {
            db.vfs.fail_after(failures % 2, Op::Read, "00000001.seg");
        }
        match walk.next_block() {
            Ok(Some((location, block))) => {
                assert_eq!(location, blocks[seen].location);
                assert_eq!(block.payload().unwrap(), blocks[seen].payload.as_slice());
                seen += 1;
                retrying = false;
            }
            Ok(None) => break,
            Err(error) => {
                assert_io(&error, "read", "00000001.seg");
                assert_eq!(walk.offset(), blocks[seen].location.offset);
                failures += 1;
                retrying = true;
            }
        }
    }
    assert_eq!(seen, blocks.len());
    assert!(failures > 10, "{failures}");
}

#[test]
fn a_dropped_segment_is_deleted_once_a_durable_manifest_does_not_name_it() {
    let mut db = Db::new();
    let mut first = db.writer();
    let first_blocks = append_blocks(&mut first, 8, 1);
    let mut second = db.writer();
    let second_blocks = append_blocks(&mut second, 8, 50);

    assert!(matches!(
        db.segments.drop_segment(id(1)).unwrap_err(),
        SegmentStoreError::SegmentBusy { file } if file == id(1)
    ));
    assert!(matches!(
        db.segments.drop_segment(id(9)).unwrap_err(),
        SegmentStoreError::UnknownSegment { file } if file == id(9)
    ));
    let sealed_len = first.seal().unwrap();
    assert!(matches!(
        db.segments.drop_segment(id(1)).unwrap_err(),
        SegmentStoreError::SegmentBusy { .. }
    ));
    drop(first);
    db.commit();
    let both = db.on_disk();
    assert_eq!(both.files()[0], segment(1, SEALED, sealed_len));

    db.segments.drop_segment(id(1)).unwrap();
    assert!(matches!(
        db.read(first_blocks[0].location).unwrap_err(),
        SegmentStoreError::UnknownSegment { file } if file == id(1)
    ));
    assert!(matches!(
        db.segments.drop_segment(id(1)).unwrap_err(),
        SegmentStoreError::UnknownSegment { .. }
    ));
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(2, OPEN, 20)]
    );
    db.assert_readable(&second_blocks);

    db.segments.manifest_replaced(&db.manifest).unwrap();
    assert!(
        db.seg_path(1).exists(),
        "the durable manifest still names it"
    );
    let renamed_wal = manifest_of(
        [1, 2, 0, 0],
        None,
        both.files()
            .iter()
            .copied()
            .chain([live(FileKind::Wal, 1, OPEN, 20)])
            .collect(),
    );
    replace(&mut db.manifest, renamed_wal).unwrap();
    db.segments.manifest_replaced(&db.manifest).unwrap();
    assert!(
        db.seg_path(1).exists(),
        "a manifest that carried it over names it"
    );

    db.vfs.fail(Op::RemoveFile, "00000001.seg");
    let error = db.segments.commit(&mut db.manifest).unwrap_err();
    assert!(matches!(
        &error,
        SegmentStoreError::NotDeleted { path, source }
            if *path == db.seg_path(1) && source.kind() == faulty::INJECTED
    ));
    assert!(db.seg_path(1).exists());
    assert_eq!(
        db.on_disk(),
        manifest_of(
            [1, 2, 0, 0],
            None,
            vec![live(FileKind::Wal, 1, OPEN, 20), segment(2, OPEN, 20)]
        )
    );

    db.segments.manifest_replaced(&db.manifest).unwrap();
    assert_eq!(names(&db.seg_dir()), ["00000002.seg"]);
    db.segments.manifest_replaced(&db.manifest).unwrap();
    db.assert_readable(&second_blocks);

    let mut third = db.writer();
    assert_eq!(
        third.file_id(),
        id(3),
        "the id of a deleted segment is not used again"
    );
    append_blocks(&mut third, 2, 3);
}

#[test]
fn a_segment_dropped_before_it_was_named_never_reaches_a_manifest() {
    let mut db = Db::new();
    let writer = db.segments.create().unwrap();
    drop(writer);
    db.segments.drop_segment(id(1)).unwrap();
    db.commit();
    assert_eq!(db.on_disk(), manifest_of([0, 1, 0, 0], None, Vec::new()));
    assert_eq!(names(&db.seg_dir()), [""; 0]);
    assert_eq!(db.writer().file_id(), id(2));
}

#[test]
fn a_read_only_store_reads_and_changes_nothing() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let blocks = append_blocks(&mut writer, 12, 1);
    writer.seal().unwrap();
    db.commit();
    drop(writer);
    drop(db.segments.create().unwrap());
    fs::write(db.path().join("MANIFEST.tmp"), b"stale").unwrap();
    let before = (names(db.path()), names(&db.seg_dir()));

    let manifest = read_manifest(&db.vfs, db.path()).unwrap();
    db.vfs.take_calls();
    let read_only = SegmentStore::open_read_only(db.arc_vfs(), db.path(), &manifest).unwrap();
    let name = "00000001.seg".to_owned();
    assert_eq!(
        db.vfs.take_calls(),
        [
            (Op::Open, name.clone()),
            (Op::Len, name.clone()),
            (Op::Read, name)
        ]
    );
    assert_eq!(read_only.segments_named().unwrap(), manifest.files());

    let mut buf = Vec::new();
    for block in &blocks {
        let read = read_only.read(block.location, MAX, &mut buf).unwrap();
        assert_eq!(read.payload().unwrap(), block.payload.as_slice());
    }
    assert_eq!(
        read_only.index(id(1), &mut buf).unwrap().len(),
        blocks.len()
    );
    read_only.sync().unwrap();
    assert_eq!(read_only.describe(&manifest).unwrap(), manifest);

    let is_read_only = |error| matches!(error, SegmentStoreError::ReadOnly);
    assert!(is_read_only(read_only.create().err().unwrap()));
    assert!(is_read_only(read_only.drop_segment(id(1)).unwrap_err()));
    assert!(is_read_only(
        read_only.manifest_replaced(&db.manifest).unwrap_err()
    ));
    assert!(is_read_only(
        read_only.commit(&mut db.manifest).unwrap_err()
    ));
    assert_eq!((names(db.path()), names(&db.seg_dir())), before);

    let fresh = tempfile::tempdir().unwrap();
    let nothing = SegmentStore::open_read_only(db.arc_vfs(), fresh.path(), &empty_manifest());
    assert_eq!(nothing.unwrap().segments_named().unwrap(), []);
    assert_eq!(names(fresh.path()), [""; 0]);
}

#[test]
fn a_manifest_of_another_directory_or_in_doubt_is_not_acted_on() {
    let mut db = Db::new();
    let mut other = Db::new();
    let (here, there) = (db.path().to_path_buf(), other.path().to_path_buf());
    let wrong = |error| {
        matches!(
            error,
            SegmentStoreError::WrongDirectory { expected, found } if expected == here && found == there
        )
    };
    assert!(wrong(
        db.segments.manifest_replaced(&other.manifest).unwrap_err()
    ));
    assert!(wrong(db.segments.commit(&mut other.manifest).unwrap_err()));

    let mut writer = db.segments.create().unwrap();
    db.vfs.fail(Op::SyncDir, &dir_name(db.path()));
    assert_io(
        &db.segments.commit(&mut db.manifest).unwrap_err(),
        "sync the directory",
        &dir_name(db.path()),
    );
    let in_doubt = |error| matches!(error, SegmentStoreError::ManifestInDoubt { .. });
    assert!(in_doubt(
        db.segments.manifest_replaced(&db.manifest).unwrap_err()
    ));
    assert!(in_doubt(db.segments.commit(&mut db.manifest).unwrap_err()));
    let refused = writer.append(page(1), BlockContent::Base, Compression::Raw, b"early");
    assert!(matches!(
        refused.unwrap_err(),
        SegmentStoreError::NotNamed { .. }
    ));

    db.manifest = ManifestStore::open(db.arc_vfs(), db.path()).unwrap();
    assert_eq!(db.durable(), &one_segment(OPEN, 20));
    db.segments.manifest_replaced(&db.manifest).unwrap();
    let blocks = append_blocks(&mut writer, 5, 1);
    db.assert_readable(&blocks);
}

#[test]
fn a_segment_that_cannot_be_created_leaves_no_file_and_its_id_is_not_used_again() {
    let mut db = Db::new();
    let name = FileNames::SEGMENT.file_name(id(1));
    for (skip, op, on, expected_op) in [
        (0, Op::Open, name.as_str(), "create"),
        (0, Op::Append, name.as_str(), "append to"),
        (0, Op::SyncData, name.as_str(), "sync"),
        (0, Op::SyncDir, "seg", "sync the directory"),
    ] {
        let name =
            FileNames::SEGMENT.file_name(id(db.segments.create().unwrap().file_id().get() + 1));
        let on = if on == "seg" { "seg".to_owned() } else { name };
        db.vfs.fail_after(skip, op, &on);
        assert_io(&db.segments.create().err().unwrap(), expected_op, &on);
    }
    let next = db.segments.create().unwrap().file_id().get() + 1;
    let name = FileNames::SEGMENT.file_name(id(next));
    db.vfs.fail_after(1, Op::Open, &name);
    assert_io(&db.segments.create().err().unwrap(), "open", &name);
    assert!(!db.seg_path(next).exists());

    let ids: Vec<_> = db
        .segments
        .segments_named()
        .unwrap()
        .iter()
        .map(|file| file.id().get())
        .collect();
    assert_eq!(ids, [1, 3, 5, 7, 9]);
    assert_eq!(
        names(&db.seg_dir()),
        [
            "00000001.seg",
            "00000003.seg",
            "00000005.seg",
            "00000007.seg",
            "00000009.seg"
        ]
    );
    assert_eq!(db.vfs.pending_faults(), 0);
    assert_eq!(db.writer().file_id(), id(11));

    fs::create_dir(db.seg_path(12)).unwrap();
    assert!(matches!(
        db.segments.create().err().unwrap(),
        SegmentStoreError::Io { op: "create", .. }
    ));
}

#[test]
fn segment_ids_run_out_at_the_largest_one() {
    let db = Db::with_manifest(
        manifest_of([0, u32::MAX - 1, 0, 0], None, Vec::new()),
        |_| {},
    );
    assert_eq!(db.segments.create().unwrap().file_id(), FileId::MAX);
    for _ in 0..2 {
        assert!(matches!(
            db.segments.create().err().unwrap(),
            SegmentStoreError::FileIdsExhausted
        ));
    }
    assert_eq!(names(&db.seg_dir()), ["4294967295.seg"]);
}

#[test]
fn blocks_are_read_from_many_threads_while_they_are_appended_and_synced() {
    const BLOCKS: usize = 3000;
    let mut db = Db::new();
    let mut writer = db.writer();
    let (segments, manifest) = (&db.segments, &mut db.manifest);
    let published = Mutex::new(Vec::<Appended>::new());
    let done = AtomicBool::new(false);

    thread::scope(|scope| {
        let readers: Vec<_> = (0..4u64)
            .map(|reader| {
                let (published, done) = (&published, &done);
                scope.spawn(move || {
                    let mut buf = Vec::new();
                    let mut reads = 0u64;
                    let mut pick = reader + 1;
                    loop {
                        let finished = done.load(Ordering::Acquire);
                        let block = {
                            let published = published.lock().unwrap();
                            pick = pick.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                            published
                                .get((pick >> 33) as usize % published.len().max(1))
                                .cloned()
                        };
                        if let Some(block) = block {
                            let read = segments.read(block.location, MAX, &mut buf).unwrap();
                            assert_eq!(read.payload().unwrap(), block.payload.as_slice());
                            reads += 1;
                        }
                        if finished {
                            return reads;
                        }
                    }
                })
            })
            .collect();
        let syncer = scope.spawn(|| {
            let mut rounds = 0;
            while !done.load(Ordering::Acquire) {
                segments.sync().unwrap();
                segments.commit(manifest).unwrap();
                rounds += 1;
            }
            rounds
        });

        for n in 0..BLOCKS {
            let block = append_blocks(&mut writer, 1, n as u64).remove(0);
            published.lock().unwrap().push(block);
            if n % 500 == 499 {
                writer.sync().unwrap();
            }
        }
        done.store(true, Ordering::Release);
        assert!(syncer.join().unwrap() > 0);
        for reader in readers {
            assert!(reader.join().unwrap() > 0);
        }
    });

    let blocks = published.into_inner().unwrap();
    let sealed_len = writer.seal().unwrap();
    db.commit();
    assert_eq!(db.on_disk(), one_segment(SEALED, sealed_len));
    db.assert_walk_gives(1, &blocks);
}

#[test]
fn the_store_and_its_parts_cross_threads() {
    fn shared<T: Send + Sync>() {}
    fn sent<T: Send>() {}
    shared::<SegmentStore>();
    shared::<ManifestStore>();
    sent::<SegmentWriter>();
    sent::<BlockWalk>();
    sent::<SegmentStoreError>();
}
