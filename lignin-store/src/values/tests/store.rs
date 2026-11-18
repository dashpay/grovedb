//! The life of a value-log file, and reading and walking its values.

use std::io::ErrorKind;

use lignin_format::{
    value_log::{extent_len, ValueLogError, ValuePointer, MAX_PART_LEN, RECORD_HEADER_LEN},
    CommitVersion, FileHeader, FormatError,
};
use lignin_io::{
    sim::{Event, OpKind, OpRecord, SimVfs},
    Create, OpenMode, StdVfs,
};

use super::{
    faulty::{FaultyVfs, Op},
    sim::*,
    *,
};

/// A database directory with a manifest and a value log, and nothing above
/// them.
pub(super) struct Store {
    pub(super) fs: SimVfs,
    pub(super) manifest: ManifestStore,
    pub(super) values: ValueStore,
}

impl Store {
    pub(super) fn new() -> Self {
        let fs = sim_fs(traced(5, 512));
        let manifest = ManifestStore::create(shared(&fs), db_dir(), empty_manifest()).unwrap();
        let values = ValueStore::open(&manifest, &[]).expect("the value log opens");
        Self {
            fs,
            manifest,
            values,
        }
    }

    /// Ends the session and starts another on the same directory, with
    /// `counts` as the live values.
    pub(super) fn reopen(self, counts: &[FileCount]) -> Self {
        self.try_reopen(counts).expect("the value log opens again")
    }

    pub(super) fn try_reopen(self, counts: &[FileCount]) -> Result<Self, ValueStoreError> {
        let Self {
            fs,
            manifest,
            values,
        } = self;
        drop((manifest, values));
        let manifest = ManifestStore::open(shared(&fs), db_dir())?;
        let values = ValueStore::open(&manifest, counts)?;
        Ok(Self {
            fs,
            manifest,
            values,
        })
    }

    pub(super) fn commit(&mut self) {
        self.values
            .commit(&mut self.manifest)
            .expect("the manifest is replaced");
    }

    /// Makes every value appended so far durable and has the manifest cover
    /// it, as has to be done before a cell's pointer to it is stored.
    pub(super) fn cover(&mut self) {
        self.values.sync().expect("the files are synced");
        self.commit();
    }

    /// A new file that the manifest names, so it takes values.
    pub(super) fn writer(&mut self) -> ValueWriter {
        let writer = self.values.create().expect("a file is created");
        self.commit();
        writer
    }

    pub(super) fn named(&self) -> Vec<LiveFile> {
        run::named_values(self.manifest.manifest().expect("not in doubt"))
    }

    pub(super) fn read(
        &self,
        pointer: ValuePointer,
        owner: u32,
    ) -> Result<Vec<u8>, ValueStoreError> {
        let mut buf = Vec::new();
        Ok(self.values.read(pointer, page(owner), &mut buf)?.to_vec())
    }

    pub(super) fn path(&self, raw_id: u32) -> std::path::PathBuf {
        FileNames::VALUE_LOG.path(db_dir(), file_id(raw_id))
    }

    pub(super) fn exists(&self, raw_id: u32) -> bool {
        self.fs.open(&self.path(raw_id), OpenMode::Read).is_ok()
    }

    /// Applies `change` at `version`, which must count without complaint.
    pub(super) fn apply(&self, change: &ValueChange, version: u64) {
        self.values
            .apply(change, super::version(version))
            .expect("the change is counted");
    }

    /// Makes every change applied so far durable, as a sync of the log does.
    pub(super) fn all_durable(&self) {
        self.values
            .mark_durable(self.values.applied())
            .expect("the store's own mark");
    }

    pub(super) fn drop_empty(&mut self, oldest_pin: u64) -> Vec<u32> {
        let dropped = self
            .values
            .drop_empty(self.values.retire(version(oldest_pin)))
            .expect("writable");
        self.commit();
        dropped.into_iter().map(FileId::get).collect()
    }
}

pub(super) fn bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at as u8).wrapping_mul(31) ^ seed)
        .collect()
}

pub(super) fn acquired(pointers: &[ValuePointer]) -> ValueChange {
    let mut change = ValueChange::new();
    pointers.iter().for_each(|&pointer| change.acquire(pointer));
    change
}

pub(super) fn released(pointers: &[ValuePointer]) -> ValueChange {
    let mut change = ValueChange::new();
    pointers.iter().for_each(|&pointer| change.release(pointer));
    change
}

fn format_error(error: ValueStoreError) -> (u64, FormatError) {
    match error {
        ValueStoreError::Format { offset, source, .. } => (offset, source),
        other => panic!("expected a format error, got {other:?}"),
    }
}

fn ops_since(fs: &SimVfs, op: u64) -> Vec<OpRecord> {
    fs.trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::Op(record) if record.op > op => Some(record),
            _ => None,
        })
        .collect()
}

