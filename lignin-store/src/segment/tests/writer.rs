use lignin_format::segment::{write_block, write_footer, IndexEntry, SegmentError, MAX_BODY_LEN};

use super::*;

const CLOSED: FileState = FileState::Closed;
const OPEN: FileState = FileState::Open;
const SEALED: FileState = FileState::Sealed;
const SEGMENT: &str = "00000001.seg";

const GOLDEN_SEGMENT_START: &str = "4c49474e494e44420300010036010000d2c72fa6\
    06000008e6e37a086c69676e696e\
    020000186e7c60c56431";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn one_segment(state: FileState, valid_len: u64) -> Manifest {
    manifest_of([0, 1, 0, 0], None, vec![segment(1, state, valid_len)])
}

fn is_failed(error: SegmentStoreError) -> bool {
    matches!(error, SegmentStoreError::SegmentFailed { file } if file == id(1))
}

fn is_not_open(error: SegmentStoreError) -> bool {
    matches!(error, SegmentStoreError::NotOpen { file } if file == id(1))
}

fn append_one(writer: &mut SegmentWriter) -> Result<BlockLocation, SegmentStoreError> {
    writer.append(page(1), BlockContent::Delta, Compression::Raw, b"one more")
}

#[track_caller]
fn assert_out_of_bounds(db: &Db, blocks: &[Appended], end: u64) {
    for block in blocks {
        assert!(matches!(
            db.read(block.location).unwrap_err(),
            SegmentStoreError::OutOfBounds { end: found, .. } if found == end
        ));
    }
}

#[test]
fn the_file_is_the_header_the_blocks_and_the_footer_as_the_format_encodes_them() {
    let mut db = Db::with_manifest(manifest_of([0, 309, 0, 0], None, Vec::new()), |_| {});
    let mut writer = db.writer();
    assert_eq!(writer.file_id(), id(310));
    let abcd: Vec<u8> = b"abcd".repeat(8);
    let appends: [(u32, BlockContent, Compression, &[u8]); 3] = [
        (7, BlockContent::Base, Compression::Lz4, b"lignin"),
        (7, BlockContent::Delta, Compression::Raw, b"d1"),
        (9, BlockContent::Base, Compression::Lz4, &abcd),
    ];

    let mut expected = header(FileKind::Segment, 310).to_vec();
    let mut index = Vec::new();
    for (page_id, content, compression, payload) in appends {
        let offset = expected.len() as u64;
        let tag = write_block(content, compression, payload, &mut expected).unwrap();
        index.push(IndexEntry {
            page: page(page_id),
            tag,
        });
        let location = writer
            .append(page(page_id), content, compression, payload)
            .unwrap();
        assert_eq!(
            (location.offset, location.len as usize),
            (offset, tag.block_len())
        );
    }
    let index_offset = expected.len() as u64;
    write_footer(&index, index_offset, &mut expected).unwrap();

    assert_eq!(writer.seal().unwrap(), expected.len() as u64);
    let written = fs::read(db.seg_path(310)).unwrap();
    assert_eq!(hex(&written), hex(&expected));
    assert_eq!(hex(&written[..44]), GOLDEN_SEGMENT_START);
    assert!(
        index[2].tag.block_len() < 8 + abcd.len(),
        "the third block is compressed"
    );
}

#[test]
fn sealing_appends_the_footer_makes_it_durable_and_ends_the_segment() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let blocks = append_blocks(&mut writer, 30, 2);
    let end = writer.end();
    db.vfs.take_calls();

    let sealed_len = writer.seal().unwrap();
    assert_eq!(
        db.vfs.take_calls(),
        [
            (Op::Append, SEGMENT.to_owned()),
            (Op::SyncData, SEGMENT.to_owned())
        ]
    );
    assert_eq!(sealed_len, end + 8 + 30 * 8 + 20);
    assert_eq!(fs::metadata(db.seg_path(1)).unwrap().len(), sealed_len);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, SEALED, sealed_len)]
    );
    assert_eq!(db.on_disk(), one_segment(OPEN, 20));

    assert!(is_not_open(append_one(&mut writer).unwrap_err()));
    assert!(is_not_open(writer.seal().unwrap_err()));
    assert_eq!(writer.sync().unwrap(), sealed_len);
    db.segments.sync().unwrap();
    assert_eq!(fs::metadata(db.seg_path(1)).unwrap().len(), sealed_len);

    db.commit();
    assert_eq!(db.on_disk(), one_segment(SEALED, sealed_len));
    db.assert_readable(&blocks);
    db.assert_walk_gives(1, &blocks);
    let mut buf = Vec::new();
    let index = db.segments.index(id(1), &mut buf).unwrap();
    assert_eq!(
        index
            .blocks()
            .map(|block| (block.page, block.offset))
            .collect::<Vec<_>>(),
        blocks
            .iter()
            .map(|block| (block.page, block.location.offset))
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        db.read(BlockLocation {
            file: id(1),
            offset: end,
            len: 8 + 30 * 8
        })
        .unwrap_err(),
        SegmentStoreError::Format {
            source: FormatError::Segment(SegmentError::UnexpectedRecordKind { code: 5 }),
            ..
        }
    ));
}

