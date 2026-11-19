//! Log files (`log-format.md`): `read_file_start`, `scan` and what it gives,
//! the group readers, and `LogCursor` driven by `record_len` as a reader
//! that fetches one record at a time, which must find the same end of the
//! valid part as `scan`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    wal::{
        self, read_file_start, read_pending_value, record_len, write_file_start, Change,
        CheckpointMark, Entry, Group, GroupWriter, LogCursor, LogItem, LogStart, LoggedValue,
    },
    FileHeader, FormatVersion,
};
use lignin_format_fuzz::{default_frame_len, ok, sealed_file, show, Input};

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    let file = sealed_file(&input, default_frame_len);
    let started = ok(read_file_start(&file));
    let Some(mut scan) = ok(wal::scan(&file)) else {
        assert!(started.is_none());
        return;
    };
    let (header, start) = started.expect("a file that scans");
    assert_eq!((scan.header(), scan.start()), (header, start));
    let id = header.file_id().expect("a log file has an id");
    let mut written = Vec::new();
    write_file_start(id, &start, &mut written).expect("a start that was read");
    assert_eq!(written, file[..wal::FIRST_ITEM_OFFSET as usize]);

    let mut at = scan.offset();
    assert_eq!(at, wal::FIRST_ITEM_OFFSET);
    while let Some(item) = scan.next() {
        let end = scan.offset();
        let bytes = &file[at as usize..end as usize];
        match item {
            LogItem::Commit(group) => commit(&file, &start, at, bytes, &group),
            LogItem::Change(change) => self::change(bytes, &change),
            LogItem::Mark(mark) => {
                let mut written = Vec::new();
                CheckpointMark::new(mark.covers()).write(&mut written).expect("a mark");
                assert_eq!(written, bytes);
            }
        }
        at = end;
    }
    let tail = scan.tail();
    assert_eq!(tail.valid_len(), at);
    if let Some(failure) = tail.failure() {
        show(&failure.error);
        assert!(failure.offset >= tail.valid_len());
    } else {
        assert_eq!(tail.valid_len(), file.len() as u64);
    }
    if let Some(next) = ok(start.next(tail.valid_len(), tail.latest())) {
        assert_eq!(ok(start.check_next(tail.valid_len(), tail.latest(), &next)), Some(()));
        let other = LogStart::new(next.base() ^ 1, next.after()).expect("a base");
        assert!(ok(start.check_next(tail.valid_len(), tail.latest(), &other)).is_none());
    }
    cursor(&file, &header, &start, tail.valid_len(), tail.latest());
});

/// Reads the file record by record, as a reader that fetches each record by
/// the length its frame states.
fn cursor(
    file: &[u8],
    header: &FileHeader,
    start: &LogStart,
    valid_len: u64,
    latest: lignin_format::CommitVersion,
) {
    let version = header.version();
    let mut cursor = LogCursor::new(header, start).expect("a log file");
    assert_eq!(cursor.offset(), wal::FIRST_ITEM_OFFSET);
    loop {
        let rest = &file[cursor.offset() as usize..];
        if rest.is_empty() {
            let _ = ok(cursor.finish());
            break;
        }
        let Some(len) = ok(record_len(rest, version)) else {
            break;
        };
        let record = &rest[..len.min(rest.len())];
        let before = cursor.clone();
        match ok(cursor.step(record)) {
            Some(step) => {
                assert_eq!(step.len, len);
                assert_eq!(cursor.offset(), before.offset() + len as u64);
                assert_eq!(step.ends_item, !cursor.in_group());
                let (decoded, decoded_len) = wal::Record::decode(record, version).expect("a step");
                assert_eq!((decoded, decoded_len), (step.record, step.len));
                let _ = step.record.kind_code();
            }
            None => {
                assert_eq!(cursor, before, "a refused step moved the cursor");
                break;
            }
        }
        assert!(cursor.item_offset() <= cursor.offset());
    }
    assert_eq!(cursor.valid_len(), valid_len);
    assert_eq!(cursor.latest(), latest);
}

fn commit(file: &[u8], start: &LogStart, at: u64, bytes: &[u8], group: &Group<'_>) {
    assert_eq!(group.as_bytes(), bytes);
    assert_eq!(group.encoded_len(), bytes.len());
    assert_eq!(start.offset_of(group.address()), Some(at));

    let mut written = Vec::new();
    let mut writer = GroupWriter::new(group.version(), group.address(), &mut written)
        .expect("a group that was read");
    for (address, value) in group.values() {
        let offset = start.offset_of(address).expect("an address in the file") as usize;
        let pending = match value {
            LoggedValue::Bytes(bytes) => {
                let whole = bytes.to_vec();
                assert_eq!(whole.len(), bytes.value_len() as usize);
                if let Some(contiguous) = bytes.contiguous() {
                    assert_eq!(contiguous, whole);
                }
                assert_eq!(bytes.parts().map(<[u8]>::len).sum::<usize>(), whole.len());
                writer.value(bytes.owner(), &whole)
            }
            LoggedValue::Carried(carried) => {
                let _ = (carried.source_owner(), carried.source());
                writer.carried(&carried)
            }
        };
        let pending = pending.expect("a value that was read");
        assert_eq!(pending.value_len(), value.value_len());
        let read = read_pending_value(&pending, &file[offset..], value.owner(), FormatVersion::V1);
        assert_eq!(ok(read), Some(value));
        let _ = (value.bytes(), value.carried());
    }
    for entry in group.entries() {
        let _ = entry.page();
        match entry {
            Entry::Tree(run) => writer.tree_run(&run),
            Entry::Flat(run) => writer.flat_run(&run),
            Entry::Freed(page) => writer.freed(page),
            Entry::Released(pointer) => writer.released(&pointer),
            Entry::Acquired(pointer) => writer.acquired(&pointer),
        }
        .expect("an entry that was read");
    }
    assert_eq!(writer.finish(), bytes.len());
    assert_eq!(written, bytes);
}

fn change(bytes: &[u8], change: &Change<'_>) {
    let _ = change.page();
    if let Change::Reclaim(reclaim) = change {
        let _ = (reclaim.removed().map(|removed| removed.page()), reclaim.freed());
        assert_eq!(reclaim.released().count(), reclaim.released().len());
    }
    if let Change::Value(value) = change {
        let _ = (value.slot(), value.from(), value.to(), value.released());
        let _ = (value.acquired(), value.delta());
    }
    let mut written = Vec::new();
    change.write(&mut written).expect("a change that was read");
    assert_eq!(written, bytes);
}