#[test]
fn a_new_file_takes_values_once_a_manifest_names_it() {
    let mut store = Store::new();
    let mut hold = ValueHold::new();
    let mut writer = store.values.create().expect("a file is created");
    assert_eq!((writer.file_id(), writer.end()), (file_id(1), HEADER));
    assert!(store.exists(1));
    assert!(store.named().is_empty());
    assert!(matches!(
        writer.append(&mut hold, page(7), b"too early"),
        Err(ValueStoreError::NotNamed { file }) if file == file_id(1)
    ));

    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Open, HEADER)]);
    let pointer = writer.append(&mut hold, page(7), b"a value").unwrap();
    assert_eq!(
        pointer,
        ValuePointer::new(file_id(1), HEADER, 7).expect("valid")
    );
    assert_eq!(writer.end(), HEADER + extent_len(7, V1));
    assert_eq!(store.read(pointer, 7).unwrap(), b"a value");

    store.commit();
    assert_eq!(
        store.named(),
        [value_file(1, FileState::Open, HEADER)],
        "a manifest names a file only up to what was synced"
    );
    assert_eq!(writer.sync().unwrap(), writer.end());
    store.commit();
    assert_eq!(
        store.named(),
        [value_file(1, FileState::Open, writer.end())]
    );
    assert_eq!(
        store.manifest.manifest().unwrap().last_file_ids().value_log,
        Some(file_id(1))
    );
}

#[test]
fn a_read_is_one_positional_read_of_the_values_records() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let values: Vec<(u32, Vec<u8>)> = [1, 2, 17, 511, 512, 513, 4096, 70_000]
        .into_iter()
        .enumerate()
        .map(|(at, len)| (1 + at as u32, bytes(len, at as u8)))
        .collect();
    let pointers: Vec<ValuePointer> = values
        .iter()
        .map(|(owner, value)| writer.append(&mut hold, page(*owner), value).unwrap())
        .collect();

    let check = |store: &Store| {
        for ((owner, value), &pointer) in values.iter().zip(&pointers) {
            let before = store.fs.stats().ops;
            assert_eq!(&store.read(pointer, *owner).unwrap(), value);
            let ops = ops_since(&store.fs, before);
            assert_eq!(ops.len(), 1, "{ops:?}");
            let read = &ops[0];
            assert_eq!(read.kind, OpKind::Read);
            assert_eq!(read.path, "/db/val/00000001.val");
            assert_eq!(
                (read.offset, read.len),
                (pointer.offset(), pointer.extent_len(V1))
            );
        }
    };
    check(&store);
    writer.finish().unwrap();
    store.commit();
    check(&store);
    check(&store.reopen(&[]));
}

#[test]
fn a_value_above_what_one_record_holds_is_still_one_read_and_moves_whole() {
    let dir = tempfile::tempdir().unwrap();
    let vfs = FaultyVfs::without_syncs(Arc::new(StdVfs));
    let shared: Arc<dyn Vfs> = Arc::new(vfs.clone());
    let mut manifest = ManifestStore::create(shared, dir.path(), empty_manifest()).unwrap();
    let values = ValueStore::open(&manifest, &[]).unwrap();
    let mut writer = values.create().unwrap();
    let mut cleaner = values.create().unwrap();
    values.commit(&mut manifest).unwrap();

    let value: Vec<u8> = (0..MAX_PART_LEN + 1).map(|at| (at % 251) as u8).collect();
    let mut hold = ValueHold::new();
    let small = writer.append(&mut hold, page(3), b"before").unwrap();
    let pointer = writer.append(&mut hold, page(9), &value).unwrap();
    let after = writer.append(&mut hold, page(3), b"after").unwrap();
    assert_eq!(
        pointer.extent_len(V1),
        (value.len() + 2 * RECORD_HEADER_LEN) as u64
    );
    assert_eq!(after.offset(), pointer.end(V1));
    writer.finish().unwrap();
    values.commit(&mut manifest).unwrap();
    let first = version(1);
    values
        .apply(&acquired(&[small, pointer, after]), first)
        .unwrap();
    drop(hold);

    let mut buf = Vec::new();
    let read_whole = |pointer: ValuePointer, buf: &mut Vec<u8>| {
        vfs.take_calls();
        let read = values.read(pointer, page(9), buf).unwrap();
        assert_eq!(read.parts().count(), 2);
        assert!(read.parts().flatten().eq(value.iter()));
        let name = FileNames::VALUE_LOG.file_name(pointer.file());
        assert_eq!(vfs.take_calls(), [(Op::Read, name)]);
    };
    read_whole(pointer, &mut buf);
    drop(buf);

    struct OnePage(std::cell::Cell<Option<ValuePointer>>);
    impl ValueOwners for OnePage {
        type Found = ();
        fn find(&self, owner: PageId, _: ValuePointer) -> Result<Option<()>, OwnersError> {
            Ok((owner == page(9)).then_some(()))
        }
        fn rewrite(&self, step: PointerRewrite<'_>, (): ()) -> Result<Rewrite, OwnersError> {
            self.0.set(Some(step.to()));
            Ok(Rewrite::Installed(step.install(version(1))))
        }
    }
    let owners = OnePage(std::cell::Cell::new(None));
    let cleaned = match values.clean(pointer.file(), &mut cleaner, &mut manifest, &owners) {
        Err(ValueStoreError::OwnersNotFound { cleaned, .. }) => cleaned,
        other => panic!("the owners of two values are in no page, got {other:?}"),
    };
    assert_eq!((cleaned.moved, cleaned.unowned, cleaned.live), (1, 2, 2));
    let moved = owners.0.get().expect("the rewrite was installed");
    assert_eq!(
        (moved.file(), moved.value_len()),
        (cleaner.file_id(), pointer.value_len())
    );
    read_whole(moved, &mut Vec::new());
}