#[test]
fn a_segment_with_no_block_seals_to_its_smallest_form() {
    let mut db = Db::new();
    let mut writer = db.writer();
    assert_eq!(writer.seal().unwrap(), 48);
    db.commit();
    assert_eq!(db.on_disk(), one_segment(SEALED, 48));
    let mut buf = Vec::new();
    assert!(db.segments.index(id(1), &mut buf).unwrap().is_empty());
    db.assert_walk_gives(1, &[]);
}

#[test]
fn a_full_segment_takes_no_more_blocks_and_can_be_sealed() {
    let mut db = Db::new();
    let mut writer = db.writer();
    writer.limit_blocks(3);
    let blocks = append_blocks(&mut writer, 3, 1);
    let end = writer.end();
    for _ in 0..2 {
        assert!(matches!(
            append_one(&mut writer).unwrap_err(),
            SegmentStoreError::SegmentFull { file } if file == id(1)
        ));
    }
    assert_eq!((writer.end(), writer.block_count()), (end, 3));
    writer.seal().unwrap();
    let mut buf = Vec::new();
    assert_eq!(db.segments.index(id(1), &mut buf).unwrap().len(), 3);
    db.assert_walk_gives(1, &blocks);
}

#[test]
fn an_append_that_fails_appends_nothing_and_the_writer_goes_on() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let mut blocks = append_blocks(&mut writer, 4, 1);
    let end = writer.end();

    db.vfs.fail(Op::Append, SEGMENT);
    assert_io(&append_one(&mut writer).unwrap_err(), "append to", SEGMENT);
    let too_long = vec![0; MAX_BODY_LEN + 1];
    let error = writer
        .append(page(1), BlockContent::Base, Compression::Lz4, &too_long)
        .unwrap_err();
    assert_eq!(
        format_error(error),
        (
            end,
            FormatError::RecordTooLong {
                len: MAX_BODY_LEN as u64 + 1,
                max: MAX_BODY_LEN as u64
            }
        )
    );
    assert_eq!((writer.end(), writer.block_count()), (end, 4));
    assert_eq!(fs::metadata(db.seg_path(1)).unwrap().len(), end);

    blocks.extend(append_blocks(&mut writer, 4, 40));
    assert_eq!(blocks[4].location.offset, end);
    writer.seal().unwrap();
    db.assert_readable(&blocks);
    db.assert_walk_gives(1, &blocks);
}

#[test]
fn a_footer_that_cannot_be_appended_leaves_the_segment_open() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let mut blocks = append_blocks(&mut writer, 4, 1);
    db.vfs.fail(Op::Append, SEGMENT);
    assert_io(&writer.seal().unwrap_err(), "append to", SEGMENT);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, 20)]
    );

    blocks.extend(append_blocks(&mut writer, 4, 40));
    let sealed_len = writer.seal().unwrap();
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, SEALED, sealed_len)]
    );
    db.assert_walk_gives(1, &blocks);
}

#[test]
fn a_failed_sync_ends_the_segment_at_what_was_synced_before() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let synced = append_blocks(&mut writer, 5, 1);
    let durable = writer.sync().unwrap();
    let in_limbo = append_blocks(&mut writer, 5, 30);

    db.vfs.fail(Op::SyncData, SEGMENT);
    assert_io(&writer.sync().unwrap_err(), "sync", SEGMENT);
    db.vfs.take_calls();
    assert!(is_failed(writer.sync().unwrap_err()));
    assert!(is_failed(append_one(&mut writer).unwrap_err()));
    assert!(is_failed(writer.seal().unwrap_err()));
    assert!(is_failed(db.segments.sync().unwrap_err()));
    assert_eq!(
        db.vfs.take_calls(),
        [],
        "a failed segment is not written or synced"
    );

    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, durable)]
    );
    db.assert_readable(&synced);
    db.assert_readable(&in_limbo);
    assert!(matches!(
        db.segments.drop_segment(id(1)).unwrap_err(),
        SegmentStoreError::SegmentBusy { .. }
    ));

    drop(writer);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, durable)]
    );
    db.assert_readable(&synced);
    assert_out_of_bounds(&db, &in_limbo, durable);
    db.assert_walk_gives(1, &synced);
    db.segments.sync().unwrap();
    db.commit();
    assert_eq!(db.on_disk(), one_segment(CLOSED, durable));

    let mut next = db.writer();
    assert_eq!(next.file_id(), id(2));
    let rewritten = append_blocks(&mut next, 5, 30);
    db.assert_readable(&rewritten);
}

