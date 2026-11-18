//! The conformance suite: what every [`Vfs`] backend must do.
//!
//! A backend runs it from its own tests with [`conformance_tests!`], which
//! makes one test out of each case here. A case gets the backend and an empty
//! directory of its own.

#![deny(dead_code)]

use std::{
    ffi::OsString,
    fmt::Debug,
    io::{self, ErrorKind},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Barrier,
    },
    thread,
};

use crate::vfs::{Create, DirEntry, EntryKind, OpenMode, Vfs, VfsFile};

/// Makes a test of every conformance case. `$setup` is evaluated once per
/// test and gives `(vfs, root, cleanup)`: the backend, an empty directory in
/// it, and a value kept alive until the test ends.
macro_rules! conformance_tests {
    ($setup:expr) => {
        $crate::conformance::conformance_tests! {
            @cases $setup;
            open_of_a_missing_file_is_not_found,
            open_of_a_directory_is_refused,
            create_new_refuses_an_existing_file,
            create_if_missing_keeps_an_existing_file,
            write_at_round_trips_and_overwrites,
            write_at_past_the_end_leaves_zeros,
            read_at_stops_at_the_end_of_the_file,
            read_exact_at_needs_every_byte,
            large_unaligned_io_round_trips,
            append_returns_consecutive_offsets,
            append_starts_at_the_end_of_an_existing_file,
            concurrent_appends_land_whole,
            reads_run_alongside_appends,
            handles_refuse_operations_outside_their_mode,
            an_append_handle_is_the_only_writer,
            set_len_shrinks_and_grows,
            append_follows_set_len,
            sync_succeeds_in_every_mode,
            handles_share_one_file,
            rename_moves_a_file,
            rename_replaces_the_destination,
            rename_of_a_missing_file_is_not_found,
            #[cfg_attr(miri, ignore = "Miri has no hard links")]
            rename_between_names_of_one_file_changes_nothing,
            #[cfg_attr(miri, ignore = "Miri has no hard links")]
            hard_link_is_another_name_for_the_same_file,
            #[cfg_attr(miri, ignore = "Miri has no hard links")]
            hard_link_refuses_a_taken_name_and_a_missing_file,
            #[cfg_attr(miri, ignore = "Miri has no hard links")]
            hard_link_outlives_the_original_name,
            remove_file_removes_the_name_only,
            create_dir_needs_its_parent_and_a_free_name,
            remove_dir_needs_an_empty_directory,
            read_dir_lists_sorted_names_with_kinds,
            sync_dir_takes_directories_only,
            lock_excludes_until_it_is_dropped,
            lock_creates_its_file_and_leaves_it,
            locks_on_different_files_are_independent,
            one_thread_wins_a_contended_lock,
            a_file_is_replaced_by_write_sync_rename_sync_dir,
            #[cfg_attr(miri, ignore = "Miri has no hard links")]
            a_checkpoint_is_a_directory_of_hard_links,
        }
    };
    (@cases $setup:expr; $($(#[$attribute:meta])* $case:ident),* $(,)?) => {
        $(
            #[test]
            $(#[$attribute])*
            fn $case() {
                let (vfs, root, _cleanup) = $setup;
                $crate::conformance::$case(&vfs, &root);
            }
        )*
    };
}
pub(crate) use conformance_tests;

const ITERATIONS: u64 = if cfg!(miri) { 12 } else { 300 };

fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn byte_at(offset: u64) -> u8 {
    (offset.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8
}

#[track_caller]
fn assert_fails<T: Debug>(result: io::Result<T>, expected: ErrorKind) {
    assert_eq!(result.unwrap_err().kind(), expected);
}

#[track_caller]
fn open(vfs: &dyn Vfs, path: &Path, mode: OpenMode) -> Box<dyn VfsFile> {
    vfs.open(path, mode).unwrap()
}

#[track_caller]
fn write_new(vfs: &dyn Vfs, path: &Path, content: &[u8]) {
    open(vfs, path, OpenMode::Write(Create::New))
        .write_at(content, 0)
        .unwrap();
}

#[track_caller]
fn read_all(file: &dyn VfsFile) -> Vec<u8> {
    let mut content = vec![0; file.len().unwrap() as usize];
    file.read_exact_at(&mut content, 0).unwrap();
    content
}

#[track_caller]
fn read_path(vfs: &dyn Vfs, path: &Path) -> Vec<u8> {
    read_all(&*open(vfs, path, OpenMode::Read))
}

fn entry(name: &str, kind: EntryKind) -> DirEntry {
    DirEntry {
        name: OsString::from(name),
        kind,
    }
}

fn names(vfs: &dyn Vfs, dir: &Path) -> Vec<OsString> {
    vfs.read_dir(dir)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

pub(crate) fn open_of_a_missing_file_is_not_found(vfs: &dyn Vfs, root: &Path) {
    let missing = root.join("missing");
    for mode in [
        OpenMode::Read,
        OpenMode::Write(Create::Never),
        OpenMode::Append(Create::Never),
    ] {
        assert_fails(vfs.open(&missing, mode), ErrorKind::NotFound);
    }

    let in_missing_dir = root.join("missing").join("file");
    for create in [Create::Never, Create::IfMissing, Create::New] {
        for mode in [OpenMode::Write(create), OpenMode::Append(create)] {
            assert_fails(vfs.open(&in_missing_dir, mode), ErrorKind::NotFound);
        }
    }
    assert!(vfs.read_dir(root).unwrap().is_empty());
}

pub(crate) fn open_of_a_directory_is_refused(vfs: &dyn Vfs, root: &Path) {
    let dir = root.join("dir");
    vfs.create_dir(&dir).unwrap();
    for mode in [
        OpenMode::Read,
        OpenMode::Write(Create::Never),
        OpenMode::Write(Create::IfMissing),
        OpenMode::Append(Create::Never),
        OpenMode::Append(Create::IfMissing),
    ] {
        assert_fails(vfs.open(&dir, mode), ErrorKind::IsADirectory);
    }
    assert_eq!(vfs.read_dir(root).unwrap(), [entry("dir", EntryKind::Dir)]);
}

pub(crate) fn create_new_refuses_an_existing_file(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    let file = open(vfs, &path, OpenMode::Write(Create::New));
    assert_eq!(file.len().unwrap(), 0);
    file.write_at(b"kept", 0).unwrap();

    for mode in [OpenMode::Write(Create::New), OpenMode::Append(Create::New)] {
        assert_fails(vfs.open(&path, mode), ErrorKind::AlreadyExists);
    }
    assert_eq!(read_path(vfs, &path), b"kept");
}

pub(crate) fn create_if_missing_keeps_an_existing_file(vfs: &dyn Vfs, root: &Path) {
    let written = root.join("written");
    let file = open(vfs, &written, OpenMode::Write(Create::IfMissing));
    assert_eq!(file.len().unwrap(), 0);
    file.write_at(b"kept", 0).unwrap();
    drop(file);
    for mode in [
        OpenMode::Write(Create::IfMissing),
        OpenMode::Append(Create::IfMissing),
    ] {
        assert_eq!(read_all(&*open(vfs, &written, mode)), b"kept");
    }

    let appended = root.join("appended");
    let file = open(vfs, &appended, OpenMode::Append(Create::IfMissing));
    assert_eq!(file.append(b"new").unwrap(), 0);
    assert_eq!(read_path(vfs, &appended), b"new");
}

pub(crate) fn write_at_round_trips_and_overwrites(vfs: &dyn Vfs, root: &Path) {
    let file = open(vfs, &root.join("file"), OpenMode::Write(Create::New));
    let mut expected = bytes(10_000, 1);
    file.write_at(&expected, 0).unwrap();
    assert_eq!(read_all(&*file), expected);

    let patch = bytes(3_000, 2);
    file.write_at(&patch, 4_500).unwrap();
    expected[4_500..7_500].copy_from_slice(&patch);
    assert_eq!(read_all(&*file), expected);

    let tail = bytes(1_000, 3);
    file.write_at(&tail, 9_500).unwrap();
    expected.truncate(9_500);
    expected.extend_from_slice(&tail);
    assert_eq!(file.len().unwrap(), 10_500);
    assert_eq!(read_all(&*file), expected);

    file.write_at(&[], 20_000).unwrap();
    assert_eq!(file.len().unwrap(), 10_500);
}

pub(crate) fn write_at_past_the_end_leaves_zeros(vfs: &dyn Vfs, root: &Path) {
    let file = open(vfs, &root.join("file"), OpenMode::Write(Create::New));
    file.write_at(b"head", 0).unwrap();
    file.write_at(b"tail", 5_000).unwrap();
    assert_eq!(file.len().unwrap(), 5_004);

    let content = read_all(&*file);
    assert_eq!(&content[..4], b"head");
    assert!(content[4..5_000].iter().all(|&byte| byte == 0));
    assert_eq!(&content[5_000..], b"tail");
}

pub(crate) fn read_at_stops_at_the_end_of_the_file(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    let content = bytes(1_000, 4);
    write_new(vfs, &path, &content);
    let file = open(vfs, &path, OpenMode::Read);

    let mut buf = [0; 300];
    assert_eq!(file.read_at(&mut buf, 0).unwrap(), 300);
    assert_eq!(buf, content[..300]);
    assert_eq!(file.read_at(&mut buf, 900).unwrap(), 100);
    assert_eq!(buf[..100], content[900..]);
    assert_eq!(file.read_at(&mut buf, 1_000).unwrap(), 0);
    assert_eq!(file.read_at(&mut buf, 1_000_000).unwrap(), 0);
    for beyond_any_file in [i64::MAX as u64 - 1, i64::MAX as u64, u64::MAX] {
        assert_eq!(file.read_at(&mut buf, beyond_any_file).unwrap(), 0);
    }
    assert_eq!(file.read_at(&mut [], 0).unwrap(), 0);
}

pub(crate) fn read_exact_at_needs_every_byte(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    let content = bytes(1_000, 5);
    write_new(vfs, &path, &content);
    let file = open(vfs, &path, OpenMode::Read);

    let mut buf = vec![0; 1_000];
    file.read_exact_at(&mut buf, 0).unwrap();
    assert_eq!(buf, content);
    file.read_exact_at(&mut buf[..1], 999).unwrap();
    assert_eq!(buf[0], content[999]);

    assert_fails(
        file.read_exact_at(&mut buf[..2], 999),
        ErrorKind::UnexpectedEof,
    );
    assert_fails(
        file.read_exact_at(&mut buf[..1], 1_000),
        ErrorKind::UnexpectedEof,
    );
    file.read_exact_at(&mut [], 1_000).unwrap();
}

pub(crate) fn large_unaligned_io_round_trips(vfs: &dyn Vfs, root: &Path) {
    let len = if cfg!(miri) {
        70_001
    } else {
        3 * 1024 * 1024 + 17
    };
    let content = bytes(len, 6);
    let mut buf = vec![0; len];

    let written = open(vfs, &root.join("written"), OpenMode::Write(Create::New));
    written.write_at(&content, 4_093).unwrap();
    assert_eq!(written.len().unwrap(), 4_093 + len as u64);
    written.read_exact_at(&mut buf, 4_093).unwrap();
    assert!(buf == content);

    let appended = open(vfs, &root.join("appended"), OpenMode::Append(Create::New));
    assert_eq!(appended.append(b"x").unwrap(), 0);
    assert_eq!(appended.append(&content).unwrap(), 1);
    buf.fill(0);
    appended.read_exact_at(&mut buf, 1).unwrap();
    assert!(buf == content);
}

pub(crate) fn append_returns_consecutive_offsets(vfs: &dyn Vfs, root: &Path) {
    let file = open(vfs, &root.join("file"), OpenMode::Append(Create::New));
    let mut expected = Vec::new();
    for (seed, len) in [1, 4_096, 0, 37, 10_000, 5].into_iter().enumerate() {
        let record = bytes(len, seed as u64);
        assert_eq!(file.append(&record).unwrap(), expected.len() as u64);
        expected.extend_from_slice(&record);
        assert_eq!(file.len().unwrap(), expected.len() as u64);
    }
    assert_eq!(read_all(&*file), expected);
}

pub(crate) fn append_starts_at_the_end_of_an_existing_file(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    write_new(vfs, &path, b"0123456789");

    let file = open(vfs, &path, OpenMode::Append(Create::Never));
    assert_eq!(file.append(b"abc").unwrap(), 10);
    drop(file);
    let file = open(vfs, &path, OpenMode::Append(Create::IfMissing));
    assert_eq!(file.append(b"def").unwrap(), 13);
    assert_eq!(read_all(&*file), b"0123456789abcdef");
}

pub(crate) fn concurrent_appends_land_whole(vfs: &dyn Vfs, root: &Path) {
    let file = open(vfs, &root.join("file"), OpenMode::Append(Create::New));
    let file = &*file;

    let mut placed: Vec<(u64, Vec<u8>)> = thread::scope(|scope| {
        let appenders: Vec<_> = (0..8)
            .map(|thread| {
                scope.spawn(move || {
                    let mut placed = Vec::new();
                    for i in 0..ITERATIONS {
                        let len = 1 + (thread * 131 + i * 17) % 700;
                        let record = bytes(len as usize, thread << 32 | i);
                        let offset = file.append(&record).unwrap();
                        if let Some((previous, _)) = placed.last() {
                            assert!(offset > *previous);
                        }
                        placed.push((offset, record));
                    }
                    placed
                })
            })
            .collect();
        appenders
            .into_iter()
            .flat_map(|appender| appender.join().unwrap())
            .collect()
    });

    placed.sort_by_key(|(offset, _)| *offset);
    let mut end = 0;
    for (offset, record) in &placed {
        assert_eq!(*offset, end);
        let mut buf = vec![0; record.len()];
        file.read_exact_at(&mut buf, *offset).unwrap();
        assert!(buf == *record);
        end += record.len() as u64;
    }
    assert_eq!(file.len().unwrap(), end);
}

pub(crate) fn reads_run_alongside_appends(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    let appender = open(vfs, &path, OpenMode::Append(Create::New));
    let appender = &*appender;
    let reader = open(vfs, &path, OpenMode::Read);
    let reader = &*reader;
    let appended = AtomicU64::new(0);
    let done = AtomicBool::new(false);

    thread::scope(|scope| {
        for (thread, file) in [appender, reader, reader].into_iter().enumerate() {
            let (appended, done) = (&appended, &done);
            scope.spawn(move || {
                let mut round = thread as u64;
                loop {
                    let finished = done.load(Ordering::Acquire);
                    let end = appended.load(Ordering::Acquire);
                    round += 1;
                    let len = (round * 257 % 5_000).min(end);
                    let offset = round * 7_919 % (end - len + 1);
                    let mut buf = vec![0; len as usize];
                    file.read_exact_at(&mut buf, offset).unwrap();
                    assert!((offset..).zip(&buf).all(|(at, &byte)| byte == byte_at(at)));
                    if finished {
                        break;
                    }
                }
            });
        }

        let mut end = 0;
        for i in 0..ITERATIONS {
            let len = 1 + i * 97 % 3_000;
            let record: Vec<u8> = (end..end + len).map(byte_at).collect();
            assert_eq!(appender.append(&record).unwrap(), end);
            end += len;
            appended.store(end, Ordering::Release);
        }
        done.store(true, Ordering::Release);
    });
}

pub(crate) fn handles_refuse_operations_outside_their_mode(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    write_new(vfs, &path, b"content");

    let read = open(vfs, &path, OpenMode::Read);
    assert_fails(read.write_at(b"x", 0), ErrorKind::InvalidInput);
    assert_fails(read.append(b"x"), ErrorKind::InvalidInput);
    assert_fails(read.set_len(0), ErrorKind::InvalidInput);

    let write = open(vfs, &path, OpenMode::Write(Create::Never));
    assert_fails(write.append(b"x"), ErrorKind::InvalidInput);
    drop(write);

    let append = open(vfs, &path, OpenMode::Append(Create::Never));
    assert_fails(append.write_at(b"x", 0), ErrorKind::InvalidInput);

    assert_eq!(read_all(&*read), b"content");
}

pub(crate) fn an_append_handle_is_the_only_writer(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    let appender = open(vfs, &path, OpenMode::Append(Create::New));
    for mode in [
        OpenMode::Append(Create::Never),
        OpenMode::Append(Create::IfMissing),
        OpenMode::Write(Create::Never),
        OpenMode::Write(Create::IfMissing),
    ] {
        assert_fails(vfs.open(&path, mode), ErrorKind::ResourceBusy);
    }
    assert_eq!(appender.append(b"appended").unwrap(), 0);
    assert_eq!(read_path(vfs, &path), b"appended");
    drop(appender);

    let (first, second) = (
        open(vfs, &path, OpenMode::Write(Create::Never)),
        open(vfs, &path, OpenMode::Write(Create::Never)),
    );
    assert_fails(
        vfs.open(&path, OpenMode::Append(Create::Never)),
        ErrorKind::ResourceBusy,
    );
    drop(first);
    assert_fails(
        vfs.open(&path, OpenMode::Append(Create::Never)),
        ErrorKind::ResourceBusy,
    );
    second.write_at(b"A", 0).unwrap();
    drop(second);

    let appender = open(vfs, &path, OpenMode::Append(Create::Never));
    assert_eq!(appender.append(b"!").unwrap(), 8);
    assert_eq!(read_all(&*appender), b"Appended!");
}

pub(crate) fn set_len_shrinks_and_grows(vfs: &dyn Vfs, root: &Path) {
    let file = open(vfs, &root.join("file"), OpenMode::Write(Create::New));
    let content = bytes(1_000, 7);
    file.write_at(&content, 0).unwrap();

    file.set_len(400).unwrap();
    assert_eq!(file.len().unwrap(), 400);
    assert_eq!(read_all(&*file), content[..400]);
    assert_eq!(file.read_at(&mut [0; 10], 400).unwrap(), 0);

    file.set_len(900).unwrap();
    let grown = read_all(&*file);
    assert_eq!(grown.len(), 900);
    assert_eq!(grown[..400], content[..400]);
    assert!(grown[400..].iter().all(|&byte| byte == 0));

    file.set_len(0).unwrap();
    assert_eq!(file.len().unwrap(), 0);
}

pub(crate) fn append_follows_set_len(vfs: &dyn Vfs, root: &Path) {
    let file = open(vfs, &root.join("file"), OpenMode::Append(Create::New));
    assert_eq!(file.append(b"0123456789").unwrap(), 0);

    file.set_len(4).unwrap();
    assert_eq!(file.append(b"ab").unwrap(), 4);
    file.set_len(8).unwrap();
    assert_eq!(file.append(b"cd").unwrap(), 8);
    assert_eq!(read_all(&*file), b"0123ab\0\0cd");
}

pub(crate) fn sync_succeeds_in_every_mode(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");

    let write = open(vfs, &path, OpenMode::Write(Create::New));
    write.sync_data().unwrap();
    write.sync_all().unwrap();
    write.write_at(b"written", 0).unwrap();
    write.sync_data().unwrap();
    write.sync_all().unwrap();
    drop(write);

    let append = open(vfs, &path, OpenMode::Append(Create::Never));
    append.append(b" appended").unwrap();
    append.sync_data().unwrap();
    append.sync_all().unwrap();
    append.sync_data().unwrap();

    let read = open(vfs, &path, OpenMode::Read);
    read.sync_data().unwrap();
    read.sync_all().unwrap();
    assert_eq!(read_all(&*read), b"written appended");
}

pub(crate) fn handles_share_one_file(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    let writer = open(vfs, &path, OpenMode::Write(Create::New));
    let early = open(vfs, &path, OpenMode::Read);
    writer.write_at(b"shared", 0).unwrap();
    let late = open(vfs, &path, OpenMode::Read);
    assert_eq!(read_all(&*early), b"shared");
    assert_eq!(read_all(&*late), b"shared");

    writer.set_len(3).unwrap();
    assert_eq!(early.len().unwrap(), 3);
    assert_eq!(read_all(&*late), b"sha");
}

pub(crate) fn rename_moves_a_file(vfs: &dyn Vfs, root: &Path) {
    let (from, to) = (root.join("from"), root.join("to"));
    write_new(vfs, &from, b"moved");
    let handle = open(vfs, &from, OpenMode::Write(Create::Never));

    vfs.rename(&from, &to).unwrap();
    assert_fails(vfs.open(&from, OpenMode::Read), ErrorKind::NotFound);
    assert_eq!(read_path(vfs, &to), b"moved");

    handle.write_at(b"MOVED", 0).unwrap();
    assert_eq!(read_path(vfs, &to), b"MOVED");

    let dir = root.join("dir");
    vfs.create_dir(&dir).unwrap();
    vfs.rename(&to, &dir.join("inside")).unwrap();
    assert_eq!(read_path(vfs, &dir.join("inside")), b"MOVED");
    assert_eq!(names(vfs, root), ["dir"]);
    assert_eq!(names(vfs, &dir), ["inside"]);
}

pub(crate) fn rename_replaces_the_destination(vfs: &dyn Vfs, root: &Path) {
    let (current, next) = (root.join("current"), root.join("next"));
    write_new(vfs, &current, b"old content");
    write_new(vfs, &next, b"new");
    let old = open(vfs, &current, OpenMode::Read);

    vfs.rename(&next, &current).unwrap();
    assert_eq!(read_path(vfs, &current), b"new");
    assert_eq!(read_all(&*old), b"old content");
    assert_eq!(names(vfs, root), ["current"]);
}

pub(crate) fn rename_of_a_missing_file_is_not_found(vfs: &dyn Vfs, root: &Path) {
    let to = root.join("to");
    write_new(vfs, &to, b"kept");
    assert_fails(vfs.rename(&root.join("missing"), &to), ErrorKind::NotFound);
    assert_eq!(read_path(vfs, &to), b"kept");
    assert_eq!(names(vfs, root), ["to"]);
}

pub(crate) fn rename_between_names_of_one_file_changes_nothing(vfs: &dyn Vfs, root: &Path) {
    let (first, second) = (root.join("first"), root.join("second"));
    write_new(vfs, &first, b"content");
    vfs.hard_link(&first, &second).unwrap();

    vfs.rename(&first, &second).unwrap();
    vfs.rename(&first, &first).unwrap();
    assert_eq!(names(vfs, root), ["first", "second"]);
    assert_eq!(read_path(vfs, &first), b"content");
    assert_eq!(read_path(vfs, &second), b"content");
}

pub(crate) fn hard_link_is_another_name_for_the_same_file(vfs: &dyn Vfs, root: &Path) {
    let (original, link) = (root.join("original"), root.join("link"));
    let appender = open(vfs, &original, OpenMode::Append(Create::New));
    appender.append(b"before ").unwrap();

    vfs.hard_link(&original, &link).unwrap();
    assert_eq!(read_path(vfs, &link), b"before ");
    assert_fails(
        vfs.open(&link, OpenMode::Write(Create::Never)),
        ErrorKind::ResourceBusy,
    );
    appender.append(b"after").unwrap();
    assert_eq!(read_path(vfs, &link), b"before after");
    drop(appender);

    let through_link = open(vfs, &link, OpenMode::Write(Create::Never));
    through_link.write_at(b"BEFORE", 0).unwrap();
    assert_eq!(read_path(vfs, &original), b"BEFORE after");
    assert_eq!(names(vfs, root), ["link", "original"]);
}

pub(crate) fn hard_link_refuses_a_taken_name_and_a_missing_file(vfs: &dyn Vfs, root: &Path) {
    let (first, second) = (root.join("first"), root.join("second"));
    write_new(vfs, &first, b"first");
    write_new(vfs, &second, b"second");

    assert_fails(vfs.hard_link(&first, &second), ErrorKind::AlreadyExists);
    assert_eq!(read_path(vfs, &second), b"second");
    assert_fails(
        vfs.hard_link(&root.join("missing"), &root.join("link")),
        ErrorKind::NotFound,
    );
    assert_eq!(names(vfs, root), ["first", "second"]);
}

pub(crate) fn hard_link_outlives_the_original_name(vfs: &dyn Vfs, root: &Path) {
    let (original, link) = (root.join("original"), root.join("link"));
    write_new(vfs, &original, b"content");
    vfs.hard_link(&original, &link).unwrap();

    vfs.remove_file(&original).unwrap();
    assert_fails(vfs.open(&original, OpenMode::Read), ErrorKind::NotFound);
    assert_eq!(read_path(vfs, &link), b"content");
    assert_eq!(names(vfs, root), ["link"]);
}

pub(crate) fn remove_file_removes_the_name_only(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("file");
    write_new(vfs, &path, b"still here");
    let handle = open(vfs, &path, OpenMode::Write(Create::Never));

    vfs.remove_file(&path).unwrap();
    assert_fails(vfs.open(&path, OpenMode::Read), ErrorKind::NotFound);
    assert_fails(vfs.remove_file(&path), ErrorKind::NotFound);
    assert!(vfs.read_dir(root).unwrap().is_empty());

    assert_eq!(read_all(&*handle), b"still here");
    handle.write_at(b"STILL", 0).unwrap();
    handle.sync_data().unwrap();

    write_new(vfs, &path, b"another file");
    assert_eq!(read_all(&*handle), b"STILL here");
    assert_eq!(read_path(vfs, &path), b"another file");
}

pub(crate) fn create_dir_needs_its_parent_and_a_free_name(vfs: &dyn Vfs, root: &Path) {
    let (dir, file) = (root.join("dir"), root.join("file"));
    vfs.create_dir(&dir).unwrap();
    write_new(vfs, &file, b"");

    assert_fails(vfs.create_dir(&dir), ErrorKind::AlreadyExists);
    assert_fails(vfs.create_dir(&file), ErrorKind::AlreadyExists);
    assert_fails(
        vfs.create_dir(&root.join("missing").join("dir")),
        ErrorKind::NotFound,
    );

    write_new(vfs, &dir.join("inside"), b"in a directory");
    assert_eq!(read_path(vfs, &dir.join("inside")), b"in a directory");
    assert_eq!(
        vfs.read_dir(root).unwrap(),
        [entry("dir", EntryKind::Dir), entry("file", EntryKind::File)]
    );
}

pub(crate) fn remove_dir_needs_an_empty_directory(vfs: &dyn Vfs, root: &Path) {
    let dir = root.join("dir");
    vfs.create_dir(&dir).unwrap();
    write_new(vfs, &dir.join("file"), b"content");

    assert_fails(vfs.remove_dir(&dir), ErrorKind::DirectoryNotEmpty);
    assert_eq!(read_path(vfs, &dir.join("file")), b"content");

    vfs.remove_file(&dir.join("file")).unwrap();
    vfs.remove_dir(&dir).unwrap();
    assert_fails(vfs.remove_dir(&dir), ErrorKind::NotFound);
    assert!(vfs.read_dir(root).unwrap().is_empty());
}

pub(crate) fn read_dir_lists_sorted_names_with_kinds(vfs: &dyn Vfs, root: &Path) {
    assert!(vfs.read_dir(root).unwrap().is_empty());

    for name in ["00000010.seg", "b", "00000002.seg", "a", "Z"] {
        write_new(vfs, &root.join(name), b"");
    }
    let wal = root.join("wal");
    vfs.create_dir(&wal).unwrap();
    write_new(vfs, &wal.join("00000001.log"), b"");

    assert_eq!(
        vfs.read_dir(root).unwrap(),
        [
            entry("00000002.seg", EntryKind::File),
            entry("00000010.seg", EntryKind::File),
            entry("Z", EntryKind::File),
            entry("a", EntryKind::File),
            entry("b", EntryKind::File),
            entry("wal", EntryKind::Dir),
        ]
    );
    assert_eq!(
        vfs.read_dir(&wal).unwrap(),
        [entry("00000001.log", EntryKind::File)]
    );
    assert_fails(vfs.read_dir(&root.join("missing")), ErrorKind::NotFound);
    assert_fails(vfs.read_dir(&root.join("a")), ErrorKind::NotADirectory);
}

pub(crate) fn sync_dir_takes_directories_only(vfs: &dyn Vfs, root: &Path) {
    vfs.sync_dir(root).unwrap();

    let dir = root.join("dir");
    vfs.create_dir(&dir).unwrap();
    write_new(vfs, &dir.join("file"), b"content");
    vfs.sync_dir(&dir).unwrap();
    vfs.sync_dir(root).unwrap();

    assert_fails(vfs.sync_dir(&root.join("missing")), ErrorKind::NotFound);
    assert_fails(vfs.sync_dir(&dir.join("file")), ErrorKind::NotADirectory);
}

pub(crate) fn lock_excludes_until_it_is_dropped(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("LOCK");
    let guard = vfs.lock(&path).unwrap();
    assert_fails(vfs.lock(&path), ErrorKind::WouldBlock);
    assert_fails(vfs.lock(&path), ErrorKind::WouldBlock);

    drop(guard);
    let guard = vfs.lock(&path).unwrap();
    assert_fails(vfs.lock(&path), ErrorKind::WouldBlock);
    drop(guard);
}

pub(crate) fn lock_creates_its_file_and_leaves_it(vfs: &dyn Vfs, root: &Path) {
    let path = root.join("LOCK");
    drop(vfs.lock(&path).unwrap());
    assert_eq!(
        vfs.read_dir(root).unwrap(),
        [entry("LOCK", EntryKind::File)]
    );

    open(vfs, &path, OpenMode::Write(Create::Never))
        .write_at(b"kept", 0)
        .unwrap();
    let guard = vfs.lock(&path).unwrap();
    assert_eq!(read_path(vfs, &path), b"kept");
    drop(guard);
    assert_eq!(read_path(vfs, &path), b"kept");

    assert_fails(
        vfs.lock(&root.join("missing").join("LOCK")),
        ErrorKind::NotFound,
    );
}

pub(crate) fn locks_on_different_files_are_independent(vfs: &dyn Vfs, root: &Path) {
    let (first, second) = (root.join("first"), root.join("second"));
    let first_guard = vfs.lock(&first).unwrap();
    let second_guard = vfs.lock(&second).unwrap();

    drop(first_guard);
    assert_fails(vfs.lock(&second), ErrorKind::WouldBlock);
    let first_guard = vfs.lock(&first).unwrap();
    drop(second_guard);
    assert_fails(vfs.lock(&first), ErrorKind::WouldBlock);
    drop(first_guard);
}

pub(crate) fn one_thread_wins_a_contended_lock(vfs: &dyn Vfs, root: &Path) {
    const THREADS: usize = 8;
    let path = root.join("LOCK");
    let (start, tried) = (Barrier::new(THREADS), Barrier::new(THREADS));

    let winners = thread::scope(|scope| {
        let contenders: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    let attempt = vfs.lock(&path);
                    tried.wait();
                    match attempt {
                        Ok(_guard) => 1,
                        Err(error) => {
                            assert_eq!(error.kind(), ErrorKind::WouldBlock);
                            0
                        }
                    }
                })
            })
            .collect();
        contenders
            .into_iter()
            .map(|contender| contender.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(winners, 1);
    drop(vfs.lock(&path).unwrap());
}

pub(crate) fn a_file_is_replaced_by_write_sync_rename_sync_dir(vfs: &dyn Vfs, root: &Path) {
    let (manifest, temporary) = (root.join("MANIFEST"), root.join("MANIFEST.tmp"));
    for version in 1..=3 {
        let content = bytes(2_000 * version, version as u64);
        let file = open(vfs, &temporary, OpenMode::Write(Create::New));
        file.write_at(&content, 0).unwrap();
        file.sync_data().unwrap();
        drop(file);

        vfs.rename(&temporary, &manifest).unwrap();
        vfs.sync_dir(root).unwrap();
        assert_eq!(read_path(vfs, &manifest), content);
        assert_eq!(names(vfs, root), ["MANIFEST"]);
    }
}

pub(crate) fn a_checkpoint_is_a_directory_of_hard_links(vfs: &dyn Vfs, root: &Path) {
    let (live, checkpoint) = (root.join("live"), root.join("checkpoint"));
    let (live_segment, linked_segment) =
        (live.join("00000001.seg"), checkpoint.join("00000001.seg"));
    vfs.create_dir(&live).unwrap();
    let segment = open(vfs, &live_segment, OpenMode::Append(Create::New));
    let committed = bytes(5_000, 8);
    segment.append(&committed).unwrap();
    segment.sync_data().unwrap();
    let valid_len = segment.len().unwrap();

    vfs.create_dir(&checkpoint).unwrap();
    vfs.hard_link(&live_segment, &linked_segment).unwrap();
    vfs.sync_dir(&checkpoint).unwrap();
    vfs.sync_dir(root).unwrap();

    segment.append(&bytes(3_000, 9)).unwrap();
    let linked = open(vfs, &linked_segment, OpenMode::Read);
    assert_eq!(linked.len().unwrap(), 8_000);
    let mut prefix = vec![0; valid_len as usize];
    linked.read_exact_at(&mut prefix, 0).unwrap();
    assert_eq!(prefix, committed);

    drop(segment);
    vfs.remove_file(&live_segment).unwrap();
    linked.read_exact_at(&mut prefix, 0).unwrap();
    assert_eq!(prefix, committed);
    drop(linked);

    for entry in vfs.read_dir(&checkpoint).unwrap() {
        vfs.remove_file(&checkpoint.join(entry.name)).unwrap();
    }
    vfs.remove_dir(&checkpoint).unwrap();
    vfs.sync_dir(root).unwrap();
    assert_eq!(names(vfs, root), ["live"]);
}