#[test]
fn a_read_checks_the_pointer_against_the_file_and_the_owner() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let first = writer
        .append(&mut hold, page(7), b"the first value")
        .unwrap();
    let second = writer.append(&mut hold, page(8), b"second").unwrap();
    assert_eq!(store.read(first, 7).unwrap(), b"the first value");

    let (offset, error) = format_error(store.read(first, 8).unwrap_err());
    assert_eq!(offset, first.offset());
    assert_eq!(
        error,
        FormatError::ValueLog(ValueLogError::WrongOwner {
            expected: page(8),
            found: page(7)
        })
    );

    let shorter = ValuePointer::new(file_id(1), first.offset(), 3).unwrap();
    assert!(matches!(
        format_error(store.read(shorter, 7).unwrap_err()).1,
        FormatError::Truncated { .. }
    ));
    let longer = ValuePointer::new(file_id(1), first.offset(), 16).unwrap();
    assert_eq!(
        format_error(store.read(longer, 7).unwrap_err()).1,
        FormatError::ValueLog(ValueLogError::WrongValueLength {
            expected: 16,
            found: 15
        })
    );
    let inside = ValuePointer::new(file_id(1), first.offset() + 4, 6).unwrap();
    assert!(matches!(
        store.read(inside, 7),
        Err(ValueStoreError::Format { .. })
    ));

    let past = ValuePointer::new(file_id(1), second.offset(), 7).unwrap();
    assert!(matches!(
        store.read(past, 8),
        Err(ValueStoreError::OutOfBounds { pointer, end }) if pointer == past && end == writer.end()
    ));
    let far = ValuePointer::new(file_id(1), u64::MAX / 2, u32::MAX).unwrap();
    assert!(matches!(
        store.read(far, 8),
        Err(ValueStoreError::OutOfBounds { .. })
    ));
    let elsewhere = ValuePointer::new(file_id(2), first.offset(), 15).unwrap();
    assert!(matches!(
        store.read(elsewhere, 7),
        Err(ValueStoreError::UnknownFile { file }) if file == file_id(2)
    ));
}

#[test]
fn an_empty_value_or_one_too_long_is_not_appended() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let (offset, error) = format_error(writer.append(&mut hold, page(1), b"").unwrap_err());
    assert_eq!(
        (offset, error),
        (HEADER, FormatError::ValueLog(ValueLogError::EmptyValue))
    );
    assert_eq!(writer.end(), HEADER);
    assert_eq!(hold.files().count(), 0);
    let pointer = writer.append(&mut hold, page(1), b"x").unwrap();
    assert_eq!(pointer.offset(), HEADER);
    assert_eq!(hold.files().collect::<Vec<_>>(), [file_id(1)]);
}

#[test]
fn a_finished_file_is_closed_and_takes_no_more_values() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let pointer = writer.append(&mut hold, page(2), &bytes(900, 1)).unwrap();
    let end = writer.end();
    assert_eq!(writer.finish().unwrap(), end);
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Closed, end)]);
    assert_eq!(store.read(pointer, 2).unwrap(), bytes(900, 1));

    let mut next = store.writer();
    assert_eq!(next.file_id(), file_id(2));
    next.append(&mut hold, page(2), b"goes on").unwrap();
    assert_eq!(
        store.named(),
        [
            value_file(1, FileState::Closed, end),
            value_file(2, FileState::Open, HEADER)
        ]
    );
}

#[test]
fn a_file_whose_writer_was_dropped_is_closed_by_the_sync_that_covers_it() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let pointer = writer.append(&mut hold, page(2), b"kept").unwrap();
    let end = writer.end();
    drop(writer);
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Open, HEADER)]);
    assert_eq!(store.read(pointer, 2).unwrap(), b"kept");

    store.values.sync().unwrap();
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Closed, end)]);
    assert_eq!(store.read(pointer, 2).unwrap(), b"kept");
    store.values.sync().unwrap();

    let unused = store.values.create().unwrap();
    drop(unused);
    store.commit();
    assert_eq!(
        store.named()[1],
        value_file(2, FileState::Closed, HEADER),
        "a file that was never written to is closed as soon as its writer is gone"
    );
}

#[test]
fn several_writers_append_to_their_own_files() {
    let mut store = Store::new();
    let (mut first, mut second) = (store.values.create().unwrap(), store.writer());
    assert_eq!(
        (first.file_id(), second.file_id()),
        (file_id(1), file_id(2))
    );
    let mut hold = ValueHold::new();
    let mut appended = Vec::new();
    for round in 0..40u32 {
        let value = bytes(1 + (round as usize * 37) % 300, round as u8);
        let writer = if round % 3 == 0 {
            &mut first
        } else {
            &mut second
        };
        let pointer = writer.append(&mut hold, page(1 + round), &value).unwrap();
        assert_eq!(pointer.file(), writer.file_id());
        appended.push((pointer, 1 + round, value));
    }
    assert_eq!(hold.files().count(), 2);
    for (pointer, owner, value) in &appended {
        assert_eq!(&store.read(*pointer, *owner).unwrap(), value);
    }
    store.values.sync().unwrap();
    store.commit();
    assert_eq!(
        store.named(),
        [
            value_file(1, FileState::Open, first.end()),
            value_file(2, FileState::Open, second.end())
        ]
    );
}