#[test]
fn a_sync_through_the_store_that_fails_stops_the_writer_too() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let synced = append_blocks(&mut writer, 5, 1);
    db.segments.sync().unwrap();
    let durable = writer.end();
    let in_limbo = append_blocks(&mut writer, 5, 30);

    db.vfs.fail(Op::SyncData, SEGMENT);
    assert_io(&db.segments.sync().unwrap_err(), "sync", SEGMENT);
    assert!(is_failed(append_one(&mut writer).unwrap_err()));
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, durable)]
    );
    db.assert_readable(&in_limbo);
    drop(writer);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, durable)]
    );
    db.assert_readable(&synced);
    assert_out_of_bounds(&db, &in_limbo, durable);
}

#[test]
fn a_sync_through_the_store_goes_on_past_a_segment_that_fails() {
    let mut db = Db::new();
    let mut first = db.writer();
    let mut second = db.writer();
    append_blocks(&mut first, 3, 1);
    append_blocks(&mut second, 3, 9);
    let second_end = second.end();

    db.vfs.fail(Op::SyncData, SEGMENT);
    assert_io(&db.segments.sync().unwrap_err(), "sync", SEGMENT);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, 20), segment(2, OPEN, second_end)]
    );

    append_blocks(&mut second, 3, 20);
    assert!(is_failed(db.segments.sync().unwrap_err()));
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, 20), segment(2, OPEN, second.end())]
    );

    drop(first);
    db.segments.sync().unwrap();
}

#[test]
fn a_walk_begun_before_a_failed_sync_ends_where_the_segment_is_cut() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let synced = append_blocks(&mut writer, 3, 1);
    let durable = writer.sync().unwrap();
    let in_limbo = append_blocks(&mut writer, 3, 30);

    let mut behind = db.segments.walk(id(1), MAX).unwrap();
    let mut ahead = db.segments.walk(id(1), MAX).unwrap();
    let mut passed = Vec::new();
    while passed.len() < 4 {
        passed.push(ahead.next_block().unwrap().unwrap().0);
    }
    let (first, _) = behind.next_block().unwrap().unwrap();
    assert_eq!(first, synced[0].location);
    assert_eq!(passed[3], in_limbo[0].location);

    db.vfs.fail(Op::SyncData, SEGMENT);
    assert_io(&writer.sync().unwrap_err(), "sync", SEGMENT);
    let (second, _) = behind.next_block().unwrap().unwrap();
    assert_eq!(second, synced[1].location);
    assert_eq!(
        ahead.next_block().unwrap().unwrap().0,
        in_limbo[1].location,
        "the segment is whole while its writer lives"
    );

    drop(writer);
    assert_out_of_bounds(&db, &in_limbo, durable);
    let (third, _) = behind.next_block().unwrap().unwrap();
    assert_eq!(third, synced[2].location);
    assert!(behind.next_block().unwrap().is_none());
    assert_eq!(behind.offset(), durable);
    assert!(ahead.next_block().unwrap().is_none());
    assert_eq!(ahead.offset(), in_limbo[2].location.offset);
}

#[test]
fn a_seal_whose_sync_fails_leaves_the_footer_out() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let synced = append_blocks(&mut writer, 5, 1);
    let durable = writer.sync().unwrap();
    let in_limbo = append_blocks(&mut writer, 5, 30);
    let end = writer.end();

    db.vfs.fail(Op::SyncData, SEGMENT);
    assert_io(&writer.seal().unwrap_err(), "sync", SEGMENT);
    assert!(fs::metadata(db.seg_path(1)).unwrap().len() > end);
    assert!(is_failed(writer.seal().unwrap_err()));
    assert!(is_failed(append_one(&mut writer).unwrap_err()));
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, durable)]
    );
    db.assert_readable(&in_limbo);
    db.assert_walk_gives(1, &[synced.clone(), in_limbo.clone()].concat());

    drop(writer);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, durable)]
    );
    db.assert_walk_gives(1, &synced);
    assert_out_of_bounds(&db, &in_limbo, durable);
}