#[test]
fn a_writer_is_full_at_the_target_length_and_never_splits_a_value() {
    let mut store = Store::new();
    store.values = store.values.with_target_file_len(1000);
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    assert!(!writer.is_full());
    writer.append(&mut hold, page(1), &bytes(900, 0)).unwrap();
    assert!(!writer.is_full());
    let long = writer.append(&mut hold, page(1), &bytes(5000, 1)).unwrap();
    assert!(writer.is_full());
    assert_eq!(store.read(long, 1).unwrap(), bytes(5000, 1));
    assert_eq!(DEFAULT_TARGET_FILE_LEN, 64 << 20, "the default is 64 MiB");
}

#[test]
fn a_later_session_reads_what_the_manifest_covers_and_no_more() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let covered = writer.append(&mut hold, page(4), b"covered").unwrap();
    let valid_len = writer.sync().unwrap();
    store.commit();
    let synced = writer.append(&mut hold, page(4), b"synced only").unwrap();
    writer.sync().unwrap();
    let appended = writer.append(&mut hold, page(4), b"appended only").unwrap();
    assert_eq!(store.read(synced, 4).unwrap(), b"synced only");
    assert_eq!(store.read(appended, 4).unwrap(), b"appended only");
    drop((writer, hold));

    let mut store = store.reopen(&acquired_counts(&[covered]));
    assert_eq!(store.read(covered, 4).unwrap(), b"covered");
    for pointer in [synced, appended] {
        assert!(matches!(
            store.read(pointer, 4),
            Err(ValueStoreError::OutOfBounds { end, .. }) if end == valid_len
        ));
    }
    assert_eq!(store.named(), [value_file(1, FileState::Open, valid_len)]);
    store.commit();
    assert_eq!(
        store.named(),
        [value_file(1, FileState::Closed, valid_len)],
        "the first manifest of a session names an earlier session's file closed"
    );

    let mut writer = store.writer();
    assert_eq!(
        writer.file_id(),
        file_id(2),
        "it is never appended to again"
    );
    let mut hold = ValueHold::new();
    writer.append(&mut hold, page(4), b"a new session").unwrap();
    assert_eq!(
        store
            .fs
            .open(&store.path(1), OpenMode::Read)
            .unwrap()
            .len()
            .unwrap(),
        appended.end(V1),
        "and never truncated"
    );
}

pub(super) fn acquired_counts(pointers: &[ValuePointer]) -> Vec<FileCount> {
    let mut counts: Vec<FileCount> = Vec::new();
    for pointer in pointers {
        let extent = pointer.extent_len(V1);
        match counts.iter_mut().find(|count| count.file == pointer.file()) {
            Some(count) => {
                count.values += 1;
                count.bytes += extent;
            }
            None => counts.push(FileCount {
                file: pointer.file(),
                values: 1,
                bytes: extent,
                miscounted: false,
            }),
        }
    }
    counts
}

fn write_file(fs: &SimVfs, path: &Path, bytes: &[u8]) {
    let file = fs.open(path, OpenMode::Write(Create::IfMissing)).unwrap();
    file.write_at(bytes, 0).unwrap();
}

fn value_log_header(raw_id: u32) -> Vec<u8> {
    FileHeader::new(FileKind::ValueLog, V1, raw_id)
        .expect("a valid header")
        .encode()
        .to_vec()
}

#[test]
fn open_removes_the_files_the_manifest_does_not_name() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let pointer = writer.append(&mut hold, page(1), b"named").unwrap();
    writer.finish().unwrap();
    store.commit();
    let never_named = store.values.create().unwrap();
    assert_eq!(never_named.file_id(), file_id(2));
    let val = FileNames::VALUE_LOG.dir(db_dir());
    let mut stale = value_log_header(1);
    stale.extend_from_slice(&bytes(100, 3));
    write_file(&store.fs, &val.join("00000000.val"), &stale);
    write_file(
        &store.fs,
        &val.join("1.val"),
        b"not a value-log file's name",
    );
    write_file(&store.fs, &val.join("notes.txt"), b"left alone");
    store.fs.create_dir(&val.join("00000009.val")).unwrap();
    drop((never_named, hold));

    let store = store.reopen(&acquired_counts(&[pointer]));
    let names: Vec<_> = store
        .fs
        .read_dir(&val)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "00000000.val",
            "00000001.val",
            "00000009.val",
            "1.val",
            "notes.txt"
        ],
        "only 00000002.val had a value-log file's name and no entry in the manifest"
    );
    assert_eq!(store.read(pointer, 1).unwrap(), b"named");
    assert_eq!(
        store.values.create().unwrap().file_id(),
        file_id(2),
        "the id of a file that no manifest named is free"
    );
}

#[test]
fn open_refuses_a_directory_with_values_the_manifest_cannot_know() {
    let store = Store::new();
    let val = FileNames::VALUE_LOG.dir(db_dir());
    let mut unknown = value_log_header(3);
    unknown.extend_from_slice(&bytes(40, 1));
    write_file(&store.fs, &val.join("00000003.val"), &unknown);
    write_file(&store.fs, &val.join("00000004.val"), &value_log_header(4));

    let fs = store.fs.clone();
    let error = store.try_reopen(&[]).map(drop).unwrap_err();
    assert!(matches!(
        &error,
        ValueStoreError::UnnamedFileHoldsValues { path, len: 60 } if path.ends_with("00000003.val")
    ));
    for name in ["00000003.val", "00000004.val"] {
        assert!(fs.open(&val.join(name), OpenMode::Read).is_ok(), "{name}");
    }
}

#[test]
fn open_checks_every_file_the_manifest_names() {
    let prepared = || {
        let mut store = Store::new();
        let mut writer = store.writer();
        let mut hold = ValueHold::new();
        writer.append(&mut hold, page(1), &bytes(300, 9)).unwrap();
        writer.finish().unwrap();
        store.commit();
        store
    };
    let replace = |store: &Store, bytes: &[u8]| {
        store.fs.remove_file(&store.path(1)).unwrap();
        write_file(&store.fs, &store.path(1), bytes);
    };

    let store = prepared();
    store.fs.remove_file(&store.path(1)).unwrap();
    let error = store.try_reopen(&[]).map(drop).unwrap_err();
    assert!(matches!(
        &error,
        ValueStoreError::Io { op: "open", source, .. } if source.kind() == ErrorKind::NotFound
    ));

    let store = prepared();
    let mut short = value_log_header(1);
    short.extend_from_slice(&bytes(100, 9));
    replace(&store, &short);
    assert!(matches!(
        store.try_reopen(&[]).map(drop).unwrap_err(),
        ValueStoreError::FileTooShort {
            valid_len: 338,
            len: 120,
            ..
        }
    ));

    let store = prepared();
    let mut other_id = value_log_header(2);
    other_id.resize(338, 0);
    replace(&store, &other_id);
    assert!(matches!(
        store.try_reopen(&[]).map(drop).unwrap_err(),
        ValueStoreError::WrongFileId { found: 2, .. }
    ));

    let store = prepared();
    let mut segment = FileHeader::new(FileKind::Segment, V1, 1)
        .unwrap()
        .encode()
        .to_vec();
    segment.resize(338, 0);
    replace(&store, &segment);
    assert_eq!(
        format_error(store.try_reopen(&[]).map(drop).unwrap_err()),
        (
            0,
            FormatError::ValueLog(ValueLogError::WrongFileKind {
                found: FileKind::Segment
            })
        )
    );

    let store = prepared();
    replace(&store, &vec![0xEE; 338]);
    assert_eq!(
        format_error(store.try_reopen(&[]).map(drop).unwrap_err()),
        (0, FormatError::BadMagic)
    );
}

#[test]
fn a_read_only_store_reads_and_changes_nothing() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let pointer = writer.append(&mut hold, page(6), b"read only").unwrap();
    writer.finish().unwrap();
    store.commit();
    let unnamed = store.values.create().unwrap();
    drop(unnamed);

    let manifest = read_manifest(&store.fs, db_dir()).unwrap();
    let before = store.fs.fingerprint();
    let reader = ValueStore::open_read_only(shared(&store.fs), db_dir(), &manifest).unwrap();
    let mut buf = Vec::new();
    assert_eq!(
        reader.read(pointer, page(6), &mut buf).unwrap().to_vec(),
        b"read only"
    );
    assert_eq!(reader.dir(), db_dir());
    assert_eq!(reader.files_named().unwrap(), store.named());
    assert!(reader.counts().is_empty());
    let first = version(1);
    assert!(matches!(reader.create(), Err(ValueStoreError::ReadOnly)));
    assert!(matches!(
        reader.apply(&acquired(&[pointer]), first),
        Err(ValueStoreError::ReadOnly)
    ));
    assert!(matches!(
        reader.drop_empty(reader.retire(first)),
        Err(ValueStoreError::ReadOnly)
    ));
    assert!(matches!(reader.step(), Err(ValueStoreError::ReadOnly)));
    assert!(matches!(
        reader.manifest_replaced(&store.manifest),
        Err(ValueStoreError::ReadOnly)
    ));
    assert!(matches!(
        reader.commit(&mut store.manifest),
        Err(ValueStoreError::ReadOnly)
    ));
    reader.sync().unwrap();
    assert_eq!(store.fs.fingerprint(), before);
    assert!(store.exists(2), "the unnamed file is still there");
}

#[test]
fn file_ids_go_up_and_are_never_given_twice() {
    let mut store = Store::new();
    let mut hold = ValueHold::new();
    for expected in 1..=3 {
        let mut writer = store.writer();
        assert_eq!(writer.file_id(), file_id(expected));
        writer.append(&mut hold, page(1), b"dead").unwrap();
        writer.finish().unwrap();
    }
    drop(hold);
    store.all_durable();
    assert_eq!(store.drop_empty(0), [1, 2, 3]);
    assert!(store.named().is_empty());
    assert!(!store.exists(1) && !store.exists(2) && !store.exists(3));

    let mut store = store.reopen(&[]);
    assert_eq!(store.writer().file_id(), file_id(4));

    let fs = sim_fs(traced(1, 512));
    let last = Manifest::new(
        lignin_format::manifest::LastFileIds {
            value_log: Some(FileId::MAX),
            ..Default::default()
        },
        None,
        Vec::new(),
    )
    .unwrap();
    let manifest = ManifestStore::create(shared(&fs), db_dir(), last).unwrap();
    let values = ValueStore::open(&manifest, &[]).unwrap();
    assert!(matches!(
        values.create(),
        Err(ValueStoreError::FileIdsExhausted)
    ));
}