#[test]
fn a_segment_whose_writer_is_gone_stays_open_and_can_still_be_synced() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let blocks = append_blocks(&mut writer, 6, 1);
    let end = writer.end();
    drop(writer);

    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, OPEN, 20)]
    );
    db.assert_readable(&blocks);
    db.segments.sync().unwrap();
    db.commit();
    assert_eq!(db.on_disk(), one_segment(OPEN, end));

    let db = db.reopen();
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, end)]
    );
    db.assert_readable(&blocks);
}

#[test]
fn a_sync_that_fails_after_the_writer_is_gone_closes_the_segment_at_once() {
    let mut db = Db::new();
    let mut writer = db.writer();
    let synced = append_blocks(&mut writer, 6, 1);
    let durable = writer.sync().unwrap();
    let in_limbo = append_blocks(&mut writer, 6, 50);
    drop(writer);

    db.vfs.fail(Op::SyncData, SEGMENT);
    assert_io(&db.segments.sync().unwrap_err(), "sync", SEGMENT);
    assert_eq!(
        db.segments.segments_named().unwrap(),
        [segment(1, CLOSED, durable)]
    );
    db.segments.sync().unwrap();
    db.assert_readable(&synced);
    assert_out_of_bounds(&db, &in_limbo, durable);
    db.segments.drop_segment(id(1)).unwrap();
    db.commit();
    assert_eq!(names(&db.seg_dir()), [""; 0]);
}

/// A segment that is being written has two handles: the store reads through
/// one, and blocks are appended and synced through the other. The second
/// is open while the segment has its writer, and after that until a sync
/// through it has covered every block or a sync of the segment has failed,
/// and no longer.
#[test]
fn the_handle_that_appends_is_open_until_a_sync_has_covered_the_last_block() {
    const NAME: &str = "00000001.seg";
    let open = |db: &Db| db.vfs.open_handles(NAME);

    // A writer that is dropped with blocks no sync has covered leaves its
    // handle to the store, whose sync is made through it.
    let mut db = Db::new();
    let mut writer = db.writer();
    assert_eq!(open(&db), 2);
    let blocks = append_blocks(&mut writer, 2, 1);
    drop(writer);
    assert_eq!(open(&db), 2);
    db.vfs.take_calls();
    db.segments.sync().expect("the blocks are synced");
    assert_eq!(db.vfs.take_calls(), [(Op::SyncData, NAME.to_owned())]);
    assert_eq!(open(&db), 1);
    db.segments.sync().expect("nothing is left to sync");
    assert!(db.vfs.take_calls().is_empty());
    db.commit();
    db.reopen().assert_walk_gives(1, &blocks);

    // One that is dropped with every block synced, or with none, takes
    // the handle with it.
    let mut db = Db::new();
    let mut writer = db.writer();
    append_blocks(&mut writer, 2, 1);
    writer.sync().expect("the blocks are synced");
    assert_eq!(open(&db), 2);
    drop(writer);
    assert_eq!(open(&db), 1);
    let mut db = Db::new();
    drop(db.writer());
    assert_eq!(open(&db), 1);

    // A sealed segment is read through the one handle.
    let mut db = Db::new();
    let mut writer = db.writer();
    append_blocks(&mut writer, 2, 1);
    writer.seal().expect("the segment is sealed");
    drop(writer);
    assert_eq!(open(&db), 1);

    // After a sync that failed nothing is synced through the handle
    // again: it goes with the writer, or at once if the writer is gone.
    let mut db = Db::new();
    let mut writer = db.writer();
    append_blocks(&mut writer, 2, 1);
    db.vfs.fail(Op::SyncData, NAME);
    writer.sync().expect_err("the sync fails");
    drop(writer);
    assert_eq!(open(&db), 1);
    let mut db = Db::new();
    let mut writer = db.writer();
    append_blocks(&mut writer, 2, 1);
    drop(writer);
    db.vfs.fail(Op::SyncData, NAME);
    db.segments.sync().expect_err("the sync fails");
    assert_eq!(open(&db), 1);

    // The record of a segment's syncs keeps the segment, and with it the
    // handle the store reads through. Once the writer of a segment with
    // nothing left to sync is dropped, that is the only one.
    let mut db = Db::new();
    let writer = db.writer();
    let record = writer.sync_record();
    drop(writer);
    assert_eq!(open(&db), 1);
    assert!(!record.failed());
}