#[test]
fn the_manifest_keeps_what_the_value_log_does_not_own() {
    let mut store = Store::new();
    let segment = LiveFile::new(FileKind::Segment, file_id(5), FileState::Closed, 20).unwrap();
    let last = lignin_format::manifest::LastFileIds {
        segment: Some(file_id(9)),
        wal: Some(file_id(2)),
        ..Default::default()
    };
    let with_segment = Manifest::new(last, None, vec![segment]).unwrap();
    store
        .manifest
        .update(|_| Ok::<_, ValueStoreError>(with_segment))
        .unwrap();

    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    writer.append(&mut hold, page(1), b"value").unwrap();
    let end = writer.finish().unwrap();
    store.commit();
    let manifest = store.manifest.manifest().unwrap();
    assert_eq!(
        manifest.files(),
        [segment, value_file(1, FileState::Closed, end)]
    );
    assert_eq!(
        manifest.last_file_ids(),
        lignin_format::manifest::LastFileIds {
            value_log: Some(file_id(1)),
            ..last
        }
    );
    assert_eq!(store.values.files_named().unwrap(), store.named());
}

#[test]
fn a_manifest_of_another_directory_is_refused() {
    let store = Store::new();
    let other = Path::new("/other");
    store.fs.create_dir(other).unwrap();
    let mut elsewhere = ManifestStore::create(shared(&store.fs), other, empty_manifest()).unwrap();
    for error in [
        store.values.manifest_replaced(&elsewhere).unwrap_err(),
        store.values.commit(&mut elsewhere).unwrap_err(),
    ] {
        assert!(matches!(
            error,
            ValueStoreError::WrongDirectory { expected, found }
                if expected == db_dir() && found == other
        ));
    }
}

fn walked(store: &Store, raw_id: u32) -> Vec<(ValuePointer, u32, Vec<u8>)> {
    let mut walk = store
        .values
        .walk(file_id(raw_id))
        .expect("the file is walked");
    let mut values = Vec::new();
    while let Some((pointer, value)) = walk.next_value().expect("a whole value") {
        values.push((pointer, value.owner().get(), value.to_vec()));
    }
    let end = values
        .last()
        .map_or(HEADER, |(pointer, ..)| pointer.end(V1));
    assert_eq!(walk.offset(), end);
    values
}

#[test]
fn a_walk_gives_every_value_with_the_pointer_to_it() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    assert!(walked(&store, 1).is_empty());
    let mut appended = Vec::new();
    for round in 0..300u32 {
        let len = match round % 7 {
            0 => 1,
            1 => 400_000,
            2 => 5000,
            _ => 1 + (round as usize * 131) % 700,
        };
        let value = bytes(len, round as u8);
        let owner = 1 + round % 11;
        let pointer = writer.append(&mut hold, page(owner), &value).unwrap();
        appended.push((pointer, owner, value));
    }
    assert_eq!(walked(&store, 1), appended);

    let mut walk = store.values.walk(file_id(1)).unwrap();
    writer
        .append(&mut hold, page(1), b"after the walk began")
        .unwrap();
    let mut count = 0;
    while walk.next_value().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(
        count,
        appended.len(),
        "values appended later are not part of it"
    );
    assert!(matches!(
        store.values.walk(file_id(2)),
        Err(ValueStoreError::UnknownFile { .. })
    ));
}

#[test]
fn a_walk_ends_with_an_error_at_the_first_value_that_is_not_whole() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let first = writer.append(&mut hold, page(1), &bytes(100, 1)).unwrap();
    let second = writer.append(&mut hold, page(2), &bytes(200, 2)).unwrap();
    let third = writer.append(&mut hold, page(3), &bytes(300, 3)).unwrap();
    writer.finish().unwrap();
    store.commit();
    drop(hold);

    let damage = |store: &Store, at: u64| {
        let file = store
            .fs
            .open(&store.path(1), OpenMode::Write(Create::Never))
            .unwrap();
        let mut byte = [0];
        file.read_exact_at(&mut byte, at).unwrap();
        file.write_at(&[byte[0] ^ 0x40], at).unwrap();
    };
    damage(&store, second.offset() + 30);
    let mut walk = store.values.walk(file_id(1)).unwrap();
    let (pointer, value) = walk.next_value().unwrap().expect("the first value");
    assert_eq!((pointer, value.to_vec()), (first, bytes(100, 1)));
    let (offset, error) = format_error(walk.next_value().map(drop).unwrap_err());
    assert_eq!(offset, second.offset());
    assert!(matches!(error, FormatError::ChecksumMismatch { .. }));
    assert_eq!(walk.offset(), second.offset());
    assert_eq!(store.read(third, 3).unwrap(), bytes(300, 3));
    assert_eq!(store.read(first, 1).unwrap(), bytes(100, 1));
}

#[test]
fn a_walk_rejects_a_file_that_ends_inside_a_value() {
    for (cut, needed) in [(1, 8), (7, 8), (8, 418), (18, 418), (200, 418)] {
        let fs = sim_fs(traced(2, 512));
        let val = FileNames::VALUE_LOG.dir(db_dir());
        fs.create_dir(&val).unwrap();
        let mut content = value_log_header(1);
        lignin_format::value_log::write_value(page(1), &bytes(50, 0), &mut content).unwrap();
        let whole = content.len() as u64;
        lignin_format::value_log::write_value(page(2), &bytes(400, 1), &mut content).unwrap();
        content.truncate(whole as usize + cut);
        write_file(&fs, &val.join("00000001.val"), &content);
        let named = Manifest::new(
            lignin_format::manifest::LastFileIds {
                value_log: Some(file_id(1)),
                ..Default::default()
            },
            None,
            vec![value_file(1, FileState::Closed, content.len() as u64)],
        )
        .unwrap();
        let manifest = ManifestStore::create(shared(&fs), db_dir(), named).unwrap();
        let values = ValueStore::open(&manifest, &[]).unwrap();

        let mut walk = values.walk(file_id(1)).unwrap();
        assert!(walk.next_value().unwrap().is_some());
        let (offset, error) = format_error(walk.next_value().map(drop).unwrap_err());
        assert_eq!(offset, whole);
        assert_eq!(
            error,
            FormatError::Truncated {
                needed,
                available: cut
            }
        );
    }
}

#[test]
fn a_walk_rejects_bytes_that_are_no_value() {
    let fs = sim_fs(traced(2, 512));
    let val = FileNames::VALUE_LOG.dir(db_dir());
    fs.create_dir(&val).unwrap();
    let mut content = value_log_header(1);
    content.extend_from_slice(&[0; 64]);
    write_file(&fs, &val.join("00000001.val"), &content);
    let named = Manifest::new(
        lignin_format::manifest::LastFileIds {
            value_log: Some(file_id(1)),
            ..Default::default()
        },
        None,
        vec![value_file(1, FileState::Closed, content.len() as u64)],
    )
    .unwrap();
    let manifest = ManifestStore::create(shared(&fs), db_dir(), named).unwrap();
    let values = ValueStore::open(&manifest, &[]).unwrap();
    let mut walk = values.walk(file_id(1)).unwrap();
    let (offset, error) = format_error(walk.next_value().map(drop).unwrap_err());
    assert_eq!(offset, HEADER);
    assert!(
        matches!(error, FormatError::ChecksumMismatch { .. }),
        "{error:?}"
    );
}

#[test]
fn the_store_is_shared_between_threads_and_a_writer_is_sent_to_one() {
    fn shared_between_threads<T: Send + Sync>() {}
    fn sent_to_a_thread<T: Send>() {}
    shared_between_threads::<ValueStore>();
    shared_between_threads::<ValueHold>();
    sent_to_a_thread::<ValueWriter>();
    sent_to_a_thread::<ValueWalk>();
    sent_to_a_thread::<ValueRecovery>();
    sent_to_a_thread::<ValueStoreError>();
}

#[test]
fn values_are_read_while_they_are_appended_counted_and_synced() {
    const WRITERS: u32 = 3;
    const VALUES: u32 = 400;
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(FaultyVfs::without_syncs(Arc::new(StdVfs)));
    let mut manifest = ManifestStore::create(vfs, dir.path(), empty_manifest()).unwrap();
    let values = ValueStore::open(&manifest, &[]).unwrap();
    let writers: Vec<ValueWriter> = (0..WRITERS).map(|_| values.create().unwrap()).collect();
    values.commit(&mut manifest).unwrap();
    let value =
        |writer: u32, at: u32| bytes(1 + ((writer * 97 + at * 13) % 900) as usize, at as u8);

    let (sender, receiver) = std::sync::mpsc::channel::<(u32, u32, ValuePointer)>();
    std::thread::scope(|scope| {
        let mut appending = Vec::new();
        for (number, mut writer) in writers.into_iter().enumerate() {
            let sender = sender.clone();
            let number = number as u32;
            appending.push(scope.spawn(move || {
                let mut hold = ValueHold::new();
                for at in 0..VALUES {
                    let owner = page(1 + number);
                    let pointer = writer.append(&mut hold, owner, &value(number, at)).unwrap();
                    sender.send((number, at, pointer)).unwrap();
                    if at % 50 == 0 {
                        writer.sync().unwrap();
                    }
                }
                writer.finish().unwrap();
                hold
            }));
        }
        drop(sender);
        let values = &values;
        scope.spawn(move || {
            for _ in 0..200 {
                values.sync().unwrap();
                let _ = values.counts();
                let _ = values.cleanable(0);
                let dropped = values
                    .drop_empty(values.retire(CommitVersion::MAX))
                    .unwrap();
                assert!(dropped.is_empty(), "every file is held or being written");
            }
        });
        let mut buf = Vec::new();
        let mut uncounted = Vec::new();
        let mut seen = 0;
        let count = |uncounted: &mut Vec<ValuePointer>, manifest: &mut ManifestStore| {
            values.sync().unwrap();
            values.commit(manifest).unwrap();
            let first = version(1);
            values.apply(&acquired(uncounted), first).unwrap();
            uncounted.clear();
        };
        for (number, at, pointer) in receiver {
            let read = values.read(pointer, page(1 + number), &mut buf).unwrap();
            assert_eq!(read.to_vec(), value(number, at));
            uncounted.push(pointer);
            seen += 1;
            if uncounted.len() == 64 {
                count(&mut uncounted, &mut manifest);
            }
        }
        count(&mut uncounted, &mut manifest);
        assert_eq!(seen, WRITERS * VALUES);
        let holds: Vec<ValueHold> = appending
            .into_iter()
            .map(|thread| thread.join().expect("the writer ran to its end"))
            .collect();
        assert_eq!(holds.len() as u32, WRITERS);
    });

    values.commit(&mut manifest).unwrap();
    let counts = values.counts();
    assert_eq!(
        counts.iter().map(|count| count.values).collect::<Vec<_>>(),
        [u64::from(VALUES); WRITERS as usize]
    );
    for named in values.files_named().unwrap() {
        assert_eq!(named.state(), FileState::Closed);
    }
    let mut walked = 0;
    for raw_id in 1..=WRITERS {
        let mut walk = values.walk(file_id(raw_id)).unwrap();
        while walk.next_value().unwrap().is_some() {
            walked += 1;
        }
    }
    assert_eq!(walked, WRITERS * VALUES);
}

/// A store with a closed file 1 of three values, the first `counted` of
/// them counted, the open file 2 of a cleaner, and the pointers to the
/// values.
pub(super) fn three_values(counted: usize) -> (Store, ValueWriter, Vec<ValuePointer>) {
    let mut store = Store::new();
    let mut writer = store.writer();
    let cleaner = store.writer();
    let mut hold = ValueHold::new();
    let pointers: Vec<ValuePointer> = (0..3)
        .map(|at| writer.append(&mut hold, page(1), &bytes(30, at)).unwrap())
        .collect();
    writer.finish().unwrap();
    store.commit();
    store.apply(&acquired(&pointers[..counted]), 1);
    (store, cleaner, pointers)
}

/// A file layer that stops the creation of one file until it is told to go
/// on.
#[derive(Debug, Clone)]
struct Pausing {
    inner: SimVfs,
    name: &'static str,
    reached: std::sync::mpsc::Sender<()>,
    resume: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl Vfs for Pausing {
    fn open(&self, path: &Path, mode: OpenMode) -> std::io::Result<Box<dyn lignin_io::VfsFile>> {
        if mode == OpenMode::Append(Create::New) && path.ends_with(self.name) {
            self.reached.send(()).expect("the test waits");
            self.resume
                .lock()
                .unwrap()
                .recv()
                .expect("the test goes on");
        }
        self.inner.open(path, mode)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.inner.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> std::io::Result<()> {
        self.inner.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<lignin_io::DirEntry>> {
        self.inner.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> std::io::Result<Box<dyn lignin_io::VfsLock>> {
        self.inner.lock(path)
    }
}

#[test]
fn a_manifest_written_while_a_file_is_created_does_not_use_up_its_id() {
    let (reached, was_reached) = std::sync::mpsc::channel();
    let (resume, resumed) = std::sync::mpsc::channel();
    let pausing = Pausing {
        inner: sim_fs(traced(6, 512)),
        name: "00000001.val",
        reached,
        resume: Arc::new(std::sync::Mutex::new(resumed)),
    };
    let mut manifest =
        ManifestStore::create(Arc::new(pausing.clone()), db_dir(), empty_manifest()).unwrap();
    let values = ValueStore::open(&manifest, &[]).unwrap();

    std::thread::scope(|scope| {
        let creating = scope.spawn(|| values.create());
        was_reached.recv().expect("the creation began");
        let committed = values.commit(&mut manifest);
        let recorded = manifest.manifest().unwrap().last_file_ids().value_log;
        resume.send(()).unwrap();
        let created = creating.join().expect("the creation ran");
        committed.unwrap();
        assert_eq!(
            recorded, None,
            "the id of a file that is not in the store yet is not recorded"
        );
        let mut writer = created.expect("the file is created");
        assert_eq!(writer.file_id(), file_id(1));

        values
            .commit(&mut manifest)
            .expect("the manifest names the new file");
        assert_eq!(
            run::named_values(manifest.manifest().unwrap()),
            [value_file(1, FileState::Open, HEADER)]
        );
        let mut hold = ValueHold::new();
        writer.append(&mut hold, page(1), b"named").unwrap();
    });
}

#[test]
fn a_file_is_not_dropped_between_a_writers_hold_and_its_last_value() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let held = std::rc::Rc::new(std::cell::RefCell::new(None));
    let appended = held.clone();
    crate::values::file::between_checks::set(move || {
        let mut hold = ValueHold::new();
        let pointer = writer.append(&mut hold, page(1), b"held").unwrap();
        writer.finish().unwrap();
        *appended.borrow_mut() = Some((hold, pointer));
    });

    store.all_durable();
    assert!(
        store.drop_empty(0).is_empty(),
        "the file was open when the store asked, and held when it was closed"
    );
    let (hold, pointer) = held.borrow_mut().take().expect("the writer ran");
    assert_eq!(store.read(pointer, 1).unwrap(), b"held");
    assert_eq!(hold.files().collect::<Vec<_>>(), [file_id(1)]);
    assert!(store.drop_empty(0).is_empty());
    drop(hold);
    assert_eq!(store.drop_empty(0), [1]);
}
